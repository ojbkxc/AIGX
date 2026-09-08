//! 迁移 000002 — users 表增加 TOTP 两列（P1：2FA/TOTP）。
//!
//! - `totp_secret`：base32 编码的 TOTP 密钥（string 64，空串=未设置）
//! - `totp_enabled`：是否已启用 TOTP 二次验证（boolean，默认 false）
//!
//! 与 FileStore 后端的 serde default 零迁移语义对齐：
//! 旧数据行加载时列缺省值与 `User` 结构体的 `#[serde(default)]` 一致。

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Users::Table)
                    .add_column(
                        ColumnDef::new(Users::TotpSecret)
                            .string_len(64)
                            .not_null()
                            .default(""),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(Users::Table)
                    .add_column(
                        ColumnDef::new(Users::TotpEnabled)
                            .boolean()
                            .not_null()
                            .default(false),
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
                    .table(Users::Table)
                    .drop_column(Users::TotpSecret)
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(Users::Table)
                    .drop_column(Users::TotpEnabled)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}

// 复用 m20260807_000001 的 Users Iden（引用同一张表）；此处仅声明本迁移
// 涉及的列，避免跨文件依赖。
#[derive(DeriveIden)]
enum Users {
    Table,
    TotpSecret,
    TotpEnabled,
}
