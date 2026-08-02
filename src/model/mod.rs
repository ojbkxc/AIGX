use anyhow::Result;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;

use crate::storage::FileStore;

/// 默认模型映射表（客户端模型名 → Cloudflare 模型名）
pub fn default_model_map() -> HashMap<String, String> {
    let mut m = HashMap::new();
    // 对话 / 文本生成模型
    m.insert("glm-5.2".into(), "@cf/zai-org/glm-5.2".into());
    m.insert("glm-4.7-flash".into(), "@cf/zai-org/glm-4.7-flash".into());
    m.insert("kimi-k2.7-code".into(), "@cf/moonshotai/kimi-k2.7-code".into());
    m.insert("kimi-k2.6".into(), "@cf/moonshotai/kimi-k2.6".into());
    m.insert("gemma-4-26b-a4b-it".into(), "@cf/google/gemma-4-26b-a4b-it".into());
    m.insert("nemotron-3-120b-a12b".into(), "@cf/nvidia/nemotron-3-120b-a12b".into());
    m.insert("gpt-oss-20b".into(), "@cf/openai/gpt-oss-20b".into());
    m.insert("gpt-oss-120b".into(), "@cf/openai/gpt-oss-120b".into());
    m.insert("llama-3.1-8b".into(), "@cf/meta/llama-3.1-8b-instruct".into());
    m.insert("llama-3.3-70b".into(), "@cf/meta/llama-3.3-70b-instruct-fp8-fast".into());
    m.insert("llama-4-scout".into(), "@cf/meta/llama-4-scout-17b-16e-instruct".into());
    m.insert("llama-4-maverick".into(), "@cf/meta/llama-4-maverick-17b-128e-instruct".into());
    m.insert("deepseek-r1-distill".into(), "@cf/deepseek-ai/deepseek-r1-distill-qwen-32b".into());
    m.insert("deepseek-v3".into(), "@cf/deepseek-ai/deepseek-v3-0324".into());
    m.insert("deepseek-r1-distill-qwen-32b".into(), "@cf/deepseek-ai/deepseek-r1-distill-qwen-32b".into());
    m.insert("qwen-2.5-72b".into(), "@cf/qwen/qwen2.5-72b-instruct".into());
    m.insert("qwen-2.5-coder-32b".into(), "@cf/qwen/qwen2.5-coder-32b-instruct".into());
    m.insert("qwen-2.5-7b".into(), "@cf/qwen/qwen2.5-7b-instruct-awq".into());
    m.insert("qwen1.5-14b".into(), "@cf/qwen/qwen1.5-14b-instruct".into());
    m.insert("qwen1.5-7b".into(), "@cf/qwen/qwen1.5-7b-instruct".into());
    m.insert("mistral-7b".into(), "@cf/mistral/mistral-7b-instruct-v0.1".into());
    m.insert("deepseek-coder-6.7b".into(), "@cf/deepseek-ai/deepseek-coder-6.7b-instruct".into());
    m.insert("llama-3.2-3b".into(), "@cf/meta/llama-3.2-3b-instruct".into());
    m.insert("llama-3.2-1b".into(), "@cf/meta/llama-3.2-1b-instruct".into());
    m.insert("codellama-34b".into(), "@cf/codellama/codellama-34b-instruct".into());
    m.insert("codellama-13b".into(), "@cf/codellama/codellama-13b-instruct".into());
    m.insert("codellama-7b".into(), "@cf/codellama/codellama-7b-instruct".into());
    m.insert("mixtral-8x7b".into(), "@cf/mistral/mixtral-8x7b-instruct".into());
    m.insert("gemma-4".into(), "@cf/google/gemma-4".into());
    m.insert("gemma-4-9b-it".into(), "@cf/google/gemma-4-9b-it".into());
    m.insert("gemma-4-27b-it".into(), "@cf/google/gemma-4-27b-it".into());
    m.insert("gemma-2-27b".into(), "@cf/google/gemma-2-27b-it".into());
    m.insert("gemma-2-9b".into(), "@cf/google/gemma-2-9b-it".into());
    m.insert("gemma-2-2b".into(), "@cf/google/gemma-2-2b-it".into());
    m.insert("phi-3-mini".into(), "@cf/microsoft/phi-3-mini-4k-instruct".into());
    m.insert("phi-2".into(), "@cf/microsoft/phi-2".into());
    m.insert("internlm2-7b".into(), "@cf/internlm/internlm2-7b-instruct".into());

    // 向量嵌入（Embeddings）模型
    m.insert("bge-m3".into(), "@cf/baai/bge-m3".into());
    m.insert("bge-base-en-v1.5".into(), "@cf/baai/bge-base-en-v1.5".into());
    m.insert("bge-large-en".into(), "@cf/baai/bge-large-en-v1.5".into());
    m.insert("qwen3-embedding".into(), "@cf/qwen/qwen3-embedding".into());
    m.insert("qwen3-embedding-0.6b".into(), "@cf/qwen/qwen3-embedding-0.6b".into());
    m.insert("embeddinggemma-300m".into(), "@cf/google/embeddinggemma-300m".into());

    // 多模态 / 图片生成
    m.insert("llava-1.5".into(), "@cf/llava-hf/llava-1.5-7b-hf".into());
    m.insert("llava-1.5-7b".into(), "@cf/llava-hf/llava-1.5-7b-hf".into());
    m.insert("flux-1-schnell".into(), "@cf/black-forest-labs/flux-1-schnell".into());
    m.insert("flux-1-dev".into(), "@cf/black-forest-labs/flux-1-dev".into());
    m.insert("sdxl".into(), "@cf/stabilityai/stable-diffusion-xl-base-1.0".into());

    // 语音识别（Whisper）
    m.insert("whisper".into(), "@cf/openai/whisper".into());
    m.insert("whisper-1".into(), "@cf/openai/whisper".into());
    m.insert("whisper-tiny-en".into(), "@cf/openai/whisper-tiny-en".into());
    m.insert("whisper-large-v3-turbo".into(), "@cf/openai/whisper-large-v3-turbo".into());

    // 视觉模型
    m.insert("moondream3.1-9B-A2B".into(), "@cf/moondream/moondream3.1-9B-A2B".into());

    // 文本转语音（TTS）
    m.insert("tts".into(), "@cf/myshell-ai/tts".into());

    m
}

/// 兜底模型
const DEFAULT_FALLBACK: &str = "@cf/zai-org/glm-4.7-flash";

/// 模型映射管理器
pub struct ModelMapper {
    custom_map: RwLock<HashMap<String, String>>,
    store: Arc<FileStore>,
}

impl ModelMapper {
    pub fn new(store: Arc<FileStore>) -> Self {
        Self {
            custom_map: RwLock::new(HashMap::new()),
            store,
        }
    }

    /// 从存储加载自定义映射
    pub fn load(&self) -> Result<()> {
        let raw = self.store.get::<HashMap<String, String>>("model_custom_map")?;
        if let Some(map) = raw {
            let mut custom = self.custom_map.write();
            custom.clear();
            custom.extend(map);
        }
        Ok(())
    }

    /// 保存自定义映射到存储
    pub fn save(&self) -> Result<()> {
        let custom = self.custom_map.read().clone();
        self.store.put("model_custom_map", &custom)
    }

    /// 解析模型名：先查自定义映射，再查默认映射，否则使用兜底模型
    pub fn resolve(&self, model: &str) -> String {
        if model.is_empty() {
            return DEFAULT_FALLBACK.to_string();
        }
        // 已经是 CF 完整模型名，直接返回
        if model.starts_with("@cf/") {
            return model.to_string();
        }
        // 查自定义映射
        let custom = self.custom_map.read();
        if let Some(mapped) = custom.get(model) {
            return mapped.clone();
        }
        drop(custom);
        // 查默认映射
        let defaults = default_model_map();
        if let Some(mapped) = defaults.get(model) {
            return mapped.clone();
        }
        // 兜底
        DEFAULT_FALLBACK.to_string()
    }

    /// 获取所有映射（合并默认 + 自定义）
    pub fn all_mappings(&self) -> HashMap<String, String> {
        let mut merged = default_model_map();
        let custom = self.custom_map.read();
        merged.extend(custom.clone());
        merged
    }

    /// 获取自定义映射
    pub fn custom_mappings(&self) -> HashMap<String, String> {
        self.custom_map.read().clone()
    }

    /// 设置自定义映射
    pub fn set_custom(&self, source: String, target: String) -> Result<()> {
        self.custom_map.write().insert(source, target);
        self.save()
    }

    /// 删除自定义映射
    pub fn remove_custom(&self, source: &str) -> Result<()> {
        self.custom_map.write().remove(source);
        self.save()
    }

    /// 重置为默认映射（清除所有自定义映射）
    pub fn reset(&self) -> Result<()> {
        self.custom_map.write().clear();
        self.save()
    }
}