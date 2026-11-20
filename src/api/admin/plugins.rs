//! 插件管理面 API（`/api/plugins`，对照 Agent2API `api/plugins.rs` 的管理面）。
//!
//! ## 两层形态
//!
//! - **内置包**（`builtin`）：编译进二进制的 provider 目录。启停走 provider
//!   级闸门（`ProviderGates`，数据面选号过滤读），整包停用/卸载如实报不支持；
//! - **外置包**：运维从 `config.toml [plugins] source_url` 或直接给 URL
//!   安装的独立二进制包（一包 = 一子进程 = 若干家 provider，见
//!   `crate::plugin_host`）。整包启停/卸载由 `PluginHostManager` 执行，
//!   进程崩溃按退避表自动重拉（M4）。
//!
//! ## 契约
//!
//! 列表/安装的 JSON 形状对齐前端 `PluginPackSummary` / `PluginsPayload`
//! / `StorePlugin`（见 `frontend/src/types/index.ts`），**形状不要改**。
//! A2A 对齐波（v1.0.63）只做**加字段**：包视图补 `icons` / `ui` / `homepage` /
//! `providerCategory` / `providerEnabled` / `startup`，顶层补 `maxPackBytes` /
//! `scripts` / `startup`。既有键一个不动。
//!
//! ## pack.json 扩展字段怎么读
//!
//! `icons`（图标**目录**相对路径）/ `ui`（页面清单）/ `homepage` /
//! `provider_category` 是上游 3.5 清单新增的键。本文件**直接读 `pack.json`
//! 的 JSON**（不依赖 `install::PackManifest` 的结构体字段），理由：
//! 清单格式是插件的对外契约、可随上游演进，网关只消费其中几把键；
//! 绑上结构体反而要求每加一个键就改一次解包类型。

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::Json;
use parking_lot::RwLock;
use serde::Deserialize;
use serde_json::{json, Value};

use super::common::{error_response, verify_admin};
use crate::api::openai::AppState;
use crate::plugin_host::install::MAX_PACK_BYTES;
use crate::plugin_host::{PackMeta, PluginHostManager};
use crate::storage::FileStore;

/// KV 键：provider 级停用名单（`Vec<provider kind>`）。
const DISABLED_PROVIDERS_KEY: &str = "plugins:disabled_providers";

/// 24 MiB zip 的 base64 长度上界（仅本地安装路由使用）。
const MAX_PACK_BASE64_BYTES: usize = MAX_PACK_BYTES.div_ceil(3) * 4;
/// 本地安装路由的请求体上限：base64 + 有限 JSON 开销（对齐 A2A）。
const MAX_PACK_UPLOAD_BYTES: usize = MAX_PACK_BASE64_BYTES + 1024 * 1024;

/// 发布目录拉取超时（`[plugins] source_url`）。目录是小 JSON，
/// 10s 足够；超时即降级为「插件源不可用」而不是拖住整个列表接口。
const CATALOG_TIMEOUT: Duration = Duration::from_secs(10);

/// 内置包 id（与前端展示名对齐；上游的包 id 是 `pack.json` 里的名字，
/// AIGX 没有外置包，固定一个）。
const BUILTIN_PACK_ID: &str = "builtin";
const BUILTIN_PACK_VERSION: &str = "1.0";

/// provider 停用名单（内存缓存 + KV 持久化，与 `model::manage::ModelRules`
/// 同一模式：写走 `persist`，读走内存）。
///
/// 数据面（`bridge::account` 的选号过滤）读这个内存集合，启停即时生效、
/// 无每请求 IO。
#[derive(Clone)]
pub struct ProviderGates {
    inner: Arc<Inner>,
}

struct Inner {
    disabled: RwLock<HashSet<String>>,
    store: Arc<FileStore>,
}

impl ProviderGates {
    pub fn new(store: Arc<FileStore>) -> Self {
        Self {
            inner: Arc::new(Inner {
                disabled: RwLock::new(HashSet::new()),
                store,
            }),
        }
    }

    /// 从 KV 加载（启动时一次）。
    pub fn load(&self) -> Result<(), String> {
        let raw = self
            .inner
            .store
            .get::<Vec<String>>(DISABLED_PROVIDERS_KEY)
            .map_err(|e| format!("读取 provider 停用名单失败: {e}"))?;
        let set = raw.unwrap_or_default().into_iter().collect();
        *self.inner.disabled.write() = set;
        Ok(())
    }

    /// 某家是否被停用（数据面选号过滤用；读内存无 IO）。
    pub fn is_disabled(&self, kind: &str) -> bool {
        self.inner.disabled.read().contains(kind)
    }

    /// 当前快照（列表视图用）。
    pub fn disabled_set(&self) -> HashSet<String> {
        self.inner.disabled.read().clone()
    }

    /// 设置启停；返回是否有变化。
    pub fn set_enabled(&self, kind: &str, enabled: bool) -> Result<bool, String> {
        let changed = {
            let mut set = self.inner.disabled.write();
            if enabled {
                set.remove(kind)
            } else {
                set.insert(kind.to_string())
            }
        };
        if changed {
            let list: Vec<String> = self.inner.disabled.read().iter().cloned().collect();
            self.inner
                .store
                .put(DISABLED_PROVIDERS_KEY, &list)
                .map_err(|e| format!("保存 provider 启停状态失败: {e}"))?;
        }
        Ok(changed)
    }
}

// ── 路由 ────────────────────────────────────────────────────────────

/// 本文件**自己接管**的那几条路由（`/api/plugins/startup|settings|reload`）。
///
/// 为什么单独出一个 `router()`：`main.rs` 的 `/api/plugins*` 那一段是
/// admin_routes 里的**平铺 `.route(...)`** 写法（历史原因），而 T2 新增的
/// 三个端点要跟同一段共存。把新端点收进这里，integrator 只需在
/// `.merge(...)` 区加一行；老的平铺行不用动（路由不重叠）。
pub fn router() -> axum::Router<AppState> {
    axum::Router::new()
        .route(
            "/api/plugins/startup",
            axum::routing::get(handle_plugin_startup),
        )
        .route(
            "/api/plugins/settings",
            axum::routing::get(handle_plugin_settings).patch(handle_save_plugin_settings),
        )
        .route(
            "/api/plugins/reload",
            axum::routing::post(handle_reload_plugin),
        )
        // 从发布目录安装/更新（A2A `install-from-release` / `POST /update`）：
        // 服务端解析当前平台的下载地址，起后台 Operation，进度走 /progress。
        .route(
            "/api/plugins/install-from-release",
            axum::routing::post(handle_install_from_release),
        )
        // 本地 zip 安装（A2A `/api/plugins/install` 的文件上传形态）。
        // 原始 zip 最大 24 MiB / base64 JSON 更大，路由级放大 body 上限，
        // 不影响全局 DefaultBodyLimit（无需改 main.rs）。
        .route(
            "/api/plugins/install-file",
            axum::routing::post(handle_install_file)
                .layer(axum::extract::DefaultBodyLimit::max(MAX_PACK_UPLOAD_BYTES)),
        )
    // beta.2 的 `/api/plugins/batch` 与 `/api/plugins/managed-task` 以平铺
    // `.route(...)` 注册在 `main.rs`（与 install/enable/progress 同一段），
    // 不在这里重复注册 —— axum 同一路径重复注册会在启动时 panic。
}

// ── 请求体 ──────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct PackIdBody {
    pub id: String,
    /// 包级启停目标（前端 `setPluginEnabled` 传 `{id, enabled}`）。
    /// 缺省 = 不解释：内置包路径不读这个字段（一律报错引导）；外置包
    /// 路径要求显式传（agent 工具 `aigx_plugin_manage` 只传 id、其
    /// enable/disable 动作本就只面向内置包，外置包场景返回 400 提示）。
    #[serde(default)]
    pub enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct ProviderBody {
    pub id: String,
    pub provider: String,
    pub enabled: bool,
}

/// 安装请求体（前端 `installPlugin(url, sha256)`）。
#[derive(Debug, Deserialize)]
pub struct InstallBody {
    /// 插件包 zip 的直链。
    pub url: String,
    /// 可选 sha256（源清单声明摘要时比对，挡「文件传坏了」）。
    #[serde(default)]
    pub sha256: Option<String>,
}

// ── 视图 ────────────────────────────────────────────────────────────

/// 全量 provider 目录（`PROVIDERS` ∪ `SESSION_PROVIDERS`）。
fn all_provider_kinds() -> Vec<&'static str> {
    let mut kinds: Vec<&'static str> = crate::account_session::registry::PROVIDERS
        .iter()
        .map(|spec| spec.kind)
        .collect();
    for (kind, _) in crate::account_session::registry::SESSION_PROVIDERS {
        if !kinds.contains(kind) {
            kinds.push(kind);
        }
    }
    kinds
}

/// 一家 provider 的启用/加载态（前端 `PluginPackProvider` 契约）。
///
/// `toggle` 是前端开关能力标志（前端按值渲染）：`"global"` = 本家开关为
/// 真实逐家启停（内置目录包写全局 `ProviderGates`；外置原生包写
/// `PackMeta.disabled_providers`）。`"none"` 目前无使用方，保留契约值。
fn provider_view(kind: &str, disabled: &HashSet<String>) -> Value {
    let label = crate::model::manage::provider_label(kind);
    json!({
        "id": kind,
        "label": label,
        // 停用名单里没有 = 启用
        "enabled": !disabled.contains(kind),
        // 内置实现：启用的家随进程常驻（registry 静态表），恒 loaded=enabled
        "loaded": !disabled.contains(kind),
        // 开关写入全局 ProviderGates（数据面选号即时生效）
        "toggle": "global",
    })
}

/// 内置包视图（前端 `PluginPackSummary` 契约）。
fn builtin_pack_view(disabled: &HashSet<String>) -> Value {
    let providers: Vec<Value> = all_provider_kinds()
        .iter()
        .map(|kind| provider_view(kind, disabled))
        .collect();
    json!({
        "id": BUILTIN_PACK_ID,
        "name": "内置供应商目录",
        "description": "AIGX 编译进二进制的 provider 目录（账号池 / 渠道的全部上游家）。停用某家 = 数据面选号时跳过它的账号，账号数据保留。",
        "version": BUILTIN_PACK_VERSION,
        "requiredVersion": null,
        "updateRequired": false,
        "compatibilityError": null,
        "enabled": true,
        "signed": true,
        "installedAt": 0,
        "entryPresent": true,
        "protocol": 1,
        "homepage": null,
        "icon": null,
        "providers": providers,
        // 内置实现无宿主进程
        "host": null,
        // 内置包混装各家，providerCategory 留空（前端只对外置包分色）
        "providerCategory": null,
        // A2A 对齐字段：内置目录无独立图标目录 / 页面 / 清单主页
        "icons": null,
        "ui": [],
        "providerEnabled": builtin_provider_enabled(disabled),
        "startup": null,
    })
}

/// 内置目录的包级 provider 启停展平（A2A `providerEnabled`：`{provider: bool}`）。
///
/// 内置包的 provider 级开关是 `ProviderGates` 的停用名单，逐个 provider
/// 表达成 `enabled` 布尔；没有记录 = 启用。
fn builtin_provider_enabled(disabled: &HashSet<String>) -> Value {
    let mut map = serde_json::Map::new();
    for kind in all_provider_kinds() {
        map.insert(kind.to_string(), Value::Bool(!disabled.contains(kind)));
    }
    Value::Object(map)
}

/// 外置包的 `pack.json` 清单子集（A2A 3.5 清单扩展键）。
///
/// 只读这几把键；读不到 / 解析失败一律返回 `None`（视图回落默认值，
/// 不报错——清单损坏由 `entryPresent` 与安装日志体现）。
#[derive(Debug, Default, Clone)]
struct PackManifestExtras {
    description: Option<String>,
    homepage: Option<String>,
    provider_category: Option<String>,
    /// 图标**目录**相对路径（清单 `icons` 键）。
    icons: Option<String>,
    /// 页面清单（清单 `ui` 键）。
    ui: Vec<Value>,
}

/// 读包版本目录里的 `pack.json` 扩展键（无缓存；缓存版见
/// [`manifest_extras_for`]）。保留无缓存版本供单测/基准直读对拍。
#[cfg(test)]
fn manifest_extras(version_dir: &std::path::Path) -> PackManifestExtras {
    let value = std::fs::read_to_string(version_dir.join("pack.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .unwrap_or(Value::Null);
    manifest_extras_from_value(&value)
}

/// 从已解析的 `pack.json` JSON 里取扩展键（缓存命中时直接复用解析结果）。
fn manifest_extras_from_value(value: &Value) -> PackManifestExtras {
    let string = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string)
    };
    PackManifestExtras {
        description: string("description"),
        homepage: string("homepage"),
        provider_category: string("provider_category"),
        // 图标声明：A2A 清单用 `icons`（目录），社区包也见 `icon`（单个路径）。
        // 两者都收，交给 `pack_icon` 判断是文件还是目录。
        icons: string("icons").or_else(|| string("icon")),
        ui: value
            .get("ui")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
    }
}

/// 列表视图缓存键：包 id + 版本 + `pack.json` 的 mtime/size。
///
/// 重装/同版本覆盖都会改写 `pack.json` → 指纹变化 → 旧缓存自动失效。
/// 读文件不存在时指纹为 `0|0`（「无清单」同样缓存，避免每轮反复尝试读盘）。
fn pack_view_key(meta: &PackMeta, version_dir: &std::path::Path) -> String {
    let (mtime_ns, size) = std::fs::metadata(version_dir.join("pack.json"))
        .ok()
        .map(|meta| {
            let ns = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            (ns, meta.len())
        })
        .unwrap_or((0, 0));
    format!("{}|{}|{mtime_ns}|{size}", meta.id, meta.version)
}

/// 带进程内缓存的 [`manifest_extras`]（hot path：列表轮询）。
fn manifest_extras_for(host: &PluginHostManager, meta: &PackMeta) -> PackManifestExtras {
    let version_dir = meta.version_dir(host.plugins_root());
    let key = format!("{}|manifest", pack_view_key(meta, &version_dir));
    if let Some(value) = host.view_cache().get_manifest(&key) {
        return manifest_extras_from_value(&value);
    }
    let value = std::fs::read_to_string(version_dir.join("pack.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .unwrap_or(Value::Null);
    let extras = manifest_extras_from_value(&value);
    host.view_cache().put_manifest(key, value);
    extras
}

/// 带进程内缓存的 [`pack_icon`]（hot path：列表轮询）。
///
/// 键沿用 `pack.json` 指纹 + 清单声明的图标路径；图标文件本身属于同一次
/// 安装产物，重装会改写 `pack.json`，故指纹足以失效。
fn pack_icon_for(
    host: &PluginHostManager,
    meta: &PackMeta,
    declared: Option<&str>,
) -> Option<String> {
    let version_dir = meta.version_dir(host.plugins_root());
    let key = format!(
        "{}|icon|{}",
        pack_view_key(meta, &version_dir),
        declared.unwrap_or("")
    );
    if let Some(value) = host.view_cache().get_icon(&key) {
        return value;
    }
    let value = pack_icon(&version_dir, declared);
    host.view_cache().put_icon(key, value.clone());
    value
}

/// 图标文件体积上限（2MB，A2A 同值）。
///
/// 包里图标是界面用的 44px 徽章，正常几十 KB。超过这个量级多半是把整张
/// 4K 原图塞进了包 —— 内联进列表响应会让面板每次刷新拖几 MB，宁可不显示
/// 图标（回落字母）。
const ICON_MAX_BYTES: u64 = 2 * 1024 * 1024;

/// 从版本目录读出包图标，转成 data URI。
///
/// ── 支持哪些清单形态 ─────────────────────────────────────────
/// 社区包的图标声明并不统一：`icons` 可能是**目录**（A2A 约定）、**单个
/// 文件路径**（`icon: "logo.svg"` / `icons: "assets/logo.png"`），也可能
/// 完全没声明但包内按常见布局放了图（`icons/`、`assets/` 或包根的
/// `logo.*` / `icon.*`）。只认「`icons` 是目录」会把后两类全部漏掉 ——
/// 这正是社区包「装了但图标不显示」的根因之一。
///
/// 查找优先级：清单声明的文件 → 清单声明的目录（`logo.*` → `icon.*` →
/// 目录内任意图）→ `icons/` → `assets/` → 包根（后三者只认 `logo`/`icon`
/// 前缀，避免把截图等无关图片当图标）。
///
/// 任何一步失败返回 `None`（界面回落字母）——图标是装饰，不该让列表失败。
/// 路径校验：声明不得是绝对路径 / 盘符 / 含 `..`；命中文件 canonicalize
/// 后必须仍在版本目录内。体积上限 [`ICON_MAX_BYTES`]。
fn pack_icon(version_dir: &std::path::Path, declared: Option<&str>) -> Option<String> {
    let root = version_dir.canonicalize().ok()?;
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if let Some(declared) = declared {
        if declared.starts_with(['/', '\\'])
            || declared.contains(':')
            || declared.split(['/', '\\']).any(|part| part == "..")
        {
            return None;
        }
        let target = version_dir.join(declared);
        if target.is_file() {
            candidates.push(target.clone());
        } else if target.is_dir() {
            push_icon_candidates(&mut candidates, &target, true);
        }
        // 声明无效 / 不存在时继续走兜底布局，而不是直接放弃。
    }
    for dir in ["icons", "assets", "."] {
        let candidate_dir = if dir == "." {
            version_dir.to_path_buf()
        } else {
            version_dir.join(dir)
        };
        if candidate_dir.is_dir() {
            push_icon_candidates(&mut candidates, &candidate_dir, false);
        }
    }
    let path = candidates
        .into_iter()
        .find(|path| path.is_file())?
        .canonicalize()
        .ok()?;
    if !path.starts_with(&root) {
        return None;
    }
    if std::fs::metadata(&path).ok()?.len() > ICON_MAX_BYTES {
        return None;
    }
    let bytes = std::fs::read(&path).ok()?;
    let mime = match path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => "image/png",
        "webp" => "image/webp",
        "jpg" | "jpeg" => "image/jpeg",
        "svg" => "image/svg+xml",
        "gif" => "image/gif",
        "ico" => "image/x-icon",
        _ => return None,
    };
    use base64::Engine;
    let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Some(format!("data:{mime};base64,{encoded}"))
}

/// 往候选表里追加一个目录下的图标路径：`logo.*` / `icon.*` 优先；
/// `include_any` 时再补齐目录内其余任意图片（排序保证确定性）。
fn push_icon_candidates(
    candidates: &mut Vec<std::path::PathBuf>,
    dir: &std::path::Path,
    include_any: bool,
) {
    for stem in ["logo", "icon"] {
        for ext in ["png", "webp", "jpg", "jpeg", "svg", "gif", "ico"] {
            candidates.push(dir.join(format!("{stem}.{ext}")));
        }
    }
    if include_any {
        if let Ok(entries) = std::fs::read_dir(dir) {
            let mut rest: Vec<std::path::PathBuf> = entries
                .filter_map(|item| item.ok())
                .map(|item| item.path())
                .filter(|path| path.is_file() && is_image_path(path))
                .collect();
            rest.sort();
            candidates.extend(rest);
        }
    }
}

/// 扩展名是不是界面能显示的图片。
fn is_image_path(path: &std::path::Path) -> bool {
    matches!(
        path.extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str(),
        "png" | "webp" | "jpg" | "jpeg" | "svg" | "gif" | "ico"
    )
}

/// 把包清单里的 UI 页面展开成客户端能直接用的形态（`url` 指向网关托管地址）。
///
/// `entry` 是包自己写的字符串。不校验的话，一个包可以写
/// `../../../../etc/passwd` 让托管端点去读包外的文件 —— 那是路径穿越。
/// 这里在**声明侧**就挡掉：必须是相对路径、不含 `..`、以 `.html` 结尾。
/// 托管端点（`plugin_ui::resolve_ui_file`）那边还会再挡一次（纵深防御）。
fn pack_ui_pages(pack_id: &str, pages: &[Value]) -> Vec<Value> {
    pages
        .iter()
        .filter_map(|page| {
            let entry = page.get("entry").and_then(Value::as_str)?;
            if !ui_entry_ok(entry) {
                return None;
            }
            let id = page
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .unwrap_or(entry);
            Some(json!({
                "id": id,
                "title": page.get("title").and_then(Value::as_str).unwrap_or(id),
                "icon": page.get("icon").and_then(Value::as_str),
                // 包 id 与入口都进 URL；前者安装期已限 `[a-z0-9-]`，后者上面已校验
                "url": format!("/plugins/{pack_id}/ui/{}", entry.replace('\\', "/")),
                // 原样转出插件申报的默认分组：分组 id 是客户端的事实，网关不解释
                "group": page.get("group").and_then(Value::as_str),
                "order": page.get("order").and_then(Value::as_i64).unwrap_or(0),
            }))
        })
        .collect()
}

/// UI 页面入口是否合法（相对、不含 `..`、以 `.html` 结尾）。
fn ui_entry_ok(entry: &str) -> bool {
    let normalized = entry.replace('\\', "/");
    if normalized.is_empty() || normalized.starts_with('/') || normalized.contains(':') {
        return false;
    }
    if normalized.split('/').any(|segment| segment == "..") {
        return false;
    }
    normalized.to_ascii_lowercase().ends_with(".html")
}

/// 某 provider 当前归属的已启用包 id（A2A `ownership::owners` 投影）。
///
/// 口径与数据面一致：包级 `enabled` ∧ 该家不在 `disabled_providers` 才算
/// 接管（[`PackMeta::provider_enabled`]）。用于在包视图里给「由别的插件
/// 接管」的家打标（前端据此禁掉逐家开关、显示接管方）。
fn provider_owner(host: &PluginHostManager, provider: &str) -> Option<String> {
    host.registry()
        .list()
        .into_iter()
        .find(|pack| {
            pack.provider_enabled(provider) && pack.providers.iter().any(|pp| pp.id == provider)
        })
        .map(|pack| pack.id)
}

/// 已装外置包相对当前发布目录的可用更新（对齐 A2A `catalog::updates_with_packs`）。
///
/// 每个「已安装 且 目录里收录」的外置包出一行：
/// `available` = 目录版本 **高于** 已装版本 **且** 当前平台有安装包；
/// `asset` 是当前平台那一份（供一键升级用）。目录不可用 / 未收录该包 →
/// 不出行（前端按「无更新」处理，不虚构「可更新」徽标）。
fn compute_updates(metas: &[PackMeta], catalog: &Value) -> Vec<Value> {
    let Some(plugins) = catalog.get("plugins").and_then(Value::as_array) else {
        return Vec::new();
    };
    let os = std::env::consts::OS;
    let mut rows = Vec::new();
    for meta in metas.iter().filter(|m| m.id != BUILTIN_PACK_ID) {
        let Some(entry) = plugins
            .iter()
            .find(|item| item.get("id").and_then(Value::as_str) == Some(meta.id.as_str()))
        else {
            continue;
        };
        let latest = entry.get("version").and_then(Value::as_str).unwrap_or("");
        let platform = entry
            .get("platforms")
            .and_then(Value::as_array)
            .and_then(|items| {
                items
                    .iter()
                    .find(|p| p.get("target").and_then(Value::as_str) == Some(os))
            });
        let newer =
            !latest.is_empty() && crate::update::compare_versions(latest, &meta.version) == Some(1);
        let available = newer && platform.is_some();
        let reason = if available {
            Value::Null
        } else if platform.is_none() {
            json!("当前平台暂无可用安装包")
        } else {
            json!("已经是最新版本")
        };
        let asset = match platform {
            Some(p) => json!({
                "target": p.get("target").cloned().unwrap_or(Value::Null),
                "size": p.get("size").cloned().unwrap_or_else(|| json!(0)),
                "url": p.get("url").cloned().unwrap_or(Value::Null),
                "current": false,
                "sha256": p.get("sha256").cloned().unwrap_or(Value::Null),
            }),
            None => Value::Null,
        };
        rows.push(json!({
            "id": meta.id,
            "available": available,
            "checked": true,
            "current": meta.version,
            "latest": if latest.is_empty() { Value::Null } else { json!(latest) },
            "notes": entry.get("notes").cloned().unwrap_or(Value::Null),
            "publishedAt": entry.get("publishedAt").cloned().unwrap_or(Value::Null),
            "prerelease": entry.get("prerelease").and_then(Value::as_bool).unwrap_or(false),
            // page 缺失时回落 homepage：两者在目录条目里指向同一发布页
            "page": entry.get("page").cloned()
                .or_else(|| entry.get("homepage").cloned())
                .unwrap_or(Value::Null),
            "asset": asset,
            "error": false,
            "reason": reason,
        }));
    }
    rows
}

/// 插件账号摘要（对齐 A2A `account_store::plugin_account_summary`）。
///
/// 返回 `(counts, hiddenAccounts)`：
/// - `counts` = `{provider: 账号数}`（前端批量条目的账号计数）；
/// - `hiddenAccounts` = `{total, byProvider, providers:[{id,label,count,reason,packId}]}`
///   —— 只含被管理面隐藏的账号（AIGX `AccountSession.hidden`）。`reason` 用
///   A2A 的 snake_case 口径（`not_installed` / `pack_disabled` /
///   `provider_disabled` / `not_loaded`），前端缺失插件横幅据此过滤。
fn plugin_account_summary(state: &AppState) -> (Value, Value) {
    let sessions = state.account_sessions.all();
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut hidden: BTreeMap<String, usize> = BTreeMap::new();
    for session in &sessions {
        *counts.entry(session.provider.clone()).or_insert(0) += 1;
        if session.hidden {
            *hidden.entry(session.provider.clone()).or_insert(0) += 1;
        }
    }
    let registry = state.plugin_host.registry();
    let providers: Vec<Value> = hidden
        .iter()
        .map(|(provider, count)| {
            json!({
                "id": provider,
                "label": crate::model::manage::provider_label(provider),
                "count": count,
                "reason": hidden_provider_reason(state, provider),
                "packId": registry.find_pack_for_provider(provider),
            })
        })
        .collect();
    let total: usize = hidden.values().sum();
    (
        json!(counts),
        json!({
            "total": total,
            "byProvider": hidden,
            "providers": providers,
        }),
    )
}

/// 隐藏账号的归属原因（A2A `HiddenReason` 的 AIGX 投影）。
fn hidden_provider_reason(state: &AppState, provider: &str) -> &'static str {
    if let Some(pack) = state
        .plugin_host
        .registry()
        .list()
        .into_iter()
        .find(|pack| pack.providers.iter().any(|pp| pp.id == provider))
    {
        if !pack.enabled {
            return "pack_disabled";
        }
        if !pack.provider_enabled(provider) {
            return "provider_disabled";
        }
        return "not_loaded";
    }
    if state.provider_gates.is_disabled(provider) {
        return "provider_disabled";
    }
    "not_installed"
}

/// 聚合运行期 provider 申报的注入点（A2A `adminPanels` / `accountFeatures`）。
///
/// 握手返回的 [`ProviderInfo`](crate::plugin_host::process::ProviderInfo) 现
/// 已完整保留插件声明；这里把已加载进程的申报投影成列表顶层字段：
/// - `adminPanels` = 各 provider 的管理面板 id 去重并集；
/// - `accountFeatures` = `{provider id -> accountFeatures}`（未申报的家不出现）。
///
/// 只统计**正在运行**的进程（`runtime_of().providers` 是握手快照），未加载的
/// 包不该在界面注入入口。
fn provider_injection_points(host: &PluginHostManager, metas: &[PackMeta]) -> (Value, Value) {
    let mut panels: Vec<Value> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut features = serde_json::Map::new();
    for meta in metas {
        for provider in host.runtime_of(&meta.id).providers {
            let summary = provider.summary_json();
            if let Some(items) = summary.get("adminPanels").and_then(Value::as_array) {
                for item in items {
                    let Some(id) = item.as_str() else { continue };
                    if seen.insert(id.to_string()) {
                        panels.push(Value::String(id.to_string()));
                    }
                }
            }
            if let Some(account_features) = summary.get("accountFeatures").filter(|v| !v.is_null())
            {
                features.insert(provider.id.clone(), account_features.clone());
            }
        }
    }
    (Value::Array(panels), Value::Object(features))
}

/// 外置插件包视图（前端 `PluginPackSummary` 契约）。
///
/// - `enabled` 以 `PackMeta.enabled` 为准（包级开关）：包声明里的
///   `default_enabled` 只是建议，网关不据此裁剪——包启用即全部申报；
/// - `loaded` = 包启用 **且** 握手申报了该家（进程真的在跑）；
/// - `entryPresent` 实测磁盘（入口被手工删过 = false，前端提示重装）；
/// - `host.crashes` 取监督循环的崩溃计数（重新启用清零）；
/// - `startup` / `providerEnabled` 是 A2A 对齐字段：宿主状态投影与逐家启停
///   （`providers[].enabled` 与 `providerEnabled` 同源 = 包级 `enabled` ∧
///   该家不在 `disabled_providers`，与上游 `provider_enabled` 同口径）；
/// - `providers[].overriddenBy`：该家实际归属的**另一个**已启用包（A2A
///   `PackProviderSummary.overriddenBy`），前端据此显示接管方并禁掉开关。
fn external_pack_view(meta: &PackMeta, host: &PluginHostManager) -> Value {
    let rt = host.runtime_of(&meta.id);
    let entry_present = meta.entry_path(host.plugins_root()).is_file();
    let extras = manifest_extras_for(host, meta);
    let providers: Vec<Value> = meta
        .providers
        .iter()
        .map(|pp| {
            let enabled = meta.provider_enabled(&pp.id);
            let loaded = rt.providers.iter().any(|p| p.id == pp.id);
            // 归属：另有已启用包声明了这家 = 本包被接管（不是本包自己）
            let overridden_by = provider_owner(host, &pp.id).filter(|owner| owner != &meta.id);
            json!({
                "id": pp.id,
                "label": pp.label,
                "enabled": enabled,
                "loaded": enabled && loaded,
                "overriddenBy": overridden_by,
                // 逐家开关写 `PackMeta.disabled_providers`（与内置包同为
                // "global" 语义：改完数据面归属即时生效）
                "toggle": "global",
            })
        })
        .collect();
    // `providerEnabled`：与 `providers[].enabled` 同源（前端两处读一致）
    let mut provider_enabled = serde_json::Map::new();
    for pp in &meta.providers {
        provider_enabled.insert(pp.id.clone(), Value::Bool(meta.provider_enabled(&pp.id)));
    }
    let startup = json!({
        "phase": if !meta.enabled {
            "disabled"
        } else if rt.running {
            "ready"
        } else if rt.last_error.is_some() {
            "failed"
        } else {
            "queued"
        },
        "running": rt.running,
        "crashes": rt.crashes,
        "error": rt.last_error,
    });
    json!({
        "id": meta.id,
        "name": meta.name,
        "description": extras.description,
        "version": meta.version,
        "requiredVersion": null,
        "updateRequired": false,
        "compatibilityError": null,
        "enabled": meta.enabled,
        "signed": meta.signed,
        "installedAt": meta.installed_at,
        "protocol": meta.protocol,
        "entryPresent": entry_present,
        "homepage": extras.homepage,
        "icon": pack_icon_for(host, meta, extras.icons.as_deref()),
        "providers": providers,
        "host": {
            "running": rt.running,
            "providers": rt.providers.iter().map(|p| p.id.clone()).collect::<Vec<_>>(),
            "crashes": rt.crashes,
            "disabled": !meta.enabled,
            "lastError": rt.last_error,
        },
        "providerCategory": extras.provider_category,
        // A2A 对齐字段
        "icons": extras.icons,
        "ui": pack_ui_pages(&meta.id, &extras.ui),
        "providerEnabled": Value::Object(provider_enabled),
        "startup": startup,
    })
}

/// GET /api/plugins —— 插件列表（内置目录包 + 全部外置包）+ 发布目录。
pub async fn handle_list_plugins(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let disabled = state.provider_gates.disabled_set();
    let metas = state.plugin_host.registry().list();
    // 内置目录包 + 注册表里的全部外置包
    let mut packs = vec![builtin_pack_view(&disabled)];
    for meta in &metas {
        packs.push(external_pack_view(meta, &state.plugin_host));
    }
    let catalog = fetch_catalog(&state).await;
    // 运行期 provider 申报的管理面板 / 账号页功能注入点（插件声明透出）
    let (admin_panels, account_features) = provider_injection_points(&state.plugin_host, &metas);
    // 已装包若清单没声明图标（社区包常见），回落到商店目录同 id 条目的
    // 图标——对齐 A2A 前端 `pack.icon || store?.icon`。
    fill_pack_icons_from_catalog(&mut packs, &catalog);
    // 更新检查：已装外置包 vs 当前目录（同一份 catalog，不额外请求）
    let updates = compute_updates(&metas, &catalog);
    // 账号摘要（A2A `accountCounts` / `hiddenAccounts`）：前端「补全缺失插件」
    // 横幅与批量条目的账号计数据此渲染。
    let (account_counts, hidden_accounts) = plugin_account_summary(&state);
    Ok(Json(json!({
        "success": true,
        "data": {
            "packs": packs,
            // 宿主进程状态在各外置包的 `host` 字段里（一包一进程）；
            // 顶层 host 留空（内置目录无宿主进程）
            "host": null,
            // 管理面板注入点（上游 adminPanels）：已加载插件申报的并集
            "adminPanels": admin_panels,
            // 账号页功能注入点（上游 accountFeatures）：provider id -> 声明
            "accountFeatures": account_features,
            "target": std::env::consts::OS,
            "protocol": 1,
            "pluginsDir": state.plugin_host.plugins_root().display().to_string(),
            // A2A 对齐字段（v1.0.63）：上传上限 / 脚本宿主快照 / 任务启动快照
            "maxPackBytes": MAX_PACK_BYTES,
            "accountCounts": account_counts,
            "hiddenAccounts": hidden_accounts,
            "scripts": script_snapshot(&state),
            "startup": startup_snapshot(&state, &packs),
            "catalog": catalog,
            "updates": updates,
            // 在途/近期操作（Operation 异步进度模型；含持久化的历史条目）
            "operations": plugin_operations(&state),
            // Wasm 宿主能力契约（beta.2：capabilities/limits/compute/scheduling/
            // tasks/desktopLaunch/verification/managedLogin/sms/accountOrigin/
            // protocol，逐项带 served 标识）
            "wasm": crate::plugin_host::wasm::contract_public(),
            // 已装载 Wasm 清单的扩展声明快照（声明了但未服务的能力由前端标黄）
            "wasmDeclarations": crate::plugin_host::wasm::declared_extensions(),
            // 桌面启动（beta.2 `desktopLaunch` 摘录）：本仓未实现桌面端启动，
            // 如实返回空表，不伪造
            "desktopLaunch": [],
            // 发布目录 provider 文档（beta.2 `providerDocs`）：从目录条目的
            // description/homepage 聚合，供前端详情页展示
            "providerDocs": provider_docs(&catalog),
        }
    })))
}

/// 已装包图标兜底：清单没声明 icon 时用商店目录同 id 条目的 icon。
///
/// 社区原生包的 `pack.json` 通常不含 `icons`（实测上游 index.json /
/// manifest 均无），但索引仓库里有图标；A2A 在**前端**做 `pack.icon ||
/// store?.icon` 兜底，AIGX 在列表接口补一份，等价且不改前端契约。
fn fill_pack_icons_from_catalog(packs: &mut [Value], catalog: &Value) {
    let Some(items) = catalog.get("plugins").and_then(Value::as_array) else {
        return;
    };
    for pack in packs.iter_mut() {
        if pack.get("icon").is_some_and(|icon| !icon.is_null()) {
            continue;
        }
        let Some(id) = pack.get("id").and_then(Value::as_str) else {
            continue;
        };
        if id == BUILTIN_PACK_ID {
            continue;
        }
        let icon = items
            .iter()
            .find(|item| item.get("id").and_then(Value::as_str) == Some(id))
            .and_then(|item| item.get("icon"))
            .filter(|icon| !icon.is_null())
            .cloned();
        if let Some(icon) = icon {
            pack["icon"] = icon;
        }
    }
}

/// `catalog` 条目的 provider 文档聚合（beta.2 `providerDocs` 投影）。
fn provider_docs(catalog: &Value) -> Value {
    let mut rows = Vec::new();
    if let Some(items) = catalog.get("plugins").and_then(Value::as_array) {
        for item in items {
            rows.push(json!({
                "id": item.get("id").cloned().unwrap_or(Value::Null),
                "name": item.get("name").cloned().unwrap_or(Value::Null),
                "description": item.get("description").cloned().unwrap_or(Value::Null),
                "homepage": item.get("homepage").cloned().unwrap_or(Value::Null),
                "docsUrl": item.get("homepage").cloned().unwrap_or(Value::Null),
            }));
        }
    }
    Value::Array(rows)
}

/// 当前操作快照的 `operations` 数组（列表页与 `/progress` 同源）。
fn plugin_operations(state: &AppState) -> Value {
    crate::plugin_host::operations::snapshot(Some(&state.plugin_host.store()))
        .get("operations")
        .cloned()
        .unwrap_or_else(|| json!([]))
}

/// 脚本插件宿主快照（A2A 顶层 `scripts`：脚本包与 provider 归属）。
///
/// 与 `/api/plugins/scripts` 同一数据源，这里只出**摘要**：状态 + 接管的
/// provider id 清单（列表页判「哪家由脚本接管」够用，详情走专用端点）。
fn script_snapshot(state: &AppState) -> Value {
    let status = state.script_host.status();
    json!({
        "running": status.running,
        "packages": status.packages,
        "providers": status.providers,
        "crashes": status.crashes,
        "lastError": status.last_error,
        // Bun 运行时诊断（A2A `payload.scripts.runtime`）：版本 / 是否已装 /
        // 可执行路径 / 平台不可用时的错误。前端据此展示运行时状态。
        "runtime": crate::plugin_host::script::bun::snapshot(state.script_host.data_dir()),
    })
}

/// 启动总览（A2A `plugin_host::startup::snapshot()` 的 AIGX 投影）。
///
/// 由包视图聚合：`packs` 里每包一行（阶段 + 错误 + 是否在跑），
/// `pending` = 有启用包但进程还没就绪（前端据此轮询列表）。
fn startup_snapshot(state: &AppState, packs: &[Value]) -> Value {
    let mut entries = Vec::new();
    let mut pending = false;
    for pack in packs {
        let id = pack["id"].as_str().unwrap_or("");
        if id == BUILTIN_PACK_ID {
            continue;
        }
        let enabled = pack["enabled"].as_bool().unwrap_or(false);
        let running = pack["host"]["running"].as_bool().unwrap_or(false);
        let phase = pack["startup"]["phase"].as_str().unwrap_or("queued");
        let error = pack["startup"]["error"].clone();
        if enabled && !running && phase != "failed" {
            pending = true;
        }
        entries.push(json!({
            "id": id,
            "enabled": enabled,
            "running": running,
            "phase": phase,
            "error": error,
        }));
    }
    json!({
        "packs": entries,
        "pending": pending,
        "script": script_snapshot(state),
        "now": chrono::Utc::now().timestamp_millis(),
    })
}

/// GET /api/plugins/startup —— 启动/加载阶段快照（各包一行）。
pub async fn handle_plugin_startup(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let disabled = state.provider_gates.disabled_set();
    let mut packs = vec![builtin_pack_view(&disabled)];
    for meta in state.plugin_host.registry().list() {
        packs.push(external_pack_view(&meta, &state.plugin_host));
    }
    Ok(Json(json!({
        "success": true,
        "data": startup_snapshot(&state, &packs),
    })))
}

/// POST /api/plugins/install —— 从 URL 安装外置插件包。
///
/// body `{"url":"https://.../pack.zip","sha256":"可选"}`；成功返回装好的
/// 包视图（前端 `PluginPackSummary` 契约，装完默认停用——运维确认后再启用）。
/// 失败 400 + 中文原因（下载失败/sha256 不匹配/协议不匹配/解压失败等）。
pub async fn handle_install_plugin(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<InstallBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let url = body.url.trim();
    if url.is_empty() {
        return Err(error_response(
            "安装地址（url）不能为空",
            StatusCode::BAD_REQUEST,
        ));
    }
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(error_response(
            "安装地址必须是 http/https URL",
            StatusCode::BAD_REQUEST,
        ));
    }
    let pack = state
        .plugin_host
        .install_from_url(url, body.sha256.as_deref())
        .await
        .map_err(|e| error_response(&e.to_string(), StatusCode::BAD_REQUEST))?;
    tracing::info!(
        "[Plugins] 已安装插件包 {}（v{}，{} 家 provider，默认停用）",
        pack.id,
        pack.version,
        pack.providers.len()
    );
    Ok(Json(json!({
        "success": true,
        "data": { "pack": external_pack_view(&pack, &state.plugin_host) }
    })))
}

/// 从发布目录安装/更新的请求体（前端 `runPluginInstallFromRelease`）。
#[derive(Debug, Deserialize)]
pub struct ReleaseInstallBody {
    pub id: String,
    /// `install`（缺省）或 `update`：`update` 会先校验目录版本确实更高。
    #[serde(default)]
    pub action: Option<String>,
    /// 可选 sha256（缺省取目录平台条目的 sha256）。
    #[serde(default)]
    pub sha256: Option<String>,
    /// 目标平台名（缺省 = 当前 OS；AIGX 目录的 `target` 就是 OS 名）。
    #[serde(default)]
    pub target: Option<String>,
}

/// POST /api/plugins/install-from-release —— 从发布目录起一次后台安装/更新操作。
///
/// 对齐 A2A `api::plugins::install_from_release` / `run_update`：
/// 服务端从当前目录解析「目标版本 + 当前平台下载地址 + sha256」，
/// 校验平台/版本后启动 `operations::start_install`，立即返回初始 Operation；
/// 下载/校验/安装与逐字节进度经 `GET /api/plugins/progress` 轮询。
/// 目录里没有该包 / 平台无包 / 更新时已是最高版 → 400（不进后台任务）。
pub async fn handle_install_from_release(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ReleaseInstallBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let id = body.id.trim();
    if id.is_empty() {
        return Err(error_response("缺少插件包 id", StatusCode::BAD_REQUEST));
    }
    let action = body.action.as_deref().unwrap_or("install");
    if !matches!(action, "install" | "update") {
        return Err(error_response(
            &format!("未知的安装动作：{action}（只支持 install / update）"),
            StatusCode::BAD_REQUEST,
        ));
    }
    let catalog = fetch_catalog(&state).await;
    let entry = catalog
        .get("plugins")
        .and_then(Value::as_array)
        .and_then(|items| {
            items
                .iter()
                .find(|item| item.get("id").and_then(Value::as_str) == Some(id))
        })
        .cloned()
        .ok_or_else(|| {
            error_response(
                &format!("发布目录里没有这个插件包：{id}"),
                StatusCode::NOT_FOUND,
            )
        })?;
    if entry.get("error").and_then(Value::as_bool) == Some(true) {
        return Err(error_response(
            entry
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("该插件在当前目录中不可用"),
            StatusCode::BAD_REQUEST,
        ));
    }
    let target = body.target.as_deref().unwrap_or(std::env::consts::OS);
    let platform = entry
        .get("platforms")
        .and_then(Value::as_array)
        .and_then(|items| {
            items
                .iter()
                .find(|p| p.get("target").and_then(Value::as_str) == Some(target))
        })
        .ok_or_else(|| {
            error_response(
                &format!("该插件不提供当前平台（{target}）的安装包"),
                StatusCode::BAD_REQUEST,
            )
        })?;
    let url = platform
        .get("url")
        .and_then(Value::as_str)
        .filter(|url| !url.trim().is_empty())
        .ok_or_else(|| error_response("目录条目缺少下载地址", StatusCode::BAD_REQUEST))?
        .to_string();
    let version = entry.get("version").and_then(Value::as_str).unwrap_or("");
    if action == "update" {
        let Some(installed) = state.plugin_host.registry().get(id) else {
            return Err(error_response(
                "还没有安装这个插件包，无法更新",
                StatusCode::NOT_FOUND,
            ));
        };
        if crate::update::compare_versions(version, &installed.version) != Some(1) {
            return Err(error_response("已经是最新版本", StatusCode::BAD_REQUEST));
        }
    }
    let sha256 = body
        .sha256
        .clone()
        .or_else(|| {
            platform
                .get("sha256")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .filter(|value| !value.trim().is_empty());
    let operation = crate::plugin_host::operations::start_install(
        state.plugin_host.clone(),
        state.plugin_host.store(),
        id.to_string(),
        action.to_string(),
        url,
        sha256,
    )
    .map_err(|e| error_response(&e, StatusCode::BAD_REQUEST))?;
    tracing::info!(
        "[Plugins] 已受理插件「{}」{}操作（后台执行，进度见 /api/plugins/progress）",
        id,
        if action == "update" {
            "更新"
        } else {
            "安装"
        }
    );
    Ok(Json(
        json!({ "success": true, "data": { "operation": operation } }),
    ))
}

/// 本地安装的查询参数（`?sha256=…`；A2A 的 expected_* 在无预览模式下降级为明文校验）。
#[derive(Debug, Deserialize, Default)]
pub struct InstallFileQuery {
    #[serde(default)]
    pub sha256: Option<String>,
}

/// POST /api/plugins/install-file —— 本地 zip 安装（A2A `install` 的文件上传形态）。
///
/// 请求体两种形态（与 A2A `pack_bytes` 同口径）：
/// - 原始 zip 字节（`Content-Type: application/octet-stream`，以 `PK` 开头）；
/// - JSON `{ "packB64": "..." }`（可带 `data:…;base64,` 前缀）。
///
/// 与 URL 安装共用 `PluginHostManager::install_bytes` 的校验链路（zip 结构、
/// 清单、入口、协议、sha256）。安装完成返回装好的包视图；失败 400 + 中文原因。
/// 本地安装是同步的（文件已在客户端，无下载阶段），不需要 Operation。
pub async fn handle_install_file(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<InstallFileQuery>,
    body: axum::body::Bytes,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let bytes =
        pack_upload_bytes(&body).map_err(|e| error_response(&e, StatusCode::BAD_REQUEST))?;
    let manager = state.plugin_host.clone();
    let sha = query.sha256.clone();
    let meta = tokio::task::spawn_blocking(move || manager.install_bytes(&bytes, sha.as_deref()))
        .await
        .map_err(|e| {
            error_response(
                &format!("安装任务中断：{e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            )
        })?
        .map_err(|e| error_response(&e.to_string(), StatusCode::BAD_REQUEST))?;
    tracing::info!(
        "[Plugins] 已从本地文件安装插件包 {}（v{}，{} 家 provider）",
        meta.id,
        meta.version,
        meta.providers.len()
    );
    Ok(Json(json!({
        "success": true,
        "data": { "pack": external_pack_view(&meta, &state.plugin_host) }
    })))
}

/// 解析本地安装请求体：原始 zip 或 base64 JSON，带与安装链路一致的体积上限。
fn pack_upload_bytes(body: &[u8]) -> Result<Vec<u8>, String> {
    if body.is_empty() {
        return Err("请求体是空的：请选择插件包 zip 后重试".to_string());
    }
    let size_error = || {
        format!(
            "插件包必须为非空且不超过 {} MiB 的 zip",
            MAX_PACK_BYTES / (1024 * 1024)
        )
    };
    if body.starts_with(b"PK") {
        if body.len() > MAX_PACK_BYTES {
            return Err(size_error());
        }
        return Ok(body.to_vec());
    }
    if body.len() > MAX_PACK_UPLOAD_BYTES {
        return Err(size_error());
    }
    let payload: Value =
        serde_json::from_slice(body).map_err(|_| "请求体既不是 zip 也不是 JSON".to_string())?;
    let encoded = payload
        .get("packB64")
        .or_else(|| payload.get("fileB64"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "缺少 packB64".to_string())?;
    let encoded = encoded
        .split_once("base64,")
        .map(|(_, text)| text)
        .unwrap_or(encoded)
        .trim();
    if encoded.len() > MAX_PACK_BASE64_BYTES {
        return Err(size_error());
    }
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|error| format!("插件包内容不是合法 base64：{error}"))?;
    if bytes.is_empty() || bytes.len() > MAX_PACK_BYTES {
        return Err(size_error());
    }
    Ok(bytes)
}

/// ── 内置发布目录：上游 Agent2API 插件索引快照 ──────────────────────────
///
/// 来源：<https://github.com/Agent2plugin/Agent2plugin> 的 `index.json` 与
/// `plugins/*.json` 清单；发布资产在
/// <https://github.com/Agent2Builder/agent2api-providers-releases>。
///
/// **AIGX 只记录元数据与上游下载地址，不打包、不分发插件二进制**：
/// 下载由运维在插件页点「下载」触发，文件直接从上游 GitHub Release 拉取。
/// 上游三个包均为 AGPL-3.0；AIGX 是 Source Available / 非商业，不复制其代码，
/// 也不在发行物里携带它们。
///
/// 未配置 `[plugins] source_url` 时用这份快照（运维开箱可见、自行选择下载）；
/// 配了 `source_url` 就以运维自己的目录为准，本快照不参与。
struct BuiltinRelease {
    id: &'static str,
    name: &'static str,
    description: &'static str,
    version: &'static str,
    homepage: &'static str,
    /// 上游 Release tag（各包前缀不同：`v…` / `web-v…` / `wasm-atomcode-v…`）
    tag: &'static str,
    published_at: &'static str,
}

const BUILTIN_RELEASES: &[BuiltinRelease] = &[
    BuiltinRelease {
        id: "agent2api-providers",
        name: "提供商增强",
        description: "上游 Agent2API 的 35 家 AI 提供商（模型调用 / 登录导入 / 凭证续期 / 模型目录 / 余额 / 签到）。AGPL-3.0，AIGX 只提供下载入口，不打包分发。",
        version: "3.5.0-beta.1",
        homepage: "https://github.com/ProvidersPlugin/Agent2Providers",
        tag: "v3.5.0-beta.1",
        published_at: "2026-11-09",
    },
    BuiltinRelease {
        id: "agent2api-web-providers",
        name: "网页提供商",
        description: "上游 Agent2API 的 4 家网页登录型提供商（智谱清言 / DeepSeek 网页 / Tabbit 国内版·国际版）。AGPL-3.0，AIGX 只提供下载入口。",
        version: "3.5.0-beta.1",
        homepage: "https://github.com/ProvidersPlugin/Agent2WebProviders",
        tag: "web-v3.5.0-beta.1",
        published_at: "2026-11-09",
    },
    BuiltinRelease {
        id: "agent2api-wasm-atomcode",
        name: "AtomCode · Wasm",
        description: "AtomCode 的 Wasm 形态（beta）。AIGX 当前没有 Wasm 运行时，可下载但无法启动——需要 AtomCode 请用「提供商增强」里的原生实现。",
        version: "0.1.0-beta.1",
        homepage: "https://github.com/ProvidersPlugin/Agent2Providers/tree/beta/wasm/atomcode",
        tag: "wasm-atomcode-v0.1.0-beta.1",
        published_at: "2026-11-09",
    },
];

/// 上游资产表：`(pack_id, 目标三元组, 字节数, sha256)`。
///
/// 三元组 → AIGX 前端的 `target`（`std::env::consts::OS`）由
/// [`current_target_triple`] 解析；前端只比 OS 名，所以这里按
/// `(OS, ARCH)` 挑唯一一条，避免把 aarch64 包发给 x86_64 机器。
const UPSTREAM_ASSETS: &[(&str, &str, u64, &str)] = &[
    (
        "agent2api-providers",
        "x86_64-unknown-linux-gnu",
        13_480_581,
        "e9be54ca633bbd01be0a69418f6269dea92afa6b0364ca6ccb6492f79726555b",
    ),
    (
        "agent2api-providers",
        "aarch64-unknown-linux-gnu",
        11_971_662,
        "2fca19deca43ddba6d43f446bdae7bb7a5ac16486938ba6878fb4374b3105664",
    ),
    (
        "agent2api-providers",
        "x86_64-apple-darwin",
        13_169_552,
        "ee1c6bfa9e3c2cfac793043ee062dd5b62dae1b66efe7b51b859d87dd897405a",
    ),
    (
        "agent2api-providers",
        "aarch64-apple-darwin",
        11_839_993,
        "70e734210eb18ac53197337b4b1f62ae0ee80ed7dc8406214c44bca023cc308c",
    ),
    (
        "agent2api-providers",
        "x86_64-pc-windows-msvc",
        17_673_616,
        "46fe91fb3f20820c578b831d395b9d77c89aab6d4be4fede00a7e7c9634d530a",
    ),
    (
        "agent2api-web-providers",
        "x86_64-unknown-linux-gnu",
        3_731_284,
        "85271f190641c9da15317bb5e95703e8e4a0fe1ed8b2a4d67cae400eec0fa653",
    ),
    (
        "agent2api-web-providers",
        "aarch64-unknown-linux-gnu",
        3_352_389,
        "c39f7bc3542816096242ee1479206065ae9e34cceab97cf1a24a48e64f950c24",
    ),
    (
        "agent2api-web-providers",
        "x86_64-apple-darwin",
        3_547_263,
        "0d04f17ee094af930776e808ac19a87e2e975cde2941cfc1443a17a99559c7c6",
    ),
    (
        "agent2api-web-providers",
        "aarch64-apple-darwin",
        3_348_088,
        "ad5a251aa6a24fc2a9dbde19287707182f798b5acf4a87be2283edcc5901e2c3",
    ),
    (
        "agent2api-web-providers",
        "x86_64-pc-windows-msvc",
        5_427_879,
        "c5a1a4a4f60698cd9c99ac205b947a22868a02f41f5289359f7bb33bec66957c",
    ),
    (
        "agent2api-wasm-atomcode",
        "wasm32-unknown-unknown",
        346_895,
        "af71de432ceb9adac0d3d83d5a3be8900b4211b195aa6e113183fdba8d514a47",
    ),
];

const UPSTREAM_RELEASE_REPO: &str = "Agent2Builder/agent2api-providers-releases";

/// 上游插件索引仓库（对齐 A2A `INDEX_REPO`）。未配置 `[plugins] source_url`
/// 时，运行时从这里的 `index.json` **动态拉取**插件清单，让上游新增插件自动出现，
/// 而非每次改 AIGX 代码里的内置快照。拉取失败回落到 [`builtin_catalog`]。
const UPSTREAM_INDEX_REPO: &str = "Agent2plugin/Agent2plugin";
const UPSTREAM_INDEX_URL: &str =
    "https://raw.githubusercontent.com/Agent2plugin/Agent2plugin/main/index.json";

/// AIGX 当前 `(OS, ARCH)` 对应的上游目标三元组；不认识的平台返回 `None`
/// （该条目会以空 `platforms` 呈现，前端显示「当前平台暂无包」）。
fn current_target_triple() -> Option<&'static str> {
    Some(match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        _ => return None,
    })
}

/// 上游 Release 资产下载地址（`{id}-{version}-{triple}.zip`）。
fn upstream_asset_url(tag: &str, id: &str, version: &str, triple: &str) -> String {
    format!(
        "https://github.com/{UPSTREAM_RELEASE_REPO}/releases/download/{tag}/{id}-{version}-{triple}.zip"
    )
}

/// 渲染 manifest 的 release 模板：`{id}` / `{version}` / `{target}`。
fn render_release_template(template: &str, id: &str, version: &str, triple: &str) -> String {
    template
        .replace("{id}", id)
        .replace("{version}", version)
        .replace("{target}", triple)
}

/// manifest 声明的平台三元组（兼容字符串数组与 `{target: …}` 对象数组）。
fn manifest_platform_targets(manifest: &Value) -> Vec<&str> {
    manifest
        .get("platforms")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    item.as_str()
                        .or_else(|| item.get("target").and_then(Value::as_str))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 选下载用的 triple：manifest 声明了 `platforms` 时按声明挑。
///
/// 1. 本机 triple 在列 → 用它（原生包）；
/// 2. 否则有 `wasm32-*` → `wasm32-unknown-unknown`（wasm 包跨平台运行）；
/// 3. 无 `platforms`（老格式）或都不匹配 → 本机 triple（保持旧行为）。
///
/// 此前无视 `platforms`、一律用本机 triple 拼资产名，导致 wasm 包下载 404。
fn resolve_download_triple(manifest: &Value) -> Option<&'static str> {
    let local = current_target_triple();
    let targets = manifest_platform_targets(manifest);
    if targets.is_empty() {
        return local;
    }
    if let Some(local) = local {
        if targets.contains(&local) {
            return Some(local);
        }
    }
    if targets.iter().any(|t| t.starts_with("wasm32-")) {
        return Some("wasm32-unknown-unknown");
    }
    local
}

/// 把上游 `index.json` 条目 + 其 `manifest` 投影成源格式条目（纯函数，便于单测）。
fn upstream_catalog_item(id: &str, entry: &Value, manifest: &Value) -> Value {
    let version = manifest
        .get("version")
        .and_then(Value::as_str)
        .unwrap_or("");
    let release = manifest.get("release");
    let repo = release
        .and_then(|r| r.get("repo"))
        .and_then(Value::as_str)
        .unwrap_or(UPSTREAM_RELEASE_REPO);
    // 选择实际下载用的 triple：manifest 声明了 `platforms` 时按声明挑
    // （wasm 包声明 `wasm32-unknown-unknown`，用本机 triple 拼 URL 会 404）。
    let triple = resolve_download_triple(manifest);
    // 组装成 `normalize_store_plugin` 能识别的源格式：当前平台一条 platforms
    let platforms: Vec<Value> = match triple {
        Some(t) => {
            let tag_template = release
                .and_then(|r| r.get("tag"))
                .and_then(Value::as_str)
                .unwrap_or("v{version}");
            let tag = render_release_template(tag_template, id, version, t);
            // 资产名同样按 manifest 模板渲染；缺省沿用 `{id}-{version}-{target}.zip`
            let asset_template = release
                .and_then(|r| r.get("asset"))
                .and_then(Value::as_str)
                .unwrap_or("{id}-{version}-{target}.zip");
            let asset = render_release_template(asset_template, id, version, t);
            let url = format!("https://github.com/{repo}/releases/download/{tag}/{asset}");
            let (size, sha) = upstream_asset_meta(id, t);
            vec![json!({
                // wasm 跨平台运行：target 仍填 OS，前端按 OS 才显示「下载」
                "target": std::env::consts::OS,
                "size": size,
                "url": url,
                "current": false,
                "sha256": sha,
            })]
        }
        // 无对应平台（如 wasm 三元组匹配不到常规平台）→ 空 platforms，
        // 前端渲染「当前平台暂无包」，与内置快照口径一致。
        None => Vec::new(),
    };
    json!({
        "id": id,
        "name": entry.get("name").and_then(Value::as_str).unwrap_or(id),
        "description": entry.get("description").and_then(Value::as_str).unwrap_or(""),
        "official": entry.get("official").and_then(Value::as_bool).unwrap_or(false),
        "version": version,
        "page": manifest.get("homepage").and_then(Value::as_str),
        "homepage": manifest.get("homepage").and_then(Value::as_str),
        "icon": resolve_store_icon(
            entry.get("icon").and_then(Value::as_str),
            Some(UPSTREAM_INDEX_RAW_BASE),
        ),
        // 分类元数据（A2A index.json：category / tags / requireSigned）
        // 透传，供前端渲染 Wasm / 需签名徽标。
        "category": entry.get("category").cloned().unwrap_or(Value::Null),
        "requireSigned": entry.get("requireSigned").and_then(Value::as_bool).unwrap_or(false),
        "tags": entry.get("tags").cloned().unwrap_or_else(|| Value::Array(Vec::new())),
        "platforms": platforms,
        // 清单声明的 provider（缺失插件横幅用来把隐藏账号映射到可安装包）
        "providers": manifest.get("providers").cloned().unwrap_or(Value::Array(Vec::new())),
    })
}

/// 上游索引仓库 raw 基址：`index.json` 的 `icon` 是**仓内相对路径**
/// （如 `icons/agent2api-wasm-kiro.svg`）。
const UPSTREAM_INDEX_RAW_BASE: &str =
    "https://raw.githubusercontent.com/Agent2plugin/Agent2plugin/main";

/// 商店条目的 `icon` → `<img>` 可直接加载的绝对地址。
///
/// `index.json` 及自定义插件源里的 `icon` 通常是相对路径；原样透给前端会
/// 被浏览器解析成管理面自身的相对地址（`/icons/…`）→ 404，这正是商店
/// 卡片「有条目、没图标」的根因。规则：
/// - `data:image/…` / `http(s)://…` 原样保留（已是可加载地址）；
/// - 相对路径按 `base`（索引仓库 raw 基址 / 自定义源目录）拼成绝对 URL；
/// - 绝对路径 / 盘符 / 含 `..` / `\\` 一律丢弃（不给任意主机/路径开口子）。
fn resolve_store_icon(icon: Option<&str>, base: Option<&str>) -> Option<String> {
    let icon = icon.map(str::trim).filter(|value| !value.is_empty())?;
    if icon.len() > 2048 {
        return None;
    }
    if icon.starts_with("data:image/")
        || icon.starts_with("https://")
        || icon.starts_with("http://")
    {
        return Some(icon.to_string());
    }
    if icon.starts_with('/')
        || icon.contains('\\')
        || icon.contains(':')
        || icon.split('/').any(|part| part == "..")
    {
        return None;
    }
    let base = base?.trim_end_matches('/');
    if base.is_empty() {
        return None;
    }
    Some(format!("{base}/{icon}"))
}

/// 自定义插件源 URL → 相对图标基址（去掉最后一段文件名）。
///
/// 例：`https://h.example/plugins/index.json` → `https://h.example/plugins`。
fn source_base(source_url: &str) -> Option<&str> {
    let source_url = source_url.split(['?', '#']).next().unwrap_or(source_url);
    let (base, _) = source_url.rsplit_once('/')?;
    // 必须仍是带 scheme 的地址（`https://h` 这类无路径 URL 没有可用的
    // 同级目录，返回 None 让相对图标走丢弃路径，而不是拼出 `https:/…`）。
    (base.starts_with("https://") || base.starts_with("http://")).then_some(base)
}

/// 内置发布目录（未配置 `[plugins] source_url` 时的默认目录）。
///
/// 形状与 [`normalize_catalog`] 的输出一致（前端 `PluginReleaseState`）。
fn builtin_catalog() -> Value {
    let triple = current_target_triple();
    let plugins: Vec<Value> = BUILTIN_RELEASES
        .iter()
        .map(|rel| {
            let platforms: Vec<Value> = triple
                .and_then(|t| {
                    UPSTREAM_ASSETS
                        .iter()
                        .find(|(id, tr, _, _)| *id == rel.id && *tr == t)
                        .map(|(_, _, size, sha)| {
                            json!({
                                "target": std::env::consts::OS,
                                "size": size,
                                "url": upstream_asset_url(rel.tag, rel.id, rel.version, t),
                                "current": false,
                                "sha256": sha,
                            })
                        })
                })
                .into_iter()
                .collect();
            json!({
                "id": rel.id,
                "name": rel.name,
                "description": rel.description,
                // 上游是社区包，不是 AIGX 官方实现 → 不打「官方」徽标
                "official": false,
                "version": rel.version,
                "page": rel.homepage,
                "notes": null,
                "publishedAt": rel.published_at,
                "prerelease": true,
                "platforms": platforms,
                "homepage": rel.homepage,
                // 上游索引仓库内有各包图标（`icons/{id}.png`），相对路径
                // 拼成 raw 绝对地址，避免内置快照卡片无图。
                "icon": resolve_store_icon(
                    Some(&format!("icons/{}.png", rel.id)),
                    Some(UPSTREAM_INDEX_RAW_BASE),
                ),
                // 内置快照不附带清单 provider 列表（动态索引才有）
                "providers": [],
            })
        })
        .collect();
    json!({
        "available": true,
        "current": "",
        "plugins": plugins,
        "reason": null,
        "error": false,
    })
}

/// 从上游 `index.json` 动态拉取插件清单（对齐 A2A `read_index` + `read_detail`）。
///
/// 链路：拉 `index.json` → 逐包拉 `manifest`（拿 `version` / `release` 模板）
/// → 按 `{id}-{version}-{target}.zip` 模板拼当前平台的下载 URL。`sha256`/`size`
/// 优先用内置 [`UPSTREAM_ASSETS`] 表（已知三包逐字节对账）；上游新增的包查不到
/// 表 → 留 `null`（`install` 侧 sha256 本就是可选校验，不阻断下载）。
///
/// **任何一步失败都返回 `Ok(None)**，由调用方回落到 [`builtin_catalog`]——
/// 动态拉取是增强，不是硬依赖：断网/限流时运维仍能看到内置快照。
/// 只读清单元数据、不分发插件二进制（AGPL 边界，见文件头）。
async fn fetch_upstream_catalog(state: &AppState) -> Option<Value> {
    let resp = state
        .http_client
        .get(UPSTREAM_INDEX_URL)
        .timeout(CATALOG_TIMEOUT)
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let index: Value = resp.json().await.ok()?;
    let entries = index.get("plugins").and_then(Value::as_array)?;

    let mut items: Vec<Value> = Vec::new();
    for entry in entries {
        let id = entry.get("id").and_then(Value::as_str)?;
        let manifest_path = entry.get("manifest").and_then(Value::as_str)?;
        // 拼 manifest 的 raw URL（index.json 里的 `manifest` 是仓内相对路径）
        let manifest_url =
            format!("https://raw.githubusercontent.com/{UPSTREAM_INDEX_REPO}/main/{manifest_path}");
        let mresp = state
            .http_client
            .get(&manifest_url)
            .timeout(CATALOG_TIMEOUT)
            .send()
            .await
            .ok()?;
        if !mresp.status().is_success() {
            continue;
        }
        let manifest: Value = match mresp.json().await {
            Ok(v) => v,
            Err(_) => continue,
        };
        items.push(upstream_catalog_item(id, entry, &manifest));
    }
    if items.is_empty() {
        return None;
    }
    // 复用现有规整，保证与内置快照 / source_url 两条路径输出形状逐字段一致
    normalize_catalog(
        &json!({ "current": "", "plugins": items }),
        std::env::consts::OS,
        Some(UPSTREAM_INDEX_RAW_BASE),
    )
}

/// 内置资产表里查 `(size, sha256)`；查不到返回 `(0, null)`（上游新增包）。
fn upstream_asset_meta(id: &str, triple: &str) -> (u64, Value) {
    match UPSTREAM_ASSETS
        .iter()
        .find(|(pid, tr, _, _)| *pid == id && *tr == triple)
    {
        Some((_, _, size, sha)) => (*size, json!(sha)),
        None => (0, Value::Null),
    }
}

/// 发布目录（`GET /api/plugins` 用）：带 TTL 缓存 + 单飞。
///
/// 前端有在途操作时每 2s 轮询列表，不应每次都打上游。成功结果缓存 60s、
/// 不可用态 10s；并发请求只发一次（见 `CatalogCache`）。配置的
/// `source_url` 变化视为未命中。
async fn fetch_catalog(state: &AppState) -> Value {
    let source_url = state
        .config_manager
        .get()
        .await
        .plugins
        .source_url
        .trim()
        .to_string();
    let fetch_state = state.clone();
    let fetch_source = source_url.clone();
    state
        .plugin_host
        .catalog_cache()
        .get_or_fetch(&source_url, false, move || async move {
            fetch_catalog_uncached(&fetch_state, &fetch_source).await
        })
        .await
}

/// 发布目录（`GET /api/plugins/release` 刷新语义）：强制绕过 TTL，重新拉取。
///
/// 仍会把新结果写回缓存，后续列表请求直接命中新值。
async fn refresh_catalog(state: &AppState) -> Value {
    let source_url = state
        .config_manager
        .get()
        .await
        .plugins
        .source_url
        .trim()
        .to_string();
    let fetch_state = state.clone();
    let fetch_source = source_url.clone();
    state
        .plugin_host
        .catalog_cache()
        .get_or_fetch(&source_url, true, move || async move {
            fetch_catalog_uncached(&fetch_state, &fetch_source).await
        })
        .await
}

/// 发布目录的实际拉取（无缓存）。`source_url` 由调用方预先从配置读出。
///
/// 未配置 / 拉取失败 / 返回内容无法识别 → `available:false` + 中文
/// reason（`error:true`，前端渲染降级态而不是空目录白屏）。
async fn fetch_catalog_uncached(state: &AppState, source_url: &str) -> Value {
    if source_url.is_empty() {
        // 未配置源 → 优先从上游 index.json **动态拉取**（运维看到上游实际有哪些包、
        // 上游新增自动出现）；动态拉取失败（断网/限流）→ 回落内置快照（开箱可见）。
        // 配了 source_url 就以运维自己的目录为准，两条动态路径都不参与。
        if let Some(dynamic) = fetch_upstream_catalog(state).await {
            return dynamic;
        }
        return builtin_catalog();
    }
    let resp = match state
        .http_client
        .get(source_url)
        .timeout(CATALOG_TIMEOUT)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return catalog_unavailable(&format!("拉取插件源失败: {e}")),
    };
    if !resp.status().is_success() {
        return catalog_unavailable(&format!("插件源返回 HTTP {}", resp.status()));
    }
    let body: Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => return catalog_unavailable(&format!("插件源返回内容无法解析: {e}")),
    };
    match normalize_catalog(&body, std::env::consts::OS, source_base(source_url)) {
        Some(v) => v,
        None => catalog_unavailable("插件源返回内容无法识别（期望 JSON 对象，含 plugins 数组）"),
    }
}

/// 发布目录不可用态（前端渲染「暂不支持」+ reason，而不是空目录白屏）。
fn catalog_unavailable(reason: &str) -> Value {
    json!({
        "available": false,
        "current": "",
        "plugins": [],
        "reason": reason,
        "error": true,
    })
}

/// 把插件源返回的 JSON 规整成前端 `PluginReleaseState` 形状。
///
/// 源格式（宽松解析，缺字段自然落默认）：
/// ```json
/// {
///   "current": "1.0.52",
///   "plugins": [
///     {
///       "id": "agent2api-providers",
///       "name": "…", "description": "…", "version": "3.4.2",
///       "official": true, "page": "…", "notes": "…",
///       "publishedAt": "2026-11-01", "prerelease": false,
///       "url": "https://…/pack.zip",   // 无 platforms 时的兜底安装地址
///       "sha256": "…", "size": 12345,
///       "platforms": [
///         { "target": "linux", "url": "…", "size": 123, "current": false, "sha256": "…" }
///       ]
///     }
///   ]
/// }
/// ```
///
/// 无法识别（缺 `plugins` 数组）返回 `None`（调用方转不可用态）。
fn normalize_catalog(v: &Value, target_os: &str, icon_base: Option<&str>) -> Option<Value> {
    let plugins = v.get("plugins")?.as_array()?;
    let current = v.get("current").and_then(|x| x.as_str()).unwrap_or("");
    let items: Vec<Value> = plugins
        .iter()
        .filter_map(|item| normalize_store_plugin(item, target_os, icon_base))
        .collect();
    Some(json!({
        "available": true,
        "current": current,
        "plugins": items,
        "reason": null,
        "error": false,
    }))
}

/// 单个发布条目 → 前端 `StorePlugin` 形状。缺 `id` 的条目丢弃（无法键控）。
fn normalize_store_plugin(item: &Value, target_os: &str, icon_base: Option<&str>) -> Option<Value> {
    let id = item.get("id")?.as_str()?.to_string();
    let name = {
        let n = item.get("name").and_then(|x| x.as_str()).unwrap_or("");
        if n.is_empty() {
            id.clone()
        } else {
            n.to_string()
        }
    };
    let str_field =
        |k: &str| -> Option<String> { item.get(k).and_then(|x| x.as_str()).map(|s| s.to_string()) };
    let platforms: Vec<Value> = match item.get("platforms").and_then(|x| x.as_array()) {
        Some(arr) => arr
            .iter()
            .filter_map(|p| {
                let url = p.get("url").and_then(|x| x.as_str())?;
                Some(json!({
                    "target": p.get("target").and_then(|x| x.as_str()).unwrap_or(target_os),
                    "size": p.get("size").and_then(|x| x.as_u64()).unwrap_or(0),
                    "url": url,
                    "current": p.get("current").and_then(|x| x.as_bool()).unwrap_or(false),
                    "sha256": p.get("sha256").and_then(|x| x.as_str()),
                }))
            })
            .collect(),
        None => {
            // 无 platforms：用顶层 url/sha256/size 合成一条当前平台条目；
            // 顶层也没有 url → 空 platforms（前端渲染「当前平台暂无包」）
            match item.get("url").and_then(|x| x.as_str()) {
                Some(url) => vec![json!({
                    "target": target_os,
                    "size": item.get("size").and_then(|x| x.as_u64()).unwrap_or(0),
                    "url": url,
                    "current": false,
                    "sha256": item.get("sha256").and_then(|x| x.as_str()),
                })],
                None => Vec::new(),
            }
        }
    };
    // 清单 provider 列表（可选）：`[{id,label}]`；缺失 = 空表（前端按 id 猜测兜底）
    let providers: Vec<Value> = item
        .get("providers")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|provider| {
                    let id = provider.get("id").and_then(Value::as_str)?;
                    let label = provider.get("label").and_then(Value::as_str).unwrap_or(id);
                    Some(json!({ "id": id, "label": label }))
                })
                .collect()
        })
        .unwrap_or_default();
    Some(json!({
        "id": id,
        "name": name,
        "description": str_field("description").unwrap_or_default(),
        "official": item.get("official").and_then(|x| x.as_bool()).unwrap_or(false),
        "version": str_field("version"),
        "page": str_field("page"),
        "notes": str_field("notes"),
        "publishedAt": str_field("publishedAt"),
        "prerelease": item.get("prerelease").and_then(|x| x.as_bool()).unwrap_or(false),
        "error": item.get("error").and_then(|x| x.as_bool()).unwrap_or(false),
        "reason": str_field("reason"),
        "homepage": str_field("homepage"),
        "icon": resolve_store_icon(str_field("icon").as_deref(), icon_base),
        // 上游 index.json 的分类元数据（A2A 商店卡片/Wasm 徽标据此渲染）：
        // 原样透传，缺失即为 null / 空表；不影响安装链路。
        "category": str_field("category"),
        "requireSigned": item.get("requireSigned").and_then(|x| x.as_bool()).unwrap_or(false),
        "tags": item
            .get("tags")
            .and_then(Value::as_array)
            .map(|tags| {
                tags.iter()
                    .filter_map(Value::as_str)
                    .map(|tag| Value::String(tag.to_string()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
        "platforms": platforms,
        "providers": providers,
    }))
}

/// POST /api/plugins/enable —— 包级启停。
///
/// - 内置包是二进制的一部分，不支持整包停用（停掉 = 没有任何上游可用），
///   如实报错引导用 provider 级开关；
/// - 外置包按 body 的 `enabled` 调 `PluginHostManager::enable/disable`
///   （启用 = spawn + 握手 + 崩溃退避监督；停用 = 关进程 + 落 enabled=false）。
pub async fn handle_set_plugin_enabled(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<PackIdBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    if body.id == BUILTIN_PACK_ID {
        return Err(error_response(
            "内置供应商目录不支持整包停用；请在包内按家停用（providers[].enabled）",
            StatusCode::BAD_REQUEST,
        ));
    }
    if state.plugin_host.registry().get(&body.id).is_none() {
        return Err(error_response(
            &format!("插件包「{}」不存在", body.id),
            StatusCode::NOT_FOUND,
        ));
    }
    // 外置包路径要求显式 enabled（见 PackIdBody 字段注释）
    let enabled = body.enabled.ok_or_else(|| {
        error_response(
            "包级启停需要显式传 enabled（true=启用，false=停用）",
            StatusCode::BAD_REQUEST,
        )
    })?;
    if enabled {
        state
            .plugin_host
            .enable(&body.id)
            .await
            .map_err(|e| error_response(&e, StatusCode::BAD_REQUEST))?;
    } else {
        state
            .plugin_host
            .disable(&body.id)
            .await
            .map_err(|e| error_response(&e, StatusCode::BAD_REQUEST))?;
    }
    tracing::info!(
        "[Plugins] 插件包「{}」已{}",
        body.id,
        if enabled { "启用" } else { "停用" }
    );
    Ok(Json(json!({
        "success": true,
        "data": { "id": body.id, "enabled": enabled }
    })))
}

/// POST /api/plugins/provider —— provider 级启停（数据面选号即时生效）。
pub async fn handle_set_plugin_provider_enabled(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ProviderBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    if body.id != BUILTIN_PACK_ID {
        // 外置（原生）插件包：逐家开关写 `PackMeta.disabled_providers` 并
        // 持久化（上游 `set_provider_enabled` 同义）。包不存在 → 404；
        // 家不在包声明里 → 400（不造幽灵开关）。
        let changed = state
            .plugin_host
            .registry()
            .set_provider_enabled(&body.id, &body.provider, body.enabled)
            .map_err(|e| {
                let status = if e.contains("不存在") {
                    StatusCode::NOT_FOUND
                } else {
                    StatusCode::BAD_REQUEST
                };
                error_response(&e, status)
            })?;
        if changed {
            tracing::info!(
                "[Plugins] 插件包「{}」的 provider「{}」已{}（数据面归属即时生效）",
                body.id,
                body.provider,
                if body.enabled { "启用" } else { "停用" }
            );
        }
        return Ok(Json(json!({
            "success": true,
            "data": {
                "id": body.provider,
                "pack": body.id,
                "enabled": body.enabled,
                "changed": changed
            }
        })));
    }
    if !all_provider_kinds().contains(&body.provider.as_str()) {
        return Err(error_response(
            &format!("未知的 provider「{}」", body.provider),
            StatusCode::BAD_REQUEST,
        ));
    }
    let changed = state
        .provider_gates
        .set_enabled(&body.provider, body.enabled)
        .map_err(|e| error_response(&e, StatusCode::INTERNAL_SERVER_ERROR))?;
    if changed {
        tracing::info!(
            "[Plugins] provider「{}」已{}（选号过滤即时生效）",
            body.provider,
            if body.enabled { "启用" } else { "停用" }
        );
    }
    Ok(Json(json!({
        "success": true,
        "data": { "id": body.provider, "enabled": body.enabled, "changed": changed }
    })))
}

/// ── 包级设置（`/api/plugins/settings`）────────────────────────────
///
/// A2A 的这份设置是「插件更新通道与检查计划」（通道跟随软件 / 自定义、
/// 自动检查、间隔、正式/beta）。AIGX 的对应事实源是**全局检查计划**
/// （`scheduled_tasks.updateCheck`，与「更新」页、更新调度循环同一份）与
/// `[update]` 段的通道/自动应用，所以这里做**投影**而不是另存一份：
/// GET 出全局子集（含 `mode` 标记它来自全局），PATCH 写回同一份事实源。
/// 这样「插件页改了检查计划」与「更新页/设置页改了」是同一份数据，不会分叉。
///
/// 键名用 A2A 的 camelCase（前端契约）：`autoCheck` / `intervalMinutes` /
/// `beta` / `autoUpdate` / `stable` / `channelMode` / `checkMode`。
///
/// 设置键白名单（PATCH 只收这些）。`interval` 是 `intervalMinutes` 的
/// 兼容别名（A2A 历史上两种拼法都出现过）。
const PLUGIN_SETTINGS_KEYS: &[&str] = &[
    "autoCheck",
    "intervalMinutes",
    "interval",
    "beta",
    "stable",
    "autoUpdate",
    "channelMode",
    "checkMode",
];

/// 全局检查计划 + 通道 → A2A 形状的插件设置。
fn plugin_settings_view(config: &crate::config::AppConfig) -> Value {
    let check = &config.scheduled_tasks.update_check;
    json!({
        // 通道与检查计划都「跟随软件」：AIGX 的更新设置是全局一份，
        // 没有独立的插件通道/计划，如实报 follow
        "channelMode": "follow",
        "checkMode": "follow",
        "autoCheck": check.enabled,
        // A2A 单位是分钟；排期也按分钟存（同一事实源，不再从小时换算）
        "interval": check.interval,
        "stable": !config.update.include_prerelease,
        "beta": config.update.include_prerelease,
        // AIGX 的「自动应用」比「自动下载」激进一档：如实映射到 auto_update
        "autoUpdate": config.update.auto_apply,
    })
}

/// GET /api/plugins/settings
pub async fn handle_plugin_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let config = state.config_manager.get().await;
    Ok(Json(json!({
        "success": true,
        "data": plugin_settings_view(&config),
    })))
}

#[derive(Debug, Deserialize)]
pub struct PluginSettingsPatch {
    #[serde(default, rename = "autoCheck")]
    pub auto_check: Option<bool>,
    /// 分钟（A2A 口径）；写回时换算成小时（最小 1 小时）。
    #[serde(default, rename = "intervalMinutes", alias = "interval")]
    pub interval_minutes: Option<i64>,
    #[serde(default)]
    pub beta: Option<bool>,
    #[serde(default)]
    pub stable: Option<bool>,
    #[serde(default, rename = "autoUpdate")]
    pub auto_update: Option<bool>,
    #[serde(default, rename = "channelMode")]
    pub channel_mode: Option<String>,
    #[serde(default, rename = "checkMode")]
    pub check_mode: Option<String>,
}

/// PATCH /api/plugins/settings —— 写回 `[update]`（与设置页同一份数据）。
pub async fn handle_save_plugin_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    // 键白名单先过一遍：拼错的键要明确 400，不能静默吞掉
    if let Some(object) = body.as_object() {
        if let Some(unknown) = object
            .keys()
            .find(|key| !PLUGIN_SETTINGS_KEYS.contains(&key.as_str()))
        {
            return Err(error_response(
                &format!("未知的插件设置：{unknown}"),
                StatusCode::BAD_REQUEST,
            ));
        }
    } else {
        return Err(error_response(
            "插件设置必须是 JSON 对象",
            StatusCode::BAD_REQUEST,
        ));
    }
    let patch: PluginSettingsPatch = match serde_json::from_value(body) {
        Ok(patch) => patch,
        Err(_) => {
            return Err(error_response(
                "插件设置字段类型无效",
                StatusCode::BAD_REQUEST,
            ))
        }
    };
    // 显式「自定义」模式：AIGX 没有独立通道/计划，只有 follow 一种事实；
    // 报错引导而不是假装接受（接受了却还是全局值，用户会以为设置生效了）
    for (key, value) in [
        ("channelMode", patch.channel_mode.as_deref()),
        ("checkMode", patch.check_mode.as_deref()),
    ] {
        if let Some(mode) = value {
            if mode != "follow" {
                return Err(error_response(
                    &format!("{key} 只支持 follow（AIGX 的更新通道与检查计划跟随全局设置）"),
                    StatusCode::BAD_REQUEST,
                ));
            }
        }
    }
    // 通道至少留一个（A2A 同口径）：两个都显式关掉 = 没有任何更新源
    if patch.beta == Some(false) && patch.stable == Some(false) {
        return Err(error_response(
            "至少需要关注正式版或 beta 版中的一个通道",
            StatusCode::BAD_REQUEST,
        ));
    }
    // 字段级读-改-写（P2）：并发保存不同 tab 时不再整包覆盖。
    // 检查计划写进与「更新」页/调度循环同一份 `scheduled_tasks.updateCheck`；
    // `[update]` 的旧字段只做镜像（既有读取方仍能读到旧值）。
    let schedule_before: std::sync::Arc<
        parking_lot::Mutex<Option<crate::scheduled_tasks::types::IntervalTask>>,
    > = std::sync::Arc::new(parking_lot::Mutex::new(None));
    let before_slot = schedule_before.clone();
    let config = super::common::update_admin_config(&state, &headers, move |config| {
        *before_slot.lock() = Some(config.scheduled_tasks.update_check);
        if let Some(value) = patch.auto_check {
            config.update.auto_check = value;
            config.scheduled_tasks.update_check.enabled = value;
        }
        if let Some(value) = patch.auto_update {
            config.update.auto_apply = value;
        }
        // `beta` 是 `include_prerelease` 的镜像：显式给值即写入
        if let Some(beta) = patch.beta {
            config.update.include_prerelease = beta;
        }
        if let Some(minutes) = patch.interval_minutes {
            let min = crate::scheduled_tasks::types::INTERVAL_MIN_MINUTES;
            let max = crate::scheduled_tasks::types::INTERVAL_MAX_MINUTES;
            if !(min..=max).contains(&minutes) {
                return Err(format!("检查间隔必须是 {min} ~ {max} 分钟"));
            }
            config.scheduled_tasks.update_check.interval = minutes;
            config.update.interval_hours = (minutes / 60).max(1) as u64;
        }
        Ok(())
    })
    .await?;
    // 与「定时任务」页的 configure 同一口径：开启即跑一轮 / 改间隔按新间隔重排。
    let schedule_before = *schedule_before.lock();
    if let Some(before) = schedule_before {
        let after = config.scheduled_tasks.update_check;
        if let Err(message) = crate::scheduled_tasks::on_schedule_changed(
            &config,
            crate::scheduled_tasks::TASK_UPDATE_CHECK,
            before,
            after,
        ) {
            tracing::warn!("[Plugins] 检查计划重排失败：{message}");
        }
    }
    Ok(Json(json!({
        "success": true,
        "data": plugin_settings_view(&config),
    })))
}

/// POST /api/plugins/reload —— 热重启一个外置包（停 → 启）。
///
/// 与「停用再启用」的差别只在**最终状态**：reload 不改变 `enabled`
/// 选择（启用中的包重启后仍启用；停用中的包 reload = 直接报错引导）。
pub async fn handle_reload_plugin(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<PackIdBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    if body.id == BUILTIN_PACK_ID {
        return Err(error_response(
            "内置供应商目录编译在二进制里，没有可重启的宿主进程",
            StatusCode::BAD_REQUEST,
        ));
    }
    let Some(meta) = state.plugin_host.registry().get(&body.id) else {
        return Err(error_response(
            &format!("插件包「{}」不存在", body.id),
            StatusCode::NOT_FOUND,
        ));
    };
    if !meta.enabled {
        return Err(error_response(
            "该插件包处于停用状态；请先启用（reload 只重启已启用包的宿主进程）",
            StatusCode::BAD_REQUEST,
        ));
    }
    state
        .plugin_host
        .disable(&body.id)
        .await
        .map_err(|e| error_response(&e, StatusCode::BAD_REQUEST))?;
    state
        .plugin_host
        .enable(&body.id)
        .await
        .map_err(|e| error_response(&e, StatusCode::BAD_REQUEST))?;
    tracing::info!("[Plugins] 插件包「{}」已重载", body.id);
    let meta = state.plugin_host.registry().get(&body.id).unwrap_or(meta);
    Ok(Json(json!({
        "success": true,
        "data": { "pack": external_pack_view(&meta, &state.plugin_host) }
    })))
}

/// POST /api/plugins/uninstall —— 卸载（内置包不支持，如实报）。
pub async fn handle_uninstall_plugin(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<PackIdBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    if body.id == BUILTIN_PACK_ID {
        return Err(error_response(
            "内置供应商目录编译在二进制里，不支持卸载；停用某家用 provider 级开关",
            StatusCode::BAD_REQUEST,
        ));
    }
    if state.plugin_host.registry().get(&body.id).is_none() {
        return Err(error_response(
            &format!("插件包「{}」不存在", body.id),
            StatusCode::NOT_FOUND,
        ));
    }
    state
        .plugin_host
        .uninstall(&body.id)
        .await
        .map_err(|e| error_response(&e, StatusCode::BAD_REQUEST))?;
    Ok(Json(json!({
        "success": true,
        "data": { "id": body.id, "uninstalled": true }
    })))
}

/// GET /api/plugins/update —— 更新检查：拉当前发布目录，逐包比对已装版本。
///
/// 对齐 A2A `api::plugins::check_update`：一次请求即刷新目录并算出
/// `updates[]`（每包 `available / current / latest / asset / reason`）。
/// 目录不可用时不报 5xx —— 如实返回空 updates + `error/reason`，前端据此
/// 提示「检查失败」而不是把整页打挂。
pub async fn handle_check_plugin_updates(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let catalog = fetch_catalog(&state).await;
    let updates = compute_updates(&state.plugin_host.registry().list(), &catalog);
    let available = catalog
        .get("available")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let (error, reason) = if available {
        (false, Value::Null)
    } else {
        (
            true,
            catalog
                .get("reason")
                .cloned()
                .unwrap_or_else(|| json!("插件目录暂不可用")),
        )
    };
    Ok(Json(json!({
        "success": true,
        "data": {
            "updates": updates,
            "checked": true,
            "error": error,
            "reason": reason,
        }
    })))
}

/// GET /api/plugins/release —— 发布目录（从 `[plugins] source_url` 实时拉取）。
pub async fn handle_refresh_plugin_release(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    Ok(Json(
        json!({ "success": true, "data": refresh_catalog(&state).await }),
    ))
}

/// GET /api/plugins/progress —— 在途/近期操作（Operation 异步进度模型）。
///
/// 返回与列表页 `operations` 同源的快照：每个操作含
/// `action/stage/active/done/percent/error/result`，前端据此轮询批量任务。
pub async fn handle_get_plugin_progress(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    Ok(Json(json!({
        "success": true,
        "data": { "operations": plugin_operations(&state) }
    })))
}

/// 批量生命周期请求体（前端 `batchPlugins`）。
///
/// 两种形态（对齐 beta.2）：
/// - `{ "items": [{ id, action, url?, sha256?, catalog? }] }` —— 逐条完整；
/// - `{ "action": "update"|"enable"|"disable"|"uninstall", "ids": [...] }` ——
///   批量简写；`update` 时目录条目由服务端从当前目录快照按 id 解析后回填。
#[derive(Debug, Deserialize)]
pub struct BatchBody {
    #[serde(default)]
    pub items: Vec<crate::plugin_host::operations::BatchItem>,
    #[serde(default)]
    pub action: Option<String>,
    #[serde(default)]
    pub ids: Vec<String>,
}

/// POST /api/plugins/batch —— 批量安装/更新/启停/卸载。
///
/// 每个条目在后台独立执行，立即返回初始操作快照（`stage=waiting`）；
/// 后续进度与结果经 `/api/plugins/progress` 轮询。命中「同包已有在途操作」
/// 或参数非法时，该条目以 `stage=failed` 即时返回，不影响其它条目。
pub async fn handle_batch_plugins(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<BatchBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let mut items = body.items;
    if let Some(action) = body.action.as_deref() {
        if !matches!(
            action,
            "install" | "update" | "enable" | "disable" | "reload" | "uninstall"
        ) {
            return Err(error_response(
                &format!("未知的批量动作：{action}"),
                StatusCode::BAD_REQUEST,
            ));
        }
        let catalog = if action == "update" {
            Some(fetch_catalog(&state).await)
        } else {
            None
        };
        for id in body.ids {
            let entry = catalog.as_ref().and_then(|catalog| {
                catalog["plugins"]
                    .as_array()
                    .and_then(|plugins| plugins.iter().find(|item| item["id"] == id.as_str()))
                    .cloned()
            });
            items.push(crate::plugin_host::operations::BatchItem {
                id,
                action: action.to_string(),
                url: None,
                sha256: None,
                catalog: entry,
            });
        }
    }
    if items.is_empty() {
        return Err(error_response(
            "批量请求至少需要一条条目（items）或一组 id（ids）",
            StatusCode::BAD_REQUEST,
        ));
    }
    // 上限：一次最多 64 条，避免误操作把整个目录塞进来。
    if items.len() > 64 {
        return Err(error_response(
            "单次批量操作最多 64 条",
            StatusCode::BAD_REQUEST,
        ));
    }
    let data = crate::plugin_host::operations::batch(
        state.plugin_host.clone(),
        state.plugin_host.store(),
        items,
    );
    tracing::info!("[Plugins] 已受理批量生命周期操作（后台执行，进度见 /api/plugins/progress）");
    Ok(Json(json!({ "success": true, "data": data })))
}

/// 受控任务审批请求体。
#[derive(Debug, Deserialize)]
pub struct ManagedTaskBody {
    pub id: String,
    pub approved: bool,
}

/// POST /api/plugins/managed-task —— 运维批准/撤销某插件包的受控任务执行权。
///
/// 批准按包声明的 provider 落地；停用/卸载/重载会经 `wasm::forget` 自动撤销
/// （fail-closed）。插件侧 `managed_task.begin` 只有在其 provider 被批准后
/// 才成功。
pub async fn handle_managed_task_approval(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ManagedTaskBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    if body.id == BUILTIN_PACK_ID {
        return Err(error_response(
            "内置供应商目录不承载 Wasm 受控任务",
            StatusCode::BAD_REQUEST,
        ));
    }
    let Some(meta) = state.plugin_host.registry().get(&body.id) else {
        return Err(error_response(
            &format!("插件包「{}」不存在", body.id),
            StatusCode::NOT_FOUND,
        ));
    };
    let providers: Vec<String> = meta.providers.iter().map(|p| p.id.clone()).collect();
    if providers.is_empty() {
        return Err(error_response(
            "该插件包没有声明可受控执行的 provider",
            StatusCode::BAD_REQUEST,
        ));
    }
    for provider in &providers {
        if body.approved {
            crate::plugin_host::wasm::managed_task::approve(provider);
        } else {
            crate::plugin_host::wasm::managed_task::revoke(provider);
        }
    }
    tracing::info!(
        "[Plugins] 受控任务执行权「{}」已{}（provider: {:?}）",
        body.id,
        if body.approved { "批准" } else { "撤销" },
        providers
    );
    Ok(Json(json!({
        "success": true,
        "data": { "id": body.id, "approved": body.approved, "providers": providers }
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> Arc<FileStore> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("aigx-plugins-{}-{seq}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Arc::new(FileStore::new(dir))
    }

    #[test]
    fn provider_gates_roundtrip_and_persistence() {
        let store = temp_store();
        let gates = ProviderGates::new(store.clone());
        gates.load().unwrap();
        assert!(!gates.is_disabled("qoder"));
        // 停用 → 内存与 KV 都落
        assert!(gates.set_enabled("qoder", false).unwrap());
        assert!(gates.is_disabled("qoder"));
        assert!(!gates.is_disabled("loomy"));
        // 重复停用不记变化
        assert!(!gates.set_enabled("qoder", false).unwrap());
        // 重新加载（模拟重启）→ 仍然停用
        let reloaded = ProviderGates::new(store);
        reloaded.load().unwrap();
        assert!(reloaded.is_disabled("qoder"));
        // 启用恢复
        assert!(reloaded.set_enabled("qoder", true).unwrap());
        assert!(!reloaded.is_disabled("qoder"));
    }

    #[test]
    fn provider_view_reflects_disabled_state() {
        let mut disabled = HashSet::new();
        disabled.insert("qoder".to_string());
        let enabled_view = provider_view("loomy", &disabled);
        assert_eq!(enabled_view["enabled"], true);
        assert_eq!(enabled_view["loaded"], true);
        let disabled_view = provider_view("qoder", &disabled);
        assert_eq!(disabled_view["enabled"], false);
        assert_eq!(disabled_view["loaded"], false);
        // 开关能力标志：内置目录包的开关写全局 ProviderGates
        assert_eq!(enabled_view["toggle"], "global");
        assert_eq!(disabled_view["toggle"], "global");
        // 标签来自 registry
        assert!(enabled_view["label"].is_string());
    }

    #[test]
    fn builtin_pack_contains_registry_providers() {
        let pack = builtin_pack_view(&HashSet::new());
        let providers = pack["providers"].as_array().unwrap();
        // registry 静态表的每家都在内置包里
        for spec in crate::account_session::registry::PROVIDERS {
            assert!(
                providers
                    .iter()
                    .any(|p| p["id"].as_str() == Some(spec.kind)),
                "provider {} 应在内置包里",
                spec.kind
            );
        }
        // 会话型家也在
        assert!(providers.iter().any(|p| p["id"].as_str() == Some("cline")));
        // 包级字段契约（前端 PluginPackSummary）
        assert_eq!(pack["id"], BUILTIN_PACK_ID);
        assert_eq!(pack["enabled"], true);
        assert!(pack["host"].is_null());
    }

    #[test]
    fn external_pack_view_matches_frontend_contract() {
        let store = temp_store();
        let registry = Arc::new(crate::plugin_host::registry::PackRegistry::new(
            store.clone(),
        ));
        registry.load().unwrap();
        let root = std::env::temp_dir().join(format!(
            "aigx-extpack-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let mgr = PluginHostManager::new(
            registry,
            root.clone(),
            crate::test_http_client(),
            crate::plugin_host::process::HostEnv::default(),
            Arc::new(crate::account_session::AccountSessionStore::new(
                store.clone(),
            )),
            store,
        );
        let meta = PackMeta {
            id: "agent2api-providers".into(),
            name: "Agent2API Providers".into(),
            version: "3.4.2".into(),
            protocol: 1,
            providers: vec![crate::plugin_host::registry::PackProvider {
                id: "kiro".into(),
                label: "Kiro".into(),
                default_enabled: true,
            }],
            entry: "bin/plugin".into(),
            signed: false,
            installed_at: 1_700_000_000,
            enabled: true,
            ..Default::default()
        };
        // 入口不在磁盘 → entryPresent=false（前端提示重装）
        let view = external_pack_view(&meta, &mgr);
        assert_eq!(view["id"], "agent2api-providers");
        assert_eq!(view["enabled"], true);
        assert_eq!(view["signed"], false);
        assert_eq!(view["protocol"], 1);
        assert_eq!(view["installedAt"], 1_700_000_000);
        assert_eq!(view["entryPresent"], false);
        assert_eq!(view["requiredVersion"], Value::Null);
        assert_eq!(view["updateRequired"], false);
        assert_eq!(view["compatibilityError"], Value::Null);
        assert_eq!(view["providerCategory"], Value::Null);
        // host 字段契约（PluginHostState）
        assert_eq!(view["host"]["running"], false);
        assert_eq!(view["host"]["crashes"], 0);
        assert_eq!(view["host"]["disabled"], false); // !meta.enabled
        assert!(view["host"]["lastError"].is_null());
        assert!(view["host"]["providers"].as_array().unwrap().is_empty());
        // providers 契约：包启用但进程没跑 → enabled=true / loaded=false
        let provs = view["providers"].as_array().unwrap();
        assert_eq!(provs[0]["id"], "kiro");
        assert_eq!(provs[0]["label"], "Kiro");
        assert_eq!(provs[0]["enabled"], true);
        assert_eq!(provs[0]["loaded"], false);
        // 原生插件包现已支持 provider 级开关（写 PackMeta.disabled_providers）
        assert_eq!(provs[0]["toggle"], "global");
        assert_eq!(view["providerEnabled"]["kiro"], true);
        // 入口落盘后 entryPresent=true
        let entry = meta.entry_path(&root);
        std::fs::create_dir_all(entry.parent().unwrap()).unwrap();
        std::fs::write(&entry, b"x").unwrap();
        assert_eq!(external_pack_view(&meta, &mgr)["entryPresent"], true);

        // 逐家停用：view 与 providerEnabled 同步反映（与上游 provider_enabled 同口径）
        let mut off = meta.clone();
        off.disabled_providers = vec!["kiro".into()];
        let off_view = external_pack_view(&off, &mgr);
        assert_eq!(off_view["providers"][0]["enabled"], false);
        assert_eq!(off_view["providers"][0]["loaded"], false);
        assert_eq!(off_view["providerEnabled"]["kiro"], false);
        // 包级停用 = 全部停用
        let mut pack_off = meta.clone();
        pack_off.enabled = false;
        assert_eq!(
            external_pack_view(&pack_off, &mgr)["providerEnabled"]["kiro"],
            false
        );
    }

    #[test]
    fn normalize_catalog_wellformed() {
        let v: Value = serde_json::from_str(
            r#"{
                "current": "1.0.52",
                "plugins": [
                    {
                        "id": "agent2api-providers",
                        "name": "Agent2API",
                        "description": "d",
                        "version": "3.4.2",
                        "official": true,
                        "url": "https://x/p.zip",
                        "sha256": "abc",
                        "size": 10,
                        "platforms": [
                            {"target": "linux", "url": "https://x/p-linux.zip", "size": 5, "current": false, "sha256": "s1"}
                        ]
                    },
                    {"id": "flat-pack", "name": "Flat", "url": "https://x/flat.zip", "sha256": "abc", "size": 10}
                ]
            }"#,
        )
        .unwrap();
        let out = normalize_catalog(&v, "linux", None).unwrap();
        assert_eq!(out["available"], true);
        assert_eq!(out["current"], "1.0.52");
        assert_eq!(out["error"], false);
        assert!(out["reason"].is_null());
        let plugins = out["plugins"].as_array().unwrap();
        assert_eq!(plugins.len(), 2);
        // 有 platforms 的条目：透传平台条目（含 sha256）
        assert_eq!(plugins[0]["id"], "agent2api-providers");
        assert_eq!(plugins[0]["official"], true);
        assert_eq!(plugins[0]["platforms"][0]["url"], "https://x/p-linux.zip");
        assert_eq!(plugins[0]["platforms"][0]["sha256"], "s1");
        // 无 platforms 但有顶层 url → 合成一条当前平台条目
        assert_eq!(plugins[1]["platforms"][0]["target"], "linux");
        assert_eq!(plugins[1]["platforms"][0]["url"], "https://x/flat.zip");
        assert_eq!(plugins[1]["platforms"][0]["sha256"], "abc");
        assert_eq!(plugins[1]["platforms"][0]["size"], 10);
    }

    #[test]
    fn normalize_catalog_resolves_relative_icon_and_keeps_meta() {
        let v: Value = serde_json::from_str(
            r#"{"plugins":[{"id":"p","name":"P","icon":"icons/p.png",
                 "category":"agent","requireSigned":true,"tags":["community","wasm"],
                 "url":"https://x/p.zip"}]}"#,
        )
        .unwrap();
        let out = normalize_catalog(&v, "linux", Some(UPSTREAM_INDEX_RAW_BASE)).unwrap();
        let p = &out["plugins"][0];
        assert_eq!(
            p["icon"].as_str().unwrap(),
            format!("{UPSTREAM_INDEX_RAW_BASE}/icons/p.png")
        );
        assert_eq!(p["category"], "agent");
        assert_eq!(p["requireSigned"], true);
        assert_eq!(p["tags"], serde_json::json!(["community", "wasm"]));
        // data URI / 绝对 URL 不被改写
        let v = serde_json::json!({"plugins":[{"id":"q","icon":"https://e/a.svg","url":"https://x/q.zip"}]});
        let out = normalize_catalog(&v, "linux", Some(UPSTREAM_INDEX_RAW_BASE)).unwrap();
        assert_eq!(out["plugins"][0]["icon"], "https://e/a.svg");
    }

    #[test]
    fn normalize_catalog_rejects_malformed() {
        // 缺 plugins 数组
        let v = serde_json::json!({"current": "1"});
        assert!(normalize_catalog(&v, "linux", None).is_none());
        // plugins 不是数组
        let v = serde_json::json!({"plugins": "x"});
        assert!(normalize_catalog(&v, "linux", None).is_none());
    }

    #[test]
    fn normalize_catalog_entry_without_url_keeps_empty_platforms() {
        let v = serde_json::json!({"plugins": [{"id": "p", "name": "P"}]});
        let out = normalize_catalog(&v, "linux", None).unwrap();
        assert_eq!(out["available"], true);
        let plugins = out["plugins"].as_array().unwrap();
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0]["platforms"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn catalog_unavailable_shape() {
        let v = catalog_unavailable("未配置插件源（config.toml [plugins] source_url）");
        assert_eq!(v["available"], false);
        assert_eq!(v["error"], true);
        assert_eq!(
            v["reason"],
            "未配置插件源（config.toml [plugins] source_url）"
        );
        assert!(v["plugins"].as_array().unwrap().is_empty());
        assert_eq!(v["current"], "");
    }

    /// 内置发布目录：三个上游包都要在列；两个原生包在当前平台能解析出一条
    /// 下载记录（含 size/sha256）；Wasm 包不产生可下载条目（AIGX 无 Wasm 运行时）。
    #[test]
    fn builtin_catalog_shape() {
        let v = builtin_catalog();
        assert_eq!(v["available"], true);
        assert_eq!(v["error"], false);
        let plugins = v["plugins"].as_array().unwrap();
        assert_eq!(plugins.len(), 3, "内置目录应列出上游三个插件");

        for id in ["agent2api-providers", "agent2api-web-providers"] {
            let p = plugins
                .iter()
                .find(|p| p["id"] == id)
                .expect("内置目录缺条目");
            assert_eq!(p["official"], false, "上游社区包不打官方徽标");
            assert_eq!(p["prerelease"], true);
            // 图标：相对路径已解析到上游索引仓库 raw 基址（可被 <img> 加载）
            let icon = p["icon"].as_str().unwrap_or_default();
            assert!(
                icon.starts_with(UPSTREAM_INDEX_RAW_BASE) && icon.ends_with(".png"),
                "内置目录图标应为绝对 raw 地址: {icon}"
            );
            let plats = p["platforms"].as_array().unwrap();
            if current_target_triple().is_some() {
                assert_eq!(plats.len(), 1, "{id} 当前平台应恰好一条");
                assert_eq!(plats[0]["target"], std::env::consts::OS);
                assert!(plats[0]["size"].as_u64().unwrap() > 0);
                let url = plats[0]["url"].as_str().unwrap();
                assert!(
                    url.starts_with("https://github.com/Agent2Builder/"),
                    "下载地址应指向上游 Release: {url}"
                );
                assert!(url.ends_with(".zip"), "{url}");
                assert_eq!(plats[0]["sha256"].as_str().unwrap().len(), 64);
            }
        }

        // Wasm 包：任何常规平台的 `current_target_triple` 都不会等于
        // `wasm32-unknown-unknown`，因此不可下载。
        let wasm = plugins
            .iter()
            .find(|p| p["id"] == "agent2api-wasm-atomcode")
            .expect("内置目录缺 Wasm 条目");
        assert!(wasm["platforms"].as_array().unwrap().is_empty());
    }

    #[test]
    fn upstream_asset_url_shape() {
        assert_eq!(
            upstream_asset_url(
                "v3.5.0-beta.1",
                "agent2api-providers",
                "3.5.0-beta.1",
                "x86_64-unknown-linux-gnu"
            ),
            "https://github.com/Agent2Builder/agent2api-providers-releases/releases/download/\
             v3.5.0-beta.1/agent2api-providers-3.5.0-beta.1-x86_64-unknown-linux-gnu.zip"
        );
    }

    /// 线上 404 根因回归：wasm 包 manifest 声明 `wasm32-unknown-unknown` 与
    /// 自定义 `release.asset` 模板时，URL 必须用 wasm 资产（而非本机 triple），
    /// 但 `platforms[].target` 仍填 OS（前端按 OS 才显示「下载」）。
    #[test]
    fn upstream_catalog_item_uses_manifest_platform_for_wasm() {
        let entry = json!({
            "id": "agent2api-wasm-accio-cn",
            "name": "Accio CN",
            "manifest": "plugins/agent2api-wasm-accio-cn.json",
        });
        let manifest = json!({
            "version": "0.1.0-beta.1",
            "platforms": ["wasm32-unknown-unknown"],
            "release": {
                "repo": "Agent2Builder/agent2api-wasm-releases",
                "tag": "wasm-accio-cn-v{version}",
                "asset": "{id}-{version}-{target}.zip"
            }
        });
        let item = upstream_catalog_item("agent2api-wasm-accio-cn", &entry, &manifest);
        let platform = &item["platforms"][0];
        assert_eq!(platform["target"], std::env::consts::OS);
        let url = platform["url"].as_str().unwrap();
        assert!(
            url.ends_with("-wasm32-unknown-unknown.zip"),
            "wasm 包应使用 manifest 声明的 wasm triple: {url}"
        );
        assert!(
            url.contains("wasm-accio-cn-v0.1.0-beta.1"),
            "tag 模板应替换 {{version}}: {url}"
        );
        // wasm 资产表里没有对应行 → size=0 / sha=null（不阻断下载）
        assert_eq!(platform["size"], 0);
        assert!(platform["sha256"].is_null());
    }

    /// 原生 manifest：本机 triple 在 platforms 里 → 仍走本机资产；
    /// 无 `platforms` 的老 manifest → 回退本机 triple（保持旧行为）。
    #[test]
    fn upstream_catalog_item_keeps_host_triple_for_native_and_legacy() {
        let Some(host_triple) = current_target_triple() else {
            return; // 非常规平台：本函数本就回退空 platforms，跳过
        };
        let entry = json!({ "id": "agent2api-providers", "name": "Providers" });
        let native = json!({
            "version": "3.4.2",
            "platforms": [host_triple],
            "release": { "tag": "v{version}" }
        });
        let item = upstream_catalog_item("agent2api-providers", &entry, &native);
        let url = item["platforms"][0]["url"].as_str().unwrap();
        assert!(
            url.ends_with(&format!("-{host_triple}.zip")),
            "原生包应走本机 triple: {url}"
        );
        assert_eq!(item["platforms"][0]["target"], std::env::consts::OS);

        // 老 manifest 不声明 platforms
        let legacy = json!({ "version": "3.4.2", "release": { "tag": "v{version}" } });
        let item = upstream_catalog_item("agent2api-providers", &entry, &legacy);
        let url = item["platforms"][0]["url"].as_str().unwrap();
        assert!(
            url.ends_with(&format!("-{host_triple}.zip")),
            "老 manifest 回退本机 triple: {url}"
        );
    }

    /// 列表热路径缓存：连续 N 次视图只读一次 `pack.json` / 图标文件；
    /// 删除图标文件（pack.json 未变）仍命中缓存；重写 pack.json（size 变化）
    /// 自动失效。
    #[test]
    fn pack_view_cache_avoids_repeated_disk_reads() {
        let (host, root) = temp_host();
        let meta = cache_test_meta();
        let version_dir = meta.version_dir(&root);
        std::fs::create_dir_all(&version_dir).unwrap();
        std::fs::write(
            version_dir.join("pack.json"),
            r#"{"description":"cached","icons":"logo.svg","ui":[]}"#,
        )
        .unwrap();
        std::fs::write(version_dir.join("logo.svg"), b"<svg/>").unwrap();

        for _ in 0..25 {
            let view = external_pack_view(&meta, &host);
            assert_eq!(view["description"], "cached");
            assert!(view["icon"]
                .as_str()
                .unwrap()
                .starts_with("data:image/svg+xml;base64,"));
        }
        let (icon_hits, icon_misses) = host.view_cache().icon_stats();
        assert_eq!(icon_misses, 1, "25 次视图只应读 1 次图标文件");
        assert_eq!(icon_hits, 24);
        let (manifest_hits, manifest_misses) = host.view_cache().manifest_stats();
        assert_eq!(manifest_misses, 1, "25 次视图只应读 1 次 pack.json");
        assert_eq!(manifest_hits, 24);
        assert_eq!(host.view_cache().icon_len(), 1);
        assert_eq!(host.view_cache().manifest_len(), 1);

        // 行为反证：图标文件删掉但 pack.json 未动 → 仍命中缓存（没有重新读盘）
        std::fs::remove_file(version_dir.join("logo.svg")).unwrap();
        assert!(external_pack_view(&meta, &host)["icon"].as_str().is_some());

        // 失效：pack.json 内容变化（size 不同）→ 新指纹，重新解析
        std::fs::write(
            version_dir.join("pack.json"),
            r#"{"description":"changed-after-rewrite"}"#,
        )
        .unwrap();
        let view = external_pack_view(&meta, &host);
        assert_eq!(view["description"], "changed-after-rewrite");
        // 图标声明已移除且文件不存在 → 回落 None
        assert!(view["icon"].is_null());
    }

    /// Micro-benchmark（打印计时，不做时间断言）：直读 vs 缓存的 N 次耗时。
    #[test]
    fn micro_bench_pack_view_cache() {
        let (host, root) = temp_host();
        let meta = cache_test_meta();
        let version_dir = meta.version_dir(&root);
        std::fs::create_dir_all(&version_dir).unwrap();
        std::fs::write(
            version_dir.join("pack.json"),
            r#"{"description":"bench","icons":"logo.png","ui":[]}"#,
        )
        .unwrap();
        // 64 KiB 图标：base64 编码是每次请求的主要成本
        std::fs::write(version_dir.join("logo.png"), vec![0x89u8; 64 * 1024]).unwrap();

        // 预热缓存
        let _ = manifest_extras_for(&host, &meta);
        let _ = pack_icon_for(&host, &meta, Some("logo.png"));

        let iterations = 200usize;
        let raw_start = std::time::Instant::now();
        for _ in 0..iterations {
            let extras = manifest_extras(&version_dir);
            let icon = pack_icon(&version_dir, Some("logo.png"));
            std::hint::black_box((extras, icon));
        }
        let raw = raw_start.elapsed();

        let cached_start = std::time::Instant::now();
        for _ in 0..iterations {
            let extras = manifest_extras_for(&host, &meta);
            let icon = pack_icon_for(&host, &meta, Some("logo.png"));
            std::hint::black_box((extras, icon));
        }
        let cached = cached_start.elapsed();

        eprintln!(
            "[bench] {iterations} 次 manifest+icon: 直读={raw:?}, 缓存={cached:?} \
             (约 {:.1}x)",
            raw.as_secs_f64() / cached.as_secs_f64().max(f64::EPSILON)
        );
        assert!(
            cached < raw,
            "缓存路径应快于每次读盘+base64（直读={raw:?}, 缓存={cached:?}）"
        );
    }

    /// 缓存测试用的管理器（独立临时目录，互不干扰）。
    fn temp_host() -> (PluginHostManager, std::path::PathBuf) {
        let store = temp_store();
        let registry = Arc::new(crate::plugin_host::registry::PackRegistry::new(
            store.clone(),
        ));
        registry.load().unwrap();
        let root = std::env::temp_dir().join(format!(
            "aigx-viewcache-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let host = PluginHostManager::new(
            registry,
            root.clone(),
            crate::test_http_client(),
            crate::plugin_host::process::HostEnv::default(),
            Arc::new(crate::account_session::AccountSessionStore::new(
                store.clone(),
            )),
            store,
        );
        (host, root)
    }

    fn cache_test_meta() -> PackMeta {
        PackMeta {
            id: "cache-pack".into(),
            name: "Cache Pack".into(),
            version: "1.0.0".into(),
            protocol: 1,
            entry: "bin/plugin".into(),
            enabled: true,
            ..Default::default()
        }
    }

    /// 发布条目与资产表必须双向对齐：改名后遗留的单边条目会让某平台永远空目录。
    #[test]
    fn builtin_releases_and_assets_stay_in_sync() {
        for rel in BUILTIN_RELEASES {
            assert!(
                UPSTREAM_ASSETS.iter().any(|(id, _, _, _)| *id == rel.id),
                "{} 在 UPSTREAM_ASSETS 里没有任何资产行",
                rel.id
            );
        }
        for (id, _, _, _) in UPSTREAM_ASSETS {
            assert!(
                BUILTIN_RELEASES.iter().any(|r| r.id == *id),
                "UPSTREAM_ASSETS 有未声明的包 {id}"
            );
        }
    }

    /// A2A 对齐字段：内置包视图必须带齐新键（前端按存在性读，缺键会显示空白）。
    #[test]
    fn builtin_pack_view_carries_a2a_alignment_fields() {
        let mut disabled = HashSet::new();
        disabled.insert("qoder".to_string());
        let pack = builtin_pack_view(&disabled);
        // 原有键不回归
        assert_eq!(pack["id"], BUILTIN_PACK_ID);
        assert_eq!(pack["enabled"], true);
        assert!(pack["host"].is_null());
        // 新键
        assert_eq!(pack["icons"], Value::Null);
        assert!(pack["ui"].as_array().unwrap().is_empty());
        assert_eq!(pack["providerEnabled"]["qoder"], false);
        assert_eq!(pack["providerEnabled"]["loomy"], true);
        assert!(pack["startup"].is_null());
        // providers 已按停用名单投影（与 providerEnabled 同源）
        let qoder = pack["providers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == "qoder")
            .expect("内置包应含 qoder");
        assert_eq!(qoder["enabled"], false);
        // 内置包的 provider 开关走全局 gates（前端按 toggle 判可点）
        assert_eq!(qoder["toggle"], "global");
        assert_eq!(
            pack["providerEnabled"]["qoder"], qoder["enabled"],
            "providerEnabled 与 providers[].enabled 必须同源"
        );
    }

    /// UI 页面声明侧校验：路径穿越在**声明期**就要挡掉（托管端点还会再挡一次）。
    #[test]
    fn pack_ui_pages_reject_traversal_entries() {
        let pages = vec![
            json!({ "id": "checkin", "title": "签到中心", "entry": "index.html", "order": 1 }),
            json!({ "id": "bad1", "title": "越界", "entry": "../../etc/passwd" }),
            json!({ "id": "bad2", "title": "绝对", "entry": "/etc/passwd.html" }),
            json!({ "id": "bad3", "title": "非 HTML", "entry": "page.js" }),
            json!({ "id": "bad4", "title": "盘符", "entry": "C:/x.html" }),
        ];
        let out = pack_ui_pages("p1", &pages);
        assert_eq!(out.len(), 1, "只有合法页应留下: {out:?}");
        assert_eq!(out[0]["id"], "checkin");
        assert_eq!(out[0]["url"], "/plugins/p1/ui/index.html");
        assert_eq!(out[0]["order"], 1);
        // 反斜杠分隔的相对路径要归一成 `/`
        let win = pack_ui_pages(
            "p2",
            &[json!({ "id": "w", "title": "W", "entry": "pages\\a.html" })],
        );
        assert_eq!(win[0]["url"], "/plugins/p2/ui/pages/a.html");
    }

    /// `ui_entry_ok` 的逐条边界（与 pack_ui_pages 共用同一判据）。
    #[test]
    fn ui_entry_validation_boundaries() {
        assert!(ui_entry_ok("index.html"));
        assert!(ui_entry_ok("pages/checkin.HTML"));
        assert!(!ui_entry_ok(""));
        assert!(!ui_entry_ok("/index.html"));
        assert!(!ui_entry_ok("../index.html"));
        assert!(!ui_entry_ok("a/../b.html"));
        assert!(!ui_entry_ok("C:/index.html"));
        assert!(!ui_entry_ok("index.js"));
    }

    /// 图标目录键校验：绝对路径 / 盘符 / `..` 一律不读。
    #[test]
    fn pack_icon_rejects_unsafe_dirs() {
        let dir = std::env::temp_dir();
        for bad in ["/etc", "C:\\icons", "../icons", "a/../../b"] {
            assert!(pack_icon(&dir, Some(bad)).is_none(), "{bad} 应被拒绝");
        }
        // 不存在的目录如实回落 None（装饰字段不该失败）；用独立空目录，
        // 避免 /tmp 里恰好有 logo.png 时误命中兜底布局。
        let empty = tempfile::tempdir().unwrap();
        assert!(pack_icon(empty.path(), Some("no-such-icons-dir")).is_none());
    }

    /// 社区包图标布局兼容：清单写文件路径 / 写目录 / 完全不写（icons、
    /// assets、包根的 logo/icon）都要找到；扩展名大小写不敏感。
    #[test]
    fn pack_icon_supports_file_and_fallback_layouts() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("assets")).unwrap();
        std::fs::write(
            dir.path().join("assets/logo.svg"),
            b"<svg xmlns='http://www.w3.org/2000/svg'></svg>",
        )
        .unwrap();
        // 清单指向文件 / 目录 / 未声明
        let file = pack_icon(dir.path(), Some("assets/logo.svg")).unwrap();
        assert!(file.starts_with("data:image/svg+xml;base64,"));
        assert!(pack_icon(dir.path(), Some("assets")).is_some());
        assert!(pack_icon(dir.path(), None).is_some());

        // 大写扩展名同样识别，且 MIME 归一到小写
        std::fs::write(dir.path().join("icon.PNG"), b"\x89PNG\r\n\x1a\nrest").unwrap();
        let upper = pack_icon(dir.path(), Some("icon.PNG")).unwrap();
        assert!(upper.starts_with("data:image/png;base64,"));

        // 没有任何图片的目录 → None
        let empty = tempfile::tempdir().unwrap();
        assert!(pack_icon(empty.path(), Some("icons")).is_none());
    }

    /// 商店条目的相对 icon 要拼成可加载的绝对地址；绝对/data URI 原样保留；
    /// 危险路径丢弃。
    #[test]
    fn store_icon_resolution() {
        let base = Some(UPSTREAM_INDEX_RAW_BASE);
        assert_eq!(
            resolve_store_icon(Some("icons/x.png"), base).as_deref(),
            Some("https://raw.githubusercontent.com/Agent2plugin/Agent2plugin/main/icons/x.png")
        );
        assert_eq!(
            resolve_store_icon(Some("https://example.com/a.svg"), base).as_deref(),
            Some("https://example.com/a.svg")
        );
        assert_eq!(
            resolve_store_icon(Some("data:image/png;base64,AA"), base).as_deref(),
            Some("data:image/png;base64,AA")
        );
        for bad in ["../x.png", "/etc/x.png", "a\\b.png", "C:/x.png"] {
            assert!(resolve_store_icon(Some(bad), base).is_none(), "{bad}");
        }
        // 无基址时相对路径无法解析
        assert!(resolve_store_icon(Some("icons/x.png"), None).is_none());
        assert!(resolve_store_icon(None, base).is_none());
        // 自定义源基址
        assert_eq!(
            source_base("https://h.example/plugins/index.json"),
            Some("https://h.example/plugins")
        );
        assert_eq!(
            source_base("https://h.example"),
            None,
            "无路径 URL 不产生基址"
        );
    }

    /// 设置投影形状：A2A camelCase 键齐，检查计划取全局 `scheduledTasks.updateCheck`
    /// （与更新页同一事实源，单位本来就是分钟）。
    #[test]
    fn plugin_settings_view_shape() {
        let config = crate::config::AppConfig {
            update: crate::config::UpdateConfig {
                auto_apply: false,
                include_prerelease: false,
                ..Default::default()
            },
            scheduled_tasks: crate::scheduled_tasks::types::ScheduledSettings {
                update_check: crate::scheduled_tasks::types::IntervalTask {
                    enabled: true,
                    interval: 1440,
                },
                ..Default::default()
            },
            ..Default::default()
        };
        let view = plugin_settings_view(&config);
        assert_eq!(view["autoCheck"], true);
        assert_eq!(view["autoUpdate"], false);
        assert_eq!(view["interval"], 1440);
        assert_eq!(view["stable"], true);
        assert_eq!(view["beta"], false);
        assert_eq!(view["channelMode"], "follow");
        assert_eq!(view["checkMode"], "follow");
        // 白名单覆盖面
        for key in [
            "autoCheck",
            "intervalMinutes",
            "beta",
            "stable",
            "autoUpdate",
            "channelMode",
            "checkMode",
        ] {
            assert!(PLUGIN_SETTINGS_KEYS.contains(&key), "白名单缺 {key}");
        }
    }

    /// 更新检查：目录新版本 + 当前平台有包 → available；同版本 → 不可更新。
    #[test]
    fn compute_updates_marks_newer_with_platform() {
        let os = std::env::consts::OS;
        let metas = vec![
            PackMeta {
                id: "pack-a".into(),
                version: "1.0.0".into(),
                ..Default::default()
            },
            PackMeta {
                id: "pack-b".into(),
                version: "2.0.0".into(),
                ..Default::default()
            },
        ];
        let catalog = json!({
            "plugins": [
                { "id": "pack-a", "version": "1.1.0",
                  "platforms": [{ "target": os, "url": "https://x/a.zip", "size": 5, "sha256": "abc" }] },
                { "id": "pack-b", "version": "2.0.0",
                  "platforms": [{ "target": os, "url": "https://x/b.zip" }] },
            ]
        });
        let rows = compute_updates(&metas, &catalog);
        let a = rows.iter().find(|r| r["id"] == "pack-a").unwrap();
        assert_eq!(a["available"], true);
        assert_eq!(a["latest"], "1.1.0");
        assert_eq!(a["current"], "1.0.0");
        assert_eq!(a["asset"]["url"], "https://x/a.zip");
        assert_eq!(a["asset"]["sha256"], "abc");
        let b = rows.iter().find(|r| r["id"] == "pack-b").unwrap();
        assert_eq!(b["available"], false, "同版本不算可更新");
        assert_eq!(b["reason"], "已经是最新版本");
    }

    /// 更新检查：目录未收录的包不出行；平台无包 → available=false + 无 asset。
    #[test]
    fn compute_updates_skips_missing_platform_and_unknown() {
        let os = std::env::consts::OS;
        let other = if os == "linux" { "windows" } else { "linux" };
        let metas = vec![
            PackMeta {
                id: "pack-a".into(),
                version: "1.0.0".into(),
                ..Default::default()
            },
            PackMeta {
                id: "pack-unknown".into(),
                version: "1.0.0".into(),
                ..Default::default()
            },
        ];
        let catalog = json!({
            "plugins": [
                { "id": "pack-a", "version": "9.9.9",
                  "platforms": [{ "target": other, "url": "https://x/a.zip" }] },
            ]
        });
        let rows = compute_updates(&metas, &catalog);
        assert!(
            rows.iter().all(|r| r["id"] != "pack-unknown"),
            "未收录包不应出行"
        );
        let a = rows.iter().find(|r| r["id"] == "pack-a").unwrap();
        assert_eq!(a["available"], false);
        assert!(a["asset"].is_null());
    }

    /// 更新检查：目录不可用（无 plugins 数组）→ 空表，不 panic。
    #[test]
    fn compute_updates_empty_when_catalog_unavailable() {
        let metas = vec![PackMeta {
            id: "pack-a".into(),
            version: "1.0.0".into(),
            ..Default::default()
        }];
        assert!(compute_updates(&metas, &json!({ "plugins": [] })).is_empty());
        assert!(compute_updates(&metas, &json!({})).is_empty());
    }

    /// 内置包（id=builtin）不参与更新检查。
    #[test]
    fn compute_updates_ignores_builtin_pack() {
        let metas = vec![PackMeta {
            id: BUILTIN_PACK_ID.into(),
            version: "1.0".into(),
            ..Default::default()
        }];
        let catalog =
            json!({ "plugins": [{ "id": BUILTIN_PACK_ID, "version": "9.9", "platforms": [] }] });
        assert!(compute_updates(&metas, &catalog).is_empty());
    }
}
