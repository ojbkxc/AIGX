//! SeaORM 数据库连接管理 — 多数据库后端支持（PostgreSQL/MySQL）。
//!
//! 参照 VFaka crates/aff-entity 的 SeaORM 使用模式。
//!
//! ## 渐进式迁移策略
//!
//! - 当 `config.database.url` 为空时：使用现有 FileStore（默认，零配置）
//! - 当 `config.database.url` 有值时：启用 SeaORM 连接，新数据写入 SeaORM，
//!   同时保留 FileStore 兼容读取
//! - 在 AppState 增加 `db_conn: Option<sea_orm::DatabaseConnection>`，
//!   有值时表示启用了 SeaORM
//!
//! ## Feature gate
//!
//! 整个 `db` 模块仅在启用 `sea-orm` feature 时编译。默认构建不引入 sea-orm 依赖，
//! 不影响现有功能与构建产物体积。
//!
//! ## 支持的数据库 URL 格式
//!
//! - `postgres://user:pass@localhost:5432/aigx` — PostgreSQL
//! - `mysql://user:pass@localhost:3306/aigx` — MySQL
//!
//! 注意：SeaORM 的 SQLite 驱动未启用（与 rusqlite 的 libsqlite3-sys 存在 links 冲突）。
//! SQLite 场景由默认的 FileStore/rusqlite 后端覆盖。

pub mod entity;
pub mod migration;

use sea_orm::{Database, DatabaseConnection};
use sea_orm_migration::MigratorTrait;

/// 数据库后端类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DatabaseBackend {
    Postgres,
    Mysql,
}

impl DatabaseBackend {
    /// 从连接 URL 推断后端类型。
    ///
    /// - `postgres://` 或 `postgresql://` → Postgres
    /// - `mysql://` → Mysql
    /// - 其他 → 返回 None
    pub fn from_url(url: &str) -> Option<Self> {
        let lower = url.to_lowercase();
        if lower.starts_with("postgres://") || lower.starts_with("postgresql://") {
            Some(Self::Postgres)
        } else if lower.starts_with("mysql://") {
            Some(Self::Mysql)
        } else {
            None
        }
    }

    /// 返回后端名称字符串。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Postgres => "postgres",
            Self::Mysql => "mysql",
        }
    }
}

/// 数据库管理器 — 封装 SeaORM 连接与迁移。
///
/// 使用方式：
/// ```ignore
/// # #[cfg(feature = "sea-orm")]
/// # async fn example() -> anyhow::Result<()> {
/// let manager = DatabaseManager::connect("postgres://user:pass@localhost:5432/aigx").await?;
/// let conn = manager.connection();
/// # Ok(())
/// # }
/// ```
pub struct DatabaseManager {
    conn: DatabaseConnection,
    backend: DatabaseBackend,
}

impl DatabaseManager {
    /// 连接数据库并执行迁移。
    ///
    /// `url` 格式：
    /// - `sqlite://./data/aigx.db`
    /// - `postgres://user:pass@localhost:5432/aigx`
    /// - `mysql://user:pass@localhost:3306/aigx`
    pub async fn connect(url: &str) -> anyhow::Result<Self> {
        let backend = DatabaseBackend::from_url(url)
            .ok_or_else(|| anyhow::anyhow!("unsupported database url: {}", url))?;

        tracing::info!(
            "connecting to {} database: {}",
            backend.as_str(),
            sanitize_url(url)
        );

        // 连接数据库
        let conn = Database::connect(url).await.map_err(|e| {
            anyhow::anyhow!("failed to connect to {} database: {}", backend.as_str(), e)
        })?;

        tracing::info!("{} database connected, running migrations...", backend.as_str());

        // 执行迁移
        migration::Migrator::up(&conn, None)
            .await
            .map_err(|e| anyhow::anyhow!("migration failed: {}", e))?;

        tracing::info!(
            "{} database migrations completed successfully",
            backend.as_str()
        );

        Ok(Self { conn, backend })
    }

    /// 返回数据库连接引用。
    pub fn connection(&self) -> &DatabaseConnection {
        &self.conn
    }

    /// 返回数据库后端类型。
    pub fn backend(&self) -> DatabaseBackend {
        self.backend
    }


    /// 是否为 PostgreSQL 后端。
    pub fn is_postgres(&self) -> bool {
        self.backend == DatabaseBackend::Postgres
    }

    /// 是否为 MySQL 后端。
    pub fn is_mysql(&self) -> bool {
        self.backend == DatabaseBackend::Mysql
    }
}

/// 隐藏 URL 中的密码部分用于日志输出。
fn sanitize_url(url: &str) -> String {
    // 简单处理：如果包含 @，隐藏 :...@ 之间的密码
    if let Some(at_pos) = url.find('@') {
        if let Some(scheme_end) = url.find("://") {
            let after_scheme = scheme_end + 3;
            if after_scheme < at_pos {
                let (scheme_part, rest) = url.split_at(after_scheme);
                let (credentials, host_part) = rest.split_at(at_pos - after_scheme);
                if let Some(colon_pos) = credentials.find(':') {
                    let user = &credentials[..colon_pos];
                    return format!("{}{}:***{}", scheme_part, user, host_part);
                }
            }
        }
    }
    url.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_backend_from_url() {
        assert_eq!(

            DatabaseBackend::from_url("postgres://user:pass@localhost:5432/aigx"),
            Some(DatabaseBackend::Postgres)
        );
        assert_eq!(
            DatabaseBackend::from_url("postgresql://user:pass@localhost:5432/aigx"),
            Some(DatabaseBackend::Postgres)
        );
        assert_eq!(
            DatabaseBackend::from_url("mysql://user:pass@localhost:3306/aigx"),
            Some(DatabaseBackend::Mysql)
        );
        assert_eq!(DatabaseBackend::from_url("invalid://foo"), None);
        assert_eq!(DatabaseBackend::from_url(""), None);
    }

    #[test]
    fn test_sanitize_url() {
        assert_eq!(
            sanitize_url("postgres://user:pass@localhost:5432/aigx"),
            "postgres://user:***@localhost:5432/aigx"
        );
        assert_eq!(
            sanitize_url("mysql://root:secret@localhost:3306/db"),
            "mysql://root:***@localhost:3306/db"
        );
        // 无密码的 URL 保持原样
        assert_eq!(
            sanitize_url("sqlite://./data/aigx.db"),
            "sqlite://./data/aigx.db"
        );
    }
}