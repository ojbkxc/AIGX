// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//
//! 账号型搜索的宿主接线（AIGX 适配）。
//!
//! ── 与上游的差异 ────────────────────────────────────────────
//! 上游的账号型搜索走上游插件的 `admin.action` 两个动作：
//! `web-search.capabilities`（能力探测）与 `web-search.search`（执行搜索）。
//! AIGX 的插件宿主（T1）当前只实现聊天/工具协议，尚未暴露 `admin.action`
//! 通用回调，因此本模块：
//!   1. 完整保留配置面（账号下拉、绑定校验、启用开关、展示行）；
//!   2. 运行时返回稳定的 `configuration` 错误（而不是静默跳过或伪装成功）；
//!   3. 预留 [`SearchSource::Account`] 的接线点：插件宿主一旦提供
//!      `admin_action(account_id, "web-search.search", {query})`，只需在
//!      `search()` 的分支里替换实现，配置形状与管理 API 不用改。
//!
//! 账号引用只保存 `accountId`；凭证仍由 `AccountSessionStore` 与插件管理。

use serde_json::{json, Value};

use super::normalize::{clip, SourceDraft, MAX_CONTENT_LENGTH, MAX_TITLE_LENGTH};
use super::{ProviderConfig, SearchError, SearchErrorCode, ToolServices};
use crate::account_session::{AccountSession, AccountSessionStore};

fn configuration(message: impl Into<String>) -> SearchError {
    SearchError::new(SearchErrorCode::Configuration, message)
}

/// 该来源是否可用（插件能力探测结果）。AIGX 当前恒为 false，理由见模块头。
const ACCOUNT_SEARCH_AVAILABLE: bool = false;

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

pub(super) fn provider_row(
    _services: &ToolServices,
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
    let account = validate_account(sessions, provider, &entry.account_id);
    let error = if !ACCOUNT_SEARCH_AVAILABLE {
        Some(
            "当前 AIGX 插件宿主未提供账号搜索能力（web-search admin.action），绑定后运行时不可用"
                .to_string(),
        )
    } else {
        account.as_ref().err().map(|error| error.message.clone())
    };
    json!({
        "provider": provider,
        "display_name": super::provider_display_name(provider),
        "auth_type": "account",
        "supports_search": true,
        "supports_fetch": false,
        "available": ACCOUNT_SEARCH_AVAILABLE,
        "configured": ACCOUNT_SEARCH_AVAILABLE && account.is_ok(),
        "enabled": entry.enabled,
        "account_id": entry.account_id,
        "account_name": account_name,
        "accounts": options,
        "max_query_chars": if ACCOUNT_SEARCH_AVAILABLE { json!(4000) } else { Value::Null },
        "unavailable_reason": error,
        "key_hint": Value::Null,
        "updated_at": if entry.updated_at > 0 { json!(entry.updated_at) } else { Value::Null },
    })
}

#[derive(Clone)]
pub(super) enum SearchSource {
    Key {
        provider: String,
        key: String,
    },
    Account {
        provider: String,
        /// 插件 ABI 接通后用于 `admin.action` 的账号点名；当前分支恒返回
        /// configuration 错误，字段先保留以免调用面形状随接通再改一次。
        #[allow(dead_code)]
        account_id: String,
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
        _services: &ToolServices,
        _sessions: &AccountSessionStore,
        client: &reqwest::Client,
        query: &str,
        timeout: u64,
    ) -> Result<Vec<SourceDraft>, SearchError> {
        match self {
            Self::Key { provider, key } => {
                super::providers::search(client, provider, key, query, timeout).await
            }
            Self::Account { provider, .. } => {
                // 接线点：插件宿主提供 admin_action 后在此调用
                // `adapter.admin_action(account_id, "web-search.search", {query})`
                // 并把响应按 `source_rows` 归一。当前返回稳定配置错误。
                let _ = provider;
                Err(configuration(
                    "账号型搜索需要提供商插件支持（web-search admin.action）；当前插件宿主未提供该能力",
                ))
            }
        }
    }
}

pub(super) async fn prepare(
    _services: &ToolServices,
    sessions: &AccountSessionStore,
    provider: &str,
    entry: &ProviderConfig,
) -> Result<SearchSource, SearchError> {
    if super::is_account_provider(provider) {
        validate_account(sessions, provider, &entry.account_id)?;
        if ACCOUNT_SEARCH_AVAILABLE {
            Ok(SearchSource::Account {
                provider: provider.to_string(),
                account_id: entry.account_id.clone(),
            })
        } else {
            Err(configuration(
                "账号型搜索需要提供商插件支持（web-search admin.action）；当前插件宿主未提供该能力",
            ))
        }
    } else {
        let key = super::decrypt_provider_key(entry)
            .ok_or_else(|| configuration("提供商尚未配置有效密钥"))?;
        Ok(SearchSource::Key {
            provider: provider.to_string(),
            key,
        })
    }
}

/// 账号型来源的搜索结果归一（插件 ABI 接通后复用；当前单测覆盖）。
#[allow(dead_code)]
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
