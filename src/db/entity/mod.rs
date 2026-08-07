//! SeaORM Entity 定义 — 为 AIGX 关键模型定义 SeaORM Entity。
//!
//! 参照 VFaka crates/aff-entity/src/entities 的 DeriveEntityModel 模式：
//! - `#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]`
//! - `#[sea_orm(table_name = "...")]`
//! - 主键用 `#[sea_orm(primary_key)]` 标注
//!
//! 为保持与现有 FileStore 模型兼容，主键采用 String（UUID）类型，
//! 时间戳采用 i64（unix timestamp）以匹配现有 schema。
//!
//! 仅当启用 `sea-orm` feature 时编译。

pub mod user;
pub mod api_key;
pub mod channel;
pub mod model_price;
pub mod user_group;
pub mod request_log;
pub mod redemption;
pub mod audit_log;