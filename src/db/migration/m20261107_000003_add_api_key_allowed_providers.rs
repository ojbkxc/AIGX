//! 迁移 000003 — api_keys 表增加 provider 白名单列（07 D 章 allowedProviders）。
//!
//! - `allowed_providers`：JSON 数组（Text 存储 JSON 字符串，与
//!   `allowed_models`/`ip_limit` 同口径），NULL=不限。
//!
//! 与 FileStore 后端的 serde default 零迁移语义对齐：
//! 旧数据行加载时列缺省值与 `ApiKey` 结构体的 `#[serde(default)]` 一致。

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(ApiKeys::Table)
                    .add_column(
                        ColumnDef::new(ApiKeys::AllowedProviders)
                            .text()
                            .null()
                            .to_owned(),
                    )
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(ApiKeys::Table)
                    .drop_column(ApiKeys::AllowedProviders)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}

// 复用 m20260807_000001 的 api_keys Iden（引用同一张表）；此处仅声明本迁移
// 涉及的列，避免跨文件依赖。
#[derive(DeriveIden)]
enum ApiKeys {
    Table,
    AllowedProviders,
}
