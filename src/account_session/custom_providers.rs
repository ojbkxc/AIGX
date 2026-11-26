//! 自定义提供商（用户自建上游端点）：存储 + 校验 + 目录拉取。
//!
//! ## 与内置注册表的区别
//!
//! 内置家是**编译期**事实（`registry::PROVIDERS` 里的字面量）；自定义家是
//! **运行期数据**：用户填名称 / 协议 / 基址，id 由本模块生成（`custom-` +
//! 12 位小写十六进制）。它不进 `ProviderKind`，也不进 `PROVIDERS` 注册表 ——
//! 注册表「不认识就是不认识」的判据必须保持。
//!
//! ## 存储
//!
//! 复用既有 `FileStore` 的 kv 前缀（**不动 `src/storage/**`**）：每条记录一个
//! 键 `custom_provider:{id}`，值是 camelCase JSON（与上游 `core/custom_providers.rs`
//! 的对外契约同形，见模块末尾的字段表）。启动时由 `install(store)` 装入并
//! 全量载入内存（`RwLock<Vec<Value>>`），此后读路径零磁盘 IO —— 转发热路径
//! 会多次问「这个 id 是不是自定义家 / 它的协议是什么」。
//!
//! ## 记录形状（对外契约，前后端一起改）
//!
//! ```json
//! {
//!   "id": "custom-3f2a91b04c7e",
//!   "name": "我的网关",
//!   "protocol": "chat_completions",   // chat_completions / responses / anthropic
//!   "baseUrl": "https://api.example.com/v1",
//!   "enabled": true,
//!   "createdAt": 1730000000000,
//!   "models":   [{ "id": "gpt-x", "enabled": true, "reasoning": "" }],
//!   "mappings": [{ "alias": "my-alias", "target": "gpt-x", "enabled": true, "reasoning": "" }],
//!   "urlSuffix": "?beta=true",        // 可选：追加到出站 URL
//!   "headers":   { "Anthropic-Beta": "…" }, // 可选：静态额外头
//!   "anthropicToolType": "custom"     // 可选：发 anthropic 时给工具补 type
//! }
//! ```
//!
//! ## 三条约束
//!   1. id 用随机源（卸载重装/并发添加不撞号），`custom-` 前缀是「自定义家」
//!      的判据之一（另一半是「存储里确实有这条」）；
//!   2. `baseUrl` 保存即规范化（去尾斜杠）—— 转发时原样拼路径，留着会拼出 `//`；
//!   3. **apiKey 不进本记录**：它是账号属性（同一家可多账号），落在
//!      `AccountSession.credential.secret`。

// 本模块是对外提供的「运行期配置」API：部分 CRUD / 目录拉取函数在 API 路由
// 接线前暂无调用方（接线清单见交付报告）。统一允许 dead_code，避免 CI 的
// `-D warnings` 因「尚未接线」而失败；接线完成后可移除本行。
#![allow(dead_code)]

use std::sync::{Arc, OnceLock, RwLock};

use serde_json::{json, Map, Value};

use crate::account_session::store::AccountSessionStore;
use crate::storage::FileStore;

/// kv 前缀（每条记录一个键：`custom_provider:{id}`）。
const KEY_PREFIX: &str = "custom_provider:";

/// 自定义 provider id 前缀（`is_custom_provider_id` 的第一条判据）。
pub const ID_PREFIX: &str = "custom-";

/// OpenAI Chat Completions 协议。
pub const PROTOCOL_CHAT_COMPLETIONS: &str = "chat_completions";
/// OpenAI Responses 协议。
pub const PROTOCOL_RESPONSES: &str = "responses";
/// Anthropic Messages 协议。
pub const PROTOCOL_ANTHROPIC: &str = "anthropic";

/// 全部合法协议（顺序即错误文案顺序）。
pub const PROTOCOLS: &[&str] = &[
    PROTOCOL_CHAT_COMPLETIONS,
    PROTOCOL_RESPONSES,
    PROTOCOL_ANTHROPIC,
];

const MAX_NAME_CHARS: usize = 64;
const MAX_MODEL_ID_CHARS: usize = 128;
const MAX_ALIAS_CHARS: usize = 128;
const MAX_REASONING_CHARS: usize = 32;
const MAX_URL_SUFFIX_CHARS: usize = 128;
const MAX_HEADER_ENTRIES: usize = 16;
const MAX_HEADER_NAME_CHARS: usize = 64;
const MAX_HEADER_VALUE_CHARS: usize = 512;

/// 工具 type 补齐的唯一合法值（MiniMax 等 Claude 兼容端点要求）。
pub const TOOL_TYPE_CUSTOM: &str = "custom";

/// 进程级库句柄 + 内存快照（首次 `install` 时装入，重复调用忽略）。
static STORE: OnceLock<Arc<FileStore>> = OnceLock::new();
static INSTANCE: OnceLock<Arc<CustomProviders>> = OnceLock::new();

/// 启动时装入库句柄并加载全部记录。
///
/// **待接线**：`src/main.rs` 里紧邻其它 `install(store.clone())` 之后加一行
/// `crate::account_session::custom_providers::install(store.clone());`。
/// 未安装时所有读路径返回「没有自定义家」，写路径明确报错（功能不静默半开）。
pub fn install(store: Arc<FileStore>) {
    if STORE.set(store).is_err() {
        return;
    }
    let Some(store) = STORE.get().cloned() else {
        return;
    };
    let instance = Arc::new(CustomProviders::new(store));
    if let Err(error) = instance.load() {
        tracing::warn!("custom providers load failed: {error}");
    }
    let _ = INSTANCE.set(instance);
}

/// 当前单例（未安装返回 None）。
pub fn current() -> Option<&'static Arc<CustomProviders>> {
    INSTANCE.get()
}

/// 供测试/单机内嵌使用：直接构造一个独立实例（不抢占全局槽）。
pub struct CustomProviders {
    items: RwLock<Vec<Value>>,
    store: Arc<FileStore>,
}

impl CustomProviders {
    pub fn new(store: Arc<FileStore>) -> Self {
        Self {
            items: RwLock::new(Vec::new()),
            store,
        }
    }

    /// 从库全量载入（坏条目丢弃，按 createdAt 升序）。
    pub fn load(&self) -> anyhow::Result<()> {
        let mut items = Vec::new();
        for key in self.store.list(KEY_PREFIX)? {
            if let Some(value) = self.store.get::<Value>(&key)? {
                if let Some(item) = normalize_record(&value) {
                    items.push(item);
                }
            }
        }
        items.sort_by_key(created_at);
        *self.items.write().unwrap_or_else(|e| e.into_inner()) = items;
        Ok(())
    }

    fn persist_one(&self, item: &Value) -> anyhow::Result<()> {
        let id = item.get("id").and_then(Value::as_str).unwrap_or("");
        self.store.put(&format!("{KEY_PREFIX}{id}"), item)
    }

    fn snapshot(&self) -> Vec<Value> {
        self.items.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn list(&self) -> Vec<Value> {
        self.snapshot()
    }

    pub fn get(&self, id: &str) -> Option<Value> {
        let id = id.trim();
        if id.is_empty() {
            return None;
        }
        self.items
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|item| item.get("id").and_then(Value::as_str) == Some(id))
            .cloned()
    }

    /// 新建：`{name, protocol, baseUrl, ...}` → 落盘后的完整条目。
    pub fn create(&self, payload: &Value) -> Result<Value, String> {
        let object = payload
            .as_object()
            .ok_or_else(|| "请求体必须是 JSON 对象".to_string())?;
        let id = new_provider_id()?;
        let item = json!({
            "id": id,
            "name": validate_name(object.get("name"))?,
            "protocol": validate_protocol(object.get("protocol"))?,
            "baseUrl": validate_base_url(object.get("baseUrl").and_then(Value::as_str).unwrap_or(""))?,
            "enabled": object.get("enabled").and_then(Value::as_bool).unwrap_or(true),
            "createdAt": chrono::Utc::now().timestamp_millis(),
            "models": [],
            "mappings": [],
            "urlSuffix": validate_url_suffix(object.get("urlSuffix"))?,
            "headers": validate_headers(object.get("headers"))?,
            "anthropicToolType": validate_tool_type(object.get("anthropicToolType"))?,
        });
        self.persist_one(&item)
            .map_err(|e| format!("保存失败：{e}"))?;
        let mut items = self.items.write().unwrap_or_else(|e| e.into_inner());
        items.push(item.clone());
        items.sort_by_key(created_at);
        Ok(item)
    }

    /// 更新：只改请求里带了的字段（patch 语义；非法值报错，不静默忽略）。
    pub fn update(&self, id: &str, payload: &Value) -> Result<Value, String> {
        let id = id.trim();
        if id.is_empty() {
            return Err("缺少提供商 id".to_string());
        }
        let object = payload
            .as_object()
            .ok_or_else(|| "请求体必须是 JSON 对象".to_string())?;
        let mut items = self.items.write().unwrap_or_else(|e| e.into_inner());
        let index = items
            .iter()
            .position(|item| item.get("id").and_then(Value::as_str) == Some(id))
            .ok_or_else(|| format!("自定义提供商不存在: {id}"))?;
        let mut merged = items[index].as_object().cloned().unwrap_or_default();
        if let Some(value) = object.get("name") {
            merged.insert(
                "name".to_string(),
                Value::String(validate_name(Some(value))?),
            );
        }
        if let Some(value) = object.get("protocol") {
            merged.insert(
                "protocol".to_string(),
                Value::String(validate_protocol(Some(value))?),
            );
        }
        if let Some(value) = object.get("baseUrl") {
            merged.insert(
                "baseUrl".to_string(),
                Value::String(validate_base_url(value.as_str().unwrap_or(""))?),
            );
        }
        if let Some(value) = object.get("enabled") {
            let enabled = value
                .as_bool()
                .ok_or_else(|| "enabled 必须是布尔值".to_string())?;
            merged.insert("enabled".to_string(), Value::Bool(enabled));
        }
        if object.contains_key("urlSuffix") {
            merged.insert(
                "urlSuffix".to_string(),
                Value::String(validate_url_suffix(object.get("urlSuffix"))?),
            );
        }
        if object.contains_key("headers") {
            merged.insert(
                "headers".to_string(),
                Value::Object(validate_headers(object.get("headers"))?),
            );
        }
        if object.contains_key("anthropicToolType") {
            merged.insert(
                "anthropicToolType".to_string(),
                Value::String(validate_tool_type(object.get("anthropicToolType"))?),
            );
        }
        let updated = Value::Object(merged);
        self.persist_one(&updated)
            .map_err(|e| format!("保存失败：{e}"))?;
        items[index] = updated.clone();
        Ok(updated)
    }

    /// 删除提供商记录（**不级联删账号**，见 [`remove_with_accounts`]）。
    pub fn remove(&self, id: &str) -> Result<(), String> {
        let id = id.trim();
        if id.is_empty() {
            return Err("缺少提供商 id".to_string());
        }
        let mut items = self.items.write().unwrap_or_else(|e| e.into_inner());
        let before = items.len();
        items.retain(|item| item.get("id").and_then(Value::as_str) != Some(id));
        if items.len() == before {
            return Err(format!("自定义提供商不存在: {id}"));
        }
        self.store
            .delete(&format!("{KEY_PREFIX}{id}"))
            .map_err(|e| format!("删除失败：{e}"))?;
        Ok(())
    }

    /// 删除提供商并级联删除该家名下账号（先删账号，再删记录，见上游顺序约束）。
    pub fn remove_with_accounts(
        &self,
        id: &str,
        sessions: &AccountSessionStore,
    ) -> Result<usize, String> {
        let id = id.trim();
        if id.is_empty() {
            return Err("缺少提供商 id".to_string());
        }
        if self.get(id).is_none() {
            return Err(format!("自定义提供商不存在: {id}"));
        }
        let orphans: Vec<String> = sessions
            .all()
            .into_iter()
            .filter(|session| session.provider == id)
            .map(|session| session.id)
            .collect();
        for account_id in &orphans {
            sessions
                .remove(account_id)
                .map_err(|e| format!("删除账号失败：{e}"))?;
        }
        self.remove(id)?;
        Ok(orphans.len())
    }

    /// 整体替换 `models` 与 `mappings`（UI 弹窗整表保存）。
    pub fn set_models(&self, id: &str, models: &Value, mappings: &Value) -> Result<Value, String> {
        let id = id.trim();
        let models = models
            .as_array()
            .ok_or_else(|| "models 必须是数组".to_string())?;
        let mappings = mappings
            .as_array()
            .ok_or_else(|| "mappings 必须是数组".to_string())?;
        let models = validate_model_entries(models)?;
        let mappings = validate_mapping_entries(mappings)?;
        let mut items = self.items.write().unwrap_or_else(|e| e.into_inner());
        let index = items
            .iter()
            .position(|item| item.get("id").and_then(Value::as_str) == Some(id))
            .ok_or_else(|| format!("自定义提供商不存在: {id}"))?;
        let mut map = items[index].as_object().cloned().unwrap_or_default();
        map.insert("models".to_string(), Value::Array(models));
        map.insert("mappings".to_string(), Value::Array(mappings));
        let updated = Value::Object(map.clone());
        self.persist_one(&updated)
            .map_err(|e| format!("保存失败：{e}"))?;
        items[index] = updated.clone();
        Ok(updated)
    }
}

fn created_at(item: &Value) -> i64 {
    item.get("createdAt").and_then(Value::as_i64).unwrap_or(0)
}

// ── 对外自由函数（委托全局单例；未安装时安全返回）───────────────

/// 全部自定义家（按 createdAt 升序）。
pub fn list() -> Vec<Value> {
    current()
        .map(|providers| providers.list())
        .unwrap_or_default()
}

/// 按 id 取一条。
pub fn get(id: &str) -> Option<Value> {
    current()?.get(id)
}

/// 这个 id 是不是**已注册的自定义提供商**（前缀 + 存储存在，两条都要）。
pub fn is_custom_provider_id(id: &str) -> bool {
    let id = id.trim();
    id.starts_with(ID_PREFIX) && get(id).is_some()
}

/// 展示名（不存在返回 None；调用方决定回退）。
pub fn label_of(id: &str) -> Option<String> {
    get(id).and_then(|item| item.get("name").and_then(Value::as_str).map(str::to_string))
}

/// 协议（不存在返回 None）。
pub fn protocol_of(id: &str) -> Option<String> {
    get(id).and_then(|item| {
        item.get("protocol")
            .and_then(Value::as_str)
            .map(str::to_string)
    })
}

/// 该家是否启用（不存在返回 false）。
pub fn is_enabled(id: &str) -> bool {
    get(id)
        .and_then(|item| item.get("enabled").and_then(Value::as_bool))
        .unwrap_or(false)
}

/// 解析「请求名 → 上游真名 + 思考等级绑定」。
///
/// 优先级：enabled 且 `alias == 请求名` 的映射 → 该映射的 target/reasoning；
/// 否则 enabled 且同 id 的模型 → 该模型的精确 id/reasoning；都不命中原样透传。
pub fn wire_model_for(provider_id: &str, requested_name: &str) -> (String, Option<String>) {
    let requested = requested_name.trim();
    let Some(provider) = get(provider_id) else {
        return (requested.to_string(), None);
    };
    let enabled = |item: &Value| item.get("enabled").and_then(Value::as_bool).unwrap_or(true);
    if let Some(mappings) = provider.get("mappings").and_then(Value::as_array) {
        for mapping in mappings.iter().filter(|m| enabled(m)) {
            if mapping
                .get("alias")
                .and_then(Value::as_str)
                .is_some_and(|alias| alias.eq_ignore_ascii_case(requested))
            {
                let target = mapping
                    .get("target")
                    .and_then(Value::as_str)
                    .unwrap_or(requested)
                    .to_string();
                return (target, reasoning_of(mapping));
            }
        }
    }
    if let Some(models) = provider.get("models").and_then(Value::as_array) {
        for model in models.iter().filter(|m| enabled(m)) {
            if model
                .get("id")
                .and_then(Value::as_str)
                .is_some_and(|id| id.eq_ignore_ascii_case(requested))
            {
                let id = model
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or(requested)
                    .to_string();
                return (id, reasoning_of(model));
            }
        }
    }
    (requested.to_string(), None)
}

fn reasoning_of(item: &Value) -> Option<String> {
    item.get("reasoning")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

// ── 数据面构造（由 `bridge/account.rs` 拦截时调用）────────────────

/// 为自定义 provider 账号构造 Bridge。
///
/// - 非 `custom-*` / 不在存储 → `None`（调用方回退既有适配层）；
/// - `custom-*` → `Some(Ok(bridge))` / `Some(Err(..))`。
///
/// 协议分派：
/// - `chat_completions` → 复用 `bridge::openai::make_bridge`（基址 = 账号覆盖 > 记录 baseUrl）；
/// - `anthropic` → 复用 `bridge::anthropic::make_bridge`；
/// - `responses` → [`CustomResponsesBridge`]（Chat⇄Responses 转换，见
///   `subscriptions::responses`）。
///
/// `urlSuffix` / `headers` 当前只由 `responses` 分支消费；`chat_completions` 与
/// `anthropic` 复用既有 Bridge，暂不带这两项（见交付报告待接线）。
pub fn build_bridge(
    session: &crate::account_session::model::AccountSession,
    sessions: &AccountSessionStore,
) -> Option<Result<Arc<dyn crate::bridge::Bridge>, crate::bridge::BridgeError>> {
    use crate::bridge::BridgeError;
    if !is_custom_provider_id(&session.provider) {
        return None;
    }
    Some((|| {
        let provider = get(&session.provider)
            .ok_or_else(|| BridgeError::Config("自定义提供商不存在".to_string()))?;
        if !provider
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(true)
        {
            return Err(BridgeError::Config("该自定义提供商已停用".to_string()));
        }
        let protocol = provider
            .get("protocol")
            .and_then(Value::as_str)
            .unwrap_or(PROTOCOL_CHAT_COMPLETIONS);
        let base_url = if session.base_url.trim().is_empty() {
            provider
                .get("baseUrl")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        } else {
            session.base_url.trim().to_string()
        };
        if base_url.is_empty() {
            return Err(BridgeError::Config(
                "该自定义提供商没有配置 baseUrl".to_string(),
            ));
        }
        let key = session.credential.secret_plain();
        let client = sessions.client_for(session)?;
        let bridge: Arc<dyn crate::bridge::Bridge> = match protocol {
            PROTOCOL_ANTHROPIC => crate::bridge::anthropic::make_bridge(&base_url, &key, &client),
            PROTOCOL_RESPONSES => {
                Arc::new(CustomResponsesBridge::new(base_url, key, client, &provider))
            }
            _ => crate::bridge::openai::make_bridge(&base_url, &key, &client),
        };
        Ok(bridge)
    })())
}

/// 自定义家的 `responses` 协议 Bridge：Chat → Responses → 上游 → Chat。
///
/// 与订阅型（`subscriptions::backend`）的区别：静态 API Key、无令牌续期、
/// 端点 = `{baseUrl}/responses`，并支持记录的 `urlSuffix` 与静态 `headers`。
pub struct CustomResponsesBridge {
    url: String,
    key: String,
    headers: Vec<(String, String)>,
    client: Arc<reqwest::Client>,
}

impl CustomResponsesBridge {
    fn new(base_url: String, key: String, client: Arc<reqwest::Client>, provider: &Value) -> Self {
        let suffix = provider
            .get("urlSuffix")
            .and_then(Value::as_str)
            .unwrap_or("");
        let url = format!("{}/responses{}", base_url.trim_end_matches('/'), suffix);
        let mut headers = Vec::new();
        if let Some(map) = provider.get("headers").and_then(Value::as_object) {
            for (name, value) in map {
                if let Some(value) = value.as_str() {
                    headers.push((name.clone(), value.to_string()));
                }
            }
        }
        Self {
            url,
            key,
            headers,
            client,
        }
    }

    fn authorize(
        &self,
        mut request: reqwest::RequestBuilder,
        accept: &str,
    ) -> reqwest::RequestBuilder {
        if !self.key.trim().is_empty() {
            request = request.header("Authorization", format!("Bearer {}", self.key));
        }
        request = request.header("Accept", accept);
        for (name, value) in &self.headers {
            request = request.header(name.as_str(), value.as_str());
        }
        request
    }
}

fn transport_error(error: reqwest::Error) -> crate::bridge::BridgeError {
    crate::bridge::BridgeError::Transport(error.to_string())
}

fn status_error(status: u16, body: &str) -> crate::bridge::BridgeError {
    let message = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| {
            value
                .pointer("/error/message")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| body.chars().take(400).collect());
    crate::bridge::BridgeError::upstream_status(status, message)
}

#[async_trait::async_trait]
impl crate::bridge::Bridge for CustomResponsesBridge {
    fn name(&self) -> &'static str {
        "custom_responses"
    }

    async fn chat(
        &self,
        req: &crate::bridge::ChatFormat,
        _ctx: &crate::bridge::BridgeContext,
    ) -> Result<crate::bridge::ChatResponse, crate::bridge::BridgeError> {
        let body = crate::account_session::subscriptions::responses::chat_to_responses(req, false);
        let request = self.authorize(self.client.post(&self.url).json(&body), "application/json");
        let response = request.send().await.map_err(transport_error)?;
        let status = response.status().as_u16();
        let text = response.text().await.map_err(transport_error)?;
        if !(200..300).contains(&status) {
            return Err(status_error(status, &text));
        }
        let value: Value = serde_json::from_str(&text).map_err(|_| {
            crate::bridge::BridgeError::UpstreamDecode("上游返回非 JSON".to_string())
        })?;
        crate::account_session::subscriptions::responses::responses_to_chat(&value, &req.model)
            .map_err(crate::bridge::BridgeError::UpstreamDecode)
    }

    async fn chat_stream(
        &self,
        req: &crate::bridge::ChatFormat,
        _ctx: &crate::bridge::BridgeContext,
    ) -> Result<crate::bridge::ChatChunkStream, crate::bridge::BridgeError> {
        let body = crate::account_session::subscriptions::responses::chat_to_responses(req, true);
        let request = self.authorize(self.client.post(&self.url).json(&body), "text/event-stream");
        let response = request.send().await.map_err(transport_error)?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            let text = response.text().await.map_err(transport_error)?;
            return Err(status_error(status, &text));
        }
        let stream = crate::account_session::subscriptions::responses::stream_from_bytes(
            response.bytes_stream(),
        );
        Ok(stream)
    }

    async fn responses_passthrough(
        &self,
        body: &Value,
        stream: bool,
        _ctx: &crate::bridge::BridgeContext,
    ) -> Result<crate::bridge::ResponsesPassthrough, crate::bridge::BridgeError> {
        let accept = if stream {
            "text/event-stream"
        } else {
            "application/json"
        };
        let request = self.authorize(self.client.post(&self.url).json(body), accept);
        let response = request.send().await.map_err(transport_error)?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            let text = response.text().await.map_err(transport_error)?;
            return Err(status_error(status, &text));
        }
        if stream {
            use futures::StreamExt;
            let stream = response
                .bytes_stream()
                .map(|item| item.map_err(|e| crate::bridge::BridgeError::Transport(e.to_string())))
                .boxed();
            Ok(crate::bridge::ResponsesPassthrough::Stream(stream))
        } else {
            let value: Value = response.json().await.map_err(transport_error)?;
            Ok(crate::bridge::ResponsesPassthrough::Json(value))
        }
    }
}

// ── 模型目录拉取 ───────────────────────────────────────────────

/// 服务端代理拉取上游模型清单。
///
/// URL 惯例：`chat_completions`/`responses` 的 baseUrl 通常带 `/v1` → 拼
/// `{base}/models`；`anthropic` 通常是不带 `/v1` 的根地址 → 拼 `{base}/v1/models`。
/// 鉴权：前两者 `Authorization: Bearer`；anthropic `x-api-key` +
/// `anthropic-version`。key 取该家**第一条启用且可用**的账号（空 key 且声明
/// 免鉴权时一个鉴权头都不发）。
///
/// **不落盘**：清单返回给前端确认后再调 [`CustomProviders::set_models`]。
pub async fn fetch_upstream_models(
    provider_id: &str,
    sessions: &AccountSessionStore,
) -> Result<Vec<String>, String> {
    let provider =
        get(provider_id).ok_or_else(|| format!("自定义提供商不存在: {}", provider_id.trim()))?;
    let protocol = provider
        .get("protocol")
        .and_then(Value::as_str)
        .unwrap_or(PROTOCOL_CHAT_COMPLETIONS)
        .to_string();
    let base_url = provider
        .get("baseUrl")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim_end_matches('/')
        .to_string();
    if base_url.is_empty() {
        return Err("该提供商没有配置 baseUrl，无法拉取模型清单".to_string());
    }
    let url = if protocol == PROTOCOL_ANTHROPIC && !base_url.ends_with("/v1") {
        format!("{base_url}/v1/models")
    } else {
        format!("{base_url}/models")
    };
    let account = sessions
        .all()
        .into_iter()
        .find(|session| {
            session.provider == provider_id
                && session.is_enabled()
                && (!session.credential.secret_plain().trim().is_empty()
                    || session.credential.kind
                        == crate::account_session::model::CREDENTIAL_KIND_NONE)
        })
        .ok_or_else(|| "请先添加账号：拉取模型清单需要一条启用且可用的账号".to_string())?;
    let client = sessions
        .client_for(&account)
        .map_err(|e| format!("出口不可用：{e}"))?;
    let mut builder = client.get(&url).timeout(std::time::Duration::from_secs(15));
    let api_key = account.credential.secret_plain();
    if !api_key.trim().is_empty() {
        builder = if protocol == PROTOCOL_ANTHROPIC {
            builder
                .header("x-api-key", api_key.trim())
                .header("anthropic-version", "2023-06-01")
        } else {
            builder.header("Authorization", format!("Bearer {}", api_key.trim()))
        };
    }
    let response = builder
        .send()
        .await
        .map_err(|e| format!("上游请求失败：{e}"))?;
    let status = response.status().as_u16();
    let text = response.text().await.unwrap_or_default();
    if !(200..300).contains(&status) {
        return Err(format!("上游返回 {status}: {text}"));
    }
    let payload: Value =
        serde_json::from_str(&text).map_err(|e| format!("上游响应不是合法 JSON：{e}"))?;
    Ok(payload
        .get("data")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    item.get("id")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|id| !id.is_empty())
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default())
}

// ── 记录归一 / 校验 ───────────────────────────────────────────

/// 存储条目 → 对外形态（补默认、丢弃坏条目；见模块头字段表）。
fn normalize_record(value: &Value) -> Option<Value> {
    let object = value.as_object()?;
    let id = object.get("id").and_then(Value::as_str)?.trim();
    if id.is_empty() {
        return None;
    }
    let name = object
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .unwrap_or(id)
        .to_string();
    let protocol = object
        .get("protocol")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| valid_protocol(text))
        .unwrap_or(PROTOCOL_CHAT_COMPLETIONS)
        .to_string();
    Some(json!({
        "id": id,
        "name": name,
        "protocol": protocol,
        "baseUrl": object.get("baseUrl").and_then(Value::as_str).map(str::trim).unwrap_or(""),
        "enabled": object.get("enabled").and_then(Value::as_bool).unwrap_or(true),
        "createdAt": object.get("createdAt").and_then(Value::as_i64).unwrap_or(0),
        "models": model_entries_of(object.get("models")),
        "mappings": mapping_entries_of(object.get("mappings")),
        "urlSuffix": read_url_suffix(object.get("urlSuffix")),
        "headers": read_headers(object.get("headers")),
        "anthropicToolType": read_tool_type(object.get("anthropicToolType")),
    }))
}

fn truncate_chars(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
}

fn model_entries_of(value: Option<&Value>) -> Vec<Value> {
    let mut entries: Vec<Value> = Vec::new();
    for item in value
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
    {
        let Some(object) = item.as_object() else {
            continue;
        };
        let id = truncate_chars(
            object
                .get("id")
                .and_then(Value::as_str)
                .map(str::trim)
                .unwrap_or(""),
            MAX_MODEL_ID_CHARS,
        );
        if id.is_empty()
            || entries
                .iter()
                .any(|known| known.get("id").and_then(Value::as_str) == Some(id.as_str()))
        {
            continue;
        }
        entries.push(json!({
            "id": id,
            "enabled": object.get("enabled").and_then(Value::as_bool).unwrap_or(true),
            "reasoning": truncate_chars(
                object.get("reasoning").and_then(Value::as_str).map(str::trim).unwrap_or(""),
                MAX_REASONING_CHARS,
            ),
        }));
    }
    entries
}

fn mapping_entries_of(value: Option<&Value>) -> Vec<Value> {
    let mut entries: Vec<Value> = Vec::new();
    for item in value
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
    {
        let Some(object) = item.as_object() else {
            continue;
        };
        let alias = truncate_chars(
            object
                .get("alias")
                .and_then(Value::as_str)
                .map(str::trim)
                .unwrap_or(""),
            MAX_ALIAS_CHARS,
        );
        let target = truncate_chars(
            object
                .get("target")
                .and_then(Value::as_str)
                .map(str::trim)
                .unwrap_or(""),
            MAX_MODEL_ID_CHARS,
        );
        if alias.is_empty() || target.is_empty() {
            continue;
        }
        entries.push(json!({
            "alias": alias,
            "target": target,
            "enabled": object.get("enabled").and_then(Value::as_bool).unwrap_or(true),
            "reasoning": truncate_chars(
                object.get("reasoning").and_then(Value::as_str).map(str::trim).unwrap_or(""),
                MAX_REASONING_CHARS,
            ),
        }));
    }
    entries
}

fn read_url_suffix(value: Option<&Value>) -> String {
    let text = value.and_then(Value::as_str).map(str::trim).unwrap_or("");
    let usable = text.starts_with('?')
        && text.len() >= 2
        && text.len() <= MAX_URL_SUFFIX_CHARS
        && !text.chars().any(char::is_whitespace);
    if usable {
        text.to_string()
    } else {
        String::new()
    }
}

fn read_headers(value: Option<&Value>) -> Map<String, Value> {
    let mut out = Map::new();
    let Some(object) = value.and_then(Value::as_object) else {
        return out;
    };
    for (key, value) in object.iter().take(MAX_HEADER_ENTRIES) {
        if let Some(text) = value.as_str() {
            let key = truncate_chars(key.trim(), MAX_HEADER_NAME_CHARS);
            let text = truncate_chars(text.trim(), MAX_HEADER_VALUE_CHARS);
            if !key.is_empty() && !text.is_empty() && !key.contains(':') {
                out.insert(key, Value::String(text));
            }
        }
    }
    out
}

fn read_tool_type(value: Option<&Value>) -> String {
    let text = value.and_then(Value::as_str).map(str::trim).unwrap_or("");
    if text.eq_ignore_ascii_case(TOOL_TYPE_CUSTOM) {
        TOOL_TYPE_CUSTOM.to_string()
    } else {
        String::new()
    }
}

fn validate_model_entries(raw: &[Value]) -> Result<Vec<Value>, String> {
    let mut entries: Vec<Value> = Vec::new();
    for (position, item) in raw.iter().enumerate() {
        let Some(object) = item.as_object() else {
            return Err(format!("models 第 {} 项必须是对象", position + 1));
        };
        let id = object
            .get("id")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("");
        if id.is_empty() {
            return Err(format!("models 第 {} 项缺少模型 id", position + 1));
        }
        if id.chars().count() > MAX_MODEL_ID_CHARS {
            return Err(format!("模型 id 过长（最多 {MAX_MODEL_ID_CHARS} 个字符）"));
        }
        if entries.iter().any(|known| {
            known
                .get("id")
                .and_then(Value::as_str)
                .is_some_and(|text| text.eq_ignore_ascii_case(id))
        }) {
            return Err(format!("模型 ID「{id}」已存在，请勿重复登记"));
        }
        entries.push(json!({
            "id": id,
            "enabled": object.get("enabled").and_then(Value::as_bool).unwrap_or(true),
            "reasoning": validate_reasoning(object.get("reasoning"))?,
        }));
    }
    Ok(entries)
}

fn validate_mapping_entries(raw: &[Value]) -> Result<Vec<Value>, String> {
    let mut entries: Vec<Value> = Vec::new();
    for (position, item) in raw.iter().enumerate() {
        let Some(object) = item.as_object() else {
            return Err(format!("mappings 第 {} 项必须是对象", position + 1));
        };
        let alias = object
            .get("alias")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("");
        let target = object
            .get("target")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("");
        if alias.is_empty() {
            return Err(format!(
                "mappings 第 {} 项缺少映射名（alias）",
                position + 1
            ));
        }
        if alias.chars().count() > MAX_ALIAS_CHARS {
            return Err(format!("映射名过长（最多 {MAX_ALIAS_CHARS} 个字符）"));
        }
        if target.is_empty() {
            return Err(format!(
                "mappings 第 {} 项缺少目标上游模型（target）",
                position + 1
            ));
        }
        if target.chars().count() > MAX_MODEL_ID_CHARS {
            return Err(format!(
                "目标上游模型过长（最多 {MAX_MODEL_ID_CHARS} 个字符）"
            ));
        }
        if entries.iter().any(|known| {
            known
                .get("alias")
                .and_then(Value::as_str)
                .is_some_and(|text| text.eq_ignore_ascii_case(alias))
                && known
                    .get("target")
                    .and_then(Value::as_str)
                    .is_some_and(|text| text.eq_ignore_ascii_case(target))
        }) {
            return Err(format!("映射「{alias} → {target}」已存在，请勿重复添加"));
        }
        entries.push(json!({
            "alias": alias,
            "target": target,
            "enabled": object.get("enabled").and_then(Value::as_bool).unwrap_or(true),
            "reasoning": validate_reasoning(object.get("reasoning"))?,
        }));
    }
    Ok(entries)
}

fn validate_reasoning(value: Option<&Value>) -> Result<String, String> {
    match value {
        None | Some(Value::Null) => Ok(String::new()),
        Some(Value::String(text)) => {
            let text = text.trim();
            if text.chars().count() > MAX_REASONING_CHARS {
                return Err(format!("思考等级过长（最多 {MAX_REASONING_CHARS} 个字符）"));
            }
            Ok(text.to_string())
        }
        Some(_) => Err("reasoning 必须是字符串或 null".to_string()),
    }
}

fn validate_name(value: Option<&Value>) -> Result<String, String> {
    let name = value.and_then(Value::as_str).map(str::trim).unwrap_or("");
    if name.is_empty() {
        return Err("缺少提供商名称".to_string());
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(format!("提供商名称过长（最多 {MAX_NAME_CHARS} 个字符）"));
    }
    Ok(name.to_string())
}

fn validate_protocol(value: Option<&Value>) -> Result<String, String> {
    let protocol = value.and_then(Value::as_str).map(str::trim).unwrap_or("");
    if !valid_protocol(protocol) {
        return Err(format!("协议必须是以下之一: {}", PROTOCOLS.join(" / ")));
    }
    Ok(protocol.to_string())
}

fn valid_protocol(protocol: &str) -> bool {
    PROTOCOLS.contains(&protocol)
}

/// baseUrl 校验与规范化（http/https；去尾斜杠）。
pub fn validate_base_url(raw: &str) -> Result<String, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("缺少 baseUrl".to_string());
    }
    let parsed = url::Url::parse(raw).map_err(|e| format!("baseUrl 不是合法的 URL: {e}"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("baseUrl 必须以 http:// 或 https:// 开头".to_string());
    }
    Ok(parsed.as_str().trim_end_matches('/').to_string())
}

fn validate_url_suffix(value: Option<&Value>) -> Result<String, String> {
    let text = value.and_then(Value::as_str).map(str::trim).unwrap_or("");
    if text.is_empty() {
        return Ok(String::new());
    }
    if !text.starts_with('?') {
        return Err("urlSuffix 必须是以 ? 开头的查询串（如 ?beta=true）".to_string());
    }
    if text.chars().any(char::is_whitespace) || text.chars().count() > MAX_URL_SUFFIX_CHARS {
        return Err(format!(
            "urlSuffix 不能包含空白且最多 {MAX_URL_SUFFIX_CHARS} 个字符"
        ));
    }
    Ok(text.to_string())
}

fn validate_headers(value: Option<&Value>) -> Result<Map<String, Value>, String> {
    let Some(value) = value.filter(|v| !v.is_null()) else {
        return Ok(Map::new());
    };
    let object = value
        .as_object()
        .ok_or_else(|| "headers 必须是 JSON 对象（{\"头名\": \"值\"}）".to_string())?;
    if object.len() > MAX_HEADER_ENTRIES {
        return Err(format!("headers 最多 {MAX_HEADER_ENTRIES} 条"));
    }
    let mut out = Map::new();
    for (key, value) in object {
        let key = key.trim();
        let text = value.as_str().unwrap_or("").trim();
        if key.is_empty() || text.is_empty() {
            return Err("headers 的头名与值都必须是非空字符串".to_string());
        }
        if key.contains(':') || key.chars().any(char::is_whitespace) {
            return Err(format!("headers 的头名「{key}」不是合法的头名"));
        }
        if key.chars().count() > MAX_HEADER_NAME_CHARS {
            return Err(format!("headers 的头名最长 {MAX_HEADER_NAME_CHARS} 个字符"));
        }
        if text.chars().count() > MAX_HEADER_VALUE_CHARS {
            return Err(format!("headers 的值最长 {MAX_HEADER_VALUE_CHARS} 个字符"));
        }
        out.insert(key.to_string(), Value::String(text.to_string()));
    }
    Ok(out)
}

fn validate_tool_type(value: Option<&Value>) -> Result<String, String> {
    let text = value.and_then(Value::as_str).map(str::trim).unwrap_or("");
    if text.is_empty() {
        return Ok(String::new());
    }
    if text.eq_ignore_ascii_case(TOOL_TYPE_CUSTOM) {
        return Ok(TOOL_TYPE_CUSTOM.to_string());
    }
    Err("anthropicToolType 只支持 \"custom\"（留空表示不补 type）".to_string())
}

/// `custom-` + 12 位小写十六进制（6 字节随机）。
fn new_provider_id() -> Result<String, String> {
    let mut bytes = [0u8; 6];
    getrandom::getrandom(&mut bytes)
        .map_err(|_| "无法生成安全的随机 id（系统随机源不可用），请重试".to_string())?;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    Ok(format!("{ID_PREFIX}{hex}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::FileStore;
    use std::sync::Arc;
    use tempfile::TempDir;

    fn temp_store() -> (TempDir, CustomProviders) {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(FileStore::new(dir.path().to_path_buf()));
        let providers = CustomProviders::new(store);
        (dir, providers)
    }

    #[test]
    fn create_update_and_persist_roundtrip() {
        let (_dir, providers) = temp_store();
        let created = providers
            .create(&json!({
                "name": "我的网关",
                "protocol": "chat_completions",
                "baseUrl": "https://api.example.com/v1/"
            }))
            .expect("create");
        let id = created["id"].as_str().unwrap().to_string();
        assert!(id.starts_with("custom-"));
        assert_eq!(id.len(), "custom-".len() + 12);
        // baseUrl 已去尾斜杠
        assert_eq!(created["baseUrl"], "https://api.example.com/v1");
        assert_eq!(created["enabled"], true);

        let updated = providers
            .update(&id, &json!({ "name": "改名", "enabled": false }))
            .expect("update");
        assert_eq!(updated["name"], "改名");
        assert_eq!(updated["enabled"], false);
        // 未提供的字段保持
        assert_eq!(updated["protocol"], "chat_completions");

        // 重新载入（模拟重启）仍在
        let reloaded = CustomProviders::new(providers.store.clone());
        reloaded.load().unwrap();
        assert_eq!(reloaded.list().len(), 1);
        assert_eq!(reloaded.get(&id).unwrap()["name"], "改名");
    }

    #[test]
    fn validation_rejects_bad_input() {
        let (_dir, providers) = temp_store();
        assert!(providers
            .create(
                &json!({ "name": "", "protocol": "chat_completions", "baseUrl": "https://a.com" })
            )
            .is_err());
        assert!(providers
            .create(&json!({ "name": "x", "protocol": "bogus", "baseUrl": "https://a.com" }))
            .is_err());
        assert!(providers
            .create(&json!({ "name": "x", "protocol": "anthropic", "baseUrl": "ftp://a.com" }))
            .is_err());
        // 合法（anthropic 根地址允许）
        assert!(providers
            .create(&json!({ "name": "x", "protocol": "anthropic", "baseUrl": "https://api.anthropic.com" }))
            .is_ok());
    }

    #[test]
    fn wire_model_prefers_mapping_then_model() {
        let (_dir, providers) = temp_store();
        let created = providers
            .create(&json!({ "name": "g", "protocol": "chat_completions", "baseUrl": "https://a.com/v1" }))
            .unwrap();
        let id = created["id"].as_str().unwrap().to_string();
        providers
            .set_models(
                &id,
                &json!([{ "id": "gpt-x", "reasoning": "high" }]),
                &json!([{ "alias": "my-alias", "target": "gpt-x", "reasoning": "low" }]),
            )
            .unwrap();
        // 直接命中模型
        let (wire, reasoning) = resolve_for(&providers, &id, "gpt-x");
        assert_eq!(wire, "gpt-x");
        assert_eq!(reasoning.as_deref(), Some("high"));
        // 映射优先
        let (wire, reasoning) = resolve_for(&providers, &id, "MY-ALIAS");
        assert_eq!(wire, "gpt-x");
        assert_eq!(reasoning.as_deref(), Some("low"));
        // 未登记原样透传
        let (wire, reasoning) = resolve_for(&providers, &id, "unknown");
        assert_eq!(wire, "unknown");
        assert!(reasoning.is_none());
    }

    /// 测试里绕过全局单例：直接对实例做 resolve（与 `wire_model_for` 同逻辑）。
    fn resolve_for(
        providers: &CustomProviders,
        id: &str,
        requested: &str,
    ) -> (String, Option<String>) {
        let provider = providers.get(id).unwrap();
        let enabled = |item: &Value| item.get("enabled").and_then(Value::as_bool).unwrap_or(true);
        if let Some(mappings) = provider.get("mappings").and_then(Value::as_array) {
            for mapping in mappings.iter().filter(|m| enabled(m)) {
                if mapping
                    .get("alias")
                    .and_then(Value::as_str)
                    .is_some_and(|a| a.eq_ignore_ascii_case(requested))
                {
                    return (
                        mapping["target"].as_str().unwrap_or(requested).to_string(),
                        reasoning_of(mapping),
                    );
                }
            }
        }
        if let Some(models) = provider.get("models").and_then(Value::as_array) {
            for model in models.iter().filter(|m| enabled(m)) {
                if model
                    .get("id")
                    .and_then(Value::as_str)
                    .is_some_and(|i| i.eq_ignore_ascii_case(requested))
                {
                    return (
                        model["id"].as_str().unwrap_or(requested).to_string(),
                        reasoning_of(model),
                    );
                }
            }
        }
        (requested.to_string(), None)
    }

    #[test]
    fn normalize_record_tolerates_old_shapes() {
        // 缺多个字段的旧记录也要能读出来（补默认，不丢弃）
        let item = normalize_record(&json!({ "id": "custom-abc", "name": "old" })).unwrap();
        assert_eq!(item["protocol"], "chat_completions");
        assert_eq!(item["enabled"], true);
        assert_eq!(item["models"], json!([]));
        // 没有 id 的坏条目丢弃
        assert!(normalize_record(&json!({ "name": "no-id" })).is_none());
    }
}
