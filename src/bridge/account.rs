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
use std::time::{Duration, Instant};

use super::{Bridge, BridgeContext, BridgeError, ChatChunkStream};
use super::{ChatFormat, ChatResponse};
use super::{EmbeddingRequest, EmbeddingResponse, ImageEditFormPart, RerankRequest};
use super::{RerankResponse, ResponsesPassthrough};
use crate::account_session::adapter;
use crate::account_session::store::{AccountLease, AccountSessionStore};
use crate::channel::ChannelStore;

/// 账号型渠道（`ChannelType::Account`）的 Bridge。
pub struct AccountBridge {
    channel_id: String,
    sessions: AccountSessionStore,
    /// 统一模型管理里按 (provider, model) 关停的规则。
    ///
    /// 账号型渠道的 provider 只存在于账号上，渠道层拿不到，只能在这里过滤。
    /// `None` = 未接入规则（测试与旧调用点），行为与改造前一致。
    rules: Option<Arc<crate::model::manage::ModelRules>>,
    /// 插件面停用的 provider 名单（停用 = 选号跳过该家的账号，账号数据保留）。
    ///
    /// `None` = 未接入（测试与旧调用点）。与 `rules` 互补：rules 管
    /// 「这家别供这个模型」，这里管「这家整个别供」。读的是
    /// `ProviderGates` 的内存集合——启停即时生效、无每请求 IO。
    plugin_disabled: Option<crate::api::admin::plugins::ProviderGates>,
    /// Key 级 provider 白名单（07 D 章 allowedProviders；`None`/空 = 不限）。
    ///
    /// 与 `plugin_disabled` 方向相反：那个是「这几家整体关停」，这个是
    /// 「这个 key 只许用这几家」。匹配单元与数据面同口径：账号 `provider`。
    allowed_providers: Option<Vec<String>>,
    /// 逐账号跳过明细缓冲（Phase1-B/#26 路由回放；`None` = 未开启回放）。
    ///
    /// 请求本地共享缓冲：同一次请求可能轮换多个账号型渠道，共用一个
    /// `Arc<Mutex<Vec<AccountSkipGroup>>>`（请求内无竞争，锁持有微秒级），
    /// 由调用方在记录日志时一次性 drain 进 `RequestLog.account_skips`。
    /// 每组行数有界 + 全组总字节兜底（log::cap_account_skip_groups）。
    account_capture: Option<Arc<std::sync::Mutex<Vec<crate::log::AccountSkipGroup>>>>,
    /// 本次请求的能力需求（#40 §1：图 / 工具 / 视频）。
    ///
    /// `CapabilityMask::EMPTY`（默认）= 不做能力过滤，行为与改造前逐字节一致。
    /// 由调用方（`plan_route` 落地后从请求体推需求）经 [`Self::with_capabilities`]
    /// 注入；选号层据此按账号目录的能力位做精确过滤。
    capabilities: crate::model::capability::CapabilityMask,
}

/// 同渠道账号轮换的总时间预算（与共享 Client 的单请求超时对齐）。
///
/// 没有这个预算时，N 个慢账号会把一个客户端请求拖到 N×300s；有预算后
/// 最坏耗时与普通渠道（单次 300s）一致。
const ROTATION_BUDGET: Duration = Duration::from_secs(300);

/// 剩余轮换预算（None = 预算已耗尽，停止换号）。
fn remaining_budget(started_at: Instant) -> Option<Duration> {
    let remaining = ROTATION_BUDGET.saturating_sub(started_at.elapsed());
    if remaining.is_zero() {
        None
    } else {
        Some(remaining)
    }
}

/// 轮换预算耗尽 / 单次尝试超时的错误。
fn rotation_timeout(started_at: Instant) -> BridgeError {
    BridgeError::Timeout {
        elapsed_ms: started_at.elapsed().as_millis() as u64,
        cause: "account rotation budget exhausted".to_string(),
    }
}

impl AccountBridge {
    pub fn new(channel_id: impl Into<String>, sessions: AccountSessionStore) -> Self {
        Self {
            channel_id: channel_id.into(),
            sessions,
            rules: None,
            plugin_disabled: None,
            allowed_providers: None,
            account_capture: None,
            capabilities: crate::model::capability::CapabilityMask::EMPTY,
        }
    }

    /// 接入本次请求的能力需求（#40 §1）。
    ///
    /// 不接入（或传 `EMPTY`）时选号路径不碰能力索引 —— 默认关。
    pub fn with_capabilities(mut self, needs: crate::model::capability::CapabilityMask) -> Self {
        self.capabilities = needs;
        self
    }

    /// 接入统一模型管理的启停规则。
    pub fn with_rules(mut self, rules: Arc<crate::model::manage::ModelRules>) -> Self {
        self.rules = Some(rules);
        self
    }

    /// 接入插件面的 provider 停用闸门（停用 = 该家账号不参与选号，即时生效）。
    pub fn with_plugin_disabled(
        mut self,
        gates: crate::api::admin::plugins::ProviderGates,
    ) -> Self {
        self.plugin_disabled = Some(gates);
        self
    }

    /// 接入 Key 级 provider 白名单（空列表与 `None` 同义 = 不限）。
    pub fn with_allowed_providers(mut self, allowed: Option<Vec<String>>) -> Self {
        self.allowed_providers = allowed;
        self
    }

    /// 接入逐账号跳过明细缓冲（route_replay 开启时由 resolve 注入）。
    pub fn with_account_capture(
        mut self,
        buf: Option<Arc<std::sync::Mutex<Vec<crate::log::AccountSkipGroup>>>>,
    ) -> Self {
        self.account_capture = buf;
        self
    }

    /// 选一个可用账号；`model` 是上游真名，`alias` 是用户可见名。
    ///
    /// 规则过滤在这一层做：`(账号 provider, 上游真名)` 与 `(provider, 别名)`
    /// 任一命中「已关停」就跳过该账号 —— 管理员在模型管理页关掉的是
    /// 「这家别再供这个模型」，而不是「把整个账号停用」。
    /// 插件面停用闸门是整家关停（比 rules 更粗一档）。
    ///
    /// 无候选时带回各过滤器的跳过计数（502 文案原因化，08-prod P1-1）。
    fn pick_account(
        &self,
        model: &str,
        alias: Option<&str>,
    ) -> (
        Option<AccountLease>,
        crate::account_session::store::SkipReasons,
    ) {
        let plugin_off = |provider: &str| {
            self.plugin_disabled
                .as_ref()
                .is_some_and(|gates| gates.is_disabled(provider))
        };
        // Key 白名单：名单内（或名单为空/未接入）才放行
        let key_allowed = |provider: &str| match &self.allowed_providers {
            None => true,
            Some(list) => list.is_empty() || list.iter().any(|p| p == provider),
        };
        // Phase1-B/#26 路由回放：每次选号的明细捕获槽（行数有界
        // ROUTE_REPLAY_MAX_SKIPS，超出转 truncated），选完并进请求级共享
        // 缓冲（按渠道分组 + 总字节兜底）。None = 开关关闭，零开销。
        let mut slot = self
            .account_capture
            .as_ref()
            .map(|_| crate::log::AccountSkipCapture::new(self.channel_id.clone()));
        let detail = slot.as_mut();
        // Phase1-A2 限制条件组门控：仅 limiter_groups（扁平规则走既有
        // balance_low 路径，双计会重复拦截）；未配置时 verdict 早退零开销
        let session_gate = |session: &crate::account_session::model::AccountSession| {
            let verdict = session.limiter_group_verdict();
            if verdict.disable {
                Some("limiter_disable")
            } else if verdict.blocks(model) {
                Some("limiter_skip")
            } else {
                None
            }
        };
        let (lease, reasons) = match &self.rules {
            None => self.sessions.acquire_with_reasons_detail(
                &self.channel_id,
                model,
                alias,
                |provider| key_allowed(provider) && !plugin_off(provider),
                Some(&session_gate),
                detail,
                self.capabilities,
            ),
            Some(rules) => self.sessions.acquire_with_reasons_detail(
                &self.channel_id,
                model,
                alias,
                |provider| {
                    key_allowed(provider)
                        && !plugin_off(provider)
                        && !rules.is_disabled(provider, model)
                        && alias
                            .map(|a| !rules.is_disabled(provider, a))
                            .unwrap_or(true)
                },
                Some(&session_gate),
                detail,
                self.capabilities,
            ),
        };
        // #26：把本次选号的捕获并进请求级共享缓冲（同渠道多次尝试共用一个
        // 组；总字节兜底在 merge 链路里生效）
        if let (Some(slot), Some(buf)) = (slot, self.account_capture.as_ref()) {
            let group = slot.finish();
            let mut buf = buf.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(existing) = buf.iter_mut().find(|g| g.channel_id == group.channel_id) {
                existing.merge(group);
            } else {
                buf.push(group);
            }
            crate::log::cap_account_skip_groups(&mut buf);
        }
        (lease, reasons)
    }

    /// 单次请求最多尝试的账号数（来自轮换策略）。
    fn max_attempts(&self) -> usize {
        self.sessions.policy().max_attempts.max(1)
    }

    /// 换号前的重试间隔（毫秒，0 = 立即换号；「请求重试」设置实时驱动）。
    async fn pause_before_retry(&self) {
        let interval = self.sessions.retry_interval_ms();
        if interval > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(interval)).await;
        }
    }

    /// 同一账号内的额外重试次数（「请求重试」设置的两档之一，
    /// 对齐 Agent2API retryCount）。
    fn self_attempts(&self) -> usize {
        crate::config::retry_self_count() as usize
    }

    /// 测试辅助：should_skip_self_retry 的静态版本（不依赖 self 状态）。
    #[cfg(test)]
    fn should_skip_self_retry_for_test(error: &BridgeError) -> bool {
        Self::classify_skip(error)
    }

    /// should_skip_self_retry 的纯判定核心。
    fn classify_skip(error: &BridgeError) -> bool {
        match error {
            BridgeError::UpstreamStatus { status, .. } => {
                crate::config::is_retry_status(*status) || *status == 404
            }
            BridgeError::ModelNotFound(_) => true,
            _ => false,
        }
    }

    /// 该失败是否应跳过「同账号重试」直接换号。
    ///
    /// 命中「直接换号状态码」名单（默认 402 余额不足）说明这枚凭据/账号已被
    /// 上游判死，原地重试只会重复撞同一面墙，直接换号更快。
    ///
    /// 404 单独豁免（对照 Agent2API 814dcf2 / issue #255）：上游按**请求的
    /// 路由/模型名**拒收——模型不在这一家的套餐里、路由不存在。结论只由
    /// 「这份 body 长什么样」决定，同一账号重发同一份 body 必然得到同一个
    /// 404，原地重试是纯白等；换号才是有意义的动作（别的账号可能有这个模型）。
    /// 普通 400 不在豁免面（语义由上游定，重发是安全默认）。
    fn should_skip_self_retry(&self, error: &BridgeError) -> bool {
        Self::classify_skip(error)
    }

    /// 在持有同一 `bridge` 的前提下按「同账号重试」档位重试一次调用。
    ///
    /// `call` 接收 `Arc<dyn Bridge>`（与 `dispatch` 的 `op` 同形），每次重试
    /// 用同一账号的 bridge 重发，不再经过选号；超时预算按整体 `started_at`
    /// 递耗。返回最后一次结果。
    async fn with_self_retry<T, F, Fut>(
        &self,
        started_at: Instant,
        bridge: Arc<dyn Bridge>,
        call: F,
    ) -> Result<T, BridgeError>
    where
        F: Fn(Arc<dyn Bridge>) -> Fut,
        Fut: std::future::Future<Output = Result<T, BridgeError>>,
    {
        let mut inner = 0usize;
        loop {
            let Some(remaining) = remaining_budget(started_at) else {
                return Err(rotation_timeout(started_at));
            };
            let attempt = match tokio::time::timeout(remaining, call(bridge.clone())).await {
                Ok(result) => result,
                Err(_) => Err(rotation_timeout(started_at)),
            };
            let retry = match &attempt {
                Ok(_) => false,
                Err(e) => inner < self.self_attempts() && !self.should_skip_self_retry(e),
            };
            if !retry {
                return attempt;
            }
            inner += 1;
            self.pause_before_retry().await;
        }
    }

    /// 构造失败（地址/代理/凭证配置问题）也要冷却该账号，避免反复踩坑。
    fn note_build_failure(&self, lease: &AccountLease, model: &str, error: &BridgeError) {
        let failure = ChannelStore::classify_bridge_error(error);
        let secs =
            self.sessions
                .record_failure(lease.id(), model, &failure, &error.to_string(), None);
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
        let status = match error {
            BridgeError::UpstreamStatus { status, .. } => Some(*status),
            _ => None,
        };
        let secs =
            self.sessions
                .record_failure(lease.id(), model, &failure, &error.to_string(), status);
        tracing::warn!(
            "account {} failed on {} (cooldown {}s): {error}",
            lease.id(),
            model,
            secs
        );
        // 上游明确拒了这枚凭据（401/403）：标记为待刷（对齐 Agent2API「401 后强制
        // 刷新」）。凭据是一次性的 refresh_token 换来的，真失效时 waits 到
        // 「临期」还早，光靠 `expires_at` 判不出来 —— 不打这个标，续期永远不会
        // 触发（`needs_refresh` 恒假），账号就死在那了。
        if matches!(
            failure,
            crate::channel::circuit_breaker::FailureType::AuthFailed
        ) && self
            .sessions
            .mark_credential_rejected(lease.id(), &lease.session().provider)
        {
            tracing::info!(
                "account {} credential rejected upstream; queued for refresh",
                lease.id()
            );
        }
        crate::api::openai::is_retryable_bridge_error(error)
    }

    /// 全部账号都不可用 / 都失败时的统一错误。
    ///
    /// `reasons` 是**最后一次**选号尝试的跳过计数（只有「第一轮就选不出」
    /// 时才反映全貌；跑过几轮后选不出，冷却计数是失败造成的，同样有信息量）。
    fn all_failed(
        &self,
        model: &str,
        reasons: &crate::account_session::store::SkipReasons,
    ) -> BridgeError {
        BridgeError::AllAccountsFailed(format!(
            "channel {} exhausted all accounts for model {model}（{}）",
            self.channel_id,
            reasons.summary()
        ))
    }

    /// 透传类接口（embed/rerank/complete/image/responses）的账号轮换。
    async fn dispatch<T, F, Fut>(
        &self,
        model: &str,
        ctx: &BridgeContext,
        op: F,
    ) -> Result<T, BridgeError>
    where
        F: Fn(Arc<dyn Bridge>) -> Fut,
        Fut: std::future::Future<Output = Result<T, BridgeError>>,
    {
        let started_at = Instant::now();
        let alias = Some(ctx.model.as_str());
        let mut last: Option<BridgeError> = None;
        let mut last_reasons = crate::account_session::store::SkipReasons::default();
        for attempt in 0..self.max_attempts() {
            if attempt > 0 {
                self.pause_before_retry().await;
            }
            if remaining_budget(started_at).is_none() {
                last = Some(rotation_timeout(started_at));
                break;
            }
            let (lease, reasons) = self.pick_account(model, alias);
            last_reasons = reasons;
            let Some(lease) = lease else {
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
            let started = Instant::now();
            let result = self.with_self_retry(started_at, bridge, &op).await;
            match result {
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
        Err(last.unwrap_or_else(|| self.all_failed(model, &last_reasons)))
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
        let started_at = Instant::now();
        let alias = Some(ctx.model.as_str());
        let mut last: Option<BridgeError> = None;
        let mut last_reasons = crate::account_session::store::SkipReasons::default();
        for attempt in 0..self.max_attempts() {
            if attempt > 0 {
                self.pause_before_retry().await;
            }
            if remaining_budget(started_at).is_none() {
                last = Some(rotation_timeout(started_at));
                break;
            }
            let (lease, reasons) = self.pick_account(&req.model, alias);
            last_reasons = reasons;
            let Some(lease) = lease else {
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
            let started = Instant::now();
            let result = self
                .with_self_retry(started_at, bridge.clone(), |b| async move {
                    b.chat(req, ctx).await
                })
                .await;
            match result {
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
        Err(last.unwrap_or_else(|| self.all_failed(&req.model, &last_reasons)))
    }

    async fn chat_stream(
        &self,
        req: &ChatFormat,
        ctx: &BridgeContext,
    ) -> Result<ChatChunkStream, BridgeError> {
        let started_at = Instant::now();
        let alias = Some(ctx.model.as_str());
        let mut last: Option<BridgeError> = None;
        let mut last_reasons = crate::account_session::store::SkipReasons::default();
        for attempt in 0..self.max_attempts() {
            if attempt > 0 {
                self.pause_before_retry().await;
            }
            if remaining_budget(started_at).is_none() {
                last = Some(rotation_timeout(started_at));
                break;
            }
            let (lease, reasons) = self.pick_account(&req.model, alias);
            last_reasons = reasons;
            let Some(lease) = lease else {
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
            let started = Instant::now();
            let result = self
                .with_self_retry(started_at, bridge.clone(), |b| async move {
                    b.chat_stream(req, ctx).await
                })
                .await;
            match result {
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
        Err(last.unwrap_or_else(|| self.all_failed(&req.model, &last_reasons)))
    }

    async fn embed(
        &self,
        req: &EmbeddingRequest,
        ctx: &BridgeContext,
    ) -> Result<EmbeddingResponse, BridgeError> {
        self.dispatch(&req.model, ctx, |bridge| async move {
            bridge.embed(req, ctx).await
        })
        .await
    }

    async fn rerank(
        &self,
        req: &RerankRequest,
        ctx: &BridgeContext,
    ) -> Result<RerankResponse, BridgeError> {
        self.dispatch(&req.model, ctx, |bridge| async move {
            bridge.rerank(req, ctx).await
        })
        .await
    }

    async fn complete(&self, body: &Value, ctx: &BridgeContext) -> Result<Value, BridgeError> {
        self.dispatch(&ctx.model, ctx, |bridge| async move {
            bridge.complete(body, ctx).await
        })
        .await
    }

    async fn generate_image(
        &self,
        body: &Value,
        ctx: &BridgeContext,
    ) -> Result<Value, BridgeError> {
        self.dispatch(&ctx.model, ctx, |bridge| async move {
            bridge.generate_image(body, ctx).await
        })
        .await
    }

    async fn edit_image(
        &self,
        parts: &[ImageEditFormPart],
        ctx: &BridgeContext,
    ) -> Result<Value, BridgeError> {
        self.dispatch(&ctx.model, ctx, |bridge| async move {
            bridge.edit_image(parts, ctx).await
        })
        .await
    }

    async fn responses_passthrough(
        &self,
        body: &Value,
        stream: bool,
        ctx: &BridgeContext,
    ) -> Result<ResponsesPassthrough, BridgeError> {
        self.dispatch(&ctx.model, ctx, |bridge| async move {
            bridge.responses_passthrough(body, stream, ctx).await
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn should_skip(e: &BridgeError) -> bool {
        AccountBridge::should_skip_self_retry_for_test(e)
    }

    /// 404 是按模型名/路由拒收的确定性失败：跳过同账号重试直接换号
    /// （对照 Agent2API 814dcf2 / issue #255）。
    #[test]
    fn not_found_skips_self_retry() {
        let mut sessions_err = BridgeError::UpstreamStatus {
            status: 404,
            retry_after: None,
            message: "Invalid model.".to_string(),
            parsed: None,
            wire: crate::bridge::UpstreamWire::Unknown,
        };
        assert!(should_skip(&sessions_err));

        // 反对照：400 语义由上游定，保留原地重发
        if let BridgeError::UpstreamStatus { status, .. } = &mut sessions_err {
            *status = 400;
        }
        assert!(!should_skip(&sessions_err));
    }
}
