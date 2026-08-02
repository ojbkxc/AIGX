use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;

use crate::account::AccountPool;
use crate::model::ModelMapper;

/// CF Workers AI API 客户端
pub struct CfApiClient {
    http: Client,
    account_pool: Arc<AccountPool>,
    model_mapper: Arc<ModelMapper>,
}

impl CfApiClient {
    pub fn new(account_pool: Arc<AccountPool>, model_mapper: Arc<ModelMapper>) -> Self {
        let http = Client::builder()
            .timeout(Duration::from_secs(120))
            .user_agent("cf-ai-gw/0.1.0")
            .build()
            .expect("Failed to create HTTP client");
        Self {
            http,
            account_pool,
            model_mapper,
        }
    }

    /// 构建 CF API URL
    fn build_url(account_id: &str, path: &str) -> String {
        format!(
            "https://api.cloudflare.com/client/v4/accounts/{}/ai/{}",
            account_id, path
        )
    }

    /// 调用 CF Workers AI API（带多账号故障转移）
    /// 类似 _worker.js 的 callCFRunAPI
    pub async fn call_ai(&self, model: &str, body: Value) -> std::result::Result<CfResponse, CfError> {
        let cf_model = self.model_mapper.resolve(model);
        let active_accounts = self.account_pool.active_accounts();

        if active_accounts.is_empty() {
            return Err(CfError::AuthError("No active Cloudflare accounts configured".into()));
        }

        let mut last_error = CfError::AllAccountsFailed("All accounts exhausted".into());

        for account in &active_accounts {
            for attempt in 0..2 {
                if attempt > 0 {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }

                let url = Self::build_url(&account.account_id, &format!("run/{}", cf_model));
                let result = self
                    .http
                    .post(&url)
                    .bearer_auth(&account.api_token)
                    .json(&body)
                    .send()
                    .await;

                match result {
                    Ok(resp) => {
                        let status = resp.status();
                        if status.is_success() {
                            match resp.json::<CfResponse>().await {
                                Ok(cf_resp) => {
                                    if cf_resp.success {
                                        return Ok(cf_resp);
                                    }
                                    last_error = CfError::ServerError(
                                        cf_resp
                                            .errors
                                            .first()
                                            .and_then(|e| e.message.clone())
                                            .unwrap_or_else(|| "Unknown CF API error".into()),
                                    );
                                    if !is_retryable_status(status.as_u16()) {
                                        return Err(last_error);
                                    }
                                }
                                Err(e) => {
                                    last_error = CfError::NetworkError(format!("Failed to parse response: {e}"));
                                }
                            }
                        } else if is_auth_error(status.as_u16()) {
                            return Err(CfError::AuthError(format!("Auth failed: {status}")));
                        } else if status.as_u16() == 429 {
                            let retry_after = resp
                                .headers()
                                .get("retry-after")
                                .and_then(|v| v.to_str().ok())
                                .and_then(|v| v.parse::<u64>().ok());
                            last_error = CfError::RateLimited { retry_after };
                            // 限流时跳过当前账号剩余重试
                            break;
                        } else if status.as_u16() == 404 {
                            last_error = CfError::ModelNotFound(format!("Model {cf_model} not found"));
                            // 模型不存在，不可重试
                            return Err(last_error);
                        } else if !is_retryable_status(status.as_u16()) {
                            let body_text = resp.text().await.unwrap_or_default();
                            last_error = CfError::ServerError(format!("CF API status {status}: {body_text}"));
                            return Err(last_error);
                        } else {
                            let body_text = resp.text().await.unwrap_or_default();
                            last_error = CfError::ServerError(format!("CF API status {status}: {body_text}"));
                        }
                    }
                    Err(e) => {
                        last_error = CfError::NetworkError(format!("Connection error: {e}"));
                    }
                }
            }
        }

        Err(last_error)
    }

    /// 调用 CF 模型列表 API
    pub async fn list_models(&self) -> std::result::Result<Vec<CfModelInfo>, CfError> {
        let active_accounts = self.account_pool.active_accounts();

        if active_accounts.is_empty() {
            return Err(CfError::AuthError("No active Cloudflare accounts configured".into()));
        }

        for account in &active_accounts {
            let url = Self::build_url(&account.account_id, "models/search");
            let result = self
                .http
                .get(&url)
                .bearer_auth(&account.api_token)
                .send()
                .await;

            match result {
                Ok(resp) => {
                    if resp.status().is_success() {
                        let cf_resp = resp.json::<CfResponse>().await.map_err(|e| {
                            CfError::NetworkError(format!("Failed to parse models response: {e}"))
                        })?;
                        if cf_resp.success {
                            let models: Vec<CfModelInfo> = cf_resp
                                .result
                                .and_then(|v| serde_json::from_value(v).ok())
                                .unwrap_or_default();
                            return Ok(models);
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("Failed to list models from account {}: {e}", account.name);
                }
            }
        }

        Err(CfError::AllAccountsFailed("Failed to list models from all accounts".into()))
    }

    /// 调用 CF 文本生成 API
    pub async fn run_text(&self, model: &str, body: Value) -> std::result::Result<Value, CfError> {
        let resp = self.call_ai(model, body).await?;
        Ok(resp.result.unwrap_or_default())
    }

    /// 调用 CF 嵌入 API
    pub async fn run_embedding(&self, model: &str, body: Value) -> std::result::Result<Value, CfError> {
        let resp = self.call_ai(model, body).await?;
        Ok(resp.result.unwrap_or_default())
    }

    /// 调用 CF 图片生成 API
    pub async fn run_image(&self, model: &str, body: Value) -> std::result::Result<Value, CfError> {
        let resp = self.call_ai(model, body).await?;
        Ok(resp.result.unwrap_or_default())
    }

    /// 调用 CF 语音识别 API（二进制上传）
    pub async fn run_audio(
        &self,
        model: &str,
        data: Vec<u8>,
        content_type: &str,
    ) -> std::result::Result<Value, CfError> {
        let cf_model = self.model_mapper.resolve(model);
        let active_accounts = self.account_pool.active_accounts();

        if active_accounts.is_empty() {
            return Err(CfError::AuthError("No active Cloudflare accounts configured".into()));
        }

        let mut last_error = CfError::AllAccountsFailed("All accounts exhausted".into());

        for account in &active_accounts {
            for attempt in 0..2 {
                if attempt > 0 {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }

                let url = Self::build_url(&account.account_id, &format!("run/{}", cf_model));
                let result = self
                    .http
                    .post(&url)
                    .bearer_auth(&account.api_token)
                    .header("Content-Type", content_type)
                    .body(data.clone())
                    .send()
                    .await;

                match result {
                    Ok(resp) => {
                        let status = resp.status();
                        if status.is_success() {
                            match resp.json::<CfResponse>().await {
                                Ok(cf_resp) => {
                                    if cf_resp.success {
                                        return Ok(cf_resp.result.unwrap_or_default());
                                    }
                                }
                                Err(e) => {
                                    last_error = CfError::NetworkError(format!("Failed to parse audio response: {e}"));
                                }
                            }
                        } else if is_auth_error(status.as_u16()) {
                            return Err(CfError::AuthError(format!("Auth failed: {status}")));
                        } else if status.as_u16() == 429 {
                            break;
                        } else if !is_retryable_status(status.as_u16()) {
                            return Err(CfError::ServerError(format!("CF API status {status}")));
                        }
                    }
                    Err(e) => {
                        last_error = CfError::NetworkError(format!("Connection error: {e}"));
                    }
                }
            }
        }

        Err(last_error)
    }
}

/// CF API 响应
#[derive(Debug, Serialize, Deserialize)]
pub struct CfResponse {
    pub success: bool,
    pub result: Option<Value>,
    #[serde(default)]
    pub errors: Vec<CfErrorInfo>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CfErrorInfo {
    pub code: Option<i64>,
    pub message: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CfModelInfo {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<CfModelTask>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CfModelTask {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// 错误类型
#[derive(Debug)]
pub enum CfError {
    AuthError(String),
    RateLimited { retry_after: Option<u64> },
    ServerError(String),
    ModelNotFound(String),
    QuotaExceeded,
    AllAccountsFailed(String),
    NetworkError(String),
}

impl std::fmt::Display for CfError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CfError::AuthError(msg) => write!(f, "Authentication error: {msg}"),
            CfError::RateLimited { retry_after } => {
                if let Some(secs) = retry_after {
                    write!(f, "Rate limited, retry after {secs}s")
                } else {
                    write!(f, "Rate limited")
                }
            }
            CfError::ServerError(msg) => write!(f, "Server error: {msg}"),
            CfError::ModelNotFound(name) => write!(f, "Model not found: {name}"),
            CfError::QuotaExceeded => write!(f, "Quota exceeded"),
            CfError::AllAccountsFailed(msg) => write!(f, "All accounts failed: {msg}"),
            CfError::NetworkError(msg) => write!(f, "Network error: {msg}"),
        }
    }
}

impl std::error::Error for CfError {}

/// 判断 HTTP 状态码是否可重试
fn is_retryable_status(status: u16) -> bool {
    matches!(status, 408 | 409 | 429 | 500..=599)
}

/// 判断是否为认证错误
fn is_auth_error(status: u16) -> bool {
    matches!(status, 401 | 403)
}

/// 从 CF 响应中提取文本输出（兼容多种格式）
pub fn process_text(output: &Value) -> Option<String> {
    if let Some(choices) = output.get("choices").and_then(|c| c.as_array()) {
        if let Some(first) = choices.first() {
            // chat completions: choices[0].message.content
            if let Some(content) = first
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_str())
            {
                return Some(content.to_string());
            }
            // text completions: choices[0].text
            if let Some(text) = first.get("text").and_then(|t| t.as_str()) {
                return Some(text.to_string());
            }
        }
    }
    if let Some(response) = output.get("response") {
        return match response {
            Value::String(s) => Some(s.clone()),
            Value::Number(n) => Some(n.to_string()),
            Value::Object(_) => Some(response.to_string()),
            _ => None,
        };
    }
    None
}

/// 从错误响应体中提取人类可读的错误信息
pub fn extract_error_message(raw: &Value) -> Option<String> {
    // CF 网关格式: { errors: [{ code, message }] }
    if let Some(errors) = raw.get("errors").and_then(|e| e.as_array()) {
        if let Some(first) = errors.first() {
            if let Some(msg) = first.get("message").and_then(|m| m.as_str()) {
                return Some(msg.to_string());
            }
        }
    }
    // 提供商格式: { error: { message } }
    if let Some(error) = raw.get("error") {
        if let Some(msg) = error.get("message").and_then(|m| m.as_str()) {
            return Some(msg.to_string());
        }
        if let Some(msg) = error.as_str() {
            return Some(msg.to_string());
        }
    }
    if let Some(msg) = raw.get("message").and_then(|m| m.as_str()) {
        return Some(msg.to_string());
    }
    None
}

/// 根据 CF 模型名获取 owned_by 信息
pub fn get_model_owned_by(cf_model: &str) -> &str {
    const OWNER_MAP: &[(&str, &str)] = &[
        ("@cf/meta/", "meta"),
        ("@cf/google/", "google"),
        ("@cf/mistral/", "mistral"),
        ("@cf/microsoft/", "microsoft"),
        ("@cf/openai/", "openai"),
        ("@cf/nvidia/", "nvidia"),
        ("@cf/deepseek-ai/", "deepseek"),
        ("@cf/qwen/", "qwen"),
        ("@cf/zai-org/", "zai-org"),
        ("@cf/moonshotai/", "moonshotai"),
        ("@cf/baai/", "baai"),
        ("@cf/stabilityai/", "stabilityai"),
        ("@cf/black-forest-labs/", "black-forest-labs"),
        ("@cf/codellama/", "codellama"),
        ("@cf/llava-hf/", "llava-hf"),
        ("@cf/internlm/", "internlm"),
        ("@cf/myshell-ai/", "myshell-ai"),
        ("@cf/moondream/", "moondream"),
    ];
    for (prefix, owner) in OWNER_MAP {
        if cf_model.starts_with(prefix) {
            return owner;
        }
    }
    "system"
}