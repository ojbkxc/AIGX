//! `api/openai` — 由单文件 openai.rs（5203 行）按功能域拆分，纯移动无逻辑改动。
//!
//! handler re-export 一律 `pub use`：main.rs（bin target）的路由要用它们。
//! lib target 里没有调用方，glob 告警用 `#[allow(unused_imports)]` 压掉
//! （对 bin 而言这些是真实使用的）。
//!
//! 同理，handler 与它们内部的辅助函数都是 `pub(crate)`（不是对外 API），
//! 而 lib target 不挂任何 `/v1/*` 路由 —— 从 lib 视角看这一整棵子树没有入口，
//! dead_code 会逐个报「never used」。bin 通过 main.rs 的私有 `mod api` 另行
//! 编译同一批文件，那里它们全部被路由引用，因此这里整模块豁免即可。
#![allow(dead_code)]

mod audio;
mod billing;
mod chat;
mod completions;
mod embeddings;
mod images;
mod messages;
mod models;
// #40 §1：请求体 → 能力需求的推导层（唯一标签映射链见该模块头注释）
mod needs;
mod rerank;
// 私有：跨模块用 RouteCapture 的调用方（api::anthropic）走下面 `pub use resolve::*`
// 的再导出，不用 `resolve::` 模块路径 —— 那要求 mod 至少 pub(crate)。
mod resolve;
mod responses;
mod state;

#[allow(unused_imports)]
pub use audio::*;
#[allow(unused_imports)]
pub use billing::*;
#[allow(unused_imports)]
pub use chat::*;
#[allow(unused_imports)]
pub use completions::*;
#[allow(unused_imports)]
pub use embeddings::*;
#[allow(unused_imports)]
pub use images::*;
#[allow(unused_imports)]
pub use messages::*;
#[allow(unused_imports)]
pub use models::*;
#[allow(unused_imports)]
pub use needs::*;
#[allow(unused_imports)]
pub use rerank::*;
#[allow(unused_imports)]
pub use resolve::*;
#[allow(unused_imports)]
pub use responses::*;
#[allow(unused_imports)]
pub use state::*;
