//! ApiKey Entity — API 密钥表。
//!
//! 对应 src/api/auth.rs 的 ApiKey 模型。
//! 参照 new-api token.go 的 Token 模型。

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "api_keys")]
pub struct Model {
    /// 唯一 ID（UUID）
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    /// API Key 字符串（唯一）
    #[sea_orm(unique)]
    pub key: String,
    /// 名称
    pub name: String,
    /// 是否启用
    pub is_active: bool,
    /// 创建时间
    pub created_at: i64,
    /// 最后使用时间
    pub last_used_at: Option<i64>,
    /// 所属用户 ID（None=管理员级令牌）
    pub user_id: Option<String>,
    /// 分组（计费倍率依据）
    pub group: String,
    /// 模型白名单（JSON 数组，None=不限）
    pub allowed_models: Option<String>,
    /// 过期时间（unix timestamp，None=永不过期）
    pub expires_at: Option<i64>,
    /// 额度上限（None=不限）
    pub quota_limit: Option<i64>,
    /// 已用额度
    pub used_quota: i64,
    /// IP 白名单（JSON 数组，None=不限）
    pub ip_limit: Option<String>,
    /// 状态：active / disabled
    pub status: String,
    /// 更新时间
    pub updated_at: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    /// ApiKey 属于一个 User
    #[sea_orm(
        belongs_to = "super::user::Entity",
        from = "Column::UserId",
        to = "super::user::Column::Id"
    )]
    User,
}

impl Related<super::user::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::User.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}