//! 会话图片压缩 + 视觉模型兼容（对齐 Agent2API 3.4.0 会话图片压缩 / 3.5.0 图片兼容设置）。
//!
//! ── 这一层解决什么 ─────────────────────────────────────────
//! 1. **压缩**：客户端把截图/照片以 `data:image/...;base64,` 内联进对话，
//!    一张几 MB 的 base64 既撑大请求体，也拉高按像素折算的上游 token 计费。
//!    转发前把超阈值的图缩到长边上限并**按原格式**重编码（jpeg 走质量参数，
//!    png 重压缩，webp 走无损编码）；同一张原图按**内容哈希**复用压缩结果
//!    （纯内存 LRU，不落盘）。
//! 2. **图片兼容**：请求含图片而目标模型能力位不含 `vision` 时，按配置
//!    - `describe`（辅助识图）：用辅助视觉模型生成图片描述并替换为文本块
//!      （一次请求内同一张图只调用一次）；
//!    - `switch`（切换视觉模型）：把请求改路由到配置的视觉模型；
//!
//!    两种模式默认关闭 —— 关闭时逐字节零行为变化。
//! 3. **视觉备选链**：视觉模型调用失败（沿用既有可重试语义）时按
//!    `fallback_models` 顺序回退。
//!
//! ── 只认内联 data URI ──────────────────────────────────────
//! `http(s)` 外链一律不抓取、不压缩（无网络 IO）；gif / svg / avif 等非
//! jpeg/png/webp 白名单格式原样放行；动图 WebP（`ANIM` 块）不改写。
//!
//! ── 硬约束 ─────────────────────────────────────────────────
//! 解码/编码失败、坏图、尺寸非法一律**原样保留**，绝不阻断请求；压完反而
//! 更大也保留原图。全程纯内存，无磁盘 IO、无网络（辅助识图的上游调用除外）。
//!
//! 本模块被 lib 与 bin 两个 target 同时编译：bin 只用到其中一部分公开入口
//! （路由侧 hook），lib 视角下另有若干入口没有调用方 —— 与 `api::openai`
//! 同口径整模块豁免 dead_code。
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::Arc;

use base64::engine::general_purpose::{GeneralPurpose, GeneralPurposeConfig};
use base64::engine::DecodePaddingMode;
use base64::Engine as _;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::bridge::{ChatFormat, ChatMessage, Role};

// ── 默认值与边界 ──────────────────────────────────────────────────────

/// 压缩长边上限默认 1568（多数视觉模型按 1568 折算 token 的最优边长）。
pub const DEFAULT_MAX_EDGE: u32 = 1568;
/// 重编码质量默认 85。
pub const DEFAULT_QUALITY: u8 = 85;
/// 触发压缩的原图体积阈值（KB）默认 256。
pub const DEFAULT_THRESHOLD_KB: u32 = 256;
/// 长边上限合法范围。
pub const MAX_EDGE_MIN: u32 = 256;
pub const MAX_EDGE_MAX: u32 = 8192;
/// 质量合法范围。
pub const QUALITY_MIN: u8 = 1;
pub const QUALITY_MAX: u8 = 100;
/// 阈值合法范围（KB；0 = 每张内联图都尝试压缩）。
pub const THRESHOLD_KB_MIN: u32 = 0;
pub const THRESHOLD_KB_MAX: u32 = 10240;

/// `mode` 合法值：辅助识图。
pub const MODE_DESCRIBE: &str = "describe";
/// `mode` 合法值：切换视觉模型。
pub const MODE_SWITCH: &str = "switch";

/// 默认辅助识图提示词。
pub const DEFAULT_DESCRIBE_PROMPT: &str =
    "请用中文详细描述这张图片的内容：主体、场景、画面中的文字（如有）以及关键细节。只输出描述本身。";

/// 单次请求最多调用辅助识图的图片张数上限（限制上游成本；超出部分记日志
/// 后保留原图，不静默丢弃）。
pub const MAX_DESCRIBE_IMAGES: usize = 8;

/// `[image_compat]` 段配置。
///
/// 压缩与视觉兼容同一段：压缩默认开（3.4.0 行为），视觉兼容默认关（3.5.0
/// 行为：关闭时请求逐字节不变）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageCompatConfig {
    /// 压缩开关，默认开。关闭后数据面不做任何扫描/改写。
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 长边上限（像素），默认 1568；越界在生效时夹到 [256, 8192]。
    #[serde(default = "default_max_edge")]
    pub max_edge: u32,
    /// 重编码质量（jpeg），默认 85；越界夹到 [1, 100]。
    #[serde(default = "default_quality")]
    pub quality: u8,
    /// 触发阈值（原图字节数的 KB），默认 256；0 = 每张都尝试。
    #[serde(default = "default_threshold_kb")]
    pub threshold_kb: u32,
    /// 图片兼容总开关（辅助识图 / 切换视觉模型），**默认关**。
    #[serde(default)]
    pub vision_enabled: bool,
    /// 模式：`describe`（默认）或 `switch`；非法值按 describe 处理。
    #[serde(default = "default_mode")]
    pub mode: String,
    /// 辅助视觉模型（describe 用；switch 模式下即目标视觉模型）。空 = 不生效。
    #[serde(default)]
    pub vision_model: String,
    /// 视觉模型备选链：视觉模型调用失败后按顺序回退。
    #[serde(default)]
    pub fallback_models: Vec<String>,
    /// 辅助识图提示词，空 = 默认提示词。
    #[serde(default)]
    pub describe_prompt: String,
}

fn default_true() -> bool {
    true
}
fn default_max_edge() -> u32 {
    DEFAULT_MAX_EDGE
}
fn default_quality() -> u8 {
    DEFAULT_QUALITY
}
fn default_threshold_kb() -> u32 {
    DEFAULT_THRESHOLD_KB
}
fn default_mode() -> String {
    MODE_DESCRIBE.to_string()
}

impl Default for ImageCompatConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_edge: DEFAULT_MAX_EDGE,
            quality: DEFAULT_QUALITY,
            threshold_kb: DEFAULT_THRESHOLD_KB,
            vision_enabled: false,
            mode: default_mode(),
            vision_model: String::new(),
            fallback_models: Vec::new(),
            describe_prompt: String::new(),
        }
    }
}

impl ImageCompatConfig {
    /// 生效值（切换/压缩统一读这一份，越界输入夹到合法区间）。
    pub fn normalized(&self) -> Self {
        Self {
            enabled: self.enabled,
            max_edge: self.max_edge.clamp(MAX_EDGE_MIN, MAX_EDGE_MAX),
            quality: self.quality.clamp(QUALITY_MIN, QUALITY_MAX),
            threshold_kb: self.threshold_kb.min(THRESHOLD_KB_MAX),
            vision_enabled: self.vision_enabled,
            mode: if self.mode.trim().eq_ignore_ascii_case(MODE_SWITCH) {
                MODE_SWITCH.to_string()
            } else {
                MODE_DESCRIBE.to_string()
            },
            vision_model: self.vision_model.trim().to_string(),
            fallback_models: self
                .fallback_models
                .iter()
                .map(|m| m.trim().to_string())
                .filter(|m| !m.is_empty())
                .collect(),
            describe_prompt: self.describe_prompt.clone(),
        }
    }

    /// 辅助识图实际使用的提示词（空配置回落默认）。
    pub fn effective_describe_prompt(&self) -> &str {
        let trimmed = self.describe_prompt.trim();
        if trimmed.is_empty() {
            DEFAULT_DESCRIBE_PROMPT
        } else {
            trimmed
        }
    }
}

// ── 进程级快照（数据面同步读取）────────────────────────────────────────

static SETTINGS: RwLock<Option<Arc<ImageCompatConfig>>> = RwLock::new(None);

/// 由 `main` 启动与 `PUT /api/image-compat/settings` 写入快照（归一化后存储）。
pub fn set_settings(cfg: ImageCompatConfig) {
    *SETTINGS.write() = Some(Arc::new(cfg.normalized()));
}

/// 读取当前生效配置（未初始化时返回内置默认值）。
pub fn settings() -> Arc<ImageCompatConfig> {
    if let Some(cfg) = SETTINGS.read().clone() {
        return cfg;
    }
    Arc::new(ImageCompatConfig::default())
}

/// 压缩是否开启（热路径短路用，一次原子读）。
pub fn compress_enabled() -> bool {
    settings().enabled
}

// ── 压缩 ──────────────────────────────────────────────────────────────

/// data URL 的 base64 引擎：标准字母表，解码放宽（允许少填充/非零尾随位），
/// 编码用规范填充。松一点只会多压几张图，收紧了会漏压。
const DATA_URL_BASE64: GeneralPurpose = GeneralPurpose::new(
    &base64::alphabet::STANDARD,
    GeneralPurposeConfig::new()
        .with_decode_padding_mode(DecodePaddingMode::Indifferent)
        .with_decode_allow_trailing_bits(true),
);

/// 本次压缩统计（只统计**发生替换**的图；日志用）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ImageCompressStats {
    /// 发生替换的图片数
    pub compressed: usize,
    /// 替换项压缩前的 data URI 字节量
    pub bytes_before: usize,
    /// 替换项压缩后的 data URI 字节量
    pub bytes_after: usize,
}

impl ImageCompressStats {
    pub fn changed(&self) -> bool {
        self.compressed > 0
    }
}

/// 消息列表里的内联图是否可压（只读早退：多数请求没有内联图，省一次深扫）。
pub fn messages_have_data_images(messages: &[ChatMessage]) -> bool {
    messages.iter().any(|m| {
        m.content_blocks
            .as_deref()
            .into_iter()
            .flatten()
            .any(|b| image_uri_of(b).is_some_and(|u| u.starts_with("data:image/")))
    })
}

/// 就地压缩消息里所有内联 base64 图片（在「即将发给这一家之前」调用）。
///
/// 单图失败/无需压缩时原样保留，绝不阻断请求。
pub fn compress_messages(
    messages: &mut [ChatMessage],
    cfg: &ImageCompatConfig,
) -> ImageCompressStats {
    let mut stats = ImageCompressStats::default();
    for message in messages.iter_mut() {
        let Some(blocks) = message.content_blocks.as_mut() else {
            continue;
        };
        stats.merge(compress_blocks(blocks, cfg));
    }
    stats
}

/// Responses 透传路径：就地压缩请求体 `input[].content[]` 里的内联图。
///
/// 只扫 `input` 列表（Responses 形态）；`messages` 形态由
/// [`compress_messages`] 在 ChatFormat 层处理。
pub fn compress_responses_body(body: &mut Value, cfg: &ImageCompatConfig) -> ImageCompressStats {
    let mut stats = ImageCompressStats::default();
    let Some(items) = body.get_mut("input").and_then(Value::as_array_mut) else {
        return stats;
    };
    for item in items.iter_mut() {
        let Some(blocks) = item.get_mut("content").and_then(Value::as_array_mut) else {
            continue;
        };
        stats.merge(compress_blocks(blocks, cfg));
    }
    stats
}

impl ImageCompressStats {
    fn merge(&mut self, other: Self) {
        self.compressed += other.compressed;
        self.bytes_before += other.bytes_before;
        self.bytes_after += other.bytes_after;
    }
}

/// content blocks 数组就地压缩（内部共用实现）。
fn compress_blocks(blocks: &mut [Value], cfg: &ImageCompatConfig) -> ImageCompressStats {
    let mut stats = ImageCompressStats::default();
    for block in blocks.iter_mut() {
        let Some(uri) = image_uri_of(block).map(str::to_string) else {
            continue;
        };
        let Some(next) = compress_with_cache(&uri, cfg) else {
            continue;
        };
        if next == uri {
            continue;
        }
        stats.compressed += 1;
        stats.bytes_before += uri.len();
        stats.bytes_after += next.len();
        set_image_uri(block, next);
    }
    stats
}

/// 取一个内容块里的图片 URL（Chat 形态 `image_url` / Responses 形态
/// `input_image`；值可为字符串或 `{url}` 对象）。
fn image_uri_of(part: &Value) -> Option<&str> {
    let kind = part.get("type").and_then(Value::as_str).unwrap_or("");
    if !kind.eq_ignore_ascii_case("image_url") && !kind.eq_ignore_ascii_case("input_image") {
        return None;
    }
    match part.get("image_url")? {
        Value::String(text) => Some(text.as_str()),
        object @ Value::Object(_) => object.get("url").and_then(Value::as_str),
        _ => None,
    }
}

/// 把压缩结果写回内容块：字符串形态直接替换，对象形态替换 `url` 字段
/// （`detail` 等其它字段原样保留）。
fn set_image_uri(part: &mut Value, next: String) {
    let Some(field) = part.get_mut("image_url") else {
        return;
    };
    match field {
        Value::String(slot) => *slot = next,
        Value::Object(map) => {
            map.insert("url".to_string(), Value::String(next));
        }
        _ => {}
    }
}

/// 带缓存的单张压缩：命中直接返回；未命中再压，成败都入缓存。
fn compress_with_cache(uri: &str, cfg: &ImageCompatConfig) -> Option<String> {
    if !uri.starts_with("data:image/") {
        return None; // http(s) 外链不抓取不压缩
    }
    let key = cache_key(uri, cfg);
    if let Some(hit) = with_cache(|cache| cache.lookup(&key)) {
        return hit;
    }
    // 压缩期间不放缓存锁（编解码可耗时几十到几百 ms，别阻塞其它请求）
    let next = compress_data_uri(uri, cfg).filter(|next| next.as_str() != uri);
    // 「无需压缩 / 坏图」也入缓存（None），避免历史里的同一张图每轮重复解码
    with_cache(|cache| cache.store(key, next.clone()));
    next
}

/// 图片基格式（按 data URI 声明的 subtype 判定；白名单外一律不处理）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ImageKind {
    Jpeg,
    Png,
    Webp,
}

impl ImageKind {
    fn from_subtype(subtype: &str) -> Option<Self> {
        if subtype.eq_ignore_ascii_case("jpeg") || subtype.eq_ignore_ascii_case("jpg") {
            Some(Self::Jpeg)
        } else if subtype.eq_ignore_ascii_case("png") {
            Some(Self::Png)
        } else if subtype.eq_ignore_ascii_case("webp") {
            Some(Self::Webp)
        } else {
            None
        }
    }

    /// 输出 data URI 的 media subtype（jpeg 统一写 `jpeg`）。
    fn out_subtype(self) -> &'static str {
        match self {
            Self::Jpeg => "jpeg",
            Self::Png => "png",
            Self::Webp => "webp",
        }
    }
}

/// 压缩单个 data URI；`None` = 保持原样（格式不符/低于阈值/动图/失败/压完更大）。
fn compress_data_uri(uri: &str, cfg: &ImageCompatConfig) -> Option<String> {
    let (kind, base64_body) = split_data_uri(uri.trim())?;
    // 阈值判定在解码前、按 base64 文本长度估算解码字节数，避免白解码一次
    let threshold = cfg.threshold_kb.min(THRESHOLD_KB_MAX) as usize * 1024;
    if decoded_len_estimate(base64_body) < threshold {
        return None;
    }
    let compact: String = base64_body
        .chars()
        .filter(|ch| *ch != '\r' && *ch != '\n')
        .collect();
    let input = DATA_URL_BASE64.decode(compact.as_bytes()).ok()?;
    if input.is_empty() || has_anim_chunk(&input) {
        return None; // 空图 / 动图 WebP：改写会把动图变静帧
    }
    let decoded = image::load_from_memory(&input).ok()?;
    let encoded = encode_image(&decoded, kind, cfg)?;
    if encoded.len() >= input.len() {
        return None; // 压完反而更大：保留原图
    }
    Some(format!(
        "data:image/{};base64,{}",
        kind.out_subtype(),
        DATA_URL_BASE64.encode(&encoded)
    ))
}

/// 取出 `data:image/<subtype>;base64,<body>` 的格式与主体（大小写不敏感）。
fn split_data_uri(uri: &str) -> Option<(ImageKind, &str)> {
    let rest = strip_prefix_ignore_ascii_case(uri, "data:image/")?;
    let separator = rest.find(';')?;
    let kind = ImageKind::from_subtype(&rest[..separator])?;
    let body = strip_prefix_ignore_ascii_case(&rest[separator..], ";base64,")?;
    if body.is_empty() {
        return None;
    }
    Some((kind, body))
}

/// 大小写不敏感的前缀剥离。
fn strip_prefix_ignore_ascii_case<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let head = text.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &text[prefix.len()..])
}

/// base64 文本长度 → 解码后字节数估算（阈值判定用，避免先解码再比较）。
fn decoded_len_estimate(base64: &str) -> usize {
    let trimmed = base64.trim_end_matches(['\r', '\n']);
    let padding = if trimmed.ends_with("==") {
        2
    } else if trimmed.ends_with('=') {
        1
    } else {
        0
    };
    (trimmed.len() * 3 / 4).saturating_sub(padding)
}

/// 动图 WebP 检出（`RIFF` 容器里的 `ANIM` 块）。误判方向是「少压一张」，
/// 不会改坏动图。
fn has_anim_chunk(bytes: &[u8]) -> bool {
    if bytes.len() < 4 || &bytes[0..4] != b"RIFF" {
        return false;
    }
    bytes.windows(4).any(|window| window == b"ANIM")
}

/// 解码 → 等比缩到长边上限（不放大）→ 按原格式重编码。
fn encode_image(
    image: &image::DynamicImage,
    kind: ImageKind,
    cfg: &ImageCompatConfig,
) -> Option<Vec<u8>> {
    let max_edge = cfg.max_edge.clamp(MAX_EDGE_MIN, MAX_EDGE_MAX);
    let quality = cfg.quality.clamp(QUALITY_MIN, QUALITY_MAX);
    let long_edge = image.width().max(image.height());
    let shrunk;
    let source: &image::DynamicImage = if long_edge > max_edge {
        shrunk = image.resize(max_edge, max_edge, image::imageops::FilterType::Lanczos3);
        &shrunk
    } else {
        image
    };
    let (width, height) = (source.width(), source.height());
    if width == 0 || height == 0 {
        return None; // JPEG 编码器对 0 尺寸有边界假设，直接拒绝
    }
    let mut output: Vec<u8> = Vec::new();
    match kind {
        ImageKind::Jpeg => {
            let rgb = flatten_to_rgb8(source)?;
            let mut encoder =
                image::codecs::jpeg::JpegEncoder::new_with_quality(&mut output, quality);
            encoder.encode_image(&rgb).ok()?;
        }
        ImageKind::Png => {
            use image::ImageEncoder as _;
            let rgba = source.to_rgba8();
            image::codecs::png::PngEncoder::new(&mut output)
                .write_image(
                    rgba.as_raw(),
                    width,
                    height,
                    image::ExtendedColorType::Rgba8,
                )
                .ok()?;
        }
        ImageKind::Webp => {
            // image 0.25 只提供无损 WebP 编码器（无 lossy）：缩放后的无损
            // 重编码通常已显著变小；若反而更大，调用方保留原图。
            let rgba = source.to_rgba8();
            image::codecs::webp::WebPEncoder::new_lossless(&mut output)
                .encode(
                    rgba.as_raw(),
                    width,
                    height,
                    image::ExtendedColorType::Rgba8,
                )
                .ok()?;
        }
    }
    Some(output)
}

/// 拍平成不含 alpha 的 8 位 RGB（有 alpha 的按白底合成，等价 sharp 的
/// `.flatten({background:'#ffffff'})`）。
fn flatten_to_rgb8(image: &image::DynamicImage) -> Option<image::RgbImage> {
    if !image.color().has_alpha() {
        return Some(image.to_rgb8());
    }
    let rgba = image.to_rgba8();
    let (width, height) = rgba.dimensions();
    let pixels = (width as usize)
        .saturating_mul(height as usize)
        .saturating_mul(3);
    let mut data: Vec<u8> = Vec::new();
    if data.try_reserve_exact(pixels).is_err() {
        return None;
    }
    for pixel in rgba.pixels() {
        let channels = pixel.0;
        let alpha = u32::from(channels[3]);
        let opaque = 255 - alpha;
        for channel in &channels[..3] {
            let blended = (u32::from(*channel) * alpha + 255 * opaque + 127) / 255;
            data.push(blended as u8);
        }
    }
    image::RgbImage::from_raw(width, height, data)
}

// ── 压缩结果缓存（同图复用）──────────────────────────────────────────

/// 缓存键 = 原图 data URI 的 sha256 + 三个压缩参数。参数参与键 ⇒ 改设置
/// 自动换键，无需失效逻辑。用强哈希而非 DefaultHasher：键碰撞会把 A 图的
/// 结果写给 B 图（静默发错图），代价远高于一次哈希。
fn cache_key(uri: &str, cfg: &ImageCompatConfig) -> String {
    format!(
        "{}|{}|{}|{}",
        sha256_hex(uri.as_bytes()),
        cfg.max_edge,
        cfg.quality,
        cfg.threshold_kb
    )
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// 条目数上限：兜「几百张小图」；内存的决定项是总字节上限。
const CACHE_MAX_ENTRIES: usize = 512;
/// 总字节上限：64 MiB（纯内存，不落盘）。
const CACHE_MAX_BYTES: usize = 64 * 1024 * 1024;

struct CacheEntry {
    payload: Option<String>,
    bytes: usize,
    used: u64,
}

struct Cache {
    entries: HashMap<String, CacheEntry>,
    bytes: usize,
    seq: u64,
}

static CACHE: RwLock<Option<Cache>> = RwLock::new(None);

/// 在缓存上执行一次操作（缓存懒初始化，单次加锁；不可重入）。
fn with_cache<R>(f: impl FnOnce(&mut Cache) -> R) -> R {
    let mut guard = CACHE.write();
    let cache = guard.get_or_insert_with(|| Cache {
        entries: HashMap::new(),
        bytes: 0,
        seq: 0,
    });
    f(cache)
}

impl Cache {
    /// 命中返回 `Some(压缩结果或 None)`；未命中返回 `None`。
    fn lookup(&mut self, key: &str) -> Option<Option<String>> {
        self.seq = self.seq.wrapping_add(1);
        let seq = self.seq;
        let entry = self.entries.get_mut(key)?;
        entry.used = seq;
        Some(entry.payload.clone())
    }

    fn store(&mut self, key: String, payload: Option<String>) {
        let bytes = key.len() + payload.as_ref().map(String::len).unwrap_or(0);
        if let Some(old) = self.entries.remove(&key) {
            self.bytes = self.bytes.saturating_sub(old.bytes);
        }
        self.seq = self.seq.wrapping_add(1);
        self.entries.insert(
            key,
            CacheEntry {
                payload,
                bytes,
                used: self.seq,
            },
        );
        self.bytes = self.bytes.saturating_add(bytes);
        self.evict();
    }

    fn evict(&mut self) {
        while self.entries.len() > CACHE_MAX_ENTRIES
            || (self.bytes > CACHE_MAX_BYTES && self.entries.len() > 1)
        {
            let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.used)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            if let Some(entry) = self.entries.remove(&oldest) {
                self.bytes = self.bytes.saturating_sub(entry.bytes);
            }
        }
    }
}

/// 压缩缓存当前条目数（仅测试用；不泄露内容）。
#[cfg(test)]
pub fn cache_entry_count() -> usize {
    with_cache(|cache| cache.entries.len())
}

/// 清空压缩缓存（仅测试用：用例间隔离）。
#[cfg(test)]
pub fn clear_cache() {
    with_cache(|cache| {
        cache.entries.clear();
        cache.bytes = 0;
    });
}

// ── 图片兼容：能力判定 / 路由决策 / 辅助识图 ──────────────────────────

/// 视觉兼容决策（纯函数，便于单测钉住默认关闭语义）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VisionDecision {
    /// 不干预（未开启 / 请求无图 / 模型本身支持 vision / 未配置视觉模型）
    Keep,
    /// 辅助识图：把图片替换为文本描述（就地改写消息）
    Describe,
    /// 切换视觉模型：改路由到该模型
    Switch(String),
}

/// 模型能力位是否含 `vision`（大小写不敏感；未知模型 = 不含）。
pub fn model_supports_vision(state: &crate::api::openai::AppState, model: &str) -> bool {
    state
        .model_metadata
        .get(model, None)
        .capabilities
        .iter()
        .any(|cap| cap.eq_ignore_ascii_case("vision"))
}

/// 视觉决策：仅在 `vision_enabled` 且请求含图且目标模型**不含** vision 时生效。
///
/// `has_images` 由调用方从请求体推导（与选路能力过滤共用同一信号）。
pub fn vision_decision(
    cfg: &ImageCompatConfig,
    model_has_vision: bool,
    has_images: bool,
) -> VisionDecision {
    if !cfg.vision_enabled || !has_images || model_has_vision {
        return VisionDecision::Keep;
    }
    let vision_model = cfg.vision_model.trim();
    if vision_model.is_empty() {
        // 未配置视觉模型 = 无从识图/切换，保持原行为（记一次告警由调用方做）
        return VisionDecision::Keep;
    }
    if cfg.mode.trim().eq_ignore_ascii_case(MODE_SWITCH) {
        VisionDecision::Switch(vision_model.to_string())
    } else {
        VisionDecision::Describe
    }
}

/// 视觉备选链：主视觉模型在前，`fallback_models` 依次在后（去重、去空）。
///
/// 只在请求含图时才返回候选链（无图请求不因视觉配置改变路由与计费）。
/// 首个元素 = 实际发出的模型（switch 模式下即视觉模型）；其后为回退顺序。
pub fn vision_attempt_chain(
    primary: &str,
    cfg: &ImageCompatConfig,
    has_images: bool,
) -> Vec<String> {
    let mut chain: Vec<String> = Vec::new();
    if !has_images {
        return chain;
    }
    let primary = primary.trim();
    if !primary.is_empty() {
        chain.push(primary.to_string());
    }
    if cfg.vision_enabled {
        for model in &cfg.fallback_models {
            let model = model.trim();
            if model.is_empty() || chain.iter().any(|m| m == model) {
                continue;
            }
            chain.push(model.to_string());
        }
    }
    chain
}

/// 辅助识图：把消息里的内联图/外链图逐张交给辅助视觉模型生成描述，
/// 以文本块替换图片块。
///
/// - 一次请求内**同一张图（按内容哈希）只调用一次**，重复出现的图片复用描述；
/// - 单张失败保留原图（不阻断请求）；
/// - 最多处理 [`MAX_DESCRIBE_IMAGES`] 张，超出部分记日志后保留原图。
///
/// 返回被替换的图片块数量。
pub async fn describe_images(
    state: &crate::api::openai::AppState,
    messages: &mut [ChatMessage],
    cfg: &ImageCompatConfig,
) -> usize {
    let vision_model = cfg.vision_model.trim();
    if vision_model.is_empty() {
        return 0;
    }
    let prompt = cfg.effective_describe_prompt().to_string();
    // 1) 收集所有图片块位置与 URI，按内容哈希去重
    let mut slots: Vec<(usize, usize, String, String)> = Vec::new(); // (msg, block, hash, uri)
    for (mi, message) in messages.iter().enumerate() {
        for (bi, block) in message
            .content_blocks
            .as_deref()
            .into_iter()
            .flatten()
            .enumerate()
        {
            let Some(uri) = image_uri_of(block) else {
                continue;
            };
            let hash = sha256_hex(uri.as_bytes());
            slots.push((mi, bi, hash, uri.to_string()));
        }
    }
    if slots.is_empty() {
        return 0;
    }
    // 2) 逐张（去重后）调用辅助视觉模型
    let mut descriptions: HashMap<String, Option<String>> = HashMap::new();
    let mut unique_order: Vec<(String, String)> = Vec::new(); // (hash, uri)
    for (_, _, hash, uri) in &slots {
        if !descriptions.contains_key(hash) && !unique_order.iter().any(|(h, _)| h == hash) {
            unique_order.push((hash.clone(), uri.clone()));
        }
    }
    let mut budget = MAX_DESCRIBE_IMAGES;
    for (hash, uri) in &unique_order {
        if budget == 0 {
            tracing::warn!(
                "image_compat: describe skipped for remaining {} image(s) (cap {})",
                unique_order.len() - descriptions.len(),
                MAX_DESCRIBE_IMAGES
            );
            break;
        }
        budget -= 1;
        let desc = describe_one(state, vision_model, &prompt, uri).await;
        descriptions.insert(hash.clone(), desc);
    }
    // 3) 就地替换：同一哈希复用同一描述
    let mut replaced = 0usize;
    for (mi, bi, hash, _) in slots {
        let Some(Some(desc)) = descriptions.get(&hash) else {
            continue; // 识图失败：保留原图
        };
        let Some(blocks) = messages[mi].content_blocks.as_mut() else {
            continue;
        };
        let Some(block) = blocks.get_mut(bi) else {
            continue;
        };
        if replace_block_with_text(block, desc) {
            replaced += 1;
        }
    }
    replaced
}

/// 把图片块替换为文本块（保留 `[图片内容描述]` 标注，便于模型区分来源）。
fn replace_block_with_text(block: &mut Value, description: &str) -> bool {
    let Some(object) = block.as_object_mut() else {
        return false;
    };
    object.clear();
    object.insert("type".to_string(), Value::String("text".to_string()));
    object.insert(
        "text".to_string(),
        Value::String(format!("[图片内容描述] {description}")),
    );
    true
}

/// 单张图片 → 描述（失败返回 None）。进程内直调 bridge（不经计费/鉴权）。
async fn describe_one(
    state: &crate::api::openai::AppState,
    vision_model: &str,
    prompt: &str,
    uri: &str,
) -> Option<String> {
    let candidates =
        crate::api::openai::resolve_bridges_with_affinity(state, vision_model, None, None, None);
    if candidates.is_empty() {
        tracing::warn!("image_compat: no bridge available for vision model {vision_model}");
        return None;
    }
    let ctx = crate::bridge::BridgeContext::new(
        format!("imgdesc-{}", uuid::Uuid::new_v4()),
        vision_model.to_string(),
    );
    let message = ChatMessage {
        role: Role::User,
        content: Some(prompt.to_string()),
        content_blocks: Some(vec![
            serde_json::json!({ "type": "text", "text": prompt }),
            serde_json::json!({ "type": "image_url", "image_url": { "url": uri } }),
        ]),
        name: None,
        tool_call_id: None,
        tool_calls: None,
        reasoning: None,
    };
    for (bridge, cid, channel) in candidates {
        let upstream = crate::api::openai::resolve_upstream_model(
            vision_model,
            channel.as_ref(),
            &state.model_mapper,
        );
        let req = ChatFormat {
            model: upstream,
            messages: vec![message.clone()],
            tools: None,
            max_tokens: Some(512),
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
        match bridge.chat(&req, &ctx).await {
            Ok(resp) => {
                let text = resp.message.content_str().trim().to_string();
                if text.is_empty() {
                    tracing::warn!("image_compat: empty description from {cid:?}");
                    continue;
                }
                return Some(text);
            }
            Err(e) => {
                if !crate::api::openai::is_retryable_bridge_error(&e) {
                    tracing::warn!("image_compat: describe failed (non-retryable) on {cid:?}: {e}");
                    return None;
                }
                tracing::warn!("image_compat: describe failed on {cid:?}: {e}, trying next");
            }
        }
    }
    None
}

// ── 数据面 hook（chat / anthropic / responses 共用）──────────────────

/// 一次请求的图片兼容上下文（会话端点进入 failover 循环前构建一次）。
///
/// 构建时按快照裁决视觉决策（`describe` / `switch` / `keep`），后续每个候选
/// 发送前只做局部操作（压缩、识图、改路由），不重复读配置。
pub struct RequestImageContext {
    cfg: Arc<ImageCompatConfig>,
    decision: VisionDecision,
    /// 请求是否含图（与选路能力过滤共用同一信号）
    pub has_images: bool,
}

impl RequestImageContext {
    /// 构建上下文。`has_images` 来自 `needs::from_chat_body(...).images`。
    ///
    /// 视觉决策只看配置与目标模型能力位；`switch` 模式下若视觉模型未定价且
    /// 无渠道声明则降级为 `Keep`（B09，不产生免费用量）。
    pub fn build(state: &crate::api::openai::AppState, model: &str, has_images: bool) -> Self {
        let cfg = settings();
        let mut decision = vision_decision(&cfg, model_supports_vision(state, model), has_images);
        if let VisionDecision::Switch(vm) = &decision {
            if crate::api::openai::ensure_model_priced(state, vm).is_err() {
                tracing::warn!(
                    "image_compat: vision model '{vm}' is not priced/declared, keep original route"
                );
                decision = VisionDecision::Keep;
            }
        }
        Self {
            cfg,
            decision,
            has_images,
        }
    }

    /// 生效配置快照。
    pub fn cfg(&self) -> &ImageCompatConfig {
        &self.cfg
    }

    /// 视觉决策。
    pub fn decision(&self) -> &VisionDecision {
        &self.decision
    }

    /// 本次请求实际应路由到的模型（`switch` = 视觉模型，其它 = 原模型）。
    pub fn routing_model(&self, requested: &str) -> String {
        match &self.decision {
            VisionDecision::Switch(vm) => vm.clone(),
            _ => requested.to_string(),
        }
    }

    /// 单次尝试发送前的就地处理（在脱敏之后、bridge 调用之前）：
    ///
    /// 1. `describe` 模式：把图片块替换为辅助识图文本（同一图只调用一次）；
    /// 2. 压缩：把剩余内联大图缩边重编码（关闭时零开销）。
    ///
    /// 返回是否发生了任何改写（调用方据此记 debug 日志）。
    pub async fn prepare_attempt(
        &self,
        state: &crate::api::openai::AppState,
        messages: &mut Vec<ChatMessage>,
    ) -> bool {
        if !self.has_images {
            return false;
        }
        let mut changed = false;
        if matches!(self.decision, VisionDecision::Describe) {
            let replaced = describe_images(state, messages, &self.cfg).await;
            if replaced > 0 {
                tracing::info!("image_compat: {replaced} image(s) replaced with descriptions");
                changed = true;
            }
        }
        if self.cfg.enabled && messages_have_data_images(messages) {
            // 压缩是纯 CPU（解码/缩放/编码）：放到阻塞线程池，别卡 tokio worker。
            // 用 Arc<Vec<ChatMessage>> 让任务在 panic 时也不丢原消息（最坏退回
            // 未压缩版本，绝不发空 messages）。
            let cfg = self.cfg.clone();
            let owned = Arc::new(std::mem::take(messages));
            let task_owned = owned.clone();
            match tokio::task::spawn_blocking(move || {
                let mut work: Vec<ChatMessage> = (*task_owned).clone();
                let stats = compress_messages(&mut work, &cfg);
                (work, stats)
            })
            .await
            {
                Ok((back, stats)) => {
                    *messages = back;
                    if stats.changed() {
                        tracing::debug!(
                            "image_compat: compressed {} image(s), {} -> {} bytes",
                            stats.compressed,
                            stats.bytes_before,
                            stats.bytes_after
                        );
                        changed = true;
                    }
                }
                Err(e) => {
                    // JoinError（panic/cancel）：退回压缩前的原消息，请求照常发出
                    tracing::warn!("image_compat: compression task failed: {e}, keep original");
                    *messages = (*owned).clone();
                }
            }
        }
        changed
    }

    /// responses 透传路径的单次尝试处理（请求体形态）。
    pub fn prepare_responses_body(&self, body: &mut Value) -> bool {
        if !self.has_images || !self.cfg.enabled {
            return false;
        }
        // describe 需要 async 上游调用，responses 透传路径只在压缩层接线
        //（Responses 请求体的图片描述替换由 chat 降级路径覆盖）
        if !messages_have_data_images_in_value(body) {
            return false;
        }
        let stats = compress_responses_body(body, &self.cfg);
        if stats.changed() {
            tracing::debug!(
                "image_compat: compressed {} image(s) (responses), {} -> {} bytes",
                stats.compressed,
                stats.bytes_before,
                stats.bytes_after
            );
            return true;
        }
        false
    }
}

/// `input[].content[]` 里是否有可压的内联图（只读早退，避免无谓深拷贝）。
fn messages_have_data_images_in_value(body: &Value) -> bool {
    let Some(items) = body.get("input").and_then(Value::as_array) else {
        return false;
    };
    items.iter().any(|item| {
        item.get("content")
            .and_then(Value::as_array)
            .is_some_and(|blocks| {
                blocks
                    .iter()
                    .any(|b| image_uri_of(b).is_some_and(|u| u.starts_with("data:image/")))
            })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> ImageCompatConfig {
        ImageCompatConfig::default()
    }

    fn png_data_uri(width: u32, height: u32) -> String {
        // 噪声图（确定性 LCG）：大片噪声在缩边后必然显著变小，
        // 「重编码后体积下降」的断言不依赖具体压缩器的启发式。
        let img = noise_rgb(width, height);
        let mut out: Vec<u8> = Vec::new();
        {
            use image::ImageEncoder as _;
            image::codecs::png::PngEncoder::new(&mut out)
                .write_image(&img, width, height, image::ExtendedColorType::Rgb8)
                .expect("encode png");
        }
        format!("data:image/png;base64,{}", DATA_URL_BASE64.encode(&out))
    }

    /// 确定性伪随机 RGB 图（不依赖 rand 的全局状态）。
    fn noise_rgb(width: u32, height: u32) -> image::RgbImage {
        let mut img = image::RgbImage::new(width, height);
        let mut state: u32 = 0x1234_5678;
        for pixel in img.pixels_mut() {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            *pixel = image::Rgb([(state >> 8) as u8, (state >> 16) as u8, (state >> 24) as u8]);
        }
        img
    }

    /// 噪声图 JPEG data URI（高质量输入 ⇒ 缩放 + 默认质量重编码必然更小）。
    fn jpeg_data_uri(width: u32, height: u32, quality: u8) -> String {
        let img = noise_rgb(width, height);
        let mut out: Vec<u8> = Vec::new();
        let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality);
        encoder.encode_image(&img).expect("encode jpeg");
        format!("data:image/jpeg;base64,{}", DATA_URL_BASE64.encode(&out))
    }

    /// 噪声图无损 WebP data URI（输入侧由本仓仅有 new_lossless 编码器产出）。
    fn webp_data_uri(width: u32, height: u32) -> String {
        let img = noise_rgb(width, height);
        let mut out: Vec<u8> = Vec::new();
        image::codecs::webp::WebPEncoder::new_lossless(&mut out)
            .encode(&img, width, height, image::ExtendedColorType::Rgb8)
            .expect("encode webp");
        format!("data:image/webp;base64,{}", DATA_URL_BASE64.encode(&out))
    }

    fn message_with_image(uri: &str) -> ChatMessage {
        ChatMessage {
            role: Role::User,
            content: None,
            content_blocks: Some(vec![
                serde_json::json!({ "type": "text", "text": "看图" }),
                serde_json::json!({
                    "type": "image_url",
                    "image_url": { "url": uri, "detail": "high" },
                }),
            ]),
            name: None,
            tool_call_id: None,
            tool_calls: None,
            reasoning: None,
        }
    }

    fn uri_of(message: &ChatMessage) -> String {
        let blocks = message.content_blocks.as_deref().expect("blocks");
        blocks[1]["image_url"]["url"]
            .as_str()
            .expect("url")
            .to_string()
    }

    #[test]
    fn compression_shrinks_long_edge_and_bytes_and_keeps_format() {
        clear_cache();
        let uri = png_data_uri(2400, 600);
        let mut messages = vec![message_with_image(&uri)];
        let mut settings = cfg();
        settings.threshold_kb = 0; // 小图也要压，先把长边验证跑通
        let stats = compress_messages(&mut messages, &settings);
        assert_eq!(stats.compressed, 1, "应发生一次替换");
        assert!(stats.bytes_after < stats.bytes_before, "体积应下降");
        let next = uri_of(&messages[0]);
        assert!(next.starts_with("data:image/png;base64,"), "按原格式重编码");
        // 解码输出，验证长边约束
        let (kind, body) = split_data_uri(&next).expect("输出仍是合法 data URI");
        assert_eq!(kind, ImageKind::Png);
        let bytes = DATA_URL_BASE64.decode(body).expect("base64");
        let decoded = image::load_from_memory(&bytes).expect("decode");
        assert!(
            decoded.width().max(decoded.height()) <= DEFAULT_MAX_EDGE,
            "长边应缩到上限内: {}x{}",
            decoded.width(),
            decoded.height()
        );
        // detail 等其它字段保留
        let blocks = messages[0].content_blocks.as_deref().expect("blocks");
        assert_eq!(blocks[1]["image_url"]["detail"], "high");
    }

    #[test]
    fn same_image_reuses_cached_result() {
        clear_cache();
        // 用高质量噪声 JPEG：重编码（质量 85 + 缩放）必然小于原图，
        // 断言不依赖 PNG 重压缩的启发式结果。
        let uri = jpeg_data_uri(1600, 400, 95);
        let mut messages = vec![message_with_image(&uri)];
        let mut settings = cfg();
        settings.threshold_kb = 0;
        let first = compress_messages(&mut messages, &settings);
        assert_eq!(first.compressed, 1);
        let after_first = cache_entry_count();
        let first_uri = uri_of(&messages[0]);
        // 第二轮（模拟多轮对话重发同一张原图）：结果一致，且不新增缓存条目
        let mut again = vec![message_with_image(&uri)];
        let second = compress_messages(&mut again, &settings);
        assert_eq!(second.compressed, 1);
        assert_eq!(uri_of(&again[0]), first_uri, "同图应复用同一压缩结果");
        assert_eq!(cache_entry_count(), after_first, "命中缓存不应新增条目");
    }

    #[test]
    fn below_threshold_and_foreign_uris_are_untouched() {
        clear_cache();
        // 低于阈值：原样（缓存也记录 None，重复不重算）
        let small = png_data_uri(32, 32);
        let mut messages = vec![message_with_image(&small)];
        let stats = compress_messages(&mut messages, &cfg());
        assert_eq!(stats.compressed, 0);
        assert_eq!(uri_of(&messages[0]), small);
        // http(s) 外链 / gif / svg：不抓取不压缩（即使阈值 0）
        let mut settings = cfg();
        settings.threshold_kb = 0;
        for uri in [
            "https://example.com/a.png",
            "data:image/gif;base64,R0lGODlhAQABAAAAACw=",
            "data:image/svg+xml;base64,PHN2Zy8+",
        ] {
            let mut messages = vec![message_with_image(uri)];
            let stats = compress_messages(&mut messages, &settings);
            assert_eq!(stats.compressed, 0, "{uri} 不应被处理");
            assert_eq!(uri_of(&messages[0]), uri);
        }
        // 坏 base64：原样且不 panic
        let mut messages = vec![message_with_image("data:image/png;base64,!!!!")];
        let stats = compress_messages(&mut messages, &settings);
        assert_eq!(stats.compressed, 0);
    }

    #[test]
    fn vision_decision_defaults_to_no_change() {
        let mut cfg = cfg();
        cfg.vision_model = "qwen-vl".to_string();
        // 总开关关闭（默认）：即使模型无 vision、请求有图也不干预
        assert_eq!(
            vision_decision(&cfg, false, true),
            VisionDecision::Keep,
            "默认关闭时行为不变"
        );
        // 无图 / 模型本身支持 vision：不干预
        cfg.vision_enabled = true;
        assert_eq!(vision_decision(&cfg, false, false), VisionDecision::Keep);
        assert_eq!(vision_decision(&cfg, true, true), VisionDecision::Keep);
        // describe（默认模式）
        assert_eq!(vision_decision(&cfg, false, true), VisionDecision::Describe);
        // switch
        cfg.mode = MODE_SWITCH.to_string();
        assert_eq!(
            vision_decision(&cfg, false, true),
            VisionDecision::Switch("qwen-vl".to_string())
        );
        // 未配置视觉模型：不干预（describe/switch 都无目标）
        cfg.vision_model.clear();
        assert_eq!(vision_decision(&cfg, false, true), VisionDecision::Keep);
        // 非法 mode 归一为 describe
        let mut cfg2 = cfg.clone();
        cfg2.vision_model = "vm".to_string();
        cfg2.mode = "weird".to_string();
        assert_eq!(cfg2.normalized().mode, MODE_DESCRIBE);
    }

    #[test]
    fn describe_replaces_image_blocks_with_text_and_dedupes() {
        // 纯替换语义：同一张图的两处块用同一描述文本替换
        let uri = "data:image/png;base64,AAAA";
        let mut messages = vec![message_with_image(uri), message_with_image(uri)];
        let desc = "一只在窗台上的橘猫";
        let mut replaced = 0;
        for message in messages.iter_mut() {
            for block in message.content_blocks.as_mut().into_iter().flatten() {
                if image_uri_of(block).is_some() && replace_block_with_text(block, desc) {
                    replaced += 1;
                }
            }
        }
        assert_eq!(replaced, 2);
        for message in &messages {
            let blocks = message.content_blocks.as_deref().expect("blocks");
            assert_eq!(blocks[1]["type"], "text");
            let text = blocks[1]["text"].as_str().expect("text");
            assert!(text.contains(desc), "描述应进入文本块: {text}");
            assert!(text.contains("[图片内容描述]"), "应标注来源");
            // 文本块保持原位（顺序不乱）
            assert_eq!(blocks[0]["text"], "看图");
        }
    }

    #[test]
    fn vision_attempt_chain_orders_fallbacks_and_dedupes() {
        let mut cfg = cfg();
        cfg.vision_enabled = true;
        cfg.fallback_models = vec![
            "fallback-a".to_string(),
            "fallback-b".to_string(),
            "primary".to_string(), // 与主模型同名：去重
            "  ".to_string(),      // 空白：丢弃
        ];
        assert_eq!(
            vision_attempt_chain("primary", &cfg, true),
            vec![
                "primary".to_string(),
                "fallback-a".to_string(),
                "fallback-b".to_string()
            ],
            "回退顺序必须与配置顺序一致，且主模型去重"
        );
        // 无图请求：不改变路由
        assert!(vision_attempt_chain("primary", &cfg, false).is_empty());
        // 总开关关闭：只有主模型
        cfg.vision_enabled = false;
        assert_eq!(
            vision_attempt_chain("primary", &cfg, true),
            vec!["primary".to_string()]
        );
    }

    #[test]
    fn config_normalization_clamps_out_of_range_values() {
        let raw = ImageCompatConfig {
            enabled: false,
            max_edge: 1,
            quality: 0,
            threshold_kb: 99_999,
            vision_enabled: true,
            mode: "SWITCH".to_string(),
            vision_model: "  vm  ".to_string(),
            fallback_models: vec![" a ".to_string(), String::new()],
            describe_prompt: String::new(),
        };
        let n = raw.normalized();
        assert_eq!(n.max_edge, MAX_EDGE_MIN);
        assert_eq!(n.quality, QUALITY_MIN);
        assert_eq!(n.threshold_kb, THRESHOLD_KB_MAX);
        assert_eq!(n.mode, MODE_SWITCH, "模式大小写不敏感");
        assert_eq!(n.vision_model, "vm");
        assert_eq!(n.fallback_models, vec!["a".to_string()]);
        assert_eq!(n.effective_describe_prompt(), DEFAULT_DESCRIBE_PROMPT);
    }

    #[test]
    fn responses_body_compression_walks_input_blocks() {
        clear_cache();
        let uri = png_data_uri(2000, 500);
        let mut body = serde_json::json!({
            "input": [{
                "type": "message",
                "role": "user",
                "content": [
                    { "type": "input_text", "text": "看图" },
                    { "type": "input_image", "image_url": uri },
                ],
            }],
        });
        let mut settings = cfg();
        settings.threshold_kb = 0;
        let stats = compress_responses_body(&mut body, &settings);
        assert_eq!(stats.compressed, 1);
        let next = body["input"][0]["content"][1]["image_url"]
            .as_str()
            .expect("url");
        assert!(next.starts_with("data:image/png;base64,"));
        assert_ne!(next, uri);
    }
}
