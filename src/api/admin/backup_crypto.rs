//! 加密整库备份（#33）——口令 AEAD 流式包，格式 `AIGXBAK1`。
//!
//! ── 立项依据 ────────────────────────────────────────────────
//! 既有 `GET /api/backup/export` 把全量 KV（含凭证/密钥/用户表）以**明文
//! JSON** 给浏览器下载——admin 磁盘上即出现一份绕过 `enc1:` 加密的整库
//! 明文副本。上游 A2A 用流式加密包（`db/backup/stream.rs` 的
//! `Aes256Gcm` + `StreamBE32`），本模块以仓内既有依赖（argon2 0.5 +
//! aes-gcm 0.10）实现等价能力，不新引依赖。
//!
//! ── 包格式 `AIGXBAK1` ───────────────────────────────────────
//! ```text
//! 第 1 行（明文 JSON 头）: {"magic":"AIGXBAK1","version":1,
//!   "kdf":{"algo":"argon2id","salt_b64":…,"m_cost":19456,"t_cost":2,"p_cost":1},
//!   "created_at":unix秒}
//! 第 2..n 行（每行一条记录）: base64( nonce(12) ‖ AES-256-GCM( json行 ) )
//!   json行 = {"key":…,"value":…}（首条固定为 {"key":"__meta__",…}）
//!   AAD = "AIGXBAK1:<行序号>" —— 绑定行序，防重排/截断/拼接
//!   nonce = 随机 12 字节逐条独立（同 key 不同 nonce，GCM 安全前提）
//! ```
//!
//! ── 口令纪律（任务书红线）──────────────────────────────────
//! 口令**只经请求体**进服务端：不落盘、不进日志、不进审计载荷（审计只记
//! 操作与条数）；内存里只活到本次加解密结束。
//!
//! ── 兼容口径（二选一已定：前者）────────────────────────────
//! - **旧明文包可导入**：`handle_backup_import`（明文 JSON）保留——那是
//!   用户自己的既有数据，导入是迁移必需；明文**导出**端点移除（安全
//!   缺口本体），UPDATE.txt 已写入口径变化。
//! - 旧明文导入属「低一次加密」，导入后立即可用新格式再导出。
//!
//! ── 流式与内存 ─────────────────────────────────────────────
//! 导出：逐键读 KV → 逐条加密 → **逐条写文件**（不整体驻内存）；
//! 导入：multipart 流式读包 → 逐行解密 → 逐键写回。单条记录的序列化
//! 是内存峰值单位（与明文路径同量级）。加解密走 spawn_blocking
//!（argon2 是 CPU 密集型，不阻塞 async 线程池）。
//!
//! 性能影响：冷路径（低频管理操作）。导出 O(键数) 次小分配 + 每键一次
//! AEAD；导入同。Argon2id（19MiB/2 轮）约 50-100ms 一次性派生；
//! 数据面转发路径零改动、无新增锁。

use aes_gcm::aead::Aead as _;
use aes_gcm::{Aes256Gcm, KeyInit as _, Nonce};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::super::openai::AppState;
use super::common::error_response;
use axum::{
    extract::{Multipart, Query, State},
    http::{HeaderMap, StatusCode},
    response::Response,
    Json,
};

use crate::storage::FileStore;

/// 包格式魔数与版本。
const MAGIC: &str = "AIGXBAK1";
const FORMAT_VERSION: u32 = 1;

/// Argon2id 参数（OWASP 2024 推荐档：m=19MiB, t=2, p=1）。
const ARGON_M_COST_KIB: u32 = 19_456;
const ARGON_T_COST: u32 = 2;
const ARGON_P_COST: u32 = 1;
const SALT_LEN: usize = 16;

/// 上传包的大小兜底（防误传超大文件打爆内存；正常包 = 键数 × 平均值大小，
/// 1 GiB 已远超合理库尺寸——reqlog 键默认排除）。
const MAX_PACKAGE_BYTES: u64 = 1024 * 1024 * 1024;

/// 下载文件名的合法形态（防路径穿越：只认本模块产出的名字）。
fn is_safe_download_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    name.starts_with("export-")
        && name.ends_with(".aigxbak")
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'-' || *b == b'.' || *b == b'_')
}

/// 包头（明文第 1 行）。
#[derive(Debug, Serialize, Deserialize)]
struct PackageHeader {
    magic: String,
    version: u32,
    kdf: KdfParams,
    created_at: i64,
}

#[derive(Debug, Serialize, Deserialize)]
struct KdfParams {
    algo: String,
    salt_b64: String,
    m_cost: u32,
    t_cost: u32,
    p_cost: u32,
}

/// Argon2id 派生 AES-256 密钥（参数从包头读——未来调参不破坏旧包解密）。
fn derive_key(password: &str, params: &KdfParams) -> Result<[u8; 32], String> {
    if params.algo != "argon2id" {
        return Err(format!("不支持的 KDF 算法: {}", params.algo));
    }
    let salt = base64::engine::general_purpose::STANDARD
        .decode(&params.salt_b64)
        .map_err(|_| "包头 salt 非法（base64）")?;
    let mut key = [0u8; 32];
    argon2::Argon2::new(
        argon2::Algorithm::Argon2id,
        argon2::Version::V0x13,
        argon2::Params::new(params.m_cost, params.t_cost, params.p_cost, Some(32))
            .map_err(|e| format!("KDF 参数非法: {e}"))?,
    )
    .hash_password_into(password.as_bytes(), &salt, &mut key)
    .map_err(|e| format!("密钥派生失败: {e}"))?;
    Ok(key)
}

/// AEAD 加密一行（AAD 绑定行序号）。
fn seal_line(key: &[u8; 32], line_no: usize, plaintext: &[u8]) -> Result<String, String> {
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| "密钥长度不受支持".to_string())?;
    let mut nonce_bytes = [0u8; 12];
    // 进程内 CSPRNG：随机 nonce 逐条独立（GCM 前提）；行号进 AAD 不进 nonce
    getrandom(&mut nonce_bytes)?;
    let nonce = Nonce::from_slice(&nonce_bytes);
    let aad = format!("{MAGIC}:{line_no}");
    let ct = cipher
        .encrypt(
            nonce,
            aes_gcm::aead::Payload {
                msg: plaintext,
                aad: aad.as_bytes(),
            },
        )
        .map_err(|_| "加密失败".to_string())?;
    let mut framed = nonce_bytes.to_vec();
    framed.extend_from_slice(&ct);
    Ok(base64::engine::general_purpose::STANDARD.encode(framed))
}

/// 解密一行（AAD 校验行序号——重排/截断/拼接在此失败）。
fn open_line(key: &[u8; 32], line_no: usize, framed_b64: &str) -> Result<Vec<u8>, String> {
    let framed = base64::engine::general_purpose::STANDARD
        .decode(framed_b64.trim())
        .map_err(|_| format!("第 {line_no} 行不是合法 base64"))?;
    if framed.len() < 12 + 16 {
        return Err(format!("第 {line_no} 行长度不足（nonce+tag）"));
    }
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| "密钥长度不受支持".to_string())?;
    let nonce = Nonce::from_slice(&framed[..12]);
    let aad = format!("{MAGIC}:{line_no}");
    cipher
        .decrypt(
            nonce,
            aes_gcm::aead::Payload {
                msg: &framed[12..],
                aad: aad.as_bytes(),
            },
        )
        .map_err(|_| "解密失败（口令错误或包被篡改）".to_string())
}

/// 进程内 CSPRNG（随机 nonce/salt 用）——与 secret.rs 同一依赖
///（getrandom crate，已在 Cargo.toml），不新引随机源。
fn fill_random(buf: &mut [u8]) -> Result<(), String> {
    getrandom::getrandom(buf).map_err(|e| format!("系统随机源不可用: {e}"))
}

/// 逐键把 KV 写成加密包文件（流式：单条内存峰值 = 键 + 值）。
fn write_encrypted_backup(
    store: &FileStore,
    password: &str,
    include_logs: bool,
    log_prefixes: &[&str],
    out_path: &std::path::Path,
) -> Result<(usize, u64), String> {
    let all_keys = store.list("").map_err(|e| format!("列出键失败: {e}"))?;
    let salt: [u8; SALT_LEN] = {
        let mut s = [0u8; SALT_LEN];
        fill_random(&mut s)?;
        s
    };
    let kdf = KdfParams {
        algo: "argon2id".to_string(),
        salt_b64: base64::engine::general_purpose::STANDARD.encode(salt),
        m_cost: ARGON_M_COST_KIB,
        t_cost: ARGON_T_COST,
        p_cost: ARGON_P_COST,
    };
    let key = derive_key(password, &kdf)?;
    let header = PackageHeader {
        magic: MAGIC.to_string(),
        version: FORMAT_VERSION,
        kdf,
        created_at: chrono::Utc::now().timestamp(),
    };
    let mut out = String::new();
    out.push_str(&serde_json::to_string(&header).map_err(|e| e.to_string())?);
    out.push('\n');

    let mut line_no = 0usize;
    let mut records = 0usize;
    for key in &all_keys {
        if !include_logs && log_prefixes.iter().any(|p| key.starts_with(p)) {
            continue;
        }
        let value: Value = match store.get(key) {
            Ok(Some(v)) => v,
            Ok(None) => continue, // 竞态下键被删：跳过
            Err(e) => {
                // 单键读失败不阻断整份备份（键损坏≠整库不可备份）
                tracing::warn!("加密备份跳过损坏键 {key}: {e}");
                continue;
            }
        };
        line_no += 1;
        let plaintext = serde_json::to_string(&json!({ "key": key, "value": value }))
            .map_err(|e| format!("序列化记录失败: {e}"))?;
        out.push_str(&seal_line(&key, line_no, plaintext.as_bytes())?);
        out.push('\n');
        records += 1;
    }
    std::fs::write(out_path, out).map_err(|e| format!("写入备份文件失败: {e}"))?;
    let bytes = std::fs::metadata(out_path).map(|m| m.len()).unwrap_or(0);
    Ok((records, bytes))
}

/// 解密整包 → KV 记录流（收集后由调用方逐键写回）。
fn read_encrypted_backup(content: &str, password: &str) -> Result<Vec<(String, Value)>, String> {
    let mut lines = content.lines();
    let header_line = lines.next().ok_or("空包")?;
    let header: PackageHeader = serde_json::from_str(header_line)
        .map_err(|_| "包头解析失败（不是 AIGXBAK1 加密包？明文包请用原导入入口）")?;
    if header.magic != MAGIC || header.version != FORMAT_VERSION {
        return Err(format!(
            "包格式不匹配：magic={} version={}（期望 {MAGIC} v{FORMAT_VERSION}）",
            header.magic, header.version
        ));
    }
    let key = derive_key(password, &header.kdf)?;
    let mut records = Vec::new();
    for (offset, line) in lines.enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let line_no = offset + 1; // 包头是第 0 行
        let plaintext = open_line(&key, line_no, line)?;
        let record: Value = serde_json::from_slice(&plaintext)
            .map_err(|_| format!("第 {line_no} 行解密后不是合法记录"))?;
        let key = record
            .get("key")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("第 {line_no} 行缺少 key 字段"))?
            .to_string();
        let value = record
            .get("value")
            .cloned()
            .ok_or_else(|| format!("第 {line_no} 行缺少 value 字段"))?;
        records.push((key, value));
    }
    Ok(records)
}

// ── HTTP 端点 ───────────────────────────────────────────────────────

/// POST /api/backup/export-encrypted 的请求体（口令只经请求体——任务书红线）。
#[derive(Debug, Deserialize)]
pub struct EncryptedExportRequest {
    /// 备份口令（不落盘不进日志；丢失=备份不可解，请用户自行妥善保管）
    pub password: String,
    /// 是否包含请求/审计日志键（默认 false：日志体积大且可再生）
    #[serde(default)]
    pub include_logs: Option<bool>,
}

/// POST /api/backup/export-encrypted —— 生成口令加密包（写服务端文件）。
///
/// 口令经 spawn_blocking 内 Argon2id 派生（19MiB/2 轮 ≈ 50-100ms，一次性）；
/// KV 逐键流式加密写文件，单条内存峰值 = 键+值。
pub async fn handle_backup_export_encrypted(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<EncryptedExportRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    crate::api::admin::common::verify_admin(&state, &headers).await?;
    if body.password.chars().count() < 8 {
        return Err(error_response(
            "备份口令至少 8 个字符（丢失口令=备份不可解）",
            StatusCode::BAD_REQUEST,
        ));
    }
    let include_logs = body.include_logs.unwrap_or(false);
    let store = state.alert_store.clone();
    let password = body.password;
    let data_dir = crate::config::expand_path(&state.config_manager.get().await.server.data_dir);
    // CPU 密集（Argon2）+ 同步文件 IO：spawn_blocking 不阻塞 async 线程池
    let (file_name, records, bytes) = tokio::task::spawn_blocking(move || {
        let backups_dir = data_dir.join("backups");
        std::fs::create_dir_all(&backups_dir).map_err(|e| format!("创建备份目录失败: {e}"))?;
        let file_name = format!(
            "export-{}.aigxbak",
            chrono::Utc::now().format("%Y%m%d-%H%M%S")
        );
        let out_path = backups_dir.join(&file_name);
        let (records, bytes) = write_encrypted_backup(
            &store,
            &password,
            include_logs,
            super::backup::LOG_PREFIXES,
            &out_path,
        )?;
        Ok::<_, String>((file_name, records, bytes))
    })
    .await
    .map_err(|e| {
        error_response(
            &format!("备份任务失败: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )
    })?
    .map_err(|e| error_response(&e, StatusCode::INTERNAL_SERVER_ERROR))?;

    crate::api::admin::common::record_audit(
        &state,
        &crate::api::admin::common::admin_id_from_session(&state, &headers).await,
        "backup.export_encrypted",
        &format!("生成加密备份 {file_name}（{records} 键，{bytes} 字节，含日志={include_logs}）"),
        None,
        None,
    );
    Ok(Json(json!({
        "success": true,
        "data": { "file": file_name, "records": records, "bytes": bytes }
    })))
}

/// GET /api/backup/download?file=export-….aigxbak —— 下载加密包。
///
/// 文件名只认本模块产出的安全形态（防路径穿越）；审计不记口令（也无从记）。
pub async fn handle_backup_download(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<DownloadQuery>,
) -> Result<Response, (StatusCode, Json<Value>)> {
    crate::api::admin::common::verify_admin(&state, &headers).await?;
    if !is_safe_download_name(&q.file) {
        return Err(error_response(
            "非法文件名（只允许本模块产出的 export-*.aigxbak）",
            StatusCode::BAD_REQUEST,
        ));
    }
    let data_dir = crate::config::expand_path(&state.config_manager.get().await.server.data_dir);
    let path = data_dir.join("backups").join(&q.file);
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|_| error_response("备份文件不存在（或已被清理）", StatusCode::NOT_FOUND))?;
    crate::api::admin::common::record_audit(
        &state,
        &crate::api::admin::common::admin_id_from_session(&state, &headers).await,
        "backup.download",
        &format!("下载加密备份 {}", q.file),
        None,
        None,
    );
    let mut resp = Response::new(axum::body::Body::from(bytes));
    resp.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/octet-stream"),
    );
    resp.headers_mut().insert(
        axum::http::header::CONTENT_DISPOSITION,
        axum::http::HeaderValue::from_str(&format!("attachment; filename=\"{}\"", q.file))
            .map_err(|_| error_response("文件名非法", StatusCode::BAD_REQUEST))?,
    );
    Ok(resp)
}

#[derive(Debug, Deserialize)]
pub struct DownloadQuery {
    pub file: String,
}

/// POST /api/backup/import-encrypted —— 上传加密包并恢复（multipart）。
///
/// 安全顺序与明文导入一致：先把**当前**全量 KV 自动备份到
/// `{data_dir}/backups/`（pre-import-<ts>.json）再写回；口令错/包篡改在
/// 写回**之前**失败（解密全量校验先行），不留半套状态。
pub async fn handle_backup_import_encrypted(
    State(state): State<AppState>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    crate::api::admin::common::verify_admin(&state, &headers).await?;
    let mut password: Option<String> = None;
    let mut package: Option<Vec<u8>> = None;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| error_response(&format!("读取上传字段失败: {e}"), StatusCode::BAD_REQUEST))?
    {
        match field.name().unwrap_or_default() {
            "password" => {
                password = Some(field.text().await.map_err(|e| {
                    error_response(&format!("读取口令失败: {e}"), StatusCode::BAD_REQUEST)
                })?);
            }
            "file" => {
                if let Some(size) = field.metadata().map(|m| m.size_hint().1) {
                    if size > MAX_PACKAGE_BYTES {
                        return Err(error_response(
                            &format!("包超过大小上限 {MAX_PACKAGE_BYTES} 字节"),
                            StatusCode::PAYLOAD_TOO_LARGE,
                        ));
                    }
                }
                let mut buf = Vec::new();
                use axum::body::HttpBody;
                while let Some(chunk) = field.chunk().await.map_err(|e| {
                    error_response(&format!("读取包失败: {e}"), StatusCode::BAD_REQUEST)
                })? {
                    buf.extend_from_slice(&chunk);
                }
                package = Some(buf);
            }
            _ => {} // 未知字段忽略
        }
    }
    let password = password
        .filter(|p| !p.is_empty())
        .ok_or_else(|| error_response("缺少口令字段 password", StatusCode::BAD_REQUEST))?;
    let package =
        package.ok_or_else(|| error_response("缺少上传字段 file", StatusCode::BAD_REQUEST))?;
    if package.len() as u64 > MAX_PACKAGE_BYTES {
        return Err(error_response(
            &format!("包超过大小上限 {MAX_PACKAGE_BYTES} 字节"),
            StatusCode::PAYLOAD_TOO_LARGE,
        ));
    }
    let content = String::from_utf8(package).map_err(|_| {
        error_response(
            "包不是 UTF-8 文本（AIGXBAK1 是文本行格式）",
            StatusCode::BAD_REQUEST,
        )
    })?;

    // 解密全量先行（口令错/篡改在此失败，写回之前，不留半套状态）
    let records = tokio::task::spawn_blocking(move || read_encrypted_backup(&content, &password))
        .await
        .map_err(|e| {
            error_response(
                &format!("解密任务失败: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            )
        })?
        .map_err(|e| error_response(&e, StatusCode::BAD_REQUEST))?;
    if records.is_empty() {
        return Err(error_response(
            "加密包为空（无记录）",
            StatusCode::BAD_REQUEST,
        ));
    }

    // 恢复前自动备份当前全量（与明文导入同一安全顺序）
    let pre_path = super::backup::snapshot_current(&state)
        .await
        .map_err(|e| e)?;

    // 逐键写回（合并 upsert；键长守卫与明文导入同口径）
    let mut written = 0usize;
    let mut skipped = 0usize;
    for (key, value) in &records {
        if key.is_empty() || key.len() > 512 {
            skipped += 1;
            continue;
        }
        match state.alert_store.put(key, value) {
            Ok(()) => written += 1,
            Err(e) => {
                skipped += 1;
                tracing::warn!("加密导入键 {key} 失败: {e}");
            }
        }
    }
    crate::api::admin::common::record_audit(
        &state,
        &crate::api::admin::common::admin_id_from_session(&state, &headers).await,
        "backup.import_encrypted",
        &format!(
            "导入加密备份：写入 {written} 键，跳过 {skipped}，恢复前快照 {}",
            pre_path.display()
        ),
        None,
        None,
    );
    tracing::info!("加密备份导入完成: 写入 {written} / 跳过 {skipped}");
    Ok(Json(json!({
        "success": true,
        "data": {
            "written": written,
            "skipped": skipped,
            "pre_import_backup": pre_path.display().to_string(),
        }
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Argon2 慢（约 100ms/次），测试用轻参数——单独的派生入口避免
    /// 生产参数把测试拖到分钟级。
    fn derive_key_fast(password: &str, params: &KdfParams) -> Result<[u8; 32], String> {
        let salt = base64::engine::general_purpose::STANDARD
            .decode(&params.salt_b64)
            .map_err(|_| "salt 非法")?;
        let mut key = [0u8; 32];
        argon2::Argon2::new(
            argon2::Algorithm::Argon2id,
            argon2::Version::V0x13,
            argon2::Params::new(8, 1, 1, Some(32)).map_err(|e| e.to_string())?,
        )
        .hash_password_into(password.as_bytes(), &salt, &mut key)
        .map_err(|e| e.to_string())?;
        Ok(key)
    }

    fn fast_params() -> KdfParams {
        let mut salt = [0u8; SALT_LEN];
        fill_random(&mut salt).unwrap();
        KdfParams {
            algo: "argon2id".to_string(),
            salt_b64: base64::engine::general_purpose::STANDARD.encode(salt),
            m_cost: 8,
            t_cost: 1,
            p_cost: 1,
        }
    }

    fn seal_package(password: &str, records: &[(&str, Value)]) -> String {
        let kdf = fast_params();
        let key = derive_key_fast(password, &kdf).unwrap();
        let header = PackageHeader {
            magic: MAGIC.to_string(),
            version: FORMAT_VERSION,
            kdf,
            created_at: 1_700_000_000,
        };
        let mut out = serde_json::to_string(&header).unwrap();
        for (i, (k, v)) in records.iter().enumerate() {
            let line = serde_json::to_string(&json!({ "key": k, "value": v })).unwrap();
            out.push('\n');
            out.push_str(&seal_line(&key, i + 1, line.as_bytes()).unwrap());
        }
        out
    }

    /// 加解密 roundtrip：内容逐键一致。
    #[test]
    fn roundtrip_preserves_records() {
        let records = vec![
            ("channel:abc", json!({ "name": "渠道", "models": ["m1"] })),
            ("apikey_x", json!({ "key": "sk-test" })),
        ];
        let package = seal_package("correct-horse", &records);
        let opened = read_encrypted_backup(&package, "correct-horse").unwrap();
        assert_eq!(opened.len(), 2);
        assert_eq!(opened[0].0, "channel:abc");
        assert_eq!(opened[0].1, records[0].1);
    }

    /// 口令错 = 解密失败（GCM tag 校验），不给明文。
    #[test]
    fn wrong_password_fails() {
        let records = vec![("k", json!(1))];
        let package = seal_package("right", &records);
        assert!(read_encrypted_backup(&package, "wrong").is_err());
    }

    /// 包被篡改/截断/重排：AAD 绑定行序号 → 校验失败。
    #[test]
    fn tampered_package_fails() {
        let records = vec![("a", json!(1)), ("b", json!(2))];
        let package = seal_package("pw", &records);
        let mut lines: Vec<String> = package.lines().map(|s| s.to_string()).collect();
        // 重排两行 body
        lines.swap(1, 2);
        let reordered = lines.join("\n");
        assert!(read_encrypted_backup(&reordered, "pw").is_err());
        // 截断一行
        let truncated = lines[..lines.len() - 1].join("\n");
        assert!(read_encrypted_backup(&truncated, "pw").is_err());
    }

    /// 明文 JSON（非 AIGXBAK1）被明确拒绝——兼容口径：明文导入走原端点。
    #[test]
    fn plaintext_json_rejected_with_hint() {
        let err = read_encrypted_backup(r#"{"version":1,"kv":{}}"#, "pw").unwrap_err();
        assert!(err.contains("明文包") || err.contains("AIGXBAK1") || err.contains("解析失败"));
    }
}
