//! 视频计费 — 支持 Seedance / Veo 等视频生成模型的按时长/分辨率计费。
//!
//! 参照 burncloud `crates/router/src/price_sync.rs` 中 `video_pricing` 字段与
//! `video_price_derived` 推导逻辑（720p 基准 + resolution_weight 加权）：
//! - `video_tokens = duration_secs × resolution_weight`
//! - `cost = video_tokens × video_price / 1_000_000`
//! - 480p 权重 1，720p 权重 2，1080p 权重 4（每级翻倍）
//!
//! AIGX 单 crate：`VideoPricingTable` 基于 `PricingStore` 的 FileStore 持久化，
//! 独立于 `ModelPrice`（视频模型可同时有 token 定价与视频定价，按请求类型选用）。

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

use crate::storage::FileStore;

/// 视频分辨率档位。
///
/// 权重用于把不同分辨率折算到统一"视频 token"度量：
/// - `P480`：权重 1（基准）
/// - `P720`：权重 2
/// - `P1080`：权重 4
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VideoResolution {
    /// 480p
    P480,
    /// 720p
    P720,
    /// 1080p
    P1080,
}

impl VideoResolution {
    /// 从字符串解析（"480p"/"720p"/"1080p"）。
    pub fn from_str_lossy(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "480p" => Some(Self::P480),
            "720p" => Some(Self::P720),
            "1080p" => Some(Self::P1080),
            _ => None,
        }
    }

    /// 转字符串。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::P480 => "480p",
            Self::P720 => "720p",
            Self::P1080 => "1080p",
        }
    }

    /// 分辨率权重（用于 video_tokens = duration × weight）。
    pub fn weight(&self) -> u32 {
        match self {
            Self::P480 => 1,
            Self::P720 => 2,
            Self::P1080 => 4,
        }
    }
}

/// 单模型视频定价配置。
///
/// `per_resolution_price_per_sec`：每分辨率每秒价格（USD）。
/// 缺省分辨率用 `default_price_per_sec`。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VideoModelPricing {
    /// 模型名
    pub model_name: String,
    /// 默认每秒价格（USD，无分辨率特例时用）
    #[serde(default)]
    pub default_price_per_sec: f64,
    /// 按分辨率覆盖的每秒价格（USD）
    #[serde(default)]
    pub per_resolution_price_per_sec: HashMap<String, f64>,
    /// 计价币种（默认 USD）
    #[serde(default = "default_currency")]
    pub currency: String,
}

fn default_currency() -> String {
    "USD".to_string()
}

impl VideoModelPricing {
    /// 取指定分辨率的每秒价格（优先 per_resolution，回退 default）。
    pub fn price_per_sec(&self, resolution: VideoResolution) -> f64 {
        self.per_resolution_price_per_sec
            .get(resolution.as_str())
            .copied()
            .unwrap_or(self.default_price_per_sec)
    }

    /// 计算单次视频生成费用（USD）。
    ///
    /// `cost = duration_secs × price_per_sec(resolution)`
    pub fn calculate_cost(&self, duration_secs: u64, resolution: VideoResolution) -> f64 {
        duration_secs as f64 * self.price_per_sec(resolution)
    }

    /// 计算视频 token 数（用于与 burncloud 公式对齐）。
    ///
    /// `video_tokens = duration_secs × resolution.weight()`
    pub fn video_tokens(duration_secs: u64, resolution: VideoResolution) -> u64 {
        duration_secs * resolution.weight() as u64
    }
}

/// 视频定价表 — 多模型视频定价配置 + FileStore 持久化。
///
/// 存储键：`video_price:{model_name}` → `VideoModelPricing` JSON。
pub struct VideoPricingTable {
    table: RwLock<HashMap<String, VideoModelPricing>>,
    store: Arc<FileStore>,
}

const VIDEO_PRICE_KEY_PREFIX: &str = "video_price:";

impl VideoPricingTable {
    /// 构造并自动从存储加载。
    pub fn new(store: Arc<FileStore>) -> Self {
        let s = Self {
            table: RwLock::new(HashMap::new()),
            store,
        };
        let _ = s.load();
        s
    }

    /// 从存储加载全部视频定价。
    pub fn load(&self) -> anyhow::Result<()> {
        let keys = self.store.list(VIDEO_PRICE_KEY_PREFIX)?;
        let mut map = HashMap::with_capacity(keys.len());
        for key in &keys {
            if let Some(p) = self.store.get::<VideoModelPricing>(key)? {
                map.insert(p.model_name.clone(), p);
            }
        }
        *self.table.write() = map;
        Ok(())
    }

    /// 列出全部视频定价。
    pub fn list(&self) -> Vec<VideoModelPricing> {
        let mut all: Vec<VideoModelPricing> = self.table.read().values().cloned().collect();
        all.sort_by(|a, b| a.model_name.cmp(&b.model_name));
        all
    }

    /// 取单模型视频定价。
    pub fn get(&self, model: &str) -> Option<VideoModelPricing> {
        self.table.read().get(model).cloned()
    }

    /// 新增/更新视频定价。
    pub fn upsert(&self, pricing: VideoModelPricing) -> anyhow::Result<VideoModelPricing> {
        self.store.put(
            &format!("{VIDEO_PRICE_KEY_PREFIX}{}", pricing.model_name),
            &pricing,
        )?;
        self.table
            .write()
            .insert(pricing.model_name.clone(), pricing.clone());
        Ok(pricing)
    }

    /// 删除视频定价。
    pub fn remove(&self, model: &str) -> anyhow::Result<()> {
        self.store
            .delete(&format!("{VIDEO_PRICE_KEY_PREFIX}{model}"))?;
        self.table.write().remove(model);
        Ok(())
    }

    /// 计算视频生成费用。
    ///
    /// 未配置定价返回 `Err`（与 `PricingStore::calculate_cost` 一致，避免免费用量）。
    pub fn calculate_cost(
        &self,
        model: &str,
        duration_secs: u64,
        resolution: VideoResolution,
    ) -> Result<f64, VideoPricingError> {
        let pricing = self
            .get(model)
            .ok_or_else(|| VideoPricingError(model.to_string()))?;
        Ok(pricing.calculate_cost(duration_secs, resolution))
    }

    /// 计算费用并向上取整为 i64 配额单位。
    pub fn calculate_cost_quoted(
        &self,
        model: &str,
        duration_secs: u64,
        resolution: VideoResolution,
    ) -> Result<i64, VideoPricingError> {
        let cost = self.calculate_cost(model, duration_secs, resolution)?;
        if cost <= 0.0 {
            return Ok(0);
        }
        Ok((cost + 0.999999) as i64)
    }
}

/// 视频定价错误：模型未配置视频定价。
#[derive(Debug, thiserror::Error)]
#[error("no video pricing configured for model: {0}")]
pub struct VideoPricingError(pub String);

/// 已知视频生成模型名（用于自动识别请求类型）。
pub const MODEL_SEEDANCE: &str = "seedance";
pub const MODEL_VEO_2: &str = "veo-2";
pub const MODEL_VEO_3: &str = "veo-3";

/// 判断模型名是否为视频生成模型（按已知名 + 后缀匹配）。
pub fn is_video_model(model: &str) -> bool {
    let lower = model.to_lowercase();
    lower == MODEL_SEEDANCE
        || lower == MODEL_VEO_2
        || lower == MODEL_VEO_3
        || lower.starts_with("seedance-")
        || lower.starts_with("veo-")
}

/// 内置默认视频定价（首次启动种子用）。
pub fn default_video_pricings() -> Vec<VideoModelPricing> {
    vec![
        VideoModelPricing {
            model_name: MODEL_SEEDANCE.to_string(),
            default_price_per_sec: 0.05,
            per_resolution_price_per_sec: HashMap::from([
                ("480p".to_string(), 0.04),
                ("720p".to_string(), 0.05),
                ("1080p".to_string(), 0.10),
            ]),
            currency: "USD".to_string(),
        },
        VideoModelPricing {
            model_name: MODEL_VEO_2.to_string(),
            default_price_per_sec: 0.06,
            per_resolution_price_per_sec: HashMap::from([
                ("720p".to_string(), 0.06),
                ("1080p".to_string(), 0.12),
            ]),
            currency: "USD".to_string(),
        },
        VideoModelPricing {
            model_name: MODEL_VEO_3.to_string(),
            default_price_per_sec: 0.08,
            per_resolution_price_per_sec: HashMap::from([
                ("720p".to_string(), 0.08),
                ("1080p".to_string(), 0.16),
            ]),
            currency: "USD".to_string(),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn table() -> VideoPricingTable {
        VideoPricingTable::new(Arc::new(FileStore::new(
            TempDir::new().unwrap().path().to_path_buf(),
        )))
    }

    #[test]
    fn resolution_weight_doubles_per_tier() {
        assert_eq!(VideoResolution::P480.weight(), 1);
        assert_eq!(VideoResolution::P720.weight(), 2);
        assert_eq!(VideoResolution::P1080.weight(), 4);
    }

    #[test]
    fn resolution_from_str_lossy() {
        assert_eq!(
            VideoResolution::from_str_lossy("720p"),
            Some(VideoResolution::P720)
        );
        assert_eq!(
            VideoResolution::from_str_lossy("1080P"),
            Some(VideoResolution::P1080)
        );
        assert_eq!(VideoResolution::from_str_lossy("4k"), None);
    }

    #[test]
    fn video_tokens_calculation() {
        // 10s 720p → 10 × 2 = 20
        assert_eq!(
            VideoModelPricing::video_tokens(10, VideoResolution::P720),
            20
        );
        // 5s 1080p → 5 × 4 = 20
        assert_eq!(
            VideoModelPricing::video_tokens(5, VideoResolution::P1080),
            20
        );
    }

    #[test]
    fn calculate_cost_uses_resolution_specific_price() {
        let pricing = VideoModelPricing {
            model_name: "test".to_string(),
            default_price_per_sec: 0.05,
            per_resolution_price_per_sec: HashMap::from([
                ("480p".to_string(), 0.04),
                ("1080p".to_string(), 0.10),
            ]),
            currency: "USD".to_string(),
        };
        // 10s 480p @ 0.04 = 0.4
        assert!((pricing.calculate_cost(10, VideoResolution::P480) - 0.4).abs() < 1e-9);
        // 10s 720p 回退 default @ 0.05 = 0.5
        assert!((pricing.calculate_cost(10, VideoResolution::P720) - 0.5).abs() < 1e-9);
        // 10s 1080p @ 0.10 = 1.0
        assert!((pricing.calculate_cost(10, VideoResolution::P1080) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn table_upsert_and_get() {
        let t = table();
        let p = VideoModelPricing {
            model_name: "seedance".to_string(),
            default_price_per_sec: 0.05,
            per_resolution_price_per_sec: HashMap::new(),
            currency: "USD".to_string(),
        };
        t.upsert(p).unwrap();
        let got = t.get("seedance").unwrap();
        assert!((got.default_price_per_sec - 0.05).abs() < 1e-9);
    }

    #[test]
    fn table_calculate_cost_returns_error_when_unconfigured() {
        let t = table();
        assert!(t
            .calculate_cost("unknown", 10, VideoResolution::P720)
            .is_err());
    }

    #[test]
    fn table_calculate_cost_quoted_rounds_up() {
        let t = table();
        t.upsert(VideoModelPricing {
            model_name: "veo-2".to_string(),
            default_price_per_sec: 0.001, // 10s = 0.01
            per_resolution_price_per_sec: HashMap::new(),
            currency: "USD".to_string(),
        })
        .unwrap();
        // 10s × 0.001 = 0.01 → 向上取整为 1
        let q = t
            .calculate_cost_quoted("veo-2", 10, VideoResolution::P720)
            .unwrap();
        assert_eq!(q, 1);
    }

    #[test]
    fn table_remove() {
        let t = table();
        t.upsert(VideoModelPricing {
            model_name: "x".to_string(),
            default_price_per_sec: 0.01,
            per_resolution_price_per_sec: HashMap::new(),
            currency: "USD".to_string(),
        })
        .unwrap();
        assert!(t.get("x").is_some());
        t.remove("x").unwrap();
        assert!(t.get("x").is_none());
    }

    #[test]
    fn is_video_model_recognizes_known() {
        assert!(is_video_model("seedance"));
        assert!(is_video_model("Seedance-1.0")); // 前缀
        assert!(is_video_model("veo-2"));
        assert!(is_video_model("veo-3-fast"));
        assert!(!is_video_model("gpt-4"));
        assert!(!is_video_model("dall-e"));
    }

    #[test]
    fn default_video_pricings_covers_seedance_and_veo() {
        let defaults = default_video_pricings();
        let names: Vec<&str> = defaults.iter().map(|p| p.model_name.as_str()).collect();
        assert!(names.contains(&MODEL_SEEDANCE));
        assert!(names.contains(&MODEL_VEO_2));
        assert!(names.contains(&MODEL_VEO_3));
    }

    #[test]
    fn default_pricing_1080p_doubles_720p() {
        // 默认定价中 1080p 价格应为 720p 的 2 倍（与分辨率权重对齐）
        for p in default_video_pricings() {
            let p720 = p.per_resolution_price_per_sec.get("720p").copied();
            let p1080 = p.per_resolution_price_per_sec.get("1080p").copied();
            if let (Some(a), Some(b)) = (p720, p1080) {
                assert!((b / a - 2.0).abs() < 1e-9, "1080p 应为 720p 的 2 倍");
            }
        }
    }
}
