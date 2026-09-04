//! 渠道管理 — 通用上游渠道模型与调度。
//!
//! 参照 aisix 的 provider/routing 设计（渠道、优先级、failover）与
//! new-api 的 channel.go 数据模型（Type/Key/Status/Weight/BaseURL/Models/Priority/Group）。
//!
//! 当前 src/account/mod.rs 仅支持 Cloudflare 账号池；本模块引入通用 Channel，
//! 可混用 Cloudflare 与第三方 OpenAI 兼容上游（DeepSeek/OpenRouter 等）。
//! 调度时按 priority 降序、weight 加权选取支持目标 model 的 enabled 渠道，
//! 调用失败自动标记不健康并尝试下一个渠道（failover）。
//!
//! api_key 采用 base64 编码静态保护（避免明文落盘），可升级为 AES-GCM。

use base64::{engine::general_purpose, Engine as _};
use parking_lot::RwLock;
use rand::seq::SliceRandom;
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::storage::FileStore;

// ── Router 核心增强子模块（阶段2）─────────────────────────────────────
//
// 参照 burncloud `crates/router/src/` 的对应模块，在 AIGX 单 crate 内以子模块
// 形式实现。各模块自包含、可独立测试，由 `ChannelStore` 持有并集成到渠道选择。
pub mod affinity;
pub mod aimd;
pub mod balancer;
pub mod circuit_breaker;
pub mod empty_response;
pub mod health_manager;
pub mod rate_budget;
// ── 阶段3：响应质量分级（喂给断路器/调度器）──────────────────────────
//
// 参照 burncloud `crates/router/src/response_quality.rs`，评估上游响应质量
// （延迟/错误率/吞吐量），用于渠道选择。
pub mod response_quality;
pub mod scheduler;

// ── 渠道类型 ─────────────────────────────────────────────────────────

/// 渠道类型枚举。
///
/// 参照 aisix Adapter 族 + new-api channel.Type：保留 Cloudflare 专用，
/// 新增 OpenAI 兼容与 Anthropic 兼容类型，支持混用上游。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum ChannelType {
    /// Cloudflare Workers AI（专用桥接）
    #[default]
    Cloudflare,
    /// OpenAI 兼容协议（DeepSeek/OpenRouter/Together 等）
    OpenaiCompatible,
    /// Anthropic 兼容协议
    Anthropic,
}

impl ChannelType {
    /// 从字符串解析渠道类型
    pub fn from_str_lossy(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "openai_compatible" | "openai-compatible" | "openai" => Self::OpenaiCompatible,
            "anthropic" => Self::Anthropic,
            _ => Self::Cloudflare,
        }
    }

    /// 转为字符串
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Cloudflare => "cloudflare",
            Self::OpenaiCompatible => "openai_compatible",
            Self::Anthropic => "anthropic",
        }
    }
}

// ── Channel 数据结构 ────────────────────────────────────────────────

/// 通用上游渠道。
///
/// 参照 new-api Channel：id/name/type/base_url/api_key/priority/weight/status/models。
/// api_key 以 base64 编码存储（前缀 `enc:` 标识），避免明文落盘。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Channel {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub channel_type: ChannelType,
    /// 上游基础 URL（OpenAI 兼容：`https://api.deepseek.com`；CF 留空）
    #[serde(default)]
    pub base_url: String,
    /// 编码后的 api_key（`enc:` 前缀 + base64）
    #[serde(default)]
    pub api_key: String,
    /// 优先级（数值越大越优先）
    #[serde(default)]
    pub priority: i64,
    /// 权重（同优先级下加权随机）
    #[serde(default = "default_weight")]
    pub weight: u32,
    /// 状态：enabled / disabled
    #[serde(default = "default_status")]
    pub status: String,
    /// 支持的模型列表（逗号分隔或数组）
    #[serde(default)]
    pub models: Vec<String>,
    /// Cloudflare account_id（仅 channel_type=cloudflare 时使用）
    #[serde(default)]
    pub account_id: String,
    /// 最近错误信息（健康检查用）
    #[serde(default)]
    pub last_error: Option<String>,
    /// 最近使用时间戳
    #[serde(default)]
    pub last_used_at: Option<i64>,
    /// 上游最近一次返回的成功模型列表快照（模型自动发现）。
    ///
    /// 拉取自渠道的 `/models` 端点；调度时若渠道已显式配置 models 则优先用
    /// 配置，未配置时尝试用该快照判断模型支持度。仅在"自动发现模型"场景填充。
    #[serde(default)]
    pub discovered_models: Vec<String>,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
}

fn default_weight() -> u32 {
    1
}

fn default_status() -> String {
    "enabled".to_string()
}

impl Channel {
    /// 是否启用
    pub fn is_enabled(&self) -> bool {
        self.status == "enabled"
    }

    /// 是否支持指定模型（models 为空视为全支持）
    pub fn supports_model(&self, model: &str) -> bool {
        if self.models.is_empty() {
            return true;
        }
        self.models.iter().any(|m| m == model)
    }

    /// 编码 api_key 用于存储。
    ///
    /// ⚠️ B11（已知风险，保留现状）：这是可逆的 Base64“混淆”而非加密——
    /// 密钥与密文同存于同一存储，无法防御能读取存储文件的攻击者，仅避免
    /// 明文密钥在浏览配置/备份/日志时被直接看到。
    /// 升级路径（后续版本）：引入 master key（环境变量或独立 KMS）做
    /// AES-GCM 对称加密，并为存量 `enc:` 数据提供迁移解码；因涉及存储
    /// 格式兼容与部署流程变更，超出本次修复范围，此处仅文档化。
    pub fn encode_api_key(plain: &str) -> String {
        if plain.is_empty() {
            return String::new();
        }
        format!("enc:{}", general_purpose::STANDARD.encode(plain.as_bytes()))
    }

    /// 解码存储中的 api_key（安全性说明见 `encode_api_key`）
    pub fn decode_api_key(&self) -> String {
        if let Some(rest) = self.api_key.strip_prefix("enc:") {
            general_purpose::STANDARD
                .decode(rest)
                .ok()
                .and_then(|b| String::from_utf8(b).ok())
                .unwrap_or_default()
        } else {
            self.api_key.clone()
        }
    }
}

// ── 渠道连通性测试结果 ──────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelTestResult {
    pub success: bool,
    pub message: String,
    pub latency_ms: u64,
}

// ── ChannelStore ────────────────────────────────────────────────────

/// 渠道存储 — 管理通用上游渠道，支持 CRUD、按模型调度选取与健康检查。
///
/// 参照 aisix Hub 的两级分发 + new-api channel_cache 的选取逻辑。
///
/// 阶段2增强（参照 burncloud crates/router/src/）：
/// - `circuit_breaker`：per-channel 断路器，`select_for_model` 过滤时跳过 Open 渠道
/// - `affinity_cache`：session+model 粘性路由，`select_for_model_with_affinity` 优先命中
/// - `health_tracker`：per-channel/model 健康状态追踪，`record_channel_*` 时记录
/// - `empty_response_counter`：连续空响应计数，超阈值由调用方触发故障处理
/// - `balancer`：RoundRobin 负载均衡，`select_for_model_round_robin` 使用
/// - `aimd`：per-channel AIMD 自适应限速（批次3）
/// - `rate_budget`：per-channel 三色令牌桶预算（批次3）
/// - `scheduler`：CombinedScheduler 多因子调度（批次3）
pub struct ChannelStore {
    channels: RwLock<Vec<Channel>>,
    store: Arc<FileStore>,
    /// channel cooldown table: channel_id -> cooldown expiry. Cooled-down channels are skipped during scheduling.
    cooldowns: dashmap::DashMap<String, Instant>,
    /// per-channel 断路器（阶段2）
    circuit_breaker: circuit_breaker::CircuitBreaker,
    /// session+model 亲和性缓存（阶段2）
    affinity_cache: affinity::AffinityCache,
    /// per-channel/model 健康状态追踪器（阶段2）
    health_tracker: health_manager::ChannelStateTracker,
    /// 连续空响应计数器（阶段2）
    empty_response_counter: empty_response::EmptyResponseCounter,
    /// RoundRobin 负载均衡器（阶段2）
    balancer: balancer::RoundRobinBalancer,
    /// per-channel AIMD 自适应限速器（批次3）
    aimd: dashmap::DashMap<String, parking_lot::Mutex<aimd::AimdController>>,
    /// 三色令牌桶预算（批次3）
    rate_budget: rate_budget::InMemoryBudget,
    /// 组合调度器（批次3）
    scheduler: parking_lot::RwLock<scheduler::CombinedScheduler>,
    /// 调度器权重配置缓存（读取用）
    scheduler_config_cache: parking_lot::RwLock<scheduler::SchedulerPolicyConfig>,
}

impl ChannelStore {
    pub fn new(store: Arc<FileStore>) -> Self {
        let s = Self {
            channels: RwLock::new(Vec::new()),
            store,
            cooldowns: dashmap::DashMap::new(),
            circuit_breaker: circuit_breaker::CircuitBreaker::with_defaults(),
            affinity_cache: affinity::AffinityCache::default(),
            health_tracker: health_manager::ChannelStateTracker::new(),
            empty_response_counter: empty_response::EmptyResponseCounter::new(),
            balancer: balancer::RoundRobinBalancer::new(),
            aimd: dashmap::DashMap::new(),
            rate_budget: rate_budget::InMemoryBudget::new(),
            scheduler: parking_lot::RwLock::new(scheduler::CombinedScheduler::default()),
            scheduler_config_cache: parking_lot::RwLock::new(
                scheduler::SchedulerPolicyConfig::default(),
            ),
        };
        let _ = s.load();
        s
    }

    /// 从存储加载所有渠道
    pub fn load(&self) -> anyhow::Result<()> {
        let keys = self.store.list("channel:")?;
        let mut channels = Vec::with_capacity(keys.len());
        for key in &keys {
            if let Some(ch) = self.store.get::<Channel>(key)? {
                channels.push(ch);
            }
        }
        channels.sort_by(|a, b| {
            b.priority
                .cmp(&a.priority)
                .then(a.created_at.cmp(&b.created_at))
        });
        *self.channels.write() = channels;
        Ok(())
    }

    fn persist(&self, ch: &Channel) -> anyhow::Result<()> {
        self.store.put(&format!("channel:{}", ch.id), ch)?;
        Ok(())
    }

    /// 列出所有渠道
    pub fn list(&self) -> Vec<Channel> {
        self.channels.read().clone()
    }

    /// 获取单个渠道
    pub fn get(&self, id: &str) -> Option<Channel> {
        self.channels.read().iter().find(|c| c.id == id).cloned()
    }

    /// 新增渠道
    pub fn add(&self, mut ch: Channel) -> anyhow::Result<Channel> {
        if ch.id.is_empty() {
            ch.id = uuid::Uuid::new_v4().to_string();
        }
        let now = chrono::Utc::now().timestamp();
        if ch.created_at == 0 {
            ch.created_at = now;
        }
        ch.updated_at = now;
        // 编码明文 api_key（若未编码）
        if !ch.api_key.is_empty() && !ch.api_key.starts_with("enc:") {
            ch.api_key = Channel::encode_api_key(&ch.api_key);
        }
        self.persist(&ch)?;
        self.channels.write().push(ch.clone());
        // 维持排序
        self.channels.write().sort_by(|a, b| {
            b.priority
                .cmp(&a.priority)
                .then(a.created_at.cmp(&b.created_at))
        });
        Ok(ch)
    }

    /// 更新渠道
    pub fn update(&self, id: &str, mut ch: Channel) -> anyhow::Result<Channel> {
        ch.id = id.to_string();
        ch.updated_at = chrono::Utc::now().timestamp();
        if !ch.api_key.is_empty() && !ch.api_key.starts_with("enc:") {
            ch.api_key = Channel::encode_api_key(&ch.api_key);
        }
        self.persist(&ch)?;
        let mut channels = self.channels.write();
        if let Some(pos) = channels.iter().position(|c| c.id == id) {
            channels[pos] = ch.clone();
        } else {
            channels.push(ch.clone());
        }
        channels.sort_by(|a, b| {
            b.priority
                .cmp(&a.priority)
                .then(a.created_at.cmp(&b.created_at))
        });
        Ok(ch)
    }

    /// 删除渠道
    pub fn remove(&self, id: &str) -> anyhow::Result<()> {
        self.store.delete(&format!("channel:{id}"))?;
        self.channels.write().retain(|c| c.id != id);
        Ok(())
    }

    /// 选取支持指定模型的可用渠道列表（按 priority 降序；同优先级按 weight 加权随机）。
    ///
    /// 参照 aisix routing：failover 时调用方依次尝试返回的渠道列表。
    ///
    /// 阶段2增强：过滤阶段额外跳过断路器打开的渠道（`circuit_breaker.allow_request`）。
    /// 该增强向后兼容——断路器默认全 Closed（`allow_request` 对未知渠道返回 true），
    /// 不影响既有行为；仅在 `record_channel_failure` 累计达阈值后才生效。
    ///
    /// 批次3增强：过滤阶段额外跳过 AIMD 冷却中的渠道（`aimd_allows`）。
    pub fn select_for_model(&self, model: &str) -> Vec<Channel> {
        let mut candidates: Vec<Channel> = self
            .channels
            .read()
            .iter()
            .filter(|c| {
                c.is_enabled()
                    && (c.supports_model(model)
                        || (!c.models.is_empty()
                            && !c.discovered_models.is_empty()
                            && c.discovered_models.iter().any(|m| m == model)))
                    && !self.is_in_cooldown(&c.id)
                    && self.circuit_breaker.allow_request(&c.id)
                    && self.aimd_allows(&c.id)
            })
            .cloned()
            .collect();

        // 按 priority 降序分组，组内按 weight 加权随机
        candidates.sort_by_key(|b| std::cmp::Reverse(b.priority));
        let mut result = Vec::with_capacity(candidates.len());
        let mut i = 0;
        let mut rng = rand::thread_rng();
        while i < candidates.len() {
            let prio = candidates[i].priority;
            let mut group = Vec::new();
            let mut total_weight = 0u32;
            while i < candidates.len() && candidates[i].priority == prio {
                total_weight += candidates[i].weight.max(1);
                group.push(i);
                i += 1;
            }
            // 加权随机抽取
            while !group.is_empty() {
                let pick = if total_weight == 0 {
                    // L2 安全性说明：此处 group 非空（外层 while 条件保证），
                    // 故 slice::choose 必返回 Some，unwrap 不会 panic。
                    // 实际上 total_weight 由各 weight.max(1) 累加，每项 ≥1，
                    // group 非空时 total_weight ≥1，此分支理论上不可达，属防御性回退。
                    group.choose(&mut rng).copied().unwrap()
                } else {
                    let mut r = rng.gen_range(0..total_weight);
                    let mut chosen = group[0];
                    for &idx in &group {
                        let w = candidates[idx].weight.max(1);
                        if r < w {
                            chosen = idx;
                            break;
                        }
                        r -= w;
                    }
                    chosen
                };
                result.push(candidates[pick].clone());
                total_weight = total_weight.saturating_sub(candidates[pick].weight.max(1));
                group.retain(|&x| x != pick);
            }
        }
        result
    }

    /// 渠道是否处于冷却期。
    pub fn is_in_cooldown(&self, id: &str) -> bool {
        if let Some(entry) = self.cooldowns.get(id) {
            if *entry > Instant::now() {
                return true;
            }
        }
        false
    }

    /// 将渠道置入冷却期（暂时不调度，冷却到期后自动恢复）。
    ///
    /// 参照 aisix RuntimeStatus::Cooldown：可重试错误发生时短期冷却，
    /// 而非永久 disable。冷却到期后在 `select_for_model` 中自然恢复。
    pub fn mark_cooldown(&self, id: &str, error: String, duration_secs: u64) {
        if !error.is_empty() {
            let mut channels = self.channels.write();
            if let Some(ch) = channels.iter_mut().find(|c| c.id == id) {
                ch.last_error = Some(error);
                ch.updated_at = chrono::Utc::now().timestamp();
            }
        }
        let expiry = Instant::now() + Duration::from_secs(duration_secs);
        self.cooldowns.insert(id.to_string(), expiry);
        tracing::warn!("channel {} cooled down for {}s", id, duration_secs);
    }

    /// 清除渠道冷却（手动恢复或健康检查通过后调用）。
    pub fn clear_cooldown(&self, id: &str) {
        self.cooldowns.remove(id);
    }

    /// 标记渠道不健康（记录错误信息，禁用）
    pub fn mark_unhealthy(&self, id: &str, error: String) {
        let mut channels = self.channels.write();
        if let Some(ch) = channels.iter_mut().find(|c| c.id == id) {
            ch.status = "disabled".to_string();
            ch.last_error = Some(error);
            ch.updated_at = chrono::Utc::now().timestamp();
            let snapshot = ch.clone();
            drop(channels);
            if let Err(e) = self.persist(&snapshot) {
                tracing::error!("Failed to persist channel {} mark_unhealthy: {}", id, e);
            }
        }
    }

    /// 标记渠道健康（恢复启用）
    pub fn mark_healthy(&self, id: &str) {
        let mut channels = self.channels.write();
        if let Some(ch) = channels.iter_mut().find(|c| c.id == id) {
            ch.status = "enabled".to_string();
            ch.last_error = None;
            ch.updated_at = chrono::Utc::now().timestamp();
            let snapshot = ch.clone();
            drop(channels);
            if let Err(e) = self.persist(&snapshot) {
                tracing::error!("Failed to persist channel {} mark_healthy: {}", id, e);
            }
        }
    }

    /// 标记渠道已使用
    pub fn mark_used(&self, id: &str) {
        let mut channels = self.channels.write();
        if let Some(ch) = channels.iter_mut().find(|c| c.id == id) {
            ch.last_used_at = Some(chrono::Utc::now().timestamp());
            let snapshot = ch.clone();
            drop(channels);
            if let Err(e) = self.persist(&snapshot) {
                tracing::error!("Failed to persist channel {} mark_used: {}", id, e);
            }
        }
    }

    /// 保存渠道的上游模型发现快照（模型自动发现）。
    ///
    /// 覆盖 `discovered_models` 并持久化。仅在渠道未显式配置 models 时，
    /// 调度才会参考该快照（见 `select_for_model`）。
    pub fn save_discovered_models(&self, id: &str, models: Vec<String>) -> anyhow::Result<()> {
        let mut channels = self.channels.write();
        let ch = channels
            .iter_mut()
            .find(|c| c.id == id)
            .ok_or_else(|| anyhow::anyhow!("channel not found"))?;
        ch.discovered_models = models;
        ch.updated_at = chrono::Utc::now().timestamp();
        let snapshot = ch.clone();
        drop(channels);
        self.persist(&snapshot)?;
        Ok(())
    }

    /// 测试渠道连通性。
    ///
    /// - OpenAI 兼容：GET {base_url}/models
    /// - Anthropic 兼容：POST {base_url}/v1/messages（最小请求，仅验证 401/403 而非 404）
    /// - Cloudflare：委托 AccountPool::test（调用方处理），此处返回 NotSupported
    pub async fn test(&self, ch: &Channel) -> ChannelTestResult {
        let start = std::time::Instant::now();
        let client = match reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                return ChannelTestResult {
                    success: false,
                    message: format!("HTTP client error: {e}"),
                    latency_ms: 0,
                }
            }
        };

        let key = ch.decode_api_key();
        // B24：各分支返回 (是否成功, 消息)。401/403 是密钥认证失败，
        // 必须判测试失败并给出明确提示（原先 Anthropic 分支把 401/403
        // 也判为成功，密钥失效的渠道会被误标记为健康）。
        let (result, message) = match ch.channel_type {
            ChannelType::OpenaiCompatible => {
                // 标准 OpenAI 路径是 /v1/models，但部分网关只有 /models，
                // 因此先试 /v1/models，404 时回退 /models。
                let base = ch.base_url.trim_end_matches('/');
                let url = format!("{base}/v1/models");
                let resp = client.get(&url).bearer_auth(&key).send().await;
                let resp = match resp {
                    Ok(r) if r.status().as_u16() == 404 => {
                        let fallback = format!("{base}/models");
                        client.get(&fallback).bearer_auth(&key).send().await
                    }
                    other => other,
                };
                match resp {
                    Ok(r) => {
                        let s = r.status().as_u16();
                        if s == 401 || s == 403 {
                            (false, format!("Auth failed: HTTP {s} (invalid api key?)"))
                        } else if (200..300).contains(&s) {
                            (true, "Channel reachable".to_string())
                        } else {
                            (false, format!("Channel returned HTTP {s}"))
                        }
                    }
                    Err(e) => {
                        return ChannelTestResult {
                            success: false,
                            message: format!("Request failed: {e}"),
                            latency_ms: start.elapsed().as_millis() as u64,
                        }
                    }
                }
            }
            ChannelType::Anthropic => {
                let url = format!("{}/v1/messages", ch.base_url.trim_end_matches('/'));
                let body = serde_json::json!({
                    "model": "claude-3-5-sonnet-20241022",
                    "max_tokens": 1,
                    "messages": [{"role": "user", "content": "ping"}]
                });
                let resp = client
                    .post(&url)
                    .header("x-api-key", &key)
                    .header("anthropic-version", "2023-06-01")
                    .json(&body)
                    .send()
                    .await;
                match resp {
                    Ok(r) => {
                        let s = r.status().as_u16();
                        // 200/400 表示渠道与密钥均正常（400 为模型名不匹配等请求错误）
                        if s == 401 || s == 403 {
                            (false, format!("Auth failed: HTTP {s} (invalid api key?)"))
                        } else if s == 200 || s == 400 {
                            (true, "Channel reachable".to_string())
                        } else {
                            (false, format!("Channel returned HTTP {s}"))
                        }
                    }
                    Err(e) => {
                        return ChannelTestResult {
                            success: false,
                            message: format!("Request failed: {e}"),
                            latency_ms: start.elapsed().as_millis() as u64,
                        }
                    }
                }
            }
            ChannelType::Cloudflare => {
                return ChannelTestResult {
                    success: false,
                    message: "Cloudflare channels are tested via account pool".to_string(),
                    latency_ms: start.elapsed().as_millis() as u64,
                }
            }
        };

        ChannelTestResult {
            success: result,
            message,
            latency_ms: start.elapsed().as_millis() as u64,
        }
    }

    // ── 阶段2：Router 核心增强集成点 ──────────────────────────────────
    //
    // 以下方法把 circuit_breaker / affinity / health_manager / empty_response /
    // balancer 集成到 ChannelStore。既有方法（mark_cooldown/mark_unhealthy/
    // mark_healthy/mark_used/select_for_model）签名与逻辑保留不变，仅新增。

    /// 带亲和性的渠道选取 — 先查 `(session_id, model)` 亲和缓存，命中且可用则置顶。
    ///
    /// 返回的列表与 `select_for_model` 同样按 priority/weight 排序，仅当亲和命中时
    /// 把命中渠道移到列表首位（若该渠道仍在候选池中）。
    ///
    /// `session_id` 为 None 时退化为 `select_for_model`。
    pub fn select_for_model_with_affinity(
        &self,
        model: &str,
        session_id: Option<&str>,
    ) -> Vec<Channel> {
        let mut candidates = self.select_for_model(model);
        if let Some(sid) = session_id {
            if let Some(affinity_id) = self.affinity_cache.lookup(sid, model) {
                // 把亲和命中渠道移到首位（若仍在候选池且断路器放行）
                if let Some(pos) = candidates.iter().position(|c| c.id == affinity_id) {
                    if pos > 0 {
                        let ch = candidates.remove(pos);
                        candidates.insert(0, ch);
                    }
                }
            }
        }
        candidates
    }

    /// 用 RoundRobin 从候选渠道中选下一个（同 priority 组内轮询）。
    ///
    /// 与 `select_for_model` 的加权随机不同，本方法提供确定性轮询，适合需要
    /// 均匀分布的场景。组 ID 用 model（同模型的候选组内轮询）。
    pub fn select_for_model_round_robin(&self, model: &str) -> Option<Channel> {
        let candidates = self.select_for_model(model);
        let group_id = format!("model:{model}");
        self.balancer.next(&group_id, &candidates).cloned()
    }

    /// 记入渠道请求成功 — 同步更新断路器、健康管理器、亲和性、空响应计数。
    ///
    /// 调用方在渠道请求成功后调用。`session_id` 与 `model` 用于建立亲和性；
    /// `latency_ms` 用于健康管理的延迟 EMA。
    pub fn record_channel_success(
        &self,
        channel_id: &str,
        model: Option<&str>,
        latency_ms: u64,
        session_id: Option<&str>,
    ) {
        self.circuit_breaker.record_success(channel_id);
        self.health_tracker
            .record_success(channel_id, model, latency_ms);
        self.empty_response_counter.reset(channel_id);
        if let (Some(sid), Some(m)) = (session_id, model) {
            self.affinity_cache.insert(sid, m, channel_id);
        }
    }

    /// 计入渠道请求失败 — 同步更新断路器与健康管理器，并按失败类型决定是否清除亲和性。
    ///
    /// 参照 burncloud P0-1 策略：ServerError/Timeout/ConnectionError → 清除亲和性
    /// （病渠道不应再被粘到）；RateLimited/AuthFailed/PaymentRequired/ModelNotFound/
    /// EmptyResponse → 保留亲和性（临时或永久故障，下次可能恢复）。
    pub fn record_channel_failure(
        &self,
        channel_id: &str,
        model: Option<&str>,
        failure_type: circuit_breaker::FailureType,
        error_message: &str,
        session_id: Option<&str>,
    ) {
        self.circuit_breaker
            .record_failure(channel_id, failure_type.clone());
        self.health_tracker
            .record_error(channel_id, model, &failure_type, error_message);

        // 批次3：限速类失败同步喂给 AIMD（降限额/进冷却）
        if let circuit_breaker::FailureType::RateLimited { retry_after, .. } = &failure_type {
            self.aimd_on_rate_limited(channel_id, *retry_after);
        }

        // P0-1 亲和性清除策略
        let should_evict = matches!(
            failure_type,
            circuit_breaker::FailureType::ServerError
                | circuit_breaker::FailureType::Timeout
                | circuit_breaker::FailureType::ConnectionError
        );
        if should_evict {
            if let (Some(sid), Some(m)) = (session_id, model) {
                self.affinity_cache.evict(sid, m);
            }
        }
    }

    /// 计入一次空响应 — 返回 `true` 表示连续空响应已超阈值（应触发故障处理）。
    ///
    /// 调用方在响应处理时检查 token 计数，零 token 则调用本方法；非零则调用
    /// `record_channel_success`（其内部会 reset 空响应计数）。
    /// 返回 `true` 时，调用方应额外调用 `record_channel_failure` 记入
    /// `FailureType::EmptyResponse`，或调用 `mark_cooldown` 置入冷却。
    pub fn record_empty_response(&self, channel_id: &str) -> bool {
        self.empty_response_counter.record_empty(channel_id)
    }

    /// 把 `BridgeError` 分类为断路器 `FailureType`（供 failover 集成用）。
    ///
    /// 调用方在 bridge 调用失败时，用本函数把 `BridgeError` 转成 `FailureType`，
    /// 再调用 `record_channel_failure`。本函数不依赖 `bridge` 模块的内部细节，
    /// 仅按 `BridgeError` 的公开变体映射。
    pub fn classify_bridge_error(e: &crate::bridge::BridgeError) -> circuit_breaker::FailureType {
        use crate::bridge::BridgeError;
        match e {
            BridgeError::AuthError(_) | BridgeError::InvalidUpstreamCredentials(_) => {
                circuit_breaker::FailureType::AuthFailed
            }
            BridgeError::RateLimited => circuit_breaker::FailureType::RateLimited {
                scope: circuit_breaker::RateLimitScope::Unknown,
                retry_after: None,
            },
            BridgeError::UpstreamStatus {
                status,
                retry_after,
                ..
            } => match *status {
                401 | 403 => circuit_breaker::FailureType::AuthFailed,
                402 => circuit_breaker::FailureType::PaymentRequired,
                404 => circuit_breaker::FailureType::ModelNotFound,
                429 => circuit_breaker::FailureType::RateLimited {
                    scope: circuit_breaker::RateLimitScope::Unknown,
                    retry_after: retry_after.as_ref().map(|d| d.as_secs()),
                },
                s if s >= 500 => circuit_breaker::FailureType::ServerError,
                _ => circuit_breaker::FailureType::ServerError,
            },
            BridgeError::Timeout { .. } => circuit_breaker::FailureType::Timeout,
            BridgeError::Transport(_) | BridgeError::StreamAborted => {
                circuit_breaker::FailureType::ConnectionError
            }
            BridgeError::ModelNotFound(_) => circuit_breaker::FailureType::ModelNotFound,
            // 解析失败/配置错误视为服务端错误（上游返回了坏数据）
            BridgeError::UpstreamDecode(_)
            | BridgeError::Config(_)
            | BridgeError::InvalidUpstreamConfig(_)
            | BridgeError::AllAccountsFailed(_) => circuit_breaker::FailureType::ServerError,
        }
    }

    // ── 阶段2：访问器（供管理面/监控面读取子模块状态）──────────────────

    /// 断路器引用（管理面重置/监控用）。
    pub fn circuit_breaker(&self) -> &circuit_breaker::CircuitBreaker {
        &self.circuit_breaker
    }

    /// 亲和性缓存引用（管理面/监控用）。
    pub fn affinity_cache(&self) -> &affinity::AffinityCache {
        &self.affinity_cache
    }

    /// 健康状态追踪器引用（管理面查询渠道健康汇总用）。
    pub fn health_tracker(&self) -> &health_manager::ChannelStateTracker {
        &self.health_tracker
    }

    /// 空响应计数器引用（管理面/监控用）。
    pub fn empty_response_counter(&self) -> &empty_response::EmptyResponseCounter {
        &self.empty_response_counter
    }

    /// RoundRobin 负载均衡器引用（管理面/测试用）。
    pub fn balancer(&self) -> &balancer::RoundRobinBalancer {
        &self.balancer
    }

    // ── 批次3：AIMD / 三色预算 / 组合调度 集成点 ───────────────────────

    /// 记入渠道请求成功到 AIMD（学习/上调限额）。
    pub fn aimd_on_success(&self, channel_id: &str, upstream_limit: Option<u32>) {
        let c = self
            .aimd
            .entry(channel_id.to_string())
            .or_insert_with(|| parking_lot::Mutex::new(aimd::AimdController::with_defaults()));
        c.lock().on_success(upstream_limit);
    }

    /// 记入 429 到 AIMD（降限额/进冷却）。
    pub fn aimd_on_rate_limited(&self, channel_id: &str, retry_after: Option<u64>) {
        let c = self
            .aimd
            .entry(channel_id.to_string())
            .or_insert_with(|| parking_lot::Mutex::new(aimd::AimdController::with_defaults()));
        c.lock().on_rate_limited(retry_after);
    }

    /// 记入 429 到 AIMD（降限额/进冷却）。
    pub fn aimd_on_rate_limited(&self, channel_id: &str, retry_after: Option<u64>) {
        let mut c = self
            .aimd
            .entry(channel_id.to_string())
            .or_insert_with(|| parking_lot::Mutex::new(aimd::AimdController::with_defaults()));
        c.lock().on_rate_limited(retry_after);
    }

    /// AIMD 是否放行该渠道（调度过滤用）。
    pub fn aimd_allows(&self, channel_id: &str) -> bool {
        self.aimd
            .get(channel_id)
            .map(|c| c.lock().check_available())
            .unwrap_or(true) // 未追踪 = 不限制
    }

    /// AIMD 只读快照（监控用）。
    pub fn aimd_snapshot(&self, channel_id: &str) -> Option<aimd::AimdSnapshot> {
        self.aimd.get(channel_id).map(|c| c.lock().snapshot())
    }

    /// 三色预算引用（try_consume/refund/配置）。
    pub fn rate_budget(&self) -> &rate_budget::InMemoryBudget {
        &self.rate_budget
    }

    /// 更新调度器权重配置。
    pub fn set_scheduler_config(&self, config: scheduler::SchedulerPolicyConfig) {
        *self.scheduler_config_cache.write() = config.clone();
        *self.scheduler.write() = scheduler::CombinedScheduler::new(config);
    }

    /// 调度器当前权重配置。
    pub fn scheduler_config(&self) -> scheduler::SchedulerPolicyConfig {
        self.scheduler_config_cache.read().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn store() -> ChannelStore {
        ChannelStore::new(Arc::new(FileStore::new(
            TempDir::new().unwrap().path().to_path_buf(),
        )))
    }

    fn sample(id: &str, prio: i64, models: Vec<&str>) -> Channel {
        Channel {
            id: id.to_string(),
            name: format!("ch-{id}"),
            channel_type: ChannelType::OpenaiCompatible,
            base_url: "https://api.deepseek.com".to_string(),
            api_key: "sk-test".to_string(),
            priority: prio,
            weight: 1,
            status: "enabled".to_string(),
            models: models.into_iter().map(String::from).collect(),
            account_id: String::new(),
            last_error: None,
            last_used_at: None,
            discovered_models: Vec::new(),
            created_at: 1,
            updated_at: 1,
        }
    }

    #[test]
    fn add_and_select() {
        let s = store();
        s.add(sample("a", 10, vec!["deepseek-chat"])).unwrap();
        s.add(sample("b", 20, vec!["deepseek-chat"])).unwrap();
        let picked = s.select_for_model("deepseek-chat");
        assert_eq!(picked.len(), 2);
        // priority 20 排前
        assert_eq!(picked[0].id, "b");
    }

    #[test]
    fn api_key_encoding() {
        let ch = sample("a", 1, vec![]);
        let enc = Channel::encode_api_key("sk-secret");
        assert!(enc.starts_with("enc:"));
        let mut ch2 = ch.clone();
        ch2.api_key = enc;
        assert_eq!(ch2.decode_api_key(), "sk-secret");
    }

    #[test]
    fn mark_unhealthy_disables() {
        let s = store();
        s.add(sample("a", 1, vec!["m"])).unwrap();
        s.mark_unhealthy("a", "boom".to_string());
        let ch = s.get("a").unwrap();
        assert_eq!(ch.status, "disabled");
        assert_eq!(ch.last_error.as_deref(), Some("boom"));
        // disabled 不被选中
        assert!(s.select_for_model("m").is_empty());
    }
}
