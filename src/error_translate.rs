//! 跨协议上游错误 `code` 推导与 OpenAI 兼容错误响应生成。
//!
//! 参考 aisix 的 `error_translate` 模块设计。每个上游提供商使用不同的
//! 错误分类体系：
//!
//! | 协议        | 有结构化 `code`? | 原生 `type` 示例                          |
//! |-------------|------------------|-------------------------------------------|
//! | OpenAI      | 是               | `rate_limit_exceeded`, `invalid_api_key`  |
//! | Anthropic   | 否               | `rate_limit_error`, `overloaded_error`    |
//! | Bedrock     | 否               | `ThrottlingException`, `ValidationException` |
//! | Vertex      | 否               | `RESOURCE_EXHAUSTED`, `PERMISSION_DENIED` |
//! | AzureOpenAI | 部分             | 多数与 OpenAI 一致，少数有 content policy 特例 |
//!
//! 当上游不暴露稳定的 `code` 字符串时，此模块从上游 `type` 推导出一个
//! OpenAI 风格的 `code`，使客户端 SDK 的 `error.code` 分支逻辑可以
//! 跨上游工作。

use crate::bridge::{BridgeError, UpstreamErrorView, UpstreamWire};

/// 对任何上游错误渲染的稳定 `error.type` 标记
const UPSTREAM_ERROR_TYPE: &str = "upstream_error";

/// OpenAI 兼容的错误响应体
#[derive(Debug, Clone, serde::Serialize)]
pub struct ErrorBody {
    pub message: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub param: Option<String>,
    pub code: Option<String>,
}

/// 将 BridgeError 转换为 OpenAI 兼容的 JSON 错误响应
///
/// 根据 `error.http_status()` 和 `error.error_type()` 生成标准化的
/// OpenAI 错误格式。对于 `UpstreamStatus` 变体，会根据 `UpstreamWire`
/// 进行跨协议错误码推导。
pub fn render_openai_error(error: &BridgeError) -> (u16, ErrorBody) {
    let status = error.http_status();
    let body = match error {
        BridgeError::Timeout { .. } => ErrorBody {
            message: error.to_string(),
            kind: "timeout".into(),
            param: None,
            code: Some("timeout".into()),
        },
        BridgeError::UpstreamStatus {
            status: s,
            parsed,
            wire,
            message,
            ..
        } => {
            let body = render_upstream_error(parsed.as_deref(), *wire, message);
            // 4xx 透传，非 4xx 转为 502
            let adjusted_status = if *s >= 400 && *s < 500 { *s } else { 502 };
            return (adjusted_status, body);
        }
        BridgeError::UpstreamDecode(msg) => ErrorBody {
            message: msg.clone(),
            kind: "upstream_decode_error".into(),
            param: None,
            code: None,
        },
        BridgeError::Config(msg) => ErrorBody {
            message: msg.clone(),
            kind: "config_error".into(),
            param: None,
            code: None,
        },
        BridgeError::InvalidUpstreamConfig(msg) => ErrorBody {
            message: msg.clone(),
            kind: "invalid_request_error".into(),
            param: None,
            code: Some("invalid_request_error".into()),
        },
        BridgeError::InvalidUpstreamCredentials(msg) => ErrorBody {
            message: msg.clone(),
            kind: "authentication_error".into(),
            param: None,
            code: Some("invalid_api_key".into()),
        },
        BridgeError::Transport(msg) => ErrorBody {
            message: msg.clone(),
            kind: "transport_error".into(),
            param: None,
            code: None,
        },
        BridgeError::StreamAborted => ErrorBody {
            message: "upstream cancelled the response mid-stream".into(),
            kind: "stream_aborted".into(),
            param: None,
            code: Some("stream_error".into()),
        },
        BridgeError::AuthError(msg) => ErrorBody {
            message: msg.clone(),
            kind: "authentication_error".into(),
            param: None,
            code: Some("invalid_api_key".into()),
        },
        BridgeError::RateLimited => ErrorBody {
            message: "rate limited".into(),
            kind: "rate_limit_error".into(),
            param: None,
            code: Some("rate_limit_exceeded".into()),
        },
        BridgeError::ModelNotFound(msg) => ErrorBody {
            message: msg.clone(),
            kind: "model_not_found".into(),
            param: None,
            code: Some("model_not_found".into()),
        },
        BridgeError::AllAccountsFailed(msg) => ErrorBody {
            message: msg.clone(),
            kind: "all_accounts_failed".into(),
            param: None,
            code: None,
        },
    };
    (status, body)
}

/// 渲染上游错误为 OpenAI 兼容格式
///
/// 参考 aisix 的 `render_openai_envelope` 设计。
fn render_upstream_error(
    view: Option<&UpstreamErrorView>,
    wire: UpstreamWire,
    fallback_message: &str,
) -> ErrorBody {
    let Some(view) = view else {
        return ErrorBody {
            message: fallback_message.to_string(),
            kind: UPSTREAM_ERROR_TYPE.to_string(),
            param: None,
            code: None,
        };
    };

    let message = view
        .message
        .clone()
        .unwrap_or_else(|| fallback_message.to_string());

    let upstream_kind = view.kind.as_deref();
    let derived_code = match wire {
        UpstreamWire::OpenAI | UpstreamWire::Unknown => view.code.clone(),
        UpstreamWire::AzureOpenAI => derive_azure_code(upstream_kind)
            .or_else(|| view.code.clone()),
        UpstreamWire::Anthropic => derive_anthropic_code(upstream_kind),
        UpstreamWire::Bedrock => derive_bedrock_code(upstream_kind),
        UpstreamWire::Vertex => derive_vertex_code(upstream_kind),
    };

    ErrorBody {
        message,
        kind: UPSTREAM_ERROR_TYPE.to_string(),
        param: view.param.clone(),
        code: derived_code,
    }
}

/// Anthropic `error.type` → OpenAI 字符串 `code`
fn derive_anthropic_code(kind: Option<&str>) -> Option<String> {
    match kind? {
        "authentication_error" => Some("invalid_api_key".into()),
        "permission_error" => Some("permission_denied".into()),
        "not_found_error" => Some("model_not_found".into()),
        "request_too_large" => Some("request_too_large".into()),
        "rate_limit_error" => Some("rate_limit_exceeded".into()),
        "overloaded_error" => Some("overloaded".into()),
        _ => None,
    }
}

/// AWS Bedrock 异常码 → OpenAI 字符串 `code`
fn derive_bedrock_code(kind: Option<&str>) -> Option<String> {
    match kind? {
        "ThrottlingException" => Some("rate_limit_exceeded".into()),
        "ServiceQuotaExceededException" => Some("insufficient_quota".into()),
        "AccessDeniedException" => Some("permission_denied".into()),
        "ResourceNotFoundException" => Some("model_not_found".into()),
        "ModelNotReadyException" => Some("model_not_ready".into()),
        "ModelTimeoutException" => Some("timeout".into()),
        "ModelStreamErrorException" => Some("stream_error".into()),
        "ModelErrorException" => Some("model_error".into()),
        "ServiceUnavailableException" => Some("overloaded".into()),
        _ => None,
    }
}

/// Vertex AI gRPC 状态码 → OpenAI 字符串 `code`
fn derive_vertex_code(kind: Option<&str>) -> Option<String> {
    match kind? {
        "RESOURCE_EXHAUSTED" => Some("rate_limit_exceeded".into()),
        "PERMISSION_DENIED" => Some("permission_denied".into()),
        "UNAUTHENTICATED" => Some("invalid_api_key".into()),
        "NOT_FOUND" => Some("model_not_found".into()),
        "UNAVAILABLE" => Some("overloaded".into()),
        "DEADLINE_EXCEEDED" => Some("timeout".into()),
        _ => None,
    }
}

/// Azure OpenAI 错误码 → OpenAI 字符串 `code`
fn derive_azure_code(kind: Option<&str>) -> Option<String> {
    match kind? {
        "DeploymentNotFound" => Some("model_not_found".into()),
        "ResponsibleAIPolicyViolation" | "content_filter" => {
            Some("content_policy_violation".into())
        }
        "invalid_encrypted_content" => Some("invalid_encrypted_content".into()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(kind: &str) -> UpstreamErrorView {
        UpstreamErrorView {
            kind: Some(kind.into()),
            message: Some("upstream said hi".into()),
            code: None,
            param: None,
        }
    }

    #[test]
    fn anthropic_rate_limit_derives_rate_limit_exceeded() {
        let v = view("rate_limit_error");
        let body = render_upstream_error(Some(&v), UpstreamWire::Anthropic, "fb");
        assert_eq!(body.kind, "upstream_error");
        assert_eq!(body.code.as_deref(), Some("rate_limit_exceeded"));
        assert_eq!(body.message, "upstream said hi");
    }

    #[test]
    fn anthropic_overloaded_derives_overloaded() {
        let v = view("overloaded_error");
        let body = render_upstream_error(Some(&v), UpstreamWire::Anthropic, "fb");
        assert_eq!(body.kind, "upstream_error");
        assert_eq!(body.code.as_deref(), Some("overloaded"));
    }

    #[test]
    fn anthropic_authentication_derives_invalid_api_key() {
        let v = view("authentication_error");
        let body = render_upstream_error(Some(&v), UpstreamWire::Anthropic, "fb");
        assert_eq!(body.kind, "upstream_error");
        assert_eq!(body.code.as_deref(), Some("invalid_api_key"));
    }

    #[test]
    fn bedrock_throttling_derives_rate_limit_exceeded() {
        let v = view("ThrottlingException");
        let body = render_upstream_error(Some(&v), UpstreamWire::Bedrock, "fb");
        assert_eq!(body.kind, "upstream_error");
        assert_eq!(body.code.as_deref(), Some("rate_limit_exceeded"));
    }

    #[test]
    fn bedrock_service_quota_exceeded_derives_insufficient_quota() {
        let v = view("ServiceQuotaExceededException");
        let body = render_upstream_error(Some(&v), UpstreamWire::Bedrock, "fb");
        assert_eq!(body.kind, "upstream_error");
        assert_eq!(body.code.as_deref(), Some("insufficient_quota"));
    }

    #[test]
    fn vertex_resource_exhausted_derives_rate_limit_exceeded() {
        let v = view("RESOURCE_EXHAUSTED");
        let body = render_upstream_error(Some(&v), UpstreamWire::Vertex, "fb");
        assert_eq!(body.kind, "upstream_error");
        assert_eq!(body.code.as_deref(), Some("rate_limit_exceeded"));
    }

    #[test]
    fn vertex_unauthenticated_derives_invalid_api_key() {
        let v = view("UNAUTHENTICATED");
        let body = render_upstream_error(Some(&v), UpstreamWire::Vertex, "fb");
        assert_eq!(body.kind, "upstream_error");
        assert_eq!(body.code.as_deref(), Some("invalid_api_key"));
    }

    #[test]
    fn azure_deployment_not_found_derives_model_not_found() {
        let v = view("DeploymentNotFound");
        let body = render_upstream_error(Some(&v), UpstreamWire::AzureOpenAI, "fb");
        assert_eq!(body.kind, "upstream_error");
        assert_eq!(body.code.as_deref(), Some("model_not_found"));
    }

    #[test]
    fn openai_same_wire_preserves_upstream_code_and_param() {
        let v = UpstreamErrorView {
            kind: Some("rate_limit_exceeded".into()),
            message: Some("hi".into()),
            code: Some("custom_code".into()),
            param: Some("model".into()),
        };
        let body = render_upstream_error(Some(&v), UpstreamWire::OpenAI, "fb");
        assert_eq!(body.kind, "upstream_error");
        assert_eq!(body.code.as_deref(), Some("custom_code"));
        assert_eq!(body.param.as_deref(), Some("model"));
    }

    #[test]
    fn missing_view_uses_fallback_message() {
        let body = render_upstream_error(None, UpstreamWire::Anthropic, "raw text");
        assert_eq!(body.kind, "upstream_error");
        assert_eq!(body.message, "raw text");
        assert!(body.code.is_none());
    }

    #[test]
    fn timeout_maps_correctly() {
        let error = BridgeError::Timeout {
            elapsed_ms: 30_000,
            cause: "connection timed out".into(),
        };
        let (status, body) = render_openai_error(&error);
        assert_eq!(status, 504);
        assert_eq!(body.kind, "timeout");
        assert_eq!(body.code.as_deref(), Some("timeout"));
    }

    #[test]
    fn auth_error_maps_to_401() {
        let error = BridgeError::AuthError("invalid key".into());
        let (status, body) = render_openai_error(&error);
        assert_eq!(status, 401);
        assert_eq!(body.kind, "authentication_error");
        assert_eq!(body.code.as_deref(), Some("invalid_api_key"));
    }

    #[test]
    fn rate_limited_maps_to_429() {
        let error = BridgeError::RateLimited;
        let (status, body) = render_openai_error(&error);
        assert_eq!(status, 429);
        assert_eq!(body.kind, "rate_limit_error");
        assert_eq!(body.code.as_deref(), Some("rate_limit_exceeded"));
    }

    #[test]
    fn model_not_found_maps_to_404() {
        let error = BridgeError::ModelNotFound("gpt-5".into());
        let (status, body) = render_openai_error(&error);
        assert_eq!(status, 404);
        assert_eq!(body.kind, "model_not_found");
        assert_eq!(body.code.as_deref(), Some("model_not_found"));
    }
}