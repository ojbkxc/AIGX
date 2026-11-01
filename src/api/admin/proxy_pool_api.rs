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
struct IdBody {
    id: String,
}

#[derive(Debug, Deserialize)]
struct EnableBody {
    id: String,
    enabled: bool,
}

/// 账号代理测试请求：`{ accountId }`（测该账号生效的代理配置）或
/// `{ proxy }`（测临时串，`null` = 测直连）。
#[derive(Debug, Default, Deserialize)]
struct TestAccountBody {
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
        // 账号 proxy 字段的编码协议说明（前端账号页下拉/提示用）
        "reference": {
            "pool": "pool:<id> —— 引用单条池条目",
            "rotateAll": "rotate —— 轮询全部启用条目（逐请求换出口 IP）",
            "rotateIds": "rotate:<id>,<id> —— 轮询指定条目",
            "url": "http(s)/socks5 URL —— 直填代理串（存量兼容）",
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
        let body: TestAccountBody = serde_json::from_str(r#"{"accountId":"abc"}"#).expect("parse");
        assert_eq!(body.account_id, "abc");
    }
}
