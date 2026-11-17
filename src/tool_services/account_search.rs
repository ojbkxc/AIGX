// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//
//! 账号型搜索的宿主接线（AIGX 适配）。
//!
//! ── 与上游的对齐 ────────────────────────────────────────────
//! 上游的账号型搜索走上游插件的 `admin.action` 两个动作：
//! `web-search.capabilities`（能力探测，不发上游请求）与
//! `web-search.search`（执行搜索，`{query, timeout_seconds}` →
//! `{sources:[{url,title,content,publishedDate}]}`）。AIGX 没有上游那个
//! 统一的 `RpcAdapter`，插件分三级路由（原生插件进程 → 脚本宿主 → Wasm
//! 宿主，与数据面 `resolve.rs` 同序），因此这里用 [`AccountSearchHost`]
//! trait 把「调一次插件 admin.action」抽象出来：
//!
//! - 生产实现 [`PluginHostSearchBridge`] 按上述三级路由转发；
//! - [`ToolServices`] 持有 `Option<Arc<dyn AccountSearchHost>>`，由
//!   `main.rs` 在 AppState 就绪后注入（见接线说明）；
//! - 测试注入桩，覆盖「无插件 / 能力不匹配 / 正常转发」三态。
//!
//! 未注入宿主或插件未接管该 provider 时返回稳定的 `configuration` 错误，
//! 不静默跳过、不伪装成功。
//!
//! 账号引用只保存 `accountId`；凭证仍由 `AccountSessionStore` 与插件管理。

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use super::normalize::{clip, SourceDraft, MAX_CONTENT_LENGTH, MAX_TITLE_LENGTH};
use super::{ProviderConfig, SearchError, SearchErrorCode, ToolServices};
use crate::account_session::{AccountSession, AccountSessionStore};

const CAPABILITIES: &str = "web-search.capabilities";
const SEARCH: &str = "web-search.search";

fn configuration(message: impl Into<String>) -> SearchError {
    SearchError::new(SearchErrorCode::Configuration, message)
}

/// 宿主未接线时的稳定错误（无插件宿主 / 未注入）。
fn host_unavailable() -> SearchError {
    configuration("账号型搜索需要提供商插件支持（web-search admin.action）；当前插件宿主未接线")
}

fn is_enabled(session: &AccountSession) -> bool {
    // 配置态状态：`enabled` / `disabled` / `expired`。`expired` 不是配置态
    // 停用（凭证可被维护任务续期），仍允许绑定；真正不可用会在执行时暴露。
    session.status != "disabled"
}

/// 公开账号视图只投影 id/name/enabled，不能把整条账号或插件 secret 送给前端。
fn account_options(sessions: &AccountSessionStore, provider: &str) -> Vec<Value> {
    sessions
        .all()
        .into_iter()
        .filter(|session| session.provider == provider)
        .map(|session| {
            let name = if session.name.trim().is_empty() {
                session.id.clone()
            } else {
                session.name.clone()
            };
            json!({ "id": session.id, "name": name, "enabled": is_enabled(&session) })
        })
        .collect()
}

pub(super) fn validate_account(
    sessions: &AccountSessionStore,
    provider: &str,
    account_id: &str,
) -> Result<(), SearchError> {
    if account_id.trim().is_empty() {
        return Err(configuration("请先选择搜索使用的账号"));
    }
    let session = sessions
        .get(account_id)
        .ok_or_else(|| configuration("已绑定的搜索账号不存在，请重新选择"))?;
    if session.provider != provider {
        return Err(configuration("搜索账号不属于该提供商，请重新选择"));
    }
    if !is_enabled(&session) {
        return Err(configuration("已绑定的搜索账号已停用"));
    }
    Ok(())
}

/* ─── 插件宿主桥 ───────────────────────────────────────────── */

/// 「调一次插件 admin.action」的抽象。
///
/// `account_id` 为空 = 能力探测（插件按上游口径不必有账号）；插件语义由
/// 插件自己实现，网关只透传。
/// `double_must_use`：`async_trait` 宏给每个方法的返回值包一层 pinned future
///（本身已是 must_use），再叠加一层 `#[must_use]` 会被 clippy 判为重复。
#[allow(clippy::double_must_use)]
#[async_trait::async_trait]
pub trait AccountSearchHost: Send + Sync {
    async fn admin_action(
        &self,
        provider: &str,
        account_id: &str,
        action: &str,
        payload: Value,
        timeout: Duration,
    ) -> Result<Value, SearchError>;
}

/// 生产实现：与数据面同序的三级路由（原生插件进程 → 脚本宿主 → Wasm 宿主）。
///
/// 三者互斥：Wasm 包的 provider 归 `PackRegistry` 管，脚本宿主不会接管；
/// Wasm 包也不占 `processes` 表，`process_for_provider` 对它返回 `None`。
pub struct PluginHostSearchBridge {
    plugin_host: Arc<crate::plugin_host::PluginHostManager>,
    script_host: Arc<crate::plugin_host::script::ScriptHostManager>,
}

impl PluginHostSearchBridge {
    pub fn new(
        plugin_host: Arc<crate::plugin_host::PluginHostManager>,
        script_host: Arc<crate::plugin_host::script::ScriptHostManager>,
    ) -> Self {
        Self {
            plugin_host,
            script_host,
        }
    }
}

/// 插件侧错误码 → 搜索错误（对齐上游 `search_failure` 的稳定口径：
/// 不把插件内部的认证/代理信息透给公开层）。
fn plugin_failure(code: Option<&str>) -> SearchError {
    if matches!(code, Some("unsupported" | "no_method")) {
        return configuration("当前提供商插件不支持账号搜索，请更新插件");
    }
    let status = code
        .and_then(|code| code.strip_prefix("provider_http:"))
        .and_then(|json| serde_json::from_str::<Value>(json).ok())
        .and_then(|value| value.get("status").and_then(Value::as_u64));
    match status {
        Some(401) => SearchError::new(
            SearchErrorCode::InvalidProviderKey,
            "搜索账号登录态已失效，请重新登录",
        ),
        Some(402 | 429) => {
            SearchError::new(SearchErrorCode::RateLimited, "搜索额度已用完或触发限流")
        }
        Some(504) => SearchError::new(SearchErrorCode::TimedOut, "搜索请求超时"),
        Some(403) => SearchError::new(
            SearchErrorCode::ProviderUnavailable,
            "搜索服务拒绝了该账号，请检查账号权限",
        ),
        _ => SearchError::new(
            SearchErrorCode::ProviderUnavailable,
            "账号搜索暂不可用，请检查账号及插件状态",
        ),
    }
}

fn timeout_error() -> SearchError {
    SearchError::new(SearchErrorCode::TimedOut, "账号搜索请求超时")
}

#[async_trait::async_trait]
impl AccountSearchHost for PluginHostSearchBridge {
    async fn admin_action(
        &self,
        provider: &str,
        account_id: &str,
        action: &str,
        payload: Value,
        timeout: Duration,
    ) -> Result<Value, SearchError> {
        let params = json!({
            "provider": provider,
            "account": account_id,
            "action": action,
            "payload": payload,
        });
        if let Some(process) = self.plugin_host.process_for_provider(provider) {
            let result = tokio::time::timeout(timeout, process.rpc.call("admin.action", params))
                .await
                .map_err(|_| timeout_error())?;
            return result.map_err(|error| plugin_failure(error.code.as_deref()));
        }
        if let Some(route) = self.script_host.route_for_provider(provider) {
            // 脚本宿主的 `main.mjs` 按 `params.provider` 在装载表里找插件，
            // 别名已由 `route.provider` 翻译成实际 id（与数据面同口径）。
            let mut params = params;
            params["provider"] = Value::String(route.provider.clone());
            let result = tokio::time::timeout(timeout, route.channel.call("admin.action", params))
                .await
                .map_err(|_| timeout_error())?;
            return result.map_err(|error| plugin_failure(error.code.as_deref()));
        }
        if let Some(host) = crate::plugin_host::wasm::host_for_provider(provider) {
            let result = tokio::time::timeout(timeout, host.call("admin.action", params, timeout))
                .await
                .map_err(|_| timeout_error())?;
            return result.map_err(|error| plugin_failure(error.code.as_deref()));
        }
        Err(configuration(
            "提供商插件未安装或未启用，请在插件管理中安装并启用该提供商的插件",
        ))
    }
}

/* ─── 能力探测 ─────────────────────────────────────────────── */

/// 沿用上游能力探测口径：`version = 1`、`search = true`、
/// `max_query_chars ∈ 1..=16000`；探测不发上游请求。
async fn capability(services: &ToolServices, provider: &str) -> Result<usize, SearchError> {
    let host = services
        .account_search_host()
        .ok_or_else(host_unavailable)?;
    let metadata = host
        .admin_action(
            provider,
            "",
            CAPABILITIES,
            json!({}),
            Duration::from_secs(5),
        )
        .await
        .map_err(|error| match error.code {
            // 无插件 / 插件声明不支持：保留可读的配置错误
            SearchErrorCode::Configuration => error,
            _ => SearchError::new(
                SearchErrorCode::ProviderUnavailable,
                "无法确认插件搜索能力，请更新或重启提供商插件",
            ),
        })?;
    if metadata.get("version").and_then(Value::as_u64) != Some(1)
        || metadata.get("search").and_then(Value::as_bool) != Some(true)
    {
        return Err(configuration("当前提供商插件不支持账号搜索，请更新插件"));
    }
    metadata
        .get("max_query_chars")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .filter(|value| (1..=16000).contains(value))
        .ok_or_else(|| configuration("插件搜索能力声明不完整，请更新插件"))
}

pub(super) async fn provider_row(
    services: &ToolServices,
    sessions: &AccountSessionStore,
    provider: &str,
    entry: ProviderConfig,
) -> Value {
    let options = account_options(sessions, provider);
    let account_name = options
        .iter()
        .find(|account| account["id"] == entry.account_id)
        .and_then(|account| account.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let support = capability(services, provider).await;
    let account = validate_account(sessions, provider, &entry.account_id);
    let error = support.as_ref().err().or_else(|| account.as_ref().err());
    let configured = support.is_ok() && account.is_ok();
    json!({
        "provider": provider,
        "display_name": super::provider_display_name(provider),
        "auth_type": "account",
        "supports_search": true,
        "supports_fetch": false,
        "available": support.is_ok(),
        "configured": configured,
        "enabled": entry.enabled,
        "account_id": entry.account_id,
        "account_name": account_name,
        "accounts": options,
        "max_query_chars": support.as_ref().ok().map(|chars| json!(chars)),
        "unavailable_reason": error.map(|error| error.message.as_str()),
        "key_hint": Value::Null,
        "updated_at": if entry.updated_at > 0 { json!(entry.updated_at) } else { Value::Null },
    })
}

/* ─── 搜索执行 ─────────────────────────────────────────────── */

#[derive(Clone, Debug)]
pub(super) enum SearchSource {
    Key {
        provider: String,
        key: String,
    },
    Account {
        provider: String,
        /// 插件 `admin.action` 点名用的账号 id（凭证由账号库与插件管理）。
        account_id: String,
        /// 插件声明的单条 query 字符上限。
        max_chars: usize,
    },
}

impl SearchSource {
    pub(super) fn provider(&self) -> &str {
        match self {
            Self::Key { provider, .. } | Self::Account { provider, .. } => provider,
        }
    }

    pub(super) async fn search(
        &self,
        services: &ToolServices,
        sessions: &AccountSessionStore,
        client: &reqwest::Client,
        query: &str,
        timeout: u64,
    ) -> Result<Vec<SourceDraft>, SearchError> {
        match self {
            Self::Key { provider, key } => {
                super::providers::search(client, provider, key, query, timeout).await
            }
            Self::Account {
                provider,
                account_id,
                max_chars,
            } => {
                if query.chars().count() > *max_chars {
                    return Err(SearchError::new(
                        SearchErrorCode::InvalidInput,
                        format!("该搜索来源最多接受 {max_chars} 个字符"),
                    ));
                }
                validate_account(sessions, provider, account_id)?;
                let host = services
                    .account_search_host()
                    .ok_or_else(host_unavailable)?;
                let payload = json!({ "query": query, "timeout_seconds": timeout });
                let result = host
                    .admin_action(
                        provider,
                        account_id,
                        SEARCH,
                        payload,
                        Duration::from_secs(timeout.saturating_add(2)),
                    )
                    .await?;
                source_rows(provider, &result)
            }
        }
    }
}

pub(super) async fn prepare(
    services: &ToolServices,
    sessions: &AccountSessionStore,
    provider: &str,
    entry: &ProviderConfig,
) -> Result<SearchSource, SearchError> {
    if super::is_account_provider(provider) {
        validate_account(sessions, provider, &entry.account_id)?;
        let max_chars = capability(services, provider).await?;
        Ok(SearchSource::Account {
            provider: provider.to_string(),
            account_id: entry.account_id.clone(),
            max_chars,
        })
    } else {
        let key = super::decrypt_provider_key(entry)
            .ok_or_else(|| configuration("提供商尚未配置有效密钥"))?;
        Ok(SearchSource::Key {
            provider: provider.to_string(),
            key,
        })
    }
}

/// 账号型来源的搜索结果归一。
pub(super) fn source_rows(provider: &str, value: &Value) -> Result<Vec<SourceDraft>, SearchError> {
    let rows = value
        .get("sources")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            SearchError::new(
                SearchErrorCode::ProviderUnavailable,
                "插件返回的搜索来源格式不正确",
            )
        })?;
    Ok(rows
        .iter()
        .take(50)
        .filter_map(|row| {
            let url = row.get("url").and_then(Value::as_str)?.trim();
            let parsed = url::Url::parse(url).ok()?;
            if !matches!(parsed.scheme(), "http" | "https")
                || parsed.host_str().is_none()
                || !parsed.username().is_empty()
                || parsed.password().is_some()
            {
                return None;
            }
            let title = row
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or(url)
                .trim();
            let content = row
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            let date = row
                .get("publishedDate")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|value| clip(value, 100));
            Some(SourceDraft {
                url: parsed.to_string(),
                title: clip(if title.is_empty() { url } else { title }, MAX_TITLE_LENGTH),
                content: clip(content, MAX_CONTENT_LENGTH),
                published_date: date,
                providers: vec![super::provider_display_name(provider).to_string()],
            })
        })
        .collect())
}

pub async fn test_provider(
    client: &reqwest::Client,
    services: &ToolServices,
    sessions: &AccountSessionStore,
    provider: &str,
) -> Value {
    if !super::is_account_provider(provider) {
        return super::providers::test_provider(client, services, provider).await;
    }
    let started = super::now_ms();
    let entry = services
        .load()
        .providers
        .get(provider)
        .cloned()
        .unwrap_or_default();
    let result = match prepare(services, sessions, provider, &entry).await {
        Ok(source) => {
            source
                .search(
                    services,
                    sessions,
                    client,
                    "AIGX connectivity check",
                    super::timeout_seconds(services, super::WEB_SEARCH_SERVICE_ID),
                )
                .await
        }
        Err(error) => Err(error),
    };
    let duration = super::now_ms() - started;
    match result {
        Ok(sources) if !sources.is_empty() => json!({
            "success": true,
            "provider": provider,
            "result_count": sources.len(),
            "duration_ms": duration,
        }),
        Ok(_) => json!({
            "success": false,
            "provider": provider,
            "code": "no_results",
            "error": "搜索提供商没有返回来源",
            "duration_ms": duration,
        }),
        Err(error) => json!({
            "success": false,
            "provider": provider,
            "code": error.code.as_str(),
            "error": error.message,
            "duration_ms": duration,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    use crate::storage::FileStore;

    /// 能力探测 / 搜索转发的桩宿主：按动作返回预置载荷并记录调用。
    struct StubHost {
        metadata: Value,
        response: Value,
        calls: Mutex<Vec<(String, String, Value)>>,
    }

    impl StubHost {
        fn new(metadata: Value, response: Value) -> Self {
            Self {
                metadata,
                response,
                calls: Mutex::new(Vec::new()),
            }
        }

        fn calls(&self) -> Vec<(String, String, Value)> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl AccountSearchHost for StubHost {
        async fn admin_action(
            &self,
            _provider: &str,
            account_id: &str,
            action: &str,
            payload: Value,
            _timeout: Duration,
        ) -> Result<Value, SearchError> {
            self.calls
                .lock()
                .unwrap()
                .push((action.to_string(), account_id.to_string(), payload));
            if action == CAPABILITIES {
                Ok(self.metadata.clone())
            } else {
                Ok(self.response.clone())
            }
        }
    }

    fn harness() -> (ToolServices, AccountSessionStore, tempfile::TempDir) {
        let dir = tempfile::TempDir::new().unwrap();
        let store = Arc::new(FileStore::new(dir.path().to_path_buf()));
        let sessions = AccountSessionStore::new(store.clone());
        (ToolServices::new(store), sessions, dir)
    }

    fn bind_account(sessions: &AccountSessionStore, provider: &str) -> String {
        let mut session = AccountSession::new("ch-plugin", "search account");
        session.provider = provider.to_string();
        sessions.upsert(session).unwrap().id
    }

    fn entry(account_id: &str) -> ProviderConfig {
        ProviderConfig {
            account_id: account_id.to_string(),
            enabled: true,
            ..ProviderConfig::default()
        }
    }

    fn good_metadata() -> Value {
        json!({ "version": 1, "search": true, "max_query_chars": 4000 })
    }

    /// 宿主已接线但 provider 无插件接管（生产里由 bridge 的 configuration
    /// 错误表达；这里直接复刻该错误，验证 capability() 不把它吞成 502）。
    struct UnroutedHost;

    #[async_trait::async_trait]
    impl AccountSearchHost for UnroutedHost {
        async fn admin_action(
            &self,
            _provider: &str,
            _account_id: &str,
            _action: &str,
            _payload: Value,
            _timeout: Duration,
        ) -> Result<Value, SearchError> {
            Err(configuration("提供商插件未安装或未启用"))
        }
    }

    /// 无插件接管：configuration 原样上抛，不降级为 ProviderUnavailable。
    #[tokio::test]
    async fn unrouted_provider_is_configuration_error() {
        let (services, sessions, _dir) = harness();
        let account_id = bind_account(&sessions, "workbuddy");
        services.set_account_search_host(Arc::new(UnroutedHost));
        let error = prepare(&services, &sessions, "workbuddy", &entry(&account_id))
            .await
            .unwrap_err();
        assert_eq!(error.code, SearchErrorCode::Configuration);
        assert!(
            error.message.contains("未安装或未启用"),
            "{}",
            error.message
        );
    }

    /// 无宿主注入：稳定 configuration，不 panic。
    #[tokio::test]
    async fn prepare_without_host_is_configuration_error() {
        let (services, sessions, _dir) = harness();
        let account_id = bind_account(&sessions, "workbuddy");
        let error = prepare(&services, &sessions, "workbuddy", &entry(&account_id))
            .await
            .unwrap_err();
        assert_eq!(error.code, SearchErrorCode::Configuration);
        assert!(
            error.message.contains("插件宿主未接线"),
            "{}",
            error.message
        );
    }

    /// 插件在，但能力声明不匹配（search=false）→ configuration。
    #[tokio::test]
    async fn capability_mismatch_is_configuration_error() {
        let (services, sessions, _dir) = harness();
        let account_id = bind_account(&sessions, "workbuddy");
        services.set_account_search_host(Arc::new(StubHost::new(
            json!({ "version": 1, "search": false, "max_query_chars": 4000 }),
            Value::Null,
        )));
        let error = prepare(&services, &sessions, "workbuddy", &entry(&account_id))
            .await
            .unwrap_err();
        assert_eq!(error.code, SearchErrorCode::Configuration);
        assert!(
            error.message.contains("不支持账号搜索"),
            "{}",
            error.message
        );
    }

    /// 能力正常：探测参数正确、search 把账号与 payload 透传到宿主、
    /// 返回来源按上游形状归一。
    #[tokio::test]
    async fn search_forwards_admin_action_and_normalizes_sources() {
        let (services, sessions, _dir) = harness();
        let account_id = bind_account(&sessions, "workbuddy");
        let host = Arc::new(StubHost::new(
            good_metadata(),
            json!({ "sources": [
                { "url": "https://example.com/a", "title": "A", "content": "body", "publishedDate": "2026-01-01" },
                { "url": "not-a-url", "title": "bad", "content": "body" }
            ] }),
        ));
        services.set_account_search_host(host.clone());
        let source = prepare(&services, &sessions, "workbuddy", &entry(&account_id))
            .await
            .unwrap();
        let rows = source
            .search(&services, &sessions, &reqwest::Client::new(), "hello", 30)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].url, "https://example.com/a");
        let calls = host.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].0, CAPABILITIES);
        assert!(calls[0].1.is_empty(), "能力探测不带账号");
        assert_eq!(calls[1].0, SEARCH);
        assert_eq!(calls[1].1, account_id);
        assert_eq!(calls[1].2["query"], json!("hello"));
        assert_eq!(calls[1].2["timeout_seconds"], json!(30));
    }

    /// 超过插件声明的 max_query_chars → InvalidInput（不发搜索请求）。
    #[tokio::test]
    async fn query_over_max_chars_is_invalid_input() {
        let (services, sessions, _dir) = harness();
        let account_id = bind_account(&sessions, "workbuddy");
        let host = Arc::new(StubHost::new(
            json!({ "version": 1, "search": true, "max_query_chars": 4 }),
            json!({ "sources": [] }),
        ));
        services.set_account_search_host(host.clone());
        let source = prepare(&services, &sessions, "workbuddy", &entry(&account_id))
            .await
            .unwrap();
        let error = source
            .search(&services, &sessions, &reqwest::Client::new(), "hello", 30)
            .await
            .unwrap_err();
        assert_eq!(error.code, SearchErrorCode::InvalidInput);
        assert_eq!(host.calls().len(), 1, "超限时只发生过能力探测");
    }

    #[test]
    fn source_rows_filters_and_clips() {
        let payload = json!({
            "sources": [
                { "url": "https://example.com/a", "title": "A", "content": "body", "publishedDate": "2026-01-01" },
                { "url": "http://127.0.0.1/x", "title": "bad", "content": "body" },
                { "url": "https://user:pass@example.com/x", "title": "bad", "content": "body" },
                { "url": "not-a-url", "title": "bad", "content": "body" },
                { "url": "https://example.com/b", "content": "" }
            ]
        });
        let rows = source_rows("workbuddy", &payload).unwrap();
        // 127.0.0.1 本身是合法 http URL（账号来源不做公网校验）；userinfo /
        // 非 URL 被丢。共 3 条：a、127.0.0.1、b。
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].title, "A");
        assert_eq!(rows[0].providers, vec!["WorkBuddy 国内版"]);
    }

    #[test]
    fn invalid_payload_is_error() {
        assert!(source_rows("kiro", &json!({})).is_err());
    }
}
