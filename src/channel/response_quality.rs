//! 响应质量检测 — 评估上游响应质量（延迟/错误率/吞吐量），用于渠道选择。
//!
//! 参照 burncloud `crates/router/src/response_quality.rs`：
//! - 超越简单"空 vs 非空"判断，给出细粒度质量分级
//! - 分级喂给智能断路器/调度器做健康加权
//!
//! 质量分级：
//! - `Healthy`：完整响应，有 token，延迟正常
//! - `Partial`：流式中断但已收到部分 token
//! - `Empty`：HTTP 200 但零 token
//! - `Malformed`：响应无法解析
//! - `UpstreamError`：上游显式错误
//!
//! AIGX 单 crate，依赖 axum::http::HeaderMap + serde_json，与 burncloud 一致。

use axum::http::HeaderMap;
use serde::{Deserialize, Serialize};

/// 最小有效 token 数阈值。低于此数视为"空"。
const MIN_VALID_TOKENS: u32 = 1;

/// 响应质量分级（带详细指标）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ResponseQuality {
    /// 完整健康响应
    Healthy {
        /// 生成 token 数（部分 provider 为 input + output）
        tokens: u32,
        /// 响应延迟（毫秒）
        latency_ms: u64,
        /// 是否流式响应
        is_streaming: bool,
    },
    /// 部分响应 — 流式中断但已收到部分 token
    Partial {
        /// 中断前已收 token
        received_tokens: u32,
        /// 期望 token（若响应给出）
        expected_tokens: Option<u32>,
        /// 中断原因（若可判定）
        interruption_reason: Option<String>,
    },
    /// 空响应 — HTTP 成功但零 token
    Empty {
        /// HTTP 状态码（通常 200）
        http_status: u16,
        /// 原始响应体（调试用，可选）
        raw_body: Option<String>,
        /// Content-Type 头
        content_type: Option<String>,
    },
    /// 畸形响应 — 无法解析
    Malformed {
        /// 解析错误描述
        error: String,
        /// 原始响应体
        raw: String,
        /// HTTP 状态码
        http_status: u16,
    },
    /// 上游显式错误
    UpstreamError {
        /// HTTP 状态码
        code: u16,
        /// 错误消息
        message: String,
        /// 分类错误类型（喂给断路器）
        error_type: UpstreamErrorType,
    },
}

/// 上游错误分类（差异化处理）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum UpstreamErrorType {
    /// 限流（429）— 临时，可重试
    RateLimited {
        /// 限流作用域（账号或模型级）
        scope: RateLimitScope,
        /// 距重置秒数
        retry_after: Option<u64>,
    },
    /// 认证失败（401）— 永久，需更新密钥
    AuthFailed,
    /// 余额不足 / 配额耗尽（402）
    PaymentRequired,
    /// 模型不存在（404）— 配置问题
    ModelNotFound,
    /// 服务端错误（500）
    ServerError,
    /// 网关错误（502/503/504）
    GatewayError,
    /// 请求超时
    Timeout,
    /// 连接失败 — 网络/DNS/TLS
    ConnectionError,
    /// 服务过载（Anthropic 专用）
    Overloaded {
        /// 预计等待时间
        retry_after: Option<u64>,
    },
}

/// 限流作用域。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub enum RateLimitScope {
    /// 账号级（影响所有模型）
    Account,
    /// 模型级（仅特定模型）
    Model,
    /// 未知
    #[default]
    Unknown,
}

/// 响应质量检测配置。
#[derive(Debug, Clone)]
pub struct QualityDetectorConfig {
    /// 最小有效 token 数
    pub min_valid_tokens: u32,
    /// 是否捕获原始响应体（空/畸形时）
    pub capture_raw_body: bool,
    /// 原始响应体最大捕获长度（避免内存膨胀）
    pub max_raw_body_len: usize,
    /// "慢"响应延迟阈值（毫秒）
    pub slow_latency_threshold_ms: u64,
}

impl Default for QualityDetectorConfig {
    fn default() -> Self {
        Self {
            min_valid_tokens: MIN_VALID_TOKENS,
            capture_raw_body: true,
            max_raw_body_len: 1024,          // 1KB
            slow_latency_threshold_ms: 5000, // 5s
        }
    }
}

/// 响应质量检测器。
pub struct ResponseQualityDetector {
    config: QualityDetectorConfig,
}

impl ResponseQualityDetector {
    /// 默认配置构造。
    pub fn new() -> Self {
        Self {
            config: QualityDetectorConfig::default(),
        }
    }

    /// 自定义配置构造。
    pub fn with_config(config: QualityDetectorConfig) -> Self {
        Self { config }
    }

    /// 检测 HTTP 响应质量。
    ///
    /// # 参数
    /// - `http_status`：HTTP 状态码
    /// - `headers`：响应头
    /// - `body`：响应体字符串
    /// - `latency_ms`：响应延迟（毫秒）
    /// - `is_streaming`：是否流式响应
    /// - `channel_type`：provider 类型（openai/anthropic 等）
    pub fn detect(
        &self,
        http_status: u16,
        headers: &HeaderMap,
        body: &str,
        latency_ms: u64,
        is_streaming: bool,
        channel_type: &str,
    ) -> ResponseQuality {
        // 1. 非 2xx → 上游错误
        if http_status >= 400 {
            return self.classify_upstream_error(http_status, headers, body, channel_type);
        }

        // 2. 空响应体
        if body.is_empty() {
            return ResponseQuality::Empty {
                http_status,
                raw_body: None,
                content_type: headers
                    .get("content-type")
                    .and_then(|v| v.to_str().ok())
                    .map(|s| s.to_string()),
            };
        }

        // 2.5 SSE 流式错误（HTTP 200 + data: {...error...}）
        if body.starts_with("data: ") {
            let json_str = &body[6..];
            if json_str.trim() != "[DONE]" {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(json_str) {
                    if let Some(error) = json.get("error") {
                        let error_message = error
                            .get("message")
                            .and_then(|m| m.as_str())
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| "SSE 响应中的上游错误".to_string());
                        let error_code = error
                            .get("code")
                            .and_then(|c| c.as_u64())
                            .map(|c| c as u16)
                            .unwrap_or(400);
                        let error_type = self.classify_sse_error_code(error_code, &error_message);
                        return ResponseQuality::UpstreamError {
                            code: error_code,
                            message: error_message,
                            error_type,
                        };
                    }
                }
            }
        }

        // 3. 解析 token 数
        match self.parse_tokens(body, channel_type) {
            Ok(tokens) if tokens >= self.config.min_valid_tokens => ResponseQuality::Healthy {
                tokens,
                latency_ms,
                is_streaming,
            },
            Ok(_tokens_below_threshold) => ResponseQuality::Empty {
                http_status,
                raw_body: self.capture_raw_body(body),
                content_type: headers
                    .get("content-type")
                    .and_then(|v| v.to_str().ok())
                    .map(|s| s.to_string()),
            },
            Err(error) => ResponseQuality::Malformed {
                error,
                raw: self.truncate_raw_body(body),
                http_status,
            },
        }
    }

    /// 流式 chunk 质量检测（部分响应识别）。
    pub fn detect_stream_chunk(
        &self,
        chunk_data: &str,
        total_received_tokens: u32,
        is_final_chunk: bool,
        channel_type: &str,
    ) -> Option<ResponseQuality> {
        if is_final_chunk {
            if total_received_tokens >= self.config.min_valid_tokens {
                return None; // 成功
            } else {
                return Some(ResponseQuality::Empty {
                    http_status: 200,
                    raw_body: self.capture_raw_body(chunk_data),
                    content_type: None,
                });
            }
        }

        if chunk_data.contains("error") || chunk_data.contains("Error") {
            if let Some(error_type) = self.parse_stream_error(chunk_data, channel_type) {
                return Some(ResponseQuality::UpstreamError {
                    code: 400,
                    message: "检测到流式错误".to_string(),
                    error_type,
                });
            }
        }

        None
    }

    /// 分类上游 HTTP 错误。
    fn classify_upstream_error(
        &self,
        http_status: u16,
        headers: &HeaderMap,
        body: &str,
        channel_type: &str,
    ) -> ResponseQuality {
        let error_type = match http_status {
            429 => {
                let retry_after = headers
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok());
                let scope = self.detect_rate_limit_scope(body, channel_type);
                UpstreamErrorType::RateLimited { scope, retry_after }
            }
            401 => UpstreamErrorType::AuthFailed,
            402 => UpstreamErrorType::PaymentRequired,
            404 => UpstreamErrorType::ModelNotFound,
            500 => UpstreamErrorType::ServerError,
            502 | 503 | 504 => UpstreamErrorType::GatewayError,
            code if code >= 500 => UpstreamErrorType::ServerError,
            _ => UpstreamErrorType::ServerError,
        };

        let message = self.parse_error_message(body, channel_type);

        ResponseQuality::UpstreamError {
            code: http_status,
            message,
            error_type,
        }
    }

    /// 按 error_code 与 message 分类 SSE 错误。
    fn classify_sse_error_code(&self, error_code: u16, error_message: &str) -> UpstreamErrorType {
        let msg_lower = error_message.to_lowercase();

        if msg_lower.contains("rate limit") || msg_lower.contains("rate_limit") || error_code == 429
        {
            return UpstreamErrorType::RateLimited {
                scope: RateLimitScope::Unknown,
                retry_after: None,
            };
        }
        if msg_lower.contains("auth")
            || msg_lower.contains("appid")
            || msg_lower.contains("unauthorized")
            || msg_lower.contains("invalid key")
        {
            return UpstreamErrorType::AuthFailed;
        }
        if msg_lower.contains("quota")
            || msg_lower.contains("payment")
            || msg_lower.contains("billing")
        {
            return UpstreamErrorType::PaymentRequired;
        }
        if msg_lower.contains("not found")
            || (msg_lower.contains("model") && msg_lower.contains("not"))
        {
            return UpstreamErrorType::ModelNotFound;
        }
        if msg_lower.contains("overloaded") || msg_lower.contains("capacity") {
            return UpstreamErrorType::Overloaded { retry_after: None };
        }
        if msg_lower.contains("timeout") {
            return UpstreamErrorType::Timeout;
        }
        if msg_lower.contains("connection") || msg_lower.contains("network") {
            return UpstreamErrorType::ConnectionError;
        }
        UpstreamErrorType::ServerError
    }

    /// 按 provider 格式解析 token 数。
    fn parse_tokens(&self, body: &str, channel_type: &str) -> Result<u32, String> {
        match channel_type.to_lowercase().as_str() {
            "openai" | "azure" => self.parse_openai_tokens(body),
            "anthropic" | "claude" => self.parse_anthropic_tokens(body),
            "gemini" | "vertex" => self.parse_gemini_tokens(body),
            _ => self.parse_generic_tokens(body),
        }
    }

    /// OpenAI 格式 token 解析。
    fn parse_openai_tokens(&self, body: &str) -> Result<u32, String> {
        let json: serde_json::Value =
            serde_json::from_str(body).map_err(|e| format!("JSON 解析错误: {e}"))?;

        if json.get("error").is_some() {
            return Err("响应包含 error".to_string());
        }

        if let Some(usage) = json.get("usage") {
            let total = usage
                .get("total_tokens")
                .and_then(|t| t.as_u64())
                .map(|t| t as u32);
            if let Some(t) = total {
                return Ok(t);
            }
            let prompt = usage
                .get("prompt_tokens")
                .and_then(|p| p.as_u64())
                .unwrap_or(0);
            let completion = usage
                .get("completion_tokens")
                .and_then(|c| c.as_u64())
                .unwrap_or(0);
            return Ok((prompt + completion) as u32);
        }

        if let Some(choices) = json.get("choices").and_then(|c| c.as_array()) {
            for choice in choices {
                if let Some(delta) = choice.get("delta") {
                    if delta.get("content").is_some() {
                        return Ok(1);
                    }
                }
                if let Some(message) = choice.get("message") {
                    if message.get("content").is_some() {
                        return Ok(1);
                    }
                }
            }
        }

        Err("OpenAI 响应中未找到 token".to_string())
    }

    /// Anthropic 格式 token 解析。
    fn parse_anthropic_tokens(&self, body: &str) -> Result<u32, String> {
        let json: serde_json::Value =
            serde_json::from_str(body).map_err(|e| format!("JSON 解析错误: {e}"))?;

        if json.get("type").and_then(|t| t.as_str()) == Some("error") {
            return Err("响应包含 error".to_string());
        }

        if let Some(usage) = json.get("usage") {
            let output = usage.get("output_tokens").and_then(|t| t.as_u64());
            let input = usage.get("input_tokens").and_then(|t| t.as_u64());
            if let (Some(o), Some(i)) = (output, input) {
                return Ok((o + i) as u32);
            }
            if let Some(o) = output {
                return Ok(o as u32);
            }
        }

        if let Some(content) = json.get("content").and_then(|c| c.as_array()) {
            if !content.is_empty() {
                return Ok(1);
            }
        }

        if json.get("type").and_then(|t| t.as_str()) == Some("content_block_delta") {
            return Ok(1);
        }

        Err("Anthropic 响应中未找到 token".to_string())
    }

    /// Gemini 格式 token 解析。
    fn parse_gemini_tokens(&self, body: &str) -> Result<u32, String> {
        let json: serde_json::Value =
            serde_json::from_str(body).map_err(|e| format!("JSON 解析错误: {e}"))?;

        if json.get("error").is_some() {
            return Err("响应包含 error".to_string());
        }

        if let Some(usage) = json.get("usageMetadata") {
            let total = usage
                .get("totalTokenCount")
                .and_then(|t| t.as_u64())
                .map(|t| t as u32);
            if let Some(t) = total {
                return Ok(t);
            }
        }

        if let Some(candidates) = json.get("candidates").and_then(|c| c.as_array()) {
            if !candidates.is_empty() {
                return Ok(1);
            }
        }

        Err("Gemini 响应中未找到 token".to_string())
    }

    /// 通用 token 解析回退。
    fn parse_generic_tokens(&self, body: &str) -> Result<u32, String> {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(body) {
            if json.get("error").is_some() {
                return Err("响应包含 error".to_string());
            }
            if json.get("choices").is_some() || json.get("content").is_some() {
                return Ok(1);
            }
            if let Some(usage) = json.get("usage") {
                if usage.get("total_tokens").is_some() {
                    return usage
                        .get("total_tokens")
                        .and_then(|t| t.as_u64())
                        .map(|t| t as u32)
                        .ok_or_else(|| "无法解析 total_tokens".to_string());
                }
            }
        }

        Err("无法从响应解析 token".to_string())
    }

    /// 解析错误消息。
    fn parse_error_message(&self, body: &str, _channel_type: &str) -> String {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(body) {
            let msg = json
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(|m| m.as_str())
                .or_else(|| json.get("message").and_then(|m| m.as_str()))
                .or_else(|| json.get("error").and_then(|e| e.as_str()));

            if let Some(m) = msg {
                return m.to_string();
            }
        }
        self.truncate_raw_body(body)
    }

    /// 检测限流作用域。
    fn detect_rate_limit_scope(&self, body: &str, _channel_type: &str) -> RateLimitScope {
        let body_lower = body.to_lowercase();
        if body_lower.contains("account") || body_lower.contains("organization") {
            RateLimitScope::Account
        } else if body_lower.contains("model") {
            RateLimitScope::Model
        } else {
            RateLimitScope::Unknown
        }
    }

    /// 解析流式 chunk 中的错误。
    fn parse_stream_error(&self, chunk: &str, channel_type: &str) -> Option<UpstreamErrorType> {
        let json_str = if chunk.starts_with("data: ") {
            &chunk[6..]
        } else {
            chunk
        };

        if let Ok(json) = serde_json::from_str::<serde_json::Value>(json_str) {
            let error_type = json.get("type").and_then(|t| t.as_str());

            match channel_type.to_lowercase().as_str() {
                "anthropic" | "claude" => match error_type {
                    Some("error") | Some("api_error") => {
                        let msg = json
                            .get("error")
                            .and_then(|e| e.as_str())
                            .or_else(|| json.get("message").and_then(|m| m.as_str()));
                        if let Some(m) = msg {
                            if m.to_lowercase().contains("overloaded") {
                                return Some(UpstreamErrorType::Overloaded { retry_after: None });
                            }
                        }
                        return Some(UpstreamErrorType::ServerError);
                    }
                    Some("rate_limit_error") => {
                        return Some(UpstreamErrorType::RateLimited {
                            scope: RateLimitScope::Unknown,
                            retry_after: None,
                        });
                    }
                    _ => {}
                },
                "openai" | "azure" => {
                    if let Some(error) = json.get("error") {
                        let error_type_str = error.get("type").and_then(|t| t.as_str());
                        match error_type_str {
                            Some("rate_limit_exceeded") => {
                                return Some(UpstreamErrorType::RateLimited {
                                    scope: RateLimitScope::Unknown,
                                    retry_after: None,
                                });
                            }
                            Some(_) => {
                                return Some(UpstreamErrorType::ServerError);
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// 捕获原始响应体（受 max 长度限制）。
    fn capture_raw_body(&self, body: &str) -> Option<String> {
        if self.config.capture_raw_body {
            Some(self.truncate_raw_body(body))
        } else {
            None
        }
    }

    /// 截断原始响应体至最大长度。
    fn truncate_raw_body(&self, body: &str) -> String {
        if body.len() > self.config.max_raw_body_len {
            // 安全截断：在 char 边界截断
            let mut end = self.config.max_raw_body_len;
            while end > 0 && !body.is_char_boundary(end) {
                end -= 1;
            }
            format!("{}...", &body[..end])
        } else {
            body.to_string()
        }
    }

    /// 把 `ResponseQuality` 转为健康分（0.0 ~ 1.0）。
    ///
    /// 喂给智能断路器/调度器的健康计算。
    pub fn quality_to_health_score(quality: &ResponseQuality) -> f64 {
        match quality {
            ResponseQuality::Healthy {
                tokens, latency_ms, ..
            } => {
                let latency_penalty = if *latency_ms > 5000 {
                    0.7
                } else if *latency_ms > 2000 {
                    0.9
                } else {
                    1.0
                };
                let token_bonus: f64 = if *tokens > 1000 { 1.05 } else { 1.0 };
                latency_penalty * token_bonus.min(1.0)
            }
            ResponseQuality::Partial {
                received_tokens, ..
            } => {
                if *received_tokens > 0 {
                    0.5
                } else {
                    0.1
                }
            }
            ResponseQuality::Empty { .. } => 0.0,
            ResponseQuality::Malformed { .. } => 0.1,
            ResponseQuality::UpstreamError { error_type, .. } => match error_type {
                UpstreamErrorType::RateLimited { .. } => 0.3,
                UpstreamErrorType::Overloaded { .. } => 0.3,
                UpstreamErrorType::ServerError => 0.2,
                UpstreamErrorType::GatewayError => 0.2,
                UpstreamErrorType::Timeout => 0.2,
                UpstreamErrorType::ConnectionError => 0.1,
                UpstreamErrorType::AuthFailed => 0.0,
                UpstreamErrorType::PaymentRequired => 0.0,
                UpstreamErrorType::ModelNotFound => 0.0,
            },
        }
    }
}

impl Default for ResponseQualityDetector {
    fn default() -> Self {
        Self::new()
    }
}

/// 检查 SSE chunk 是否包含错误。
/// 返回 `Some((error_code, error_message, is_auth_error))` 表示有错误。
pub fn check_sse_error_in_chunk(chunk: &[u8]) -> Option<(u16, String, bool)> {
    let text = String::from_utf8_lossy(chunk);
    for line in text.lines() {
        let line = line.trim();
        if !line.starts_with("data: ") {
            continue;
        }
        let data = &line[6..];
        if data.trim() == "[DONE]" {
            continue;
        }
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(data) {
            if let Some(error) = json.get("error") {
                let error_msg = error
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("Unknown SSE error")
                    .to_string();
                let error_code = error.get("code").and_then(|c| c.as_u64()).unwrap_or(400) as u16;

                let msg_lower = error_msg.to_lowercase();
                let is_auth_error = msg_lower.contains("auth")
                    || msg_lower.contains("appid")
                    || msg_lower.contains("unauthorized")
                    || msg_lower.contains("invalid key")
                    || error_code == 401;

                return Some((error_code, error_msg, is_auth_error));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn detect_healthy_openai_response() {
        let detector = ResponseQualityDetector::new();
        let mut headers = HeaderMap::new();
        headers.insert("content-type", HeaderValue::from_static("application/json"));

        let body = r#"{"choices":[{"message":{"content":"Hello"}}],"usage":{"total_tokens":10}}"#;

        let quality = detector.detect(200, &headers, body, 100, false, "openai");

        match quality {
            ResponseQuality::Healthy { tokens, .. } => {
                assert_eq!(tokens, 10);
            }
            _ => panic!("期望 Healthy"),
        }
    }

    #[test]
    fn detect_empty_response() {
        let detector = ResponseQualityDetector::new();
        let headers = HeaderMap::new();

        let quality = detector.detect(200, &headers, "", 100, false, "openai");

        match quality {
            ResponseQuality::Empty { http_status, .. } => {
                assert_eq!(http_status, 200);
            }
            _ => panic!("期望 Empty"),
        }
    }

    #[test]
    fn detect_rate_limit_error() {
        let detector = ResponseQualityDetector::new();
        let mut headers = HeaderMap::new();
        headers.insert("retry-after", HeaderValue::from_static("30"));

        let body = r#"{"error":{"message":"Rate limit exceeded","type":"rate_limit_error"}}"#;

        let quality = detector.detect(429, &headers, body, 100, false, "anthropic");

        match quality {
            ResponseQuality::UpstreamError {
                code, error_type, ..
            } => {
                assert_eq!(code, 429);
                match error_type {
                    UpstreamErrorType::RateLimited { retry_after, .. } => {
                        assert_eq!(retry_after.clone(), Some(30));
                    }
                    _ => panic!("期望 RateLimited"),
                }
            }
            _ => panic!("期望 UpstreamError"),
        }
    }

    #[test]
    fn health_score_calculation() {
        let healthy = ResponseQuality::Healthy {
            tokens: 100,
            latency_ms: 100,
            is_streaming: false,
        };
        assert_eq!(
            ResponseQualityDetector::quality_to_health_score(&healthy),
            1.0
        );

        let slow_healthy = ResponseQuality::Healthy {
            tokens: 100,
            latency_ms: 6000,
            is_streaming: false,
        };
        assert_eq!(
            ResponseQualityDetector::quality_to_health_score(&slow_healthy),
            0.7
        );

        let empty = ResponseQuality::Empty {
            http_status: 200,
            raw_body: None,
            content_type: None,
        };
        assert_eq!(
            ResponseQualityDetector::quality_to_health_score(&empty),
            0.0
        );

        let rate_limited = ResponseQuality::UpstreamError {
            code: 429,
            message: "Rate limit".to_string(),
            error_type: UpstreamErrorType::RateLimited {
                scope: RateLimitScope::Unknown,
                retry_after: Some(30),
            },
        };
        assert_eq!(
            ResponseQualityDetector::quality_to_health_score(&rate_limited),
            0.3
        );
    }

    #[test]
    fn detect_anthropic_healthy() {
        let detector = ResponseQualityDetector::new();
        let headers = HeaderMap::new();
        let body = r#"{"type":"message","content":[{"type":"text","text":"hi"}],"usage":{"input_tokens":5,"output_tokens":7}}"#;
        let quality = detector.detect(200, &headers, body, 50, false, "anthropic");
        match quality {
            ResponseQuality::Healthy { tokens, .. } => assert_eq!(tokens, 12),
            _ => panic!("期望 Healthy"),
        }
    }

    #[test]
    fn detect_gemini_healthy() {
        let detector = ResponseQualityDetector::new();
        let headers = HeaderMap::new();
        let body = r#"{"candidates":[{"content":{"parts":[{"text":"hi"}]}}],"usageMetadata":{"totalTokenCount":8}}"#;
        let quality = detector.detect(200, &headers, body, 50, false, "gemini");
        match quality {
            ResponseQuality::Healthy { tokens, .. } => assert_eq!(tokens, 8),
            _ => panic!("期望 Healthy"),
        }
    }

    #[test]
    fn detect_malformed_json() {
        let detector = ResponseQualityDetector::new();
        let headers = HeaderMap::new();
        let body = "not json at all";
        let quality = detector.detect(200, &headers, body, 50, false, "openai");
        match quality {
            ResponseQuality::Malformed { http_status, .. } => assert_eq!(http_status, 200),
            _ => panic!("期望 Malformed"),
        }
    }

    #[test]
    fn detect_404_model_not_found() {
        let detector = ResponseQualityDetector::new();
        let headers = HeaderMap::new();
        let body = r#"{"error":{"message":"model not found"}}"#;
        let quality = detector.detect(404, &headers, body, 50, false, "openai");
        match quality {
            ResponseQuality::UpstreamError {
                code, error_type, ..
            } => {
                assert_eq!(code, 404);
                assert_eq!(error_type, UpstreamErrorType::ModelNotFound);
            }
            _ => panic!("期望 UpstreamError"),
        }
    }

    #[test]
    fn check_sse_error_in_chunk_detects_error() {
        let chunk = b"data: {\"error\":{\"code\":401,\"message\":\"unauthorized\"}}\n";
        let result = check_sse_error_in_chunk(chunk);
        assert!(result.is_some());
        let (code, msg, is_auth) = result.unwrap();
        assert_eq!(code, 401);
        assert!(is_auth);
        assert!(msg.contains("unauthorized"));
    }

    #[test]
    fn check_sse_error_in_chunk_ignores_done() {
        let chunk = b"data: [DONE]\n";
        assert!(check_sse_error_in_chunk(chunk).is_none());
    }

    #[test]
    fn detect_stream_chunk_final_with_tokens() {
        let detector = ResponseQualityDetector::new();
        // 最终 chunk 且 token 足阈值 → None（成功）
        assert!(detector
            .detect_stream_chunk("data: [DONE]", 10, true, "openai")
            .is_none());
        // 最终 chunk 但零 token → Empty
        let result = detector.detect_stream_chunk("data: [DONE]", 0, true, "openai");
        assert!(matches!(result, Some(ResponseQuality::Empty { .. })));
    }

    #[test]
    fn truncate_raw_body_handles_multibyte() {
        let detector = ResponseQualityDetector::with_config(QualityDetectorConfig {
            max_raw_body_len: 3, // 极小阈值测试多字节安全截断
            ..Default::default()
        });
        // 中文字符（每个 3 字节）
        let truncated = detector.truncate_raw_body("你好世界");
        assert!(truncated.ends_with("..."));
        // 不应 panic（在 char 边界截断）
    }
}
