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
//! - `registry` —— 包元数据 + provider 归属路由
//! - `install` —— 下载 / 解压 / 校验
//!
//! ## 硬约束
//!
//! - AIGX **不拷插件代码、不打包分发插件**，只实现宿主通用设施；插件由
//!   运维在运行期从配置的源 URL 拉取
//! - 不写 provider 伪装实现（规避上游检测类，既有立场）
//! - 插件只接管「插件声明了、且 AIGX 无原生实现」的 provider

pub mod callbacks;
pub mod install;
pub mod manager;
pub mod process;
pub mod registry;
pub mod rpc;

pub use callbacks::PluginCallbacks;
pub use install::{install_from_zip, InstallError, PackManifest};
pub use manager::{PackRuntime, PluginHostManager};
pub use process::{host_target_triple, PluginProcess, ProcessState, SpawnError};
pub use registry::{is_native_provider, PackMeta, PackProvider, PackRegistry};
pub use rpc::{
    CallbackHandler, Frame, FrameKind, NoopCallbacks, RpcChannel, RpcError, StreamCall,
    StreamEvent, MAX_CHUNK_BYTES, MAX_LINE_BYTES, PROTOCOL_VERSION,
};
