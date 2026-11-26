//! SeaORM Migration — 创建 AIGX 数据库表结构。
//!
//! 参照 VFaka crates/aff-entity/src/migration 的 Migration 模式：
//! - 每个迁移文件定义一个 `Migration` struct 实现 `MigrationTrait`
//! - `Migrator` 汇总所有迁移
//! - 使用 `DeriveIden` 生成类型安全的表/列引用
//!
//! 仅当启用 `sea-orm` feature 时编译。

pub mod m20260807_000001_init;
pub mod m20260908_000002_add_totp;
pub mod m20261107_000003_add_api_key_allowed_providers;

use sea_orm_migration::prelude::*;

/// Migrator — 汇总所有迁移。
pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20260807_000001_init::Migration),
            Box::new(m20260908_000002_add_totp::Migration),
            Box::new(m20261107_000003_add_api_key_allowed_providers::Migration),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm_migration::prelude::*;

    /// 迁移清单自检：名称取源码文件名（`get_file_stem(file!())`），必须唯一且
    /// 按时间戳升序——SeaORM 按返回顺序执行 `up()`，乱序会让「先加列后建表」
    /// 这类有依赖的迁移在真实库上失败；重名则 `seaql_migrations` 台账互相覆盖。
    #[test]
    fn migrations_are_unique_and_chronological() {
        let names: Vec<String> = Migrator::migrations()
            .iter()
            .map(|m| m.name().to_string())
            .collect();

        assert_eq!(names.len(), 3, "迁移条数与预期不符: {names:?}");
        assert!(names.iter().all(|n| !n.is_empty()), "迁移名不能为空");

        let mut sorted = names.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), names.len(), "迁移名重复: {names:?}");
        assert_eq!(sorted, names, "迁移必须按文件名时间戳升序: {names:?}");
    }
}
