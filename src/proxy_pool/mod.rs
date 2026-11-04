//! 网络代理池（对照 Agent2API 的 `core::proxy_pool` + ds2api 的 `admin/proxies`）。
//!
//! ## 设计（去掉 Clash / 订阅镜像后的最小完整版）
//!
//! 改造前 AIGX 的账号代理只能逐账号手填一个代理 URL 串：同一个出口用在
//! 三个账号上要填三遍、改一次端口要改三处，也看不到「这个出口通不通、
//! 出口 IP 是多少」。代理池把「出口」抽成独立实体：配一次、测一次、
//! 多个账号引用，账号记录里只留一个 `pool:<id>` 引用。
//!
//! ## 存储
//!
//! 条目数量级是几个到几十个、无查询需求（永远是整份读、整份写），
//! 与 `account_rotation_policy` 同类零散状态 —— 落 FileStore KV 的固定键
//! [`KV_KEY`]（SQLite / PG 两后端通用），不新增表。
//!
//! ## 账号引用编码（`AccountSession.proxy` 字段的扩展协议）
//!
//! AIGX 的账号代理字段是 `Option<String>`（URL 串），为兼容存量数据扩展
//! 前缀编码，解析统一走 [`resolve_proxy`]：
//!
//! | 账号 proxy 值 | 语义 |
//! |---|---|
//! | `None` / 空串 | 直连 |
//! | `pool:<id>` | 引用池条目（条目被删/禁用时**报错**而不是静默直连） |
//! | `rotate` | 轮询池里全部启用条目（逐请求换出口 = 换 IP） |
//! | `rotate:<id>,<id>,…` | 轮询指定条目（最多 64 条） |
//! | 其余（`http://` / `socks5://` …） | 原样当代理 URL（存量数据零迁移） |
//!
//! ## 轮询档（对照 A2A 的 `core::proxy_rotate`）
//!
//! 轮询的目的是摊开出口 IP（上游按 IP 限流 / 风控的场景）。游标是
//! **进程级、按账号各一条**：落库没有意义（重启后从头转完全不影响正确性，
//! 落库反而要处理「条目被删后游标指向不存在的条目」）。取用是先取号再
//! 解析：某条解析失败（被禁用）就顺延下一条，最多绕一圈 —— 池里有一条
//! 坏的不会让轮询停在坏条目上，也不会让请求直接失败。
//!
//! `rotate`（无参数）的含义是「**此刻**池里全部启用的出口」：池里增删条目
//! 后候选集合自然跟着变，不必回写账号记录。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use crate::storage::FileStore;

pub mod clash;

/// KV 键名（整份 JSON 数组读写）。
pub const KV_KEY: &str = "proxy_pool_items";

/// 名称 / 主机 / 用户名的长度上限（照 A2A 的量级）。
const MAX_NAME_LENGTH: usize = 60;
const MAX_HOST_LENGTH: usize = 255;
const MAX_USER_LENGTH: usize = 200;
/// 密码长度上限。
const MAX_PASSWORD_LENGTH: usize = 200;

/// 轮询指定条目的 id 数上限（照 A2A `proxy_rotate::MAX_IDS`）。
pub const MAX_ROTATE_IDS: usize = 64;

/// 轮询游标表的大小上限（超出时整表清空；键是账号 id，几十个是正常规模）。
const MAX_CURSORS: usize = 512;

/// 进程级存储句柄（main.rs 启动时 install 一次；测试里也用它）。
static STORE: OnceLock<std::sync::Arc<FileStore>> = OnceLock::new();

/// 测试并发时各模块私有锁互不管用，统一拿这一把（与 secret::test_lock 同思路）。
#[cfg(test)]
pub static TEST_LOCK: Mutex<()> = Mutex::new(());

/// 接线（main.rs 启动时调用）。重复调用无副作用。
pub fn install(store: std::sync::Arc<FileStore>) {
    let _ = STORE.set(store);
}

fn store() -> Result<&'static std::sync::Arc<FileStore>, String> {
    STORE
        .get()
        .ok_or_else(|| "proxy pool storage not installed".to_string())
}

/// 出口连通性测试的默认目标（照 A2A 的取舍：判据是「能连上一个常规
/// HTTPS 站点」，拿到任意 HTTP 响应（含 401/404）就算通）。
const DEFAULT_TEST_URL: &str = "https://www.cloudflare.com/cdn-cgi/trace";
/// 测试超时（毫秒）。
const TEST_TIMEOUT_MS: u64 = 10_000;
/// 出口 IP 回显服务（测试成功后顺带查出口 IP；失败一律吞掉——
/// 查不到 IP 不代表出口不可用）。
const IP_ECHO_URL: &str = "https://api.ipify.org?format=json";

// ─── 数据模型 ───────────────────────────────────────────────────────

/// 池条目（手动录入；协议 http / socks5，地址 + 可选认证）。
///
/// `source` 区分条目来源：`manual`（手填）/ `clash`（从 Clash Verge 镜像）。
/// `listener_uid` 仅 clash 来源有值，是 Clash Verge 监听器的稳定标识。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyEntry {
    pub id: String,
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// `manual`（默认，兼容存量数据）或 `clash`（镜像自 Clash Verge）。
    #[serde(default)]
    pub source: String,
    /// clash 来源条目的监听器 uid（manual 来源为空）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub listener_uid: String,
    /// `http` 或 `socks5`（socks5h 由 reqwest 的 socks 支持自动覆盖远端解析）。
    #[serde(default)]
    pub protocol: String,
    pub host: String,
    pub port: u16,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
    /// 上次测试结果（历史事实，不参与选路；编辑配置后保留）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_test: Option<LastTest>,
}

fn default_true() -> bool {
    true
}

/// 上次测试结果（照 A2A：重启、切页后仍能看到「上次测出来是什么」）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LastTest {
    pub success: bool,
    #[serde(default)]
    pub ip: String,
    pub duration_ms: u64,
    #[serde(default)]
    pub error: Option<String>,
    pub tested_at: i64,
}

/// 新建 / 编辑请求体（管理 API 的写入口形状）。
#[derive(Debug, Default, Deserialize)]
pub struct EntryInput {
    /// 编辑时必带（新建忽略）。
    #[serde(default)]
    pub id: String,
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub protocol: String,
    pub host: String,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub username: String,
    /// 编辑时留空 = 保留旧密码（与渠道 api_key 的三态口径一致：密码不回传，
    /// 前端编辑时不回显，留空提交即「不改」）。
    #[serde(default)]
    pub password: String,
}

// ─── 存取 ───────────────────────────────────────────────────────────

/// 读全部条目（库不可用 / 解析失败按空表处理——被外部工具改坏时页面表现为
/// 「池空了」，比整页报错更可恢复）。
pub fn read_entries() -> Vec<ProxyEntry> {
    STORE
        .get()
        .and_then(|s| s.get::<Vec<ProxyEntry>>(KV_KEY).ok())
        .flatten()
        .unwrap_or_default()
}

/// 整份写回（条目数量小，整份重写开销可忽略）。
fn write_entries(entries: &[ProxyEntry]) -> Result<(), String> {
    let store = store()?;
    store
        .put(KV_KEY, &entries.to_vec())
        .map_err(|e| format!("save proxy pool: {e}"))
}

/// 公开形态（密码不回传，只回 has_password —— 与渠道 api_key 脱敏同口径；
/// 编辑时密码留空 = 保留旧值）。
fn public(entry: &ProxyEntry) -> serde_json::Value {
    serde_json::json!({
        "id": entry.id,
        "name": entry.name,
        "enabled": entry.enabled,
        "source": if entry.source.is_empty() { "manual" } else { entry.source.as_str() },
        "listenerUid": entry.listener_uid,
        "protocol": entry.protocol,
        "host": entry.host,
        "port": entry.port,
        "username": entry.username,
        "has_password": !entry.password.trim().is_empty(),
        "createdAt": entry.created_at,
        "updatedAt": entry.updated_at,
        "lastTest": entry.last_test,
    })
}

/// 列表（公开形态）。
pub fn list_public() -> Vec<serde_json::Value> {
    read_entries().iter().map(public).collect()
}

fn find(id: &str) -> Option<ProxyEntry> {
    if id.is_empty() {
        return None;
    }
    read_entries().into_iter().find(|e| e.id == id)
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// 条目 id：`px_<毫秒>_<8 位随机>`（A2A 同手法，不引依赖）。
fn new_id() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u64(now() as u64);
    hasher.write_u64(std::process::id() as u64);
    let suffix = (hasher.finish() & 0xffff_ffff) as u32;
    format!("px_{}_{suffix:08x}", now())
}

fn clean(input: &str, max: usize) -> String {
    input.trim().chars().take(max).collect()
}

/// 校验并归一输入（新建传 `existing = None`；编辑时沿用 id / created_at /
/// last_test，密码留空沿用旧值）。
fn normalize(input: &EntryInput, existing: Option<&ProxyEntry>) -> Result<ProxyEntry, String> {
    let name = clean(&input.name, MAX_NAME_LENGTH);
    if name.is_empty() {
        return Err("请填写代理名称".to_string());
    }
    let protocol = {
        let p = clean(&input.protocol, 10).to_lowercase();
        if p.is_empty() {
            "http".to_string()
        } else {
            p
        }
    };
    if protocol != "http" && protocol != "socks5" && protocol != "https" {
        return Err("代理协议只支持 http / https / socks5".to_string());
    }
    let host = clean(&input.host, MAX_HOST_LENGTH);
    if host.is_empty() {
        return Err("请填写代理主机地址".to_string());
    }
    let port = input.port.ok_or("代理端口必须是 1-65535 的整数")?;
    if port == 0 {
        return Err("代理端口必须是 1-65535 的整数".to_string());
    }
    let timestamp = now();
    let password = if input.password.is_empty() {
        existing.map(|e| e.password.clone()).unwrap_or_default()
    } else {
        clean(&input.password, MAX_PASSWORD_LENGTH)
    };
    Ok(ProxyEntry {
        id: existing
            .map(|e| e.id.clone())
            .filter(|id| !id.is_empty())
            .unwrap_or_else(new_id),
        name,
        enabled: input.enabled,
        // 保留已有条目的来源（clash 镜像条目被手动编辑时不降级为 manual）
        source: existing
            .map(|e| e.source.clone())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "manual".to_string()),
        listener_uid: existing.map(|e| e.listener_uid.clone()).unwrap_or_default(),
        protocol,
        host,
        port,
        username: clean(&input.username, MAX_USER_LENGTH),
        password,
        created_at: existing.map(|e| e.created_at).unwrap_or(timestamp),
        updated_at: timestamp,
        last_test: existing.and_then(|e| e.last_test.clone()),
    })
}

// ─── 写操作 ─────────────────────────────────────────────────────────

/// 新建（新条目排最前——「新加的看得见」）。
pub fn create(input: &EntryInput) -> Result<serde_json::Value, String> {
    let entry = normalize(input, None)?;
    let mut entries = read_entries();
    entries.insert(0, entry.clone());
    write_entries(&entries)?;
    tracing::info!("[ProxyPool] 新增代理「{}」", entry.name);
    Ok(public(&entry))
}

/// 编辑（body 带 id；不存在 → None）。
pub fn update(input: &EntryInput) -> Result<serde_json::Value, String> {
    let id = input.id.trim().to_string();
    if id.is_empty() {
        return Err("缺少代理 id".to_string());
    }
    let mut entries = read_entries();
    let existing = entries
        .iter()
        .find(|e| e.id == id)
        .cloned()
        .ok_or_else(|| format!("代理「{id}」不存在或已被删除"))?;
    let entry = normalize(input, Some(&existing))?;
    *entries
        .iter_mut()
        .find(|e| e.id == id)
        .expect("found above") = entry.clone();
    write_entries(&entries)?;
    tracing::info!("[ProxyPool] 更新代理「{}」", entry.name);
    Ok(public(&entry))
}

/// 启用 / 禁用（只碰开关这一个字段）。
pub fn set_enabled(id: &str, enabled: bool) -> Result<serde_json::Value, String> {
    let mut entries = read_entries();
    let entry = entries
        .iter_mut()
        .find(|e| e.id == id)
        .ok_or_else(|| format!("代理「{id}」不存在或已被删除"))?;
    entry.enabled = enabled;
    entry.updated_at = now();
    let result = public(entry);
    write_entries(&entries)?;
    Ok(result)
}

/// 删除。引用检查（防悬空）由 API 层做（要读账号池）。
pub fn remove(id: &str) -> Result<String, String> {
    let mut entries = read_entries();
    let before = entries.len();
    entries.retain(|e| e.id != id);
    if entries.len() == before {
        return Err(format!("代理「{id}」不存在或已被删除"));
    }
    write_entries(&entries)?;
    Ok(id.to_string())
}

/// 记录一次测试结果（失败只影响展示，不影响选路）。
pub fn record_test(id: &str, result: &LastTest) -> Result<(), String> {
    let mut entries = read_entries();
    let entry = entries
        .iter_mut()
        .find(|e| e.id == id)
        .ok_or_else(|| format!("代理「{id}」不存在或已被删除"))?;
    entry.last_test = Some(result.clone());
    entry.updated_at = now();
    write_entries(&entries)
}

// ─── Clash Verge 同步 ────────────────────────────────────────────────

/// 同步结果：变更条数 + 失败原因。
///
/// 「Clash 没装」不是错误路径 —— 池原样保留、不算失败，
/// 只在 `error` 字段给一句说明。
pub struct ClashSyncReport {
    pub changes: usize,
    pub error: Option<String>,
}

/// 把 Clash 当前出口集合整体镜像进池。
///
/// - 手动条目（`source != "clash"`）从不被碰。
/// - 镜像条目按 `listener_uid` 匹配：Clash 改了名称/启用状态就更新，
///   新增的监听器就插入，消失的就删除（严格镜像）。
/// - clash 条目的 host/port 在同步时落库（指向 127.0.0.1:Clash端口）；
///   Clash 改端口后需重新同步（3 秒 TTL 缓存保证 GET 列表读到最新）。
pub fn sync_clash() -> ClashSyncReport {
    let snapshot = clash::clash_snapshot();
    if !snapshot.available {
        return ClashSyncReport {
            changes: 0,
            error: Some(
                snapshot
                    .error
                    .unwrap_or_else(|| clash::CLASH_UNAVAILABLE.to_string()),
            ),
        };
    }

    // 目标集合：混合端口排在最前
    let mut targets: Vec<(String, String, u16, bool)> = Vec::new();
    if let Some(port) = snapshot.mixed_port {
        targets.push((
            clash::CLASH_MIXED_UID.to_string(),
            "Clash 混合端口（按规则分流）".to_string(),
            port,
            true,
        ));
    }
    for listener in &snapshot.listeners {
        targets.push((
            listener.uid.clone(),
            listener.name.clone(),
            listener.port,
            listener.enabled,
        ));
    }

    let now_ts = now();
    let mut entries = read_entries();
    let mut changes = 0usize;

    // ① 目标集合逐条 upsert
    for (uid, name, port, enabled) in &targets {
        let existing = entries
            .iter()
            .position(|e| e.source == "clash" && e.listener_uid == *uid);
        match existing {
            Some(index) => {
                if entries[index].name != *name || entries[index].enabled != *enabled {
                    entries[index].name = name.clone();
                    entries[index].enabled = *enabled;
                    entries[index].updated_at = now_ts;
                    changes += 1;
                }
            }
            None => {
                entries.insert(
                    0,
                    ProxyEntry {
                        id: new_id(),
                        name: name.clone(),
                        enabled: *enabled,
                        source: "clash".to_string(),
                        listener_uid: uid.clone(),
                        protocol: "http".to_string(),
                        host: "127.0.0.1".to_string(),
                        port: *port,
                        username: String::new(),
                        password: String::new(),
                        created_at: now_ts,
                        updated_at: now_ts,
                        last_test: None,
                    },
                );
                changes += 1;
            }
        }
    }

    // ② Clash 侧已消失的镜像条目摘掉（严格镜像）
    let before = entries.len();
    let target_uids: std::collections::HashSet<&str> =
        targets.iter().map(|(uid, ..)| uid.as_str()).collect();
    entries.retain(|e| {
        if e.source != "clash" {
            return true;
        }
        !e.listener_uid.is_empty() && target_uids.contains(e.listener_uid.as_str())
    });
    let removed = before - entries.len();
    if removed > 0 {
        changes += removed;
    }

    if changes > 0 {
        let _ = write_entries(&entries);
    }

    ClashSyncReport {
        changes,
        error: None,
    }
}

// ─── 解析（账号 proxy 字段 → 可用的代理 URL）────────────────────────

/// 条目 → 代理 URL（reqwest 可直接消费的形态；带认证时拼 userinfo）。
///
/// 公开给测试链路用：测试不看禁用位（「先测通、再启用」），直拼即可。
pub fn entry_url(entry: &ProxyEntry) -> String {
    if entry.username.is_empty() && entry.password.is_empty() {
        format!("{}://{}:{}", entry.protocol, entry.host, entry.port)
    } else {
        format!(
            "{}://{}:{}@{}:{}",
            entry.protocol, entry.username, entry.password, entry.host, entry.port
        )
    }
}

/// 引用解析（`pool:<id>`；禁用 / 不存在都报错——静默直连会让账号流量
/// 从错误的出口 IP 发出，那比失败更糟）。
fn resolve_reference(id: &str) -> Result<String, String> {
    let entry = find(id).ok_or_else(|| format!("代理「{id}」不存在或已被删除"))?;
    if !entry.enabled {
        return Err(format!(
            "代理「{}」已禁用（在「网络代理」页启用它）",
            entry.name
        ));
    }
    Ok(entry_url(&entry))
}

/// 轮询游标表（进程级；键 = 账号 id）。
static CURSORS: Mutex<Option<HashMap<String, u64>>> = Mutex::new(None);

fn take_cursor(key: &str, len: usize) -> usize {
    let mut guard = match CURSORS.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    let cursors = guard.get_or_insert_with(HashMap::new);
    if cursors.len() >= MAX_CURSORS && !cursors.contains_key(key) {
        cursors.clear();
    }
    let entry = cursors.entry(key.to_string()).or_insert(0);
    let start = (*entry as usize) % len;
    *entry = entry.wrapping_add(1);
    start
}

/// 轮询候选 id（`rotate` = 池里全部启用条目；`rotate:<ids>` = 指定条目
/// 中仍存在的，按存储顺序）。
fn rotate_candidates(spec: &str) -> Vec<String> {
    if spec.is_empty() {
        return read_entries()
            .iter()
            .filter(|e| e.enabled)
            .map(|e| e.id.clone())
            .collect();
    }
    let wanted: Vec<&str> = spec.split(',').map(str::trim).collect();
    read_entries()
        .into_iter()
        .filter(|e| wanted.contains(&e.id.as_str()))
        .map(|e| e.id)
        .collect()
}

/// 从 `start` 起绕一圈找第一条能解析的候选；全失败时给出第一条的原因。
fn pick_at(ids: &[String], start: usize, empty_reason: &str) -> Result<String, String> {
    if ids.is_empty() {
        return Err(empty_reason.to_string());
    }
    let mut first_error: Option<String> = None;
    for offset in 0..ids.len() {
        match resolve_reference(&ids[(start + offset) % ids.len()]) {
            Ok(url) => return Ok(url),
            Err(reason) => {
                if first_error.is_none() {
                    first_error = Some(reason);
                }
            }
        }
    }
    Err(format!(
        "轮询的出口都不可用：{}",
        first_error.unwrap_or_else(|| "没有可用的代理".to_string())
    ))
}

/// 账号 proxy 字段的统一解析入口。
///
/// 返回 `Ok(None)` = 直连；`Ok(Some(url))` = 代理 URL（交给
/// `EgressCache::client` 缓存建连）；`Err(原因)` = 配置坏了（调用方
/// 按配置错误报出，不静默直连）。
///
/// `account_key` 用于轮询游标归属（同一账号各转各的；空串退化为取第一条）。
pub fn resolve_proxy(proxy: Option<&str>, account_key: &str) -> Result<Option<String>, String> {
    let Some(raw) = proxy.map(str::trim).filter(|p| !p.is_empty()) else {
        return Ok(None);
    };
    if let Some(id) = raw.strip_prefix("pool:") {
        let id = id.trim();
        if id.is_empty() {
            return Err("代理引用缺少条目 id".to_string());
        }
        return resolve_reference(id).map(Some);
    }
    if let Some(spec) = raw.strip_prefix("rotate") {
        // `rotate`（无冒号）= 全部启用；`rotate:` 后为空也按全部（宽容脏数据）
        let spec = spec.strip_prefix(':').unwrap_or("").trim();
        let ids = rotate_candidates(spec);
        let empty_reason = if spec.is_empty() {
            "轮询没有可用出口：「网络代理」页还没有启用的代理".to_string()
        } else {
            "轮询没有可用出口：账号设置里选中的代理都不在了（可能已被删除）".to_string()
        };
        let start = if ids.is_empty() || account_key.is_empty() {
            0
        } else {
            take_cursor(account_key, ids.len())
        };
        return pick_at(&ids, start, &empty_reason).map(Some);
    }
    // 存量 / 直接填写的代理 URL 原样透传（合法性由 EgressCache 建连时把关）
    Ok(Some(raw.to_string()))
}

/// 不前进游标的静态描述（账号列表展示用：这条配置现在大概走哪儿）。
pub fn describe_proxy(proxy: Option<&str>) -> String {
    match resolve_proxy(proxy, "") {
        Ok(None) => String::new(),
        Ok(Some(url)) => {
            // 描述里不泄露认证信息
            let (scheme, rest) = url.split_once("://").unwrap_or(("", &url));
            match rest.rsplit_once('@') {
                Some((_, hostport)) => format!("{scheme}://{hostport}"),
                None => url.clone(),
            }
        }
        Err(reason) => format!("⚠️ {reason}"),
    }
}

// ─── 连通性测试 ─────────────────────────────────────────────────────

/// 测试结果。
#[derive(Debug, Clone, Serialize)]
pub struct TestResult {
    pub success: bool,
    pub status: Option<u16>,
    pub ip: String,
    pub duration_ms: u64,
    pub error: Option<String>,
}

/// 测试一个出口（判据：能拿到任意 HTTP 响应；成功后顺带查出口 IP）。
///
/// `proxy_url` 为 `None` = 测直连。
pub async fn test_connectivity(proxy_url: Option<&str>) -> TestResult {
    let started = std::time::Instant::now();
    let mut builder = reqwest::Client::builder()
        .http1_only()
        .timeout(Duration::from_millis(TEST_TIMEOUT_MS))
        .connect_timeout(Duration::from_secs(8));
    if let Some(url) = proxy_url {
        match reqwest::Proxy::all(url) {
            Ok(p) => builder = builder.proxy(p),
            Err(e) => {
                return TestResult {
                    success: false,
                    status: None,
                    ip: String::new(),
                    duration_ms: started.elapsed().as_millis() as u64,
                    error: Some(format!("invalid proxy '{url}': {e}")),
                }
            }
        }
    }
    let client = match builder.build() {
        Ok(c) => c,
        Err(e) => {
            return TestResult {
                success: false,
                status: None,
                ip: String::new(),
                duration_ms: started.elapsed().as_millis() as u64,
                error: Some(format!("build client: {e}")),
            }
        }
    };
    let response = match client
        .get(DEFAULT_TEST_URL)
        .header("Accept", "text/plain")
        .timeout(Duration::from_millis(TEST_TIMEOUT_MS))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            let message = if e.is_timeout() {
                format!("连接超时（{} 秒）", TEST_TIMEOUT_MS / 1000)
            } else {
                describe_error_detail(&e)
            };
            return TestResult {
                success: false,
                status: None,
                ip: String::new(),
                duration_ms: started.elapsed().as_millis() as u64,
                error: Some(message),
            };
        }
    };
    let status = response.status().as_u16();
    drop(response);
    let ip = read_exit_ip(&client).await;
    TestResult {
        success: true,
        status: Some(status),
        ip,
        duration_ms: started.elapsed().as_millis() as u64,
        error: None,
    }
}

/// reqwest 错误链的可读描述（连接类错误常挂在 source 上；去重防重复文案）。
fn describe_error_detail(error: &reqwest::Error) -> String {
    let mut parts: Vec<String> = Vec::new();
    let head = error.to_string();
    if !head.is_empty() {
        parts.push(head);
    }
    let mut current: Option<&(dyn std::error::Error + 'static)> = std::error::Error::source(error);
    while let Some(source) = current {
        let text = source.to_string();
        if !text.is_empty() && !parts.iter().any(|part| part == &text) {
            parts.push(text);
        }
        current = source.source();
    }
    if parts.is_empty() {
        return "未知错误".to_string();
    }
    parts.join(" → ")
}

async fn read_exit_ip(client: &reqwest::Client) -> String {
    let Ok(response) = client
        .get(IP_ECHO_URL)
        .header("Accept", "application/json")
        .timeout(Duration::from_millis(5_000))
        .send()
        .await
    else {
        return String::new();
    };
    if !response.status().is_success() {
        return String::new();
    }
    let Ok(text) = response.text().await else {
        return String::new();
    };
    match serde_json::from_str::<serde_json::Value>(&text) {
        Ok(value) => value
            .get("ip")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_default(),
        Err(_) => text.trim().chars().take(64).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 轻量存储（tempdir；测试全拿 TEST_LOCK 串行）。
    fn install_temp() -> &'static std::sync::Arc<FileStore> {
        static INIT: OnceLock<std::sync::Arc<FileStore>> = OnceLock::new();
        INIT.get_or_init(|| {
            let dir = std::env::temp_dir().join(format!("aigx-pp-test-{}", std::process::id()));
            std::sync::Arc::new(FileStore::open(dir).expect("open temp store"))
        })
    }

    fn sample(name: &str, port: u16) -> EntryInput {
        EntryInput {
            id: String::new(),
            name: name.to_string(),
            enabled: true,
            protocol: "http".to_string(),
            host: "127.0.0.1".to_string(),
            port: Some(port),
            username: String::new(),
            password: String::new(),
        }
    }

    #[test]
    fn create_list_remove_roundtrip() {
        let _guard = TEST_LOCK.lock();
        let _ = STORE.set(install_temp().clone());
        let created = create(&sample("节点A", 8080)).expect("create");
        let id = created["id"].as_str().unwrap().to_string();
        assert!(!list_public().is_empty());
        assert!(remove(&id).is_ok());
        assert!(remove(&id).is_err());
    }

    #[test]
    fn update_keeps_password_when_blank() {
        let _guard = TEST_LOCK.lock();
        let _ = STORE.set(install_temp().clone());
        let mut input = sample("节点B", 8081);
        input.password = "secret".to_string();
        let created = create(&input).expect("create");
        let id = created["id"].as_str().unwrap().to_string();
        // 编辑：不带密码 → 保留
        let mut edit = sample("节点B改", 8082);
        edit.id = id.clone();
        let updated = update(&edit).expect("update");
        assert_eq!(updated["name"], "节点B改");
        let entry = find(&id).unwrap();
        assert_eq!(entry.password, "secret");
        assert_eq!(entry.port, 8082);
    }

    #[test]
    fn validation_rejects_bad_input() {
        let _guard = TEST_LOCK.lock();
        let _ = STORE.set(install_temp().clone());
        let mut no_name = sample("", 8080);
        no_name.name = String::new();
        assert!(create(&no_name).is_err());
        let mut no_port = sample("X", 8080);
        no_port.port = None;
        assert!(create(&no_port).is_err());
        let mut bad_proto = sample("X", 8080);
        bad_proto.protocol = "grpc".to_string();
        assert!(create(&bad_proto).is_err());
    }

    #[test]
    fn resolve_pool_reference() {
        let _guard = TEST_LOCK.lock();
        let _ = STORE.set(install_temp().clone());
        let created = create(&sample("引用目标", 9090)).expect("create");
        let id = created["id"].as_str().unwrap().to_string();
        let url = resolve_proxy(Some(&format!("pool:{id}")), "acct").expect("resolve");
        assert_eq!(url.unwrap(), "http://127.0.0.1:9090");
        // 禁用后解析报错
        set_enabled(&id, false).expect("disable");
        assert!(resolve_proxy(Some(&format!("pool:{id}")), "acct").is_err());
        set_enabled(&id, true).expect("enable");
        // 不存在的条目
        assert!(resolve_proxy(Some("pool:px_missing"), "acct").is_err());
        // 删除后
        remove(&id).expect("remove");
        assert!(resolve_proxy(Some(&format!("pool:{id}")), "acct").is_err());
    }

    #[test]
    fn resolve_rotate_and_legacy_url() {
        let _guard = TEST_LOCK.lock();
        let _ = STORE.set(install_temp().clone());
        let a = create(&sample("轮询A", 9101)).expect("create");
        let b = create(&sample("轮询B", 9102)).expect("create");
        let ida = a["id"].as_str().unwrap().to_string();
        let idb = b["id"].as_str().unwrap().to_string();
        // 全部档：两条候选
        let url = resolve_proxy(Some("rotate"), "acct").expect("rotate");
        assert!(url.is_some());
        // 指定档
        let url = resolve_proxy(Some(&format!("rotate:{ida},{idb}")), "acct").expect("rotate ids");
        assert!(url.is_some());
        // 空指定 → 报错
        assert!(resolve_proxy(Some("rotate:px_missing"), "acct").is_err());
        // 存量 URL 原样透传
        let url = resolve_proxy(Some("socks5://u:p@1.2.3.4:1080"), "acct").expect("legacy");
        assert_eq!(url.as_deref(), Some("socks5://u:p@1.2.3.4:1080"));
        // 直连
        assert_eq!(resolve_proxy(None, "acct").unwrap(), None);
        assert_eq!(resolve_proxy(Some("  "), "acct").unwrap(), None);
    }

    #[test]
    fn describe_masks_credentials() {
        let url = describe_proxy(Some("socks5://user:pass@1.2.3.4:1080"));
        assert_eq!(url, "socks5://1.2.3.4:1080");
        assert!(!url.contains("user"));
    }
}
