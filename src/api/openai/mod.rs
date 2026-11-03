//! `api/openai` — 由单文件 openai.rs（5203 行）按功能域拆分，纯移动无逻辑改动。
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

pub use state::*;
pub use resolve::*;
pub use billing::*;
pub use messages::*;
pub use chat::*;
pub use responses::*;
pub use completions::*;
pub use embeddings::*;
pub use rerank::*;
pub use images::*;
pub use audio::*;
pub use models::*;
