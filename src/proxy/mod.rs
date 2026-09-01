use futures::stream::BoxStream;
use futures::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;

use crate::account::{AccountPool, CfAccount};
use crate::model::ModelMapper;

/// CF Workers AI API 客户端（AI Binding 桥接方式）。
///
/// ## 架构说明：Binding 方式（不使用 REST API）
///
/// AIGX 不再直接调用 Cloudflare REST API（`api.cloudflare.com/client/v4/accounts/...`）。
/// 而是通过 HTTP 调用 **cf-ai-gw Worker**（Cloudflare Workers 上部署的网关），
/// Worker 内部使用 **AI Binding**（`env.AI.run(model, input)`）直接调用 Workers AI。
///
/// 链路：
/// ```text
/// AIGX (Rust) --HTTP--> cf-ai-gw Worker --AI Binding--> Cloudflare Workers AI
/// ```
///
/// 好处：
/// - AI Binding 是 Worker 内部 RPC，零额外网络跳转，无需 API Token 鉴权
/// - 自动享受 Cloudflare 免费额度（`@cf/` 开头的模型）
/// - 风控规避最佳（cf-ai-gw 的模式 A），无需暴露 API Token
///
/// 每个 cf-ai-gw Worker 对应一个「账号」（账号 = Worker 部署），账号的
/// `account_id` / `api_token` 字段语义变为：`account_id` = Worker 地址，
/// `api_token` = 调用该 Worker 时使用的 API Key（cf-ai-gw 管理面板中创建；
/// 空则匿名调用，需 cf-ai-gw 未启用代理鉴权）。多账号负载均衡与故障切换
/// 仍在 AIGX 层面按账号池实现。
pub struct CfApiClient {
    http: Client,
    account_pool: Arc<AccountPool>,
    model_mapper: Arc<ModelMapper>,
    /// 兜底 cf-ai-gw Worker 地址（无账号时使用，取自配置 `cf_binding_url`）
    fallback_url: String,
    /// 兜底调用凭证（空则匿名）
    fallback_key: String,
}

impl CfApiClient {
    pub fn new(account_pool: Arc<AccountPool>, model_mapper: Arc<ModelMapper>) -> Self {
        let http = Client::builder()
            .timeout(Duration::from_secs(120))
            .user_agent("aigx/0.1.0")
            .build()
            .expect("Failed to create HTTP client");
        Self {
            http,
            account_pool,
            model_mapper,
            fallback_url: String::new(),
            fallback_key: String::new(),
        }
    }

    /// 配置兜底 cf-ai-gw Worker 地址与凭证（启动时由 `AppState` 调用）。
    pub fn with_fallback(&mut self, url: String, key: String) {
        self.fallback_url = url.trim().trim_end_matches('/').to_string();
        self.fallback_key = key;
    }

    /// 解析账号 → (worker_url, api_key) 的桥接目标。
    fn resolve_target(&self, account: &CfAccount) -> (String, String) {
        // account_id 即 cf-ai-gw Worker 部署地址（含 scheme 的 URL 或裸域名）。
        // 兼容 wrangler dev 本地调试地址：127.0.0.1 / localhost 视为 http://，
        // 其余裸域名默认补 https://。
        let mut url = account.account_id.trim().trim_end_matches('/').to_string();
        if !url.starts_with("http://") && !url.starts_with("https://") {
            let is_localhost =
                url.starts_with("127.0.0.1") || url.starts_with("localhost") || url.starts_with("::1");
            url = if is_localhost {
                format!("http://{url}")
            } else {
                format!("https://{url}")
            };
        }
        let key = account.api_token.trim().to_string();
        (url, key)
    }

    /// 调用 cf-ai-gw Worker（AI Binding），带多账号故障转移。
    ///
    /// - 优先遍历账号池中的活跃账号，把请求转发到对应的 cf-ai-gw Worker
    /// - 无账号时回退到配置的兜底 `cf_binding_url`
    pub async fn call_ai(&self, model: &str, body: Value) -> std::result::Result<CfResponse, CfError> {
        let cf_model = self.model_mapper.resolve(model);
        let active_accounts = self.account_pool.active_accounts();

        if active_accounts.is_empty() {
            if self.fallback_url.is_empty() {
                return Err(CfError::AuthError(
                    "No cf-ai-gw Worker accounts configured and no fallback cf_binding_url".into(),
                ));
            }
            return self.call_worker(&self.fallback_url, &self.fallback_key, &cf_model, body).await;
        }

        let mut last_error = CfError::AllAccountsFailed("All cf-ai-gw Workers exhausted".into());

        for account in &active_accounts {
            for attempt in 0..2 {
                if attempt > 0 {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }

                let (url, key) = self.resolve_target(account);
                let result = self.call_worker(&url, &key, &cf_model, body.clone()).await;

                match result {
                    Ok(cf_resp) => {
                        self.account_pool.mark_used(&account.id);
                        return Ok(cf_resp);
                    }
                    Err(e) => match &e {
                        // 认证失败只代表该 Worker 的 API Key 失效，跳过本账号剩余重试、继续下一账号
                        CfError::AuthError(_) => {
                            last_error = e;
                            break;
                        }
                        // 限流时跳过当前账号剩余重试
                        CfError::RateLimited { .. } => {
                            last_error = e;
                            break;
                        }
                        // 模型不存在，不可重试
                        CfError::ModelNotFound(_) => return Err(e),
                        _ => {
                            last_error = e;
                        }
                    },
                }
            }
        }

        Err(last_error)
    }

    /// 向单个 cf-ai-gw Worker 发起一次调用（内部 HTTP POST）。
    ///
    /// 目标端点固定为 `/v1/chat/completions`（cf-ai-gw 的 OpenAI 兼容代理入口，
    /// 内部经 AI Binding 转发到 Workers AI）。注意：`/v1/chat/completions`
    /// 的 `/v1/completions`、`/v1/embeddings`、`/v1/images/generations`、
    /// `/v1/audio/transcriptions` 路由在 cf-ai-gw 中均存在；统一走
    /// `/v1/chat/completions` 会在 chat 模型上误用文本补全请求，因此
    /// 各调用方需要按语义选择正确端点。
    ///
    /// 当前实现将 `body` 直接透传给 Worker，Worker 侧的模型名解析
    /// （自定义映射 / `@cf/` 直通 / 兜底）与用量统计都由 Worker 处理。
    async fn call_worker(
        &self,
        worker_url: &str,
        api_key: &str,
        cf_model: &str,
        body: Value,
    ) -> std::result::Result<CfResponse, CfError> {
        let url = format!("{worker_url}/v1/chat/completions");
        let mut req = self
            .http
            .post(&url)
            .json(&body);
        if !api_key.is_empty() {
            req = req.bearer_auth(api_key);
        }

        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) => {
                return Err(CfError::NetworkError(format!("Connection error: {e}")));
            }
        };

        let status = resp.status();
        if status.is_success() {
            let v: Value = match resp.json().await {
                Ok(v) => v,
                Err(e) => {
                    return Err(CfError::NetworkError(format!("Failed to parse response: {e}")));
                }
            };
            // cf-ai-gw 返回 OpenAI 兼容结构 { choices: [...], usage: {...} }
            Ok(CfResponse {
                success: true,
                result: Some(v),
                errors: Vec::new(),
            })
        } else if is_auth_error(status.as_u16()) {
            Err(CfError::AuthError(format!("Auth failed: {status}")))
        } else if status.as_u16() == 429 {
            let retry_after = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok());
            Err(CfError::RateLimited { retry_after })
        } else if status.as_u16() == 404 {
            Err(CfError::ModelNotFound(format!("Model {cf_model} not found")))
        } else if !is_retryable_status(status.as_u16()) {
            let body_text = resp.text().await.unwrap_or_default();
            Err(CfError::ServerError(format!("cf-ai-gw status {status}: {body_text}")))
        } else {
            let body_text = resp.text().await.unwrap_or_default();
            Err(CfError::ServerError(format!("cf-ai-gw status {status}: {body_text}")))
        }
    }

    /// 调用 cf-ai-gw Worker 的 `/v1/models`（AI Binding 方式）。
    ///
    /// 返回 Worker 内置模型映射（`@cf/...`）。AIGX 的模型列表主要来自
    /// `ModelMapper`（默认映射 + 自定义映射），此方法作为动态发现补充。
    pub async fn list_models(&self) -> std::result::Result<Vec<CfModelInfo>, CfError> {
        let active_accounts = self.account_pool.active_accounts();

        if active_accounts.is_empty() {
            if self.fallback_url.is_empty() {
                return Err(CfError::AuthError(
                    "No cf-ai-gw Worker accounts configured and no fallback cf_binding_url".into(),
                ));
            }
            return self.list_models_from_worker(&self.fallback_url, &self.fallback_key).await;
        }

        let mut last_error = CfError::AllAccountsFailed("Failed to list models from all workers".into());

        for account in &active_accounts {
            let (url, key) = self.resolve_target(account);
            match self.list_models_from_worker(&url, &key).await {
                Ok(models) => return Ok(models),
                Err(e) => {
                    tracing::warn!("Failed to list models from worker {}: {e}", account.name);
                    last_error = e;
                }
            }
        }

        Err(last_error)
    }

    async fn list_models_from_worker(
        &self,
        worker_url: &str,
        api_key: &str,
    ) -> std::result::Result<Vec<CfModelInfo>, CfError> {
        let url = format!("{worker_url}/v1/models");
        let mut req = self.http.get(&url);
        if !api_key.is_empty() {
            req = req.bearer_auth(api_key);
        }

        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) => return Err(CfError::NetworkError(format!("Connection error: {e}"))),
        };

        if !resp.status().is_success() {
            let status = resp.status();
            return Err(CfError::ServerError(format!("list models status {status}")));
        }

        let v: Value = match resp.json().await {
            Ok(v) => v,
            Err(e) => return Err(CfError::NetworkError(format!("Failed to parse models response: {e}"))),
        };

        let models: Vec<CfModelInfo> = v
            .get("data")
            .and_then(|d| d.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|m| {
                        let name = m.get("id").and_then(|i| i.as_str())?.to_string();
                        Some(CfModelInfo {
                            name,
                            task: None,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        Ok(models)
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

    /// 流式调用 cf-ai-gw Worker（AI Binding，`stream: true`）。
    ///
    /// 返回原始字节流，调用方用 `crate::sse::SseDecoder` 解析为 SSE 事件。
    /// 请求体带 `stream: true` 时 cf-ai-gw 会以 `text/event-stream` 返回
    /// OpenAI 风格 SSE（内部经 AI Binding 流式输出）。
    pub async fn run_text_stream(
        &self,
        model: &str,
        body: Value,
    ) -> std::result::Result<BoxStream<'static, std::result::Result<bytes::Bytes, CfError>>, CfError> {
        let cf_model = self.model_mapper.resolve(model);
        let active_accounts = self.account_pool.active_accounts();

        if active_accounts.is_empty() {
            if self.fallback_url.is_empty() {
                return Err(CfError::AuthError(
                    "No cf-ai-gw Worker accounts configured and no fallback cf_binding_url".into(),
                ));
            }
            return self.stream_from_worker(&self.fallback_url, &self.fallback_key, &cf_model, body).await;
        }

        let mut last_error = CfError::AllAccountsFailed("All cf-ai-gw Workers exhausted".into());
        for account in &active_accounts {
            let (url, key) = self.resolve_target(account);
            match self.stream_from_worker(&url, &key, &cf_model, body.clone()).await {
                Ok(stream) => {
                    self.account_pool.mark_used(&account.id);
                    return Ok(stream);
                }
                Err(e) => match &e {
                    // 认证失败只代表该 Worker 的 API Key 失效，继续尝试下一账号
                    CfError::AuthError(_) => {
                        last_error = e;
                        continue;
                    }
                    CfError::RateLimited { .. } => {
                        last_error = e;
                        continue;
                    }
                    CfError::ModelNotFound(_) => return Err(e),
                    _ => {
                        last_error = e;
                    }
                },
            }
        }
        Err(last_error)
    }

    async fn stream_from_worker(
        &self,
        worker_url: &str,
        api_key: &str,
        cf_model: &str,
        body: Value,
    ) -> std::result::Result<BoxStream<'static, std::result::Result<bytes::Bytes, CfError>>, CfError> {
        let url = format!("{worker_url}/v1/chat/completions");
        let mut req = self
            .http
            .post(&url)
            .json(&body);
        if !api_key.is_empty() {
            req = req.bearer_auth(api_key);
        }

        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) => return Err(CfError::NetworkError(format!("Connection error: {e}"))),
        };

        let status = resp.status();
        if status.is_success() {
            let stream = resp
                .bytes_stream()
                .map(|chunk| chunk.map_err(|e| CfError::NetworkError(format!("stream read error: {e}"))));
            Ok(Box::pin(stream))
        } else if is_auth_error(status.as_u16()) {
            Err(CfError::AuthError(format!("Auth failed: {status}")))
        } else if status.as_u16() == 429 {
            Err(CfError::RateLimited { retry_after: None })
        } else if status.as_u16() == 404 {
            Err(CfError::ModelNotFound(format!("Model {cf_model} not found")))
        } else {
            let body_text = resp.text().await.unwrap_or_default();
            Err(CfError::ServerError(format!("cf-ai-gw status {status}: {body_text}")))
        }
    }

    /// 调用 cf-ai-gw Worker 语音识别（AI Binding）。
    ///
    /// cf-ai-gw 的 `/v1/audio/transcriptions` 接收 multipart/form-data
    /// （字段 `file` + `model`），内部通过 AI Binding 调用 Whisper。
    /// 这里将原始音频字节以 multipart 形式转发到 Worker。
    pub async fn run_audio(
        &self,
        model: &str,
        data: Vec<u8>,
        _content_type: &str,
    ) -> std::result::Result<Value, CfError> {
        let cf_model = self.model_mapper.resolve(model);
        let active_accounts = self.account_pool.active_accounts();

        if active_accounts.is_empty() {
            if self.fallback_url.is_empty() {
                return Err(CfError::AuthError(
                    "No cf-ai-gw Worker accounts configured and no fallback cf_binding_url".into(),
                ));
            }
            return self.audio_from_worker(&self.fallback_url, &self.fallback_key, &cf_model, data).await;
        }

        let mut last_error = CfError::AllAccountsFailed("All cf-ai-gw Workers exhausted".into());

        for account in &active_accounts {
            for attempt in 0..2 {
                if attempt > 0 {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }

                let (url, key) = self.resolve_target(account);
                match self.audio_from_worker(&url, &key, &cf_model, data.clone()).await {
                    Ok(v) => {
                        self.account_pool.mark_used(&account.id);
                        return Ok(v);
                    }
                    Err(e) => match &e {
                        CfError::AuthError(_) => {
                            last_error = e;
                            break;
                        }
                        CfError::RateLimited { .. } => {
                            last_error = e;
                            break;
                        }
                        CfError::ModelNotFound(_) => return Err(e),
                        _ => {
                            last_error = e;
                        }
                    },
                }
            }
        }

        Err(last_error)
    }

    async fn audio_from_worker(
        &self,
        worker_url: &str,
        api_key: &str,
        cf_model: &str,
        data: Vec<u8>,
    ) -> std::result::Result<Value, CfError> {
        let url = format!("{worker_url}/v1/audio/transcriptions");

        // 构造 multipart 表单：字段 file（音频字节，泛型文件名）+ model
        let mut form = reqwest::multipart::Form::new();
        form = form.part(
            "file",
            reqwest::multipart::Part::bytes(data)
                .file_name("audio.bin")
                .mime_str("application/octet-stream")
                .map_err(|e| CfError::ServerError(format!("mime error: {e}")))?,
        );
        form = form.text("model", cf_model.to_string());

        let mut req = self.http.post(&url).multipart(form);
        if !api_key.is_empty() {
            req = req.bearer_auth(api_key);
        }

        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) => return Err(CfError::NetworkError(format!("Connection error: {e}"))),
        };

        let status = resp.status();
        if status.is_success() {
            let v: Value = match resp.json().await {
                Ok(v) => v,
                Err(e) => {
                    return Err(CfError::NetworkError(format!("Failed to parse audio response: {e}")));
                }
            };
            Ok(v)
        } else if is_auth_error(status.as_u16()) {
            Err(CfError::AuthError(format!("Auth failed: {status}")))
        } else if status.as_u16() == 429 {
            Err(CfError::RateLimited { retry_after: None })
        } else if status.as_u16() == 404 {
            Err(CfError::ModelNotFound(format!("Model {cf_model} not found")))
        } else {
            let body_text = resp.text().await.unwrap_or_default();
            Err(CfError::ServerError(format!("cf-ai-gw status {status}: {body_text}")))
        }
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