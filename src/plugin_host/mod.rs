//! 插件宿主 —— 通用子进程基础设施。
//!
//! ## 定位
//!
//! 与 Agent2API 的插件协议对齐（上游 `provider-protocol` crate）。插件是
//! **独立二进制**，通过 stdin/stdout 逐行 JSON 与网关双向 RPC。本模块
//! 实现网关侧的宿主设施：
//!
//! - `rpc` —— 帧编解码 + 双向路由内核
//! - `process` —— 子进程 spawn / 握手 / 崩溃重启 / 优雅关闭
//!
//! ## 硬约束
//!
//! - AIGX **不拷插件代码、不打包分发插件**，只实现宿主通用设施；插件由
//!   运维在运行期从配置的源 URL 拉取
//! - 不写 provider 伪装实现（规避上游检测类，既有立场）
//! - 插件只接管「插件声明了、且 AIGX 无原生实现」的 provider

pub mod process;
pub mod rpc;

pub use process::{PluginProcess, ProcessState, SpawnError};
pub use rpc::{
    CallbackHandler, Frame, FrameKind, NoopCallbacks, RpcChannel, RpcError, MAX_CHUNK_BYTES,
    MAX_LINE_BYTES, PROTOCOL_VERSION,
};
