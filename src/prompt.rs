//! 系统提示词注入 — 对照 Agent2API「设置 → 网关 → 系统提示词」（core/prompt.rs）。
//!
//! 三种模式（`passthrough` / `custom` / `append`）：
//! - `passthrough`：透传客户端 system（默认，行为与引入本模块前逐字相同）
//! - `custom`：删除 messages 里所有 system 消息，头部插入网关提示词
//! - `append`：在开头连续的 system 块之后追加一条网关提示词，既有消息不动
//!
//! AIGX 数据面在 `ChatFormat`（归一化 ChatMessage 列表）构造后调用
//! [`apply_prompt`]；管理面经 `/api/gateway/prompt` 读写（FileStore 键
//! `gateway_prompt`），保存即生效。
//!
//! 简化口径（相对 A2A）：不做逐 provider 覆盖、不做提示词文件来源三档、
//! 不做降级中性提示词——AIGX 无该降级机制，只有全局一份文本 + 模式。

use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::bridge::{ChatMessage, Role};
use crate::storage::FileStore;

/// 持久化键。
pub const GATEWAY_PROMPT_KEY: &str = "gateway_prompt";

/// 提示词模式（字符串形态是 API 契约，见 `as_str`）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptMode {
    /// 透传客户端 system（默认）
    #[default]
    Passthrough,
    /// 用网关提示词替换客户端的 system 消息
    Custom,
    /// 在开头连续 system 块之后追加网关提示词
    Append,
}

impl PromptMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Passthrough => "passthrough",
            Self::Custom => "custom",
            Self::Append => "append",
        }
    }

    /// 解析：大小写不敏感 + 去首尾空白；空串按默认，非法值 None。
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "" | "passthrough" => Some(Self::Passthrough),
            "custom" => Some(Self::Custom),
            "append" => Some(Self::Append),
            _ => None,
        }
    }
}

/// 单个模型的提示词覆盖。
///
/// AIGX 适配形态：A2A 按 provider 覆盖（它一家一个上游），而 AIGX 的数据面
/// 入口在解析阶段只拿到客户端请求的**模型名**，provider 要等选路后才确定。
/// 因此这里按「模型名」覆盖 —— 语义等价（都是「这一路请求用另一套提示词」），
/// 且能在入口处直接判定。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelPromptOverride {
    #[serde(default)]
    pub mode: PromptMode,
    /// 该模型的提示词文本（空文本 = 该模型不做注入）
    #[serde(default)]
    pub text: String,
}

/// 网关提示词配置。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GatewayPrompt {
    /// 注入模式
    #[serde(default)]
    pub mode: PromptMode,
    /// 提示词文本（custom/append 模式生效；空文本等价 passthrough）
    #[serde(default)]
    pub text: String,
    /// 按模型覆盖（键 = 客户端请求里的模型名原文）：命中则整条替代全局
    /// mode/text，未命中回落全局。空表 = 全部走全局（默认路径）。
    #[serde(default)]
    pub models: std::collections::BTreeMap<String, ModelPromptOverride>,
    /// developer 角色兼容（对照 A2A developerRoleCompat，默认开）：
    /// OpenAI 新 SDK 用 `role:"developer"` 取代 system，多数上游只认
    /// system——开启时入口解析把 developer 归一为 system；关闭时按原
    /// 样丢弃未知角色（引入该开关前的行为）。
    #[serde(default = "default_developer_role_compat")]
    pub developer_role_compat: bool,
}

fn default_developer_role_compat() -> bool {
    true
}

impl Default for GatewayPrompt {
    fn default() -> Self {
        Self {
            mode: PromptMode::Passthrough,
            text: String::new(),
            models: std::collections::BTreeMap::new(),
            developer_role_compat: default_developer_role_compat(),
        }
    }
}

/// 进程级持有者：每请求快照（Arc clone），保存即生效。
pub struct GatewayPromptStore {
    current: parking_lot::RwLock<Arc<GatewayPrompt>>,
    store: Option<Arc<FileStore>>,
}

impl GatewayPromptStore {
    pub fn in_memory(cfg: GatewayPrompt) -> Self {
        Self {
            current: parking_lot::RwLock::new(Arc::new(cfg)),
            store: None,
        }
    }

    pub fn with_store(store: Arc<FileStore>) -> Self {
        let cfg = store
            .get::<GatewayPrompt>(GATEWAY_PROMPT_KEY)
            .ok()
            .flatten()
            .unwrap_or_default();
        Self {
            current: parking_lot::RwLock::new(Arc::new(cfg)),
            store: Some(store),
        }
    }

    pub fn snapshot(&self) -> Arc<GatewayPrompt> {
        self.current.read().clone()
    }

    pub fn update(&self, cfg: GatewayPrompt) -> anyhow::Result<GatewayPrompt> {
        if let Some(store) = &self.store {
            store.put(GATEWAY_PROMPT_KEY, &cfg)?;
        }
        *self.current.write() = Arc::new(cfg.clone());
        Ok(cfg)
    }
}

/// 对归一化消息列表应用提示词策略（数据面注入点）。
///
/// 返回 `Some(新列表)` = 改动过（调用方用返回值替换）；`None` = 不动
/// （passthrough 或文本为空——默认路径零开销，连 Vec 都不分配）。
pub fn apply_prompt(messages: &[ChatMessage], cfg: &GatewayPrompt) -> Option<Vec<ChatMessage>> {
    apply_prompt_for("", messages, cfg)
}

/// 按模型名应用提示词策略：先查该模型是否有专用覆盖，未命中回落全局。
///
/// 与 [`apply_prompt`] 是同一套逻辑，只是多了一层「按模型覆盖」的查表。
pub fn apply_prompt_for(
    model: &str,
    messages: &[ChatMessage],
    cfg: &GatewayPrompt,
) -> Option<Vec<ChatMessage>> {
    let (mode, text) = match cfg.models.get(model) {
        Some(over) => (over.mode, over.text.as_str()),
        None => (cfg.mode, cfg.text.as_str()),
    };
    apply_with(mode, text, messages)
}

/// 提示词策略本体：给定了模式与文本时如何改写消息列表。
fn apply_with(mode: PromptMode, text: &str, messages: &[ChatMessage]) -> Option<Vec<ChatMessage>> {
    if text.trim().is_empty() {
        return None;
    }
    let gateway = || ChatMessage {
        role: Role::System,
        content: Some(text.to_string()),
        content_blocks: None,
        name: None,
        tool_call_id: None,
        tool_calls: None,
        reasoning: None,
    };
    match mode {
        PromptMode::Passthrough => None,
        PromptMode::Custom => {
            // 替换：删掉全部 system 消息，头部插网关提示词
            let mut next = Vec::with_capacity(messages.len() + 1);
            next.push(gateway());
            next.extend(messages.iter().filter(|m| m.role != Role::System).cloned());
            Some(next)
        }
        PromptMode::Append => {
            // 追加：开头连续 system 块之后插入一条，既有消息不动
            let insert_at = messages
                .iter()
                .take_while(|m| m.role == Role::System)
                .count();
            let mut next = Vec::with_capacity(messages.len() + 1);
            next.extend_from_slice(&messages[..insert_at]);
            next.push(gateway());
            next.extend_from_slice(&messages[insert_at..]);
            Some(next)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sys(text: &str) -> ChatMessage {
        ChatMessage {
            role: Role::System,
            content: Some(text.to_string()),
            content_blocks: None,
            name: None,
            tool_call_id: None,
            tool_calls: None,
            reasoning: None,
        }
    }

    fn user(text: &str) -> ChatMessage {
        ChatMessage {
            role: Role::User,
            content: Some(text.to_string()),
            content_blocks: None,
            name: None,
            tool_call_id: None,
            tool_calls: None,
            reasoning: None,
        }
    }

    #[test]
    fn passthrough_and_empty_text_are_noop() {
        let msgs = vec![sys("client"), user("hi")];
        let cfg = GatewayPrompt::default();
        assert!(apply_prompt(&msgs, &cfg).is_none());
        // append 模式但文本为空 → 同样不动
        let cfg = GatewayPrompt {
            mode: PromptMode::Append,
            text: "   ".to_string(),
            ..GatewayPrompt::default()
        };
        assert!(apply_prompt(&msgs, &cfg).is_none());
    }

    #[test]
    fn custom_replaces_all_system() {
        let msgs = vec![sys("a"), user("q"), sys("b"), user("r")];
        let cfg = GatewayPrompt {
            mode: PromptMode::Custom,
            text: "GATEWAY".to_string(),
            ..GatewayPrompt::default()
        };
        let out = apply_prompt(&msgs, &cfg).unwrap();
        assert_eq!(out.len(), 3, "两条 system 都被删，插入一条网关的");
        assert_eq!(out[0].content.as_deref(), Some("GATEWAY"));
        assert_eq!(out[1].content.as_deref(), Some("q"));
        assert_eq!(out[2].content.as_deref(), Some("r"));
        assert!(out.iter().filter(|m| m.role == Role::System).count() == 1);
    }

    #[test]
    fn append_keeps_existing_and_inserts_after_leading_system() {
        let msgs = vec![sys("a"), sys("b"), user("q"), sys("late")];
        let cfg = GatewayPrompt {
            mode: PromptMode::Append,
            text: "GATEWAY".to_string(),
            ..GatewayPrompt::default()
        };
        let out = apply_prompt(&msgs, &cfg).unwrap();
        assert_eq!(out.len(), 5);
        // 位置：a, b, GATEWAY, q, late（中段/尾部的 system 原样保留）
        assert_eq!(out[0].content.as_deref(), Some("a"));
        assert_eq!(out[1].content.as_deref(), Some("b"));
        assert_eq!(out[2].content.as_deref(), Some("GATEWAY"));
        assert_eq!(out[3].content.as_deref(), Some("q"));
        assert_eq!(out[4].content.as_deref(), Some("late"));
    }

    #[test]
    fn append_without_leading_system_inserts_at_head() {
        let msgs = vec![user("q")];
        let cfg = GatewayPrompt {
            mode: PromptMode::Append,
            text: "G".to_string(),
            ..GatewayPrompt::default()
        };
        let out = apply_prompt(&msgs, &cfg).unwrap();
        assert_eq!(out[0].content.as_deref(), Some("G"));
        assert_eq!(out[1].content.as_deref(), Some("q"));
    }

    #[test]
    fn model_override_beats_global_and_falls_back_when_absent() {
        let msgs = vec![user("q")];
        let mut models = std::collections::BTreeMap::new();
        models.insert(
            "gpt-4o".to_string(),
            ModelPromptOverride {
                mode: PromptMode::Append,
                text: "PER-MODEL".to_string(),
            },
        );
        let cfg = GatewayPrompt {
            mode: PromptMode::Custom,
            text: "GLOBAL".to_string(),
            models,
            ..GatewayPrompt::default()
        };
        // 命中覆盖：用模型自己的文本
        let hit = apply_prompt_for("gpt-4o", &msgs, &cfg).unwrap();
        assert_eq!(hit[0].content.as_deref(), Some("PER-MODEL"));
        // 未命中：回落全局
        let miss = apply_prompt_for("claude-3-7", &msgs, &cfg).unwrap();
        assert_eq!(miss[0].content.as_deref(), Some("GLOBAL"));
    }

    #[test]
    fn model_override_with_empty_text_disables_injection_for_that_model() {
        let msgs = vec![user("q")];
        let mut models = std::collections::BTreeMap::new();
        models.insert(
            "silent-model".to_string(),
            ModelPromptOverride {
                mode: PromptMode::Append,
                text: "  ".to_string(),
            },
        );
        let cfg = GatewayPrompt {
            mode: PromptMode::Custom,
            text: "GLOBAL".to_string(),
            models,
            ..GatewayPrompt::default()
        };
        // 该模型文本为空 → 不做任何注入（即使全局是 custom）
        assert!(apply_prompt_for("silent-model", &msgs, &cfg).is_none());
        // 其它模型仍走全局
        assert!(apply_prompt_for("other", &msgs, &cfg).is_some());
    }

    #[test]
    fn mode_parse() {
        assert_eq!(PromptMode::parse(""), Some(PromptMode::Passthrough));
        assert_eq!(PromptMode::parse(" Custom "), Some(PromptMode::Custom));
        assert_eq!(PromptMode::parse("APPEND"), Some(PromptMode::Append));
        assert_eq!(PromptMode::parse("bogus"), None);
    }

    #[test]
    fn config_roundtrip_defaults() {
        let v: GatewayPrompt = serde_json::from_str(r#"{"text":"hi"}"#).unwrap();
        assert_eq!(v.mode, PromptMode::Passthrough);
        assert_eq!(v.text, "hi");
        // 旧数据（无 developer_role_compat 字段）回落默认开
        assert!(v.developer_role_compat);
    }

    #[test]
    fn developer_compat_can_be_disabled() {
        let v: GatewayPrompt = serde_json::from_str(r#"{"developer_role_compat":false}"#).unwrap();
        assert!(!v.developer_role_compat);
    }

    /// developer 角色归一（数据面入口解析层；A2A developerRoleCompat）。
    /// 兼容开关开：developer → system；关：按未知角色丢弃（老行为）。
    #[test]
    fn parse_messages_developer_role_compat() {
        use serde_json::json;
        let body = json!([
            { "role": "developer", "content": "You are a helpful assistant." },
            { "role": "user", "content": "hi" },
        ]);
        let raw = serde_json::to_value(&body).unwrap();

        // 开：归一为 system，内容逐字保留
        let msgs = crate::api::openai::parse_messages_with_dev(Some(&raw), true).unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, Role::System);
        assert_eq!(
            msgs[0].content.as_deref(),
            Some("You are a helpful assistant.")
        );
        assert_eq!(msgs[1].role, Role::User);

        // 关：developer 按未知角色丢弃（引入兼容前的行为）
        let msgs = crate::api::openai::parse_messages_with_dev(Some(&raw), false).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, Role::User);

        // system/tool 不受开关影响
        let body2 = json!([
            { "role": "system", "content": "s" },
            { "role": "tool", "content": "t" },
        ]);
        let raw2 = serde_json::to_value(&body2).unwrap();
        let msgs = crate::api::openai::parse_messages_with_dev(Some(&raw2), false).unwrap();
        assert_eq!(msgs.len(), 2);
    }
}
