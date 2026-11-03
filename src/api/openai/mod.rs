//! `api/openai` — 由单文件 openai.rs（5203 行）按功能域拆分，纯移动无逻辑改动。
//!
//! handler re-export 一律 `pub use`：main.rs（bin target）的路由要用它们。
//! lib target 里没有调用方，glob 告警用 `#[allow(unused_imports)]` 压掉
//! （对 bin 而言这些是真实使用的）。
mod state;
mod resolve;
mod billing;
mod messages;
mod chat;
mod responses;
mod completions;
mod embeddings;
mod rerank;
mod images;
mod audio;
mod models;

#[allow(unused_imports)]
pub use state::*;
#[allow(unused_imports)]
pub use resolve::*;
#[allow(unused_imports)]
pub use billing::*;
#[allow(unused_imports)]
pub use messages::*;
#[allow(unused_imports)]
pub use chat::*;
#[allow(unused_imports)]
pub use responses::*;
#[allow(unused_imports)]
pub use completions::*;
#[allow(unused_imports)]
pub use embeddings::*;
#[allow(unused_imports)]
pub use rerank::*;
#[allow(unused_imports)]
pub use images::*;
#[allow(unused_imports)]
pub use audio::*;
#[allow(unused_imports)]
pub use models::*;
