//! 模型定价目录 — per-model 输入/输出分别计价 + 全局/分组倍率。
//!
//! 参照 new-api pricing.go 的数据模型（ModelName/ModelRatio/ModelPrice/CompletionRatio/
//! EnableGroup）与 controller/pricing.go 的定价目录设计。
//!
//! 计费公式：
//!   cost = (input_tokens * input_price + output_tokens * output_price) / 1000
//!          * model_ratio(model) * group_ratio(group)
//! price_type=token 按 token 计价；price_type=count 按次计价（input_price 即每次价格）。

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

use crate::storage::FileStore;
use tracing::error;

// ── ModelPrice ──────────────────────────────────────────────────────

/// 单模型定价条目。
///
/// 参照 new-api Pricing：input_price/output_price 对应每 1k token 价格（USD 或配额单位）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelPrice {
    /// 模型名（唯一键）
    pub model_name: String,
    /// 输入 token 每 1k 价格
    #[serde(default)]
    pub input_price: f64,
    /// 输出 token 每 1k 价格
    #[serde(default)]
    pub output_price: f64,
    /// 缓存 token 价格（可选）
    #[serde(default)]
    pub cache_price: Option<f64>,
    /// 计价类型：token（按量）或 count（按次）
    #[serde(default = "default_price_type")]
    pub price_type: String,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
}

fn default_price_type() -> String {
    "token".to_string()
}

impl ModelPrice {
    pub fn new(model_name: impl Into<String>, input_price: f64, output_price: f64) -> Self {
        let now = chrono::Utc::now().timestamp();
        Self {
            model_name: model_name.into(),
            input_price,
            output_price,
            cache_price: None,
            price_type: "token".to_string(),
            created_at: now,
            updated_at: now,
        }
    }
}

// ── RatioConfig ─────────────────────────────────────────────────────

/// 倍率配置 — 全局模型倍率 + 分组倍率。
///
/// 参照 new-api ratio_setting：model_ratio 控制模型整体计费倍率，
/// group_ratio 控制用户分组计费倍率，最终费用 = 基础费用 * model_ratio * group_ratio。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RatioConfig {
    /// 模型倍率：model_name -> ratio（缺省 1.0）
    #[serde(default)]
    pub model_ratio: HashMap<String, f64>,
    /// 分组倍率：group_name -> ratio（缺省 1.0）
    #[serde(default)]
    pub group_ratio: HashMap<String, f64>,
}

impl RatioConfig {
    pub fn model_ratio(&self, model: &str) -> f64 {
        self.model_ratio.get(model).copied().unwrap_or(1.0)
    }

    pub fn group_ratio(&self, group: &str) -> f64 {
        self.group_ratio.get(group).copied().unwrap_or(1.0)
    }
}

// ── PricingError ────────────────────────────────────────────────────

/// 计费错误：模型未配置定价。
///
/// B09：原先无定价条目时静默按 0 计费（免费放行），未配置价格的模型
/// 会产生免费用量；现改为显式错误，由调用方前置拦截
/// （`openai.rs::ensure_model_priced`）或记录告警。
#[derive(Debug, thiserror::Error)]
#[error("no price configured for model: {0}")]
pub struct PricingError(pub String);

// ── PricingStore ────────────────────────────────────────────────────

/// 定价存储 — 管理 ModelPrice 目录与 RatioConfig 倍率配置。
///
/// ModelPrice 以 `price:{model_name}` 为 key 存储支持快速按模型查找；
/// RatioConfig 以单 key `ratio_config` 存储。
pub struct PricingStore {
    prices: RwLock<HashMap<String, ModelPrice>>,
    ratios: RwLock<RatioConfig>,
    store: Arc<FileStore>,
}

const RATIO_KEY: &str = "ratio_config";
/// 首次启动种子定价的标志位（见 `ensure_default_prices`）。
const PRICING_SEEDED_KEY: &str = "pricing_seeded";

/// 内置默认定价目录（单位：每 1k token 美元，USD）。
///
/// 与 `model::default_model_map` 的默认模型对齐，价格参照 Cloudflare Workers AI
/// 公开价目（`@cf/` 免费额度模型按 0 计费；付费模型取近似官方价）。
/// 首次启动时若目录为空则自动种子（`ensure_default_prices`）。
fn default_prices() -> Vec<ModelPrice> {
    let mut p = |m: &str, i: f64, o: f64| ModelPrice::new(m, i, o);
    let mut c = |m: &str, price: f64| {
        let mut x = ModelPrice::new(m, price, 0.0);
        x.price_type = "count".to_string();
        x
    };
    vec![
        // 文本生成（对话 / 推理）
        p("glm-5.2", 0.0015, 0.0025),
        p("glm-4.7-flash", 0.0003, 0.0006),
        p("kimi-k2.7-code", 0.0004, 0.0009),
        p("kimi-k2.6", 0.0004, 0.0009),
        p("gemma-4-26b-a4b-it", 0.0002, 0.0004),
        p("nemotron-3-120b-a12b", 0.0002, 0.0004),
        p("gpt-oss-20b", 0.0002, 0.0004),
        p("gpt-oss-120b", 0.0002, 0.0004),
        p("llama-3.1-8b", 0.0001, 0.0001),
        p("llama-3.3-70b", 0.0002, 0.0002),
        p("llama-4-scout", 0.0002, 0.0004),
        p("llama-4-maverick", 0.0002, 0.0004),
        p("deepseek-r1-distill", 0.0002, 0.0004),
        p("deepseek-v3", 0.0003, 0.0006),
        p("deepseek-r1-distill-qwen-32b", 0.0002, 0.0004),
        p("qwen-2.5-72b", 0.0002, 0.0004),
        p("qwen-2.5-coder-32b", 0.0002, 0.0004),
        p("qwen-2.5-7b", 0.0001, 0.0002),
        p("qwen1.5-14b", 0.0001, 0.0002),
        p("qwen1.5-7b", 0.0001, 0.0002),
        p("mistral-7b", 0.0001, 0.0001),
        p("deepseek-coder-6.7b", 0.0001, 0.0002),
        p("llama-3.2-3b", 0.0001, 0.0001),
        p("llama-3.2-1b", 0.0001, 0.0001),
        p("codellama-34b", 0.0001, 0.0001),
        p("codellama-13b", 0.0001, 0.0001),
        p("codellama-7b", 0.0001, 0.0001),
        p("mixtral-8x7b", 0.0001, 0.0001),
        p("gemma-4", 0.0001, 0.0002),
        p("gemma-4-9b-it", 0.0001, 0.0002),
        p("gemma-4-27b-it", 0.0001, 0.0002),
        p("gemma-2-27b", 0.0001, 0.0002),
        p("gemma-2-9b", 0.0001, 0.0002),
        p("gemma-2-2b", 0.0001, 0.0002),
        p("phi-3-mini", 0.0001, 0.0001),
        p("phi-2", 0.0001, 0.0001),
        p("internlm2-7b", 0.0001, 0.0001),
        // 向量嵌入（按 token 计费）
        p("bge-m3", 0.0002, 0.0),
        p("bge-base-en-v1.5", 0.0002, 0.0),
        p("bge-large-en", 0.0002, 0.0),
        p("qwen3-embedding", 0.0002, 0.0),
        p("qwen3-embedding-0.6b", 0.0002, 0.0),
        p("embeddinggemma-300m", 0.0002, 0.0),
        // 多模态 / 图片生成（按次计价）
        c("llava-1.5", 0.0),
        c("llava-1.5-7b", 0.0),
        c("flux-1-schnell", 0.0),
        c("flux-1-dev", 0.0),
        c("sdxl", 0.0),
        // 语音识别（Whisper，按次计价）
        c("whisper", 0.0),
        c("whisper-1", 0.0),
        c("whisper-tiny-en", 0.0),
        c("whisper-large-v3-turbo", 0.0),
        // 视觉 / 语音合成（按次计价）
        c("moondream3.1-9B-A2B", 0.0),
        c("tts", 0.0),
    ]
}

impl PricingStore {
    pub fn new(store: Arc<FileStore>) -> Self {
        let s = Self {
            prices: RwLock::new(HashMap::new()),
            ratios: RwLock::new(RatioConfig::default()),
            store,
        };
        let _ = s.load();
        s.ensure_default_prices();
        s
    }

    /// 首次启动种子内置默认定价目录。
    ///
    /// 仅当目录为空（尚无任何定价条目）且未打过种子标记时执行一次：
    /// 将 `default_prices()` 逐条 upsert 到存储并置位标记。标记落盘可保证
    /// 管理员之后删光所有定价也不会被重新种子（尊重管理员意图）。
    fn ensure_default_prices(&self) {
        // 目录非空 → 管理员已配置过，跳过
        if !self.prices.read().is_empty() {
            return;
        }
        // 已打过种子标记 → 尊重管理员删光定价的操作，跳过
        if self
            .store
            .get::<bool>(PRICING_SEEDED_KEY)
            .ok()
            .flatten()
            .unwrap_or(false)
        {
            return;
        }
        let mut seeded = 0;
        for price in default_prices() {
            if self.upsert_price(price).is_ok() {
                seeded += 1;
            }
        }
        if self.store.put(PRICING_SEEDED_KEY, &true).is_err() {
            error!("failed to persist pricing seed marker");
        }
        tracing::info!("Seeded default pricing catalog: {seeded} models");
    }

    /// 从存储加载定价与倍率
    pub fn load(&self) -> anyhow::Result<()> {
        let keys = self.store.list("price:")?;
        let mut prices = HashMap::with_capacity(keys.len());
        for key in &keys {
            if let Some(p) = self.store.get::<ModelPrice>(key)? {
                prices.insert(p.model_name.clone(), p);
            }
        }
        *self.prices.write() = prices;

        if let Some(r) = self.store.get::<RatioConfig>(RATIO_KEY)? {
            *self.ratios.write() = r;
        }
        Ok(())
    }

    // ── 定价 CRUD ──────────────────────────────────────────────────

    /// 列出所有定价条目
    pub fn list_prices(&self) -> Vec<ModelPrice> {
        let mut all: Vec<ModelPrice> = self.prices.read().values().cloned().collect();
        all.sort_by(|a, b| a.model_name.cmp(&b.model_name));
        all
    }

    /// 获取单模型定价
    pub fn get_price(&self, model: &str) -> Option<ModelPrice> {
        self.prices.read().get(model).cloned()
    }

    /// 新增/更新定价（upsert，以 model_name 为键）
    pub fn upsert_price(&self, mut price: ModelPrice) -> anyhow::Result<ModelPrice> {
        let now = chrono::Utc::now().timestamp();
        if price.created_at == 0 {
            price.created_at = now;
        }
        price.updated_at = now;
        self.store.put(&format!("price:{}", price.model_name), &price)?;
        self.prices.write().insert(price.model_name.clone(), price.clone());
        Ok(price)
    }

    /// 删除定价
    pub fn delete_price(&self, model: &str) -> anyhow::Result<()> {
        self.store.delete(&format!("price:{model}"))?;
        self.prices.write().remove(model);
        Ok(())
    }

    // ── 倍率配置 ──────────────────────────────────────────────────

    /// 获取当前倍率配置
    pub fn get_ratios(&self) -> RatioConfig {
        self.ratios.read().clone()
    }

    /// 更新倍率配置
    pub fn update_ratios(&self, ratios: RatioConfig) -> anyhow::Result<RatioConfig> {
        self.store.put(RATIO_KEY, &ratios)?;
        *self.ratios.write() = ratios.clone();
        Ok(ratios)
    }

    // ── 计费计算 ──────────────────────────────────────────────────

    /// 计算单次请求费用。
    ///
    /// 公式：
    /// - price_type=token: (input * input_price + output * output_price) / 1000 * model_ratio * group_ratio
    /// - price_type=count: input_price * model_ratio * group_ratio（按次计价，input_price 即每次价格）
    ///
    /// B09：无定价条目时返回 `Err(PricingError)` 而非静默按 0 计费——
    /// 未配置定价的模型若被放行调用将产生免费用量，调用方应前置拦截
    /// （见 `openai.rs::ensure_model_priced`）或对 Err 记告警日志。
    pub fn calculate_cost(
        &self,
        model: &str,
        input_tokens: u64,
        output_tokens: u64,
        group: &str,
    ) -> Result<f64, PricingError> {
        let price = self
            .get_price(model)
            .ok_or_else(|| PricingError(model.to_string()))?;
        let ratios = self.ratios.read();
        let model_ratio = ratios.model_ratio(model);
        let group_ratio = ratios.group_ratio(group);

        let base = if price.price_type == "count" {
            price.input_price
        } else {
            (input_tokens as f64 * price.input_price + output_tokens as f64 * price.output_price) / 1000.0
        };
        Ok(base * model_ratio * group_ratio)
    }

    /// 计算费用并向上取整为 i64 配额单位（避免浮点扣费）。
    pub fn calculate_cost_quoted(
        &self,
        model: &str,
        input_tokens: u64,
        output_tokens: u64,
        group: &str,
    ) -> Result<i64, PricingError> {
        let cost = self.calculate_cost(model, input_tokens, output_tokens, group)?;
        if cost <= 0.0 {
            return Ok(0);
        }
        // 向上取整，最低 1 配额单位
        Ok((cost + 0.999999) as i64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn store() -> PricingStore {
        PricingStore::new(Arc::new(FileStore::new(
            TempDir::new().unwrap().path().to_path_buf(),
        )))
    }

    #[test]
    fn upsert_and_get() {
        let s = store();
        s.upsert_price(ModelPrice::new("gpt-4", 0.03, 0.06)).unwrap();
        let p = s.get_price("gpt-4").unwrap();
        assert_eq!(p.input_price, 0.03);
        assert_eq!(p.output_price, 0.06);
    }

    #[test]
    fn calculate_token_cost() {
        let s = store();
        s.upsert_price(ModelPrice::new("gpt-4", 0.03, 0.06)).unwrap();
        // 1000 input * 0.03/1k + 500 output * 0.06/1k = 0.03 + 0.03 = 0.06
        let cost = s.calculate_cost("gpt-4", 1000, 500, "default").unwrap();
        assert!((cost - 0.06).abs() < 1e-9);
    }

    #[test]
    fn calculate_with_ratios() {
        let s = store();
        s.upsert_price(ModelPrice::new("gpt-4", 0.03, 0.06)).unwrap();
        let mut ratios = RatioConfig::default();
        ratios.model_ratio.insert("gpt-4".to_string(), 2.0);
        ratios.group_ratio.insert("vip".to_string(), 0.5);
        s.update_ratios(ratios).unwrap();
        // 0.06 * 2.0 * 0.5 = 0.06
        let cost = s.calculate_cost("gpt-4", 1000, 500, "vip").unwrap();
        assert!((cost - 0.06).abs() < 1e-9);
    }

    #[test]
    fn count_pricing() {
        let s = store();
        let mut p = ModelPrice::new("dall-e", 0.04, 0.0);
        p.price_type = "count".to_string();
        s.upsert_price(p).unwrap();
        // 按次计价：input_price = 0.04
        let cost = s.calculate_cost("dall-e", 0, 0, "default").unwrap();
        assert!((cost - 0.04).abs() < 1e-9);
    }

    #[test]
    fn no_price_is_error() {
        // B09：无定价不再静默按 0 计费（免费放行），而是显式返回错误
        let s = store();
        assert!(s.calculate_cost("unknown", 1000, 1000, "default").is_err());
    }
}