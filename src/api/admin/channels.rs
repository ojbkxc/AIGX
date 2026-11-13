//! 渠道管理 API（P0-W1 自 `admin.rs` 迁移）
//!
//! 提供渠道列表、添加、更新、删除、健康检查等功能。
//!
//! ## 路径说明
//!
//! - 使用 `crate::` 访问主 crate 的类型和资源

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::Json,
};
use serde::Deserialize;
use serde_json::{json, Value};

use super::super::openai::AppState;
use super::common::{
    admin_id_from_session, error_response, record_audit, verify_admin, verify_user,
};

// 这里需要引用主 crate 的 Channel 和相关类型
use crate::channel::{Channel, ChannelType};

/// GET /api/channels/:id/health-archive — 渠道健康档案（C1）。
///
/// 返回该渠道最近 N 天（默认 30，上限 30）的健康趋势：
/// 每日成功率 / P95 延迟 / 熔断次数 / 最后错误。用于渠道行展开的
/// 「健康档案」面板（ROADMAP P1-14）。
pub async fn handle_channel_health_archive(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;

    let days = 30u32;
    let snapshots = state.channel_store.health_archive().query(&id, days);
    let data: Vec<Value> = snapshots
        .iter()
        .map(|s| {
            json!({
                "day": s.day,
                "success": s.success,
                "failure": s.failure,
                "trips": s.trips,
                "success_rate": (s.success_rate() * 100.0).round() / 100.0,
                "p95_ms": s.p95_ms(),
                "last_error": s.last_error,
            })
        })
        .collect();

    Ok(Json(json!({
        "success": true,
        "data": {
            "channel_id": id,
            "days": days,
            "snapshots": data,
        }
    })))
}

/// `channel_type` 查询参数可识别的精确值（from_str_lossy 的全部别名）。
/// 未知值不过滤（from_str_lossy 会兜底 Cloudflare，不能让它把乱传的值
/// 当成「过滤 cloudflare」）。
const KNOWN_CHANNEL_TYPE_FILTERS: &[&str] = &[
    "cloudflare",
    "openai_compatible",
    "openai-compatible",
    "openai",
    "anthropic",
    "gemini",
    "google",
    "google_gemini",
    "zai",
    "z_ai",
    "zhipu",
    "glm",
    "account",
    "accounts",
    "account_pool",
    "account_session",
];

/// 渠道创建请求
///
/// `api_key` 三态语义（对齐 cf-ai-gw expiresAt 的 undefined/null/值 模式）：
/// - 缺省 / `None`：不涉及密钥（新增=空密钥渠道，如免鉴权上游；更新=保持原值）
/// - `Some("")`：显式清除密钥
/// - `Some("sk-..")`：设置新密钥
///
/// `Serialize` **只为让「表单字段名单」能在测试里派生**（见本文件测试
/// `channel_fields_are_all_accounted_for_by_form_or_backfill`），
/// 不是行为变更：本结构仍然只被反序列化（请求体入口），没有任何出口序列化它。
#[derive(Debug, Deserialize, serde::Serialize)]
pub struct ChannelRequest {
    pub name: String,
    #[serde(default)]
    pub channel_type: String,
    #[serde(default)]
    pub base_url: String,
    #[serde(default, deserialize_with = "deserialize_optional_string")]
    pub api_key: Option<String>,
    #[serde(default)]
    pub priority: i64,
    #[serde(default = "default_weight")]
    pub weight: u32,
    #[serde(default = "default_enabled_status")]
    pub status: String,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default)]
    pub account_id: String,
    #[serde(default)]
    pub model_mapping: std::collections::HashMap<String, String>,
    #[serde(default)]
    pub cost_pricing: std::collections::HashMap<String, crate::pricing::ModelPrice>,
    /// 上游特判（预置供应商卡片写入：urlSuffix / 静态头 / anthropicToolType /
    /// clientEmulation）。三态：缺省 = 更新时保持原值；`{}` = 清空；有值 = 覆盖。
    #[serde(default)]
    pub quirks: Option<crate::channel::ChannelQuirks>,
    /// 余额查询方案（见 `channel::balance::SCHEMES`）；空 = OpenAI 计费端点
    #[serde(default)]
    pub balance_scheme: Option<String>,
}

fn default_weight() -> u32 {
    1
}

fn default_enabled_status() -> String {
    "enabled".to_string()
}

/// `Option<String>` 反序列化：字段缺省 → None；显式 `null` / `""` → Some("")。
///
/// 不能用 `Option<String>` 的默认行为（null 会变成 None，与"字段缺省"无法区分），
/// 这里把 null 归一为 Some("")（= 清除语义），缺省才是 None（= 不涉及）。
fn deserialize_optional_string<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let opt: Option<String> = Option::deserialize(deserializer)?;
    Ok(Some(opt.unwrap_or_default()))
}

impl ChannelRequest {
    pub fn to_channel(&self, id: String) -> Channel {
        let now = chrono::Utc::now().timestamp();
        Channel {
            id,
            name: self.name.clone(),
            channel_type: ChannelType::from_str_lossy(&self.channel_type),
            base_url: self.base_url.clone(),
            // 三态归一：None/Some("") → 空密钥；Some(key) → 密钥
            // （新增时 None = 空密钥渠道；更新时 handler 先按 prev 保持原值再应用 Some）
            api_key: self.api_key.clone().unwrap_or_default(),
            priority: self.priority,
            weight: self.weight,
            status: self.status.clone(),
            models: self.models.clone(),
            account_id: self.account_id.clone(),
            plugin_provider: String::new(),
            model_mapping: self.model_mapping.clone(),
            cost_pricing: self.cost_pricing.clone(),
            last_error: None,
            last_used_at: None,
            discovered_models: Vec::new(),
            response_time: None,
            test_time: None,
            balance: None,
            credit: None,
            // 新增时缺省 = 无特判；更新时 handler 会按 prev 保持原值
            quirks: self.quirks.clone().unwrap_or_default(),
            balance_scheme: self
                .balance_scheme
                .as_deref()
                .map(validate_balance_scheme)
                .unwrap_or_default(),
            balance_unit: String::new(),
            balance_query_interval_secs: None,
            balance_last_query_at: None,
            balance_last_error: None,
            created_at: now,
            updated_at: now,
        }
    }
}

/// 校验余额方案：只认 `channel::balance::SCHEMES` 里的值，未知值归一为空
/// （= 回落 OpenAI 计费端点）。与 `client_emulation` 的读侧容错同一取向：
/// 手改坏的值不该让整条渠道记录写不进去。
fn validate_balance_scheme(raw: &str) -> String {
    let trimmed = raw.trim();
    if crate::channel::balance::SCHEMES.contains(&trimmed) {
        trimmed.to_string()
    } else {
        String::new()
    }
}

/// 密钥脱敏（列表/详情展示用）：>12 字符保留首 8 + 末 4，否则全掩码。
///
/// 按字符（而非字节）切片：非 ASCII key 的字节切片会 panic
/// （`chars().count()` 判长 + 字节下标切是不匹配的组合）。
pub fn mask_secret(secret: &str) -> String {
    let chars: Vec<char> = secret.chars().collect();
    if chars.is_empty() {
        String::new()
    } else if chars.len() > 12 {
        let head: String = chars[..8].iter().collect();
        let tail: String = chars[chars.len() - 4..].iter().collect();
        format!("{head}...{tail}")
    } else {
        "****".to_string()
    }
}

/// #32 PR2：渠道错误文案的**唯一**出口侧脱敏道。
///
/// `last_error` 与 `balance_last_error` 的值都不是本地生成的短语，而是**上游响应内容**：
/// - `last_error` ← `mark_unhealthy(&id, result.message)`；
/// - `balance_last_error` ← `channel::balance` 的失败分支
///   （`上游返回 HTTP {status}：{shorten(正文, 160)}` / `商汤登录失败（HTTP {status}）：{shorten(正文, 120)}`）。
///
/// 401/403 类正文常回显渠道侧凭证（转发密钥、控制台令牌），所以两个键**必须走同一道**：
/// 只处理新加的 `balance_last_error` 而放过已在库里的 `last_error`，等于换个键继续漏。
///
/// 底下调的是 `error_translate::sanitize_error_message`（与数据面发给客户端的错误出口
/// 同一函数），它只做三件事：截断到 500 字节、`Bearer <token>` → `Bearer ***`、
/// `sk-<密钥段>` → `sk-***`（前边界锚 + 吞满多段 `-`/`_`，见 #45），**其余文本逐字符原样保留**。
/// 刻意不额外抹掉状态码与文案，因为运维排障就靠 `last_error` 这句话——脱敏过度等于把
/// 可观测性拆了。它的边界要如实登记：只拦这两类形态，**裸 JWT / 手机号 /
/// `"access_token":"…"` 不在拦围内**（best-effort，非安全保证）；完整清单见该函数 doc。
fn mask_error_text(raw: Option<&str>) -> Option<String> {
    raw.map(crate::error_translate::sanitize_error_message)
}

/// 构造渠道 JSON 响应（脱敏 API Key）
/// `seq`：展示用短编号（列表接口按创建顺序注入 1..N；单渠道场景传 None 留空）。
/// 构造渠道 JSON 响应（脱敏 API Key 与余额查询凭证）
///
/// `seq`：展示用短编号（列表接口按创建顺序注入 1..N；单渠道场景传 None 留空）。
///
/// 脱敏范围：
/// - `api_key` → 首 8 + 末 4
/// - `quirks.headers` 里的 `x-balance-*`（控制台令牌 / 账号密码）→ 全掩码。
///   这些是**余额查询专用凭证**，与转发密钥同等敏感；`mask_channel` 会把
///   整条 quirks 序列化给前端，不剔除等于把控制台密码明文吐出来。
/// - `last_error` / `balance_last_error`（上游文案，可能回显凭证）→ 见 [`mask_error_text`]。
pub fn mask_channel(ch: &Channel, seq: Option<u64>) -> Value {
    let masked_key = mask_secret(&ch.api_key);
    // quirks.headers 脱敏：敏感头（余额凭证 / 客户端令牌 / 账号密码）整体
    // 替换为掩码（前端只需要知道「有没有配」）
    let mut quirks = ch.quirks.clone();
    for (name, value) in quirks.headers.iter_mut() {
        if crate::channel::is_sensitive_header(name) && !value.is_empty() {
            *value = "****".to_string();
        }
    }
    json!({
        "id": ch.id,
        "seq": seq.unwrap_or(0),
        "name": ch.name,
        "channel_type": ch.channel_type.as_str(),
        "base_url": ch.base_url,
        "api_key": masked_key,
        "priority": ch.priority,
        "weight": ch.weight,
        "status": ch.status,
        "models": ch.models,
        "discovered_models": ch.discovered_models,
        "model_mapping": ch.model_mapping,
        "cost_pricing": ch.cost_pricing,
        "account_id": ch.account_id,
        "last_error": mask_error_text(ch.last_error.as_deref()),
        "last_used_at": ch.last_used_at,
        "response_time": ch.response_time,
        "test_time": ch.test_time,
        "balance": ch.balance,
        "credit": ch.credit,
        "quirks": quirks,
        "balance_scheme": ch.balance_scheme,
        "balance_unit": ch.balance_unit,
        // #32 PR2：渠道型周期余额查询三键（配置态 + 巡检写回的运行态）。
        // - `balance_query_interval_secs`：`None` = 不参与周期巡检（0/null 在 PATCH
        //   里就归一成 None），前端据此画「关闭」而不是「1970 年下次查询」；
        // - `balance_last_query_at`：unix 秒，只说明「什么时候查过」，无凭证语义；
        // - `balance_last_error`：失败文案，**含上游正文片段** ⇒ 与 `last_error` 同规则。
        "balance_query_interval_secs": ch.balance_query_interval_secs,
        "balance_last_query_at": ch.balance_last_query_at,
        "balance_last_error": mask_error_text(ch.balance_last_error.as_deref()),
        "created_at": ch.created_at,
        "updated_at": ch.updated_at,
    })
}

/// 列出所有渠道（支持分页）。
///
/// 查询参数（均可选，向后兼容）：
/// - `page`：从 1 开始，缺省 1
/// - `page_size`：缺省 0 = 不分页，返回全量（旧前端/脚本兼容）
/// - `search`：按名称/类型/base_url 模糊过滤
///
/// 分页时返回 `{ data, page, page_size, total }`；不分页时 data 保持原数组形状。
pub async fn handle_list_channels(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    // 渠道类型过滤（账号池「已建 API」下拉用：channel_type=account 只列
    // 账号池容器渠道）。from_str_lossy 对未知值兜底 Cloudflare，因此只认
    // 能精确命中的串（含 cloudflare 本身），乱传的值静默不过滤。
    let type_filter = params
        .get("channel_type")
        .map(|t| crate::channel::ChannelType::from_str_lossy(t))
        .filter(|_| {
            params.get("channel_type").is_some_and(|raw| {
                KNOWN_CHANNEL_TYPE_FILTERS.contains(&raw.to_lowercase().as_str())
            })
        });
    // 注入展示用短编号：按创建时间升序 1..N（同列表排序键，新增渠道追加新号，
    // 既有渠道编号稳定不变；删除渠道不回收编号——避免错位显示）
    let all: Vec<Channel> = state
        .channel_store
        .list()
        .into_iter()
        .filter(|ch| type_filter.map(|t| ch.channel_type == t).unwrap_or(true))
        .collect();
    let mut sorted: Vec<&Channel> = all.iter().collect();
    sorted.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
    let id_to_seq: std::collections::HashMap<&str, u64> = sorted
        .iter()
        .enumerate()
        .map(|(i, ch)| (ch.id.as_str(), (i + 1) as u64))
        .collect();

    // 服务端搜索过滤（名称/类型/base_url/模型名，前端一致）
    let search = params
        .get("search")
        .map(|s| s.to_lowercase())
        .unwrap_or_default();
    let filtered: Vec<&Channel> = if search.is_empty() {
        all.iter().collect()
    } else {
        all.iter()
            .filter(|ch| {
                ch.name.to_lowercase().contains(&search)
                    || ch.channel_type.as_str().to_lowercase().contains(&search)
                    || ch.base_url.to_lowercase().contains(&search)
                    || ch.models.iter().any(|m| m.to_lowercase().contains(&search))
            })
            .collect()
    };
    let total = filtered.len();

    let page = params
        .get("page")
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&p| p >= 1)
        .unwrap_or(1);
    let page_size = params
        .get("page_size")
        .or_else(|| params.get("pageSize")) // 前端 camelCase 兼容
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&s| s > 0)
        .unwrap_or(0);

    let channels: Vec<Value> = if page_size > 0 {
        // saturating_mul：page 无上限时 `(page-1)*page_size` 会 usize 溢出
        // （debug panic / release 回绕到错误下标）。溢出时 start 极大，
        // 下面的 `start < end` 判据自然返回空页。
        let start = page.saturating_sub(1).saturating_mul(page_size);
        let end = start.saturating_add(page_size).min(total);
        // 页码越界（start >= total）时返回空页而非 panic：
        // 前端删除/搜索缩小 total 后旧页码仍可能被保留
        let page_slice: Vec<Value> = if start < end {
            filtered[start..end]
                .iter()
                .map(|ch| mask_channel(ch, id_to_seq.get(ch.id.as_str()).copied()))
                .collect()
        } else {
            Vec::new()
        };
        page_slice
    } else {
        // 不分页：全量（兼容旧调用方）
        filtered
            .iter()
            .map(|ch| mask_channel(ch, id_to_seq.get(ch.id.as_str()).copied()))
            .collect()
    };
    Ok(Json(json!({
        "success": true,
        "data": channels,
        "page": page,
        "page_size": page_size,
        "total": total
    })))
}

/// 列出可用模型（登录用户即可，对齐 new-api `GET /api/user/models`）
///
/// 控制台模型下拉（聊天页 / AI 运维 / 密钥白名单共用）。
///
/// **数据源与客户端 API 的 `/v1/models` 完全同源**：两者都调
/// [`crate::api::openai::collect_available_models`]，所以「客户端调得通的模型，
/// 下拉里一定选得到」以及反过来都成立。
///
/// 本端点在此之上只多做两件事：
/// 1. 按用户分组 `allowed_models` 白名单收窄（None/空 = 不限）；
/// 2. 附加价格字段（`price_input` / `price_output` / `price_type`），
///    供控制台展示 —— 客户端清单不含价格。
pub async fn handle_available_models(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let user = verify_user(&state, &headers).await?;
    // 组白名单：None 与 Some(空) 均表示不限
    let group_allow: Option<Vec<String>> = state
        .user_group_store
        .get(&user.group)
        .and_then(|g| g.allowed_models);
    let unrestricted = match &group_allow {
        None => true,
        Some(list) => list.is_empty(),
    };
    let allow_set = group_allow.unwrap_or_default();

    // 聚合 = 与 /v1/models 同一实现（渠道/账号模型 + 懒发现 + 全局别名 + 自定义模型）
    let models = super::super::openai::collect_available_models(&state).await;

    let data: Vec<Value> = models
        .into_iter()
        .filter(|(m, _)| unrestricted || allow_set.iter().any(|a| a == m))
        .map(|(m, owned_by)| {
            let meta = state.model_metadata.get(&m, owned_by);
            let price = state.pricing_store.get_price(&m);
            json!({
                "id": m,
                "owned_by": meta.owned_by,
                "context_length": meta.context_length,
                "capabilities": meta.capabilities,
                "price_input": price.as_ref().map(|p| p.input_price).unwrap_or(0.0),
                "price_output": price.as_ref().map(|p| p.output_price).unwrap_or(0.0),
                "price_type": price.as_ref().map(|p| p.price_type.as_str()).unwrap_or("token"),
            })
        })
        .collect();
    Ok(Json(json!({ "success": true, "data": data })))
}

/// 添加新渠道
pub async fn handle_add_channel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ChannelRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = verify_admin(&state, &headers).await?;
    // SSRF 防护：拒绝把上游指向内网/环回/云元数据地址。
    // 空 base_url 合法（Cloudflare / 账号型渠道无渠道级地址）。
    if !body.base_url.trim().is_empty() {
        if let Err(rej) = crate::net_guard::validate_outbound_url(
            &body.base_url,
            config.usage.allow_internal_upstream,
        ) {
            return Err(error_response(&rej.message(), StatusCode::BAD_REQUEST));
        }
    }
    let ch = body.to_channel(String::new());
    match state.channel_store.add(ch) {
        Ok(c) => {
            let admin_id = admin_id_from_session(&state, &headers).await;
            record_audit(
                &state,
                &admin_id,
                "create_channel",
                &format!("id={} name={}", c.id, c.name),
                None,
                Some(json!({
                    "name": c.name,
                    "channel_type": c.channel_type.as_str(),
                    "base_url": c.base_url,
                })),
            );
            Ok(Json(
                json!({ "success": true, "data": mask_channel(&c, None) }),
            ))
        }
        Err(e) => Err(error_response(
            &format!("Failed to add channel: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )),
    }
}

/// 用于 PUT 时把「表单里没有但存库里有」的字段从已存渠道搬回来。
///
/// `ChannelRequest::to_channel()` 重建整条 Channel，表单里没有的字段（运行态读数 +
/// 只在 PATCH 面暴露的巡检配置）会被硬置成 `None` ⇒ 必须按已存渠道回填，否则在渠道
/// 表单里点一次保存就会静默关掉周期余额巡检并清空其读数；而 `ChannelRequest` 不含这
/// 三键、PATCH 是唯一写路径，丢了补不回来（2026-11-09 fe-fix 在 PR3 侦察时发现）。
///
/// 抽成纯函数是为了能直接单测（`handle_update_channel` 要吃 `AppState` + `verify_admin`）。
/// `quirks` / `balance_scheme` / `api_key` 那些「表单提交了就覆盖」的分支依赖
/// `ChannelRequest`，留在调用方处理；`to_channel()` 里把三键硬置 `None` 是「新建渠道」
/// 正确的 opt-in 默认，不改构造器。
fn preserve_non_form_fields(ch: &mut Channel, prev: &Channel) {
    ch.response_time = prev.response_time;
    ch.test_time = prev.test_time;
    ch.balance = prev.balance;
    ch.credit = prev.credit;
    ch.balance_unit = prev.balance_unit.clone();
    ch.discovered_models = prev.discovered_models.clone();
    ch.last_used_at = prev.last_used_at;
    ch.last_error = prev.last_error.clone();
    ch.created_at = prev.created_at;
    ch.balance_query_interval_secs = prev.balance_query_interval_secs;
    ch.balance_last_query_at = prev.balance_last_query_at;
    ch.balance_last_error = prev.balance_last_error.clone();
}

/// 更新渠道信息
///
/// PUT 全量更新：保留既有测试结果/余额等运维字段（`to_channel` 不携带这些，
/// 旧实现直接 add 会把它们清零），再覆盖表单提交的字段。
pub async fn handle_update_channel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<ChannelRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = verify_admin(&state, &headers).await?;
    // SSRF 防护（与新增同口径）：拒绝内网/环回/元数据地址。
    if !body.base_url.trim().is_empty() {
        if let Err(rej) = crate::net_guard::validate_outbound_url(
            &body.base_url,
            config.usage.allow_internal_upstream,
        ) {
            return Err(error_response(&rej.message(), StatusCode::BAD_REQUEST));
        }
    }
    let mut ch = body.to_channel(id.clone());
    // 保留运维字段（test_time/response_time/balance/credit/discovered_models/
    // last_used_at/created_at）：编辑表单不提交这些，不应被清空
    if let Some(prev) = state.channel_store.get(&id) {
        // 表单不携带的运行态/巡检字段：一律按已存渠道回填（见函数文档）
        preserve_non_form_fields(&mut ch, &prev);
        // quirks / 余额方案：缺省（前端未提交）→ 保持原值；提交了 → 覆盖
        if body.quirks.is_none() {
            ch.quirks = prev.quirks.clone();
        } else {
            // 敏感头被脱敏成 `****` 后前端原样提交回来：此时保留原值
            // （否则一次「只改名称」的保存会把所有凭证覆盖成字面量 ****）。
            for (name, value) in ch.quirks.headers.iter_mut() {
                if value == "****" {
                    if let Some(original) = prev.quirks.headers.get(name) {
                        *value = original.clone();
                    }
                }
            }
        }
        if body.balance_scheme.is_none() {
            ch.balance_scheme = prev.balance_scheme.clone();
        }
        // api_key 三态：None（前端未动密钥）→ 保持原值；Some("") → 清除；Some(key) → 设置
        match body.api_key {
            None => ch.api_key = prev.api_key,
            Some(ref k) => ch.api_key = k.clone(),
        }
    }
    match state.channel_store.update(&id, ch) {
        Ok(c) => {
            let admin_id = admin_id_from_session(&state, &headers).await;
            record_audit(
                &state,
                &admin_id,
                "update_channel",
                &format!("id={} name={}", c.id, c.name),
                None,
                Some(json!({
                    "name": c.name,
                    "channel_type": c.channel_type.as_str(),
                    "status": c.status,
                })),
            );
            Ok(Json(
                json!({ "success": true, "data": mask_channel(&c, None) }),
            ))
        }
        Err(e) => Err(error_response(
            &format!("Failed to update channel: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )),
    }
}

/// 删除渠道请求体（敏感操作二次认证）
///
/// `password`：当前管理员登录密码。HTTP 前端直调必须携带且校验通过。
#[derive(Debug, Deserialize)]
pub struct DeleteChannelRequest {
    pub password: Option<String>,
}

/// 删除渠道（HTTP 入口：会话校验 + 密码二次认证）
///
/// 管理员敏感操作二次认证：HTTP 请求必须携带当前管理员登录密码，
/// 校验失败返回 403（防会话劫持/误操作直接删除生产渠道）。
/// Agent 工具路径不经过本 handler，直调 [`delete_channel_core`]。
pub async fn handle_delete_channel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Option<Json<DeleteChannelRequest>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    verify_admin_password(&state, &headers, body.and_then(|Json(b)| b.password)).await?;
    delete_channel_core(&state, &headers, &id).await
}

/// 删除渠道核心逻辑（HTTP 与 Agent 工具共用）。
///
/// Agent 路径免密码：高危工具已经审批矩阵人工确认（runner.rs），
/// 审批本身即同等强度的身份复核；HTTP 路径在 handler 层已验密。
/// 调用方需自行完成身份校验（HTTP = verify_admin + 密码；Agent = verify_admin）。
pub async fn delete_channel_core(
    state: &AppState,
    headers: &HeaderMap,
    id: &str,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let admin_id = admin_id_from_session(state, headers).await;
    let before = state.channel_store.get(id).map(|ch| {
        json!({
            "name": ch.name,
            "status": ch.status,
            "models_count": ch.models.len(),
        })
    });
    match state.channel_store.remove(id) {
        Ok(_) => {
            // 级联清理账号池：账号凭证是独立 KV，渠道删除后不应残留悬挂账号
            if let Err(e) = state.account_sessions.remove_for_channel(id) {
                tracing::warn!("Failed to cascade remove account sessions for {id}: {e}");
            }
            record_audit(
                state,
                &admin_id,
                "delete_channel",
                &format!("id={id}"),
                before,
                None,
            );
            Ok(Json(json!({ "success": true, "data": null })))
        }
        Err(e) => Err(error_response(
            &format!("Failed to delete channel: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )),
    }
}

/// 管理员密码二次认证：HTTP 路径必须验密，Agent 进程内直调（password=None）
/// 跳过（审批矩阵已人工确认）。密码错误返回 403 并记安全日志。
async fn verify_admin_password(
    state: &AppState,
    headers: &HeaderMap,
    password: Option<String>,
) -> Result<(), (StatusCode, Json<Value>)> {
    let Some(password) = password.filter(|p| !p.is_empty()) else {
        return Err(error_response(
            "删除渠道需要输入登录密码进行认证",
            StatusCode::BAD_REQUEST,
        ));
    };
    let email = admin_id_from_session(state, headers).await;
    let user = state
        .user_store
        .get_by_email(&email)
        .ok_or_else(|| error_response("Session user not found", StatusCode::UNAUTHORIZED))?;
    if crate::user::verify_password(&password, &user.password) {
        Ok(())
    } else {
        state
            .log_store
            .record_security(crate::log::SecurityEvent::new(
                crate::log::SecurityEventType::AuthFailure,
                "warning",
                format!("渠道删除密码认证失败（邮箱: {email}）"),
            ));
        Err(error_response(
            "密码错误，无法执行删除操作",
            StatusCode::FORBIDDEN,
        ))
    }
}

// TODO: 继续迁移剩余 10 个 channel handler
// publish_channel, unpublish_channel, channel_health, channel_metrics 等

#[cfg(test)]
mod tests {
    use super::*;
    // 只在 #43 守卫里用 ⇒ 必须放在 `mod tests` 内（放模块顶层会在 lib 目标下
    // 变成 unused_import，而 `cargo check` 看不见，2de58e68 那轮踩过同型）。
    use std::collections::BTreeSet;

    /// 造渠道：**走 `to_channel`**（本模块唯一的「请求 → Channel」构造点），
    /// 不写字段全字面量 —— `Channel` 一加字段，字面量会集体 E0063
    /// （#32 PR1 就是这么把 main 编红的，4 个构造点 + 1 个 test target 构造点）。
    fn channel_for_mask() -> Channel {
        let req: ChannelRequest =
            serde_json::from_value(json!({ "name": "mask-test" })).expect("最小请求体可反序列化");
        req.to_channel("ch-mask".to_string())
    }

    /// PR2 要求①：两个含上游文案的键**都**过同一道脱敏——凭证形态必须消失。
    /// 只钉新加的 `balance_last_error` 而放过已在漏的 `last_error` 就是假闭合，
    /// 所以这里用同一个 `for` 覆盖两个键，而不是写两条各测各的。
    #[test]
    fn both_error_keys_share_one_redaction() {
        let mut ch = channel_for_mask();
        ch.last_error =
            Some(r#"上游返回 HTTP 401：invalid api key provided: sk-ABC123def456"#.to_string());
        ch.balance_last_error =
            Some(r#"余额查询失败：Authorization: Bearer eyJhbGciOiJIUzI1NiJ9"#.to_string());
        let v = mask_channel(&ch, None);

        for key in ["last_error", "balance_last_error"] {
            let s = v[key]
                .as_str()
                .unwrap_or_else(|| panic!("{key} 脱敏后应仍是字符串，实得 {:?}", v[key]));
            assert!(
                !s.contains("sk-ABC123def456"),
                "{key} 仍带明文 sk- 密钥: {s}"
            );
            assert!(
                !s.contains("eyJhbGciOiJIUzI1NiJ9"),
                "{key} 仍带明文 Bearer 令牌: {s}"
            );
        }
        // 掩码后的形状（证明走的是 sanitize 而不是整句抹掉）
        assert!(v["last_error"].as_str().unwrap().contains("sk-***"));
        assert!(v["balance_last_error"]
            .as_str()
            .unwrap()
            .contains("Bearer ***"));

        // 要求②：**非凭证的诊断文本原样保留**（运维排障靠这句话，抹了等于拆可观测性）
        assert_eq!(
            v["last_error"].as_str().unwrap(),
            r#"上游返回 HTTP 401：invalid api key provided: sk-***"#
        );
        assert_eq!(
            v["balance_last_error"].as_str().unwrap(),
            r#"余额查询失败：Authorization: Bearer ***"#
        );
    }

    /// 两键为 `None` 时必须序列化出 `null`，而不是 `"****"` / `""`——
    /// 前端「最近错误」列按 null 判空态，非空字符串会画出一条从未发生过的错误。
    #[test]
    fn absent_error_keys_stay_null() {
        let ch = channel_for_mask();
        let v = mask_channel(&ch, None);
        assert!(
            v["last_error"].is_null(),
            "last_error 缺省应为 null，实得 {:?}",
            v["last_error"]
        );
        assert!(
            v["balance_last_error"].is_null(),
            "balance_last_error 缺省应为 null，实得 {:?}",
            v["balance_last_error"]
        );
    }

    /// 三键里另两键**不该**被脱敏改动碰到：数字与时间戳是配置/运行态语义，
    /// 掩码它们会让前端周期列显示空值（`balance_query_interval_secs` 被抹 = 看起来没开）。
    #[test]
    fn interval_and_timestamp_keys_pass_through_unchanged() {
        let mut ch = channel_for_mask();
        ch.balance_query_interval_secs = Some(3600);
        ch.balance_last_query_at = Some(1_730_000_000);
        let v = mask_channel(&ch, None);
        assert_eq!(v["balance_query_interval_secs"], json!(3600));
        assert_eq!(v["balance_last_query_at"], json!(1_730_000_000));
        // 关闭态（PATCH 把 0/null 归一成 None）必须是 null，让前端画「关闭」
        // 而不是回落成 0（0 会被读成「每 0 秒查一次」）。
        let mut off = channel_for_mask();
        off.balance_query_interval_secs = None;
        assert!(mask_channel(&off, None)["balance_query_interval_secs"].is_null());
    }

    /// prev 夹具：已开定频 + 有巡检运行态读数（走 `to_channel` 再改字段，
    /// 不用全字段字面量——理由见 [`channel_for_mask`]）。
    fn channel_with_balance_runtime() -> Channel {
        let mut ch = channel_for_mask();
        ch.balance_query_interval_secs = Some(3600);
        ch.balance_last_query_at = Some(1_700_000_000);
        ch.balance_last_error = Some("上游返回 HTTP 401：invalid api key provided".to_string());
        ch
    }

    /// 回归钉子（lead 要求）：PUT 用 `to_channel()` 重建整条 Channel，而 `ChannelRequest`
    /// 不含余额巡检三键 ⇒ 必须按 prev 回填。缺了回填，用户在渠道表单里点一次保存就会
    /// **静默关掉周期余额巡检并清空读数**，且 PATCH 是唯一写路径 ⇒ 丢了补不回来。
    #[test]
    fn put_preserves_balance_triplet_from_prev() {
        let prev = channel_with_balance_runtime();
        // 模拟「只改名字」的一次表单保存：走同一个 to_channel 重建路径
        let mut fresh = channel_for_mask();
        fresh.name = "renamed-by-form".to_string();
        assert_eq!(
            fresh.balance_query_interval_secs, None,
            "前提：to_channel 重建后三键必为 None（否则这条测试钉不住缺陷本身）"
        );

        preserve_non_form_fields(&mut fresh, &prev);

        assert_eq!(
            fresh.balance_query_interval_secs,
            Some(3600),
            "一次表单保存不得关掉周期余额巡检"
        );
        assert_eq!(fresh.balance_last_query_at, Some(1_700_000_000));
        assert_eq!(fresh.balance_last_error, prev.balance_last_error);
        // 回填只搬「表单不携带」的字段，真正提交的内容不能被 prev 覆盖回去
        assert_eq!(fresh.name, "renamed-by-form", "回填越界覆盖了表单提交字段");
    }

    /// 关闭态也必须原样保留：回填不是「默认帮你打开」，也不该造出从未发生过的读数。
    /// 与上一合起来才钉住「回填 ＝ 逐键等于 prev」，而不是「回填 ＝ 写一组看起来合理的值」。
    #[test]
    fn put_preserves_closed_balance_triplet_without_fabricating() {
        let prev = channel_for_mask(); // 三键全 None
        let mut fresh = channel_with_balance_runtime();
        assert!(
            fresh.balance_query_interval_secs.is_some(),
            "前提：fresh 带着脏值进入"
        );

        preserve_non_form_fields(&mut fresh, &prev);

        assert_eq!(
            fresh.balance_query_interval_secs, None,
            "关闭态被回填成 Some = 表单保存把巡检打开了"
        );
        assert_eq!(fresh.balance_last_query_at, None);
        assert_eq!(fresh.balance_last_error, None);
    }

    // ================= #43：防「新字段被 PUT 静默清零」复发的派生守卫 =================
    //
    // 名单**一律从真实数据源派生，不许手写 `&'static str` 数组**（lead 2026-11-09 明确
    // 否决过手写台账：那本身就是第二份名单，加字段照样漏）。三个来源：
    // - 字段全集＝`Channel` 结构体文本（`declared_channel_fields`）；
    // - 表单侧＝`serde_json::to_value(&ChannelRequest)` 的键（`ChannelRequest` 因此加
    //   `Serialize`，只为派生名单，非行为变更）；
    // - 回填侧＝**真跑一遍 `preserve_non_form_fields`，取「值真的变了」的键**
    //   （名单由函数体自己交代）。
    //
    // 「运行态字段只能由产出它的那条路径写」是这条守卫与 PATCH 不接受
    // `balance_last_query_at`/`balance_last_error` 写入（写了等于造假）的共同精神。
    //
    // **正例模板**（将来给 `Channel` 加字段的人照这个办）：
    // `account_sessions.rs` 的 `resolve_or_create_auto_channel` 复用分支——它不重建
    // Channel，而是 `let mut updated = ch.clone();` 后逐字段覆盖再 `update`，
    // 所以新字段**天然保留**，不需要在第 N 个出口补回填。反例才是 PUT：表单不含的字段
    // 被 `to_channel()` 硬置 `None`，漏一处就静默清零。

    /// `Channel` 结构体声明的字段名清单——从**结构体文本**派生。
    ///
    /// 不用 `serde_json::to_value` 当权威清单：带 `skip_serializing_if` 的字段在值为空时
    /// 根本不出现，纯 serde 口径对「新加一条可跳过字段」是**静默漏检**，而漏检的守卫等于
    /// 没有守卫。结构体文本没有这个盲区。
    /// `include_str!` 编译期内联（不依赖 cwd，也不依赖 CI 镜像里装了什么），
    /// 先例＝`agent::registry.rs` 的 `ROUTE_SOURCES`。
    fn declared_channel_fields() -> BTreeSet<String> {
        const SRC: &str = include_str!("../../channel/mod.rs");
        const MARK: &str = "pub struct Channel {";
        let hits = SRC.matches(MARK).count();
        assert_eq!(
            hits, 1,
            "MARK `{}` 在 channel/mod.rs 里应恰好出现一次，实得 {} 次：结构体定义写法变了要同步本守卫",
            MARK, hits
        );
        let at = SRC.find(MARK).expect("上面已断言 MARK 存在");
        let bytes = SRC.as_bytes();
        let mut depth = 1usize;
        let mut i = at + MARK.len();
        while i < bytes.len() && depth > 0 {
            match bytes[i] {
                b'{' => depth += 1,
                b'}' => depth -= 1,
                _ => {}
            }
            i += 1;
        }
        let body = &SRC[at + MARK.len()..i - 1];
        let out: BTreeSet<String> = body.lines().filter_map(struct_field_name).collect();
        // 量级地板（同 registry.rs 的做法）：清单一旦塌掉，下面的 ⊆ 断言会「全体假绿」。
        assert!(
            out.len() >= 20,
            "从结构体只解析出 {} 个字段，量级应为 ~27：解析口径分叉",
            out.len()
        );
        out
    }

    /// 结构体正文里的一行是不是字段声明（属性行以 `#` 开头、文档注释以 `/` 开头，都不算）。
    /// 形状只认 `pub 名: 类型,` —— `Channel` 全字段都是 `pub`，非 pub 字段出现时这里会漏，
    /// 但漏掉的字段同时也不会被 `to_channel`/`preserve_non_form_fields` 写到，
    /// 且第 0) 段「结构体字段 == 序列化键」的断言会因为它对不上而响。
    fn struct_field_name(raw: &str) -> Option<String> {
        let rest = raw.trim().strip_prefix("pub ")?;
        let (name, _ty) = rest.split_once(':')?;
        Some(name.trim().to_string())
    }

    /// JSON 对象的键集合。
    fn json_keys(v: &Value) -> BTreeSet<String> {
        v.as_object()
            .map(|m| m.keys().cloned().collect())
            .expect("应为 JSON 对象")
    }

    /// 给每条字段灌**可区分的非默认值**。两个用途：当回填测试的 prev，
    /// 以及证明「序列化能看见全部字段」——三条带 `skip_serializing_if` 的
    /// （`quirks`/`balance_scheme`/`balance_unit`）必须被填上，否则它们从 JSON 里消失。
    /// 仍然只走 `channel_for_mask()`（＝`to_channel`）再改字段，不写字段全字面量。
    fn sentinel_channel() -> Channel {
        let mut ch = channel_for_mask();
        ch.id = "prev-sentinel".to_string();
        ch.name = "sentinel-name".to_string();
        ch.base_url = "https://sentinel.invalid".to_string();
        ch.api_key = "enc:sentinel".to_string();
        ch.priority = 7;
        ch.weight = 3;
        ch.status = "disabled".to_string();
        ch.models = vec!["sentinel-model".to_string()];
        ch.model_mapping = [("a".to_string(), "b".to_string())].into_iter().collect();
        ch.account_id = "acct-sentinel".to_string();
        ch.quirks = serde_json::from_value(json!({ "url_suffix": "?beta=true" }))
            .expect("最小特判可反序列化");
        ch.balance_scheme = "openai".to_string();
        ch.balance_unit = "USD".to_string();
        ch.response_time = Some(111);
        ch.test_time = Some(222);
        ch.balance = Some(3.5);
        ch.credit = Some(4.5);
        ch.discovered_models = vec!["discovered-1".to_string()];
        ch.last_used_at = Some(333);
        ch.last_error = Some("sentinel-last-error".to_string());
        ch.created_at = 1;
        ch.updated_at = 2;
        ch.balance_query_interval_secs = Some(3600);
        ch.balance_last_query_at = Some(1_700_000_000);
        ch.balance_last_error = Some("sentinel-balance-error".to_string());
        ch
    }

    /// 表单侧名单：最小请求体反序列化后**再序列化**。
    /// serde 会吐出全部声明字段（与输入带不带那些键无关），所以加字段自动进集合。
    fn form_field_names() -> BTreeSet<String> {
        let req: ChannelRequest =
            serde_json::from_value(json!({ "name": "form-keys" })).expect("最小请求体可反序列化");
        json_keys(
            &serde_json::to_value(&req)
                .expect("ChannelRequest 可序列化（加 Serialize 就是为了这一句）"),
        )
    }

    /// 回填侧名单：**真跑** `preserve_non_form_fields`，取「值真的变了」的键。
    fn backfilled_field_names() -> BTreeSet<String> {
        let prev = sentinel_channel();
        let mut rebuilt = channel_for_mask();
        let before = serde_json::to_value(&rebuilt).expect("Channel 可序列化");
        preserve_non_form_fields(&mut rebuilt, &prev);
        let after = serde_json::to_value(&rebuilt).expect("Channel 可序列化");
        let mut all = json_keys(&before);
        all.extend(json_keys(&after));
        let names: BTreeSet<String> = all
            .into_iter()
            .filter(|key| before.get(key.as_str()) != after.get(key.as_str()))
            .collect();
        names
    }

    /// #43 主守卫：`Channel` 字段 ⊆ 表单字段 ∪ PUT 回填名单 ∪ 处理器自赋字段。
    #[test]
    fn channel_fields_are_all_accounted_for_by_form_or_backfill() {
        let declared = declared_channel_fields();
        let form = form_field_names();
        let backfilled = backfilled_field_names();

        // 0) 哨兵的序列化键必须与结构体字段**一模一样**：这条先把「serde 视野完整性」钉住，
        //    否则后面所有派生名单都可能建立在一个漏了字段的世界视图上（rename/跳过都会在这里响）。
        let sentinel_json = json_keys(&serde_json::to_value(sentinel_channel()).expect("可序列化"));
        assert_eq!(
            declared, sentinel_json,
            "结构体字段与序列化键对不上：哨兵夹具没覆盖到某条带 skip_serializing_if 的字段，\
             或字段加了 #[serde(rename)]（本守卫按「Rust 字段名＝JSON 键名」口径写）"
        );

        // 1) 主断言：每条字段都要有归属。`id`/`updated_at` 是唯一无法从数据源派生的两条，
        //    各有明确理由（见下面第 3 段的自校验），不是「先塞进来再说」。
        let handler_set: BTreeSet<String> = ["id", "updated_at"]
            .into_iter()
            .map(str::to_string)
            .collect();
        // 用 `iter().filter()` 而不是任何「集合减法」写法：std 的 `BTreeSet` 只有
        // `difference`/`intersection` 这类**稳定**迭代器，`subtract` 不存在；本机不能编译，
        // 编不过的提交会卡住全队，所以这里只用最保守的 API 组合。
        let orphans: BTreeSet<String> = declared
            .iter()
            .filter(|k| {
                let k = k.as_str();
                !form.contains(k) && !backfilled.contains(k) && !handler_set.contains(k)
            })
            .cloned()
            .collect();
        assert!(
            orphans.is_empty(),
            "PUT /api/channels/:id 会静默清零这些字段：{orphans:?}。二选一：\
             加进 `ChannelRequest`（表单可编辑，to_channel 会带上）\
             或加进 `preserve_non_form_fields`（运行态，只能由产出它的那条路径写）。\
             另一条正路是别整条重建：见 account_sessions.rs 的 \
             `resolve_or_create_auto_channel` 复用分支（clone + 逐字段覆盖 ⇒ 天然保留）"
        );

        // 2) 回填名单不得与表单字段相交：相交＝用户在表单里提交的值会被 prev 盖回去。
        let clash: Vec<&String> = form.intersection(&backfilled).collect();
        assert!(
            clash.is_empty(),
            "preserve_non_form_fields 覆盖了表单字段 {clash:?}：提交值会被 prev 盖掉"
        );

        // 3) `handler_set` 自我收缩：这两条一旦不再是 Channel 字段、或有了归属（进了表单/
        //    回填名单），这条就响，逼着这里同步——防止它变成越塞越宽的垃圾桶。
        for key in ["id", "updated_at"] {
            assert!(
                declared.contains(key),
                "`{key}` 已不是 Channel 字段，handler_set 要同步删除"
            );
            assert!(
                !form.contains(key) && !backfilled.contains(key),
                "`{key}` 现在已有归属，不该再留在 handler_set"
            );
        }

        // 4) #32 PR2 修的那条缺陷本身不许从名单里滑出去（三键必须在回填名单里）。
        for key in [
            "balance_query_interval_secs",
            "balance_last_query_at",
            "balance_last_error",
        ] {
            assert!(
                backfilled.contains(key),
                "余额巡检三键之一 `{key}` 不在回填名单里：PUT 又会把它清零了"
            );
        }
    }
}
