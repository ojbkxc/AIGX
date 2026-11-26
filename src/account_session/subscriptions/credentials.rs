//! 订阅账号的令牌续期（chatgpt / codex）。
//!
//! 两家都用 OpenAI 的 OAuth token 端点刷新（`refresh_token` 会轮换），
//! 字段口径对齐 Agent2API：
//! - chatgpt：`grant_type=refresh_token` + `client_id` + `resource`；
//! - codex：`grant_type=refresh_token` + 官方公开客户端 id + `refresh_reason`。
//!
//! ⚠️ 本续期由**数据面按需**触发（`SubscriptionBridge::access_token`），
//! 不在 `account_session::refresh` 的巡检表里 —— `refresh.rs` 不在本任务
//! 的文件域，等主控接线后应把这两家并入同一套单飞（见交付报告的待接线清单）。
//! 此处用 `update_tokens_cas` 做「access + refresh + expires_at 一次落盘」，
//! CAS 冲突（并发刷新）时回读最新令牌，不覆盖别人已写的新值。

use serde_json::Value;

use crate::account_session::model::AccountSession;
use crate::bridge::BridgeError;

use super::backend::Flavor;

/// ChatGPT 的 token 端点与资源（对齐上游 `chatgpt_subscription/mod.rs`）。
const CHATGPT_TOKEN_URL: &str = "https://auth.openai.com/api/accounts/oauth/token";
const CHATGPT_RESOURCE: &str = "https://api.openai.com/v1";
/// Codex 的 token 端点与官方公开 OAuth 客户端（对齐 `codex_subscription/http.rs`）。
const CODEX_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";

/// 刷新得到的新令牌三件套（`expires_at` 为 unix 秒）。
pub struct Tokens {
    pub access: String,
    pub refresh: String,
    pub id_token: Option<String>,
    pub expires_at: Option<i64>,
}

/// 用账号里的 `refresh_secret` 换一次新令牌。
pub async fn refresh(
    flavor: Flavor,
    client: &reqwest::Client,
    session: &AccountSession,
) -> Result<Tokens, BridgeError> {
    let refresh = crate::account_session::secret::open_secret(
        &session.refresh_secret,
        &crate::account_session::secret::data_dir(),
    );
    let refresh = refresh.trim();
    if refresh.is_empty() {
        return Err(BridgeError::AuthError(
            "订阅账号缺少刷新令牌，请重新登录".to_string(),
        ));
    }
    let (url, form): (&str, Vec<(&str, String)>) = match flavor {
        Flavor::Chatgpt => {
            let client_id = {
                let stored = session.credential.field_plain("clientId");
                if stored.trim().is_empty() {
                    "dynamic_agent_client".to_string()
                } else {
                    stored
                }
            };
            (
                CHATGPT_TOKEN_URL,
                vec![
                    ("grant_type", "refresh_token".to_string()),
                    ("client_id", client_id),
                    ("refresh_token", refresh.to_string()),
                    ("resource", CHATGPT_RESOURCE.to_string()),
                ],
            )
        }
        Flavor::Codex => (
            CODEX_TOKEN_URL,
            vec![
                ("client_id", CODEX_CLIENT_ID.to_string()),
                ("grant_type", "refresh_token".to_string()),
                ("refresh_token", refresh.to_string()),
                ("refresh_reason", "proactive_expiry".to_string()),
            ],
        ),
    };
    let response = client
        .post(url)
        .form(&form)
        .send()
        .await
        .map_err(|error| BridgeError::Transport(error.to_string()))?;
    let status = response.status().as_u16();
    let value: Value = response
        .json()
        .await
        .map_err(|error| BridgeError::Transport(error.to_string()))?;
    if !(200..300).contains(&status) {
        let message = value
            .get("error_description")
            .and_then(Value::as_str)
            .or_else(|| value.get("error").and_then(Value::as_str))
            .unwrap_or("订阅令牌续期失败")
            .to_string();
        // 400 / 401 基本是 refresh token 已失效：归为凭据错误（可判死重登）。
        if matches!(status, 400 | 401) {
            return Err(BridgeError::AuthError(message));
        }
        return Err(BridgeError::upstream_status(status, message));
    }
    let access = value
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| BridgeError::UpstreamDecode("续期响应缺少 access_token".to_string()))?
        .to_string();
    let next_refresh = value
        .get("refresh_token")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| refresh.to_string());
    let id_token = value
        .get("id_token")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
        .map(str::to_string);
    let expires_at = value
        .get("expires_in")
        .and_then(Value::as_i64)
        .filter(|seconds| *seconds > 0)
        .map(|seconds| chrono::Utc::now().timestamp().saturating_add(seconds));
    Ok(Tokens {
        access,
        refresh: next_refresh,
        id_token,
        expires_at,
    })
}
