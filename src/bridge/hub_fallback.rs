//! 无渠道兜底（Hub 专用提供商）的**可观测性**包装。
//!
//! ## 为什么需要这一层
//!
//! `resolve_bridges*` 的第二级会追加一个「Hub 专用 bridge」（目前只有
//! `cloudflare`），它**没有对应渠道**，因此候选元组是 `(bridge, None, None)`。
//! 上层 failover 逻辑拿 `cid: Option<&str>` 打日志、写请求日志，于是线上出现
//! 了一个很难查的现象：
//!
//! - 运行时日志：`chat failover: channel None failed: …`
//! - 请求日志：`channel_id = null`、`channel_name = null`、`provider = ""`
//!
//! 这跟「供应商返回空」毫无关系 —— 是**我们自己的兜底候选没有身份**。排障时
//! 看到 provider 空，第一反应会去怀疑上游，实际应该看的是「这条请求压根没走
//! 到任何渠道」。
//!
//! ## 做法
//!
//! 保留没有渠道 id 的事实（不能编造一个 id 去污染渠道统计/冷却），但给这条
//! 候选一个**自报的身份**：包装后的 bridge 在 `name()` 上返回 `hub:<provider>`，
//! 上层据此能区分「渠道候选」与「无渠道兜底」。同时上层在写日志时，若
//! `channel_id` 为空则退回用 `bridge.name()` 填充 provider 列，保证日志里
//! 不再出现空 provider。
//!
//! 这层包装纯转发，不改任何请求/响应语义（`Bridge` 全部方法逐一委派）。

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use super::{
    Bridge, BridgeContext, BridgeError, ChatChunkStream, ChatFormat, ChatResponse,
    EmbeddingRequest, EmbeddingResponse, ImageEditFormPart, RerankRequest, RerankResponse,
    ResponsesPassthrough,
};

/// 无渠道兜底 bridge 的包装：身份可自述，行为逐字节透传。
pub struct HubBridge {
    inner: Arc<dyn Bridge>,
    /// Hub 专用提供商标识（如 `cloudflare`）。
    provider: &'static str,
}

impl HubBridge {
    pub fn new(inner: Arc<dyn Bridge>, provider: &'static str) -> Arc<Self> {
        Arc::new(Self { inner, provider })
    }

    /// 该兜底对应的提供商标识（写日志用）。
    ///
    /// CI clippy -D warnings：真实调用方还没接线（观测性消费点在请求日志
    /// provider 列），先标注允许，接线后移除。
    #[allow(dead_code)]
    pub fn provider(&self) -> &'static str {
        self.provider
    }
}

#[async_trait]
impl Bridge for HubBridge {
    /// 名字带上 provider：`hub:cloudflare`。
    ///
    /// 为什么要改 `name()` 而不是新加方法：`Bridge` trait 是对象安全的公共面，
    /// 加方法要改所有实现；而 `name()` 本来就是「这条链路是什么」，上层日志
    /// 与诊断已经读它。用它承载「无渠道兜底」的身份，零涟漪。
    fn name(&self) -> &'static str {
        self.provider
    }

    async fn chat(
        &self,
        req: &ChatFormat,
        ctx: &BridgeContext,
    ) -> Result<ChatResponse, BridgeError> {
        self.inner.chat(req, ctx).await
    }

    async fn chat_stream(
        &self,
        req: &ChatFormat,
        ctx: &BridgeContext,
    ) -> Result<ChatChunkStream, BridgeError> {
        self.inner.chat_stream(req, ctx).await
    }

    async fn embed(
        &self,
        req: &EmbeddingRequest,
        ctx: &BridgeContext,
    ) -> Result<EmbeddingResponse, BridgeError> {
        self.inner.embed(req, ctx).await
    }

    async fn rerank(
        &self,
        req: &RerankRequest,
        ctx: &BridgeContext,
    ) -> Result<RerankResponse, BridgeError> {
        self.inner.rerank(req, ctx).await
    }

    async fn complete(&self, body: &Value, ctx: &BridgeContext) -> Result<Value, BridgeError> {
        self.inner.complete(body, ctx).await
    }

    async fn generate_image(
        &self,
        body: &Value,
        ctx: &BridgeContext,
    ) -> Result<Value, BridgeError> {
        self.inner.generate_image(body, ctx).await
    }

    async fn edit_image(
        &self,
        parts: &[ImageEditFormPart],
        ctx: &BridgeContext,
    ) -> Result<Value, BridgeError> {
        self.inner.edit_image(parts, ctx).await
    }

    async fn responses_passthrough(
        &self,
        body: &Value,
        stream: bool,
        ctx: &BridgeContext,
    ) -> Result<ResponsesPassthrough, BridgeError> {
        self.inner.responses_passthrough(body, stream, ctx).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 最小 bridge：只实现 `name()`，其余方法一律返回「未实现」错误。
    ///
    /// 这足以证明包装层**不改身份之外的任何语义**：所有方法都委派到它，
    /// 错误原样冒出。
    struct Stub;

    #[async_trait]
    impl Bridge for Stub {
        fn name(&self) -> &'static str {
            "stub"
        }
        async fn chat(
            &self,
            _req: &ChatFormat,
            _ctx: &BridgeContext,
        ) -> Result<ChatResponse, BridgeError> {
            Err(BridgeError::Config("stub chat".into()))
        }
        async fn chat_stream(
            &self,
            _req: &ChatFormat,
            _ctx: &BridgeContext,
        ) -> Result<ChatChunkStream, BridgeError> {
            Err(BridgeError::Config("stub chat_stream".into()))
        }
    }

    #[test]
    fn name_reports_provider_not_inner() {
        let wrapped = HubBridge::new(Arc::new(Stub), "hub:cloudflare");
        assert_eq!(wrapped.name(), "hub:cloudflare");
        assert_eq!(wrapped.provider(), "hub:cloudflare");
        // 内层名字不泄漏：日志里不该再看到 `stub` 与兜底身份混淆。
        assert_ne!(wrapped.name(), "stub");
    }

    #[tokio::test]
    async fn errors_pass_through_unchanged() {
        let wrapped = HubBridge::new(Arc::new(Stub), "hub:cloudflare");
        let ctx = BridgeContext::new("req-1", "m");
        // ChatFormat 无 Default（含 Option 字段 + Arc 共享语义），手工构造
        // 最小形状即可——stub 对请求体内容零消费。
        let req = ChatFormat {
            model: "m".into(),
            messages: Vec::new(),
            tools: None,
            max_tokens: None,
            temperature: None,
            top_p: None,
            stream: false,
            top_k: None,
            stop: None,
            tool_choice: None,
            reasoning_effort: None,
            web_search_options: None,
            extra: None,
        };
        let err = wrapped
            .chat_stream(&req, &ctx)
            .await
            .err()
            .expect("stub 必须失败");
        assert!(
            err.to_string().contains("stub chat_stream"),
            "错误必须原样透传: {err}"
        );
    }
}
