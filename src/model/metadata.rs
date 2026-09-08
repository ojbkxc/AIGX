//! 模型元信息注册表（P1「模型元信息同步」）—— owned_by / 上下文长度 / 能力标签。
//!
//! 数据来源分三层（优先级从低到高）：
//! 1. 内置推断：模型名前缀 → 归属厂商；常见模型 → 上下文长度；名字特征 → 能力标签
//! 2. 渠道类型：list_models 聚合时由渠道的 channel_type 推导 owned_by（覆盖名字推断）
//! 3. 管理员覆盖：FileStore `model_meta:{name}` 持久化（显式覆盖前两层）
//!
//! 百年工程红线：不引 crate；上下文长度表为静态内置 + 按需覆盖，
//! 不做联网同步（上游 /models 端点不含上下文长度字段）。

use anyhow::Result;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

use crate::storage::FileStore;

/// 单模型元信息。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelMetadata {
    /// 归属方（openai / anthropic / google / cloudflare / …）
    pub owned_by: String,
    /// 上下文长度（token 数；未知为 None）
    #[serde(default)]
    pub context_length: Option<u32>,
    /// 能力标签（chat / embedding / rerank / image / audio）
    #[serde(default)]
    pub capabilities: Vec<String>,
}

// ── 内置推断表 ──────────────────────────────────────────────────────

/// 模型名前缀 → 归属厂商。
///
/// 网关语义下的「名字归谁」只用于 /v1/models 的 owned_by 展示，
/// 不参与路由决策（路由看渠道 models 声明）。
const KNOWN_PREFIXES: &[(&str, &str)] = &[
    ("gpt-", "openai"),
    ("o1", "openai"),
    ("o3", "openai"),
    ("o4", "openai"),
    ("chatgpt", "openai"),
    ("davinci", "openai"),
    ("text-embedding", "openai"),
    ("whisper", "openai"),
    ("tts-", "openai"),
    ("dall-e", "openai"),
    ("claude-", "anthropic"),
    ("gemini-", "google"),
    ("glm-", "zhipu"),
    ("deepseek", "deepseek"),
    ("qwen", "alibaba"),
    ("llama", "meta"),
    ("mistral", "mistral"),
    ("mixtral", "mistral"),
    ("kimi", "moonshot"),
    ("ernie", "baidu"),
    ("hunyuan", "tencent"),
    ("minimax", "minimax"),
    ("@cf/", "cloudflare"),
    ("@hf/", "huggingface"),
];

/// 常见模型上下文长度（token）。
///
/// 只收录确定性高的主流型号；不在表内的模型 context_length 为 None，
/// 管理员可通过覆盖机制补齐。前缀匹配取最长命中。
const KNOWN_CONTEXT_LENGTHS: &[(&str, u32)] = &[
    // OpenAI
    ("gpt-4o", 128000),
    ("gpt-4-turbo", 128000),
    ("gpt-4.1", 1047576),
    ("gpt-4", 8192),
    ("gpt-3.5-turbo", 16385),
    ("o1", 200000),
    ("o3", 200000),
    ("o4-mini", 200000),
    // Anthropic
    ("claude-3-5-sonnet", 200000),
    ("claude-3-5-haiku", 200000),
    ("claude-3-opus", 200000),
    ("claude-3-sonnet", 200000),
    ("claude-3-haiku", 200000),
    ("claude-sonnet-4", 200000),
    ("claude-opus-4", 200000),
    // Google
    ("gemini-1.5-pro", 2097152),
    ("gemini-1.5-flash", 1048576),
    ("gemini-2.0-flash", 1048576),
    ("gemini-2.5-pro", 1048576),
    ("gemini-2.5-flash", 1048576),
    // DeepSeek
    ("deepseek-chat", 65536),
    ("deepseek-reasoner", 65536),
    ("deepseek-r1", 65536),
    // 智谱
    ("glm-4", 128000),
    ("glm-5", 128000),
];

/// 模型名特征 → 能力标签。
const CAPABILITY_HINTS: &[(&str, &str)] = &[
    ("embedding", "embedding"),
    ("embed", "embedding"),
    ("rerank", "rerank"),
    ("dall-e", "image"),
    ("image", "image"),
    ("sd3", "image"),
    ("stable-diffusion", "image"),
    ("flux", "image"),
    ("whisper", "audio"),
    ("tts", "audio"),
    ("speech", "audio"),
];

/// 渠道类型 → owned_by（list_models 聚合时的渠道维度覆盖）。
pub fn owned_by_for_channel_type(ct: &str) -> Option<&'static str> {
    match ct {
        "cloudflare" => Some("cloudflare"),
        "anthropic" => Some("anthropic"),
        "gemini" => Some("google"),
        "zai" => Some("zhipu"),
        // openai_compatible 渠道上游混杂（DeepSeek/OpenRouter/…），
        // 不能一概归 openai——回落到模型名推断
        _ => None,
    }
}

/// 按模型名推断归属厂商（无匹配返回 None）。
pub fn infer_owned_by(model: &str) -> Option<&'static str> {
    let lower = model.to_lowercase();
    // 精确前缀匹配（KNOWN_PREFIXES 按长度降序保证 gpt-4.1 优先于 gpt-4 不适用——
    // owned_by 推断无此精度需求，首个命中即可；但 o1/o3/o4 系列需防误命中
    // 如 "o1xxx" 之外的名字，故放在较长前缀之后兜底）
    KNOWN_PREFIXES
        .iter()
        .find(|(p, _)| lower.starts_with(p))
        .map(|(_, owner)| *owner)
}

/// 按模型名查内置上下文长度（最长前缀命中优先）。
pub fn builtin_context_length(model: &str) -> Option<u32> {
    let lower = model.to_lowercase();
    KNOWN_CONTEXT_LENGTHS
        .iter()
        .filter(|(p, _)| lower.starts_with(p))
        .max_by_key(|(p, _)| p.len())
        .map(|(_, n)| *n)
}

/// 按模型名推断能力标签（未命中时默认 chat）。
pub fn infer_capabilities(model: &str) -> Vec<String> {
    let lower = model.to_lowercase();
    let mut caps: Vec<String> = CAPABILITY_HINTS
        .iter()
        .filter(|(p, _)| lower.contains(p))
        .map(|(_, c)| c.to_string())
        .collect();
    caps.dedup();
    if caps.is_empty() {
        caps.push("chat".to_string());
    }
    caps
}

// ── ModelMetadataRegistry ───────────────────────────────────────────

/// 元信息注册表：内置推断 + 管理员覆盖（FileStore 持久化）。
pub struct ModelMetadataRegistry {
    overrides: RwLock<HashMap<String, ModelMetadata>>,
    store: Arc<FileStore>,
}

impl ModelMetadataRegistry {
    pub fn new(store: Arc<FileStore>) -> Self {
        Self {
            overrides: RwLock::new(HashMap::new()),
            store,
        }
    }

    /// 从存储加载覆盖项。
    pub fn load(&self) -> Result<()> {
        let keys = self.store.list("model_meta:")?;
        let mut overrides = HashMap::new();
        for key in keys {
            let name = key.trim_start_matches("model_meta:").to_string();
            if name.is_empty() {
                continue;
            }
            if let Some(meta) = self.store.get::<ModelMetadata>(&key)? {
                overrides.insert(name, meta);
            }
        }
        *self.overrides.write() = overrides;
        Ok(())
    }

    fn persist(&self, model: &str) -> Result<()> {
        let meta = self
            .overrides
            .read()
            .get(model)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("metadata for {model} missing"))?;
        self.store.put(&format!("model_meta:{model}"), &meta)?;
        Ok(())
    }

    /// 查询某模型的元信息（覆盖 > 内置推断）。
    ///
    /// `channel_owned_by`：渠道维度推导的归属（优先于名字推断）；
    /// 覆盖项存在时整体以覆盖为准。
    pub fn get(&self, model: &str, channel_owned_by: Option<&str>) -> ModelMetadata {
        if let Some(m) = self.overrides.read().get(model) {
            return m.clone();
        }
        let owned_by = channel_owned_by
            .or_else(|| infer_owned_by(model))
            .unwrap_or("aigx")
            .to_string();
        ModelMetadata {
            owned_by,
            context_length: builtin_context_length(model),
            capabilities: infer_capabilities(model),
        }
    }

    /// 设置/更新覆盖项。
    pub fn set_override(&self, model: String, meta: ModelMetadata) -> Result<()> {
        self.overrides.write().insert(model.clone(), meta);
        self.persist(&model)
    }

    /// 删除覆盖项（回落到内置推断）。
    pub fn remove_override(&self, model: &str) -> Result<()> {
        if self.overrides.write().remove(model).is_some() {
            self.store.delete(&format!("model_meta:{model}"))?;
        }
        Ok(())
    }

    /// 当前全部覆盖项。
    pub fn all_overrides(&self) -> HashMap<String, ModelMetadata> {
        self.overrides.read().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infer_owned_by_common_families() {
        assert_eq!(infer_owned_by("gpt-4o"), Some("openai"));
        assert_eq!(
            infer_owned_by("GPT-4o".to_uppercase().as_str()),
            Some("openai")
        );
        assert_eq!(infer_owned_by("claude-3-5-sonnet"), Some("anthropic"));
        assert_eq!(infer_owned_by("gemini-2.0-flash"), Some("google"));
        assert_eq!(infer_owned_by("@cf/meta/llama-3"), Some("cloudflare"));
        assert_eq!(infer_owned_by("glm-4-plus"), Some("zhipu"));
        assert_eq!(infer_owned_by("totally-unknown-model"), None);
    }

    #[test]
    fn context_length_prefix_priority() {
        // gpt-4.1 有专属条目（1M），不被 gpt-4 的 8192 覆盖
        assert_eq!(builtin_context_length("gpt-4.1"), Some(1047576));
        assert_eq!(builtin_context_length("gpt-4"), Some(8192));
        assert_eq!(builtin_context_length("gpt-4o-mini"), Some(128000));
        assert_eq!(
            builtin_context_length("claude-3-5-sonnet-20241022"),
            Some(200000)
        );
        assert_eq!(builtin_context_length("unknown-model"), None);
    }

    #[test]
    fn capabilities_default_chat_and_hints() {
        assert!(infer_capabilities("gpt-4o").contains(&"chat".to_string()));
        assert!(infer_capabilities("text-embedding-3-small").contains(&"embedding".to_string()));
        assert!(infer_capabilities("bge-reranker-v2").contains(&"rerank".to_string()));
        assert!(infer_capabilities("dall-e-3").contains(&"image".to_string()));
    }

    #[test]
    fn channel_type_overrides_name_inference() {
        // openai_compatible 渠道不强行归属 → 名字推断兜底
        assert_eq!(owned_by_for_channel_type("openai_compatible"), None);
        assert_eq!(owned_by_for_channel_type("cloudflare"), Some("cloudflare"));
        assert_eq!(owned_by_for_channel_type("zai"), Some("zhipu"));
    }

    #[test]
    fn registry_override_beats_inference() {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("aigx-meta-test-{}-{seq}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Arc::new(FileStore::new(dir));

        let reg = ModelMetadataRegistry::new(store);
        // 内置推断路径
        let m = reg.get("gpt-4o", Some("cloudflare"));
        assert_eq!(m.owned_by, "cloudflare");
        assert_eq!(m.context_length, Some(128000));

        // 覆盖路径
        reg.set_override(
            "gpt-4o".into(),
            ModelMetadata {
                owned_by: "my-org".into(),
                context_length: Some(2000),
                capabilities: vec!["chat".into()],
            },
        )
        .unwrap();
        let m = reg.get("gpt-4o", Some("cloudflare"));
        assert_eq!(m.owned_by, "my-org");
        assert_eq!(m.context_length, Some(2000));

        // 删除覆盖后回落
        reg.remove_override("gpt-4o").unwrap();
        let m = reg.get("gpt-4o", None);
        assert_eq!(m.owned_by, "openai");
    }

    #[test]
    fn override_persists_across_registry_reload() {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(1);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("aigx-meta-persist-{}-{seq}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Arc::new(FileStore::new(dir));

        let reg = ModelMetadataRegistry::new(store.clone());
        reg.set_override(
            "gemini-2.5-pro".into(),
            ModelMetadata {
                owned_by: "my-gemini".into(),
                context_length: Some(2_000_000),
                capabilities: vec!["chat".into(), "vision".into()],
            },
        )
        .unwrap();

        // 模拟重启：新注册表从同一 FileStore load
        let reg2 = ModelMetadataRegistry::new(store);
        reg2.load().unwrap();
        let m = reg2.get("gemini-2.5-pro", Some("google"));
        assert_eq!(m.owned_by, "my-gemini");
        assert_eq!(m.context_length, Some(2_000_000));
        assert_eq!(m.capabilities, vec!["chat".to_string(), "vision".to_string()]);
    }
}
