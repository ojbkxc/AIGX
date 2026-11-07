//! 插件管理面 API（`/api/plugins`，对照 Agent2API `api/plugins.rs` 的管理面）。
//!
//! ## AIGX 的分层决策（07-recon2.md B 章方案②「契约化但不进程化」）
//!
//! Agent2API 的插件宿主是外置子进程 + RPC 契约（一包 = 一进程 = 若干家
//! provider），收益是「一个包崩溃不带走网关」。AIGX 的 provider 已内生在
//! `account_session::registry`（30 家静态表），自托管单实例语境下进程隔离
//! 收益低、代价高（第二个包分发渠道 + 签名信任链）。
//!
//! 因此本模块做的是**管理面契约**：
//! - `/api/plugins` 列表：把 registry 的静态目录按「内置包」的形状下发
//!   （`packs[].providers[].{id,label,enabled,loaded}`），前端插件页
//!   零改动渲染；
//! - 启停：包级（整包）与 provider 级两级开关，落 KV（`alert_store`）；
//!   provider 级停用 = 数据面选号时跳过该家的账号（`bridge::account`
//!   的 acquire 过滤），账号数据保留、重启用即恢复 —— 与上游「停用/
//!   卸载回落内置」同一语义；
//! - 卸载/更新/进度/目录：如实回「内置实现不支持」（200 + 说明），页面
//!   显示降级态而不是 404 白屏。
//!
//! 进程隔离（spawn/握手/崩溃退避/流式 RPC）**刻意不做**——需要外置插件
//! 包分发链路时再评估（B 章方案①，8–14 人周）。

use std::collections::HashSet;
use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::Json;
use parking_lot::RwLock;
use serde::Deserialize;
use serde_json::{json, Value};

use super::common::{error_response, verify_admin};
use crate::api::openai::AppState;
use crate::storage::FileStore;

/// KV 键：provider 级停用名单（`Vec<provider kind>`）。
const DISABLED_PROVIDERS_KEY: &str = "plugins:disabled_providers";

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
}

#[derive(Debug, Deserialize)]
pub struct ProviderBody {
    pub id: String,
    pub provider: String,
    pub enabled: bool,
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

/// GET /api/plugins —— 插件列表（内置包）+ 不支持能力的降级态。
pub async fn handle_list_plugins(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let disabled = state.provider_gates.disabled_set();
    Ok(Json(json!({
        "success": true,
        "data": {
            "packs": [builtin_pack_view(&disabled)],
            // 内置实现无宿主进程（进程化刻意不做，见模块头）
            "host": null,
            // 管理面板注入点（上游 adminPanels）：内置实现暂无
            "adminPanels": [],
            // 账号页功能注入点（上游 accountFeatures）：内置实现暂无
            "accountFeatures": {},
            "target": std::env::consts::OS,
            "protocol": 1,
            "pluginsDir": "builtin",
            // 发布目录 / 更新检查 / 在途操作：外置包能力，内置实现不支持
            "catalog": catalog_stub(),
            "updates": [],
            "operations": [],
        }
    })))
}

/// 发布目录的不支持态（前端渲染「暂不支持」而不是空目录白屏）。
fn catalog_stub() -> Value {
    json!({
        "available": false,
        "current": "",
        "plugins": [],
        "reason": "AIGX 为内置 provider 目录（无外置插件包分发渠道），发布目录暂不适用",
        "error": false,
    })
}

/// POST /api/plugins/enable —— 包级启停。
///
/// 内置包是二进制的一部分，不支持整包停用（停掉 = 没有任何上游可用）；
/// 如实报错引导用 provider 级开关。
pub async fn handle_set_plugin_enabled(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<PackIdBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    if body.id != BUILTIN_PACK_ID {
        return Err(error_response(
            &format!("插件包「{}」不存在（AIGX 当前只有内置目录包）", body.id),
            StatusCode::NOT_FOUND,
        ));
    }
    Err(error_response(
        "内置供应商目录不支持整包停用；请在包内按家停用（providers[].enabled）",
        StatusCode::BAD_REQUEST,
    ))
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
    let _ = body;
    Err(error_response(
        "内置供应商目录编译在二进制里，不支持卸载；停用某家用 provider 级开关",
        StatusCode::BAD_REQUEST,
    ))
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

/// GET /api/plugins/release —— 发布目录（内置实现：不可用态）。
pub async fn handle_refresh_plugin_release(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    Ok(Json(json!({ "success": true, "data": catalog_stub() })))
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
}
