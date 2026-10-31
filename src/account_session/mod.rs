//! 账号型渠道会话池 —— 渠道内的第二级调度（账号级优先级/冷却/并发/粘性）。
//!
//! 模块划分：
//! - [`model`]：账号/凭证数据模型（凭证独立于 `Channel` 存储）
//! - [`registry`]：provider 目录（粘贴哪家 Key、用哪个原生 Bridge）
//! - [`adapter`]：账号 → `Bridge` 适配（复用既有原生 Bridge）
//! - [`store`]：KV 持久化 + 内存池 + 租约式并发 + 冷却表
//! - [`rotate`]：失败分类 → 冷却时长（分类复用渠道断路器）
//! - [`maintenance`]：周期清理（冷却过期、凭证过期状态同步）
//! - [`refresh`]：账号级 token 自动刷新（qoder / autoclaw / trae，表驱动）
//! - [`catalog`]：按账号自动发现上游模型目录（抄 Agent2API 的 provider 目录）
//! - [`import`]：从本机其他 AI 客户端（cc-switch）导入账号

pub mod adapter;
pub mod balance;
pub mod catalog;
pub mod import;
pub mod maintenance;
pub mod model;
pub mod refresh;
pub mod registry;
pub mod rotate;
pub mod store;

pub use store::AccountSessionStore;
