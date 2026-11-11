//! 统一模型管理 —— 把「渠道声明的模型」与「账号池里的模型」聚合到一张表。
//!
//! ## 为什么需要这一层
//!
//! 改造前模型散在三处：`Channel.models`（渠道白名单）、`Channel.discovered_models`
//! （上游拉取快照）、`AccountSession.models`（账号级白名单）。前两者进 `/v1/models`
//! 聚合与路由，后者只在选号时做二次收窄 —— 一个只在账号上声明的模型，管理员在
//! 任何页面上都看不到它。这一层把三者按 **provider** 归并成一行一个模型，
//! 附带来源（哪个渠道 / 哪个账号）、价格、能力位与启停状态。
//!
//! ## 分组口径
//!
//! 行粒度是 `(provider, model_id)`：
//! - 账号型渠道（`ChannelType::Account`）的 provider 取账号自己的 `provider`；
//! - 其余渠道取 `channel_type`（`openai_compatible` / `anthropic` / …）。
//!
//! 同一 provider 下同名模型只出一行，`sources` 列出全部承载方。
//!
//! ## 只聚合启用渠道
//!
//! 需求原话：「渠道管理里启用的才在模型管理里跟账号池的一起显示」。
//! 禁用渠道的模型不进视图 —— 它们本来就不可路由，列出来只会让人误以为在售。

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use crate::account_session::AccountSessionStore;
use crate::channel::{Channel, ChannelType};
use crate::model::metadata::ModelMetadataRegistry;
use crate::model::ModelMapper;
use crate::pricing::{ModelPrice, PricingStore};
use crate::storage::FileStore;

/// 规则存储键（与 `model_custom_map` / `model_meta:*` 同库）。
const RULES_KEY: &str = "model_rules";

/// 一条启停规则：粒度是 `(provider, model_id)`。
///
/// 用 `provider` 而不是全局 model 名：同一个 `gpt-4o` 可能同时由 openai 直连
/// 渠道和某个中转账号承载，关掉其中一家不该连带关掉另一家。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RuleEntry {
    pub provider: String,
    pub id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct RulesFile {
    #[serde(default)]
    disabled: Vec<RuleEntry>,
}

/// 模型启停规则存储（KV `model_rules`）。
pub struct ModelRules {
    disabled: RwLock<HashSet<RuleEntry>>,
    store: Arc<FileStore>,
}

impl ModelRules {
    pub fn new(store: Arc<FileStore>) -> Self {
        Self {
            disabled: RwLock::new(HashSet::new()),
            store,
        }
    }

    pub fn load(&self) -> anyhow::Result<()> {
        let raw = self.store.get::<RulesFile>(RULES_KEY)?;
        let mut set = HashSet::new();
        if let Some(file) = raw {
            set.extend(file.disabled);
        }
        *self.disabled.write() = set;
        Ok(())
    }

    fn persist(&self) -> anyhow::Result<()> {
        let mut disabled: Vec<RuleEntry> = self.disabled.read().iter().cloned().collect();
        disabled.sort();
        self.store.put(RULES_KEY, &RulesFile { disabled })
    }

    /// 该 `(provider, model)` 是否被管理员关闭。
    pub fn is_disabled(&self, provider: &str, model: &str) -> bool {
        self.disabled.read().contains(&RuleEntry {
            provider: provider.to_string(),
            id: model.to_string(),
        })
    }

    /// 设置启停；返回是否有变化。
    pub fn set_enabled(&self, provider: &str, model: &str, enabled: bool) -> anyhow::Result<bool> {
        let entry = RuleEntry {
            provider: provider.to_string(),
            id: model.to_string(),
        };
        let changed = {
            let mut set = self.disabled.write();
            if enabled {
                set.remove(&entry)
            } else {
                set.insert(entry)
            }
        };
        if changed {
            self.persist()?;
        }
        Ok(changed)
    }

    /// 全部禁用项（管理面回读 / 调试用）。
    pub fn all_disabled(&self) -> Vec<RuleEntry> {
        let mut list: Vec<RuleEntry> = self.disabled.read().iter().cloned().collect();
        list.sort();
        list
    }

    /// 模型在**所有**承载它的 provider 下都被关掉时返回 true。
    ///
    /// 数据面用它做入口拦截（不知道最终会走哪个 provider）。只要还有一家开着
    /// 就放行 —— 精确到渠道的过滤交给调度器，这里只挡「全关」的模型。
    pub fn fully_disabled(&self, providers: &[String], model: &str) -> bool {
        if providers.is_empty() {
            return false;
        }
        providers
            .iter()
            .all(|provider| self.is_disabled(provider, model))
    }
}

// ── 视图结构 ────────────────────────────────────────────────────────

/// 一个承载方：渠道直连，或账号池里的某个账号。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelSource {
    /// `channel` / `account`
    pub kind: String,
    /// 渠道 id（account 来源时也有：账号挂在某个账号型渠道下）
    pub channel_id: String,
    /// 渠道名或账号名
    pub name: String,
    /// 账号 id（仅 kind=account）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
}

/// 聚合后的单行模型。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManageModel {
    pub provider: String,
    pub provider_label: String,
    pub id: String,
    /// 行启用状态（**派生值**，SPEC §2.1）：默认绑定开着，**或**任一条别名开着。
    pub enabled: bool,
    /// 默认绑定（`id → id` 自映射）那颗开关 = `!rules.is_disabled(provider, id)`。
    /// 写入仍走既有 `POST /api/models/state` → `model_rules`（**存储零迁移**）。
    pub default_enabled: bool,
    pub sources: Vec<ModelSource>,
    pub owned_by: String,
    pub context_length: Option<u32>,
    pub capabilities: Vec<String>,
    /// 能力/上下文来源标记（Phase1-A4 capSources：user > modelsdev >
    /// builtin；null = 该字段无读数）。前端可信度展示用。
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub cap_sources: serde_json::Value,
    /// 定价条目（未定价为 null，前端据此高亮）
    pub price: Option<ModelPrice>,
    /// 模型倍率（`ratio_config.model_ratio`，缺省 1.0）
    pub model_ratio: f64,
    /// 该模型的对外别名（`model_custom_map` 里 target == 本模型的行）
    pub aliases: Vec<String>,
    /// 别名明细（含开关状态）。与 `aliases` 同源，前者是纯名字清单
    /// （老前端 / 老调用点用），后者带启用位（新前端渲染 chip 用）。
    ///
    /// 两个字段并存是刻意的：`aliases: Vec<String>` 已被 `/v1/models`
    /// 一类出口与既有前端消费，改成对象会让它们全部失效；新增字段
    /// 只增不改。
    #[serde(default)]
    pub alias_entries: Vec<AliasEntry>,
}

/// 一条别名映射的渲染视图。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AliasEntry {
    pub alias: String,
    pub target: String,
    /// 是否启用（关掉的别名仍出现在清单里，但不参与改名）
    pub enabled: bool,
    /// 映射级思考等级（`reasoning_for` 的快照）。None = 未设置。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
}

/// 左栏分组。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProviderGroup {
    pub id: String,
    pub label: String,
    pub total: usize,
    pub enabled: usize,
    /// 是否来自账号池（前端用于区分图标/提示）
    pub account: bool,
}

/// 完整视图。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManageView {
    pub providers: Vec<ProviderGroup>,
    pub models: Vec<ManageModel>,
    /// 全局别名映射（alias → target）
    pub mappings: HashMap<String, String>,
    /// 未定价的模型数（顶部统计）
    pub unpriced: usize,
}

// ── provider 展示名 ─────────────────────────────────────────────────

/// provider kind → 中文展示名。
///
/// 账号池的 provider 直接查 registry；渠道类型走下面这张小表。
pub fn provider_label(kind: &str) -> String {
    if let Some(spec) = crate::account_session::registry::find(kind) {
        return spec.label.to_string();
    }
    match kind {
        "openai_compatible" => "OpenAI 兼容".to_string(),
        "cloudflare" => "Cloudflare Workers AI".to_string(),
        "anthropic" => "Anthropic".to_string(),
        "gemini" => "Google Gemini".to_string(),
        "zai" => "智谱 Z.AI".to_string(),
        "account" => "账号池".to_string(),
        other => other.to_string(),
    }
}

/// 渠道类型是否属于「账号容器」。
fn is_account_channel(channel: &Channel) -> bool {
    channel.channel_type == ChannelType::Account
}

// ── 聚合 ────────────────────────────────────────────────────────────

/// 构建统一模型视图。
///
/// 只读：不改任何存储。调用方（HTTP handler）负责鉴权。
pub fn build_view(
    channels: &[Channel],
    sessions: &AccountSessionStore,
    rules: &ModelRules,
    metadata: &ModelMetadataRegistry,
    pricing: &PricingStore,
    mapper: &ModelMapper,
) -> ManageView {
    // (provider, model) → 行
    let mut rows: BTreeMap<(String, String), ManageModel> = BTreeMap::new();
    // provider → 是否账号池来源
    let mut account_providers: HashSet<String> = HashSet::new();

    // 预取账号池：按渠道分组，避免每渠道重复扫描全量账号。
    let mut sessions_by_channel: HashMap<
        String,
        Vec<crate::account_session::model::AccountSession>,
    > = HashMap::new();
    for session in sessions.all() {
        sessions_by_channel
            .entry(session.channel_id.clone())
            .or_default()
            .push(session);
    }

    for channel in channels {
        if !channel.is_enabled() {
            continue;
        }

        if is_account_channel(channel) {
            // 账号型渠道：模型来自账号（账号白名单非空时以账号为准，
            // 否则回落到渠道 models / discovered_models）。
            let empty = Vec::new();
            let channel_sessions = sessions_by_channel.get(&channel.id).unwrap_or(&empty);
            for session in channel_sessions {
                if session.status != "enabled" {
                    continue;
                }
                let provider = if session.provider.is_empty() {
                    crate::account_session::registry::DEFAULT_PROVIDER_KIND
                } else {
                    session.provider.as_str()
                };
                account_providers.insert(provider.to_string());
                let models: Vec<String> = if session.models.is_empty() {
                    // 白名单为空：先回落该账号的目录缓存（SPEC §3.3，与
                    // collect_available_models **同口径**），仍空才回落渠道声明。
                    let cached = crate::account_session::catalog::cached_model_ids(
                        sessions,
                        &session.provider,
                        &session.id,
                    );
                    if cached.is_empty() {
                        channel.effective_models().to_vec()
                    } else {
                        cached
                    }
                } else {
                    session.models.clone()
                };
                for model in models {
                    if model.is_empty() {
                        continue;
                    }
                    let row = rows
                        .entry((provider.to_string(), model.clone()))
                        .or_insert_with(|| new_row(provider, &model, metadata, pricing, mapper));
                    row.sources.push(ModelSource {
                        kind: "account".to_string(),
                        channel_id: channel.id.clone(),
                        name: if session.name.is_empty() {
                            session.id.clone()
                        } else {
                            session.name.clone()
                        },
                        account_id: Some(session.id.clone()),
                    });
                }
            }
            // 账号型渠道本身不贡献渠道级来源（它的模型就是账号的模型）。
            continue;
        }

        let provider = channel.channel_type.as_str();
        for model in channel.effective_models() {
            if model.is_empty() {
                continue;
            }
            let row = rows
                .entry((provider.to_string(), model.clone()))
                .or_insert_with(|| new_row(provider, model, metadata, pricing, mapper));
            // 同一渠道既可能来自 models 也可能来自 discovered_models，
            // effective_models 已去重，这里再按 channel_id 去重来源。
            if !row.sources.iter().any(|s| s.channel_id == channel.id) {
                row.sources.push(ModelSource {
                    kind: "channel".to_string(),
                    channel_id: channel.id.clone(),
                    name: channel.name.clone(),
                    account_id: None,
                });
            }
        }
    }

    // 自定义模型登记（管理员手动添加、上游未发现的模型）
    for entry in mapper.custom_models() {
        if entry.id.is_empty() {
            continue;
        }
        rows.entry((entry.provider.clone(), entry.id.clone()))
            .or_insert_with(|| new_row(&entry.provider, &entry.id, metadata, pricing, mapper));
    }

    // 启停状态 + 分组统计
    let mut groups: BTreeMap<String, ProviderGroup> = BTreeMap::new();
    let mut models: Vec<ManageModel> = Vec::with_capacity(rows.len());
    let mut unpriced = 0usize;

    for ((provider, _id), mut row) in rows {
        // 行启用 = 默认绑定开着，**或**任一条别名开着（SPEC §2.1 派生语义，
        // 对齐 Agent2API 的 `rowEnabled = bindingsOf(m).some(b => b.enabled)`）。
        let default_enabled = !rules.is_disabled(&provider, &row.id);
        let alias_enabled = row.alias_entries.iter().any(|entry| entry.enabled);
        row.default_enabled = default_enabled;
        row.enabled = default_enabled || alias_enabled;
        if row.price.is_none() {
            unpriced += 1;
        }
        let group = groups
            .entry(provider.clone())
            .or_insert_with(|| ProviderGroup {
                id: provider.clone(),
                label: provider_label(&provider),
                total: 0,
                enabled: 0,
                account: account_providers.contains(&provider),
            });
        group.total += 1;
        if row.enabled {
            group.enabled += 1;
        }
        models.push(row);
    }

    // 按 provider 展示名、再按模型 id 排序，保证前端表格稳定。
    models.sort_by(|a, b| {
        a.provider_label
            .cmp(&b.provider_label)
            .then(a.provider.cmp(&b.provider))
            .then(a.id.cmp(&b.id))
    });

    ManageView {
        providers: groups.into_values().collect(),
        models,
        mappings: mapper.all_mappings(),
        unpriced,
    }
}

fn new_row(
    provider: &str,
    model: &str,
    metadata: &ModelMetadataRegistry,
    pricing: &PricingStore,
    mapper: &ModelMapper,
) -> ManageModel {
    let channel_owned_by = crate::model::metadata::owned_by_for_channel_type(provider);
    let (meta, cap_sources) = metadata.resolve_with_sources(model, channel_owned_by);
    let price = pricing.get_price(model);
    let model_ratio = pricing.get_ratios().model_ratio(model);
    let all_mappings = mapper.all_mappings();
    let disabled = mapper.disabled_aliases();
    let all_reasoning = mapper.all_reasoning();
    let alias_entries: Vec<AliasEntry> = all_mappings
        .iter()
        .filter(|(_, target)| *target == model)
        .map(|(alias, target)| AliasEntry {
            alias: alias.clone(),
            target: target.clone(),
            enabled: !disabled.contains(alias),
            reasoning_effort: all_reasoning.get(alias).cloned(),
        })
        .collect();
    let mut aliases: Vec<String> = alias_entries.iter().map(|e| e.alias.clone()).collect();
    aliases.sort_unstable();
    let mut alias_entries = alias_entries;
    alias_entries.sort_by(|a, b| a.alias.cmp(&b.alias));
    ManageModel {
        provider: provider.to_string(),
        provider_label: provider_label(provider),
        id: model.to_string(),
        enabled: true,
        default_enabled: true,
        sources: Vec::new(),
        owned_by: meta.owned_by,
        context_length: meta.context_length,
        capabilities: meta.capabilities,
        cap_sources,
        price,
        model_ratio,
        aliases,
        alias_entries,
    }
}

/// 视图序列化为管理面响应信封里的 `data`。
pub fn view_json(view: &ManageView) -> Value {
    json!({
        "providers": view.providers,
        "models": view.models,
        "mappings": view.mappings,
        "unpriced": view.unpriced,
    })
}

/// 为 Agent 自动挑选默认推理模型：启用 + 已定价优先，偏好非预览版，字典序稳定。
///
/// config.model 为空时的自动发现入口（`agent::llm::effective_model`）。
/// 全部启用模型都未定价时兜底取启用集（总比 ModelNotConfigured 强）。
pub fn pick_default_model(view: &ManageView) -> Option<String> {
    let is_preview = |id: &str| {
        let l = id.to_ascii_lowercase();
        ["preview", "beta", "alpha", "exp", "canary", "nightly", "rc"]
            .iter()
            .any(|k| l.contains(k))
    };
    let mut cands: Vec<&ManageModel> = view
        .models
        .iter()
        .filter(|m| m.enabled && m.price.is_some())
        .collect();
    if cands.is_empty() {
        cands = view.models.iter().filter(|m| m.enabled).collect();
    }
    cands.sort_by(|a, b| {
        is_preview(&a.id)
            .cmp(&is_preview(&b.id))
            .then_with(|| a.id.cmp(&b.id))
    });
    cands.first().map(|m| m.id.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store(tag: &str) -> Arc<FileStore> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "aigx-model-manage-{tag}-{}-{seq}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Arc::new(FileStore::new(dir))
    }

    #[test]
    fn rules_toggle_and_persist() {
        let store = temp_store("rules");
        let rules = ModelRules::new(store.clone());
        rules.load().unwrap();
        assert!(!rules.is_disabled("openai", "gpt-4o"));

        assert!(rules.set_enabled("openai", "gpt-4o", false).unwrap());
        assert!(rules.is_disabled("openai", "gpt-4o"));
        // 另一家不受影响
        assert!(!rules.is_disabled("anthropic", "gpt-4o"));

        // 重新加载
        let reloaded = ModelRules::new(store);
        reloaded.load().unwrap();
        assert!(reloaded.is_disabled("openai", "gpt-4o"));
        assert!(!reloaded.is_disabled("anthropic", "gpt-4o"));

        // 重复设置同值不算变化
        assert!(!reloaded.set_enabled("openai", "gpt-4o", false).unwrap());
    }

    #[test]
    fn fully_disabled_requires_all_providers() {
        let store = temp_store("fully");
        let rules = ModelRules::new(store);
        rules.load().unwrap();
        rules.set_enabled("openai", "gpt-4o", false).unwrap();
        assert!(!rules.fully_disabled(&["openai".into(), "openai_compatible".into()], "gpt-4o"));
        rules
            .set_enabled("openai_compatible", "gpt-4o", false)
            .unwrap();
        assert!(rules.fully_disabled(&["openai".into(), "openai_compatible".into()], "gpt-4o"));
        assert!(!rules.fully_disabled(&[], "gpt-4o"));
    }

    #[test]
    fn provider_label_falls_back_to_kind() {
        assert_eq!(provider_label("unknown-vendor"), "unknown-vendor");
        // registry 里有这一家的正式条目，用它自己的展示名（含「自填地址」后缀）
        assert_eq!(
            provider_label("openai_compatible"),
            "OpenAI 兼容（自填地址）"
        );
        // registry 里的 provider 用中文展示名
        assert!(!provider_label("deepseek").is_empty());
    }
}
