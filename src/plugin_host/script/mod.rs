//! 脚本插件宿主 —— **一个** Bun 进程承载全部 npm 风格 JS 脚本。
//!
//! 对齐 A2A 3.5.0-beta.1 的 Magpie/OpenCode 脚本插件体系（仅对齐可观察的
//! 接口契约：协议行为、数据结构、模块职责；实现为 AIGX 独立书写）。
//!
//! ## 三类插件的关系
//!
//! | 类型 | 宿主 | 运行时 | 来源 | AIGX 状态 |
//! |---|---|---|---|---|
//! | 原生包 | `super::process`（每包一个子进程） | Rust 二进制 | AGPL 包动态拉取 | 已有（`../manager.rs`） |
//! | **脚本插件** | 本模块（一个 Bun 进程承载全部脚本） | Bun（按需下载） | Magpie 社区 / GitHub / 本地 | 本模块 |
//! | Wasm 插件 | 上游有（导出函数 ABI） | wasm 运行时 | beta 分支 | 近期不做（只覆盖已原生的 atomcode） |
//!
//! 脚本 ↔ 宿主用 **stdio NDJSON**（帧形状与原生包同一套，复用 `super::rpc`
//! 的 `Frame` 与 id 奇偶规则：网关奇、脚本偶、0 为通知）。宿主对脚本暴露
//! 完整 provider 能力面（conversation + 23 个能力方法，清单见 `rpc.rs` 与
//! `host/provider.mjs` 的 DISPATCH_METHODS），并经网关回调获得 HTTP egress
//! 代理与 SSE 协议转换（anthropic / responses / gemini → OpenAI chat SSE）。
//!
//! ## 模块布局
//!
//! - `types` —— 共享类型契约（ScriptPackage / ScriptSource / ScriptPublisher /
//!   ScriptProviderInfo / ScriptCatalogEntry / ScriptInitPayload / 设置 schema）
//! - `rpc` —— provider 能力面方法名常量 + 超时档位（控制面 10s / 网络面
//!   120s）。**不定义新 trait/Adapter**：Bun 进程的 stdin/stdout 直接塞进
//!   `super::rpc::RpcChannel`，帧编解码/奇偶 id/credit 背压全部现成复用
//! - `protocol` —— SSE 协议转换器（anthropic / responses / gemini → OpenAI
//!   chat SSE；并发 ≤128，见终态标志才收尾）
//! - `transport` —— HTTP egress 代理（体 ≤120MB、分片 1KB–256KB、空闲 30min
//!   回收）。transport / protocol 两个出口模块都以 `script.transport.*` /
//!   `script.protocol.*` 回调方法的形态由 `callbacks::ScriptCallbacks` 分发
//! - `runtime` —— 宿主生命周期门面：rebuild 热切换（spawn 成功才换指针，
//!   旧宿主排空在途流后关闭）、崩溃退避重启、provider→package owners 表 +
//!   同步 `route_for_provider`（数据面调用点是同步函数，不能 await）
//! - `operations` —— 安装（强制 trusted）/ 启停（rebuild 失败回滚）/ 卸载 /
//!   配置更新
//! - `settings` —— 包 options（≤256KiB）与 settings_schema 逐键对照
//! - `callbacks` —— `PluginCallbacks`（账号/设置/台账）+ egress/协议转换的
//!   异步组合器
//! - `bun` —— Bun 运行时按需下载 + 九平台 SHA256 表 + 探针校验
//! - `catalog` —— Magpie 双源目录（registry.json + dist/index.json +
//!   GitHub topic 发现）三分类 + TTL 缓存 + 后台刷新循环
//! - `host_rs` —— Bun 子进程生命周期（宿主文件指纹落盘 / init 60s 握手 /
//!   有界 stderr 泵 / 优雅关闭）；`host/*.mjs` 是随二进制嵌入的 JS 宿主
//! - `install` / `source` / `packages` / `dependencies` —— 安装链路（四条
//!   下载通路：magpie dist tgz / npm registry / GitHub codeload / 本机目录）
//! - `storage` —— KV 权威状态（`plugin:script-packs`）+ `registry.json`
//!   磁盘投影（js-host 启动时读）
//!
//! WebSocket egress（`script.websocket.*`）**本期不做**：当前脚本契约
//! （`host/provider.mjs`）没有消费方，等有包真正需要时再补；不预先写死
//! 一套没有调用者的会话协议。
//!
//! ## 硬约束
//!
//! - AIGX **托管**社区脚本（按需下载、运维显式信任），**不分发、不抄写**
//!   AGPL 许可的上游脚本与宿主实现——本模块全部为独立实现
//! - 脚本不是 OS 沙箱：安装必须显式 `trusted=true`（`operations` 强制）
//! - 脚本只接管 `claimed`（已启用脚本包声明）且不在 `reserved`（原生实现、
//!   内置订阅、非脚本宿主已声明）中的 provider——与原生包的
//!   `is_plugin_owned` 同立场：脚本不能换掉能用的家

pub mod bun;
pub mod callbacks;
pub mod catalog;
pub mod dependencies;
pub mod host_rs;
pub mod install;
pub mod operations;
pub mod packages;
pub mod protocol;
pub mod runtime;
pub mod settings;
pub mod source;
pub mod storage;
pub mod transport;
pub mod types;

pub use runtime::ScriptHostManager;
pub use types::{
    script_plugins_root, ScriptCatalogEntry, ScriptInitPayload, ScriptPackage, ScriptProviderInfo,
    ScriptPublisher, ScriptSettingField, ScriptSettingsOptions, ScriptSource, MAX_SCRIPT_PACKAGES,
    MAX_SETTINGS_BYTES, SCRIPT_PLUGINS_DIR,
};
