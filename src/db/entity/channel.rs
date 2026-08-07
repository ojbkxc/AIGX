//! Channel Entity — 通用上游渠道表。
//!
//! 对应 src/channel/mod.rs 的 Channel 模型。
//! 参照 new-api channel.go 的数据模型。

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "channels")]
pub struct Model {
    /// 唯一 ID（UUID）
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    /// 渠道名称
    pub name: String,
    /// 渠道类型：cloudflare / openai_compatible / anthropic
    pub channel_type: String,
    /// 上游基础 URL
    pub base_url: String,
    /// 编码后的 api_key（`enc:` 前缀 + base64）
    pub api_key: String,
    /// 优先级（数值越大越优先）
    pub priority: i32,
    /// 权重（同优先级内加权选取）
    pub weight: i32,
    /// 状态：enabled / disabled
    pub status: String,
    /// 支持的模型列表（JSON 数组）
    pub models: String,
    /// 分组（渠道所属分组）
    #[sea_orm(default_value = "default")]
    pub group: String,
    /// 是否健康
    pub healthy: bool,
    /// 连续失败次数
    pub fail_count: i32,
    /// 创建时间
    pub created_at: i64,
    /// 更新时间
    pub updated_at: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    /// Channel 产生多个 RequestLog
    #[sea_orm(has_many = "super::request_log::Entity")]
    RequestLogs,
}

impl Related<super::request_log::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::RequestLogs.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}