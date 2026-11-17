//! 脚本宿主的能力面方法表 —— **常量与超时档位，不定义 trait**。
//!
//! ## 职责边界（lead 架构更正后的定稿）
//!
//! Bun 进程的 stdin/stdout 直接塞进 [`RpcChannel`](super::rpc::RpcChannel)，
//! 帧编解码 / 奇偶 id / credit 背压全部现成复用。本模块只把「网关 → 脚本
//! 宿主」的方法名与超时档位钉成常量，供数据面（bridge 经 conversation）、
//! 运维面（models/usage/login 等）与 js-host（`host/provider.mjs` 的
//! `dispatch()` switch）三方对照。方法清单与 JS 侧**逐字一致**——两边各写
//! 一份，测试钉住（`dispatch_methods_match_js_table`）。
//!
//! ## 方法面口径
//!
//! 全量 25 个 = `provider.info` + `conversation` + 23 个能力方法。
//! `provider.info` 是 A2A 的元数据查询（`metadata(provider)`），不在能力面
//! 的 23 个里，但同样走控制面超时。
//!
//! ## 超时分档
//!
//! - **控制面**（10s）：models / token / usage / login / admin / provider.info
//!   等元数据与账号管理方法。脚本钩子是本地计算 + 少量回调，10s 足够；
//!   超时快失败让 failover 尽早换渠道。
//! - **网络面**（120s）：conversation 主链路（脚本要 fetch 上游、SSE 循环
//!   走完整个生成）。与网关的流式请求预算同量级——上游长文生成本来就
//!   可能超过 1 分钟。
//!
//! 注意：conversation 的实际消费走 `call_stream`（head 超时 30s 是
//! `plugin_host::rpc` 的常量，与本表的网络面预算独立——head 是「开始吐流」
//! 的信号，不是流的终态）。
//!
//! ## `login.interaction` / `login.submit` 不在本表（A2A 同口径）
//!
//! A2A v3.5.0-beta.1 的 `rpc.rs::admin_action` 里只有 wasm 宿主会转发这两个
//! 方法（`host.options.wasm.is_some() && matches!(action, "login.interaction" |
//! "login.submit")`）；**脚本（Bun）宿主的 `dispatch()` 不实现它们**，调用会
//! 落到 `default: throw unsupported()`。AIGX 照抄这一分层：脚本包自己实现
//! 登录交互（`hooks.auth` 的 authorize 里做），宿主不额外转发。管理面读交互
//! 步骤时脚本家应按「无交互」处理（见 `account_sessions::plugin_login_interaction`
//! 的降级分支）。

use std::time::Duration;

/// 控制面方法超时（元数据/账号管理类）。
pub const CONTROL_TIMEOUT: Duration = Duration::from_secs(10);

/// 网络面方法超时（conversation 主链路）。
pub const NETWORK_TIMEOUT: Duration = Duration::from_secs(120);

/// conversation（流式；走 `call_stream`，head 30s / chunk 循环无总限）。
pub const M_CONVERSATION: &str = "conversation";

/// provider 元数据查询（`metadata(provider)`；控制面）。
pub const M_PROVIDER_INFO: &str = "provider.info";

/// 能力面方法表（`provider.info` / `conversation` 之外的 23 个；与
/// `host/provider.mjs` 的 `dispatch()` switch 逐字一致，顺序也保持同源）。
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

/// 全量方法表（25 个 = provider.info + conversation + 能力面）。
pub fn all_methods() -> impl Iterator<Item = &'static str> {
    std::iter::once(M_PROVIDER_INFO)
        .chain(std::iter::once(M_CONVERSATION))
        .chain(CAPABILITY_METHODS.iter().copied())
}

/// 方法属于哪一档超时。conversation 走流式路径不进本表（head/chunk 各自
/// 的预算在 `plugin_host::rpc` / bridge 层）；其余全部按控制面 10s。
pub fn timeout_for(method: &str) -> Duration {
    let _ = method;
    CONTROL_TIMEOUT
}

/// 网关 → 宿主的**管理面**方法（不在能力面 dispatch 表里）。
///
/// `host/main.mjs` 的 `handle()` 在 `dispatch(item, …)` 之前用
/// `frame.method === "<name>"` 分支处理这两个方法（与 provider 能力方法
/// 的 `switch case` 分开）——网关侧在这里钉常量，测试与宿主 JS 逐字对表。
pub mod admin {
    /// 宿主包清单（`loader.mjs::packageList()`）：
    /// `[{id,providers,aliases,icon,providerIcons,error,settings}]`。
    /// rebuild 成功后由网关拉取并回填 `settings_schema` / provider 别名等。
    pub const PACKAGES: &str = "script.packages";
    /// 应用 `providerOptions` 并回最新设置声明（`environment.mjs::updateSettings`）。
    /// `{package, providerOptions}` → `{id, optionsExample, providerOptions}`。
    pub const SETTINGS: &str = "script.settings";
}

/// 网关 → 宿主的回调方法族（这些由网关的 `ScriptCallbacks` 实现，脚本侧
/// 按偶数 id 发起请求）。清单来自 `host/*.mjs` 里全部 `rpc("<name>")` 调用。
pub mod callbacks {
    /// HTTP egress：开一个请求会话（返回 `{id,status,headers,url}`）。
    pub const HTTP_OPEN: &str = "script.http.open";
    /// HTTP egress：读下一块（阻塞到下一块或 EOF，`{data(b64),done}`）。
    pub const HTTP_READ: &str = "script.http.read";
    /// HTTP egress：关闭会话。
    pub const HTTP_CLOSE: &str = "script.http.close";
    /// WebSocket egress：建立连接。
    pub const WS_OPEN: &str = "script.ws.open";
    /// WebSocket egress：读一条消息。
    pub const WS_READ: &str = "script.ws.read";
    /// WebSocket egress：发一条消息。
    pub const WS_SEND: &str = "script.ws.send";
    /// WebSocket egress：中止连接。
    pub const WS_ABORT: &str = "script.ws.abort";
    /// 上游 SSE → OpenAI SSE：开转换器（字符串 id）。
    pub const STREAM_OPEN: &str = "script.stream.open";
    /// 上游 SSE → OpenAI SSE：推一块字节。
    pub const STREAM_PUSH: &str = "script.stream.push";
    /// 上游 SSE → OpenAI SSE：收尾。
    pub const STREAM_CLOSE: &str = "script.stream.close";
    /// OpenAI 请求体 → 上游协议请求体（网关转换）。
    pub const PROTOCOL_REQUEST: &str = "script.protocol.request";
    /// 代理解析（`{source:"direct"}` / 池条目 / 静态 URL）。
    pub const PROXY_RESOLVE: &str = "script.proxy.resolve";
    /// 会话签名缓存查询（antigravity 等）。
    pub const SIGNATURE: &str = "script.signature";
    /// 用量上报（拼 `RequestLog` 入日志存储）。
    pub const USAGE: &str = "script.usage";

    /// ── 旧 AIGX transport / protocol 形状（向后兼容，仍由 callbacks 分发）──
    /// HTTP egress（旧形状；新脚本用 `script.http.*`）。
    pub const TRANSPORT_REQUEST: &str = "script.transport.request";
    /// HTTP egress 读下一块（旧形状）。
    pub const TRANSPORT_READ: &str = "script.transport.read";
    /// HTTP egress 关闭会话（旧形状）。
    pub const TRANSPORT_CLOSE: &str = "script.transport.close";
    /// 转换器推块（旧形状；新脚本用 `script.stream.push`）。
    pub const PROTOCOL_CHUNK: &str = "script.protocol.chunk";
    /// 转换器收尾（旧形状；新脚本用 `script.stream.close`）。
    pub const PROTOCOL_FINISH: &str = "script.protocol.finish";
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 宿主 JS 侧全量源码（`dispatch()` 在 provider.mjs，其余回调分散在
    /// 各模块；拼起来做包含断言）。
    const HOST_JS: &[&str] = &[
        include_str!("host/provider.mjs"),
        include_str!("host/fetch.mjs"),
        include_str!("host/websocket.mjs"),
        include_str!("host/environment.mjs"),
        include_str!("host/accounts.mjs"),
        include_str!("host/gemini.mjs"),
        include_str!("host/main.mjs"),
        include_str!("host/loader.mjs"),
    ];

    fn js_contains(needle: &str) -> bool {
        HOST_JS.iter().any(|js| js.contains(needle))
    }

    /// 与 `host/provider.mjs` 的 `dispatch()` switch 对表：两个清单必须逐字
    /// 一致。JS 侧改动而本测试不跟 → CI 红，逼同步。
    #[test]
    fn dispatch_methods_match_js_table() {
        let js = include_str!("host/provider.mjs");
        for method in all_methods() {
            assert!(
                js.contains(&format!("case \"{method}\"")),
                "方法 {method} 必须作为 case 出现在 host/provider.mjs 的 dispatch()"
            );
        }
        // 反向：JS switch 里的每个 `case "x.y"` 本模块也要有（防 JS 加了网关没跟）
        let cases: Vec<&str> = js
            .lines()
            .filter_map(|line| {
                let line = line.trim_start();
                let rest = line.strip_prefix("case \"")?;
                let end = rest.find('"')?;
                Some(&rest[..end])
            })
            .collect();
        for case in &cases {
            assert!(
                all_methods().any(|m| m == *case),
                "JS dispatch 有 case {case}，但 rpc.rs 方法表没有（网关会不认识）"
            );
        }
        assert_eq!(
            all_methods().count(),
            cases.len(),
            "方法表数量与 JS dispatch 的 case 数不一致"
        );
    }

    #[test]
    fn timeout_tiers_are_documented_values() {
        assert_eq!(CONTROL_TIMEOUT, Duration::from_secs(10));
        assert_eq!(NETWORK_TIMEOUT, Duration::from_secs(120));
    }

    /// 网关回调方法名必须真的被宿主 JS 调用（否则是死常量，或 JS 改了名
    /// 而网关还在实现旧名）。
    #[test]
    fn callback_method_names_match_host_mjs() {
        for name in [
            callbacks::HTTP_OPEN,
            callbacks::HTTP_READ,
            callbacks::HTTP_CLOSE,
            callbacks::WS_OPEN,
            callbacks::WS_READ,
            callbacks::WS_SEND,
            callbacks::WS_ABORT,
            callbacks::STREAM_OPEN,
            callbacks::STREAM_PUSH,
            callbacks::STREAM_CLOSE,
            callbacks::PROTOCOL_REQUEST,
            callbacks::PROXY_RESOLVE,
            callbacks::SIGNATURE,
            callbacks::USAGE,
        ] {
            assert!(
                js_contains(&format!("rpc(\"{name}\"")),
                "回调方法 {name} 必须被宿主 JS 以 rpc(\"{name}\", …) 调用"
            );
        }
    }

    /// 管理面方法（网关 → 宿主）：`main.mjs` 的 `handle()` 必须在
    /// `dispatch()` 之前显式分支处理；否则网关调用会落进 provider 能力
    /// 分派并报 unsupported。
    #[test]
    fn admin_methods_are_branched_in_host_main() {
        let js = include_str!("host/main.mjs");
        for method in [admin::PACKAGES, admin::SETTINGS] {
            assert!(
                js.contains(&format!("frame.method === \"{method}\"")),
                "管理面方法 {method} 必须在 host/main.mjs 的 handle() 中分支处理"
            );
        }
    }
}
