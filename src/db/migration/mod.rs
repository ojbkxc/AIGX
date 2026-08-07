//! SeaORM Migration — 创建 AIGX 数据库表结构。
//!
//! 参照 VFaka crates/aff-entity/src/migration 的 Migration 模式：
//! - 每个迁移文件定义一个 `Migration` struct 实现 `MigrationTrait`
//! - `Migrator` 汇总所有迁移
//! - 使用 `DeriveIden` 生成类型安全的表/列引用
//!
//! 仅当启用 `sea-orm` feature 时编译。

pub mod m20260807_000001_init;

use sea_orm_migration::prelude::*;

/// Migrator — 汇总所有迁移。
pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![Box::new(m20260807_000001_init::Migration)]
    }
}