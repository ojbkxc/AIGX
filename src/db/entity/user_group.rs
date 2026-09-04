//! UserGroup Entity — 用户分组表。
//!
//! 对应 src/user_group/mod.rs 的 UserGroup 模型。
//! 参照 new-api group.go 的分组倍率设计。

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "user_groups")]
pub struct Model {
    /// 分组名（唯一键）
    #[sea_orm(primary_key, auto_increment = false)]
    pub name: String,
    /// 计费倍率（1.0=原价）
    pub ratio: f64,
    /// 组内模型权限白名单（JSON 数组，None=不限）
    pub allowed_models: Option<String>,
    /// 描述
    pub description: String,
    /// 创建时间
    pub created_at: i64,
    /// 更新时间
    pub updated_at: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
