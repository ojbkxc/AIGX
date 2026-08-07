//! ModelPrice Entity — 模型定价目录表。
//!
//! 对应 src/pricing/mod.rs 的 ModelPrice 模型。
//! 参照 new-api pricing.go 的定价目录设计。

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "model_prices")]
pub struct Model {
    /// 模型名（唯一键）
    #[sea_orm(primary_key, auto_increment = false)]
    pub model_name: String,
    /// 输入 token 每 1k 价格
    pub input_price: f64,
    /// 输出 token 每 1k 价格
    pub output_price: f64,
    /// 缓存 token 价格（可选）
    pub cache_price: Option<f64>,
    /// 计价类型：token（按量）或 count（按次）
    pub price_type: String,
    /// 创建时间
    pub created_at: i64,
    /// 更新时间
    pub updated_at: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}