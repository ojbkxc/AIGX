// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//
//! 工具服务（「工具服务」页）：网络搜索与视觉分析的配置存储与服务清单。
//!
//! ── 这是什么 ────────────────────────────────────────────────
//! 对外提供两种可选能力，客户端可以按两种可管理协议接入：
//!   · Skill：打包成 Agent Skill，由面板一次性安装到本机各客户端的 skills 目录；
//!   · MCP：Streamable HTTP 远程服务，在客户端 MCP 配置里填地址即可。
//! 两种协议共享同一份上游配置，开关与执行超时按「服务 × 协议」各自独立。
//! 网络搜索另有一个固定的 REST 接口（`/api/web-search/*`），它是接口而不是
//! 可管理协议，运行时见 `api::tool_runtime`。
//!
//! ── 配置存在哪（AIGX 适配）──────────────────────────────────
//! 上游存在网关配置 `toolServices` 键；AIGX 的通用配置是 TOML 且不接受任意
//! 结构，因此本模块把同一份 JSON 形状落到 `FileStore` 的 `toolServices` 键
//! （与上游同名）。写入口只有 [`ToolServices::update`]，进程内加锁做串行
//! 「读改写」，两个页面同时保存不会互相覆盖。
//!
//! ── 密钥 ────────────────────────────────────────────────────
//! 四家搜索提供商的 API Key 落盘前用 `account_session::secret`（AES-256-GCM，
//! `enc1:` 信封，与账号凭证同一套密钥文件）加密；读取路径只在真正发请求时
//! 解密。管理 API 只回 `configured` 与 `key_hint`，不回明文。
//!
//! ── 账号型来源（workbuddy / workbuddy-intl / kiro）──────────
//! 与上游一致，靠提供商插件的 `admin.action`（`web-search.capabilities` /
//! `web-search.search`）执行搜索，凭证留给账号库与插件。插件宿主桥由
//! [`ToolServices::set_account_search_host`] 注入（生产实现见
//! [`PluginHostSearchBridge`]；接线点在 `main.rs`）。未注入 / 无插件时返回
//! 稳定的 `configuration` 错误（见 `account_search.rs` 顶部说明）。

mod account_search;
mod bridge;
mod config_files;
pub(crate) mod image_bridge;
pub(crate) mod image_config;
pub(crate) mod image_jobs;
pub(crate) mod image_mcp;
pub(crate) mod image_runtime;
mod install;
mod mcp;
mod normalize;
mod provider_errors;
mod provider_keys;
mod provider_request;
mod providers;
mod search;
mod skill_package;
mod vision;

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::account_session::AccountSessionStore;
use crate::api::openai::AppState;
use crate::storage::FileStore;

pub use account_search::{test_provider, AccountSearchHost, PluginHostSearchBridge};
pub use bridge::VISION_BRIDGE_SCRIPT;
pub use image_bridge::{ImageBridgeOptions, IMAGE_BRIDGE_SCRIPT};
pub use image_config::ImageConfig;
pub use install::{
    install_mcp, install_skill, mcp_targets, skill_targets, uninstall_mcp, uninstall_skill,
    InstallOutcome,
};
pub use mcp::{
    dispatch as mcp_dispatch, unsupported as mcp_unsupported, McpRequestContext, McpServerKind,
    McpSpec,
};
#[allow(unused_imports)] // 对外常量（前端/后续管理面可能用到，与上游同名）
pub use provider_keys::{ProviderKeyConfig, ProviderKeyUpdate, MAX_PROVIDER_KEYS};
pub use search::{batch_items, run_fetch, run_search, SearchError, SearchErrorCode};
pub use skill_package::{
    build_skill_files, is_skill_variant, recommended_skill_variant, skill_variants,
    WEB_SEARCH_SKILL_NAME, WEB_SEARCH_SKILL_VERSION,
};
pub use vision::{
    clear_vision_config, is_usable_key, mask_key, run_vision, save_vision_config, test_vision,
    vision_config, vision_key_options, VisionCatalogModel, VisionConfig, VISION_SERVICE_NAME,
    VISION_SERVICE_VERSION,
};

/// 配置在 `FileStore` 里的键名（与 Agent2API 的网关配置键同名）。
pub const KEY_TOOL_SERVICES: &str = "toolServices";

/// 固定服务 id（顺序即界面展示顺序）。
pub const SERVICE_IDS: [&str; 3] = [
    WEB_SEARCH_SERVICE_ID,
    VISION_SERVICE_ID,
    IMAGE_GENERATION_SERVICE_ID,
];
pub const WEB_SEARCH_SERVICE_ID: &str = "web-search";
pub const VISION_SERVICE_ID: &str = "vision";
pub const IMAGE_GENERATION_SERVICE_ID: &str = "image-generation";
pub const IMAGE_GENERATION_VERSION: &str = "1.0.0";

/// 接入协议（顺序即界面展示顺序）。
///
/// API 不在这里：它是固定的对外工具接口（`/api/web-search/*`），不参与
/// 「工具服务」页的协议开关，只在「文档」页说明；运行时仍由 [`endpoint`]
/// 与 `api::tool_runtime` 提供。
pub const PROTOCOLS: [&str; 2] = ["skill", "mcp"];

/// 密钥型来源同时支持搜索与抓取；账号型来源只支持搜索，默认不选中。
pub const API_KEY_PROVIDERS: [&str; 4] = ["tavily", "querit", "firecrawl", "ollama"];
pub const ACCOUNT_PROVIDERS: [&str; 3] = ["workbuddy", "workbuddy-intl", "kiro"];
pub const SEARCH_PROVIDERS: [&str; 7] = [
    "tavily",
    "querit",
    "firecrawl",
    "ollama",
    "workbuddy",
    "workbuddy-intl",
    "kiro",
];

/// 服务固定元数据（文案由前端按 id 映射，后端只回数据）。
pub const WEB_SEARCH_PACKAGE: &str = WEB_SEARCH_SKILL_NAME;
pub const VISION_PACKAGE: &str = VISION_SERVICE_NAME;
pub const WEB_SEARCH_VERSION: &str = WEB_SEARCH_SKILL_VERSION;
pub const VISION_VERSION: &str = VISION_SERVICE_VERSION;

pub const CAPABILITY_COUNTS: [(&str, u32); 2] =
    [(WEB_SEARCH_SERVICE_ID, 2), (VISION_SERVICE_ID, 1)];

/// 工具执行超时（秒）：同一服务各协议共用；服务端强制上限，装 MCP 时写进客户端。
pub const TIMEOUT_MIN_SECONDS: u64 = 5;
pub const TIMEOUT_MAX_SECONDS: u64 = 600;
pub const DEFAULT_TIMEOUTS: [(&str, u64); 3] = [
    (WEB_SEARCH_SERVICE_ID, 30),
    (VISION_SERVICE_ID, 60),
    (IMAGE_GENERATION_SERVICE_ID, 300),
];

/// 端点路径（写死：改动会影响已接入的客户端）。
///
/// `"api"` 不进 [`PROTOCOLS`]，但这里保留映射：REST 工具接口是固定入口，
/// 由 `tool_runtime` 直接使用，不是可管理的「服务 × 协议」。
pub fn endpoint(service: &str, protocol: &str) -> &'static str {
    match (service, protocol) {
        (WEB_SEARCH_SERVICE_ID, "skill") => "/skill/web-search",
        (WEB_SEARCH_SERVICE_ID, "mcp") => "/mcp/web-search",
        (WEB_SEARCH_SERVICE_ID, "api") => "/api/web-search",
        (VISION_SERVICE_ID, "mcp") => "/mcp/vision",
        (IMAGE_GENERATION_SERVICE_ID, "mcp") => "/mcp/image-generation",
        _ => "",
    }
}

pub fn is_service(value: &str) -> bool {
    SERVICE_IDS.contains(&value)
}

pub fn is_protocol(value: &str) -> bool {
    PROTOCOLS.contains(&value)
}

/// 该服务支持的协议（保持 PROTOCOLS 顺序）。
pub fn service_protocols(service: &str) -> Vec<&'static str> {
    PROTOCOLS
        .iter()
        .copied()
        .filter(|protocol| !endpoint(service, protocol).is_empty())
        .collect()
}

pub fn package_name(service: &str) -> &'static str {
    match service {
        WEB_SEARCH_SERVICE_ID => WEB_SEARCH_PACKAGE,
        VISION_SERVICE_ID => VISION_PACKAGE,
        IMAGE_GENERATION_SERVICE_ID => "agent2api-image-generation",
        _ => "",
    }
}

pub fn version(service: &str) -> &'static str {
    match service {
        WEB_SEARCH_SERVICE_ID => WEB_SEARCH_VERSION,
        VISION_SERVICE_ID => VISION_VERSION,
        IMAGE_GENERATION_SERVICE_ID => IMAGE_GENERATION_VERSION,
        _ => "",
    }
}

/// 工具服务运行时句柄：底层 KV 存储 + 进程内读改写锁 + 账号搜索的插件宿主桥。
pub struct ToolServices {
    /// 底层 KV 存储（工具服务配置落盘用）。
    pub store: Arc<FileStore>,
    /// 配置读改写串行锁（单实例语义：避免两个页面同时保存时互相覆盖）。
    lock: parking_lot::Mutex<()>,
    /// 账号型搜索的插件宿主桥（`main.rs` 在 AppState 就绪后注入；
    /// `None` = 未接线，运行时回稳定的 configuration 错误）。
    account_search_host: parking_lot::RwLock<Option<Arc<dyn AccountSearchHost>>>,
}

impl ToolServices {
    pub fn new(store: Arc<FileStore>) -> Self {
        Self {
            store,
            lock: parking_lot::Mutex::new(()),
            account_search_host: parking_lot::RwLock::new(None),
        }
    }

    /// 注入（或替换）账号型搜索的插件宿主桥。
    ///
    /// 生产：`main.rs` 用 `PluginHostSearchBridge::new(plugin_host, script_host)`
    /// 注入；测试：注入桩。只影响账号型来源，密钥型搜索路径不经过它。
    pub fn set_account_search_host(&self, host: Arc<dyn AccountSearchHost>) {
        *self.account_search_host.write() = Some(host);
    }

    /// 当前宿主桥快照（`None` = 未接线）。
    pub fn account_search_host(&self) -> Option<Arc<dyn AccountSearchHost>> {
        self.account_search_host.read().clone()
    }

    /// 读取整份配置（自动补齐缺省项）。
    pub fn load(&self) -> ToolConfig {
        match self.store.get::<Value>(KEY_TOOL_SERVICES) {
            Ok(Some(value)) => parse_stored(&value),
            Ok(None) => ToolConfig::default().normalized(),
            Err(error) => {
                tracing::warn!("[ToolServices] 读取配置失败，回退默认值：{error}");
                ToolConfig::default().normalized()
            }
        }
    }

    /// 在锁内做一次读改写（两个页面同时保存时后写者基于前写者的结果）。
    pub fn update(&self, mutate: impl FnOnce(&mut ToolConfig)) -> Result<ToolConfig, String> {
        self.try_update(|config| {
            mutate(config);
            Ok(())
        })
    }

    /// 同 [`ToolServices::update`]，但闭包可以返回错误中止写回（多密钥校验用）。
    pub fn try_update(
        &self,
        mutate: impl FnOnce(&mut ToolConfig) -> Result<(), String>,
    ) -> Result<ToolConfig, String> {
        let _guard = self.lock.lock();
        let mut current = self.load();
        mutate(&mut current)?;
        let current = current.normalized();
        let value = serde_json::to_value(&current)
            .map_err(|error| format!("工具服务配置序列化失败：{error}"))?;
        self.store
            .put(KEY_TOOL_SERVICES, &value)
            .map_err(|error| format!("工具服务配置写入失败：{error}"))?;
        Ok(current)
    }
}

/// 启动期初始化（校验配置可读；无残留任务，仅记录状态）。
pub fn init(store: Arc<FileStore>) {
    let services = ToolServices::new(store);
    let config = services.load();
    tracing::debug!(
        "[ToolServices] init：{} 个服务 / {} 家提供商配置",
        SERVICE_IDS.len(),
        config.providers.len()
    );
}

/// 工具服务模块自带路由（数据面路由统一在 `api::tool_runtime::router()`，
/// 安装脚本下载等管理面在 `api::admin::tools::router()`）。
pub fn router() -> axum::Router<AppState> {
    axum::Router::new()
}

/* ─── 配置形状 ─────────────────────────────────────────────── */

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ProviderConfig {
    /// 旧版单密钥字段（`enc1:` 信封）；读取后迁移进 [`ProviderConfig::keys`]。
    #[serde(skip_serializing_if = "String::is_empty")]
    pub encrypted_key: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub key_hint: String,
    /// 有序密钥列表（多密钥 + 每密钥限额状态）；详见 `provider_keys`。
    pub keys: Vec<ProviderKeyConfig>,
    /// 账号型来源只保存账号引用，凭证仍由账号库与插件管理。
    pub account_id: String,
    /// 单次最多返回条数（`None` = 用该来源的默认值；A2A `ProviderConfig.max_results`）。
    /// 仅在来源申报了能力边界（`providers::api_provider_meta` / 插件 capability）时有效。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_results: Option<usize>,
    pub enabled: bool,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Selection {
    pub search: Vec<String>,
    pub fetch: String,
}

impl Default for Selection {
    fn default() -> Self {
        Self {
            search: API_KEY_PROVIDERS
                .iter()
                .map(|item| item.to_string())
                .collect(),
            fetch: "tavily".into(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ToolConfig {
    /// service → protocol → 开关
    pub enabled: BTreeMap<String, BTreeMap<String, bool>>,
    /// service → 超时秒数
    pub timeouts: BTreeMap<String, u64>,
    /// provider → 密钥与启用状态
    pub providers: BTreeMap<String, ProviderConfig>,
    pub selection: Selection,
    /// 单次汇总总条数上限（A2A `ToolConfig.max_global_sources`）：跨 query 去重后
    /// 最多保留多少条来源。默认 200，写入侧夹在
    /// `MIN_GLOBAL_SOURCES..=MAX_GLOBAL_SOURCES_LIMIT`。
    pub max_global_sources: usize,
    /// 搜索词超长处理（A2A `ToolConfig.long_query_policy`）：`truncate` / `skip` / `error`。
    pub long_query_policy: String,
    pub vision: Option<VisionConfig>,
    #[serde(default, deserialize_with = "image_config::deserialize_stored")]
    pub image_generation: Option<ImageConfig>,
}

impl ToolConfig {
    /// 补齐缺省项（旧版本写入的配置 / 手工改过的 JSON 都走这一层）。
    fn normalized(mut self) -> Self {
        for service in SERVICE_IDS {
            let entry = self.enabled.entry(service.to_string()).or_default();
            for protocol in service_protocols(service) {
                entry
                    .entry(protocol.to_string())
                    .or_insert(service != IMAGE_GENERATION_SERVICE_ID);
            }
            let fallback = DEFAULT_TIMEOUTS
                .iter()
                .find(|(id, _)| *id == service)
                .map(|(_, value)| *value)
                .unwrap_or(30);
            self.timeouts.entry(service.to_string()).or_insert(fallback);
        }
        for provider in SEARCH_PROVIDERS {
            self.providers.entry(provider.to_string()).or_default();
        }
        // 密钥型来源：把旧版单密钥迁移进有序列表并去重。
        for provider in API_KEY_PROVIDERS {
            if let Some(entry) = self.providers.get_mut(provider) {
                provider_keys::normalize(entry);
                // 单次返回条数：手工改过的配置也不许把上游打出 400，按能力边界夹回去
                // （A2A `ToolConfig::normalized` 同口径；没有 meta 的来源原样保留）。
                if let (Some(limit), Some(meta)) =
                    (entry.max_results, providers::api_provider_meta(provider))
                {
                    entry.max_results = Some(limit.clamp(1, meta.max_results_limit));
                }
            }
        }
        self.selection
            .search
            .retain(|item| is_search_provider(item));
        if self.selection.search.is_empty() {
            self.selection.search = Selection::default().search;
        }
        if !is_fetch_provider(&self.selection.fetch) {
            self.selection.fetch = "tavily".into();
        }
        // 0（旧配置/手工清空）回落到默认值，其余夹进可配区间；策略不在值域就回默认。
        self.max_global_sources = if self.max_global_sources == 0 {
            normalize::DEFAULT_MAX_GLOBAL_SOURCES
        } else {
            self.max_global_sources.clamp(
                normalize::MIN_GLOBAL_SOURCES,
                normalize::MAX_GLOBAL_SOURCES_LIMIT,
            )
        };
        if !normalize::LONG_QUERY_POLICIES.contains(&self.long_query_policy.as_str()) {
            self.long_query_policy = normalize::DEFAULT_LONG_QUERY_POLICY.to_string();
        }
        self
    }
}

fn parse_stored(value: &Value) -> ToolConfig {
    if value.is_null() {
        return ToolConfig::default().normalized();
    }
    serde_json::from_value::<ToolConfig>(value.clone())
        .unwrap_or_default()
        .normalized()
}

/* ─── 开关与超时 ───────────────────────────────────────────── */

pub fn is_enabled(services: &ToolServices, service: &str, protocol: &str) -> bool {
    if endpoint(service, protocol).is_empty() {
        return false;
    }
    // API 是固定的对外工具接口，不再是「工具服务」页里可开关的协议；
    // 保留 endpoint 与运行时路由，旧配置里的 api 开关一律不再生效。
    if protocol == "api" {
        return true;
    }
    services
        .load()
        .enabled
        .get(service)
        .and_then(|entry| entry.get(protocol))
        .copied()
        .unwrap_or(service != IMAGE_GENERATION_SERVICE_ID)
}

pub fn set_enabled(
    services: &ToolServices,
    service: &str,
    protocol: &str,
    enabled: bool,
) -> Result<Value, String> {
    if endpoint(service, protocol).is_empty() {
        return Err(format!("服务 {service} 不支持 {protocol} 协议"));
    }
    services.update(|config_value| {
        config_value
            .enabled
            .entry(service.to_string())
            .or_default()
            .insert(protocol.to_string(), enabled);
    })?;
    Ok(service_info(services, service, protocol))
}

pub fn timeout_seconds(services: &ToolServices, service: &str) -> u64 {
    let value = services.load().timeouts.get(service).copied().unwrap_or(30);
    value.clamp(TIMEOUT_MIN_SECONDS, TIMEOUT_MAX_SECONDS)
}

pub fn set_timeout(services: &ToolServices, service: &str, seconds: u64) -> Result<Value, String> {
    if !is_service(service) {
        return Err("未知的工具服务".into());
    }
    if !(TIMEOUT_MIN_SECONDS..=TIMEOUT_MAX_SECONDS).contains(&seconds) {
        return Err(format!(
            "超时必须是 {TIMEOUT_MIN_SECONDS}~{TIMEOUT_MAX_SECONDS} 秒之间的整数"
        ));
    }
    services.update(|config_value| {
        config_value.timeouts.insert(service.to_string(), seconds);
    })?;
    Ok(service_info(
        services,
        service,
        service_protocols(service)[0],
    ))
}

/* ─── 服务清单 ─────────────────────────────────────────────── */

pub fn capability_count(services: &ToolServices, service: &str) -> u32 {
    if service == IMAGE_GENERATION_SERVICE_ID {
        return if services
            .load()
            .image_generation
            .as_ref()
            .is_some_and(|config| config.editing.is_some())
        {
            3
        } else {
            2
        };
    }
    CAPABILITY_COUNTS
        .iter()
        .find(|(id, _)| *id == service)
        .map(|(_, count)| *count)
        .unwrap_or(0)
}

pub fn service_info(services: &ToolServices, service: &str, protocol: &str) -> Value {
    let mut info = json!({
        "id": service,
        "protocol": protocol,
        "key": format!("{service}:{protocol}"),
        "enabled": is_enabled(services, service, protocol),
        "capability_count": capability_count(services, service),
        "endpoint": endpoint(service, protocol),
        "package_name": package_name(service),
        "version": version(service),
        "timeout_seconds": timeout_seconds(services, service),
    });
    if service == WEB_SEARCH_SERVICE_ID && protocol == "skill" {
        info["recommended_variant"] = json!(recommended_skill_variant());
        info["variants"] = serde_json::to_value(skill_variants()).unwrap_or(Value::Null);
    }
    info
}

pub fn list_services(services: &ToolServices, protocol: Option<&str>) -> Vec<Value> {
    let mut rows = Vec::new();
    for service in SERVICE_IDS {
        for supported in service_protocols(service) {
            if protocol.is_none() || protocol == Some(supported) {
                rows.push(service_info(services, service, supported));
            }
        }
    }
    rows
}

/* ─── 搜索提供商 ───────────────────────────────────────────── */

pub fn is_search_provider(value: &str) -> bool {
    SEARCH_PROVIDERS.contains(&value)
}

pub fn is_account_provider(value: &str) -> bool {
    ACCOUNT_PROVIDERS.contains(&value)
}

pub fn is_fetch_provider(value: &str) -> bool {
    API_KEY_PROVIDERS.contains(&value)
}

pub fn provider_display_name(provider: &str) -> &'static str {
    match provider {
        "tavily" => "Tavily",
        "querit" => "Querit",
        "firecrawl" => "Firecrawl",
        "ollama" => "Ollama Web Search",
        "workbuddy" => "WorkBuddy 国内版",
        "workbuddy-intl" => "WorkBuddy 国际版",
        "kiro" => "Kiro",
        _ => "",
    }
}

pub fn key_hint(value: &str) -> String {
    if value.chars().count() <= 12 {
        return format!("{}••••", value.chars().take(4).collect::<String>());
    }
    let head: String = value.chars().take(7).collect();
    let tail: String = value
        .chars()
        .skip(value.chars().count().saturating_sub(4))
        .collect();
    format!("{head}••••{tail}")
}

pub async fn list_providers(services: &ToolServices, sessions: &AccountSessionStore) -> Value {
    let config_value = services.load();
    let mut rows: Vec<Value> = API_KEY_PROVIDERS
        .iter()
        .map(|provider| {
            let entry = config_value
                .providers
                .get(*provider)
                .cloned()
                .unwrap_or_default();
            provider_keys::provider_row(provider, &entry)
        })
        .collect();
    for provider in ACCOUNT_PROVIDERS {
        let entry = config_value
            .providers
            .get(provider)
            .cloned()
            .unwrap_or_default();
        rows.push(account_search::provider_row(services, sessions, provider, entry).await);
    }
    Value::Array(rows)
}

pub fn provider_selection(services: &ToolServices) -> Value {
    let config_value = services.load();
    json!({
        "search_providers": config_value.selection.search,
        "fetch_provider": config_value.selection.fetch,
        // A2A v3.5.1 provider_selection 的四键形状：两个选择 + 两个搜索行为参数
        "max_global_sources": config_value.max_global_sources,
        "long_query_policy": config_value.long_query_policy,
    })
}

/// 校验「单次最多返回条数」（A2A `validate_provider_max_results` 同语义）。
///
/// 三态：外层 `None` = 不修改；内层 `None` = 清空并回落到来源默认值；
/// `Some(n)` = 设定，必须落在该来源的能力边界内。
///
/// **与 A2A 的已知差异（如实记录）**：A2A 对账号型来源会去插件申报的
/// `web_search_capability().max_results_limit` 校验，那是一次 `async` 的插件
/// 调用；本函数保持 `sync`（`update_provider` 也是 sync，改成 async 会波及调用方
/// 与既有测试），因此账号型来源回落到 A2A 的最后一条分支——通用上限
/// [`normalize::MAX_QUERY_RESULT_LIMIT`]。这是**更宽松**的校验：插件边界更小时
/// 这里不会拦，运行期 `account_search` 仍会按插件的 `max_results_limit` 夹一次。
fn validate_provider_max_results(
    provider: &str,
    max_results: Option<Option<usize>>,
) -> Result<Option<Option<usize>>, String> {
    let Some(next) = max_results else {
        return Ok(None);
    };
    let Some(value) = next else {
        return Ok(Some(None));
    };
    if let Some(meta) = providers::api_provider_meta(provider) {
        if !(1..=meta.max_results_limit).contains(&value) {
            return Err(format!(
                "{} 单次最多返回条数需在 1～{} 之间",
                provider_display_name(provider),
                meta.max_results_limit
            ));
        }
        return Ok(Some(Some(value)));
    }
    if !(1..=normalize::MAX_QUERY_RESULT_LIMIT).contains(&value) {
        return Err(format!(
            "单次最多返回条数需在 1～{} 之间",
            normalize::MAX_QUERY_RESULT_LIMIT
        ));
    }
    Ok(Some(Some(value)))
}

#[allow(clippy::too_many_arguments)] // v3.5.1 对齐加 max_results 三态后为 8 参；同上，只保留语义
pub fn update_provider(
    services: &ToolServices,
    sessions: &AccountSessionStore,
    provider: &str,
    api_key: Option<&str>,
    enabled: Option<bool>,
    account_id: Option<&str>,
    keys: Option<Vec<ProviderKeyUpdate>>,
    max_results: Option<Option<usize>>,
) -> Result<Value, String> {
    if !is_search_provider(provider) {
        return Err("未知的搜索提供商".into());
    }
    // 单次返回条数：外层 None = 不改；内层 None = 清空回默认；Some(n) = 设定值。
    let validated_max_results = validate_provider_max_results(provider, max_results)?;
    if is_account_provider(provider) {
        if keys.is_some() || api_key.is_some_and(|key| !key.trim().is_empty()) {
            return Err("该搜索来源使用已有账号，不接受 API Key".into());
        }
        let account_id = account_id.map(str::trim);
        if let Some(id) = account_id.filter(|id| !id.is_empty()) {
            account_search::validate_account(sessions, provider, id)
                .map_err(|error| error.message)?;
        }
        let config_value = services.try_update(|config_value| {
            let entry = config_value
                .providers
                .entry(provider.to_string())
                .or_default();
            if let Some(id) = account_id {
                entry.account_id = id.to_string();
            }
            if let Some(enabled) = enabled {
                entry.enabled = enabled;
            }
            if let Some(next) = validated_max_results {
                entry.max_results = next;
            }
            entry.updated_at = now_ms();
            Ok(())
        })?;
        let entry = config_value
            .providers
            .get(provider)
            .cloned()
            .unwrap_or_default();
        return Ok(json!({
            "success": true,
            "provider": provider,
            "account_id": entry.account_id,
            "max_results": entry.max_results,
        }));
    }
    if account_id.is_some() {
        return Err("该搜索来源使用 API Key，不接受账号绑定".into());
    }
    if keys.is_some() && api_key.is_some() {
        return Err("不能同时提交单密钥和密钥列表".into());
    }
    let prepared = keys
        .as_deref()
        .map(provider_keys::prepare_updates)
        .transpose()?;
    let replacement = api_key
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(provider_keys::prepare_legacy_key)
        .transpose()?;
    let config_value = services.try_update(|config_value| {
        let entry = config_value
            .providers
            .entry(provider.to_string())
            .or_default();
        if let Some(items) = prepared {
            provider_keys::apply_updates(entry, items)?;
        }
        if let Some(key) = replacement {
            provider_keys::replace_legacy_key(entry, key);
        }
        if let Some(enabled) = enabled {
            entry.enabled = enabled;
        }
        if let Some(next) = validated_max_results {
            entry.max_results = next;
        }
        Ok(())
    })?;
    let entry = config_value
        .providers
        .get(provider)
        .cloned()
        .unwrap_or_default();
    let mut result = provider_keys::provider_row(provider, &entry);
    result["success"] = json!(true);
    Ok(result)
}

pub fn clear_provider_key(services: &ToolServices, provider: &str) -> bool {
    if !API_KEY_PROVIDERS.contains(&provider) {
        return false;
    }
    services
        .update(|config_value| {
            let entry = config_value
                .providers
                .entry(provider.to_string())
                .or_default();
            entry.keys.clear();
            entry.encrypted_key.clear();
            entry.key_hint.clear();
            entry.updated_at = now_ms();
        })
        .is_ok()
}

#[allow(dead_code)] // 预留/线表符号（见 main.rs 债务清单）
pub fn configured_provider(services: &ToolServices, provider: &str) -> bool {
    services
        .load()
        .providers
        .get(provider)
        .is_some_and(|entry| provider_keys::has_keys(entry) && entry.enabled)
}

/// 已启用且已配置好的提供商（密钥型看密钥，账号型看绑定账号是否存在）。
pub fn ready_providers(
    services: &ToolServices,
    sessions: &AccountSessionStore,
) -> Vec<&'static str> {
    let config_value = services.load();
    SEARCH_PROVIDERS
        .iter()
        .copied()
        .filter(|provider| {
            let Some(entry) = config_value.providers.get(*provider) else {
                return false;
            };
            if !entry.enabled {
                return false;
            }
            if is_account_provider(provider) {
                account_search::validate_account(sessions, provider, &entry.account_id).is_ok()
            } else {
                provider_keys::has_keys(entry) && provider_keys::usable(entry, "search")
            }
        })
        .collect()
}

/// 已选择但未启用或未配置的提供商（展示名，供运行时返回可读配置错误）。
#[allow(dead_code)] // 预留/线表符号（见 main.rs 债务清单）
pub fn missing_selected_providers(
    services: &ToolServices,
    sessions: &AccountSessionStore,
) -> Vec<&'static str> {
    let selected = services.load().selection.search;
    let ready = ready_providers(services, sessions);
    selected
        .iter()
        .filter(|provider| !ready.contains(&provider.as_str()))
        .map(|provider| provider_display_name(provider))
        .filter(|name| !name.is_empty())
        .collect()
}

pub fn update_selection(
    services: &ToolServices,
    search: Option<Vec<String>>,
    fetch: Option<String>,
    max_global_sources: Option<usize>,
    long_query_policy: Option<String>,
) -> Result<Value, String> {
    let mut next_search = None;
    if let Some(items) = search {
        if items.is_empty() {
            return Err("批量网页搜索至少选择一个提供商".into());
        }
        if items.iter().any(|item| !is_search_provider(item)) {
            return Err("搜索提供商包含未知项".into());
        }
        let mut seen = Vec::new();
        for item in items {
            if !seen.contains(&item) {
                seen.push(item);
            }
        }
        next_search = Some(seen);
    }
    if let Some(provider) = fetch.as_deref() {
        if !is_fetch_provider(provider) {
            return Err("该提供商不支持网页抓取".into());
        }
    }
    // A2A v3.5.1 同值域：条数 20~1000（不夹取，越界直接 400），策略三选一。
    if let Some(max_sources) = max_global_sources {
        if !(normalize::MIN_GLOBAL_SOURCES..=normalize::MAX_GLOBAL_SOURCES_LIMIT)
            .contains(&max_sources)
        {
            return Err(format!(
                "单次汇总总条数上限需在 {}～{} 之间",
                normalize::MIN_GLOBAL_SOURCES,
                normalize::MAX_GLOBAL_SOURCES_LIMIT
            ));
        }
    }
    if let Some(policy) = long_query_policy.as_deref() {
        if !normalize::LONG_QUERY_POLICIES.contains(&policy) {
            return Err("搜索词超长处理方式无效".into());
        }
    }
    services.update(|config_value| {
        if let Some(items) = next_search {
            config_value.selection.search = items;
        }
        if let Some(provider) = fetch {
            config_value.selection.fetch = provider;
        }
        if let Some(max_sources) = max_global_sources {
            config_value.max_global_sources = max_sources;
        }
        if let Some(policy) = long_query_policy {
            config_value.long_query_policy = policy;
        }
    })?;
    Ok(provider_selection(services))
}

/// 毫秒时间戳（与账号/维护模块同口径）。
pub(crate) fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn temp_services() -> (ToolServices, tempfile::TempDir) {
        let dir = tempfile::TempDir::new().unwrap();
        let store = Arc::new(FileStore::new(dir.path().to_path_buf()));
        (ToolServices::new(store), dir)
    }

    #[test]
    fn config_defaults_and_normalization() {
        let (services, _dir) = temp_services();
        let config = services.load();
        // 默认选中四家密钥型；web-search skill/mcp 默认开、image-generation 默认关
        assert_eq!(config.selection.search.len(), 4);
        assert_eq!(config.selection.fetch, "tavily");
        assert!(config.enabled["web-search"]["skill"]);
        assert!(config.enabled["web-search"]["mcp"]);
        assert!(!config.enabled["image-generation"]["mcp"]);
        assert_eq!(config.timeouts["web-search"], 30);
        assert_eq!(config.providers.len(), 7);
    }

    #[test]
    fn update_is_normalized_and_persisted() {
        let (services, _dir) = temp_services();
        services
            .update(|config| {
                config
                    .enabled
                    .entry("vision".into())
                    .or_default()
                    .insert("mcp".into(), false);
                config.selection.search = vec!["unknown".into()];
            })
            .unwrap();
        let config = services.load();
        assert!(!config.enabled["vision"]["mcp"]);
        // 未知项被过滤后为空 → 回落到默认集合
        assert_eq!(config.selection.search.len(), 4);
    }

    #[test]
    fn service_protocol_matrix() {
        assert_eq!(service_protocols("web-search"), vec!["skill", "mcp"]);
        assert_eq!(service_protocols("vision"), vec!["mcp"]);
        assert_eq!(service_protocols("image-generation"), vec!["mcp"]);
        assert_eq!(endpoint("web-search", "api"), "/api/web-search");
        assert_eq!(endpoint("vision", "skill"), "");
        assert!(!is_service("nope"));
    }

    #[test]
    fn key_hint_shape() {
        assert_eq!(key_hint("shortkey"), "shor••••");
        assert_eq!(key_hint("tvly-abcdefghijklmnop"), "tvly-ab••••mnop");
    }
}
