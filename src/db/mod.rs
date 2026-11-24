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

        tracing::info!(
            "{} database connected, running migrations...",
            backend.as_str()
        );

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
///
/// 只处理 **authority 段**（`scheme://` 之后到第一个 `/ ? #` 之前），且取该段里
/// **最后一个** `@`：密码里带 `@`（`postgres://user:p@ss@host/db`）时，按第一个
/// `@` 切会把 `ss@host` 当主机名原样打出来 —— 密码后半段进日志，而这一段的
/// 唯一职责就是「别把密码写进日志」。authority 之外出现的 `@`（路径/query 里的
/// 邮箱之类）不参与判定，避免把无凭据的 URL 改坏。
fn sanitize_url(url: &str) -> String {
    let Some(scheme_end) = url.find("://") else {
        return url.to_string();
    };
    let after_scheme = scheme_end + 3;
    let rest = &url[after_scheme..];
    let authority_len = rest
        .find(|ch| ch == '/' || ch == '?' || ch == '#')
        .unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(authority_len);
    let Some((userinfo, host)) = authority.rsplit_once('@') else {
        return url.to_string();
    };
    let Some((user, _password)) = userinfo.split_once(':') else {
        return url.to_string();
    };
    format!("{}://{}:***@{}{}", &url[..scheme_end], user, host, tail)
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

    /// 密码里带 `@` 时按第一个 `@` 切会把密码后半段当主机名原样打出来。
    #[test]
    fn test_sanitize_url_password_containing_at() {
        assert_eq!(
            sanitize_url("postgres://user:p@ss@localhost:5432/aigx"),
            "postgres://user:***@localhost:5432/aigx"
        );
        assert_eq!(
            sanitize_url("mysql://root:a@b@c@10.0.0.1/db"),
            "mysql://root:***@10.0.0.1/db"
        );
    }

    /// authority 之外的 `@`（路径/query 里的邮箱）不参与判定，也不改坏 URL。
    #[test]
    fn test_sanitize_url_ignores_at_outside_authority() {
        assert_eq!(
            sanitize_url("postgres://localhost/db?owner=ops@example.com"),
            "postgres://localhost/db?owner=ops@example.com"
        );
        assert_eq!(
            sanitize_url("sqlite://./data/a@b/aigx.db"),
            "sqlite://./data/a@b/aigx.db"
        );
    }

    /// 用户名是多字节字符时按字符边界切片（`&url[..scheme_end]` 只取 ASCII scheme）。
    #[test]
    fn test_sanitize_url_multibyte_user() {
        assert_eq!(
            sanitize_url("postgres://运维:密码@localhost/db"),
            "postgres://运维:***@localhost/db"
        );
    }
}
