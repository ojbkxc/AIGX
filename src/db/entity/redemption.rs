//! Redemption Entity — 兑换码表。
//!
//! 对应 src/redemption/mod.rs 的 Redemption 模型。
//! 参照 new-api model/redemption.go 的 Redemption 模型。

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "redemptions")]
pub struct Model {
    /// 唯一 ID（UUID）
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    /// 兑换码（用户输入，唯一）
    #[sea_orm(unique)]
    pub code: String,
    /// 名称（备注）
    pub name: String,
    /// 面额（配额单位）
    pub quota: i64,
    /// 状态：1=未使用, 2=已使用, 3=禁用
    pub status: i32,
    /// 使用者用户 ID
    pub used_by: Option<String>,
    /// 兑换时间
    pub used_at: Option<i64>,
    /// 创建时间
    pub created_at: i64,
    /// 过期时间（0=永不过期）
    pub expires_at: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}