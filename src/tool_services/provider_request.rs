// Ported from Agent2API v3.5.0-beta.2 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//
//! 统一的搜索上游请求层：密钥选择、失败换钥重试、限额状态记录。
//!
//! 一次 `post_json` 在 [`MAX_PROVIDER_KEYS`] 个密钥范围内轮换：取下一个未在冷却期
//! 的密钥 → 解密 → 发送 → 归一错误。遇到额度耗尽 / 限流 / 无效密钥就记录该密钥
//! 的状态并换下一个；其它错误（输入错误、上游不可用、超时）直接返回。整轮受
//! `deadline`（服务执行超时）约束。

use std::collections::BTreeSet;
use std::time::Duration;

use serde_json::Value;
use tokio::time::Instant;

use super::provider_keys::{self, MAX_PROVIDER_KEYS};
use super::search::{SearchError, SearchErrorCode};
use super::ToolServices;

pub(super) struct ProviderRequest<'a> {
    provider: &'a str,
    operation: &'static str,
    deadline: Instant,
    services: &'a ToolServices,
    client: &'a reqwest::Client,
    test_key_id: Option<&'a str>,
    allow_disabled: bool,
}

impl<'a> ProviderRequest<'a> {
    pub fn new(
        services: &'a ToolServices,
        client: &'a reqwest::Client,
        provider: &'a str,
        operation: &'static str,
        timeout_seconds: u64,
    ) -> Self {
        Self {
            provider,
            operation,
            deadline: Instant::now() + Duration::from_secs(timeout_seconds.max(1)),
            services,
            client,
            test_key_id: None,
            allow_disabled: false,
        }
    }

    /// 管理端「测试连接」用：可点名某个密钥（即使它在冷却期），且只试一次。
    pub fn for_test(
        services: &'a ToolServices,
        client: &'a reqwest::Client,
        provider: &'a str,
        operation: &'static str,
        timeout_seconds: u64,
        key_id: Option<&'a str>,
    ) -> Self {
        Self {
            test_key_id: key_id,
            allow_disabled: true,
            ..Self::new(services, client, provider, operation, timeout_seconds)
        }
    }

    pub async fn post_json(&self, url: &str, payload: Value) -> Result<Value, SearchError> {
        let mut attempted = BTreeSet::new();
        let mut last_error = None;
        while attempted.len() < MAX_PROVIDER_KEYS {
            let remaining = self.deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(last_error.unwrap_or_else(|| {
                    SearchError::new(SearchErrorCode::TimedOut, "The upstream provider timed out")
                }));
            }
            let attempt = match provider_keys::next_key(
                self.services,
                self.provider,
                self.operation,
                &attempted,
                self.test_key_id,
                self.allow_disabled,
            ) {
                Ok(attempt) => attempt,
                Err(error) => return Err(last_error.unwrap_or(error)),
            };
            attempted.insert(attempt.key.id.clone());
            let result = match attempt.decrypt() {
                Ok(api_key) => {
                    let response = self
                        .client
                        .post(url)
                        .timeout(remaining)
                        .header("authorization", format!("Bearer {api_key}"))
                        .header("content-type", "application/json")
                        .header("accept", "application/json")
                        .json(&payload)
                        .send()
                        .await
                        .map_err(transport_error)?;
                    let status = response.status().as_u16();
                    let headers = response.headers().clone();
                    let raw = response.text().await.map_err(transport_error)?;
                    let body: Option<Value> = serde_json::from_str(&raw).ok();
                    match super::provider_errors::response_error(
                        self.provider,
                        status,
                        &headers,
                        body.as_ref(),
                        &raw,
                        &api_key,
                    ) {
                        Some(error) => Err(error),
                        None => body.ok_or_else(|| {
                            SearchError::new(
                                SearchErrorCode::ProviderUnavailable,
                                "The upstream provider returned an invalid JSON response",
                            )
                        }),
                    }
                }
                Err(error) => Err(error),
            };
            match result {
                Ok(body) => {
                    provider_keys::record_success(
                        self.services,
                        self.provider,
                        self.operation,
                        &attempt,
                    )?;
                    return Ok(body);
                }
                Err(error) => {
                    if !matches!(
                        error.code,
                        SearchErrorCode::QuotaExhausted
                            | SearchErrorCode::RateLimited
                            | SearchErrorCode::InvalidProviderKey
                    ) {
                        return Err(error);
                    }
                    provider_keys::record_failure(
                        self.services,
                        self.provider,
                        self.operation,
                        &attempt,
                        &error,
                    )?;
                    if self.test_key_id.is_some() {
                        return Err(error);
                    }
                    last_error = Some(error);
                }
            }
        }
        Err(last_error.unwrap_or_else(|| {
            SearchError::new(
                SearchErrorCode::ProviderUnavailable,
                "No provider API key is available for this request",
            )
        }))
    }
}

fn transport_error(error: reqwest::Error) -> SearchError {
    if error.is_timeout() {
        SearchError::new(SearchErrorCode::TimedOut, "The upstream provider timed out")
    } else {
        SearchError::new(
            SearchErrorCode::ProviderUnavailable,
            "Unable to connect to the upstream provider",
        )
    }
}
