// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//
//! 网络搜索的批量编排（搜索 + 抓取）。
//!
//! ── 搜索 ────────────────────────────────────────────────────
//! queries 1~10 条，可并发；每条 query 并发后台选中的**全部**提供商，单条
//! query 内先合并各家重复来源；跨 query 再按全局编号去重（最多 100 条来源）。
//! 部分上游失败时返回成功结果 + warnings；全部 query 失败时由 HTTP 层回错误
//! 状态。响应形状与上游 Skill 端点逐字段一致。
//!
//! ── 抓取 ────────────────────────────────────────────────────
//! urls 1~10 条，保持输入顺序，尽量部分成功；只接受公网 http/https。

use serde_json::{json, Value};

use super::normalize::{
    dedupe_strings, deduplicate_sources, normalize_fetch_urls, web_source_dedup_key, SourceDraft,
    FETCH_NOTICE, MAX_BATCH_ITEMS, MAX_GLOBAL_SOURCES, SEARCH_NOTICE,
};
use super::ToolServices;
use super::{account_search, providers};
use crate::account_session::AccountSessionStore;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchErrorCode {
    InvalidProviderKey,
    QuotaExhausted,
    RateLimited,
    TimedOut,
    NoResults,
    ProviderUnavailable,
    Configuration,
    InvalidInput,
    PermissionDenied,
}

impl SearchErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidProviderKey => "invalid_provider_key",
            Self::QuotaExhausted => "quota_exhausted",
            Self::RateLimited => "rate_limited",
            Self::TimedOut => "timed_out",
            Self::NoResults => "no_results",
            Self::ProviderUnavailable => "provider_unavailable",
            Self::Configuration => "configuration",
            Self::InvalidInput => "invalid_input",
            Self::PermissionDenied => "permission_denied",
        }
    }

    pub fn http_status(self) -> u16 {
        match self {
            Self::InvalidInput => 400,
            Self::PermissionDenied => 403,
            Self::QuotaExhausted => 402,
            Self::RateLimited => 429,
            Self::Configuration => 503,
            Self::TimedOut => 504,
            _ => 502,
        }
    }
}

#[derive(Clone, Debug)]
pub struct SearchError {
    pub code: SearchErrorCode,
    pub message: String,
    /// 上游声明的额度/限流恢复时间（毫秒时间戳，`None` = 未识别）。
    pub reset_at: Option<i64>,
}

impl SearchError {
    pub fn new(code: SearchErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            reset_at: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct FetchErrorItem {
    pub url: String,
    pub code: SearchErrorCode,
    pub message: String,
}

#[derive(Clone, Debug)]
pub struct FetchPageDraft {
    pub requested_url: String,
    pub title: String,
    pub url: String,
    pub published_date: Option<String>,
    pub providers: Vec<String>,
    pub content: String,
}

impl FetchPageDraft {
    pub fn from_source(requested_url: String, source: SourceDraft) -> Self {
        Self {
            requested_url,
            title: source.title,
            url: source.url,
            published_date: source.published_date,
            providers: source.providers,
            content: source.content,
        }
    }
}

fn source_json(number: usize, source: &SourceDraft) -> Value {
    json!({
        "number": number,
        "title": source.title,
        "url": source.url,
        "publishedDate": source.published_date,
        "providers": source.providers,
        "content": source.content,
    })
}

/// 校验 queries / urls 的批量入参（1~10 条非空字符串）。
pub fn batch_items(values: Option<&Value>, field: &str) -> Result<Vec<String>, SearchError> {
    let Some(items) = values.and_then(Value::as_array) else {
        return Err(SearchError::new(
            SearchErrorCode::InvalidInput,
            format!("{field} must contain between 1 and {MAX_BATCH_ITEMS} strings"),
        ));
    };
    if items.is_empty() || items.len() > MAX_BATCH_ITEMS {
        return Err(SearchError::new(
            SearchErrorCode::InvalidInput,
            format!("{field} must contain between 1 and {MAX_BATCH_ITEMS} strings"),
        ));
    }
    let result: Vec<String> = items
        .iter()
        .map(|item| item.as_str().map(str::trim).unwrap_or("").to_string())
        .collect();
    if result.iter().any(String::is_empty) {
        return Err(SearchError::new(
            SearchErrorCode::InvalidInput,
            format!("{field} must contain only non-empty strings"),
        ));
    }
    Ok(result)
}

struct QueryOutcome {
    query: String,
    sources: Vec<SourceDraft>,
    error: Option<SearchError>,
    warnings: Vec<String>,
}

async fn search_one_query(
    services: &ToolServices,
    sessions: &AccountSessionStore,
    client: &reqwest::Client,
    query: String,
    sources_to_use: &[account_search::SearchSource],
    timeout: u64,
    skipped: &[String],
) -> QueryOutcome {
    let calls = sources_to_use.iter().map(|source| async {
        (
            source.provider(),
            source
                .search(services, sessions, client, &query, timeout)
                .await,
        )
    });
    let outcomes = futures::future::join_all(calls).await;
    let mut warnings = skipped.to_vec();
    let mut sources = Vec::new();
    let mut first_error: Option<SearchError> = None;
    let mut all_failed = true;
    for (provider, outcome) in outcomes {
        match outcome {
            Ok(items) => {
                all_failed = false;
                sources.extend(items);
            }
            Err(error) => {
                warnings.push(format!(
                    "{}: {}",
                    super::provider_display_name(provider),
                    error.message
                ));
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
    }
    let sources = deduplicate_sources(sources);
    if sources.is_empty() {
        let code = if all_failed {
            first_error
                .as_ref()
                .map(|error| error.code)
                .unwrap_or(SearchErrorCode::ProviderUnavailable)
        } else {
            SearchErrorCode::NoResults
        };
        let message = if all_failed {
            "All configured search providers failed for this query"
        } else {
            "No configured search provider returned results for this query"
        };
        return QueryOutcome {
            query,
            sources: Vec::new(),
            error: Some(SearchError::new(code, message)),
            warnings,
        };
    }
    QueryOutcome {
        query,
        sources,
        error: None,
        warnings,
    }
}

pub struct SearchRun {
    pub payload: Value,
    pub successful: usize,
}

/// 跨 query 的全局编号与去重（纯函数，单测直接覆盖）。
fn assemble_searches(outcomes: Vec<QueryOutcome>) -> (Vec<Value>, usize) {
    let mut index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut next_number = 1usize;
    let mut searches = Vec::new();
    let mut successful = 0usize;

    for item in outcomes {
        if let Some(error) = item.error {
            let mut entry = json!({
                "query": item.query,
                "results": [],
                "error": { "code": error.code.as_str(), "message": error.message },
            });
            if !item.warnings.is_empty() {
                entry["warnings"] = json!(item.warnings);
            }
            searches.push(entry);
            continue;
        }
        successful += 1;
        let mut results = Vec::new();
        let mut duplicates = Vec::new();
        for draft in item.sources {
            let key = web_source_dedup_key(&draft.url);
            let existing = if key.is_empty() {
                None
            } else {
                index.get(&key).copied()
            };
            if existing.is_some() || next_number > MAX_GLOBAL_SOURCES {
                duplicates.push(draft);
            } else {
                let number = next_number;
                next_number += 1;
                if !key.is_empty() {
                    index.insert(key, number);
                }
                results.push((number, draft));
            }
        }
        let mut entry = json!({
            "query": item.query,
            "results": results
                .iter()
                .map(|(number, source)| source_json(*number, source))
                .collect::<Vec<_>>(),
        });
        if !item.warnings.is_empty() {
            entry["warnings"] = json!(item.warnings);
        }
        if !duplicates.is_empty() {
            entry["duplicateResultsMergedIntoSources"] = Value::Array(
                duplicates
                    .iter()
                    .map(|source| {
                        json!({
                            "title": source.title,
                            "url": source.url,
                            "publishedDate": source.published_date,
                            "providers": source.providers,
                            "content": source.content,
                        })
                    })
                    .collect(),
            );
        }
        searches.push(entry);
    }
    (searches, successful)
}

/// 执行一次批量搜索。
///
/// `allowed_providers`：网关 Key 的 provider 白名单（`None` = 不限）。账号型
/// 搜索遵守白名单；密钥型不受 provider 白名单限制（与上游同一取舍：白名单
/// 管的是账号的复用边界，密钥是管理员直接配置的外部配额）。
pub async fn run_search(
    services: &ToolServices,
    sessions: &AccountSessionStore,
    client: &reqwest::Client,
    allowed_providers: Option<&[String]>,
    queries: Vec<String>,
) -> Result<SearchRun, SearchError> {
    let queries: Vec<String> = queries
        .into_iter()
        .map(|query| query.trim().to_string())
        .collect();
    if queries.iter().any(String::is_empty) {
        return Err(SearchError::new(
            SearchErrorCode::InvalidInput,
            "Query strings must not be empty",
        ));
    }
    let queries = dedupe_strings(queries);

    let config = services.load();
    let timeout = config
        .timeouts
        .get(super::WEB_SEARCH_SERVICE_ID)
        .copied()
        .unwrap_or(30)
        .clamp(super::TIMEOUT_MIN_SECONDS, super::TIMEOUT_MAX_SECONDS);
    let mut sources = Vec::new();
    let mut skipped = Vec::new();
    let mut denied = false;
    // 一批请求只取一次配置与能力快照。账号引用失效时不改选其他账号。
    for provider in &config.selection.search {
        let entry = config.providers.get(provider).cloned().unwrap_or_default();
        if !entry.enabled {
            continue;
        }
        if super::is_account_provider(provider)
            && !allowed_providers.is_none_or(|list| list.iter().any(|item| item == provider))
        {
            denied = true;
            skipped.push(format!(
                "{}: 当前网关 Key 未获准使用该提供商",
                super::provider_display_name(provider)
            ));
            continue;
        }
        match account_search::prepare(services, sessions, provider, &entry).await {
            Ok(source) => sources.push(source),
            Err(error) => skipped.push(format!(
                "{}: {}",
                super::provider_display_name(provider),
                error.message
            )),
        }
    }
    if sources.is_empty() {
        return Err(SearchError::new(
            if denied {
                SearchErrorCode::PermissionDenied
            } else {
                SearchErrorCode::Configuration
            },
            if skipped.is_empty() {
                "没有已启用并配置好的搜索来源".into()
            } else {
                skipped.join("；")
            },
        ));
    }
    let active: Vec<String> = sources
        .iter()
        .map(|source| source.provider().to_string())
        .collect();
    let outcomes = futures::future::join_all(queries.iter().cloned().map(|query| {
        search_one_query(
            services, sessions, client, query, &sources, timeout, &skipped,
        )
    }))
    .await;

    let (searches, successful) = assemble_searches(outcomes);
    Ok(SearchRun {
        payload: json!({
            "notice": SEARCH_NOTICE,
            "providers": active,
            "searches": searches,
        }),
        successful,
    })
}

pub struct FetchRun {
    pub payload: Value,
    pub successful: usize,
}

/// 执行一次批量抓取。
pub async fn run_fetch(
    services: &ToolServices,
    _sessions: &AccountSessionStore,
    client: &reqwest::Client,
    urls: Vec<String>,
) -> Result<FetchRun, SearchError> {
    let provider = services.load().selection.fetch;
    if !super::is_fetch_provider(&provider) || !super::configured_provider(services, &provider) {
        return Err(SearchError::new(
            SearchErrorCode::Configuration,
            format!(
                "Configured fetch provider \"{provider}\" is not enabled or not configured with an API key"
            ),
        ));
    }

    let (accepted, rejected) = normalize_fetch_urls(urls);
    if accepted.is_empty() {
        return Err(SearchError::new(
            SearchErrorCode::InvalidInput,
            "All provided URLs were rejected (only public http/https URLs are allowed)",
        ));
    }
    let normalized: Vec<String> = accepted
        .iter()
        .map(|(_, normalized)| normalized.clone())
        .collect();
    let timeout = super::timeout_seconds(services, super::WEB_SEARCH_SERVICE_ID);
    let (pages, mut errors) =
        providers::fetch(services, client, &provider, &normalized, timeout).await?;
    let by_normalized: std::collections::HashMap<&str, &str> = accepted
        .iter()
        .map(|(input, normalized)| (normalized.as_str(), input.as_str()))
        .collect();
    let mut numbered = Vec::new();
    for page in pages {
        let number = numbered.len() + 1;
        let requested_url = by_normalized
            .get(page.url.as_str())
            .map(|input| (*input).to_string())
            .unwrap_or_else(|| page.requested_url.clone());
        numbered.push(json!({
            "number": number,
            "requestedUrl": requested_url,
            "title": page.title,
            "url": page.url,
            "publishedDate": page.published_date,
            "providers": page.providers,
            "content": page.content,
        }));
    }
    for error in &mut errors {
        if let Some(input) = by_normalized.get(error.url.as_str()) {
            error.url = (*input).to_string();
        }
    }
    let mut payload = json!({
        "notice": FETCH_NOTICE,
        "provider": provider,
        "pages": numbered,
    });
    if !errors.is_empty() {
        payload["errors"] = Value::Array(
            errors
                .iter()
                .map(|error| {
                    json!({
                        "url": error.url,
                        "code": error.code.as_str(),
                        "message": error.message,
                    })
                })
                .collect(),
        );
    }
    if !rejected.is_empty() {
        payload["rejectedUrls"] = json!(rejected);
    }
    Ok(FetchRun {
        payload,
        successful: numbered.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn batch_items_validates_shape() {
        assert!(batch_items(None, "queries").is_err());
        assert!(batch_items(Some(&json!([])), "queries").is_err());
        assert!(batch_items(Some(&json!(["a", "", "b"])), "queries").is_err());
        let ok = batch_items(Some(&json!([" a ", "b"])), "queries").unwrap();
        assert_eq!(ok, vec!["a", "b"]);
        let too_many: Vec<String> = (0..11).map(|i| i.to_string()).collect();
        assert!(batch_items(Some(&json!(too_many)), "queries").is_err());
    }

    fn draft(url: &str, provider: &str, content: &str) -> SourceDraft {
        SourceDraft {
            title: url.to_string(),
            url: url.to_string(),
            published_date: None,
            providers: vec![provider.to_string()],
            content: content.to_string(),
        }
    }

    #[test]
    fn assemble_searches_numbers_and_dedupes_across_queries() {
        let outcomes = vec![
            QueryOutcome {
                query: "q1".into(),
                sources: vec![
                    draft("https://example.com/a", "Tavily", "body a"),
                    draft("https://example.com/b", "Tavily", "body b"),
                ],
                error: None,
                warnings: vec!["skip1".into()],
            },
            QueryOutcome {
                query: "q2".into(),
                sources: vec![
                    // 跨 query 重复（末尾斜杠 / www 归一后同键）
                    draft("https://www.example.com/a/", "Querit", "body a2"),
                    draft("https://example.com/c", "Querit", "body c"),
                ],
                error: None,
                warnings: Vec::new(),
            },
        ];
        let (searches, successful) = assemble_searches(outcomes);
        assert_eq!(successful, 2);
        assert_eq!(searches[0]["results"].as_array().unwrap().len(), 2);
        assert_eq!(searches[0]["warnings"], json!(["skip1"]));
        // q2 的 /a 重复 → 只保留 /c，编号接续为 3
        let second = searches[1]["results"].as_array().unwrap();
        assert_eq!(second.len(), 1);
        assert_eq!(second[0]["number"], 3);
        assert_eq!(
            searches[1]["duplicateResultsMergedIntoSources"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn assemble_searches_reports_error_entries() {
        let outcomes = vec![QueryOutcome {
            query: "q".into(),
            sources: Vec::new(),
            error: Some(SearchError::new(SearchErrorCode::RateLimited, "slow down")),
            warnings: Vec::new(),
        }];
        let (searches, successful) = assemble_searches(outcomes);
        assert_eq!(successful, 0);
        assert_eq!(searches[0]["error"]["code"], "rate_limited");
    }

    #[test]
    fn error_codes_map_to_http_status() {
        assert_eq!(SearchErrorCode::InvalidInput.http_status(), 400);
        assert_eq!(SearchErrorCode::PermissionDenied.http_status(), 403);
        assert_eq!(SearchErrorCode::RateLimited.http_status(), 429);
        assert_eq!(SearchErrorCode::Configuration.http_status(), 503);
        assert_eq!(SearchErrorCode::TimedOut.http_status(), 504);
        assert_eq!(SearchErrorCode::ProviderUnavailable.http_status(), 502);
    }
}
