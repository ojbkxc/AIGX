//! 云同步：把本机 KV 业务数据快照传到 WebDAV / S3 兼容存储，或从远端取回。
//!
//! 对标 Agent2API 的 `core/cloud_sync`（WebDAV + S3 + 增量 + 合并恢复），按
//! AIGX 的存储形态（单一 FileStore KV）重新分层：
//!
//! ```text
//! config     设置与状态（KV 键 sync:config / sync:status / sync:key_versions）
//! snapshot   快照构建 / 内容哈希 / 合并策略 / 安全备份
//! http       重定向逐跳策略（跨源丢弃凭据）与 URL 脱敏
//! webdav     WebDAV 原语（PROPFIND / MKCOL / PUT / GET）
//! s3         S3 兼容存储原语（SigV4 手写签名 + PUT / GET / HEAD）
//! transport  协议无关的「读写一个文件」抽象
//! engine     上传 / 拉取 / 测试连接 / 状态（唯一的编排入口）
//! scheduler  常驻调度循环（复用 scheduled_tasks::task_state 的排期与退避）
//! ```
//!
//! ── 数据范围与安全边界 ────────────────────────────────────
//! 快照只含 FileStore KV 键值，排除请求 / 审计日志（`reqlog:` / `auditlog:`）
//! 与**云同步自己的配置 / 状态 / 版本表**——最后一条是硬约束：同步配置里有
//! 远端凭据的密文，把它传上云等于把「打开云盘的钥匙」放进云盘，且换机恢复时
//! 会被另一台机器的旧凭据覆盖。判定入口只有 [`snapshot::is_synced_key`] 一处。
//!
//! ── 凭据存放 ──────────────────────────────────────────────
//! WebDAV 密码 / S3 Secret 一律经 `account_session::secret::seal_secret`
//! （`enc1:` AES-256-GCM，密钥是本机 `account.key`）加密后落 KV；读取设置时
//! 只回 `has_password` / `has_secret_key`，绝不回显明文。明文只在发起请求的
//! 那一刻存在于内存。
//!
//! ── 同步锁 ────────────────────────────────────────────────
//! 上传与拉取共用一把 `tokio::sync::Mutex`：两者都读写同一份远端数据，并发时
//! 结果取决于谁后完成，用户点两下按钮就能得到最难解释的状态。锁跨 await 持有，
//! 因此必须是 tokio 的锁。
//!
//! ── 调度接入 ──────────────────────────────────────────────
//! 云同步的开关 / 间隔在 [`config::SyncConfig`] 里（单一真源），调度循环
//! [`scheduler::spawn`] 常驻；排期状态（上次尝试 / 下次执行 / 失败退避 / 在途
//! 租约）复用 `scheduled_tasks::task_state` 的持久化机制，键 `cloudSync`。

pub mod config;
pub mod engine;
pub mod http;
pub mod s3;
pub mod scheduler;
pub mod snapshot;
pub mod transport;
pub mod webdav;

use std::sync::OnceLock;

/// 全局同步锁（上传 / 拉取 / 调度循环共用）。
pub(crate) fn sync_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

/// sha256 → 小写十六进制。
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest as _, Sha256};
    hex::encode(Sha256::digest(bytes))
}

/// 毫秒时间戳。
pub(crate) fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// 本机设备名（只用于快照展示与排障，拿不到给 AIGX）。
pub(crate) fn device_name() -> String {
    for key in ["COMPUTERNAME", "HOSTNAME"] {
        if let Ok(value) = std::env::var(key) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return trimmed.chars().take(64).collect();
            }
        }
    }
    "AIGX".to_string()
}

/// 启动云同步调度循环（`main` 在 `scheduled_tasks::spawn` 之后调用一次）。
pub fn spawn(state: crate::api::openai::AppState) {
    scheduler::spawn(state);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_hex_matches_known_vector() {
        // SHA-256("abc") 的标准测试向量：钉住「小写十六进制 + 64 字符」口径
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
