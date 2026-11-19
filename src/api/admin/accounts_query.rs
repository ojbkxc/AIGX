//! 账号列表查询/分页与凭证替换（对齐 Agent2API v3.5.0-beta.2
//! `core/accounts_list.rs` + `api/accounts_credentials.rs`）。
//!
//! 与既有 `/api/accounts/sessions`（`account_sessions.rs`，按渠道 + 全量视图）互补：
//! 本模块提供 A2A 形状的 `AccountListQuery`（关键词/多选/状态/到期/用量/分页/
//! 只取元数据），以及 `replace_cline_free` 的凭证替换（`expected_updated_at` CAS）。
//!
//! ## beta.2 增量
//! - `POST /api/accounts/query` —— 从请求体反序列化筛选条件，返回分页列表；
//! - `POST /api/accounts/selection` —— 同上但只返回 id 集；
//! - `GET /api/accounts/:id` —— 单账号详情（`detailsLoaded: true`）。

use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::Json,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::account_session::model::Credential;
use crate::api::openai::AppState;

use super::common::{error_response, verify_admin};

/// GET /api/accounts/list —— A2A 形状的账号列表（分页）。
///
/// `?view=ids` 只返回 id；`?view=summary` 只返回计数；缺省返回分页列表 + 分组。
pub async fn handle_accounts_list_query(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<crate::accounts_list::AccountListQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let sessions = state.account_sessions.all();
    let now = chrono::Utc::now().timestamp();
    let data = crate::accounts_list::page(&sessions, &query, false, now);
    Ok(Json(json!({ "success": true, "data": data })))
}

/// GET /api/accounts/summary —— 账号计数汇总。
pub async fn handle_accounts_summary(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let sessions = state.account_sessions.all();
    let now = chrono::Utc::now().timestamp();
    Ok(Json(json!({
        "success": true,
        "data": crate::accounts_list::summary(&sessions, now),
    })))
}

/// GET /api/accounts/ids —— 只返回满足查询条件的账号 id。
pub async fn handle_accounts_ids(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<crate::accounts_list::AccountListQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let sessions = state.account_sessions.all();
    let now = chrono::Utc::now().timestamp();
    let data = crate::accounts_list::page(&sessions, &query, true, now);
    Ok(Json(json!({ "success": true, "data": data })))
}

/// POST /api/accounts/query —— 从请求体反序列化 `AccountListQuery`，返回分页列表。
///
/// 对齐上游 `api/accounts_list.rs::query(state, body, false)`。
/// 前端在筛选条件复杂（多 provider / 多 id / 折叠组）时走 POST 而非 GET query string。
#[allow(dead_code)] // 预留/线表符号（A2A 契约面，路由由集成批次接入）
pub async fn handle_accounts_query_post(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let filter = serde_json::from_slice::<crate::accounts_list::AccountListQuery>(&body)
        .map_err(|_| error_response("账号筛选参数无效", StatusCode::BAD_REQUEST))?;
    validate_query(&filter)?;
    let sessions = state.account_sessions.all();
    let now = chrono::Utc::now().timestamp();
    let data = crate::accounts_list::page(&sessions, &filter, false, now);
    Ok(Json(json!({ "success": true, "data": data })))
}

/// POST /api/accounts/selection —— 同 query 但只返回 id 列表（`ids_only = true`）。
///
/// 对齐上游 `api/accounts_list.rs::query(state, body, true)`。
/// 用于前端在不拉全量数据的情况下做「选中集」同步（如批量操作前的 id 收集）。
#[allow(dead_code)] // 预留/线表符号（A2A 契约面，路由由集成批次接入）
pub async fn handle_accounts_selection_post(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let filter = serde_json::from_slice::<crate::accounts_list::AccountListQuery>(&body)
        .map_err(|_| error_response("账号筛选参数无效", StatusCode::BAD_REQUEST))?;
    validate_query(&filter)?;
    let sessions = state.account_sessions.all();
    let now = chrono::Utc::now().timestamp();
    let data = crate::accounts_list::page(&sessions, &filter, true, now);
    Ok(Json(json!({ "success": true, "data": data })))
}

/// 校验分页与筛选参数范围（对齐上游 `query()` 的边界检查）。
#[allow(dead_code)] // 预留/线表符号（A2A 契约面，路由由集成批次接入）
fn validate_query(
    filter: &crate::accounts_list::AccountListQuery,
) -> Result<(), (StatusCode, Json<Value>)> {
    if !(1..=200).contains(&filter.page_size)
        || filter.q.len() > 4096
        || filter
            .ids
            .iter()
            .chain(&filter.providers)
            .chain(&filter.collapsed)
            .any(|id| id.len() > 512)
    {
        return Err(error_response(
            "账号分页或筛选参数超出范围",
            StatusCode::BAD_REQUEST,
        ));
    }
    Ok(())
}

/// GET /api/accounts/:id —— 单账号详情（对齐上游 `accounts_list::detail`）。
///
/// 返回该账号的完整 JSON 投影（含 `detailsLoaded: true`）。
/// 与 `/api/account-sessions/:id`（PUT/PATCH/DELETE）互补：那条是管理操作，
/// 这条是只读详情，前端在账号列表里点行展开时调它。
#[allow(dead_code)] // 预留/线表符号（A2A 契约面，路由由集成批次接入）
pub async fn handle_account_detail(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let sessions = state.account_sessions.all();
    let now = chrono::Utc::now().timestamp();
    match crate::accounts_list::detail(&sessions, &id, now) {
        Some(account) => Ok(Json(json!({ "success": true, "data": account }))),
        None => Err(error_response(
            "账号不存在，请刷新后重试",
            StatusCode::NOT_FOUND,
        )),
    }
}

/// 凭证替换请求（对齐参考树 `Replacement`）。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialReplacement {
    pub credential_json: String,
    pub confirmed: bool,
    #[serde(default)]
    pub expected_updated_at: i64,
    #[serde(default)]
    pub authorize_refresh: bool,
}

/// 从粘贴的凭证 JSON 构造 [`Credential`]。
///
/// 主密钥优先取 `cookie` / `token` / `accessToken` / `apiKey` / `key`；其余字符串
/// 字段进 `fields`（加密落盘）。非对象输入原样作为 `secret`（kind=token）。
fn credential_from_json(raw: &str) -> Result<Credential, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("凭证内容不能为空".into());
    }
    let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
        return Ok(Credential {
            kind: "token".into(),
            secret: trimmed.to_string(),
            fields: Default::default(),
        });
    };
    let object = value
        .as_object()
        .ok_or("凭证 JSON 必须是对象或非空字符串")?;
    let mut secret = String::new();
    let mut kind = "api_key".to_string();
    let mut fields = std::collections::HashMap::new();
    for (key, value) in object {
        let Some(text) = value.as_str() else { continue };
        match key.as_str() {
            "cookie" => {
                secret = text.to_string();
                kind = "cookie".into();
            }
            "token" | "accessToken" | "access_token" if secret.is_empty() => {
                secret = text.to_string();
                kind = "token".into();
            }
            "apiKey" | "api_key" | "key" if secret.is_empty() => {
                secret = text.to_string();
                kind = "api_key".into();
            }
            _ => {
                fields.insert(key.clone(), text.to_string());
            }
        }
    }
    if secret.is_empty() {
        return Err("凭证 JSON 缺少主密钥（cookie / token / accessToken / apiKey）".into());
    }
    Ok(Credential {
        kind,
        secret,
        fields,
    })
}

/// PUT /api/accounts/:id/credentials/replace —— 替换账号登录凭证。
///
/// `expected_updated_at` CAS：与当前 `updated_at` 不符返回 409（提示刷新后重试）。
/// `authorize_refresh` 目前仅记录（AIGX 各家续期链路独立接入，见 `account_session::refresh`）。
pub async fn handle_replace_account_credentials(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<CredentialReplacement>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    if body.credential_json.len() > 1024 * 1024 {
        return Err(error_response(
            "凭证输入过大，请只粘贴当前账号的完整凭证 JSON",
            StatusCode::BAD_REQUEST,
        ));
    }
    if !body.confirmed || body.expected_updated_at < 0 {
        return Err(error_response(
            "请先确认更新当前账号的登录凭证",
            StatusCode::BAD_REQUEST,
        ));
    }
    let session = state
        .account_sessions
        .get(&id)
        .ok_or_else(|| error_response("账号不存在", StatusCode::NOT_FOUND))?;
    if body.expected_updated_at != 0 && session.updated_at != body.expected_updated_at {
        return Err(error_response(
            "账号已变更，请刷新后重试",
            StatusCode::CONFLICT,
        ));
    }
    let next = credential_from_json(&body.credential_json)
        .map_err(|msg| error_response(&msg, StatusCode::BAD_REQUEST))?;
    let old_secret = session.credential.secret_plain();
    let authorized_refresh = body.authorize_refresh;
    match state
        .account_sessions
        .update_credential_cas(&id, &old_secret, next)
    {
        crate::account_session::store::CasOutcome::Updated => {
            let updated = state.account_sessions.get(&id);
            tracing::info!(
                "account {id} credentials replaced (authorize_refresh={authorized_refresh})"
            );
            Ok(Json(json!({
                "success": true,
                "data": {
                    "updated": true,
                    "authorizeRefresh": authorized_refresh,
                    "account": updated.map(|s| {
                        json!({
                            "id": s.id,
                            "name": s.name,
                            "provider": s.provider,
                            "updatedAt": s.updated_at,
                        })
                    }),
                }
            })))
        }
        crate::account_session::store::CasOutcome::Stale => Err(error_response(
            "账号凭证已被并发刷新，本次替换被拒绝，请刷新后重试",
            StatusCode::CONFLICT,
        )),
        crate::account_session::store::CasOutcome::Missing => {
            Err(error_response("账号不存在", StatusCode::NOT_FOUND))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_from_json_extracts_primary_secret() {
        let cred =
            credential_from_json(r#"{"cookie":"abc","uid":"42","refreshToken":"r"}"#).unwrap();
        assert_eq!(cred.kind, "cookie");
        assert_eq!(cred.secret, "abc");
        assert_eq!(cred.fields.get("uid"), Some(&"42".to_string()));
    }

    #[test]
    fn credential_from_json_plain_string_is_token() {
        let cred = credential_from_json("opaque-token").unwrap();
        assert_eq!(cred.kind, "token");
        assert_eq!(cred.secret, "opaque-token");
    }

    #[test]
    fn credential_from_json_requires_primary() {
        assert!(credential_from_json(r#"{"uid":"42"}"#).is_err());
        assert!(credential_from_json("   ").is_err());
    }
}
