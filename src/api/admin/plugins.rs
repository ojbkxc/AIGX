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

use std::collections::HashSet;
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
use crate::plugin_host::{PackMeta, PluginHostManager};
use crate::storage::FileStore;

/// KV 键：provider 级停用名单（`Vec<provider kind>`）。
const DISABLED_PROVIDERS_KEY: &str = "plugins:disabled_providers";

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
fn provider_view(kind: &str, disabled: &HashSet<String>) -> Value {
    let label = crate::model::manage::provider_label(kind);
    json!({
        "id": kind,
        "label": label,
        // 停用名单里没有 = 启用
        "enabled": !disabled.contains(kind),
        // 内置实现：启用的家随进程常驻（registry 静态表），恒 loaded=enabled
        "loaded": !disabled.contains(kind),
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
    })
}

/// 外置插件包视图（前端 `PluginPackSummary` 契约）。
///
/// - `enabled` 以 `PackMeta.enabled` 为准（包级开关）：包声明里的
///   `default_enabled` 只是建议，网关不据此裁剪——包启用即全部申报；
/// - `loaded` = 包启用 **且** 握手申报了该家（进程真的在跑）；
/// - `entryPresent` 实测磁盘（入口被手工删过 = false，前端提示重装）；
/// - `host.crashes` 取监督循环的崩溃计数（重新启用清零）。
fn external_pack_view(meta: &PackMeta, host: &PluginHostManager) -> Value {
    let rt = host.runtime_of(&meta.id);
    let entry_present = meta.entry_path(host.plugins_root()).is_file();
    let providers: Vec<Value> = meta
        .providers
        .iter()
        .map(|pp| {
            let loaded = rt.providers.iter().any(|p| p.id == pp.id);
            json!({
                "id": pp.id,
                "label": pp.label,
                "enabled": meta.enabled,
                "loaded": meta.enabled && loaded,
            })
        })
        .collect();
    json!({
        "id": meta.id,
        "name": meta.name,
        "description": null,
        "version": meta.version,
        "requiredVersion": null,
        "updateRequired": false,
        "compatibilityError": null,
        "enabled": meta.enabled,
        "signed": meta.signed,
        "installedAt": meta.installed_at,
        "protocol": meta.protocol,
        "entryPresent": entry_present,
        "homepage": null,
        "icon": null,
        "providers": providers,
        "host": {
            "running": rt.running,
            "providers": rt.providers.iter().map(|p| p.id.clone()).collect::<Vec<_>>(),
            "crashes": rt.crashes,
            "disabled": !meta.enabled,
            "lastError": rt.last_error,
        },
        "providerCategory": null,
    })
}

/// GET /api/plugins —— 插件列表（内置目录包 + 全部外置包）+ 发布目录。
pub async fn handle_list_plugins(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let disabled = state.provider_gates.disabled_set();
    // 内置目录包 + 注册表里的全部外置包
    let mut packs = vec![builtin_pack_view(&disabled)];
    for meta in state.plugin_host.registry().list() {
        packs.push(external_pack_view(&meta, &state.plugin_host));
    }
    let catalog = fetch_catalog(&state).await;
    Ok(Json(json!({
        "success": true,
        "data": {
            "packs": packs,
            // 宿主进程状态在各外置包的 `host` 字段里（一包一进程）；
            // 顶层 host 留空（内置目录无宿主进程）
            "host": null,
            // 管理面板注入点（上游 adminPanels）：暂无
            "adminPanels": [],
            // 账号页功能注入点（上游 accountFeatures）：暂无
            "accountFeatures": {},
            "target": std::env::consts::OS,
            "protocol": 1,
            "pluginsDir": state.plugin_host.plugins_root().display().to_string(),
            "catalog": catalog,
            "updates": [],
            "operations": [],
        }
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

/// 发布目录：从 `config.toml [plugins] source_url` 拉取（10s 超时）。
///
/// 未配置 / 拉取失败 / 返回内容无法识别 → `available:false` + 中文
/// reason（`error:true`，前端渲染降级态而不是空目录白屏）。
async fn fetch_catalog(state: &AppState) -> Value {
    let source_url = state
        .config_manager
        .get()
        .await
        .plugins
        .source_url
        .trim()
        .to_string();
    if source_url.is_empty() {
        return catalog_unavailable("未配置插件源（config.toml [plugins] source_url）");
    }
    let resp = match state
        .http_client
        .get(&source_url)
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
    match normalize_catalog(&body, std::env::consts::OS) {
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
fn normalize_catalog(v: &Value, target_os: &str) -> Option<Value> {
    let plugins = v.get("plugins")?.as_array()?;
    let current = v.get("current").and_then(|x| x.as_str()).unwrap_or("");
    let items: Vec<Value> = plugins
        .iter()
        .filter_map(|item| normalize_store_plugin(item, target_os))
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
fn normalize_store_plugin(item: &Value, target_os: &str) -> Option<Value> {
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
        "icon": str_field("icon"),
        "platforms": platforms,
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
        return Err(error_response(
            &format!("插件包「{}」不存在（AIGX 当前只有内置目录包）", body.id),
            StatusCode::NOT_FOUND,
        ));
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

/// GET /api/plugins/update —— 更新检查（内置实现：返回空 + 说明）。
pub async fn handle_check_plugin_updates(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    Ok(Json(json!({
        "success": true,
        "data": {
            "updates": [],
            "note": "内置 provider 目录随网关版本发布（无独立插件包更新渠道）",
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
        json!({ "success": true, "data": fetch_catalog(&state).await }),
    ))
}

/// GET /api/plugins/progress —— 在途操作（内置实现恒空）。
pub async fn handle_get_plugin_progress(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    Ok(Json(
        json!({ "success": true, "data": { "operations": [] } }),
    ))
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
            reqwest::Client::new(),
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
        // 入口落盘后 entryPresent=true
        let entry = meta.entry_path(&root);
        std::fs::create_dir_all(entry.parent().unwrap()).unwrap();
        std::fs::write(&entry, b"x").unwrap();
        assert_eq!(external_pack_view(&meta, &mgr)["entryPresent"], true);
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
        let out = normalize_catalog(&v, "linux").unwrap();
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
    fn normalize_catalog_rejects_malformed() {
        // 缺 plugins 数组
        let v = serde_json::json!({"current": "1"});
        assert!(normalize_catalog(&v, "linux").is_none());
        // plugins 不是数组
        let v = serde_json::json!({"plugins": "x"});
        assert!(normalize_catalog(&v, "linux").is_none());
    }

    #[test]
    fn normalize_catalog_entry_without_url_keeps_empty_platforms() {
        let v = serde_json::json!({"plugins": [{"id": "p", "name": "P"}]});
        let out = normalize_catalog(&v, "linux").unwrap();
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
}
