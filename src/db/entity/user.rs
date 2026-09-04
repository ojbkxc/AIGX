//! User Entity — 用户表。
//!
//! 对应 src/user/mod.rs 的 User 模型。

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "users")]
pub struct Model {
    /// 唯一 ID（UUID 字符串）
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    /// 邮箱（唯一标识，用于登录）
    #[sea_orm(unique)]
    pub email: String,
    /// 用户名（展示昵称）
    pub username: String,
    /// 密码哈希 (argon2)
    pub password: String,
    /// 角色：admin / user
    pub role: String,
    /// 总配额（充值 + 赠送）
    pub quota: i64,
    /// 已用配额
    pub used_quota: i64,
    /// 状态: active / disabled
    pub status: String,
    /// 用户分组（计费倍率与模型权限依据）
    pub group: String,
    /// 创建时间（unix timestamp）
    pub created_at: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    /// User 拥有多个 ApiKey
    #[sea_orm(has_many = "super::api_key::Entity")]
    ApiKeys,
    /// User 产生多个 RequestLog
    #[sea_orm(has_many = "super::request_log::Entity")]
    RequestLogs,
}

impl Related<super::api_key::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::ApiKeys.def()
    }
}

impl Related<super::request_log::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::RequestLogs.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
