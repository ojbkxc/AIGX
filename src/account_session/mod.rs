//! 账号型渠道会话池 —— 渠道内的第二级调度（账号级优先级/冷却/并发/粘性）。
//!
//! 模块划分：
//! - [`model`]：账号/凭证数据模型（凭证独立于 `Channel` 存储）
//! - [`registry`]：provider 目录（粘贴哪家 Key、用哪个原生 Bridge）
//! - [`adapter`]：账号 → `Bridge` 适配（复用既有原生 Bridge）
//! - [`store`]：KV 持久化 + 内存池 + 租约式并发 + 冷却表
//! - [`rotate`]：失败分类 → 冷却时长（分类复用渠道断路器）
//! - [`maintenance`]：周期清理（冷却过期、凭证过期状态同步）

pub mod adapter;
pub mod maintenance;
pub mod model;
pub mod registry;
pub mod rotate;
pub mod store;

pub use store::AccountSessionStore;
