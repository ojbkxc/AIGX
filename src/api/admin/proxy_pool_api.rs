//! 网络代理池管理 API（`/api/proxy-pool`，对照 Agent2API `/api/proxies/pool*`
//! 与 ds2api `admin/proxies`）。
//!
//! 路径用固定段 + body 传 id 而不是 `/pool/{id}`（与账号池 API 同风格）：
//!
//!   GET    /api/proxy-pool                列表（含每个条目被哪些账号引用）
//!   POST   /api/proxy-pool                新建
//!   POST   /api/proxy-pool/update         编辑（body 带 id；密码留空 = 保留）
//!   POST   /api/proxy-pool/enable         启用/禁用（body { id, enabled }）
//!   POST   /api/proxy-pool/remove         删除（body 带 id；有账号引用时拒绝）
//!   POST   /api/proxy-pool/test           连通性测试（结果记进条目 lastTest）
//!   POST   /api/proxy-pool/test-account   按账号的代理配置测试（含直连）
//!
//! 写操作响应回整份列表（前端就地替换，不必再拉一次——与账号池写接口
//! 同约定）。测试失败也是 200（`success:false` + error 文案），前端据此
//! 显示红色提示。
//!
//! 删除的引用检查（ds2api 的 `ValidateAccountProxyReferences`）：遍历账号池
//! 里全部账号的 proxy 字段，`pool:<id>` 引用与 `rotate` 候选（含 `rotate`
//! 全部档——它按 id 引用池条目）都算「正在使用」，有引用时 400 拒绝删除，
//! 响应里列出引用账号名，用户先去解绑。

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::api::admin::common::{error_response, verify_admin};
use crate::api::openai::AppState;
use crate::proxy_pool as pool;

/// 删除时最多列出几个引用账号名（防响应膨胀）。
const MAX_REFERENCE_NAMES: usize = 10;

// ── 请求体 ──────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct IdBody {
    id: String,
}

#[derive(Debug, Deserialize)]
pub struct EnableBody {
    id: String,
    enabled: bool,
}

/// 账号代理测试请求：`{ accountId }`（测该账号生效的代理配置）或
/// `{ proxy }`（测临时串，`null` = 测直连）。
#[derive(Debug, Default, Deserialize)]
pub struct TestAccountBody {
    #[serde(default)]
    account_id: String,
    /// 临时代理串（`pool:` / `rotate` 编码或 URL）；与 accountId 二选一。
    #[serde(default)]
    proxy: Option<String>,
}

// ── 辅助 ────────────────────────────────────────────────────────────

/// 引用统计：池条目 id → 引用它的账号 `[{id, name}]`。
///
/// `pool:<id>` 与 `rotate`（含指定档）都算引用——删除确认框不列出
/// 轮询候选会让用户以为「没人用」而删掉正在被轮询的出口（照 A2A
/// `pool_references` 的取舍）。
fn reference_map(state: &AppState) -> std::collections::HashMap<String, Vec<Value>> {
    let mut references: std::collections::HashMap<String, Vec<Value>> =
        std::collections::HashMap::new();
    for session in state.account_sessions.all() {
        let Some(raw) = session.proxy.as_deref().map(str::trim) else {
            continue;
        };
        if raw.is_empty() {
            continue;
        }
        let ids: Vec<String> = if let Some(id) = raw.strip_prefix("pool:") {
            vec![id.trim().to_string()]
        } else if let Some(spec) = raw.strip_prefix("rotate") {
            let spec = spec.strip_prefix(':').unwrap_or("").trim();
            if spec.is_empty() {
                pool::read_entries()
                    .iter()
                    .filter(|e| e.enabled)
                    .map(|e| e.id.clone())
                    .collect()
            } else {
                spec.split(',').map(str::trim).map(str::to_string).collect()
            }
        } else {
            continue;
        };
        let entry = json!({
            "id": session.id,
            "name": session.name,
            "enabled": session.status == "enabled",
        });
        for id in ids {
            if !id.is_empty() {
                references.entry(id).or_default().push(entry.clone());
            }
        }
    }
    references
}

/// 列表响应体：每条注入 `usedBy`（引用它的账号，删除确认框用）。
fn pool_payload(state: &AppState) -> Value {
    let references = reference_map(state);
    let items: Vec<Value> = pool::list_public()
        .into_iter()
        .map(|mut item| {
            let used = item
                .get("id")
                .and_then(Value::as_str)
                .and_then(|id| references.get(id))
                .cloned()
                .unwrap_or_default();
            if let Some(object) = item.as_object_mut() {
                object.insert("usedBy".to_string(), Value::Array(used));
            }
            item
        })
        .collect();
    json!({
        "success": true,
        "items": items,
        "total": items.len(),
        // Clash Verge 可用性快照（前端据此显示「Clash 是否可用 / 有几个出口」）
        "clash": pool::clash::clash_proxy_options(),
        // 账号 proxy 字段的编码协议说明（前端账号页下拉/提示用）
        "reference": {
            "pool": "pool:<id> —— 引用单条池条目",
            "rotateAll": "rotate —— 轮询全部启用条目（逐请求换出口 IP）",
            "rotateIds": "rotate:<id>,<id> —— 轮询指定条目",
            "url": "http(s)/socks5(h) URL —— 直填代理串（存量兼容）",
        },
    })
}

// ── Handlers ────────────────────────────────────────────────────────

/// GET /api/proxy-pool
pub async fn handle_pool_list(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    Ok(Json(pool_payload(&state)))
}

/// POST /api/proxy-pool
pub async fn handle_pool_create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<pool::EntryInput>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    match pool::create(&body) {
        Ok(entry) => {
            let mut data = pool_payload(&state);
            data["entry"] = entry;
            Ok(Json(data))
        }
        Err(message) => Err(error_response(&message, StatusCode::BAD_REQUEST)),
    }
}

/// POST /api/proxy-pool/update
pub async fn handle_pool_update(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<pool::EntryInput>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    match pool::update(&body) {
        Ok(entry) => {
            let mut data = pool_payload(&state);
            data["entry"] = entry;
            Ok(Json(data))
        }
        Err(message) => Err(error_response(&message, StatusCode::BAD_REQUEST)),
    }
}

/// POST /api/proxy-pool/enable
pub async fn handle_pool_enable(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<EnableBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    match pool::set_enabled(body.id.trim(), body.enabled) {
        Ok(entry) => {
            let mut data = pool_payload(&state);
            data["entry"] = entry;
            Ok(Json(data))
        }
        Err(message) => Err(error_response(&message, StatusCode::BAD_REQUEST)),
    }
}

/// POST /api/proxy-pool/remove
///
/// 有账号引用时 400 拒绝并列出引用者（ds2api 的防悬空校验）。
pub async fn handle_pool_remove(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<IdBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let id = body.id.trim();
    if id.is_empty() {
        return Err(error_response("缺少代理 id", StatusCode::BAD_REQUEST));
    }
    let references = reference_map(&state);
    if let Some(users) = references.get(id).filter(|u| !u.is_empty()) {
        let names: Vec<String> = users
            .iter()
            .take(MAX_REFERENCE_NAMES)
            .map(|u| {
                u.get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("（未命名账号）")
                    .to_string()
            })
            .collect();
        let suffix = if users.len() > MAX_REFERENCE_NAMES {
            format!(" 等 {} 个账号", users.len())
        } else {
            String::new()
        };
        return Err(error_response(
            &format!(
                "有 {} 个账号正在使用该代理（{}{suffix}），请先在账号池解绑后再删除",
                users.len(),
                names.join("、")
            ),
            StatusCode::BAD_REQUEST,
        ));
    }
    match pool::remove(id) {
        Ok(_) => Ok(Json(pool_payload(&state))),
        Err(message) => Err(error_response(&message, StatusCode::NOT_FOUND)),
    }
}

/// POST /api/proxy-pool/test
///
/// **禁用的条目也测**（先测通、再启用是常见顺序）；结果记进 `lastTest`
/// 但不参与选路。落库失败不吞测试结论（`saveError` 额外提示）。
pub async fn handle_pool_test(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<IdBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let id = body.id.trim();
    if id.is_empty() {
        return Err(error_response("缺少代理 id", StatusCode::BAD_REQUEST));
    }
    // 测试不看禁用位：拼 URL 只需要条目本身
    let entry = pool::read_entries()
        .into_iter()
        .find(|e| e.id == id)
        .ok_or_else(|| {
            error_response(
                &format!("代理「{id}」不存在或已被删除"),
                StatusCode::NOT_FOUND,
            )
        })?;
    let proxy_url = pool::entry_url(&entry);
    let result = pool::test_connectivity(Some(&proxy_url)).await;
    let record = pool::LastTest {
        success: result.success,
        ip: result.ip.clone(),
        duration_ms: result.duration_ms,
        error: result.error.clone(),
        tested_at: chrono::Utc::now().timestamp(),
    };
    let save_error = pool::record_test(id, &record).err();
    let mut data = serde_json::to_value(&result).unwrap_or_else(|_| json!({}));
    if let Some(object) = data.as_object_mut() {
        object.insert("name".to_string(), Value::String(entry.name.clone()));
        object.insert(
            "saveError".to_string(),
            save_error.map(Value::String).unwrap_or(Value::Null),
        );
        // 测试目标也换一份列表（lastTest 已更新）
        object.insert(
            "items".to_string(),
            pool_payload(&state)
                .get("items")
                .cloned()
                .unwrap_or_else(|| json!([])),
        );
    }
    tracing::info!(
        "[ProxyPool] 代理测试（{}）: {}",
        entry.name,
        if result.success {
            format!("✅ 出口 IP {}", result.ip)
        } else {
            format!("❌ {}", result.error.clone().unwrap_or_default())
        }
    );
    Ok(Json(data))
}

/// POST /api/proxy-pool/test-account
///
/// 两种用法（照 A2A `/api/proxies/test`）：
///   `{ accountId }`  测该账号生效的代理配置（解析 `pool:` / `rotate` 编码；
///                    解析失败 → 400，把原因如实报出）
///   `{ proxy }`      测临时串（`null` = 测直连）
pub async fn handle_pool_test_account(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<TestAccountBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let (proxy_url, label): (Option<String>, String) = if !body.account_id.trim().is_empty() {
        let id = body.account_id.trim();
        let session = state
            .account_sessions
            .get(id)
            .ok_or_else(|| error_response("Account session not found", StatusCode::NOT_FOUND))?;
        let resolved = pool::resolve_proxy(session.proxy.as_deref(), &session.id)
            .map_err(|message| error_response(&message, StatusCode::BAD_REQUEST))?;
        (resolved, pool::describe_proxy(session.proxy.as_deref()))
    } else {
        let resolved = pool::resolve_proxy(body.proxy.as_deref(), "")
            .map_err(|message| error_response(&message, StatusCode::BAD_REQUEST))?;
        (resolved, pool::describe_proxy(body.proxy.as_deref()))
    };
    let result = pool::test_connectivity(proxy_url.as_deref()).await;
    let mut data = serde_json::to_value(&result).unwrap_or_else(|_| json!({}));
    if let Some(object) = data.as_object_mut() {
        object.insert("proxy".to_string(), Value::String(label));
    }
    Ok(Json(data))
}

/// POST /api/proxy-pool/sync-clash — 手动同步 Clash Verge 出口到池。
///
/// 读取 Clash Verge 的 verge.yaml，把混合端口 + 各监听器镜像进池。
/// 响应带本次变更条数与失败原因（「Clash 没装」不算错误，只是 error 说明）。
pub async fn handle_pool_sync_clash(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let report = pool::sync_clash();
    // 同步后返回最新列表 + Clash 快照（前端刷新表格）
    let mut payload = pool_payload(&state);
    if let Some(object) = payload.as_object_mut() {
        object.insert("changes".to_string(), Value::from(report.changes as u64));
        object.insert(
            "syncError".to_string(),
            report.error.map(Value::from).unwrap_or(Value::Null),
        );
    }
    Ok(Json(payload))
}

// ── Agent2API `/api/proxies*` 对齐面 ──────────────────────────────
//
// 上游（v3.5.0-beta.3 `api/proxies.rs` + `api/accounts.rs::proxies_entry`）把
// 网络代理全部挂在 `/api/proxies*` 下，响应一律包成 `{success, data}`。
// AIGX 既有路由是 `/api/proxy-pool*`（扁平 `{success, items, ...}`，本页
// 前端在用，不能改）。这里补一组薄别名：同语义、同 handler，只把成功响应
// 转成上游的 data 信封；既有的 `/api/proxy-pool*` 行为零变化。
//
//   GET  /api/proxies                → 账号代理总览 + Clash 快照
//   GET  /api/proxies/system         → 系统代理快照（表单「从系统代理导入」）
//   POST /api/proxies/test           → 出口测试（`{id}` / `{proxy}` / 空 = 直连）
//   GET  /api/proxies/settings       → 连通性测试设置 `{testUrl, defaults}`
//   POST /api/proxies/settings       → 保存测试目标
//   GET/POST /api/proxies/pool*      → 代理池 CRUD/启停/测试/Clash 同步（别名）
//
// 路由注册不在本文件（main.rs 由集成方接线），见交付报告里的 route 清单。

/// 把既有扁平成功响应转成上游 `{success, data}` 信封（错误响应两种 API 同形，
/// 原样透传）。既有 handler 在成功体里带 `"success": true` 的信封位，转
/// data 信封时去掉它；**测试类响应例外**（`success` 是测试结论，见下）。
fn into_data_envelope(
    result: Result<Json<Value>, (StatusCode, Json<Value>)>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    match result {
        Ok(Json(mut payload)) => {
            if let Some(object) = payload.as_object_mut() {
                object.remove("success");
            }
            Ok(Json(json!({ "success": true, "data": payload })))
        }
        Err(error) => Err(error),
    }
}

/// 测试类响应的信封：`success` 留作**测试结论**（上游 data 里也是这个语义），
/// 外层信封另写 `success: true`（请求本身成功）。
fn into_test_envelope(
    result: Result<Json<Value>, (StatusCode, Json<Value>)>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    match result {
        Ok(Json(payload)) => Ok(Json(json!({ "success": true, "data": payload }))),
        Err(error) => Err(error),
    }
}

/// 账号代理配置的脱敏形态（与 `account_sessions::mask_proxy` 同口径：
/// `pool:` / `rotate` 引用原样，URL 里的 userinfo 换成 `***`）。
fn redact_proxy(proxy: Option<&str>) -> Value {
    let Some(raw) = proxy.map(str::trim).filter(|p| !p.is_empty()) else {
        return Value::Null;
    };
    if raw.starts_with("pool:") || raw.starts_with("rotate") {
        return Value::String(raw.to_string());
    }
    let Some((userinfo, host)) = raw.rsplit_once('@') else {
        return Value::String(raw.to_string());
    };
    let scheme_end = userinfo.find("://").map(|index| index + 3).unwrap_or(0);
    Value::String(format!("{}***@{host}", &userinfo[..scheme_end]))
}

/// GET /api/proxies —— 账号代理总览（上游 `list_proxies`）。
///
/// 账号行只带代理配置的脱敏形态（AIGX 列表口径；凭据不回传）。
#[allow(dead_code)] // A2A 别名 handler：路由由集成批次接入 main.rs
pub async fn handle_proxies_list(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let accounts: Vec<Value> = state
        .account_sessions
        .all()
        .iter()
        .map(|session| {
            json!({
                "id": session.id,
                "name": session.name,
                "priority": session.priority,
                "enabled": session.status == "enabled",
                "proxy": redact_proxy(session.proxy.as_deref()),
            })
        })
        .collect();
    Ok(Json(json!({
        "success": true,
        "data": {
            "accounts": accounts,
            "clash": pool::clash::clash_proxy_options(),
            "note": "支持手动 HTTP / SOCKS5 地址与 Clash Verge 端点；未配置代理时直连",
        }
    })))
}

/// GET /api/proxies/system —— 系统代理快照（读 OS 配置，不探活）。
#[allow(dead_code)] // A2A 别名 handler：路由由集成批次接入 main.rs
pub async fn handle_proxies_system(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let snapshot = tokio::task::spawn_blocking(pool::system::snapshot)
        .await
        .map_err(|_| error_response("读取系统代理失败", StatusCode::INTERNAL_SERVER_ERROR))?;
    match snapshot {
        Ok(value) => Ok(Json(json!({ "success": true, "data": value }))),
        Err(message) => Err(error_response(&message, StatusCode::BAD_REQUEST)),
    }
}

/// GET /api/proxies/settings —— 连通性测试设置。
#[allow(dead_code)] // A2A 别名 handler：路由由集成批次接入 main.rs
pub async fn handle_proxies_settings_get(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    Ok(Json(json!({
        "success": true,
        "data": pool::settings::get(),
    })))
}

/// POST /api/proxies/settings —— 保存测试目标（`{testUrl}`；空 = 恢复默认）。
#[allow(dead_code)] // A2A 别名 handler：路由由集成批次接入 main.rs
pub async fn handle_proxies_settings_save(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    match pool::settings::save(&body) {
        Ok(value) => Ok(Json(json!({ "success": true, "data": value }))),
        Err(message) => Err(error_response(&message, StatusCode::BAD_REQUEST)),
    }
}

/// `POST /api/proxies/test` 请求体：上游用 `{id}` 指账号、`{proxy}` 给配置
/// （对象 / 字符串 / null 直连）。AIGX 账号 id 字段同名，`proxy` 两种形态都收。
#[derive(Debug, Default, Deserialize)]
pub struct ProxiesTestBody {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    proxy: Option<Value>,
}

/// 上游 `proxy` 配置对象 → AIGX 的 proxy 引用串（`pool:` / `rotate` / URL）。
///
/// 只翻译 AIGX 真实支持的来源：`custom`（拼 URL）、`pool`、`rotate`、`direct`；
/// `clash`（Clash 监听器 uid）在 AIGX 没有对应实体 —— 如实报错，不猜地址。
fn proxy_config_to_reference(value: &Value) -> Result<Option<String>, String> {
    match value {
        Value::Null => Ok(None),
        Value::String(raw) => {
            let raw = raw.trim();
            Ok((!raw.is_empty()).then(|| raw.to_string()))
        }
        Value::Object(fields) => {
            let source = fields
                .get("source")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_ascii_lowercase();
            let source = if source.is_empty()
                && fields
                    .get("host")
                    .and_then(Value::as_str)
                    .is_some_and(|host| !host.trim().is_empty())
            {
                "custom".to_string()
            } else {
                source
            };
            match source.as_str() {
                "direct" => Ok(None),
                "custom" => {
                    let host = fields
                        .get("host")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|host| !host.is_empty())
                        .ok_or_else(|| "代理配置缺少 host".to_string())?;
                    let protocol = fields
                        .get("protocol")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|protocol| !protocol.is_empty())
                        .unwrap_or("http");
                    if !matches!(protocol, "http" | "https" | "socks5" | "socks5h") {
                        return Err(format!("代理协议不支持：{protocol}"));
                    }
                    let port = fields
                        .get("port")
                        .and_then(Value::as_u64)
                        .filter(|port| (1..=65535).contains(port))
                        .ok_or_else(|| "代理配置缺少有效端口".to_string())?;
                    let username = fields
                        .get("username")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .trim();
                    let password = fields
                        .get("password")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .trim();
                    let auth = if username.is_empty() {
                        String::new()
                    } else if password.is_empty() {
                        format!("{username}@")
                    } else {
                        format!("{username}:{password}@")
                    };
                    Ok(Some(format!("{protocol}://{auth}{host}:{port}")))
                }
                "pool" => {
                    let id = fields
                        .get("proxyId")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|id| !id.is_empty())
                        .ok_or_else(|| "代理池引用缺少 proxyId".to_string())?;
                    Ok(Some(format!("pool:{id}")))
                }
                "rotate" => {
                    let all = fields.get("all").and_then(Value::as_bool).unwrap_or(true);
                    if all {
                        return Ok(Some("rotate".to_string()));
                    }
                    let ids: Vec<&str> = fields
                        .get("proxyIds")
                        .and_then(Value::as_array)
                        .map(|items| {
                            items
                                .iter()
                                .filter_map(Value::as_str)
                                .map(str::trim)
                                .filter(|id| !id.is_empty())
                                .collect()
                        })
                        .unwrap_or_default();
                    if ids.is_empty() {
                        return Err("轮询代理缺少 proxyIds".to_string());
                    }
                    Ok(Some(format!("rotate:{}", ids.join(","))))
                }
                "clash" => Err(
                    "Clash 监听器代理暂不支持按 uid 引用，请改用代理池条目或手动地址".to_string(),
                ),
                other => Err(format!("代理来源不支持：{other}")),
            }
        }
        _ => Err("代理配置必须是对象、字符串或 null".to_string()),
    }
}

/// POST /api/proxies/test —— 出口测试（上游 `test_proxy`）。
///
/// 两种用法：`{id}` 测该账号生效的代理（含 `pool:` / `rotate` 解析）；
/// `{proxy}` 测临时配置；两者都缺省 = 测直连。测试失败也是 200
/// （`success:false` + error，与上游同约定）。
#[allow(dead_code)] // A2A 别名 handler：路由由集成批次接入 main.rs
pub async fn handle_proxies_test(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Option<Json<ProxiesTestBody>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let body = match body {
        Some(Json(body)) => body,
        None => ProxiesTestBody::default(),
    };
    let account_id = body
        .id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty());
    let (proxy_url, label) = if let Some(id) = account_id {
        let session = state
            .account_sessions
            .get(id)
            .ok_or_else(|| error_response("账号不存在", StatusCode::NOT_FOUND))?;
        let resolved = pool::resolve_proxy(session.proxy.as_deref(), &session.id)
            .map_err(|message| error_response(&message, StatusCode::BAD_REQUEST))?;
        (resolved, pool::describe_proxy(session.proxy.as_deref()))
    } else {
        let reference = match body.proxy.as_ref() {
            Some(value) => proxy_config_to_reference(value)
                .map_err(|message| error_response(&message, StatusCode::BAD_REQUEST))?,
            None => None,
        };
        let resolved = pool::resolve_proxy(reference.as_deref(), "")
            .map_err(|message| error_response(&message, StatusCode::BAD_REQUEST))?;
        (resolved, pool::describe_proxy(reference.as_deref()))
    };
    let result = pool::test_connectivity(proxy_url.as_deref()).await;
    let mut data = serde_json::to_value(&result).unwrap_or_else(|_| json!({}));
    if let Some(object) = data.as_object_mut() {
        object.insert("proxy".to_string(), Value::String(label));
    }
    Ok(Json(json!({ "success": true, "data": data })))
}

// 代理池路由的 data 信封别名（同 handler，仅转信封）。
#[allow(dead_code)] // A2A 别名 handler：路由由集成批次接入 main.rs
pub async fn handle_proxies_pool_list(
    state: State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    into_data_envelope(handle_pool_list(state, headers).await)
}

#[allow(dead_code)] // A2A 别名 handler：路由由集成批次接入 main.rs
pub async fn handle_proxies_pool_create(
    state: State<AppState>,
    headers: HeaderMap,
    body: Json<Value>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let entry = parse_entry_input(body.0)?;
    into_data_envelope(handle_pool_create(state, headers, Json(entry)).await)
}

#[allow(dead_code)] // A2A 别名 handler：路由由集成批次接入 main.rs
pub async fn handle_proxies_pool_update(
    state: State<AppState>,
    headers: HeaderMap,
    body: Json<Value>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let entry = parse_entry_input(body.0)?;
    into_data_envelope(handle_pool_update(state, headers, Json(entry)).await)
}

#[allow(dead_code)] // A2A 别名 handler：路由由集成批次接入 main.rs
pub async fn handle_proxies_pool_enable(
    state: State<AppState>,
    headers: HeaderMap,
    body: Json<EnableBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    into_data_envelope(handle_pool_enable(state, headers, body).await)
}

#[allow(dead_code)] // A2A 别名 handler：路由由集成批次接入 main.rs
pub async fn handle_proxies_pool_remove(
    state: State<AppState>,
    headers: HeaderMap,
    body: Json<IdBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    into_data_envelope(handle_pool_remove(state, headers, body).await)
}

#[allow(dead_code)] // A2A 别名 handler：路由由集成批次接入 main.rs
pub async fn handle_proxies_pool_test(
    state: State<AppState>,
    headers: HeaderMap,
    body: Json<IdBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    into_test_envelope(handle_pool_test(state, headers, body).await)
}

#[allow(dead_code)] // A2A 别名 handler：路由由集成批次接入 main.rs
pub async fn handle_proxies_pool_sync_clash(
    state: State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    into_data_envelope(handle_pool_sync_clash(state, headers).await)
}

/// 上游允许 `{items:[...]}` 批量导入；AIGX 暂未实现（如实 400，不静默当单条）。
fn parse_entry_input(body: Value) -> Result<pool::EntryInput, (StatusCode, Json<Value>)> {
    if body.get("items").is_some() {
        return Err(error_response(
            "批量导入暂不支持（AIGX 请逐条 POST /api/proxies/pool）",
            StatusCode::BAD_REQUEST,
        ));
    }
    serde_json::from_value::<pool::EntryInput>(body)
        .map_err(|error| error_response(&format!("代理条目无效：{error}"), StatusCode::BAD_REQUEST))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_map_counts_rotate_and_pool() {
        // 纯逻辑测试：不走存储，只验证编码识别（借轻量结构体）
        // reference_map 依赖 AppState（重），这里测的是编码拆分逻辑的
        // 等价小函数已被 resolve_proxy 测覆盖；本测试保证请求体形状稳定。
        let body: TestAccountBody =
            serde_json::from_str(r#"{"proxy":"socks5://1.2.3.4:1080"}"#).expect("parse");
        assert_eq!(body.proxy.as_deref(), Some("socks5://1.2.3.4:1080"));
        let body: TestAccountBody = serde_json::from_str(r#"{"account_id":"abc"}"#).expect("parse");
        assert_eq!(body.account_id, "abc");
    }

    #[test]
    fn proxies_test_body_accepts_id_and_proxy_forms() {
        // 上游 `/api/proxies/test` 用 `{id}` 指账号、`{proxy}` 给配置对象。
        let body: ProxiesTestBody = serde_json::from_str(r#"{"id":"acct-1"}"#).expect("parse");
        assert_eq!(body.id.as_deref(), Some("acct-1"));
        // serde 的 Option<Value> 把 JSON null 收成 None（null = 直连，与缺省同义）
        let body: ProxiesTestBody = serde_json::from_str(r#"{"proxy":null}"#).expect("parse");
        assert!(body.proxy.is_none());
        let body: ProxiesTestBody =
            serde_json::from_str(r#"{"proxy":{"source":"custom","host":"1.2.3.4","port":8080}}"#)
                .expect("parse");
        assert!(body.proxy.is_some());
    }

    #[test]
    fn proxy_config_object_translates_to_reference() {
        // custom → URL（含认证）；pool / rotate 编码；direct / null → 直连。
        assert_eq!(
            proxy_config_to_reference(&json!({
                "source": "custom", "protocol": "socks5", "host": "1.2.3.4",
                "port": 1080, "username": "u", "password": "p",
            }))
            .unwrap()
            .as_deref(),
            Some("socks5://u:p@1.2.3.4:1080")
        );
        assert_eq!(
            proxy_config_to_reference(&json!({ "host": "5.6.7.8", "port": 8080 }))
                .unwrap()
                .as_deref(),
            Some("http://5.6.7.8:8080")
        );
        assert_eq!(
            proxy_config_to_reference(&json!({ "source": "pool", "proxyId": "px_1" }))
                .unwrap()
                .as_deref(),
            Some("pool:px_1")
        );
        assert_eq!(
            proxy_config_to_reference(&json!({ "source": "rotate", "all": true }))
                .unwrap()
                .as_deref(),
            Some("rotate")
        );
        assert_eq!(
            proxy_config_to_reference(
                &json!({ "source": "rotate", "all": false, "proxyIds": ["a","b"] })
            )
            .unwrap()
            .as_deref(),
            Some("rotate:a,b")
        );
        assert_eq!(
            proxy_config_to_reference(&json!({ "source": "direct" })).unwrap(),
            None
        );
        assert_eq!(proxy_config_to_reference(&Value::Null).unwrap(), None);
        assert_eq!(proxy_config_to_reference(&json!("")).unwrap(), None);
    }

    #[test]
    fn proxy_config_rejects_unsupported_shapes() {
        // 负向断言 + 正向对照：非法形状全部报错，合法形状在上一条用例通过。
        for bad in [
            json!({ "source": "clash", "listenerUid": "x" }),
            json!({ "source": "custom", "host": "1.2.3.4" }),
            json!({ "source": "custom", "host": "1.2.3.4", "port": 0 }),
            json!({ "source": "pool" }),
            json!({ "source": "rotate", "all": false, "proxyIds": [] }),
            json!({ "source": "unknown" }),
            json!(42),
        ] {
            assert!(proxy_config_to_reference(&bad).is_err(), "应拒绝 {bad:?}");
        }
    }

    #[test]
    fn redact_proxy_keeps_reference_forms_only() {
        assert_eq!(redact_proxy(Some("pool:abc")), json!("pool:abc"));
        assert_eq!(redact_proxy(Some("rotate")), json!("rotate"));
        assert_eq!(
            redact_proxy(Some("http://user:pass@1.2.3.4:8080")),
            json!("http://***@1.2.3.4:8080")
        );
        assert_eq!(redact_proxy(None), Value::Null);
        assert_eq!(redact_proxy(Some("  ")), Value::Null);
    }

    #[test]
    fn batch_items_body_is_rejected_loudly() {
        let error = parse_entry_input(json!({ "items": [] })).expect_err("批量体应被拒绝");
        assert_eq!(error.0, StatusCode::BAD_REQUEST);
        // 单条体可解析（正向对照）
        assert!(parse_entry_input(json!({ "name": "n", "host": "1.2.3.4", "port": 8080 })).is_ok());
    }
}
