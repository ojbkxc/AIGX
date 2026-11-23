// Ported from Agent2API v3.5.0-beta.7 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//! 本机客户端登录态读取 —— `desktop.credentials.read`（账号池「从桌面客户端导入」的宿主侧）。
//!
//! ── 语义 ────────────────────────────────────────────────────────────
//! 插件在某家 provider 的登录/导入界面上没有令牌，需要宿主去读**本机官方客户端**
//! 已经保存的登录态（workbuddy / raccoon / autoclaw / kiro / kuku / loomy …），
//! 读出的对象交给插件自己的解析器归一；宿主只负责「按固定来源读、按固定字段投影」。
//!
//! ── 边界（与上游同口径，逐条落实）──────────────────────────────────
//! 1. **来源固定**：路径写死在本模块的 `read`/各子模块里，`params` **不接受任何
//!    路径/文件名/SQL**——接受路径就是任意文件读取。
//! 2. **只读**：只做 `stat` + `read`（SQLite 用 `SQLITE_OPEN_READ_ONLY`）。
//! 3. **符号链接/重parse 拒收**：路径链上任一层是 symlink、Windows 上带
//!    reparse 位，一律判不可读。
//! 4. **大小上限**：单文件 ≤ 512 KiB、SQLite ≤ 64 MiB，超限即失败，不截断。
//! 5. **能力+声明双门**：能力 `desktop.credentials.v1` 且 `wasm.json` 的
//!    `helpers` 里确实声明了该 provider 的固定 helper id。
//! 6. **归属**：有账号时校验账号属于当前 provider；没有账号（新增导入）时要求
//!    本次调用处在 [`import_scope`] 内（账号新增/登录入口登记的作用域）。
//!
//! ── 与上游的差异（如实声明）────────────────────────────────────────
//! - 上游把结果落到 `account_store::plugin_accounts`，并读 `record.desktop` 标记
//!   区分「本机来源账号」；AIGX 的插件账号记录没有 `desktop` 字段，故改用
//!   「导入作用域 + 归属校验」两道判据，语义等价（同样只允许本次导入或已保存账号）。
//! - 上游 `autoclaw` / `doubao` 的 safeStorage/DPAPI 解密链是 Windows-only 的原生
//!   加密实现；AIGX 当前**没有** DPAPI 依赖，这两个来源在 R2 单独补齐（不是静默跳过：
//!   调用时如实报 `desktop_credentials_unavailable`）。
//! - 上游的续期（`refresh/*`）与 Cookie 文件解析（`cookie_file`）属于另一组能力
//!   （`desktop.refresh.*` / `resources.cookie-file.v1`），R2 补齐。

use std::io::Read;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::context::Context;
use super::wire::WireError;

mod astudio;
mod autoclaw;
mod codar;
mod commandcode;
mod doubao;
mod dumate;
mod kiro;
mod kuku;
mod loomy;
mod raccoon;
mod snapshots;

tokio::task_local! {
    /// 本次调用属于「新增账号导入」：值是 provider（空串 = 非导入）。
    static IMPORT_PROVIDER: String;
}

/// 在「新增账号导入」作用域里执行 `work`：插件在域内调用
/// `desktop.credentials.read` 时不需要账号记录。
pub(crate) async fn import_scope<F: std::future::Future>(
    provider: &str,
    selected: bool,
    work: F,
) -> F::Output {
    IMPORT_PROVIDER
        .scope(
            if selected {
                provider.to_string()
            } else {
                String::new()
            },
            work,
        )
        .await
}

/// 当前调用是否在 `provider` 的新增导入作用域内。
pub(crate) fn selected_import(provider: &str) -> bool {
    IMPORT_PROVIDER
        .try_with(|selected| !selected.is_empty() && selected == provider)
        .unwrap_or(false)
}

/// provider → 本机登录态的固定 helper id（与 `wasm.json` 的 `helpers` 对应）。
///
/// `None` = 该 provider 没有本机来源，插件不该声明对应 helper。
pub(crate) fn helper(provider: &str) -> Option<&'static str> {
    match provider {
        "workbuddy" => Some("desktop.credentials.workbuddy.v1"),
        "workbuddy-intl" => Some("desktop.credentials.workbuddy-intl.v1"),
        "raccoon" => Some("desktop.credentials.raccoon.v1"),
        "autoclaw" => Some("desktop.credentials.autoclaw.v1"),
        "autoclaw-intl" => Some("desktop.credentials.autoclaw-intl.v1"),
        "catpaw" => Some("desktop.credentials.catpaw.v1"),
        "doubaowork" => Some("desktop.credentials.doubaowork.v1"),
        "codar" => Some("desktop.credentials.codar.v1"),
        "commandcode" => Some("desktop.credentials.commandcode.v1"),
        "dumate" => Some("desktop.credentials.dumate.v1"),
        "quwork" => Some("desktop.credentials.quwork.v1"),
        "orcaterm" => Some("desktop.credentials.orcaterm.v1"),
        "astudio" => Some("desktop.credentials.astudio.v1"),
        "kiro" => Some("desktop.credentials.kiro.v1"),
        "cline-free" => Some("desktop.credentials.cline.v1"),
        "kuku" => Some("desktop.credentials.kuku.v1"),
        "loomy" => Some("desktop.credentials.loomy.v1"),
        _ => None,
    }
}

/// 能力 + 声明 + 归属三道门（README §边界 5/6）。
fn authorize(context: &Context) -> Result<(), WireError> {
    context.permission("desktop.credentials.v1")?;
    let provider = &context.shared.manifest.provider;
    let id = helper(provider).ok_or_else(invalid)?;
    if !context
        .shared
        .manifest
        .helpers
        .iter()
        .any(|value| value == id)
    {
        return Err(invalid());
    }
    // 已有账号：必须属于当前 provider（`account()` 内部做 provider 归属校验）。
    if !context.account.is_empty() {
        context.account()?;
        return Ok(());
    }
    // 没有账号：只允许新增导入作用域内的一次性读取。
    if selected_import(provider) {
        return Ok(());
    }
    Err(WireError::with_code(
        "本机读取只用于本次导入或已保存的本机账号",
        "forbidden",
    ))
}

/// `desktop.credentials.read` 入口：零参数，返回投影后的凭据内容。
pub(crate) async fn invoke(context: &Context, params: &Value) -> Result<Value, WireError> {
    authorize(context)?;
    if params.as_object().is_none_or(|fields| !fields.is_empty()) {
        return Err(invalid());
    }
    let provider = context.shared.manifest.provider.clone();
    let result = tokio::task::spawn_blocking(move || {
        if provider == "kiro" {
            kiro::read_source("kiro")
        } else {
            read(&provider)
        }
    })
    .await
    .map_err(|_| invalid())?;
    result
}

/// 按 provider 分发到固定来源读取器（路径不接受参数）。
pub(super) fn read(provider: &str) -> Result<Value, WireError> {
    match provider {
        "workbuddy" | "workbuddy-intl" => snapshots::workbuddy(provider),
        "raccoon" => raccoon::read(),
        "catpaw" => catpaw(),
        "codar" => codar::read(),
        "commandcode" => commandcode::read(),
        "dumate" => dumate::read(),
        "quwork" => snapshots::quwork(),
        "orcaterm" => snapshots::orcaterm(),
        "astudio" => astudio::read(),
        "kiro" => kiro::read(),
        "cline-free" => snapshots::cline(),
        "kuku" => kuku::read(),
        "loomy" => loomy::read(),
        // autoclaw / autoclaw-intl：safeStorage（DPAPI 解 key + AES-256-GCM 解密文）。
        "autoclaw" | "autoclaw-intl" => autoclaw::read(provider),
        // doubaowork：客户端 Cookie 库 + DPAPI（Windows-only，其余平台如实失败）。
        "doubaowork" => doubao::read(),
        _ => Err(invalid()),
    }
}

/// 美团猫爪：`~/.meituan-catpaw/auth.json` 的固定字段投影（明文 JSON）。
fn catpaw() -> Result<Value, WireError> {
    let path = home()?.join(".meituan-catpaw/auth.json");
    let value = object(&path, 64 * 1024)?;
    let auth = value["auth"].as_object().ok_or_else(invalid)?;
    let account = value["account"].as_object().ok_or_else(invalid)?;
    if let Some(kind) = auth.get("loginType").filter(|value| !value.is_null()) {
        let kind = kind.as_str().ok_or_else(invalid)?.trim();
        if !kind.is_empty() && !kind.eq_ignore_ascii_case("passport") {
            return Err(invalid());
        }
    }
    let token = catpaw_field(auth.get("accessToken"), 8192)?;
    let uid = catpaw_field(account.get("uid"), 256)?;
    let login = catpaw_field(account.get("loginName"), 256)?;
    let user = if uid.is_empty() { &login } else { &uid };
    if token.is_empty()
        || user.is_empty()
        || user == "desktop-auth"
        || user.len() > 512
        || [&uid, &login].iter().any(|value| value.contains(&token))
    {
        return Err(invalid());
    }
    Ok(json!({
        "accessToken": token,
        "uid": user,
        "loginName": login,
        "loginType": "passport"
    }))
}

fn catpaw_field(value: Option<&Value>, maximum: usize) -> Result<String, WireError> {
    let raw = match value {
        None | Some(Value::Null) => return Ok(String::new()),
        Some(Value::String(text)) => text.clone(),
        Some(Value::Number(number)) if number.is_i64() || number.is_u64() => number.to_string(),
        _ => return Err(invalid()),
    };
    let text = raw.trim();
    if text.chars().count() > maximum
        || text.contains(';')
        || text.chars().any(char::is_control)
        || reqwest::header::HeaderValue::from_str(text).is_err()
    {
        return Err(invalid());
    }
    Ok(text.to_string())
}

/// 用户 home（绝对路径才算数；相对路径直接判无效）。
pub(super) fn home() -> Result<PathBuf, WireError> {
    #[cfg(windows)]
    let name = "USERPROFILE";
    #[cfg(not(windows))]
    let name = "HOME";
    absolute_env(name).ok_or_else(invalid)
}

fn absolute_env(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

/// 客户端配置根目录（平台固定，不读插件参数）。
///
/// 写成「各平台各一个 cfg 表达式」而不是 `return`：clippy 的 `needless_return`
/// 会盯上「函数体只有一条 return」的形态，而这函数每个平台恰好只有一条。
fn app_data() -> Result<PathBuf, WireError> {
    #[cfg(windows)]
    {
        absolute_env("APPDATA")
            .or_else(|| home().ok().map(|path| path.join("AppData/Roaming")))
            .ok_or_else(invalid)
    }
    #[cfg(target_os = "macos")]
    {
        Ok(home()?.join("Library/Application Support"))
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        absolute_env("XDG_CONFIG_HOME")
            .or_else(|| home().ok().map(|path| path.join(".config")))
            .ok_or_else(invalid)
    }
}

/// 常规文件校验：路径链上不许有 symlink / Windows reparse 点，目标必须是普通文件。
pub(super) fn regular(path: &Path) -> Result<std::fs::Metadata, WireError> {
    let mut parent = Some(path);
    while let Some(current) = parent {
        let meta = std::fs::symlink_metadata(current).map_err(|_| invalid())?;
        if meta.file_type().is_symlink() {
            return Err(invalid());
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if meta.file_attributes() & 0x400 != 0 {
                return Err(invalid());
            }
        }
        parent = current.parent();
        if parent.is_some_and(|parent| parent.as_os_str().is_empty()) {
            break;
        }
    }
    let meta = std::fs::metadata(path).map_err(|_| invalid())?;
    if !meta.is_file() {
        return Err(invalid());
    }
    Ok(meta)
}

/// 读文件（大小上限内一次读完，超限即失败）。
pub(super) fn file(path: &Path, maximum: u64) -> Result<Vec<u8>, WireError> {
    regular(path)?;
    let file = std::fs::File::open(path).map_err(|_| invalid())?;
    let meta = file.metadata().map_err(|_| invalid())?;
    if !meta.is_file() || meta.len() == 0 || meta.len() > maximum {
        return Err(invalid());
    }
    let mut bytes = Vec::new();
    file.take(maximum + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| invalid())?;
    if bytes.len() as u64 > maximum {
        return Err(invalid());
    }
    Ok(bytes)
}

/// 读 JSON 对象文件。
pub(super) fn object(path: &Path, maximum: u64) -> Result<Value, WireError> {
    let bytes = file(path, maximum)?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
    if !value.is_object() {
        return Err(invalid());
    }
    Ok(value)
}

/// 只投影白名单字段（不返回未筛选的嵌套 JSON）。
pub(super) fn project(value: &Value, keys: &[&str]) -> Value {
    let mut result = json!({});
    for key in keys {
        if let Some(field) = value.get(*key) {
            result[*key] = field.clone();
        }
    }
    result
}

/// 把投影结果包成上游同形的 `{sourceId, sourceRevision, content}`。
pub(super) fn content(path: &Path, value: Value) -> Result<Value, WireError> {
    let content = serde_json::to_string(&value).map_err(|_| invalid())?;
    if content.len() > 256 * 1024 {
        return Err(invalid());
    }
    Ok(json!({
        "sourceId": digest(path.to_string_lossy().as_bytes()),
        "sourceRevision": digest(content.as_bytes()),
        "content": content
    }))
}

pub(super) fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// 只读打开客户端 SQLite（含 `-wal` / `-shm` 的 symlink 校验）。
pub(super) fn database(path: &Path) -> Result<rusqlite::Connection, WireError> {
    if regular(path)?.len() > 64 * 1024 * 1024 {
        return Err(invalid());
    }
    for suffix in ["-wal", "-shm"] {
        let sidecar = PathBuf::from(format!("{}{suffix}", path.to_string_lossy()));
        if sidecar.exists() {
            regular(&sidecar)?;
        }
    }
    let db = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| invalid())?;
    db.busy_timeout(std::time::Duration::from_secs(2))
        .map_err(|_| invalid())?;
    Ok(db)
}

/// 统一失败形状：本机登录态不可读/未登录/格式不支持。
pub(super) fn invalid() -> WireError {
    WireError::with_code(
        "本机客户端登录态不可读、未登录或格式不支持，请先在官方客户端登录，或改用网页登录和手工凭据",
        "desktop_credentials_unavailable",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// helper 表的 provider 全集（新增/删除 provider 时这里必须同步改，否则测试红）。
    const PROVIDERS: &[&str] = &[
        "workbuddy",
        "workbuddy-intl",
        "raccoon",
        "autoclaw",
        "autoclaw-intl",
        "catpaw",
        "doubaowork",
        "codar",
        "commandcode",
        "dumate",
        "quwork",
        "orcaterm",
        "astudio",
        "kiro",
        "cline-free",
        "kuku",
        "loomy",
    ];

    #[test]
    fn helper_covers_every_supported_provider() {
        for provider in PROVIDERS {
            let id = helper(provider)
                .unwrap_or_else(|| panic!("provider {provider} 必须有固定 helper id"));
            assert!(
                id.starts_with("desktop.credentials."),
                "{provider} 的 helper id 命名越界: {id}"
            );
            assert!(id.ends_with(".v1"), "{provider} 的 helper id 缺版本段: {id}");
        }
        assert_eq!(helper("qwen"), None, "未知 provider 不能有 helper");
        assert_eq!(helper(""), None);
    }

    #[test]
    fn dispatch_rejects_unknown_provider_with_unavailable_code() {
        let error = read("qwen").err().expect("未知 provider 必须失败");
        assert_eq!(error.code.as_deref(), Some("desktop_credentials_unavailable"));
        // 大小写不敏感不能放宽：分派只认注册表里的小写 id。
        assert!(read("Kiro").is_err(), "provider 必须精确匹配");
    }

    #[test]
    fn project_only_keeps_requested_keys() {
        let value = json!({"a": 1, "b": {"c": 2}, "d": [1, 2]});
        let projected = project(&value, &["b", "missing", "a"]);
        assert_eq!(projected, json!({"b": {"c": 2}, "a": 1}));
        // 缺失键不进结果集：不能把 null 塞进投影（上游同口径）。
        assert!(projected.get("missing").is_none());
    }

    #[test]
    fn content_envelope_matches_upstream_shape() {
        let path = Path::new("C:/fixture/auth.json");
        let envelope = content(path, json!({"token": "t"})).expect("投影可序列化");
        let expected_revision = digest(b"{\"token\":\"t\"}");
        assert_eq!(
            envelope["sourceId"],
            json!(digest(path.to_string_lossy().as_bytes()))
        );
        assert_eq!(envelope["sourceRevision"], json!(expected_revision));
        let inner: Value =
            serde_json::from_str(envelope["content"].as_str().unwrap_or("")).expect("content 是 JSON");
        assert_eq!(inner, json!({"token": "t"}));
    }

    #[test]
    fn object_reads_only_regular_files_within_limit() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("auth.json");
        std::fs::write(&path, br#"{"auth":{"accessToken":"t"}}"#).expect("write");
        let value = object(&path, 64 * 1024).expect("合法 JSON 对象可读");
        assert_eq!(value["auth"]["accessToken"], json!("t"));

        // 上限外的文件整份拒收（不截断后当成功）。
        assert!(object(&path, 8).is_err(), "超过上限必须失败");
        // 顶层不是对象同样拒收。
        let array = dir.path().join("array.json");
        std::fs::write(&array, b"[1,2]").expect("write");
        assert!(object(&array, 64 * 1024).is_err(), "顶层数组必须失败");
        // 空文件拒收。
        let empty = dir.path().join("empty.json");
        std::fs::write(&empty, b"").expect("write");
        assert!(object(&empty, 64 * 1024).is_err(), "空文件必须失败");
    }

    #[cfg(unix)]
    #[test]
    fn regular_rejects_symlinked_sources() {
        let dir = tempfile::tempdir().expect("temp dir");
        let target = dir.path().join("auth.json");
        std::fs::write(&target, b"{}").expect("write");
        let link = dir.path().join("link.json");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");
        assert!(regular(&link).is_err(), "路径链上有 symlink 必须拒绝");
        assert!(regular(&target).is_ok(), "常规文件必须通过");
    }

    #[test]
    fn catpaw_rejects_tokens_with_header_metacharacters() {
        // `;` 与换行会污染 Cookie 头，取字段阶段就要拒（不是留给上层）。
        let header_unsafe = json!("token;injected");
        assert!(catpaw_field(Some(&header_unsafe), 8192).is_err());
        let newline = json!("token\nnext");
        assert!(catpaw_field(Some(&newline), 8192).is_err());
        // 数字型 uid 允许（上游同口径），超长直接失败。
        assert_eq!(catpaw_field(Some(&json!(42)), 256).expect("uid"), "42");
        assert!(catpaw_field(Some(&json!("x".repeat(300))), 256).is_err());
    }

    #[tokio::test]
    async fn import_scope_is_scoped_to_one_provider_and_one_call() {
        // 作用域外：任何 provider 都不算导入。
        assert!(!selected_import("kiro"));
        let inside = import_scope("kiro", true, async {
            (
                selected_import("kiro"),
                selected_import("raccoon"),
                selected_import(""),
            )
        })
        .await;
        assert_eq!(inside, (true, false, false));
        // 作用域退出后立刻失效：不是常开开关。
        assert!(!selected_import("kiro"));
        // selected=false 时即使调用 import_scope 也不放行。
        let disabled = import_scope("kiro", false, async { selected_import("kiro") }).await;
        assert!(!disabled);
    }
}
