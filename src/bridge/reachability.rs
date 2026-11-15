//! 连接阶段失败分类 —— 只回答「这次 Transport 错误是不是 DNS 解析 / 连接建立失败」。
//!
//! 用途：渠道的「备用入口」（[`crate::channel::Channel::backup_base_urls`]）
//! 只在**主入口这一跳根本没连上**时按序改试备用地址；上游已受理请求后的
//! 传输错误（流中断、读超时、响应体损坏）换入口只会造成重复计费/重复执行，
//! 必须放行走正常 failover/报错路径。
//!
//! `BridgeError::Transport` 的文本来自 reqwest 的 `Display`（各桥统一
//! `.map_err(|e| BridgeError::Transport(e.to_string()))`），这里按连接阶段的
//! 典型子串判定：
//! - `error sending request ...: error trying to connect: dns error: failed to
//!   lookup address information` —— DNS 解析失败；
//! - `error trying to connect: tcp connect error: Connection refused` / reset /
//!   aborted / no route to host / network is unreachable —— 连接建立失败。
//!
//! 刻意**不含**单纯的超时（`operation timed out` / `timed out`）：非流式请求的
//! 总超时可能发生在「请求已发出、上游正在推理」阶段，换备用入口会重复触发一次
//! 计费任务。带 `error trying to connect` 前缀的连接超时属于连接阶段，仍算
//! 连接失败。

use super::BridgeError;

/// 该错误是否属于「DNS 解析失败 / 连接建立失败」。
///
/// 仅识别 `BridgeError::Transport`；`Timeout` / `UpstreamStatus` / `StreamAborted`
/// 等一律 false（它们要么发生在请求受理之后，要么语义上不是连接失败）。
pub fn is_connect_failure(error: &BridgeError) -> bool {
    let BridgeError::Transport(message) = error else {
        return false;
    };
    let lower = message.to_ascii_lowercase();
    let connect_phase = lower.contains("error trying to connect")
        || lower.contains("tcp connect")
        || lower.contains("connect error");
    if connect_phase {
        // 连接阶段的任何失败（含 connect timeout）都可以换入口。
        return true;
    }
    [
        "dns error",
        "failed to lookup address",
        "lookup address information",
        "connection refused",
        "connection reset",
        "connection aborted",
        "network is unreachable",
        "no route to host",
        // hyper/reqwest 在建立连接前后常见的底层错误
        "connection closed before message completed",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transport(message: &str) -> BridgeError {
        BridgeError::Transport(message.to_string())
    }

    #[test]
    fn dns_resolution_failure_detected() {
        // reqwest 在 DNS 失败时的真实文案（Linux/glibc 与 Windows 子串不同，两个都认）
        assert!(is_connect_failure(&transport(
            "error sending request for url (https://api.example.com/v1/chat/completions): \
             error trying to connect: dns error: failed to lookup address information: \
             Name or service not known"
        )));
        assert!(is_connect_failure(&transport(
            "error trying to connect: dns error: No such host is known. (os error 11001)"
        )));
    }

    #[test]
    fn connection_failures_detected() {
        assert!(is_connect_failure(&transport(
            "error sending request: error trying to connect: tcp connect error: \
             Connection refused (os error 111)"
        )));
        assert!(is_connect_failure(&transport("connection reset by peer")));
        assert!(is_connect_failure(&transport("network is unreachable")));
        assert!(is_connect_failure(&transport(
            "connect error: no route to host"
        )));
        // 连接阶段的超时：仍算连接失败（请求没发出去，换入口不重复计费）
        assert!(is_connect_failure(&transport(
            "error trying to connect: connection timed out"
        )));
    }

    #[test]
    fn post_connect_errors_are_not_connect_failures() {
        // 请求已发出/已受理后的错误：不得换入口
        assert!(!is_connect_failure(&transport(
            "operation timed out while reading response body"
        )));
        assert!(!is_connect_failure(&transport(
            "error decoding response body: unexpected end of file"
        )));
        assert!(!is_connect_failure(&BridgeError::StreamAborted));
        assert!(!is_connect_failure(&BridgeError::Timeout {
            elapsed_ms: 30_000,
            cause: "首个 SSE 事件未在等待上限内到达".into(),
        }));
        assert!(!is_connect_failure(&BridgeError::upstream_status(
            502,
            "bad gateway"
        )));
    }
}
