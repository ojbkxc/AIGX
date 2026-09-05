//! 智能会话路由
//!
//! 提供多种路由策略，选择最佳会话（re-export 自 mod.rs 的统一实现）

pub use super::{RouterStrategy, SessionTransitionTable, SmartRouter};
