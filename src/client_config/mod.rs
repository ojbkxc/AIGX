//! 客户端接入多配置管理（对齐 Agent2API `core/agents` 的非插件能力）。
//!
//! ## 能力
//!
//! 为四个本机 AI 客户端目标维护**多份命名配置**，并支持把其中一份
//! 预览 → 备份 → 写入本机配置文件：
//!
//! | 目标 | 主配置文件 | 凭据落点 |
//! |---|---|---|
//! | `claude-code` | `~/.claude/settings.json` | 同文件 `env` |
//! | `codex` | `~/.codex/config.toml` | 同文件 `model_providers.*` |
//! | `claude-desktop` | Claude Desktop 的 `claude_desktop_config.json` | 同文件 `env` |
//! | `dsh` | `~/.dsh/settings.yaml` | **独立** `~/.dsh/.credentials.yaml` |
//!
//! ## 硬性纪律
//!
//! - **配置文件路径写死**（仅接受四个环境变量做目录覆盖），HTTP 请求体
//!   不接受任何路径参数 —— 接受用户路径等于任意文件写入漏洞。
//! - **写入前必须预览**：预览签发短时确认令牌（指纹绑定「记录 + 文件
//!   现状 + 目标内容」），应用时重新计算计划并校验令牌，防 TOCTOU。
//! - **写前自动备份**：原文件复制为 `<file>.bak.<时间戳>`，每个文件只保留
//!   最近 [`KEEP_BACKUPS`] 份。
//! - **凭据加密落库**：记录里的 API Key 一律以 `enc1:`（AES-256-GCM，
//!   密钥在库外的 `account.key`）信封保存，复用
//!   [`crate::account_session::secret`]；接口响应只回 `hasApiKey`，不回明文。
//! - **不猜格式**：目标文件现状解析失败（JSON/TOML/YAML 非法、DSH 凭据
//!   文件不是 `version: 1` 布局）时拒绝写入，绝不覆盖用户手写坏掉的文件。
//! - **注释保留**：Codex 的 `config.toml` 用 `toml_edit` 做原位补丁，
//!   注释、键顺序与空白全部保留；JSON/DSH YAML 走结构化读写。
//!
//! ## 模块划分
//!
//! - [`edit`]：三种格式的解析 / 渲染 / 预览脱敏
//! - [`targets`]：目标定义、默认路径、字段校验、按目标生成新配置内容
//! - [`store`]：KV 持久化 + 多配置 CRUD（同一目标至多一份启用）
//! - [`plan`]：预览计划、确认令牌、备份轮转、原子写入、恢复

pub mod edit;
pub mod plan;
pub mod store;
pub mod targets;

use std::sync::{Mutex, MutexGuard, OnceLock};

/// 每个目标最多保留的配置份数（管理面软上限，防误用刷爆 KV）。
pub const MAX_CONFIGS_PER_TARGET: usize = 100;

/// 每个配置文件保留的备份份数。
pub const KEEP_BACKUPS: usize = 10;

/// 单个配置文件大小上限（8 MiB）；超限视为异常文件，拒绝读写。
pub const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;

/// 预览确认令牌有效期（10 分钟，与 Agent2API 一致）。
pub const PREVIEW_TOKEN_TTL_MS: i64 = 10 * 60 * 1000;

fn mutation_lock_cell() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// 配置存储与文件写入共用的进程级互斥锁。
///
/// - 存储侧：load → 修改 → save 的读改写必须串行；
/// - 文件侧：预览令牌校验 → 备份 → 写入 的窗口不能被同进程的另一次
///   应用插入。
///
/// 管理面冷路径，持锁期间只有小文件 IO，不涉及数据面。
pub fn mutation_lock() -> Result<MutexGuard<'static, ()>, String> {
    mutation_lock_cell()
        .lock()
        .map_err(|_| "客户端配置写入锁不可用".to_string())
}

fn apply_lock_cell() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// 文件应用/恢复的临界区锁（与 [`mutation_lock`] 分离，避免与存储层的
/// 读改写锁嵌套 —— `std::sync::Mutex` 不可重入）。
///
/// 应用序列（校验令牌 → 备份 → 写入 → 回读）必须整体串行：两个并发应用
/// 同读旧内容再先后写，会产出互相覆盖且备份翻倍。
pub fn apply_lock() -> Result<MutexGuard<'static, ()>, String> {
    apply_lock_cell()
        .lock()
        .map_err(|_| "客户端配置写入锁不可用".to_string())
}

/// 当前时间（Unix 毫秒）。
pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
