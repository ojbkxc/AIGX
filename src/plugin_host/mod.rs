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
pub mod process;
pub mod provider_settings;
pub mod registry;
pub mod rpc;
pub mod script;
// Wasm 插件沙箱（对齐 Agent2API v3.5.0-beta.1 的 Wasm 插件宿主；
// 内部为进程级注册表，数据面经 `wasm::host_for_provider` 路由）。
pub(crate) mod wasm;

pub use manager::PluginHostManager;
pub use process::{host_target_triple, PluginProcess};
pub use registry::PackMeta;
pub use rpc::{RpcError, StreamCall, StreamEvent};
