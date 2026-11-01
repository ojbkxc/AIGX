//! 账号凭证的**真加密**存储 —— 移植 Agent2API 的 `core/crypto.rs`。
//!
//! ## 为什么不用现有的 `enc:` + base64
//!
//! 现状（`channel::Channel::encode_api_key`）是**可逆的 base64 混淆**，不是加密：
//! 密钥与密文同存于同一存储，攻击者读到存储文件就能还原明文。它的作用只是
//! 「不在浏览配置/备份/日志里直接看到明文」。
//!
//! Agent2API 的做法（本模块对齐）：**AES-256-GCM** + `enc1:` 信封 +
//! **32 字节随机密钥存在库外**（`account.key`）。存储文件被拷走也解不开 ——
//! 因为密钥不在同一个文件里。
//!
//! ## 信封格式
//!
//! `enc1:` + base64( nonce(12) ‖ 密文 ‖ tag(16) )
//!
//! 与 Agent2API **逐字节一致** —— 两边互相导入对方的账号记录时不用转码。
//!
//! ## 密钥文件
//!
//! - 位置：数据目录下 `account.key`（与库同目录，权限 0600）
//! - 不存在时**首次使用才生成**（纯读操作绝不生成 —— 见 [`key_fingerprint`]）
//! - 环境变量 `AIGX_ACCOUNT_KEY` 可覆盖（十六进制 64 字符）
//!
//! ## 与旧数据的兼容
//!
//! 存量 `enc:` 值照旧能读（[`open_secret`] 认两种前缀），但**新写入一律用
//! `enc1:`**。旧值在下次被保存时自动升级（[`seal_secret`] 只处理明文，
//! 迁移发生在 [`crate::account_session::store`] 的保存路径）。
//!
//! ## 密钥丢失的后果
//!
//! 账号凭证全部解不开（只剩渠道的 `api_key` 还能用，因为它是另一套路径）。
//! 所以密钥文件与数据库**必须一起备份**，且密钥文件不能进版本库。
//!
//! ## 全局目录 vs 显式目录
//!
//! 运行时（生产）由 [`set_data_dir`] 在启动时设定全局目录，调用方用 [`data_dir`]
//! 拿到当前值再传给本模块函数；测试则直接传独立临时目录，避免共享全局态。

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;

/// 信封前缀（与 Agent2API 一致）。
const ENVELOPE_PREFIX: &str = "enc1:";

/// 旧的可逆混淆前缀（读兼容，写不再产出）。
const LEGACY_PREFIX: &str = "enc:";

/// 密钥长度（AES-256）。
const KEY_LEN: usize = 32;

/// nonce 长度（GCM 标准 96 bit）。
const NONCE_LEN: usize = 12;

/// 密钥文件名（落在数据目录下）。
const KEY_FILE: &str = "account.key";

/// 密钥文件路径。
fn key_path(data_dir: &Path) -> PathBuf {
    data_dir.join(KEY_FILE)
}

/// 从环境变量读密钥（十六进制 64 字符 = 32 字节）。
fn env_key() -> Option<[u8; KEY_LEN]> {
    let hex = std::env::var("AIGX_ACCOUNT_KEY").ok()?;
    let hex = hex.trim();
    if hex.len() != KEY_LEN * 2 {
        return None;
    }
    let mut key = [0u8; KEY_LEN];
    for (i, byte) in key.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(key)
}

/// 取密钥（写入路径用）：环境变量 → 密钥文件（不存在则生成）。
///
/// 进程内不缓存：每次调用都走盘，避免「换密钥后旧密钥仍生效」。
/// 真实负载下这点 IO 相对账号存储本身的读写可忽略。
fn secret_key(data_dir: &Path) -> Result<[u8; KEY_LEN], String> {
    if let Some(key) = env_key() {
        return Ok(key);
    }
    let path = key_path(data_dir);
    match std::fs::read(&path) {
        Ok(bytes) if bytes.len() == KEY_LEN => {
            let mut key = [0u8; KEY_LEN];
            key.copy_from_slice(&bytes);
            Ok(key)
        }
        Ok(_) => Err(format!(
            "密钥文件 {} 长度异常（应为 {KEY_LEN} 字节）",
            path.display()
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => create_key_file(&path),
        Err(e) => Err(format!("读取密钥文件 {} 失败：{e}", path.display())),
    }
}

/// 取密钥（**纯读**路径用）：文件不存在则报错，绝不生成。
fn secret_key_readonly(data_dir: &Path) -> Result<[u8; KEY_LEN], String> {
    if let Some(key) = env_key() {
        return Ok(key);
    }
    let path = key_path(data_dir);
    let bytes =
        std::fs::read(&path).map_err(|e| format!("读取密钥文件 {} 失败：{e}", path.display()))?;
    if bytes.len() != KEY_LEN {
        return Err(format!(
            "密钥文件 {} 长度异常（应为 {KEY_LEN} 字节）",
            path.display()
        ));
    }
    let mut key = [0u8; KEY_LEN];
    key.copy_from_slice(&bytes);
    Ok(key)
}

/// 生成并写入密钥文件（0600）。
fn create_key_file(path: &Path) -> Result<[u8; KEY_LEN], String> {
    let mut key = [0u8; KEY_LEN];
    getrandom::getrandom(&mut key).map_err(|e| format!("系统随机源不可用：{e}"))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("创建数据目录失败：{e}"))?;
    }
    std::fs::write(path, key).map_err(|e| format!("写入密钥文件 {} 失败：{e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(key)
}

/// 只读地取当前生效的密钥指纹（十六进制前 16 位）。
///
/// **纯读**：密钥文件不存在时返回 `None`，**绝不生成** —— 一个「只是看看
/// 现在用哪把钥匙」的导出动作顺手造出 `account.key`，等于悄悄改变了本机的
/// 加密状态（下次保存账号用的就是这把「导出时生出来的」钥匙，语义错位）。
pub fn key_fingerprint(data_dir: &Path) -> Option<String> {
    if let Some(key) = env_key() {
        return Some(short_fingerprint(&key));
    }
    let bytes = std::fs::read(key_path(data_dir)).ok()?;
    if bytes.len() != KEY_LEN {
        return None;
    }
    let mut key = [0u8; KEY_LEN];
    key.copy_from_slice(&bytes);
    Some(short_fingerprint(&key))
}

/// `sha256(bytes)` 的十六进制前 16 位。
fn short_fingerprint(bytes: &[u8]) -> String {
    use sha2::{Digest as _, Sha256};
    let digest = Sha256::digest(bytes);
    let mut text = String::with_capacity(16);
    for byte in digest.iter().take(8) {
        use std::fmt::Write as _;
        let _ = write!(text, "{byte:02x}");
    }
    text
}

/// 加密明文 → `enc1:<base64(nonce‖密文+tag)>`。
///
/// nonce 每次现生成 —— GCM 的 nonce 重用是灾难性的（同一密钥下两条明文用
/// 同一 nonce 会互相泄底）。不用计数器：进程重启后计数没有持久化。
pub fn seal_secret(plaintext: &str, data_dir: &Path) -> Result<String, String> {
    use aes_gcm::aead::Aead as _;
    use aes_gcm::{Aes256Gcm, KeyInit as _, Nonce};

    let key = secret_key(data_dir)?;
    let cipher =
        Aes256Gcm::new_from_slice(&key).map_err(|_| "密钥长度不受 AES-256 支持".to_string())?;
    let mut nonce_bytes = [0u8; NONCE_LEN];
    getrandom::getrandom(&mut nonce_bytes).map_err(|e| format!("系统随机源不可用：{e}"))?;
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ciphertext = cipher
        .encrypt(nonce, plaintext.as_bytes())
        .map_err(|_| "AES-GCM 加密失败".to_string())?;
    let mut blob = Vec::with_capacity(NONCE_LEN + ciphertext.len());
    blob.extend_from_slice(&nonce_bytes);
    blob.extend_from_slice(&ciphertext);
    Ok(format!("{ENVELOPE_PREFIX}{}", BASE64.encode(&blob)))
}

/// 解开一段 `enc1:` 信封 → 明文（纯读路径，不生成密钥文件）。
fn open_envelope(value: &str, data_dir: &Path) -> Result<String, String> {
    use aes_gcm::aead::Aead as _;
    use aes_gcm::{Aes256Gcm, KeyInit as _, Nonce};

    let body = value
        .strip_prefix(ENVELOPE_PREFIX)
        .ok_or_else(|| "不是 enc1: 信封".to_string())?;
    let blob = BASE64
        .decode(body.trim())
        .map_err(|_| "信封不是合法 base64".to_string())?;
    if blob.len() < NONCE_LEN + 16 {
        return Err("信封长度异常".to_string());
    }
    let key = secret_key_readonly(data_dir)?;
    let cipher =
        Aes256Gcm::new_from_slice(&key).map_err(|_| "密钥长度不受 AES-256 支持".to_string())?;
    let nonce = Nonce::from_slice(&blob[..NONCE_LEN]);
    let plaintext = cipher
        .decrypt(nonce, &blob[NONCE_LEN..])
        .map_err(|_| "解密失败（密钥不对或数据被改过）".to_string())?;
    String::from_utf8(plaintext).map_err(|_| "解密结果不是合法 UTF-8".to_string())
}

/// 当前数据目录的全局 cell（仅生产运行时由 [`set_data_dir`] 设置一次）。
fn current_data_dir_cell() -> &'static Mutex<PathBuf> {
    static DIR: OnceLock<Mutex<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| Mutex::new(PathBuf::from(".")))
}

/// 当前数据目录的快照（公共访问器）。
///
/// 调用方（`Credential`）拿它传给本模块的加密/解密函数；测试可绕过它直接
/// 传独立临时目录。
pub fn data_dir() -> PathBuf {
    current_data_dir_cell()
        .lock()
        .map(|p| p.clone())
        .unwrap_or_else(|_| PathBuf::from("."))
}

/// 设定全局数据目录（启动时调用一次）。
///
/// 密钥文件落在**数据目录**下 —— 与库同目录，但不进版本库。
pub fn set_data_dir(dir: &Path) {
    if let Ok(mut guard) = current_data_dir_cell().lock() {
        *guard = dir.to_path_buf();
    }
}

/// 测试专用的跨模块互斥锁。
///
/// `data_dir` 是进程级全局态，cargo test 默认并行 —— `model` / `channel`
/// 等模块的测试若各自持私有锁再 `set_data_dir`，锁与锁之间不互斥，
/// 全局目录仍会被别的测试线程中途换掉（enc1: 用错密钥 → flaky）。
/// 所有需要「设目录 + 跑断言」的测试统一拿这一把锁。
#[cfg(test)]
pub(crate) fn test_lock() -> &'static Mutex<()> {
    static LOCK: Mutex<()> = Mutex::new(());
    &LOCK
}

/// 解密一段存储值 → 明文。
///
/// 认三种形态：
/// - `enc1:` → AES-256-GCM（本模块产出）
/// - `enc:`  → 旧的可逆 base64 混淆（存量兼容）
/// - 其他    → 原样返回（明文，尚未加密的存量）
pub fn open_secret(value: &str, data_dir: &Path) -> String {
    if value.starts_with(ENVELOPE_PREFIX) {
        return open_envelope(value, data_dir).unwrap_or_default();
    }
    if value.starts_with(LEGACY_PREFIX) {
        return crate::channel::Channel::decode_api_key_value(value);
    }
    value.to_string()
}

/// 加密一段明文 → 存储值。
///
/// 已是 `enc1:` 信封的原样返回（幂等）。
pub fn seal_or_keep(value: &str, data_dir: &Path) -> String {
    if value.is_empty() || value.starts_with(ENVELOPE_PREFIX) {
        return value.to_string();
    }
    match seal_secret(value, data_dir) {
        Ok(sealed) => sealed,
        // 加密失败时不能把明文写回存储 —— 那等于把「加密」变成摆设。
        // 退回到旧的可逆混淆，至少不是明文；并留下可见的失败痕迹。
        Err(_) => crate::channel::Channel::encode_api_key(value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("aigx-secret-{tag}-{}-{seq}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn seal_open_roundtrip() {
        let dir = temp_dir("roundtrip");
        let sealed = seal_secret("sk-plain-123456", &dir).unwrap();
        assert!(sealed.starts_with(ENVELOPE_PREFIX));
        assert!(!sealed.contains("sk-plain"));
        assert_eq!(open_secret(&sealed, &dir), "sk-plain-123456");
    }

    #[test]
    fn nonce_differs_per_call() {
        let dir = temp_dir("nonce");
        let a = seal_secret("same", &dir).unwrap();
        let b = seal_secret("same", &dir).unwrap();
        assert_ne!(a, b, "同一明文两次加密必须不同（nonce 复用会泄底）");
    }

    #[test]
    fn legacy_enc_still_readable() {
        let dir = temp_dir("legacy");
        let legacy = crate::channel::Channel::encode_api_key("sk-legacy");
        assert!(legacy.starts_with(LEGACY_PREFIX));
        assert_eq!(open_secret(&legacy, &dir), "sk-legacy");
    }

    #[test]
    fn plaintext_passes_through() {
        let dir = temp_dir("plain");
        assert_eq!(open_secret("sk-plain", &dir), "sk-plain");
    }

    #[test]
    fn key_file_created_once_and_fingerprint_stable() {
        let dir = temp_dir("keyfile");
        // 纯读（不存在）不生成
        assert!(key_fingerprint(&dir).is_none());
        assert!(!dir.join(KEY_FILE).exists(), "纯读不该生成密钥文件");
        // 首次加密生成
        let _ = seal_secret("x", &dir).unwrap();
        assert!(dir.join(KEY_FILE).exists());
        let fp = key_fingerprint(&dir).unwrap();
        assert_eq!(fp.len(), 16);
        // 稳定
        assert_eq!(key_fingerprint(&dir).unwrap(), fp);
    }
}
