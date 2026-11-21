//! 插件宿主 —— 通用子进程基础设施。
//!
//! ## 定位
//!
//! 与 Agent2API 的插件协议对齐（上游 `provider-protocol` crate）。插件是
//! **独立二进制**，通过 stdin/stdout 逐行 JSON 与网关双向 RPC。本模块
//! 实现网关侧的宿主设施：
//!
//! - `rpc` —— 帧编解码 + 双向路由内核（含流式背压 credit）
//! - `callbacks` —— 插件 → 网关的回调实现（store/settings/ledger/log/notify）
//! - `process` —— 子进程 spawn / 握手 / 崩溃观察 / 优雅关闭
//! - `registry` —— 包元数据 + provider 归属路由（**插件优先**）
//! - `install` —— 下载 / 解压 / 校验
//! - `provider_settings` —— 插件声明的「提供商设置」（schema + CAS 存储）
//! - `support` —— 包支持状态（beta.2：旧原生 → Wasm 迁移判定）
//! - `stream_events` —— 流式事件接收器（beta.2：`StreamEvents` 类型）
//! - `rpc_refresh` —— 凭据续期就绪检查（beta.2）
//!
//! ## 对齐版本
//!
//! 对齐 Agent2API v3.5.0-beta.2（2026-11-18）。beta.1 → beta.2 的关键变更：
//! - 旧原生插件停止运行，全面转向独立 WASM 包（`support::PackSupportState`）
//! - `StreamCall.events` 改为 `StreamEvents`（封装接收器 + 内存守卫）
//! - 新增错误码 `plugin_busy`、`account_origin_authorization_required`、
//!   `account_origin_proxy_unsupported`
//! - 新增能力位 `web_tool_emulation`、`sms_login`
//! - `ErrorPolicy` 新增 `stop_switching` 字段
//! - `dynamic_login` 从 `wasm::owns` 移至 `registry`，`wasm::owns` 改名 `is_wasm_provider`
//!
//! ## 硬约束
//!
//! - AIGX **不拷插件代码、不打包分发插件**，只实现宿主通用设施；插件由
//!   运维在运行期从配置的源 URL 拉取
//! - 不写 provider 伪装实现（规避上游检测类，既有立场）
//! - 归属是**插件优先**：已装且启用的插件声明了就接管，原生实现只在
//!   插件未装/停用/加载失败时兜底（硬保留集合除外，见 `registry`）

pub mod callbacks;
pub mod install;
pub mod manager;
/// provider 维度的插件模型目录（`wasm::catalog` 并集的落点与读取面）。
pub mod model_catalog;
/// 插件生命周期操作（异步进度模型 + 批量安装/更新/启停/卸载）。
pub mod operations;
pub mod process;
pub mod provider_settings;
pub mod registry;
pub mod rpc;
/// 凭据续期就绪检查（对齐 beta.2 `rpc_refresh`）。
///
/// beta.2 协议形状占位：AIGX 的续期由 `account_session` 层实现，本模块只
/// 保留错误码与签名形状供未来接线。整模块 allow(dead_code)，接线后应移除。
#[allow(dead_code)]
mod rpc_refresh;
pub mod script;
/// 流式事件接收器（对齐 beta.2 `stream_events`）。
///
/// beta.2 从裸 `UnboundedReceiver` 改为 `StreamEvents` 包装；AIGX 的
/// `StreamCall` 尚未切到该包装（数据面用 `next()` 直读），故本模块当前无
/// 消费者。整模块 allow(dead_code)，切流后应移除。
#[allow(dead_code)]
mod stream_events;
#[allow(unused_imports)] // 公开别名：消费者在 main.rs/bridge（当前未接线）
pub(crate) use stream_events::StreamEvents;
/// 包支持状态（对齐 beta.2 `support`）。
///
/// 判定 Wasm vs 旧原生包的可用性；管理面当前直接看 `manifest::is_wasm`，
/// 本模块作为 beta.2 API 形状保留，待管理面/安装闸门接线后移除 allow。
#[allow(dead_code)]
mod support;
#[allow(unused_imports)] // 公开别名：消费者待接线
pub(crate) use support::installed_state as support_state;
#[allow(unused_imports)] // 公开别名：消费者待接线
pub use support::PackSupportState;
// Wasm 插件沙箱（对齐 Agent2API v3.5.0-beta.2 的 Wasm 插件宿主；
// 内部为进程级注册表，数据面经 `wasm::host_for_provider` 路由）。
pub(crate) mod wasm;

pub use manager::PluginHostManager;
pub use process::{host_target_triple, PluginProcess};
#[allow(unused_imports)] // `dynamic_login` 别名待接线（registry 内有同名函数）
pub use registry::{dynamic_login, PackMeta};
pub use rpc::{CancellationToken, RpcError, StreamCall, StreamEvent};
/// 该 provider 是否由 Wasm 包接管（对齐 beta.2 `is_wasm_provider`）。
#[allow(unused_imports)] // 公开别名待接线
pub use wasm::owns as is_wasm_provider;
