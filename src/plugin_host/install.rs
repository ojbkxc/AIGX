//! 插件包下载、解压与校验。
//!
//! ## 信任链
//!
//! 1. **sha256**：源清单声明摘要时比对（挡「文件传坏了」）；
//! 2. **协议版本**：`pack.json` 的 `protocol` 必须等于网关的
//!    [`PROTOCOL_VERSION`]（挡版本不匹配）；
//! 3. **入口存在性 + zip-slip 防护**：解包时拒绝越界路径，并确认入口
//!    二进制真的在包里；
//! 4. **ed25519 验签**（对齐 Agent2API v3.5.0-beta.2）：`pack.json` 带
//!    `signature` 时，按 [`signing_payload`] 的**规范化载荷**（内容而非 zip
//!    字节）用内置公钥表 [`OFFICIAL_PUBKEYS`] 验签。**未签名的包仍允许安装**
//!    （界面标「未签名」），带签名却验不过的包**一律拒绝**。
//!
//! `signature` 覆盖的平台与本平台不一致时按「没有适用于本平台的签名」处理
//! （多平台包每平台各签一份）。

use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};

use base64::Engine;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::registry::{PackMeta, PackProvider};
use super::rpc::PROTOCOL_VERSION;

/// 单个 zip 的大小上限（对齐上游 `MAX_PACK_BYTES`：24 MiB）。
pub const MAX_PACK_BYTES: usize = 24 * 1024 * 1024;

/// 官方发布者公钥表：`(key_id, 32 字节 ed25519 公钥的十六进制)`。
///
/// 编译期常量（运行期不可改 —— 一个能被配置改掉的公钥等于没有签名）；
/// 支持轮换：新密钥先加进来、旧包重签后再删旧的。表内容与上游
/// Agent2API v3.5.0-beta.2 的 `OFFICIAL_PUBKEYS` 逐字一致。
pub const OFFICIAL_PUBKEYS: &[(&str, &str)] = &[(
    "official-2026",
    "7cdac378ff9bf23b72ae1b686f19859b3c802c26eed13c699f7df39f027352d6",
)];

/// 验签结果（三态：未签名 ≠ 验签失败，处置完全不同）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignatureState {
    /// 带了本平台签名且验签通过。
    Verified,
    /// 没有适用于本平台的签名（未签名包 / 签的是别的平台）。
    Unsigned,
    /// 带了本平台签名但验不过（算法不认 / 公钥不在表里 / 签名不匹配）。
    Invalid,
}

impl SignatureState {
    /// 是否算「已签名」（界面展示用）。
    pub fn is_signed(self) -> bool {
        matches!(self, Self::Verified)
    }
}

/// `pack.json` 的签名块（字段与上游 `PackSignature` 一致）。
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackSignature {
    pub algorithm: String,
    pub key_id: String,
    pub value: String,
    #[serde(default)]
    pub target: Option<String>,
}

fn hex_sha256(bytes: &[u8]) -> String {
    sha256_hex(bytes)
}

/// 规范化签名载荷（与上游 `signing_payload` 逐字一致）。
///
/// ```text
/// agent2api-pack-v1\n
/// id=<id>\n
/// version=<version>\n
/// protocol=<protocol>\n
/// target=<target>\n
/// providers=<id>:<order>:<default_enabled(1/0)>,…   ← 字典序
/// entry=<target>=<path>,…                          ← 字典序
/// binary=<entry 二进制 sha256>\n
/// ui=<relpath>=<sha256>,…                          ← 字典序
/// ```
///
/// AIGX 的 `PackProvider` 带 `order` 字段（缺省 0），`default_enabled` 取
/// 清单原值 —— 两者都按上游 `pack.json` 的真实值参与载荷。**不可把 `order`
/// 固定成 0**：早期包 order 恰好是 0，但 wasm 新包会写非 0（如 accio=26），
/// 写死 0 会导致「签名与包内容不匹配」。
///
/// Wasm 包的额外项：`wasm=<wasm.json sha256>`；带 `host.json` 时追加
/// `host=<host.json sha256>`（两行都在 `ui=` 之后，与上游顺序一致）。
pub fn signing_payload(
    manifest: &PackManifest,
    target: &str,
    binary_sha256: &str,
    ui_files: &[(String, String)],
    wasm_sha256: Option<&str>,
    host_sha256: Option<&str>,
) -> String {
    let mut providers: Vec<String> = manifest
        .providers
        .iter()
        .map(|item| {
            format!(
                "{}:{}:{}",
                item.id,
                item.order,
                if item.default_enabled { 1 } else { 0 }
            )
        })
        .collect();
    providers.sort();
    let mut entries: Vec<String> = manifest
        .entry
        .per_target
        .iter()
        .map(|(target, path)| format!("{target}={path}"))
        .collect();
    if !manifest.entry.single.is_empty() {
        entries.push(format!("{target}={}", manifest.entry.single));
    }
    entries.sort();
    let mut ui: Vec<String> = ui_files
        .iter()
        .map(|(path, hash)| format!("{path}={hash}"))
        .collect();
    ui.sort();
    let mut payload = format!(
        "agent2api-pack-v1\nid={}\nversion={}\nprotocol={}\ntarget={}\nproviders={}\nentry={}\nbinary={}\nui={}\n",
        manifest.id,
        manifest.version,
        manifest.protocol,
        target,
        providers.join(","),
        entries.join(","),
        binary_sha256,
        ui.join(","),
    );
    if let Some(wasm) = wasm_sha256 {
        payload.push_str(&format!("wasm={wasm}\n"));
    }
    if let Some(host) = host_sha256 {
        payload.push_str(&format!("host={host}\n"));
    }
    payload
}

/// 十六进制（64 字符）→ 32 字节。
fn hex_to_32(text: &str) -> Option<[u8; 32]> {
    let text = text.trim();
    if text.len() != 64 {
        return None;
    }
    let mut bytes = [0u8; 32];
    for (index, chunk) in text.as_bytes().as_chunks::<2>().0.iter().enumerate() {
        let chunk = std::str::from_utf8(chunk).ok()?;
        bytes[index] = u8::from_str_radix(chunk, 16).ok()?;
    }
    Some(bytes)
}

/// ed25519 验签（`ring` 的 `ED25519`，与上游 dalek 逐位等价）。
fn verify_ed25519(public: &[u8; 32], message: &[u8], signature: &[u8]) -> bool {
    if signature.len() != 64 {
        return false;
    }
    ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, public.as_ref())
        .verify(message, signature)
        .is_ok()
}

/// 递归遍历 UI 资源（`ui/` 下全部文件），返回 `(POSIX 相对路径, sha256)`。
fn collect_ui_files(dir: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let root = match dir.join("ui").canonicalize() {
        Ok(root) => root,
        Err(_) => return out,
    };
    let mut stack = vec![root.clone()];
    while let Some(current) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&current) else {
            continue;
        };
        for entry in rd.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.is_file() {
                let Ok(relative) = path.strip_prefix(&root) else {
                    continue;
                };
                let relative = relative
                    .components()
                    .map(|part| part.as_os_str().to_string_lossy().to_string())
                    .collect::<Vec<_>>()
                    .join("/");
                if let Ok(bytes) = std::fs::read(&path) {
                    out.push((format!("ui/{relative}"), hex_sha256(&bytes)));
                }
            }
        }
    }
    out.sort();
    out
}

/// 验一份已解包目录的签名。
///
/// 返回 `(状态, 失败原因)` —— 原因只在 `Invalid` 时有值。
///
/// `target` 是**当前宿主的**目标三元组；Wasm 包（入口声明在
/// `wasm32-unknown-unknown` 下）强制按该三元组验。
pub fn verify_signature(
    dir: &Path,
    manifest: &PackManifest,
    target: &str,
) -> (SignatureState, Option<String>) {
    let wasm_entry = manifest
        .entry
        .per_target
        .get(super::wasm::manifest::TARGET)
        .cloned();
    let target = if wasm_entry.is_some() {
        super::wasm::manifest::TARGET
    } else {
        target
    };
    let Some(signature) = manifest.signature.as_ref() else {
        return (SignatureState::Unsigned, None);
    };
    let signature: PackSignature = match serde_json::from_value(signature.clone()) {
        Ok(signature) => signature,
        Err(_) => return (SignatureState::Invalid, Some("签名块格式无效".to_string())),
    };
    // 签名绑定的平台与本平台不一致：当作「没有适用于本平台的签名」。
    if signature.target.as_deref() != Some(target) {
        return (SignatureState::Unsigned, None);
    }
    if !signature.algorithm.eq_ignore_ascii_case("ed25519") {
        return (
            SignatureState::Invalid,
            Some(format!("不支持的签名算法：{}", signature.algorithm)),
        );
    }
    let Some((_, public_hex)) = OFFICIAL_PUBKEYS
        .iter()
        .find(|(id, _)| *id == signature.key_id)
    else {
        return (
            SignatureState::Invalid,
            Some(format!(
                "签名密钥 {} 不在本版本的信任列表里（可能需要更新网关）",
                signature.key_id
            )),
        );
    };
    let Some(public) = hex_to_32(public_hex) else {
        return (
            SignatureState::Invalid,
            Some(format!("内置公钥 {} 的格式无效", signature.key_id)),
        );
    };
    let Some(entry) = manifest.entry.for_target(target) else {
        return (
            SignatureState::Invalid,
            Some("清单缺少本平台入口".to_string()),
        );
    };
    let binary = match std::fs::read(dir.join(entry)) {
        Ok(bytes) => bytes,
        Err(_) => {
            return (
                SignatureState::Invalid,
                Some(format!("入口二进制不可读：{entry}")),
            )
        }
    };
    let ui = collect_ui_files(dir);
    let wasm_sha = if wasm_entry.is_some() {
        std::fs::read(dir.join("wasm.json"))
            .ok()
            .map(|bytes| hex_sha256(&bytes))
    } else {
        None
    };
    let host_sha = std::fs::read(dir.join("host.json"))
        .ok()
        .map(|bytes| hex_sha256(&bytes));
    let payload = signing_payload(
        manifest,
        target,
        &hex_sha256(&binary),
        &ui,
        wasm_sha.as_deref(),
        host_sha.as_deref(),
    );
    if signature.value.len() > 256 {
        return (SignatureState::Invalid, Some("签名本体过长".to_string()));
    }
    let decoded = match base64::engine::general_purpose::STANDARD.decode(signature.value.trim()) {
        Ok(decoded) => decoded,
        Err(_) => {
            return (
                SignatureState::Invalid,
                Some("签名本体不是有效的 base64".to_string()),
            )
        }
    };
    if verify_ed25519(&public, payload.as_bytes(), &decoded) {
        (SignatureState::Verified, None)
    } else {
        (
            SignatureState::Invalid,
            Some("签名与包内容不匹配（包被改动或来源不是它自称的发布者）".to_string()),
        )
    }
}

/// 解压后文件数上限（防 zip bomb）。
const MAX_ENTRIES: usize = 4096;

/// `pack.json` 的形状（只取网关需要的字段）。
#[derive(Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct PackManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub protocol: u32,
    /// 入口二进制。**两种上游形态都收**，见 [`PackEntry`]。
    pub entry: PackEntry,
    pub providers: Vec<PackProvider>,
    /// 包自带的签名（原样保留，AIGX 不校验）。
    pub signature: Option<serde_json::Value>,
}

/// `pack.json` 的 `entry` 字段。
///
/// - **字符串**：单平台包的入口相对路径（AIGX 自建包 / 早期约定）；
/// - **对象**：`{目标三元组: 入口相对路径}`（上游 Agent2API 3.5.x 起）。
///   一个 zip 的 `pack.json` 会列出全部平台，但**只随包附一个二进制** ——
///   所以必须按当前宿主的三元组挑，不能照单全收。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PackEntry {
    /// 字符串形态（原样保留）
    pub single: String,
    /// 对象形态：目标三元组 → 入口路径
    pub per_target: std::collections::HashMap<String, String>,
}

impl PackEntry {
    /// 当前宿主的入口相对路径；无法确定返回 `None`（交给 `bin/` 自动探测）。
    pub fn for_host(&self) -> Option<&str> {
        if !self.single.is_empty() {
            return Some(self.single.as_str());
        }
        if self.per_target.is_empty() {
            return None;
        }
        let triple = crate::plugin_host::process::host_target_triple();
        if let Some(p) = self.per_target.get(&triple) {
            return Some(p.as_str());
        }
        // 未精确命中（上游三元组命名与宿主不同，或单平台包只列了一项）：
        // 只有唯一一项时用它，否则放弃——宁可让 `bin/` 自动探测，也不猜错平台。
        if self.per_target.len() == 1 {
            return self.per_target.values().next().map(|s| s.as_str());
        }
        None
    }

    /// 指定平台三元组的入口相对路径（签名载荷用；精确命中才算）。
    pub fn for_target(&self, target: &str) -> Option<&str> {
        if let Some(path) = self.per_target.get(target) {
            return Some(path.as_str());
        }
        if !self.single.is_empty() {
            return Some(self.single.as_str());
        }
        if self.per_target.len() == 1 {
            return self.per_target.values().next().map(|s| s.as_str());
        }
        None
    }

    /// 清单是否完全没声明入口。
    #[allow(dead_code)] // 预留/线表符号（见 main.rs 债务清单）
    pub fn is_unset(&self) -> bool {
        self.single.is_empty() && self.per_target.is_empty()
    }
}

impl<'de> Deserialize<'de> for PackEntry {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error as _;
        let v = serde_json::Value::deserialize(deserializer)?;
        match v {
            serde_json::Value::Null => Ok(PackEntry::default()),
            serde_json::Value::String(s) => Ok(PackEntry {
                single: s,
                per_target: Default::default(),
            }),
            serde_json::Value::Object(map) => {
                let mut per_target = std::collections::HashMap::new();
                for (k, val) in map {
                    match val {
                        serde_json::Value::String(p) => {
                            per_target.insert(k, p);
                        }
                        _ => {
                            return Err(D::Error::custom(format!("entry.\"{k}\" 必须是字符串路径")))
                        }
                    }
                }
                Ok(PackEntry {
                    single: String::new(),
                    per_target,
                })
            }
            other => Err(D::Error::custom(format!(
                "entry 必须是字符串或 {{目标三元组: 路径}} 对象，实际是 {other}"
            ))),
        }
    }
}

/// 安装错误。
#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("下载插件包失败: {0}")]
    Download(String),
    #[error("插件包过大（{0} 字节，上限 {1}）")]
    TooLarge(usize, usize),
    #[error("sha256 不匹配（期望 {expected}，实际 {actual}）")]
    DigestMismatch { expected: String, actual: String },
    #[error("解压失败: {0}")]
    Unzip(String),
    #[error("包清单无效: {0}")]
    Manifest(String),
    #[error("协议版本不匹配：包要求 {pack}，网关是 {host}")]
    ProtocolMismatch { pack: u32, host: u32 },
    #[error("落盘失败: {0}")]
    Io(String),
}

impl From<std::io::Error> for InstallError {
    fn from(e: std::io::Error) -> Self {
        InstallError::Io(e.to_string())
    }
}

/// 算字节的 sha256（十六进制小写）。
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// 把 zip 内条目名安全地拼到根目录下（zip-slip 防护）。
///
/// 拒绝：绝对路径、含 `..` 的段、反斜杠（Windows 分隔符绕过）、盘符前缀。
fn safe_join(root: &Path, name: &str) -> Result<PathBuf, InstallError> {
    let norm = name.replace('\\', "/");
    let norm = norm.trim_start_matches('/');
    if norm.is_empty() {
        return Err(InstallError::Unzip("空条目名".into()));
    }
    if norm.len() >= 2 && norm.as_bytes()[1] == b':' {
        return Err(InstallError::Unzip(format!("盘符路径: {name}")));
    }
    if norm.split('/').any(|seg| seg == "..") {
        return Err(InstallError::Unzip(format!("越界路径: {name}")));
    }
    Ok(root.join(norm))
}

/// 解压 zip 到 `dest`（自建目录）。
pub fn extract_zip(bytes: &[u8], dest: &Path) -> Result<(), InstallError> {
    if bytes.len() > MAX_PACK_BYTES {
        return Err(InstallError::TooLarge(bytes.len(), MAX_PACK_BYTES));
    }
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|e| InstallError::Unzip(format!("不是有效的 zip: {e}")))?;
    if archive.len() > MAX_ENTRIES {
        return Err(InstallError::Unzip(format!(
            "条目过多（{}）",
            archive.len()
        )));
    }
    std::fs::create_dir_all(dest)?;
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| InstallError::Unzip(format!("读取条目 {i} 失败: {e}")))?;
        // `enclosed_name` 会拒绝越界路径；再用 safe_join 兜一道
        let raw_name = entry.name().to_string();
        let out = safe_join(dest, &raw_name)?;
        if entry.is_dir() {
            std::fs::create_dir_all(&out)?;
            continue;
        }
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut buf = Vec::with_capacity(entry.size() as usize);
        entry
            .read_to_end(&mut buf)
            .map_err(|e| InstallError::Unzip(format!("读取 {raw_name} 失败: {e}")))?;
        let mut f = std::fs::File::create(&out)?;
        f.write_all(&buf)?;
    }
    // 入口可执行位（Unix）
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        ensure_tree_executable(dest, |p| {
            let meta = std::fs::metadata(p).ok()?;
            let mode = meta.permissions().mode();
            if mode & 0o100 == 0 {
                let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode | 0o100));
            }
            Some(())
        });
    }
    Ok(())
}

#[cfg(unix)]
fn ensure_tree_executable<F: FnMut(&Path) -> Option<()>>(root: &Path, mut f: F) {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                f(&p);
            }
        }
    }
}

/// 读并解析 `pack.json`。
pub fn parse_manifest(dir: &Path) -> Result<PackManifest, InstallError> {
    let path = dir.join("pack.json");
    let text = std::fs::read_to_string(&path).map_err(|e| {
        InstallError::Manifest(format!("读不到 pack.json（{}）: {e}", path.display()))
    })?;
    serde_json::from_str::<PackManifest>(&text)
        .map_err(|e| InstallError::Manifest(format!("pack.json 解析失败: {e}")))
}

/// 探测入口二进制：`pack.json` 的 `entry`（取当前平台那条），或 `bin/` 下的第一个文件。
fn resolve_entry(dir: &Path, manifest: &PackManifest) -> Result<String, InstallError> {
    if let Some(entry) = manifest.entry.for_host() {
        let entry = entry.to_string();
        let p = safe_join(dir, &entry)?;
        if p.is_file() {
            return Ok(entry);
        }
        return Err(InstallError::Manifest(format!(
            "清单声明的入口不存在: {entry}（包内没有当前平台的二进制？）"
        )));
    }
    let bin = dir.join("bin");
    if let Ok(rd) = std::fs::read_dir(&bin) {
        for e in rd.flatten() {
            if e.path().is_file() {
                return Ok(format!("bin/{}", e.file_name().to_string_lossy()));
            }
        }
    }
    Err(InstallError::Manifest(
        "包里找不到入口二进制（清单未声明 entry，bin/ 也为空）".into(),
    ))
}

/// 从 zip 字节安装一个包。
///
/// `plugins_root` 是插件根目录；`expected_sha256` 由源清单提供（可选）。
/// 解压到暂存目录 → 校验 → `rename` 进版本目录（半成品包绝不会出现在
/// 正式目录里）。
pub fn install_from_zip(
    bytes: &[u8],
    plugins_root: &Path,
    expected_sha256: Option<&str>,
) -> Result<PackMeta, InstallError> {
    if bytes.len() > MAX_PACK_BYTES {
        return Err(InstallError::TooLarge(bytes.len(), MAX_PACK_BYTES));
    }
    // 1) sha256
    let actual = sha256_hex(bytes);
    if let Some(exp) = expected_sha256 {
        if !exp.trim().eq_ignore_ascii_case(&actual) {
            return Err(InstallError::DigestMismatch {
                expected: exp.to_string(),
                actual,
            });
        }
    }
    // 2) 解压到暂存目录
    std::fs::create_dir_all(plugins_root)?;
    let staging_root = plugins_root.join(".staging");
    std::fs::create_dir_all(&staging_root)?;
    let staging = staging_root.join(&actual[..16.min(actual.len())]);
    if staging.exists() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    extract_zip(bytes, &staging)?;
    // 暂存目录可能被 zip 套了一层（顶层目录）；找到含 pack.json 的那层
    let content = find_manifest_dir(&staging)?;
    // 3) 清单 + 校验
    let manifest = parse_manifest(&content)?;
    if manifest.id.is_empty() {
        return Err(InstallError::Manifest("清单缺 id".into()));
    }
    if manifest.version.is_empty() {
        return Err(InstallError::Manifest("清单缺 version".into()));
    }
    if manifest.protocol != PROTOCOL_VERSION {
        return Err(InstallError::ProtocolMismatch {
            pack: manifest.protocol,
            host: PROTOCOL_VERSION,
        });
    }
    let entry = resolve_entry(&content, &manifest)?;
    // 入口二进制摘要（上游 `InstalledPack.sha256` 同义）：native helper 用它做包指纹。
    // 与整包摘要 `actual` 不同：同一入口的包重打包（改图标/文案）仍视为同一实现。
    let entry_sha256 = match std::fs::read(content.join(&entry)) {
        Ok(bytes) => Some(sha256_hex(&bytes)),
        Err(_) => None,
    };
    // 4.5) 验签（对齐 beta.2）：签名在**解包后的内容**上验，不是 zip 字节。
    //      · Verified  → signed=true
    //      · Unsigned  → 允许安装，界面标「未签名」
    //      · Invalid   → 一律拒绝（包被改动或不是它自称的发布者）
    let (signature_state, signature_error) = verify_signature(
        &content,
        &manifest,
        &crate::plugin_host::process::host_target_triple(),
    );
    if signature_state == SignatureState::Invalid {
        let reason = signature_error.unwrap_or_else(|| "签名校验失败".to_string());
        return Err(InstallError::Manifest(format!(
            "插件包验签未通过：{reason}"
        )));
    }
    // 5) 移到版本目录
    let dest = plugins_root.join(&manifest.id).join(&manifest.version);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if dest.exists() {
        let _ = std::fs::remove_dir_all(&dest);
    }
    std::fs::rename(&content, &dest)
        .map_err(|e| InstallError::Io(format!("移动到 {} 失败: {e}", dest.display())))?;
    // 清理暂存（若 content != staging，两者都要清）
    let _ = std::fs::remove_dir_all(&staging);

    let signed = signature_state.is_signed();
    Ok(PackMeta {
        id: manifest.id,
        name: manifest.name,
        version: manifest.version,
        protocol: manifest.protocol,
        providers: manifest.providers,
        entry,
        signature: manifest.signature.map(|v| v.to_string()),
        sha256: entry_sha256,
        signed,
        installed_at: now_unix(),
        enabled: false,
        disabled_providers: Vec::new(),
    })
}

/// 找到含 `pack.json` 的那一层（容忍 zip 顶层套了一层目录）。
fn find_manifest_dir(root: &Path) -> Result<PathBuf, InstallError> {
    if root.join("pack.json").is_file() {
        return Ok(root.to_path_buf());
    }
    let Ok(rd) = std::fs::read_dir(root) else {
        return Err(InstallError::Manifest("暂存目录不可读".into()));
    };
    let mut dirs: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    for d in dirs {
        if d.join("pack.json").is_file() {
            return Ok(d);
        }
    }
    Err(InstallError::Manifest("包里没有 pack.json".into()))
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 下载一个 URL 到内存（带上限）。
pub async fn download_bytes(client: &reqwest::Client, url: &str) -> Result<Vec<u8>, InstallError> {
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| InstallError::Download(e.to_string()))?;
    if !resp.status().is_success() {
        return Err(InstallError::Download(format!(
            "HTTP {} ({url})",
            resp.status()
        )));
    }
    if let Some(len) = resp.content_length() {
        if len as usize > MAX_PACK_BYTES {
            return Err(InstallError::TooLarge(len as usize, MAX_PACK_BYTES));
        }
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| InstallError::Download(e.to_string()))?;
    if bytes.len() > MAX_PACK_BYTES {
        return Err(InstallError::TooLarge(bytes.len(), MAX_PACK_BYTES));
    }
    Ok(bytes.to_vec())
}

/// 下载一个 URL 到内存（带上限），并在每收到一块时回报进度。
///
/// 与 [`download_bytes`] 的唯一差异是 `on_progress(received, total)`：
/// 服务端给出 `Content-Length` 时 `total` 是确定值，否则为 `0`（前端据此
/// 显示不确定进度条）。累计字节数单调递增，首帧固定回调一次 `(0, total)`。
///
/// 这是插件「Operation 异步进度」模型的数据来源（下载阶段）。
pub async fn download_bytes_with_progress<F>(
    client: &reqwest::Client,
    url: &str,
    mut on_progress: F,
) -> Result<Vec<u8>, InstallError>
where
    F: FnMut(u64, u64),
{
    use futures::StreamExt;
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| InstallError::Download(e.to_string()))?;
    if !resp.status().is_success() {
        return Err(InstallError::Download(format!(
            "HTTP {} ({url})",
            resp.status()
        )));
    }
    let total = resp.content_length().unwrap_or(0);
    if total as usize > MAX_PACK_BYTES {
        return Err(InstallError::TooLarge(total as usize, MAX_PACK_BYTES));
    }
    let mut received: u64 = 0;
    let mut buffer: Vec<u8> = Vec::with_capacity(total.min(MAX_PACK_BYTES as u64) as usize);
    on_progress(0, total);
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| InstallError::Download(e.to_string()))?;
        received += chunk.len() as u64;
        if received as usize > MAX_PACK_BYTES {
            return Err(InstallError::TooLarge(received as usize, MAX_PACK_BYTES));
        }
        buffer.extend_from_slice(&chunk);
        on_progress(received, total);
    }
    Ok(buffer)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一个 zip（内存）。
    fn make_zip(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let mut w = zip::ZipWriter::new(Cursor::new(&mut buf));
            let opts: zip::write::FileOptions<()> = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            for (name, data) in files {
                w.start_file(*name, opts).unwrap();
                w.write_all(data).unwrap();
            }
            w.finish().unwrap();
        }
        buf
    }

    fn pack_json(
        id: &str,
        version: &str,
        protocol: u32,
        entry: &str,
        providers: &[&str],
    ) -> String {
        let provs: Vec<String> = providers
            .iter()
            .map(|p| format!(r#"{{"id":"{p}","label":"{p}","default_enabled":true}}"#))
            .collect();
        format!(
            r#"{{"id":"{id}","name":"{id}","version":"{version}","protocol":{protocol},"entry":"{entry}","providers":[{}]}}"#,
            provs.join(",")
        )
    }

    fn temp_root() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("aigx-install-{}-{seq}", std::process::id()));
        // PID 复用 + 残留目录会让用例读到上一次的数据（并行/重跑偶发）
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn install_happy_path() {
        let root = temp_root();
        let manifest = pack_json(
            "agent2api-providers",
            "3.4.2",
            1,
            "bin/plugin",
            &["kiro", "astudio"],
        );
        let bytes = make_zip(&[
            ("pack.json", manifest.as_bytes()),
            ("bin/plugin", b"#!/bin/sh\necho hi\n"),
        ]);
        let meta = install_from_zip(&bytes, &root, None).unwrap();
        assert_eq!(meta.id, "agent2api-providers");
        assert_eq!(meta.version, "3.4.2");
        assert_eq!(meta.entry, "bin/plugin");
        assert_eq!(meta.providers.len(), 2);
        assert!(!meta.signed);
        // 落盘到位
        assert!(meta.entry_path(&root).is_file());
        assert!(!meta.enabled, "新装包默认停用");
    }

    #[test]
    fn install_with_top_level_dir() {
        // zip 顶层套了一层目录（常见打包形态）
        let root = temp_root();
        let manifest = pack_json("p", "1.0.0", 1, "bin/plugin", &["kiro"]);
        let bytes = make_zip(&[
            ("p-1.0.0/pack.json", manifest.as_bytes()),
            ("p-1.0.0/bin/plugin", b"x"),
        ]);
        let meta = install_from_zip(&bytes, &root, None).unwrap();
        assert_eq!(meta.id, "p");
        assert!(meta.entry_path(&root).is_file());
    }

    #[test]
    fn install_sha256_mismatch_rejected() {
        let root = temp_root();
        let manifest = pack_json("p", "1.0.0", 1, "bin/plugin", &["kiro"]);
        let bytes = make_zip(&[("pack.json", manifest.as_bytes()), ("bin/plugin", b"x")]);
        let err = install_from_zip(&bytes, &root, Some("deadbeef")).unwrap_err();
        assert!(matches!(err, InstallError::DigestMismatch { .. }));
    }

    #[test]
    fn install_sha256_match_accepted() {
        let root = temp_root();
        let manifest = pack_json("p", "1.0.0", 1, "bin/plugin", &["kiro"]);
        let bytes = make_zip(&[("pack.json", manifest.as_bytes()), ("bin/plugin", b"x")]);
        let digest = sha256_hex(&bytes);
        assert!(install_from_zip(&bytes, &root, Some(&digest)).is_ok());
    }

    #[test]
    fn install_protocol_mismatch_rejected() {
        let root = temp_root();
        let manifest = pack_json("p", "1.0.0", 99, "bin/plugin", &["kiro"]);
        let bytes = make_zip(&[("pack.json", manifest.as_bytes()), ("bin/plugin", b"x")]);
        let err = install_from_zip(&bytes, &root, None).unwrap_err();
        assert!(matches!(
            err,
            InstallError::ProtocolMismatch { pack: 99, host: 1 }
        ));
    }

    #[test]
    fn install_missing_entry_rejected() {
        let root = temp_root();
        let manifest = pack_json("p", "1.0.0", 1, "bin/plugin", &["kiro"]);
        // 声明了入口但包里没有
        let bytes = make_zip(&[("pack.json", manifest.as_bytes())]);
        let err = install_from_zip(&bytes, &root, None).unwrap_err();
        assert!(matches!(err, InstallError::Manifest(_)));
    }

    #[test]
    fn install_entry_autodetect() {
        let root = temp_root();
        // 清单不声明 entry → 自动探测 bin/
        let manifest = r#"{"id":"p","name":"p","version":"1.0.0","protocol":1,"providers":[{"id":"kiro","label":"Kiro"}]}"#;
        let bytes = make_zip(&[("pack.json", manifest.as_bytes()), ("bin/plugin", b"x")]);
        let meta = install_from_zip(&bytes, &root, None).unwrap();
        assert_eq!(meta.entry, "bin/plugin");
    }

    #[test]
    fn zip_slip_rejected() {
        let root = temp_root();
        let manifest = pack_json("p", "1.0.0", 1, "bin/plugin", &["kiro"]);
        let bytes = make_zip(&[
            ("pack.json", manifest.as_bytes()),
            ("bin/plugin", b"x"),
            ("../escape.txt", b"pwned"),
        ]);
        let err = install_from_zip(&bytes, &root, None).unwrap_err();
        assert!(
            matches!(err, InstallError::Unzip(_)),
            "越界路径应被拒绝: {err:?}"
        );
        // 确认没写到 root 外
        assert!(!root.parent().unwrap().join("escape.txt").exists());
    }

    #[test]
    fn manifest_with_malformed_signature_block_is_rejected() {
        // beta.2 起签名参与信任链：缺 keyId 的签名块是「验不过」，一律拒绝。
        let root = temp_root();
        let manifest = r#"{"id":"p","name":"p","version":"1.0.0","protocol":1,"entry":"bin/plugin","providers":[],"signature":{"algorithm":"ed25519","value":"abc"}}"#;
        let bytes = make_zip(&[("pack.json", manifest.as_bytes()), ("bin/plugin", b"x")]);
        let err = install_from_zip(&bytes, &root, None).unwrap_err();
        assert!(err.to_string().contains("验签"), "{err}");
        assert!(!root.join("p").exists());
    }

    #[test]
    fn safe_join_rejects_bad_paths() {
        let root = Path::new("/tmp/x");
        assert!(safe_join(root, "a/b").is_ok());
        assert!(safe_join(root, "../a").is_err());
        assert!(safe_join(root, "/etc/passwd").is_ok()); // 前导 / 被去掉，落在 root 内
        assert!(safe_join(root, "C:/win").is_err());
        assert!(safe_join(root, "a\\..\\b").is_err());
    }

    /// 上游形态：`entry` 是「全平台三元组 → 路径」映射，包里只附当前平台那个二进制。
    /// 必须按宿主三元组挑，否则会拿到别的平台的路径 → 入口不存在。
    #[test]
    fn install_upstream_entry_map_resolves_for_host() {
        let root = temp_root();
        let triple = crate::plugin_host::host_target_triple();
        let manifest = r#"{"id":"p","name":"p","version":"3.5.0-beta.1","protocol":1,"entry":{"x86_64-pc-windows-msvc":"bin/p.exe","__TRIPLE__":"bin/p"},"providers":[{"id":"kiro","label":"Kiro"}]}"#
            .replace("__TRIPLE__", &triple);
        let bytes = make_zip(&[("pack.json", manifest.as_bytes()), ("bin/p", b"x")]);
        let meta = install_from_zip(&bytes, &root, None).unwrap();
        assert_eq!(meta.entry, "bin/p");
    }

    /// 映射里没有当前平台且有多项 → 不猜，退回 `bin/` 自动探测。
    #[test]
    fn install_entry_map_unknown_platform_falls_back_to_probe() {
        let root = temp_root();
        let manifest = r#"{"id":"p","name":"p","version":"1.0.0","protocol":1,"entry":{"triple-a":"bin/x","triple-b":"bin/y"},"providers":[]}"#;
        let bytes = make_zip(&[("pack.json", manifest.as_bytes()), ("bin/p", b"x")]);
        let meta = install_from_zip(&bytes, &root, None).unwrap();
        assert_eq!(meta.entry, "bin/p", "多平台未命中时应自动探测 bin/");
    }

    /// 映射只有一项时不认得也用（单平台包）。
    #[test]
    fn install_entry_map_single_unknown_platform_uses_it() {
        let root = temp_root();
        let manifest = r#"{"id":"p","name":"p","version":"1.0.0","protocol":1,"entry":{"totally-unknown-triple":"bin/only"},"providers":[]}"#;
        let bytes = make_zip(&[("pack.json", manifest.as_bytes()), ("bin/only", b"x")]);
        let meta = install_from_zip(&bytes, &root, None).unwrap();
        assert_eq!(meta.entry, "bin/only");
    }

    /// 映射值不是字符串 → 清单无效（不要静默当成空）。
    #[test]
    fn install_entry_map_bad_value_type_rejected() {
        let root = temp_root();
        let manifest = r#"{"id":"p","name":"p","version":"1.0.0","protocol":1,"entry":{"triple":123},"providers":[]}"#;
        let bytes = make_zip(&[("pack.json", manifest.as_bytes()), ("bin/p", b"x")]);
        let err = install_from_zip(&bytes, &root, None).unwrap_err();
        assert!(matches!(err, InstallError::Manifest(_)), "{err:?}");
    }

    /// 回归：`entry` 写成字符串（AIGX 自建包）仍按原样解析。
    #[test]
    fn install_entry_string_still_works() {
        let root = temp_root();
        let manifest = pack_json("p", "1.0.0", 1, "bin/plugin", &["kiro"]);
        let bytes = make_zip(&[("pack.json", manifest.as_bytes()), ("bin/plugin", b"x")]);
        let meta = install_from_zip(&bytes, &root, None).unwrap();
        assert_eq!(meta.entry, "bin/plugin");
    }

    // ── ed25519 验签 ────────────────────────────────────────────────

    /// 用测试密钥签一个可复现的包：返回 `(pack.json 字符串, 签名 base64, 公钥)`。
    ///
    /// 载荷拼装规则与 [`signing_payload`] 同源，签名用 ring 的 Ed25519KeyPair
    /// （与验签侧同一个实现，保证「拼装/验签」两边一致才过）。
    fn sign_pack(
        manifest_json: &str,
        binary: &[u8],
        target: &str,
        ui: &[(&str, &[u8])],
    ) -> (String, String, [u8; 32]) {
        use ring::rand::SystemRandom;
        use ring::signature::{Ed25519KeyPair, KeyPair};
        let rng = SystemRandom::new();
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
        let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        let public: [u8; 32] = pair.public_key().as_ref().try_into().unwrap();
        let manifest: PackManifest = serde_json::from_str(manifest_json).unwrap();
        let mut ui_files: Vec<(String, String)> = ui
            .iter()
            .map(|(path, data)| (path.to_string(), hex_sha256(data)))
            .collect();
        ui_files.sort();
        let payload = signing_payload(
            &manifest,
            target,
            &hex_sha256(binary),
            &ui_files,
            None,
            None,
        );
        let signature = pair.sign(payload.as_bytes());
        let encoded = base64::engine::general_purpose::STANDARD.encode(signature.as_ref());
        (manifest_json.to_string(), encoded, public)
    }

    fn with_signature(manifest_json: &str, signature_b64: &str, target: &str) -> String {
        let mut value: serde_json::Value = serde_json::from_str(manifest_json).unwrap();
        value["signature"] = serde_json::json!({
            "algorithm": "ed25519",
            "keyId": "official-2026",
            "value": signature_b64,
            "target": target,
        });
        serde_json::to_string(&value).unwrap()
    }

    #[test]
    fn signature_payload_matches_reference_format() {
        let manifest = pack_json("demo", "1.0.0", 1, "bin/plugin", &["kiro", "loomy"]);
        let manifest: PackManifest = serde_json::from_str(&manifest).unwrap();
        let payload = signing_payload(
            &manifest,
            "x86_64-unknown-linux-gnu",
            "abc",
            &[("ui/a.js".into(), "def".into())],
            None,
            None,
        );
        assert!(payload.starts_with("agent2api-pack-v1\n"));
        assert!(payload.contains("\nid=demo\n"));
        assert!(payload.contains("\nversion=1.0.0\n"));
        assert!(payload.contains("\nbinary=abc\n"));
        assert!(payload.contains("\nui=ui/a.js=def\n"));
        assert!(payload.ends_with('\n'), "载荷末尾不留空行");
    }

    #[test]
    fn signing_payload_uses_provider_order() {
        // 回归：上游 wasm 包的 provider 带非 0 order（accio=26），
        // 载荷必须用真实 order；写死 0 会「签名与包内容不匹配」。
        let manifest_json = r#"{"id":"agent2api-wasm-accio","version":"0.1.0-beta.1","protocol":1,"entry":{"wasm32-unknown-unknown":"accio.wasm"},"providers":[{"id":"accio","label":"Accio","order":26,"default_enabled":true}]}"#;
        let manifest: PackManifest = serde_json::from_str(manifest_json).unwrap();
        let payload = signing_payload(&manifest, "wasm32-unknown-unknown", "abc", &[], None, None);
        assert!(payload.contains("\nproviders=accio:26:1\n"), "{payload}");
        assert!(
            !payload.contains("\nproviders=accio:0:1\n"),
            "order 不应被写死为 0: {payload}"
        );
    }

    #[test]
    fn unsigned_pack_is_allowed() {
        let manifest = pack_json("p", "1.0.0", 1, "bin/p", &["kiro"]);
        let manifest: PackManifest = serde_json::from_str(&manifest).unwrap();
        let dir = temp_root();
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(dir.join("bin/p"), b"x").unwrap();
        let (state, reason) = verify_signature(&dir, &manifest, "x86_64-unknown-linux-gnu");
        assert_eq!(state, SignatureState::Unsigned);
        assert!(reason.is_none());
    }

    #[test]
    fn signature_for_other_target_is_unsigned() {
        let manifest = pack_json("p", "1.0.0", 1, "bin/p", &["kiro"]);
        let signed = with_signature(&manifest, "AAAA", "some-other-target");
        let manifest: PackManifest = serde_json::from_str(&signed).unwrap();
        let dir = temp_root();
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(dir.join("bin/p"), b"x").unwrap();
        let (state, _) = verify_signature(&dir, &manifest, "x86_64-unknown-linux-gnu");
        assert_eq!(state, SignatureState::Unsigned);
    }

    #[test]
    fn unknown_key_id_is_invalid() {
        let manifest = pack_json("p", "1.0.0", 1, "bin/p", &["kiro"]);
        let mut value: serde_json::Value = serde_json::from_str(&manifest).unwrap();
        value["signature"] = serde_json::json!({
            "algorithm": "ed25519", "keyId": "not-in-table", "value": "AAAA",
            "target": "x86_64-unknown-linux-gnu",
        });
        let manifest: PackManifest = serde_json::from_value(value).unwrap();
        let dir = temp_root();
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(dir.join("bin/p"), b"x").unwrap();
        let (state, reason) = verify_signature(&dir, &manifest, "x86_64-unknown-linux-gnu");
        assert_eq!(state, SignatureState::Invalid);
        assert!(reason.unwrap().contains("信任列表"));
    }

    #[test]
    fn non_ed25519_algorithm_is_invalid() {
        let manifest = pack_json("p", "1.0.0", 1, "bin/p", &["kiro"]);
        let mut value: serde_json::Value = serde_json::from_str(&manifest).unwrap();
        value["signature"] = serde_json::json!({
            "algorithm": "rsa", "keyId": "official-2026", "value": "AAAA",
            "target": "x86_64-unknown-linux-gnu",
        });
        let manifest: PackManifest = serde_json::from_value(value).unwrap();
        let dir = temp_root();
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(dir.join("bin/p"), b"x").unwrap();
        let (state, _) = verify_signature(&dir, &manifest, "x86_64-unknown-linux-gnu");
        assert_eq!(state, SignatureState::Invalid);
    }

    #[test]
    fn invalid_pack_is_rejected_before_disk_write() {
        // 带官方 keyId、算法 ed25519，但签名与内容不符 —— 验签必须失败且**不落盘**。
        let root = temp_root();
        let manifest = pack_json("p", "1.0.0", 1, "bin/p", &["kiro"]);
        let target = crate::plugin_host::process::host_target_triple();
        // 用测试密钥按正确载荷签，但把包里的二进制换掉 -> 载荷不匹配
        let (_, signature_b64, _public) = sign_pack(&manifest, b"original", &target, &[]);
        let tampered = with_signature(&manifest, &signature_b64, &target);
        let bytes = make_zip(&[("pack.json", tampered.as_bytes()), ("bin/p", b"tampered")]);
        // 测试密钥不在 OFFICIAL_PUBKEYS 里 → 「密钥不在信任列表」即 Invalid；
        // 即便将来把测试密钥加入信任表，载荷不匹配仍会 Invalid。两种情况都必须
        // 拒绝且不落盘 —— 这里验证的正是这个硬性质。
        let err = install_from_zip(&bytes, &root, None).unwrap_err();
        assert!(matches!(err, InstallError::Manifest(_)));
        assert!(err.to_string().contains("验签"), "{err}");
        assert!(!root.join("p").exists(), "验签失败的包不能出现在插件目录");
    }

    #[test]
    fn invalid_signature_but_other_target_is_not_rejected() {
        // 别的平台的签名（哪怕无效值）不算「本平台验不过」，按未签名放行。
        let root = temp_root();
        let manifest = pack_json("p", "1.0.0", 1, "bin/p", &["kiro"]);
        let signed = with_signature(&manifest, "AAAA", "another-platform");
        let bytes = make_zip(&[("pack.json", signed.as_bytes()), ("bin/p", b"x")]);
        let meta = install_from_zip(&bytes, &root, None).unwrap();
        assert!(!meta.signed, "别的平台的签名不算已签名");
    }

    #[test]
    fn signing_pack_list_is_compilable() {
        // 复现签名一侧的调用形状（official-2026 之外不存在私钥，只验证
        // 函数可调用、返回非空载荷）。
        let manifest = pack_json("p", "1.0.0", 1, "bin/p", &["kiro"]);
        let (_json, signature_b64, public) =
            sign_pack(&manifest, b"x", "x86_64-unknown-linux-gnu", &[]);
        assert!(!signature_b64.is_empty());
        assert_eq!(public.len(), 32);
    }
}
