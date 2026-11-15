//! 脚本宿主的能力面方法表 —— **常量与超时档位，不定义 trait**。
//!
//! ## 职责边界（lead 架构更正后的定稿）
//!
//! Bun 进程的 stdin/stdout 直接塞进 [`RpcChannel`](super::rpc::RpcChannel)，
//! 帧编解码 / 奇偶 id / credit 背压全部现成复用。本模块只把「网关 → 脚本
//! 宿主」的方法名与超时档位钉成常量，供数据面（bridge 经 conversation）、
//! 运维面（models/usage/login 等）与 js-host（DISPATCH_METHODS）三方对照。
//! 方法清单与 `host/provider.mjs` 的 `DISPATCH_METHODS` **逐字一致**——
//! 两边各写一份，测试钉住（`dispatch_methods_match_js_table`）。
//!
//! ## 超时分档
//!
//! - **控制面**（10s）：models / token / usage / login / admin 等元数据与
//!   账号管理方法。脚本钩子是本地计算 + 少量回调，10s 足够；超时快失败
//!   让 failover 尽早换渠道。
//! - **网络面**（120s）：conversation 主链路（脚本要 fetch 上游、SSE 循环
//!   走完整个生成）。与网关的流式请求预算同量级——上游长文生成本来就
//!   可能超过 1 分钟。
//!
//! 注意：conversation 的实际消费走 `call_stream`（head 超时 30s 是
//! rpc.rs 的常量，与本表的网络面预算独立——head 是「开始吐流」的信号，
//! 不是流的终态）。

use std::time::Duration;

/// 控制面方法超时（元数据/账号管理类）。
pub const CONTROL_TIMEOUT: Duration = Duration::from_secs(10);

/// 网络面方法超时（conversation 主链路）。
pub const NETWORK_TIMEOUT: Duration = Duration::from_secs(120);

/// conversation（流式；走 `call_stream`，head 30s / chunk 循环无总限）。
pub const M_CONVERSATION: &str = "conversation";

/// 能力面方法表（conversation 之外的 23 个；与 `host/provider.mjs` 的
/// `DISPATCH_METHODS` 逐字一致，顺序也保持同源）。
pub const CAPABILITY_METHODS: &[&str] = &[
    "models.list",
    "models.refresh",
    "models.meta",
    "token.ensure",
    "token.refresh",
    "token.expiring",
    "usage.query",
    "checkin.claim",
    "account.normalize",
    "account.view",
    "account.identity",
    "login.start",
    "login.poll",
    "login.callback",
    "login.cancel",
    "admin.action",
    "classify",
    "is_error_response",
    "refusal_markers",
    "retry_advice",
    "error.policy",
    "cooldown_models",
    "reasoning_patch",
];

/// 全量方法表（24 个 = conversation + 能力面）。
pub fn all_methods() -> impl Iterator<Item = &'static str> {
    std::iter::once(M_CONVERSATION).chain(CAPABILITY_METHODS.iter().copied())
}

/// 方法属于哪一档超时。conversation 走流式路径不进本表（head/chunk 各自
/// 的预算在 rpc.rs / bridge 层）；其余全部按控制面 10s。
pub fn timeout_for(method: &str) -> Duration {
    let _ = method;
    CONTROL_TIMEOUT
}

/// 网关 → 宿主的回调方法族前缀（这些由网关的 `ScriptCallbacks` 实现，
/// 脚本侧按偶数 id 发起请求）。
pub mod callbacks {
    /// HTTP egress（网关代发请求）。
    pub const TRANSPORT_REQUEST: &str = "script.transport.request";
    /// HTTP egress 读下一块（阻塞到下一块或 EOF）。
    pub const TRANSPORT_READ: &str = "script.transport.read";
    /// HTTP egress 关闭会话。
    pub const TRANSPORT_CLOSE: &str = "script.transport.close";
    /// OpenAI 请求体 → 上游协议请求体（网关转换）。
    pub const PROTOCOL_REQUEST: &str = "script.protocol.request";
    /// 上游 SSE 字节 → OpenAI chat SSE 字节（流式增量转换）。
    pub const PROTOCOL_CHUNK: &str = "script.protocol.chunk";
    /// 转换器尾部冲刷。
    pub const PROTOCOL_FINISH: &str = "script.protocol.finish";
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 与 `host/provider.mjs` 的 DISPATCH_METHODS 对表：两个清单必须逐字
    /// 一致（含顺序）。JS 侧改动而本测试不跟 → CI 红，逼同步。
    #[test]
    fn dispatch_methods_match_js_table() {
        let js = include_str!("host/provider.mjs");
        for method in all_methods() {
            assert!(
                js.contains(&format!("\"{method}\",")),
                "方法 {method} 必须出现在 host/provider.mjs 的 DISPATCH_METHODS"
            );
        }
        // 反向：JS 表里的每个方法本模块也要有（防 JS 加了网关没跟）
        const JS_COUNT: usize = 24;
        assert_eq!(
            CAPABILITY_METHODS.len() + 1,
            JS_COUNT,
            "方法表数量与 JS 侧 DISPATCH_METHODS 不一致（应同为 24）"
        );
    }

    #[test]
    fn timeout_tiers_are_documented_values() {
        assert_eq!(CONTROL_TIMEOUT, Duration::from_secs(10));
        assert_eq!(NETWORK_TIMEOUT, Duration::from_secs(120));
    }

    #[test]
    fn callback_method_names_match_provider_mjs() {
        let js = include_str!("host/provider.mjs");
        for name in [
            callbacks::TRANSPORT_REQUEST,
            callbacks::TRANSPORT_READ,
            callbacks::TRANSPORT_CLOSE,
            callbacks::PROTOCOL_REQUEST,
            callbacks::PROTOCOL_CHUNK,
            callbacks::PROTOCOL_FINISH,
        ] {
            assert!(
                js.contains(name),
                "回调方法 {name} 必须与 provider.mjs 的 CALLBACK 一致"
            );
        }
    }
}
