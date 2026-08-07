//! AuditLog Entity — 管理员操作审计日志表。
//!
//! 对应 src/log/mod.rs 的 AuditLog 模型。
//! 记录管理员写操作（创建/删除/更新用户、渠道、令牌、定价等）的 before/after。

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "audit_logs")]
pub struct Model {
    /// 唯一 ID（UUID）
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    /// 操作者用户 ID
    pub actor_id: String,
    /// 操作者邮箱
    pub actor_email: String,
    /// 操作类型：create / update / delete
    pub action: String,
    /// 目标资源类型：user / channel / api_key / pricing / group / redemption / config
    pub resource_type: String,
    /// 目标资源 ID
    pub resource_id: String,
    /// 变更前快照（JSON）
    pub before: Option<String>,
    /// 变更后快照（JSON）
    pub after: Option<String>,
    /// 客户端 IP
    pub ip: Option<String>,
    /// 创建时间（unix timestamp）
    pub created_at: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}