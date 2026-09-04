//! RequestLog Entity — 请求日志表。
//!
//! 对应 src/log/mod.rs 的 RequestLog 模型。
//! 参照 new-api model/log.go 的 Log struct。

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "request_logs")]
pub struct Model {
    /// 唯一 ID（UUID）
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    /// 用户 ID（可能为空，管理员级令牌）
    pub user_id: Option<String>,
    /// API Key ID
    pub key_id: Option<String>,
    /// 渠道 ID
    pub channel_id: Option<String>,
    /// 模型名
    pub model: String,
    /// 输入 token 数
    pub input_tokens: u64,
    /// 输出 token 数
    pub output_tokens: u64,
    /// 本次请求费用（配额单位）
    pub cost: i64,
    /// 延迟（毫秒）
    pub latency_ms: u64,
    /// HTTP 状态码
    pub status_code: u16,
    /// 错误信息（失败时填）
    pub error_msg: Option<String>,
    /// 客户端 IP
    pub ip: Option<String>,
    /// 请求 ID
    pub request_id: Option<String>,
    /// 创建时间（unix timestamp）
    pub created_at: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    /// RequestLog 属于一个 User
    #[sea_orm(
        belongs_to = "super::user::Entity",
        from = "Column::UserId",
        to = "super::user::Column::Id"
    )]
    User,
    /// RequestLog 属于一个 Channel
    #[sea_orm(
        belongs_to = "super::channel::Entity",
        from = "Column::ChannelId",
        to = "super::channel::Column::Id"
    )]
    Channel,
}

impl Related<super::user::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::User.def()
    }
}

impl Related<super::channel::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Channel.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
