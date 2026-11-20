//! 脚本插件管理面 API（`/api/plugins/scripts*`，Magpie 脚本包）。
//!
//! ## 与原生插件管理面（`plugins.rs`）的关系
//!
//! 原生包是「一包一子进程的 Rust 二进制」，脚本包是「一个 Bun 进程承载全部
//! npm 风格脚本」（见 `plugin_host::script` 模块文档）。两组端点并行共存：
//! 原生包走 `/api/plugins*`（本文件不触碰），脚本包走 `/api/plugins/scripts*`。
//!
//! ## 端点契约（前端 `ScriptPluginsPanel` / api 层按此消费）
//!
//! | 端点 | 方法 | 说明 |
//! |---|---|---|
//! | `/api/plugins/scripts` | GET | 宿主状态 + 已装包列表（含 providerRouted） |
//! | `/api/plugins/scripts/catalog` | GET | Magpie 目录状态 + 条目（camelCase） |
//! | `/api/plugins/scripts/refresh` | POST | 手动刷新目录（force，无视 TTL） |
//! | `/api/plugins/scripts/install` | POST | `{selector, trusted}` 安装（几十秒，直 await） |
//! | `/api/plugins/scripts/enable` | POST | `{id, version?, enabled}` 启停（rebuild 失败自动回滚） |
//! | `/api/plugins/scripts/uninstall` | POST | `{id, version?}` 卸载（先停用再删盘） |
//! | `/api/plugins/scripts/options` | POST | `{id, version?, values}` 保存包配置 |
//! | `/api/plugins/scripts/rebuild` | POST | 手动重启宿主（运维排障用） |
//!
//! 全部走管理面鉴权（`verify_admin`）；操作语义都在
//! `plugin_host::script::operations`，本层只做参数校验 + 视图投影。
//!
//! ## JSON 形状
//!
//! - 已装包/宿主状态用手写视图：`source`/`publisher` 出小写字符串
//!   （`"magpie"`/`"official"`…，前端徽标直接比字符串），时间戳出
//!   camelCase（`installedAt`/`lastError`）；
//! - 目录侧（Status/PublicEntry）直接 serde 透传：Status 本就 snake_case，
//!   PublicEntry 本就 camelCase（`magpieOfficial` 等），不二次投影。

use std::collections::HashSet;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use super::common::{error_response, verify_admin};
use crate::api::openai::AppState;
use crate::plugin_host::script::operations;
use crate::plugin_host::script::runtime::ScriptHostStatus;
use crate::plugin_host::script::types::ScriptPackage;

/// 管理面写操作串行闸。
///
/// `operations` 的落盘是「读 KV → 改 → 整表写回」（见 `script/storage.rs`），
/// 跨请求并发会互相覆盖（丢更新）；安装/卸载还会同时动包目录与触发宿主重建。
/// 管理面（含 Agent 工具经 `registry/w_resources` 复用的同一批 handler）在这里
/// 串行：安装几十秒期间其它写操作排队等待——与前端 busy 互斥的语义一致，
/// 「同一时刻只有一个写操作」。
///
/// 只包写端点；GET 与 refresh（纯唤醒后台任务）不占锁。
static WRITE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

// ── 错误文案整形 ────────────────────────────────────────────────────

/// 管理面错误文案上限（字符数）。宿主启动失败时底层会把子进程 stderr
/// 全文塞进 error（生产实测 5937 字节、40 行 `Error relocating ...`），
/// 直接回给前端既撑爆 UI 也毫无信息量。
const MAX_ERROR_CHARS: usize = 512;

/// 截断后追加的指引（日志在服务端，前端拿不到）。
const ERROR_LOG_HINT: &str = "…（完整日志见 journalctl -u aigx）";

/// 已知根因归类：命中即返回一句可操作的提示。
///
/// 宿主（Bun）起不来的常见原因在 error 里只表现为一大段 loader 报错，
/// 归类后用户才知道下一步该做什么。
fn classify_host_error(raw: &str) -> Option<&'static str> {
    let lower = raw.to_lowercase();
    let bun_load_failure = lower.contains("bun runtime unavailable")
        || lower.contains("error loading shared library")
        || lower.contains("error relocating");
    if bun_load_failure {
        return Some(
            "Bun 运行时无法在当前系统加载（libc 或 CPU 架构不匹配）。请查看 journalctl -u aigx 获取详情。",
        );
    }
    // 「找不到 bun 可执行文件」：error 里通常带 bun 路径 + No such file。
    if lower.contains("no such file or directory") && lower.contains("bun") {
        return Some(
            "找不到 Bun 可执行文件。请确认 Bun 已安装且网关进程的 PATH 可见（journalctl -u aigx 查看详情）。",
        );
    }
    None
}

/// 把底层错误整形成「归类提示 + 截断摘要」。
///
/// 语义不变：调用方仍以 `success:false` + 4xx 返回，前端照旧按失败处理；
/// 这里只改文案长度与可读性。归类命中时直接返回归类提示（丢掉 stderr
/// 噪声），否则保留前 3 行 + 末 1 行、总长 ≤ [`MAX_ERROR_CHARS`]。
pub fn summarize_error(raw: &str) -> String {
    if let Some(hint) = classify_host_error(raw) {
        return hint.to_string();
    }
    let lines: Vec<&str> = raw.lines().filter(|l| !l.trim().is_empty()).collect();
    let mut picked: Vec<&str> = Vec::new();
    if lines.len() <= 4 {
        picked.extend(lines.iter().copied());
    } else {
        picked.extend(lines.iter().take(3).copied());
        picked.push(lines[lines.len() - 1]);
    }
    let joined = picked.join("\n");
    if joined.chars().count() <= MAX_ERROR_CHARS {
        return joined;
    }
    // 截断时预留指引长度，保证「摘要 + 指引」仍在上限内。
    let budget = MAX_ERROR_CHARS.saturating_sub(ERROR_LOG_HINT.chars().count());
    let mut truncated: String = joined.chars().take(budget).collect();
    truncated.push_str(ERROR_LOG_HINT);
    truncated
}

// ── 请求体 ──────────────────────────────────────────────────────────

/// 安装请求体。`trusted` 必须由前端二次确认弹窗后显式传 true——
/// 脚本不是沙箱，将以 Bun 进程权限运行（operations 强制，缺省 false）。
///
/// 兼容两套字段名：
/// - AIGX 原生：`{selector, trusted}`；
/// - A2A（`POST /api/plugins/scripts/install`）：`{source|name, version,
///   trusted, id}`。`id` 是「更新已装包」的期望 id（防止装成别的包）。
#[derive(Debug, Deserialize)]
pub struct ScriptInstallBody {
    /// 安装指示串：`magpie:{name}` / 裸 npm 名 / `github:{owner/repo}` / `file:{路径}`。
    #[serde(default)]
    pub selector: String,
    /// A2A 字段：来源/包名（与 `selector` 同义，二选一）。
    #[serde(default)]
    pub source: Option<String>,
    /// A2A 字段：包名（`source` 的别名）。
    #[serde(default)]
    pub name: Option<String>,
    /// A2A 字段：显式版本（缺省 = 目录/npm 的 latest）。
    #[serde(default)]
    pub version: Option<String>,
    /// A2A 字段：期望包 id（更新场景的防呆；与下载结果不一致时拒绝）。
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub trusted: bool,
}

impl ScriptInstallBody {
    /// 归一成 `(selector, version)`；`selector` 为空时报错。
    fn resolve(&self) -> Result<(String, Option<String>), &'static str> {
        let raw = if !self.selector.trim().is_empty() {
            self.selector.trim().to_string()
        } else if let Some(source) = self
            .source
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            source.to_string()
        } else if let Some(name) = self
            .name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            name.to_string()
        } else {
            return Err("安装指示串（selector/source/name）不能为空");
        };
        // selector 已带 `@版本` 时以 selector 为准（A2A 的 version 字段只
        // 在 selector 无版本时附加）
        let version = match self
            .version
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            Some(version) if !raw.contains('@') || raw.starts_with('@') => {
                Some(version.to_string())
            }
            _ => None,
        };
        Ok((raw, version))
    }
}

/// 启停请求体。多版本共存时按 `version` 选中，缺省取最后登记的同 id 包。
#[derive(Debug, Deserialize)]
pub struct ScriptEnableBody {
    pub id: String,
    #[serde(default)]
    pub version: Option<String>,
    pub enabled: bool,
}

/// A2A 通用变更请求体（`POST /api/plugins/scripts/change`）。
///
/// `action`：`enable`（配 `enabled`）/ `reload` / `provider`（配
/// `provider` + `enabled`，单独启停包内一家）/ `remove` / `checkin`（no-op，
/// 签到在签到中心管理）。
#[derive(Debug, Deserialize)]
pub struct ScriptChangeBody {
    pub id: String,
    #[serde(default)]
    pub version: Option<String>,
    pub action: String,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub provider: Option<String>,
}

/// A2A 设置请求体（`POST /api/plugins/scripts/settings`）。
///
/// 三个字段都可选：`options`（包级参数）/ `providerOptions`（provider 开关）
/// 是补丁；都不带 = 只读（返回当前设置视图）。
#[derive(Debug, Deserialize)]
pub struct ScriptSettingsBody {
    pub id: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub options: Option<serde_json::Map<String, Value>>,
    #[serde(default, rename = "providerOptions")]
    pub provider_options: Option<serde_json::Map<String, Value>>,
}

/// A2A prompts 请求体（`POST /api/plugins/scripts/prompts`）。
#[derive(Debug, Deserialize)]
pub struct ScriptPromptsBody {
    pub provider: String,
    /// 透传给插件 `admin.action` 的 payload（动态 prompts 的输入）。
    #[serde(default)]
    pub payload: Value,
}

/// 卸载请求体（卸载 = 先停用再删元数据与磁盘目录；Local 来源不删本机目录）。
#[derive(Debug, Deserialize)]
pub struct ScriptUninstallBody {
    pub id: String,
    #[serde(default)]
    pub version: Option<String>,
}

/// 配置保存请求体。`values` 是逐键配置对象（键与包 settings_schema 对应，
/// 字节上限 ≤ 256KiB 由 settings 校验，超限 400）。
#[derive(Debug, Deserialize)]
pub struct ScriptOptionsBody {
    pub id: String,
    #[serde(default)]
    pub version: Option<String>,
    pub values: serde_json::Map<String, Value>,
}

// ── 视图 ────────────────────────────────────────────────────────────

/// 宿主状态 → 前端 JSON（`lastError` camelCase；providers 是实际接管 id 列表）。
fn host_view(s: &ScriptHostStatus) -> Value {
    json!({
        "running": s.running,
        "packages": s.packages,
        "providers": s.providers,
        "lastError": s.last_error,
        "crashes": s.crashes,
    })
}

/// 已装包 → 前端 JSON（前端 `ScriptPackageView` 契约）。
///
/// - `source`/`publisher` 出小写字符串（`"magpie"` / `"official"`…）；
/// - `providerRouted`：包声明的任一 provider 在宿主路由表里（宿主在跑且
///   装载成功）；宿主没跑时恒 false——前端据此显示「已接管/未接管」。
fn package_view(p: &ScriptPackage, routed_providers: &HashSet<String>) -> Value {
    let provider_routed = p
        .providers
        .iter()
        .any(|pp| routed_providers.contains(&pp.id));
    json!({
        "id": p.id,
        "label": p.label,
        "version": p.version,
        // 图标：图标缓存的公开路径（`/plugins/script-icons/{file}`，文件名
        // 即内容摘要）。未命中（未启用/尚未刷新）为 null，前端回落首字母。
        "icon": crate::plugin_host::script::icons::installed(p),
        "source": p.source.as_str(),
        "publisher": p.publisher.as_str(),
        "providers": p
            .providers
            .iter()
            .map(|pp| json!({
                "id": pp.id,
                "label": pp.label,
                "upstream": pp.upstream,
                "models": pp.models,
                // 包内 provider 级启停的投影（`disabled_providers`，由
                // `handle_script_change(action=provider)` 写入；视图不反映
                // 会让前端开关「点了停用刷新又变回启用」的假象）
                "enabled": !p.disabled_providers.iter().any(|id| id == &pp.id),
            }))
            .collect::<Vec<_>>(),
        "entry": p.entry,
        "sourceRef": p.source_ref,
        "repository": p.repository,
        "summary": p.summary,
        "installedAt": p.installed_at,
        "enabled": p.enabled,
        "trusted": p.trusted,
        "options": { "values": p.options.values },
        // 宿主 `script.packages` 回填的包级设置声明（`settings` 原样），
        // 管理面「提供商设置 / 包配置」表单据此渲染。
        "settingsSchema": p.settings_schema,
        "providerRouted": provider_routed,
    })
}

// ── 端点 ────────────────────────────────────────────────────────────

/// GET /api/plugins/scripts —— 宿主状态 + 全部已装脚本包。
pub async fn handle_list_script_packages(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let host = state.script_host.status();
    let routed: HashSet<String> = state.script_host.routed_providers().into_iter().collect();
    let packages: Vec<Value> = operations::list(&state.script_host)
        .iter()
        .map(|p| package_view(p, &routed))
        .collect();
    Ok(Json(json!({
        "success": true,
        "data": { "host": host_view(&host), "packages": packages }
    })))
}

/// GET /api/plugins/scripts/catalog —— Magpie 目录状态 + 全部条目。
///
/// 条目已按 publisher rank → stars 降序 → label 升序排好（目录侧保证），
/// 前端按 `magpieOfficial` / `magpieRecommended` 分组即可，不必再排。
pub async fn handle_script_catalog(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let catalog = state.script_host.catalog();
    let status = catalog.status().await;
    let entries = catalog.public_entries().await;
    let entries_v: Vec<Value> = entries
        .iter()
        .map(|e| serde_json::to_value(e).unwrap_or(Value::Null))
        .collect();
    Ok(Json(json!({
        "success": true,
        "data": {
            "status": serde_json::to_value(&status).unwrap_or(Value::Null),
            "entries": entries_v,
        }
    })))
}

/// POST /api/plugins/scripts/refresh —— 手动刷新目录（force 无视 TTL；
/// 限流重试闸门仍生效——刷新是异步后台任务，本端点立即返回）。
pub async fn handle_script_refresh(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    state.script_host.catalog().wake(true);
    tracing::info!("[Script] 手动刷新 Magpie 目录（force）");
    Ok(Json(json!({ "success": true, "data": { "ok": true } })))
}

/// POST /api/plugins/scripts/install —— 安装脚本包（下载/解包/依赖安装，
/// 可能几十秒；axum handler 直 await）。成功返回装好的包视图（未启用）。
///
/// 兼容 AIGX `{selector}` 与 A2A `{source|name, version, id}` 两套字段。
pub async fn handle_script_install(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ScriptInstallBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let (selector, version) = body
        .resolve()
        .map_err(|e| error_response(e, StatusCode::BAD_REQUEST))?;
    // A2A 的 `version` 字段：selector 不带版本时附加（`name@version` 形态
    // 由 source::parse 解析；magpie 走 `magpie:{name}@{version}`）
    let selector = match version {
        Some(version) if !selector.contains('@') || selector.starts_with('@') => {
            format!("{selector}@{version}")
        }
        _ => selector,
    };
    let _write = WRITE_LOCK.lock().await;
    let package = operations::install(&state.script_host, &selector, body.trusted)
        .await
        .map_err(|e| error_response(&summarize_error(&e), StatusCode::BAD_REQUEST))?;
    // A2A 的 `id` 是「更新已装包」的防呆：下载结果与期望不一致时保留原
    // 元数据不动（install 已落盘 → 需要显式提示，不静默覆盖）
    if let Some(expected) = body.id.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        if expected != package.id {
            return Err(error_response(
                &format!(
                    "下载的脚本插件包名（{}）与待更新插件（{expected}）不一致，已安装为独立包，请手工卸载",
                    package.id
                ),
                StatusCode::BAD_REQUEST,
            ));
        }
    }
    tracing::info!(
        "[Script] 管理面安装脚本包 {}（v{}，来源 {}）",
        package.id,
        package.version,
        package.source.as_str()
    );
    let routed: HashSet<String> = state.script_host.routed_providers().into_iter().collect();
    Ok(Json(json!({
        "success": true,
        "data": { "package": package_view(&package, &routed) }
    })))
}

/// POST /api/plugins/scripts/change —— A2A 通用变更入口。
///
/// `action`：
/// - `enable`：启停整个包（配 `enabled`，等价 `/enable`）；
/// - `reload`：重建宿主（等价 `/rebuild`，要求包已启用）；
/// - `provider`：单独启停包内某一家（配 `provider` + `enabled`）；
/// - `remove`：卸载（等价 `/uninstall`）；
/// - `checkin`：no-op（签到在签到中心管理，A2A 同口径）。
pub async fn handle_script_change(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ScriptChangeBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let id = body.id.trim().to_string();
    if id.is_empty() {
        return Err(error_response("包 id 不能为空", StatusCode::BAD_REQUEST));
    }
    let _write = WRITE_LOCK.lock().await;
    match body.action.as_str() {
        "enable" => {
            let enabled = body
                .enabled
                .ok_or_else(|| error_response("缺少 enabled", StatusCode::BAD_REQUEST))?;
            operations::set_enabled(&state.script_host, &id, body.version.as_deref(), enabled)
                .await
                .map_err(|e| error_response(&summarize_error(&e), StatusCode::BAD_REQUEST))?;
        }
        "reload" => {
            state
                .script_host
                .rebuild("manual")
                .await
                .map_err(|e| error_response(&summarize_error(&e), StatusCode::BAD_REQUEST))?;
        }
        "provider" => {
            let provider = body
                .provider
                .as_deref()
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .ok_or_else(|| error_response("缺少 provider", StatusCode::BAD_REQUEST))?;
            let enabled = body
                .enabled
                .ok_or_else(|| error_response("缺少 enabled", StatusCode::BAD_REQUEST))?;
            operations::set_provider_enabled(&state.script_host, &id, provider, enabled)
                .await
                .map_err(|e| error_response(&summarize_error(&e), StatusCode::BAD_REQUEST))?;
        }
        "remove" => {
            operations::uninstall(&state.script_host, &id, body.version.as_deref())
                .await
                .map_err(|e| error_response(&summarize_error(&e), StatusCode::BAD_REQUEST))?;
        }
        // 签到及附带任务默认允许，统一在签到中心管理（A2A 同文案）
        "checkin" => {}
        other => {
            return Err(error_response(
                &format!("未知的脚本插件操作：{other}"),
                StatusCode::BAD_REQUEST,
            ))
        }
    }
    tracing::info!("[Script] 管理面变更脚本包 {id}（action={}）", body.action);
    Ok(Json(json!({
        "success": true,
        "data": { "host": host_view(&state.script_host.status()) }
    })))
}

/// POST /api/plugins/scripts/settings —— A2A 设置入口。
///
/// 三个字段都可选：`options` / `providerOptions` 是补丁；都不带 = 只读
/// （返回当前设置视图，含宿主回填的 settings_schema）。
///
/// 写入走 [`crate::plugin_host::script::runtime::ScriptHostManager::configure_settings`]：
/// 只改 providerOptions 时经 `script.settings` 在**运行中的宿主**上应用并
/// 回最新 schema（不重启 Bun）；options 变化才落盘 + 重建宿主。
pub async fn handle_script_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ScriptSettingsBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let id = body.id.trim().to_string();
    if id.is_empty() {
        return Err(error_response("包 id 不能为空", StatusCode::BAD_REQUEST));
    }
    if body.options.is_none() && body.provider_options.is_none() {
        // 只读：返回当前设置视图
        let package = operations::list(&state.script_host)
            .into_iter()
            .rfind(|p| p.id == id && body.version.as_deref().is_none_or(|v| p.version == v))
            .ok_or_else(|| error_response("包未安装", StatusCode::NOT_FOUND))?;
        return Ok(Json(json!({
            "success": true,
            "data": {
                "id": package.id,
                "options": package.options.values,
                "providerOptions": package.provider_options,
                "schema": package.settings_schema,
            }
        })));
    }
    let _write = WRITE_LOCK.lock().await;
    let package = state
        .script_host
        .configure_settings(
            &id,
            body.version.as_deref(),
            body.options,
            body.provider_options,
        )
        .await
        .map_err(|e| error_response(&summarize_error(&e), StatusCode::BAD_REQUEST))?;
    let routed: HashSet<String> = state.script_host.routed_providers().into_iter().collect();
    Ok(Json(json!({
        "success": true,
        "data": { "package": package_view(&package, &routed) }
    })))
}

/// POST /api/plugins/scripts/prompts —— A2A 登录提示词入口。
///
/// 透传插件 `admin.action`（`action = "auth.prompts"`）：插件按自身登录
/// 方式返回动态 prompts（`login-prompts.mjs` 的 `resolvePrompts`）。
pub async fn handle_script_prompts(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ScriptPromptsBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let provider = body.provider.trim().to_string();
    if provider.is_empty() {
        return Err(error_response("provider 不能为空", StatusCode::BAD_REQUEST));
    }
    let value = state
        .script_host
        .call_provider(
            &provider,
            "admin.action",
            json!({
                "provider": provider,
                "action": "auth.prompts",
                "payload": body.payload,
            }),
            std::time::Duration::from_secs(10),
        )
        .await
        .map_err(|e| error_response(&summarize_error(&e), StatusCode::SERVICE_UNAVAILABLE))?;
    Ok(Json(json!({ "success": true, "data": value })))
}

/// GET /api/plugins/scripts/check —— A2A 检查更新入口。
pub async fn handle_script_check(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let result = operations::check_updates(&state.script_host).await;
    Ok(Json(json!({ "success": true, "data": result })))
}

/// POST /api/plugins/scripts/catalog/refresh —— A2A 目录刷新别名
/// （等价 `/refresh`）。
pub async fn handle_script_catalog_refresh(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    state.script_host.catalog().wake(true);
    tracing::info!("[Script] 手动刷新 Magpie 目录（A2A catalog/refresh）");
    Ok(Json(json!({ "success": true, "data": { "ok": true } })))
}

/// POST /api/plugins/scripts/enable —— 启停脚本包（rebuild 失败自动回滚）。
/// 返回最新宿主状态（启用/停用都会触发宿主重建）。
pub async fn handle_script_enable(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ScriptEnableBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let id = body.id.trim().to_string();
    if id.is_empty() {
        return Err(error_response("包 id 不能为空", StatusCode::BAD_REQUEST));
    }
    let _write = WRITE_LOCK.lock().await;
    operations::set_enabled(
        &state.script_host,
        &id,
        body.version.as_deref(),
        body.enabled,
    )
    .await
    .map_err(|e| error_response(&summarize_error(&e), StatusCode::BAD_REQUEST))?;
    tracing::info!(
        "[Script] 管理面{}脚本包 {}",
        if body.enabled { "启用" } else { "停用" },
        id
    );
    Ok(Json(json!({
        "success": true,
        "data": { "host": host_view(&state.script_host.status()) }
    })))
}

/// POST /api/plugins/scripts/uninstall —— 卸载脚本包（先停用再删盘）。
pub async fn handle_script_uninstall(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ScriptUninstallBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let id = body.id.trim().to_string();
    if id.is_empty() {
        return Err(error_response("包 id 不能为空", StatusCode::BAD_REQUEST));
    }
    let _write = WRITE_LOCK.lock().await;
    operations::uninstall(&state.script_host, &id, body.version.as_deref())
        .await
        .map_err(|e| error_response(&summarize_error(&e), StatusCode::BAD_REQUEST))?;
    Ok(Json(json!({ "success": true, "data": { "ok": true } })))
}

/// POST /api/plugins/scripts/options —— 保存包配置（启用中的包会重建宿主
/// 让脚本读到新值；宿主重建失败时配置已落盘，错误文案如实说明）。
pub async fn handle_script_options(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ScriptOptionsBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let id = body.id.trim().to_string();
    if id.is_empty() {
        return Err(error_response("包 id 不能为空", StatusCode::BAD_REQUEST));
    }
    let _write = WRITE_LOCK.lock().await;
    let package = operations::update_options(
        &state.script_host,
        &id,
        body.version.as_deref(),
        body.values,
    )
    .await
    .map_err(|e| error_response(&summarize_error(&e), StatusCode::BAD_REQUEST))?;
    let routed: HashSet<String> = state.script_host.routed_providers().into_iter().collect();
    Ok(Json(json!({
        "success": true,
        "data": { "package": package_view(&package, &routed) }
    })))
}

/// POST /api/plugins/scripts/rebuild —— 手动重启脚本宿主（崩溃连续重试
/// 停止后 / 排障时用；无已启用包时等于关停宿主）。
pub async fn handle_script_rebuild(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let _write = WRITE_LOCK.lock().await;
    let status = state
        .script_host
        .rebuild("manual")
        .await
        .map_err(|e| error_response(&summarize_error(&e), StatusCode::BAD_REQUEST))?;
    tracing::info!("[Script] 管理面手动重启脚本宿主");
    Ok(Json(json!({
        "success": true,
        "data": { "host": host_view(&status) }
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin_host::script::types::{ScriptProviderInfo, ScriptSettingsOptions};

    fn sample_package() -> ScriptPackage {
        ScriptPackage {
            id: "@magpie-community/kiro".into(),
            label: "Kiro".into(),
            version: "1.2.0".into(),
            source_ref: "@magpie-community/kiro".into(),
            repository: Some("kiro-community/kiro".into()),
            summary: Some("Kiro 接入".into()),
            installed_at: 1_700_000_000,
            enabled: true,
            trusted: true,
            providers: vec![ScriptProviderInfo {
                id: "kiro".into(),
                label: "Kiro".into(),
                upstream: Some("anthropic".into()),
                models: vec![],
                ..Default::default()
            }],
            options: ScriptSettingsOptions::default(),
            ..Default::default()
        }
    }

    #[test]
    fn package_view_shape() {
        let mut routed = HashSet::new();
        routed.insert("kiro".to_string());
        let v = package_view(&sample_package(), &routed);
        // 契约键（前端 ScriptPackageView）
        assert_eq!(v["id"], "@magpie-community/kiro");
        assert_eq!(v["version"], "1.2.0");
        assert_eq!(v["source"], "magpie");
        assert_eq!(v["publisher"], "official");
        assert_eq!(v["installedAt"], 1_700_000_000);
        assert_eq!(v["enabled"], true);
        assert_eq!(v["trusted"], true);
        assert_eq!(v["providerRouted"], true);
        assert_eq!(v["sourceRef"], "@magpie-community/kiro");
        assert_eq!(v["providers"][0]["id"], "kiro");
        assert_eq!(v["providers"][0]["upstream"], "anthropic");
        assert_eq!(
            v["providers"][0]["enabled"], true,
            "未停用的 provider 默认 enabled"
        );
        assert!(v["options"]["values"].is_object());
        // 包内 provider 级停用 → 视图投影 enabled=false（前端开关据此显示）
        let mut disabled = sample_package();
        disabled.disabled_providers = vec!["kiro".into()];
        let v = package_view(&disabled, &routed);
        assert_eq!(
            v["providers"][0]["enabled"], false,
            "disabled_providers 必须投影到视图"
        );
        // 路由表为空 → providerRouted=false
        let v = package_view(&sample_package(), &HashSet::new());
        assert_eq!(v["providerRouted"], false);
        // 宿主回填的 settings_schema 必须透出给前端（设置页表单数据源）
        let mut with_schema = sample_package();
        with_schema.settings_schema = json!({ "version": 1, "providerOptions": {} });
        let v = package_view(&with_schema, &routed);
        assert_eq!(v["settingsSchema"]["version"], 1);
    }

    #[test]
    fn host_view_shape() {
        let s = ScriptHostStatus {
            running: true,
            packages: 2,
            providers: vec!["kiro".into()],
            last_error: Some("boom".into()),
            crashes: 3,
        };
        let v = host_view(&s);
        assert_eq!(v["running"], true);
        assert_eq!(v["packages"], 2);
        assert_eq!(v["providers"][0], "kiro");
        assert_eq!(v["lastError"], "boom");
        assert_eq!(v["crashes"], 3);
    }

    /// 信任闸门的线上契约：`trusted` 缺省必须是 false（安装必须显式确认），
    /// 绝不能因为字段漏传而在反序列化层被放大成 true。
    #[test]
    fn install_body_trusted_defaults_false() {
        let body: ScriptInstallBody = serde_json::from_value(json!({
            "selector": "magpie:kiro"
        }))
        .expect("selector only should deserialize");
        assert!(!body.trusted, "trusted 缺省必须为 false");

        let body: ScriptInstallBody = serde_json::from_value(json!({
            "selector": "magpie:kiro",
            "trusted": true
        }))
        .expect("explicit trusted should deserialize");
        assert!(body.trusted);
    }

    /// `version` 为可选字段：多版本共存时管理面必须能按 version 精确操作。
    #[test]
    fn enable_uninstall_body_version_optional() {
        let body: ScriptEnableBody =
            serde_json::from_value(json!({ "id": "a", "enabled": true })).unwrap();
        assert_eq!(body.version, None);
        let body: ScriptEnableBody =
            serde_json::from_value(json!({ "id": "a", "version": "1.0.0", "enabled": true }))
                .unwrap();
        assert_eq!(body.version.as_deref(), Some("1.0.0"));
        let body: ScriptUninstallBody = serde_json::from_value(json!({ "id": "a" })).unwrap();
        assert_eq!(body.version, None);
    }

    /// 生产实测形态：宿主启动失败时 error 是 40 行 loader 报错（5937 字节）。
    /// 归类后必须只剩一句可操作提示。
    #[test]
    fn summarize_bun_loader_failure_is_classified() {
        let mut raw = String::from(
            "宿主重建失败，已回滚启停状态：启动脚本宿主失败: Bun runtime unavailable: bun --version 退出码 Some(127)：Error loading shared library libstdc++.so.6: No such file or directory\n",
        );
        for i in 0..40 {
            raw.push_str(&format!(
                "Error relocating /opt/aigx/bun: symbol __libc_start_main version GLIBC_2.34 not defined ({i})\n"
            ));
        }
        let out = summarize_error(&raw);
        assert!(
            out.chars().count() <= MAX_ERROR_CHARS,
            "must be truncated: {} chars",
            out.chars().count()
        );
        assert!(out.contains("Bun 运行时"), "got: {out}");
        assert!(
            !out.contains("Error relocating"),
            "stderr noise must be dropped: {out}"
        );
    }

    /// 非归类命中：保留前 3 行 + 末 1 行；未超上限时不加指引（不给用户噪声）。
    #[test]
    fn summarize_plain_error_keeps_head_and_tail() {
        let raw: String = (0..40)
            .map(|i| format!("line-{i}-{}", "x".repeat(20)))
            .collect::<Vec<_>>()
            .join("\n");
        let out = summarize_error(&raw);
        assert!(out.chars().count() <= MAX_ERROR_CHARS);
        assert!(out.contains("line-0-"));
        assert!(out.contains("line-2-"));
        assert!(out.contains("line-39-"));
        assert!(!out.contains("line-10-"));
        assert!(!out.ends_with(ERROR_LOG_HINT));
    }

    /// 超长摘要：截断到上限并追加日志指引（指引本身也在上限内）。
    #[test]
    fn summarize_long_error_truncates_with_hint() {
        let raw: String = (0..40)
            .map(|i| format!("line-{i}-{}", "x".repeat(200)))
            .collect::<Vec<_>>()
            .join("\n");
        let out = summarize_error(&raw);
        assert!(out.chars().count() <= MAX_ERROR_CHARS);
        assert!(out.ends_with(ERROR_LOG_HINT));
        assert!(out.contains("line-0-"));
    }

    /// 短错误原样透传（不要给用户加噪声）。
    #[test]
    fn summarize_short_error_passthrough() {
        assert_eq!(summarize_error("包 id 不能为空"), "包 id 不能为空");
        assert_eq!(summarize_error(""), "");
    }
}
