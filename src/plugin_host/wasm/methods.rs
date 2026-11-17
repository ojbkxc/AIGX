// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//! 插件方法名常量（替代上游 `proto::methods`）。
//!
//! 只收录 Wasm 宿主实际用到的方法名；值与上游 `provider-protocol` 的
//! `methods::gateway::*` 逐字一致（这些是插件与网关之间的线上契约）。

/// 网关方法名。
pub(crate) mod gateway {
    /// 握手（插件返回 `Vec<ProviderInfo>` + 可选 settings schema）。
    pub const INIT: &str = "init";
    /// 流式对话（走 `open_wasm_stream`）。
    pub const CONVERSATION: &str = "conversation";
    /// 账号公开视图校验。
    pub const ACCOUNT_VIEW: &str = "account.view";
    /// 账号归一化（导入）。
    pub const ACCOUNT_NORMALIZE: &str = "account.normalize";
    /// 登录轮询。
    pub const LOGIN_POLL: &str = "login.poll";
    /// 登录回调。
    pub const LOGIN_CALLBACK: &str = "login.callback";
}
