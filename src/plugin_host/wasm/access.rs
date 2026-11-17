// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//! Wasm 沙箱对宿主能力的**注入面**。
//!
//! ## 为什么是 trait 而不是直接调 account_session
//!
//! 上游的 wasm 模块直接 `use crate::server::core::account_store::plugin_accounts`，
//! 因为它的账号存储是全局单例。AIGX 的账号在 `AppState.account_sessions`
//! （`Arc<AccountSessionStore>`，随进程状态注入），且 wasm 模块被要求**不直接
//! 依赖** `account_session` —— 否则「沙箱可独立编译/独立单测」这条就破了。
//!
//! 这里把沙箱用到的宿主能力收成三个 trait，由 integrator 在 `main.rs` 构造
//! `WasmOptions` 时注入实现（实现体放在 `plugin_host` 的接线层，不在本目录）：
//!
//! - [`WasmAccountAccess`] —— 账号记录 / 台账 / 设置 / 插件状态 CAS / 账号目录
//! - [`WasmEgress`] —— HTTP / WebSocket 出站（复用脚本宿主的 `script::transport`）
//! - [`WasmStreamSink`] —— 流式事件出口（head / chunk / 收尾）
//!
//! ## 语义约定（实现方必须遵守）
//!
//! - `store_get` 返回的记录形状与原生插件回调 `store.get` **完全一致**
//!   （`id` / `identity` / `public` / `secret` / `proxy`）；沙箱只读其中
//!   `id`、`identity`、`public.userAgent`、`proxy`。
//! - `plugin_state*` 是**账号维度**的持久 KV（上游 `plugin_accounts` 同语义），
//!   `revision` 单调递增，`compare_exchange` 版本不符时返回
//!   `{"updated": false, "revision": <当前>}` 而不是报错。
//! - `store_update_with_public` 的 `expect` 是插件读到的原 secret，用于
//!   乐观并发；宿主必须拒绝版本不符的写入。

use async_trait::async_trait;
use serde_json::Value;

use super::wire::WireError;

/// 账号 / 设置 / 台账 / 插件状态访问面。
// `async-trait` 0.1.91 的 `expand.rs:69` 无条件给每个 async trait 方法推
// `#[must_use]`，而这些方法返回的 `Result` / `Future` 本身已是 must_use →
// `clippy::double_must_use` 误报。升级 async-trait 前先在这里 allow。
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait WasmAccountAccess: Send + Sync {
    /// 该 provider 当前是否允许被插件接管（原生优先 / 白名单口径由宿主决定）。
    fn provider_allowed(&self, provider: &str) -> bool;

    /// 账号归属的 provider（未知账号返回 `None`）。
    fn account_provider(&self, account: &str) -> Option<String>;

    /// 读账号记录（形状见模块注释）。
    async fn store_get(&self, provider: &str, account: &str) -> Result<Value, WireError>;

    /// 带公开视图的账号写入（CAS）。
    async fn store_update_with_public(
        &self,
        account: &str,
        expect: Value,
        secret: Value,
        public: Value,
    ) -> Result<Value, WireError>;

    /// 读插件台账。
    async fn ledger_get(&self, account: &str, kind: &str) -> Result<Value, WireError>;

    /// 写插件台账。
    async fn ledger_put(&self, account: &str, kind: &str, value: Value) -> Result<(), WireError>;

    /// 读 provider 级设置（管理面保存的值）。
    async fn settings_get(&self, provider: &str, key: &str) -> Result<Value, WireError>;

    /// 读账号维度插件状态。
    async fn plugin_state(
        &self,
        provider: &str,
        account: &str,
        key: &str,
    ) -> Result<Value, WireError>;

    /// 账号维度插件状态 CAS 写入。
    async fn plugin_state_compare_exchange(
        &self,
        provider: &str,
        account: &str,
        key: &str,
        revision: u64,
        schema: u32,
        value: Value,
    ) -> Result<Value, WireError>;

    /// 该 provider 下的全部账号（每条是 `store_get` 同形记录）。
    fn accounts_for_provider(&self, provider: &str) -> Vec<Value>;
}

/// 出站网络访问面（HTTP / WebSocket）。
///
/// 方法名与脚本宿主的 `script.transport.*` 一致，实现方通常就是转发到
/// `crate::plugin_host::script::transport::Transport`。
// 同 [`WasmAccountAccess`]：`async-trait` 宏给 async 方法推 `#[must_use]`，
// 与 `Result` / `Future` 自带的 must_use 重复，属宏误报。
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait WasmEgress: Send + Sync {
    /// 发请求；`params.stream=true` 时返回 `session`，否则返回 `chunk_b64`。
    async fn request(&self, params: Value) -> Result<Value, WireError>;

    /// 读下一块（`{"chunk_b64": ..., "done": bool}`）。
    async fn read(&self, session: &str) -> Result<Value, WireError>;

    /// 关会话（幂等）。
    async fn close(&self, session: &str) -> Result<Value, WireError>;

    /// 打开 WebSocket。
    async fn ws_open(&self, params: Value) -> Result<Value, WireError>;

    /// 读 WebSocket 消息。
    async fn ws_read(&self, params: Value) -> Result<Value, WireError>;

    /// 写 WebSocket 消息。
    async fn ws_send(&self, params: Value) -> Result<Value, WireError>;

    /// 关 WebSocket。
    async fn ws_close(&self, params: Value) -> Result<Value, WireError>;

    /// 把账号的代理配置解析成实际代理（上游 `script::resolve_proxy`）。
    fn resolve_proxy(&self, params: &Value) -> Result<Value, WireError>;
}

/// 流式事件的出口。
///
/// 沙箱每次 `stream.head` / `stream.chunk` 都同步调它；背压由实现方自己
/// 用 [`WasmStreamSink::acquire`] 的额度语义控制（见 `context::Sink`）。
pub trait WasmStreamSink: Send + Sync {
    /// 响应头（只允许一次）。
    fn head(&self, status: u16, headers: Vec<(String, String)>) -> Result<(), WireError>;

    /// 一段正文（已解码字节，≤64 KiB）。
    fn chunk(&self, bytes: Vec<u8>) -> Result<(), WireError>;

    /// 流以错误终结（超时 / 陷阱 / 插件自报失败）。
    ///
    /// **默认实现为丢弃** —— 保持既有实现零改动；有能力传递错误的实现
    /// （如 `egress::ChannelSink`）应覆写它，否则消费方只看到「流正常关闭」，
    /// 把上游 4xx/超时当成「空回复成功」，failover 也失效。
    fn fail(&self, error: WireError) {
        let _ = error;
    }

    /// 流正常收尾，**必须**被宿主在结束调用时调一次。
    ///
    /// 作用是把「事件通道的终点」显式写给消费方：没有它，消费者只能靠
    /// 通道关闭探测结束，而通道的发送端由宿主持有、消费者又持有宿主，
    /// 会互相等待。默认实现为空（`ChannelSink` 会发一帧 `StreamEvent::End`）。
    fn end(&self) {}
}
