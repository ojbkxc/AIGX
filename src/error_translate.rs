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
        UpstreamWire::AzureOpenAI => derive_azure_code(upstream_kind).or_else(|| view.code.clone()),
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

/// 发往客户端的错误消息截断上限（**字节**，回退到 UTF-8 字符边界）。
const SANITIZE_MAX_CHARS: usize = 500;

/// `sk-` 的**前边界**判定：`s` 之前必须是词边界——`pos == 0`，或前一字节不是
/// 词字符（ASCII 字母数字 / `-` / `_`）。
///
/// 缺此检查时 `sk-` 的匹配从任意字节位置起，`risk-score`、`desk-top`、
/// `task-42` 这类普通连字符词里的 `sk-` 片段都会被误吞成 `***`（#45 三例同形）。
fn is_sk_front_boundary(bytes: &[u8], pos: usize) -> bool {
    pos == 0 || !is_sk_key_byte(bytes[pos - 1])
}

/// `sk-` 密钥段字符类：ASCII 字母数字、`-`、`_`。前边界判定复用同一集合。
///
/// 现代密钥多为多段格式（OpenAI `sk-proj-…`、Anthropic `sk-ant-api03-…`）。
/// 旧的「只吞字母数字」会在第一个 `-`/`_` 就停，产出 `sk-***-<真密钥>`——看着
/// 已脱敏、真秘密却完整跟在后面（#45 的假绿形态）。
fn is_sk_key_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'-' || b == b'_'
}

/// T3：脱敏发往客户端的上游错误消息。
///
/// 上游报错体可能回显其收到的认证凭据（如 "Invalid api key sk-xxx"、
/// "Bearer xxx"），原样透传给付费客户端会泄露渠道侧密钥。本函数只拦
/// "发给用户"的错误出口；日志/熔断侧保留原始消息以利排查。
///
/// 处理（参照 rust-tunnel `sanitize_error_message` 的手动字节匹配实现，
/// 不引入 regex 依赖）：
/// 1. 截断到最多 500 **字节**（UTF-8 字符边界安全，手动回退到合法边界，
///    不依赖尚未在 MSRV 稳定的 `floor_char_boundary`；超限时追加 `...`）
/// 2. 脱敏两类模式（best-effort，**非安全保证**）：
///    - `Bearer <token>`：前缀大小写不敏感、**必须带尾随 ASCII 空格**，
///      token 吞到下一个 ASCII 空白 → 统一写回 `Bearer ***`
///    - `sk-<密钥段>`：要求**前边界**（`sk-` 前一字节不是 `[A-Za-z0-9_-]`）
///      且 `sk-` 后首字符为 ASCII 字母数字，随后吞满连续的 `[A-Za-z0-9_-]`
///      （覆盖 `sk-proj-…`/`sk-ant-api03-…` 多段密钥）→ `sk-***`
/// 3. 其余逐 UTF-8 字符原样拷贝，状态码与诊断文案一律保留。
///
/// **已知边界（刻意不拦，登记于此，勿当漏修）**：裸 JWT（`eyJ…`）、11 位
/// 手机号、JSON `"access_token":"…"`、`Bearer` 后无空格或跟 `\t`、`Cookie:`
/// 头、URL 内嵌凭据（`https://user:pass@host`）、`sk-` 后首字符非字母数字、
/// 无 `sk-` 前缀的高熵串（Azure 32-hex / Google `AIza…`）。判据见
/// `_a2a-port-recon/55-sanitize-scope-analysis.md` §2.2：四台生产 454 条非空
/// 错误值 + 1488 行原始日志里这些形态出现率为 0，扩它们会给数据面 5 条
/// 客户端可见出口引入新掩码形状与误伤面。
pub fn sanitize_error_message(body: &str) -> String {
    // ── 截断：最多 500 字节，仅切在 UTF-8 边界上 ──
    let end = if body.len() <= SANITIZE_MAX_CHARS {
        body.len()
    } else {
        // 找 ≤500 字节处的最后一个完整字符边界
        let mut boundary = SANITIZE_MAX_CHARS;
        while boundary > 0 && !body.is_char_boundary(boundary) {
            boundary -= 1;
        }
        boundary
    };
    let truncated = if end < body.len() {
        format!("{}...", &body[..end])
    } else {
        body.to_string()
    };

    // ── 脱敏：逐字节扫描手动匹配，避免引入 regex ──
    let bytes = truncated.as_bytes();
    let n = bytes.len();
    let mut out = String::with_capacity(n);
    let mut pos = 0;

    while pos < n {
        // `Bearer ` 前缀（大小写不敏感，字节比较——多字节内容上字符串
        // 切片 remaining[..7] 可能落在 UTF-8 字符内部而 panic）
        let remaining = &bytes[pos..];
        if remaining.len() > 7 && remaining[..7].eq_ignore_ascii_case(b"bearer ") {
            pos += 7; // 跳过前缀
            while pos < n && !bytes[pos].is_ascii_whitespace() {
                pos += 1; // 吞掉 token 本体
            }
            out.push_str("Bearer ***");
            continue;
        }

        // `sk-` 后跟密钥段 → `sk-***`（#45：前边界锚 + 吞满多段）。
        // 三个条件缺一不可，既防误伤又防假绿：
        //   1. 前边界：`s` 前一个字节是词边界（`risk-score`/`desk-top`/`task-42` 不被吞）；
        //   2. `sk-` 后首字符必须是 ASCII 字母数字（`sk- ` / `sk--` 不替换）；
        //   3. 吞满其后连续的 `[A-Za-z0-9_-]`，不设长度上限——输入已截断到 ≤503 字节，
        //      整段吞掉是唯一不泄尾段的选择（旧实现 65 上限 + 只吞 alnum ⇒ `sk-proj-…` 泄尾）。
        if pos + 3 < n
            && bytes[pos] == b's'
            && bytes[pos + 1] == b'k'
            && bytes[pos + 2] == b'-'
            && is_sk_front_boundary(bytes, pos)
            && bytes[pos + 3].is_ascii_alphanumeric()
        {
            let mut key_end = pos + 3;
            while key_end < n && is_sk_key_byte(bytes[key_end]) {
                key_end += 1;
            }
            out.push_str("sk-***");
            pos = key_end;
            continue;
        }

        // 普通字符原样输出（按 UTF-8 字符步进，多字节安全）
        let ch = truncated[pos..].chars().next().unwrap_or_default();
        out.push(ch);
        pos += ch.len_utf8();
    }

    out
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

    /// T3：超限消息截断到 500 字符并追加省略号。
    #[test]
    fn sanitize_truncates_over_limit() {
        let long = "a".repeat(600);
        let sanitized = sanitize_error_message(&long);
        assert_eq!(sanitized.len(), 503); // 500 字符 + "..."
        assert!(sanitized.ends_with("..."));
    }

    /// 恰好 500 字符的消息原样通过（不截断）。
    #[test]
    fn sanitize_keeps_exact_limit_intact() {
        let exact = "a".repeat(500);
        assert_eq!(sanitize_error_message(&exact), exact);
    }

    /// 截断边界落在多字节 UTF-8 字符内部时安全回退到字符边界。
    #[test]
    fn sanitize_truncation_respects_utf8_boundary() {
        // 300 个中文字符 = 900 字节；500 字节处切在字符中间 → 回退到 498
        let long = "密".repeat(300);
        let sanitized = sanitize_error_message(&long);
        assert!(sanitized.len() <= 503);
        assert!(sanitized.chars().all(|c| c == '密' || c == '.'));
    }

    /// `Bearer <token>` 前缀大小写不敏感脱敏，token 本体吞到空白符。
    #[test]
    fn sanitize_redacts_bearer_case_insensitive() {
        assert_eq!(
            sanitize_error_message("unauthorized: bearer tok123 stuff"),
            "unauthorized: Bearer *** stuff"
        );
        assert_eq!(
            sanitize_error_message("BEARER abc.def-ghi end"),
            "Bearer *** end"
        );
    }

    /// `sk-` 后跟字母数字的密钥脱敏；`sk-` 后无字母数字时不误替换。
    #[test]
    fn sanitize_redacts_sk_key() {
        assert_eq!(
            sanitize_error_message("Invalid api key sk-abc123XYZ"),
            "Invalid api key sk-***"
        );
        // "desk-" 中的 "sk-" 后是空格（非字母数字）→ 原样保留
        assert_eq!(
            sanitize_error_message("desk- top corner"),
            "desk- top corner"
        );
    }

    /// #45：`sk-` 必须有**前边界锚**——词中的 `sk-` 片段不得被吞。
    ///
    /// 前三例（risk-score / desk-top / task-42）是旧实现的真实误伤（旧实现把
    /// `sk-` 从词中吞起，产出 `risk-***` 等）；`disks-4` 本身**不含** `sk-` 子串
    /// （d-i-s-k-**s**-4），旧实现亦不误伤，留作「不含 `sk-` 子串不得被改动」的反向 guard
    /// （注：`_a2a-port-recon/55-sanitize-scope-analysis.md` §2.2-C 把 `disks-4` 列为误伤，与
    /// 其自带的复刻矩阵结论矛盾——matrix.txt 那条自报「未误伤」，以实跑为准）。
    #[test]
    fn sanitize_sk_requires_word_boundary() {
        // 四例误伤：`sk-` 前一字节是字母 ⇒ 非词边界 ⇒ 逐字符原样保留。
        assert_eq!(
            sanitize_error_message("risk-score is 0.5"),
            "risk-score is 0.5"
        );
        assert_eq!(sanitize_error_message("desk-top corner"), "desk-top corner");
        assert_eq!(
            sanitize_error_message("task-42 was aborted"),
            "task-42 was aborted"
        );
        assert_eq!(
            sanitize_error_message("disks-4 were removed"),
            "disks-4 were removed"
        );
        // 正例：词边界之后的 `sk-<key>` 仍必须被吞（串首 / 空格 / 分隔符都算边界）。
        assert_eq!(
            sanitize_error_message("sk-abc123 rejected"),
            "sk-*** rejected"
        );
        assert_eq!(sanitize_error_message("key=sk-abc123"), "key=sk-***");
        assert_eq!(
            sanitize_error_message("Invalid api key sk-abc123XYZ"),
            "Invalid api key sk-***"
        );
    }

    /// #45：`sk-` 吞满多段密钥（含 `-`/`_`、不限 65 字符）——旧实现产出
    /// `sk-***-<真密钥>` 的假绿，真秘密完整跟在后面。
    #[test]
    fn sanitize_sk_swallows_multisegment_and_long_keys() {
        // OpenAI 新格式：旧实现只吞到第一个 `-`，留下 `-<SECRET>` 明文。
        assert_eq!(
            sanitize_error_message("invalid key sk-proj-AKIAREALSECRETVALUE_abcdef end"),
            "invalid key sk-*** end"
        );
        // Anthropic 格式（多段 `-`）。
        assert_eq!(
            sanitize_error_message("x-api-key rejected sk-ant-api03-SECRETVALUE end"),
            "x-api-key rejected sk-*** end"
        );
        // 下划线连接的密钥段。
        assert_eq!(
            sanitize_error_message("prefix sk-AKIAREAL_restoftoken"),
            "prefix sk-***"
        );
        // 超 65 字符的长密钥必须整段吞掉（旧实现会泄尾段）。
        let long = format!("key sk-{}", "E".repeat(200));
        assert_eq!(sanitize_error_message(&long), "key sk-***");
        // `sk-` 后首字符非字母数字（空格）⇒ 不替换。
        assert_eq!(sanitize_error_message("sk- top"), "sk- top");
    }

    /// #45：脱敏**幂等**——掩码产物再过一次不变，这是「落库 + 出口双重
    /// sanitize 是安全的」这一前提。
    #[test]
    fn sanitize_is_idempotent() {
        for raw in [
            "unauthorized Bearer tok123 end",
            "invalid key sk-proj-SECRETVALUE end",
            "risk-score is 0.5",
            "上游返回 HTTP 500：服务暂时不可用",
        ] {
            let once = sanitize_error_message(raw);
            assert_eq!(
                sanitize_error_message(&once),
                once,
                "二次脱敏改变了产物: {once}"
            );
        }
    }

    /// #45 `known_boundary`：裸 JWT（`eyJ…`）**刻意不拦**——当前行为是逐字符原样保留。
    ///
    /// 为何不扩：见 `_a2a-port-recon/55-sanitize-scope-analysis.md` §5（四台生产 454 条
    /// 非空错误值 + 1488 行原始日志出现率 0；扩它会把所有 8+ 字符 base64url 串纳入吞并面）。
    /// 加这条 pin 的目的：将来谁改了行为，这里**立刻变红**，不让「刻意不做」静默漂移。
    #[test]
    fn sanitize_known_boundary_naked_jwt_is_not_masked() {
        let raw = "invalid token eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.SIGNATUREPART";
        assert_eq!(sanitize_error_message(raw), raw);
    }

    /// #45 `known_boundary`：11 位手机号**刻意不拦**——当前行为是逐字符原样保留。
    ///
    /// 为何不扩：见 55 §5（出现率 0；手机号正则会吃订单号 / 时间戳）。
    #[test]
    fn sanitize_known_boundary_phone_number_is_not_masked() {
        let raw = "login failed for 13812345678";
        assert_eq!(sanitize_error_message(raw), raw);
    }

    /// #45 `known_boundary`：JSON `"access_token":"…"` 字段**刻意不拦**——当前行为是逐字符原样保留。
    ///
    /// 为何不扩：见 55 §5（出现率 0；该键名在真实文案里只见于「缺少 access_token」这类无值场景）。
    #[test]
    fn sanitize_known_boundary_access_token_field_is_not_masked() {
        let raw = r#"{"error":{"access_token":"eyJhbGciOiJIUzI1NiJ9.SIG"}}"#;
        assert_eq!(sanitize_error_message(raw), raw);
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
