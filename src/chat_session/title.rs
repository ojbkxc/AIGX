//! LLM 自动标题——异步、可开关、失败静默降级（#37）。
//!
//! 触发：会话首条用户消息落库后（append / replace 两处），HTTP handler
//! 不等待、同步返回；LLM 调用在 `tokio::spawn` 里走**进程内自环**
//! （复用 `agent::llm::chat_once_as`：渠道调度 + 熔断 + failover 同链），
//! 不经 HTTP 端口、不占热路径。
//!
//! 覆盖纪律：只接手 `title_source="auto"`（或缺省旧数据）的会话；
//! 飞行中用户 `rename` 过则写回守卫拒绝（`try_set_llm_title` 双检），
//! 用户标题永不被覆盖。失败静默保持截断标题（`tracing::debug`）。
//!
//! 计费口径：与 `[agent]` 运维自环同口径——记 RequestLog
//! （key_id="chat_title"，用量日志页可见），input/output token 如实记账，
//! `cost=0` **不计费**。理由：标题是平台增值功能，调用发生在用户 API key
//! 之外（聊天页走会话认证，无 key 身份可扣），单次输入 ≪1K token，
//! 销售成本可忽略；与 agent「记日志不扣销售成本」的先例一致。

use crate::agent::llm::{chat_once_as, system_message, user_message};
use crate::api::openai::AppState;
use crate::chat_session::ChatSessionStore;
use crate::config::AgentConfig;

/// 标题生成系统提示词：只输出标题本身，无引号无标点尾巴。
const TITLE_SYSTEM_PROMPT: &str = "你是会话标题生成器。根据用户消息概括核心话题，\
生成一个不超过 20 个字的简短中文标题。只输出标题本身：不要引号、不要句号等结尾标点、\
不要任何解释或前后缀。";

/// 清洗模型输出：取首行、去成对引号与空白，截 30 字符（与截断命名同上限）。
/// 输出为空返回 None（保持原标题）。
fn clean_title(raw: &str) -> Option<String> {
    let first_line = raw.trim().lines().next().unwrap_or("");
    let unquoted = first_line.trim_matches(|c| {
        matches!(
            c,
            '"' | '\'' | '`' | '“' | '”' | '‘' | '’' | '「' | '」' | '『' | '』'
        )
    });
    let t: String = unquoted.trim().chars().take(30).collect();
    if t.is_empty() {
        None
    } else {
        Some(t)
    }
}

/// 构造自环调用的 AgentConfig。`chat_once` 实际只用 model / channel /
/// reasoning_effort 三个字段，其余填无害值（穷举字面量：未来 AgentConfig
/// 加字段时此处编译错，是期望的同步点）。
fn title_agent_config(model: String, channel: String) -> AgentConfig {
    AgentConfig {
        enabled: true,
        model,
        channel,
        max_turns: 1,
        approval_timeout_secs: 30,
        max_history: 1,
        reasoning_effort: None,
        system_prompt: String::new(),
        disabled_tools: Vec::new(),
        // 标题生成是后台附加任务：失败静默、保留原标题即可，不值得为它
        // 等整轮退避（用户不会等标题）。1 = 只跑一轮候选遍历，不重试。
        transient_retry_rounds: 1,
        retry_interval_ms: 0,
    }
}

/// 异步生成并写回标题。
///
/// 调用方需先 `begin_title_task` 占位（同会话去重），本函数结束（成功/
/// 失败/早退）时释放；失败静默（debug 日志），截断标题兜底不受影响。
pub fn spawn_auto_title(state: AppState, user_id: String, session_id: String, sample: String) {
    tokio::spawn(async move {
        let result = generate(&state, &user_id, &session_id, &sample).await;
        state.chat_sessions.end_title_task(&user_id, &session_id);
        if let Err(e) = result {
            tracing::debug!("chat auto-title failed (silently keep truncated title): {e:#}");
        }
    });
}

/// 生成主体：开关 → 双检标题状态 → 自环调用 → 清洗写回。
async fn generate(
    state: &AppState,
    user_id: &str,
    session_id: &str,
    sample: &str,
) -> anyhow::Result<()> {
    let cfg = state.config_manager.get().await;
    if !cfg.chat.auto_title {
        return Ok(()); // 开关关闭：静默早退
    }
    // 双检：飞行中用户 rename / LLM 已写过 → 放弃（不覆盖）
    let Some(s) = state.chat_sessions.get(user_id, session_id)? else {
        return Ok(());
    };
    if !ChatSessionStore::title_llm_eligible(&s) {
        return Ok(());
    }
    let sample: String = sample
        .trim()
        .chars()
        .take(cfg.chat.title_prompt_chars)
        .collect();
    if sample.is_empty() {
        return Ok(());
    }
    let messages = vec![
        system_message(TITLE_SYSTEM_PROMPT.to_string()),
        user_message(sample),
    ];
    let agent_cfg =
        title_agent_config(cfg.chat.title_model.clone(), cfg.chat.title_channel.clone());
    let (resp, _upstream, _channel_id) =
        chat_once_as(state, &agent_cfg, messages, None, "chat_title").await?;
    let content = resp.message.content.unwrap_or_default();
    if let Some(title) = clean_title(&content) {
        // 写回守卫在 store 内再检一次（竞态最后防线）
        state
            .chat_sessions
            .try_set_llm_title(user_id, session_id, &title)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::clean_title;

    #[test]
    fn clean_title_strips_quotes_and_limits_length() {
        assert_eq!(
            clean_title("  帮我写个爬虫  "),
            Some("帮我写个爬虫".to_string())
        );
        assert_eq!(
            clean_title("「帮我写个爬虫」"),
            Some("帮我写个爬虫".to_string())
        );
        assert_eq!(
            clean_title("\u{201c}帮我写个爬虫\u{201d}\n"),
            Some("帮我写个爬虫".to_string())
        );
        // 超长截 30
        let long = "长".repeat(50);
        assert_eq!(clean_title(&long), Some("长".repeat(30)));
        // 多行只取首行
        assert_eq!(clean_title("标题一\n标题二"), Some("标题一".to_string()));
        assert_eq!(clean_title("   \n "), None);
        assert_eq!(clean_title(""), None);
    }
}
