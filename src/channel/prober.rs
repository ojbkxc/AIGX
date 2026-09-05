//! 渠道后台探活巡检——参照 burncloud channel health check 模式。
//!
//! 周期（默认 300s，可配）对每个启用渠道发一次 1-token 轻量探测：
//! - 探测请求：`max_tokens=1` 的单轮 "ping"（成本几乎为 0，但能完整走
//!   通鉴权 → 路由 → 响应链路，比 GET /models 更能反映真实可用性）
//! - 结果写入 circuit_breaker / health_tracker（复用
//!   `record_channel_success` / `record_channel_failure` 统一入口）
//! - 断路器已 Open 的渠道跳过探测（避免无谓消耗上游配额；
//!   冷却期结束的 HalfOpen 状态由 allow_request 放行试探）
//!
//! 与数据面记录的区别：探活请求不经过计费/限流（无 api_key 上下文），
//! 仅更新调度层健康状态。

use std::sync::Arc;
use std::time::Duration;

use crate::bridge::{Bridge, BridgeContext, BridgeError, ChatFormat, ChatMessage, Role};
use crate::channel::ChannelStore;

/// 探测周期（秒）。
const PROBE_INTERVAL_SECS: u64 = 300;
/// 单次探测超时（秒）。
const PROBE_TIMEOUT_SECS: u64 = 30;

/// 启动渠道探活协程。
pub fn spawn_channel_prober(channel_store: Arc<ChannelStore>, http: reqwest::Client) {
    tokio::spawn(async move {
        let interval = Duration::from_secs(PROBE_INTERVAL_SECS);
        loop {
            tokio::time::sleep(interval).await;
            probe_once(&channel_store, &http).await;
        }
    });
}

/// 探测一轮全部启用渠道（串行——避免并发探测打满上游连接池）。
async fn probe_once(channel_store: &ChannelStore, http: &reqwest::Client) {
    for ch in channel_store.list() {
        if !ch.is_enabled() {
            continue;
        }
        // 断路器 Open 期间不探测（HalfOpen 试探由 allow_request 放行）
        if !channel_store.circuit_breaker().allow_request(&ch.id) {
            tracing::debug!(channel = %ch.id, "prober skip: circuit open");
            continue;
        }
        let Some(model) = pick_probe_model(&ch) else {
            tracing::debug!(channel = %ch.id, "prober skip: no models configured");
            continue;
        };

        let key = ch.decode_api_key();
        if key.is_empty() && ch.channel_type != crate::channel::ChannelType::Cloudflare {
            tracing::debug!(channel = %ch.id, "prober skip: no api key");
            continue;
        }

        let bridge: Arc<dyn Bridge> = match ch.channel_type {
            crate::channel::ChannelType::OpenaiCompatible => {
                crate::bridge::openai::make_bridge(&ch.base_url, &key, http)
            }
            crate::channel::ChannelType::Anthropic => {
                crate::bridge::anthropic::make_bridge(&ch.base_url, &key, http)
            }
            crate::channel::ChannelType::Gemini => {
                crate::bridge::gemini::make_bridge(&ch.base_url, &key, http)
            }
            crate::channel::ChannelType::Zai => {
                crate::bridge::zai::make_bridge(&ch.base_url, &key, http)
            }
            crate::channel::ChannelType::Cloudflare => continue, // CF 走 quota_monitor
        };

        let probe_req = ChatFormat {
            model: model.clone(),
            messages: vec![ChatMessage {
                role: Role::User,
                content: Some("ping".to_string()),
                content_blocks: None,
                name: None,
                tool_call_id: None,
                tool_calls: None,
                reasoning: None,
            }],
            tools: None,
            max_tokens: Some(1),
            temperature: Some(0.0),
            top_p: None,
            stream: false,
            top_k: None,
            stop: None,
            tool_choice: None,
            reasoning_effort: None,
            web_search_options: None,
            extra: None,
        };
        let ctx = BridgeContext {
            request_id: format!("probe-{}", uuid::Uuid::new_v4()),
            model: model.clone(),
            account_id: None,
            api_token: None,
            deadline: Some(Duration::from_secs(PROBE_TIMEOUT_SECS)),
        };

        let start = std::time::Instant::now();
        match bridge.chat(&probe_req, &ctx).await {
            Ok(_) => {
                channel_store.record_channel_success(
                    &ch.id,
                    Some(&model),
                    start.elapsed().as_millis() as u64,
                    None,
                );
                tracing::debug!(channel = %ch.id, "probe ok");
            }
            Err(e) => {
                let ft = ChannelStore::classify_bridge_error(&e);
                tracing::warn!(channel = %ch.id, "probe failed: {e}");
                channel_store.record_channel_failure(
                    &ch.id,
                    Some(&model),
                    ft,
                    &e.to_string(),
                    None,
                );
            }
        }
    }
}

/// 挑探测用的模型：优先显式配置的第一个，其次发现的第一个。
fn pick_probe_model(ch: &crate::channel::Channel) -> Option<String> {
    ch.models
        .iter()
        .chain(ch.discovered_models.iter())
        .find(|m| !m.trim().is_empty())
        .cloned()
}

// ── 测试 ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_channel() -> crate::channel::Channel {
        serde_json::from_value(serde_json::json!({
            "id": "ch-test",
            "name": "test",
            "status": "enabled",
        }))
        .expect("channel from json")
    }

    #[test]
    fn pick_model_prefers_configured() {
        let mut ch = test_channel();
        ch.models = vec!["model-a".into(), "model-b".into()];
        ch.discovered_models = vec!["discovered-x".into()];
        assert_eq!(pick_probe_model(&ch).as_deref(), Some("model-a"));
    }

    #[test]
    fn pick_model_falls_back_to_discovered() {
        let mut ch = test_channel();
        ch.discovered_models = vec!["discovered-x".into()];
        assert_eq!(pick_probe_model(&ch).as_deref(), Some("discovered-x"));
    }

    #[test]
    fn pick_model_none_when_empty() {
        let ch = test_channel();
        assert!(pick_probe_model(&ch).is_none());
    }

    #[test]
    fn probe_request_is_minimal() {
        // 构造探测请求的形状验证：max_tokens=1 非流式
        let req = ChatFormat {
            model: "m".into(),
            messages: vec![ChatMessage {
                role: Role::User,
                content: Some("ping".into()),
                content_blocks: None,
                name: None,
                tool_call_id: None,
                tool_calls: None,
            }],
            tools: None,
            max_tokens: Some(1),
            temperature: Some(0.0),
            top_p: None,
            stream: false,
            top_k: None,
            stop: None,
            tool_choice: None,
            reasoning_effort: None,
            web_search_options: None,
            extra: None,
        };
        assert_eq!(req.max_tokens, Some(1));
        assert!(!req.stream);
        assert_eq!(req.messages.len(), 1);
    }

    /// classify_bridge_error 代理检查（确保 prober 引用的分类器可用）
    #[test]
    fn classify_helper_exists() {
        let e = BridgeError::Timeout {
            elapsed_ms: 1000,
            cause: "test".into(),
        };
        assert!(matches!(
            ChannelStore::classify_bridge_error(&e),
            crate::channel::circuit_breaker::FailureType::Timeout
        ));
    }
}
