//! 账号型渠道 Bridge —— 渠道内再做一层账号级调度。
//!
//! 与既有 bridge 的关系：本 Bridge 不实现任何协议细节，选号后复用
//! [`crate::account_session::adapter`] 构造的原生 Bridge（openai/anthropic/
//! gemini），因此工具调用、reasoning、SSE、图片等能力与普通渠道完全一致。
//!
//! failover 语义：同一渠道内换账号重试（4xx 等不可重试错误直接透出），
//! 每次失败按 `ChannelStore::classify_bridge_error` 的分类冷却该账号。

use async_trait::async_trait;
use futures::StreamExt;
use serde_json::Value;
use std::sync::Arc;

use super::{Bridge, BridgeContext, BridgeError, ChatChunkStream};
use super::{ChatFormat, ChatResponse};
use super::{EmbeddingRequest, EmbeddingResponse, RerankRequest};
use super::{RerankResponse, ResponsesPassthrough};
use crate::account_session::adapter;
use crate::account_session::store::{AccountLease, AccountSessionStore};
use crate::channel::ChannelStore;

/// 账号型渠道（`ChannelType::Account`）的 Bridge。
pub struct AccountBridge {
    channel_id: String,
    sessions: AccountSessionStore,
}

impl AccountBridge {
    pub fn new(channel_id: impl Into<String>, sessions: AccountSessionStore) -> Self {
        Self {
            channel_id: channel_id.into(),
            sessions,
        }
    }

    /// 单次请求最多尝试的账号数（来自轮换策略）。
    fn max_attempts(&self) -> usize {
        self.sessions.policy().max_attempts.max(1)
    }

    /// 构造失败（地址/代理/凭证配置问题）也要冷却该账号，避免反复踩坑。
    fn note_build_failure(&self, lease: &AccountLease, model: &str, error: &BridgeError) {
        let failure = ChannelStore::classify_bridge_error(error);
        let secs = self
            .sessions
            .record_failure(lease.id(), model, &failure, &error.to_string());
        tracing::warn!(
            "account {} build failed on {} (cooldown {}s): {error}",
            lease.id(),
            model,
            secs
        );
    }

    /// 记入一次上游失败；返回是否值得换号重试。
    fn note_failure(&self, lease: &AccountLease, model: &str, error: &BridgeError) -> bool {
        let failure = ChannelStore::classify_bridge_error(error);
        let secs = self
            .sessions
            .record_failure(lease.id(), model, &failure, &error.to_string());
        tracing::warn!(
            "account {} failed on {} (cooldown {}s): {error}",
            lease.id(),
            model,
            secs
        );
        crate::api::openai::is_retryable_bridge_error(error)
    }

    /// 全部账号都不可用 / 都失败时的统一错误。
    fn all_failed(&self, model: &str) -> BridgeError {
        BridgeError::AllAccountsFailed(format!(
            "channel {} exhausted all accounts for model {model}",
            self.channel_id
        ))
    }

    /// 透传类接口（embed/rerank/complete/image/responses）的账号轮换。
    async fn dispatch<T, F, Fut>(&self, model: &str, op: F) -> Result<T, BridgeError>
    where
        F: Fn(Arc<dyn Bridge>) -> Fut,
        Fut: std::future::Future<Output = Result<T, BridgeError>>,
    {
        let mut last: Option<BridgeError> = None;
        for _ in 0..self.max_attempts() {
            let Some(lease) = self.sessions.acquire(&self.channel_id, model) else {
                break;
            };
            let bridge = match adapter::build_bridge(lease.session(), &self.sessions) {
                Ok(bridge) => bridge,
                Err(e) => {
                    self.note_build_failure(&lease, model, &e);
                    last = Some(e);
                    continue;
                }
            };
            let started = std::time::Instant::now();
            match op(bridge).await {
                Ok(value) => {
                    let elapsed = started.elapsed().as_millis() as u64;
                    self.sessions.record_success(lease.id(), model, elapsed);
                    return Ok(value);
                }
                Err(e) => {
                    if !self.note_failure(&lease, model, &e) {
                        return Err(e);
                    }
                    last = Some(e);
                }
            }
        }
        Err(last.unwrap_or_else(|| self.all_failed(model)))
    }
}

#[async_trait]
impl Bridge for AccountBridge {
    fn name(&self) -> &'static str {
        "account_pool"
    }

    async fn chat(
        &self,
        req: &ChatFormat,
        ctx: &BridgeContext,
    ) -> Result<ChatResponse, BridgeError> {
        let mut last: Option<BridgeError> = None;
        for _ in 0..self.max_attempts() {
            let Some(lease) = self.sessions.acquire(&self.channel_id, &req.model) else {
                break;
            };
            let bridge = match adapter::build_bridge(lease.session(), &self.sessions) {
                Ok(bridge) => bridge,
                Err(e) => {
                    self.note_build_failure(&lease, &req.model, &e);
                    last = Some(e);
                    continue;
                }
            };
            let started = std::time::Instant::now();
            match bridge.chat(req, ctx).await {
                Ok(response) => {
                    let elapsed = started.elapsed().as_millis() as u64;
                    self.sessions
                        .record_success(lease.id(), &req.model, elapsed);
                    return Ok(response);
                }
                Err(e) => {
                    if !self.note_failure(&lease, &req.model, &e) {
                        return Err(e);
                    }
                    last = Some(e);
                }
            }
        }
        Err(last.unwrap_or_else(|| self.all_failed(&req.model)))
    }

    async fn chat_stream(
        &self,
        req: &ChatFormat,
        ctx: &BridgeContext,
    ) -> Result<ChatChunkStream, BridgeError> {
        let mut last: Option<BridgeError> = None;
        for _ in 0..self.max_attempts() {
            let Some(lease) = self.sessions.acquire(&self.channel_id, &req.model) else {
                break;
            };
            let bridge = match adapter::build_bridge(lease.session(), &self.sessions) {
                Ok(bridge) => bridge,
                Err(e) => {
                    self.note_build_failure(&lease, &req.model, &e);
                    last = Some(e);
                    continue;
                }
            };
            let started = std::time::Instant::now();
            match bridge.chat_stream(req, ctx).await {
                Ok(stream) => {
                    let elapsed = started.elapsed().as_millis() as u64;
                    self.sessions
                        .record_success(lease.id(), &req.model, elapsed);
                    // 并发槽位随流生命周期释放：租约移入闭包持有，流结束才归还
                    let guarded = stream.map(move |item| {
                        let _lease = &lease;
                        item
                    });
                    return Ok(guarded.boxed());
                }
                Err(e) => {
                    if !self.note_failure(&lease, &req.model, &e) {
                        return Err(e);
                    }
                    last = Some(e);
                }
            }
        }
        Err(last.unwrap_or_else(|| self.all_failed(&req.model)))
    }

    async fn embed(
        &self,
        req: &EmbeddingRequest,
        ctx: &BridgeContext,
    ) -> Result<EmbeddingResponse, BridgeError> {
        self.dispatch(
            &req.model,
            |bridge| async move { bridge.embed(req, ctx).await },
        )
        .await
    }

    async fn rerank(
        &self,
        req: &RerankRequest,
        ctx: &BridgeContext,
    ) -> Result<RerankResponse, BridgeError> {
        self.dispatch(
            &req.model,
            |bridge| async move { bridge.rerank(req, ctx).await },
        )
        .await
    }

    async fn complete(&self, body: &Value, ctx: &BridgeContext) -> Result<Value, BridgeError> {
        self.dispatch(&ctx.model, |bridge| async move {
            bridge.complete(body, ctx).await
        })
        .await
    }

    async fn generate_image(
        &self,
        body: &Value,
        ctx: &BridgeContext,
    ) -> Result<Value, BridgeError> {
        self.dispatch(&ctx.model, |bridge| async move {
            bridge.generate_image(body, ctx).await
        })
        .await
    }

    async fn responses_passthrough(
        &self,
        body: &Value,
        stream: bool,
        ctx: &BridgeContext,
    ) -> Result<ResponsesPassthrough, BridgeError> {
        self.dispatch(&ctx.model, |bridge| async move {
            bridge.responses_passthrough(body, stream, ctx).await
        })
        .await
    }
}
