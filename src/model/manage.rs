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
//!
//! ## T5：A2A v3.5.0-beta.1 视图形状（`{models, mappings, providers, dangling, …}`）
//!
//! 移植自 `core/providers/catalog/view.rs::manage_view`。新增三块：
//! - `mappings[]`：**逐条**映射（`{alias, target, provider, enabled, reasoning,
//!   isDefault, dangling, carried}`），含关闭条目与未挂载条目；
//!   旧的 `mappings: {alias: target}` 平表保留（老前端 / 老调用点继续用）。
//! - `providers[].source / refreshedAt`：这一家的清单是「内置静态」还是
//!   「远程拉回」、什么时候拉到的（管理页两列徽章的数据源）。
//! - `dangling[]`：`provider` 指向的家**不在视图里**（无可用账号 / 渠道被删）
//!   的映射 —— 页面上没有那一行可以挂，但用户仍需要一个入口看见并删掉它们。
//!
//! 模型行另带 `capOverrides`（被用户覆盖的能力键）与 `capSources`
//! （逐键来源 `user` / `upstream` / `modelsdev`），见
//! `model::capability_overrides`。

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use crate::account_session::AccountSessionStore;
use crate::channel::{Channel, ChannelType};
use crate::model::capability_overrides;
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
    ///
    /// 大小写不敏感（与 A2A `RuleEntry::matches` 同口径）：下游请求
    /// `Deepseek-v4.1-Flash` 与模型清单里的 `deepseek-v4.1-flash` 视为同一模型。
    pub fn is_disabled(&self, provider: &str, model: &str) -> bool {
        self.disabled
            .read()
            .iter()
            .any(|e| e.provider.eq_ignore_ascii_case(provider) && e.id.eq_ignore_ascii_case(model))
    }

    /// 设置启停；返回是否有变化。
    ///
    /// 大小写不敏感：命中已有条目（任意大小写写法）即更新，不产生重复条目。
    pub fn set_enabled(&self, provider: &str, model: &str, enabled: bool) -> anyhow::Result<bool> {
        let entry = RuleEntry {
            provider: provider.to_string(),
            id: model.to_string(),
        };
        let changed = {
            let mut set = self.disabled.write();
            if enabled {
                // 大小写不敏感地移除所有匹配项，避免残留大小写变体
                let matched: Vec<RuleEntry> = set
                    .iter()
                    .filter(|e| {
                        e.provider.eq_ignore_ascii_case(&entry.provider)
                            && e.id.eq_ignore_ascii_case(&entry.id)
                    })
                    .cloned()
                    .collect();
                let n = matched.len();
                for e in matched {
                    set.remove(&e);
                }
                n > 0
            } else {
                // 大小写不敏感：已有匹配则不重复插入
                if set.iter().any(|e| {
                    e.provider.eq_ignore_ascii_case(&entry.provider)
                        && e.id.eq_ignore_ascii_case(&entry.id)
                }) {
                    false
                } else {
                    set.insert(entry);
                    true
                }
            }
        };
        if changed {
            self.persist()?;
        }
        Ok(changed)
    }

    /// 全部禁用项（管理面回读 / 调试用）。
    #[allow(dead_code)] // 预留/线表符号（见 main.rs 债务清单）
    pub fn all_disabled(&self) -> Vec<RuleEntry> {
        let mut list: Vec<RuleEntry> = self.disabled.read().iter().cloned().collect();
        list.sort();
        list
    }

    /// 模型在**所有**承载它的 provider 下都被关掉时返回 true。
    ///
    /// 数据面用它做入口拦截（不知道最终会走哪个 provider）。只要还有一家开着
    /// 就放行 —— 精确到渠道的过滤交给调度器，这里只挡「全关」的模型。
    #[allow(dead_code)] // 预留/未接线（见 main.rs 债务清单）
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
    /// 逐模型清单来源（对齐 A2A `catalog/view.rs` 的 per-model `source`）：
    /// - `manual`：管理员通过「自定义模型」手动登记；
    /// - `remote`：上游目录/发现（`discovered_models`、账号目录缓存）拉回；
    /// - `builtin`：渠道声明（`models`）或账号白名单等内置配置；
    /// - `modelsdev`：仅由 models.dev 元数据兜底（无任何承载方声明时）。
    ///
    /// 同一 `(provider, model)` 有多路来源时按 `manual > remote > builtin >
    /// modelsdev` 取最强的一档。
    #[serde(default)]
    pub source: String,
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
    /// 能力三态出口（T5，A2A `capability::effective` 形状）：六键齐全，
    /// 未声明为 null（「不知道」≠「不支持」）。带用户覆盖的生效值。
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub capability_flags: serde_json::Value,
    /// 工具调用模式（A2A beta.2 `catalog/view.rs` 的 `toolCallingMode`）。
    ///
    /// 与 `/v1/models` 条目的 `tool_calling_mode` 同源。管理页据此展示
    /// 模型的工具调用模式（如 `"emulated"` / `"disabled"` / `"native"`）。
    /// `None` = 未声明，不输出键。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calling_mode: Option<String>,
    /// 被用户覆盖的能力键（管理页「已改」标记；空数组 = 全部继承）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cap_overrides: Vec<String>,
    /// 可见性判据（对齐 Agent2API `providers/<kind>/models.rs` 的 `visibility` 键）。
    ///
    /// 取值域逐字同 A2A：`"hidden"` / `"shown"`；`None` = 没有判据（管理页归「未声明」）。
    /// AIGX 的判据来源是管理员的元信息覆盖（`model_meta:{model}` 的 `visibility`），
    /// 因为本仓还没接带可见性判据的 provider 目录；覆盖没设就是 `None`，
    /// 前端**不**把它当成「可见」来撒谎（与 A2A 「没依据就不藏」同一纪律）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visibility: Option<String>,
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
    /// provider 维度（None = 全局条目，对所有家生效）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
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
    /// 清单来源：`builtin`（内置静态 / 渠道声明）/ `remote`（上游目录拉回）
    /// / `none`（这家没有可用模型）。对齐 A2A `manage_view` 的 `source` 三态。
    #[serde(default)]
    pub source: String,
    /// 该家清单最近一次刷新时刻（unix 秒；0 = 从未拉过）。
    #[serde(default)]
    pub refreshed_at: i64,
}

/// 完整视图。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManageView {
    pub providers: Vec<ProviderGroup>,
    pub models: Vec<ManageModel>,
    /// 全局别名映射（alias → target；老前端 / 老调用点的扁平形态，只含全局条目）
    pub mappings: HashMap<String, String>,
    /// **逐条**映射（A2A `manage_view.mappings` 形状）：含 provider 维度、
    /// 关闭条目与未挂载（dangling）条目。新前端按这一份渲染 chip 与开关。
    pub mapping_entries: Vec<MappingView>,
    /// 未挂载的映射（`provider` 指向的家不在视图里）；前端「未挂载的映射」区块。
    pub dangling: Vec<MappingView>,
    /// 能力三态存储是否已初始化（`capability_overrides::install` 是否被调用）。
    ///
    /// **不静默降级**：未初始化时所有三态覆盖都不生效 —— 管理页必须把这件事
    /// 显示出来（否则运维改了能力位、页面按无覆盖渲染，误以为改成功了）。
    /// 写入路径（`POST /api/models/capabilities` 新形态）在未初始化时返回 500。
    pub capability_store_ready: bool,
    /// 未定价的模型数（顶部统计）
    pub unpriced: usize,
}

/// 一条映射的管理视图条目（A2A `catalog::manage_view` 的 `mappings[]` 一项）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MappingView {
    pub alias: String,
    pub target: String,
    /// provider 维度（None = 全局条目）
    #[serde(default)]
    pub provider: Option<String>,
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    /// 是否自映射（`alias == target` = 模型行的默认绑定，不可删除、只能关）
    pub is_default: bool,
    /// 是否悬空（挂不到任何模型行）
    pub dangling: bool,
    /// 目标模型是否仍被这家承载（false = 上游已下线 / 这家没这个模型）
    pub carried: bool,
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
                let (models, source): (Vec<String>, &str) = if session.models.is_empty() {
                    // 白名单为空：先回落该账号的目录缓存（SPEC §3.3，与
                    // collect_available_models **同口径**），仍空才回落渠道声明。
                    let cached = crate::account_session::catalog::cached_model_ids(
                        sessions,
                        &session.provider,
                        &session.id,
                    );
                    if cached.is_empty() {
                        (channel.effective_models().to_vec(), "builtin")
                    } else {
                        (cached, "remote")
                    }
                } else {
                    // 账号白名单 = 管理员声明
                    (session.models.clone(), "builtin")
                };
                for model in models {
                    if model.is_empty() {
                        continue;
                    }
                    let row = rows
                        .entry((provider.to_string(), model.clone()))
                        .or_insert_with(|| new_row(provider, &model, metadata, pricing, mapper));
                    merge_source(&mut row.source, source);
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

        if channel.channel_type == ChannelType::Plugin {
            // 插件渠道：provider 用 `plugin_provider`（不显示字面量 "plugin"）；
            // 模型优先渠道声明，留空回落插件目录（与 `collect_available_models`
            // 同源，见 `model_catalog::plugin_channel_source`）。
            let (plugin_provider, models, _) =
                crate::plugin_host::model_catalog::plugin_channel_source(channel);
            let provider = if plugin_provider.is_empty() {
                channel.channel_type.as_str()
            } else {
                plugin_provider.as_str()
            };
            // 渠道声明 = builtin；回落到插件目录 = remote。
            let source = if channel.effective_models().is_empty() {
                "remote"
            } else {
                "builtin"
            };
            for model in models {
                if model.is_empty() {
                    continue;
                }
                let row = rows
                    .entry((provider.to_string(), model.clone()))
                    .or_insert_with(|| new_row(provider, &model, metadata, pricing, mapper));
                merge_source(&mut row.source, source);
                if !row.sources.iter().any(|s| s.channel_id == channel.id) {
                    row.sources.push(ModelSource {
                        kind: "channel".to_string(),
                        channel_id: channel.id.clone(),
                        name: channel.name.clone(),
                        account_id: None,
                    });
                }
            }
            continue;
        }
        let provider = channel.channel_type.as_str();
        for model in channel.effective_models() {
            if model.is_empty() {
                continue;
            }
            // 声明清单 = builtin；仅经上游发现（discovered）拉回 = remote。
            let declared = channel.models.iter().any(|m| m.eq_ignore_ascii_case(model));
            let source = if declared { "builtin" } else { "remote" };
            let row = rows
                .entry((provider.to_string(), model.clone()))
                .or_insert_with(|| new_row(provider, model, metadata, pricing, mapper));
            merge_source(&mut row.source, source);
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
        let row = rows
            .entry((entry.provider.clone(), entry.id.clone()))
            .or_insert_with(|| new_row(&entry.provider, &entry.id, metadata, pricing, mapper));
        merge_source(&mut row.source, "manual");
    }

    // 启停状态 + 分组统计
    let mut groups: BTreeMap<String, ProviderGroup> = BTreeMap::new();
    let mut models: Vec<ManageModel> = Vec::with_capacity(rows.len());
    let mut unpriced = 0usize;
    // 各家清单的来源 / 刷新时刻（A2A `manage_view.providers[].source/refreshedAt`）。
    //
    // 口径（只用公开 API：`catalog::cached_model_ids` + `Channel.updated_at`，
    // 不碰 account-pool 的私有缓存结构）：
    // - 账号白名单非空 → `builtin`（管理员声明）；
    // - 账号白名单为空但目录缓存有内容 → `remote`（上游拉回），时刻取渠道的
    //   `updated_at`（渠道维度最近一次落盘，含目录写回）；
    // - 其余 → `builtin`，时刻 0（从未成功拉过）。
    let mut provider_meta: HashMap<String, (String, i64)> = HashMap::new();
    for channel in channels.iter().filter(|c| c.is_enabled()) {
        if is_account_channel(channel) {
            for session in sessions
                .all()
                .iter()
                .filter(|s| s.channel_id == channel.id && s.status == "enabled")
            {
                let provider = if session.provider.is_empty() {
                    crate::account_session::registry::DEFAULT_PROVIDER_KIND.to_string()
                } else {
                    session.provider.clone()
                };
                let (source, refreshed_at) = if !session.models.is_empty() {
                    ("builtin".to_string(), 0)
                } else if !crate::account_session::catalog::cached_model_ids(
                    sessions,
                    &session.provider,
                    &session.id,
                )
                .is_empty()
                {
                    ("remote".to_string(), channel.updated_at)
                } else {
                    ("builtin".to_string(), 0)
                };
                provider_meta
                    .entry(provider)
                    .and_modify(|slot| {
                        // 远程来源优先（有真实刷新时刻的一家代表这一家）
                        if source == "remote" && slot.0 != "remote" {
                            *slot = (source.clone(), refreshed_at);
                        } else if refreshed_at > slot.1 {
                            slot.1 = refreshed_at;
                        }
                    })
                    .or_insert((source, refreshed_at));
            }
        } else if channel.channel_type == ChannelType::Plugin {
            // 插件渠道：source/时刻用插件目录的 meta（与模型行走同一份口径；
            // 渠道声明非空 = builtin，空 + 目录命中 = remote + 目录刷新时刻）。
            let (plugin_provider, _, refreshed_at) =
                crate::plugin_host::model_catalog::plugin_channel_source(channel);
            let provider = if plugin_provider.is_empty() {
                channel.channel_type.as_str().to_string()
            } else {
                plugin_provider
            };
            let (source, at) = if !channel.effective_models().is_empty() {
                ("builtin".to_string(), 0)
            } else if refreshed_at > 0 {
                ("remote".to_string(), refreshed_at)
            } else {
                ("builtin".to_string(), 0)
            };
            provider_meta
                .entry(provider)
                .and_modify(|slot| {
                    if source == "remote" && slot.0 != "remote" {
                        *slot = (source.clone(), at);
                    } else if at > slot.1 {
                        slot.1 = at;
                    }
                })
                .or_insert((source, at));
        } else {
            provider_meta
                .entry(channel.channel_type.as_str().to_string())
                .or_insert((
                    if channel.discovered_models.is_empty() {
                        "builtin".to_string()
                    } else {
                        "remote".to_string()
                    },
                    channel.updated_at,
                ));
        }
    }

    for ((provider, _id), mut row) in rows {
        // 兜底：任何承载方都没给出处时，仅能归因到 models.dev 元数据。
        if row.source.is_empty() {
            row.source = "modelsdev".to_string();
        }
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
                source: "none".to_string(),
                refreshed_at: 0,
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

    // provider 分组补 source / refreshedAt（A2A 两列徽章的数据源）
    for group in groups.values_mut() {
        if let Some((source, refreshed_at)) = provider_meta.get(&group.id) {
            group.source = source.clone();
            group.refreshed_at = *refreshed_at;
        }
    }

    // ── mappings[] / dangling[]（T5：A2A `manage_view` 形状）──────────────
    let (mapping_entries, dangling) = compose_mapping_views(&models, &mapper.all_mapping_entries());

    ManageView {
        providers: groups.into_values().collect(),
        models,
        mappings: mapper.all_mappings(),
        mapping_entries,
        dangling,
        capability_store_ready: capability_overrides::current().is_some(),
        unpriced,
    }
}

/// 由模型行 + 映射条目合成 `mappings[]` / `dangling[]`（纯函数，可单测）。
///
/// 规则（与 A2A `manage_view` 同构）：
/// 1. **每条模型行先放一条默认绑定**（`id → id`）：真实条目存在就用它的
///    reasoning / provider，开关 = `model_rules` 的 default_enabled AND 条目
///    enabled；条目不存在（关闭的模型行 / 内建模型行）就合成一条虚拟
///    默认绑定（`provider` 填行自己的家）。
/// 2. 其余条目挂到 `target` 命中的行上（provider 维度的条目只挂自己家；
///    全局条目挂任何承载 target 的行）。同一行上一个 alias 只渲染一次。
/// 3. 剩下没挂上的条目：**全局条目 target 不在任何行** = dangling；
///    **provider 条目那一家不在视图里**（无可用账号 / 渠道被删）= dangling。
///    都进 `dangling[]`（同时也进全量 `mappings[]` —— 前端两处都拿得到）。
fn compose_mapping_views(
    models: &[ManageModel],
    entries: &[crate::model::Mapping],
) -> (Vec<MappingView>, Vec<MappingView>) {
    let mut attached: HashSet<usize> = HashSet::new();
    let mut mapping_views: Vec<MappingView> = Vec::new();
    for row in models {
        let default = entries.iter().enumerate().find(|(_, m)| {
            m.alias == row.id
                && m.target == row.id
                && match &m.provider {
                    None => true,
                    Some(owner) => owner.eq_ignore_ascii_case(&row.provider),
                }
        });
        match default {
            Some((index, mapping)) => {
                attached.insert(index);
                mapping_views.push(MappingView {
                    alias: mapping.alias.clone(),
                    target: mapping.target.clone(),
                    provider: mapping.provider.clone(),
                    enabled: row.default_enabled && mapping.enabled,
                    reasoning: mapping.reasoning.clone(),
                    is_default: true,
                    dangling: false,
                    carried: true,
                });
            }
            None => {
                // 关闭的模型行也有一条「虚拟默认绑定」：开关状态来自 model_rules
                mapping_views.push(MappingView {
                    alias: row.id.clone(),
                    target: row.id.clone(),
                    provider: Some(row.provider.clone()),
                    enabled: row.default_enabled,
                    reasoning: None,
                    is_default: true,
                    dangling: false,
                    carried: true,
                });
            }
        }
        for (index, mapping) in entries.iter().enumerate() {
            if attached.contains(&index) {
                continue;
            }
            if !mapping.target.eq_ignore_ascii_case(&row.id) {
                continue;
            }
            if let Some(owner) = &mapping.provider {
                if !owner.eq_ignore_ascii_case(&row.provider) {
                    continue;
                }
            }
            attached.insert(index);
            if mapping.alias == row.id {
                continue; // 自映射已经渲染为默认绑定
            }
            mapping_views.push(MappingView {
                alias: mapping.alias.clone(),
                target: mapping.target.clone(),
                provider: mapping.provider.clone(),
                enabled: mapping.enabled,
                reasoning: mapping.reasoning.clone(),
                is_default: false,
                dangling: false,
                carried: true,
            });
        }
    }
    // 未挂到任何行的条目 → dangling（页面「未挂载的映射」区块的入口）
    let managed_providers: HashSet<String> = models
        .iter()
        .map(|row| row.provider.to_lowercase())
        .collect();
    for (index, mapping) in entries.iter().enumerate() {
        if attached.contains(&index) {
            continue;
        }
        let (dangling, carried) = match &mapping.provider {
            Some(owner) => {
                let carried = models.iter().any(|row| {
                    row.provider.eq_ignore_ascii_case(owner) && row.id == mapping.target
                });
                // 家不在视图里 → 没有行可挂（悬空）；家还在但 target 已下线 → 也悬空
                (
                    !managed_providers.contains(&owner.to_lowercase()) || !carried,
                    carried,
                )
            }
            // 全局条目：target 不在任何模型行里 = 悬空
            None => {
                let carried = models.iter().any(|row| row.id == mapping.target);
                (!carried, carried)
            }
        };
        mapping_views.push(MappingView {
            alias: mapping.alias.clone(),
            target: mapping.target.clone(),
            provider: mapping.provider.clone(),
            enabled: mapping.enabled,
            reasoning: mapping.reasoning.clone(),
            is_default: mapping.alias == mapping.target,
            dangling,
            carried,
        });
    }
    let dangling: Vec<MappingView> = mapping_views
        .iter()
        .filter(|v| v.dangling)
        .cloned()
        .collect();
    mapping_views.sort_by(|a, b| {
        a.is_default
            .cmp(&b.is_default)
            .then_with(|| a.alias.cmp(&b.alias))
            .then_with(|| a.provider.cmp(&b.provider))
    });
    (mapping_views, dangling)
}

/// 清单来源强弱（同一模型多路来源时取最强）：`manual > remote > builtin > modelsdev`。
fn source_rank(source: &str) -> u8 {
    match source {
        "manual" => 4,
        "remote" => 3,
        "builtin" => 2,
        "modelsdev" => 1,
        _ => 0,
    }
}

/// 合并一路来源到行上：只升级不降级（空/未知值不覆盖已定来源）。
fn merge_source(slot: &mut String, candidate: &str) {
    if source_rank(candidate) > source_rank(slot) {
        *slot = candidate.to_string();
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
    let entries = mapper.all_mapping_entries();
    // 别名明细：只收**挂到这一家**的条目（provider 维度条目只出现在自己家
    // 的行上；全局条目在所有承载 target 的行上都出现 —— 与 A2A `aliases_of`
    // 同口径）。刻意不过滤 disabled：管理页的 chip 要能看见并再打开。
    let alias_entries: Vec<AliasEntry> = entries
        .iter()
        .filter(|mapping| mapping.target.eq_ignore_ascii_case(model))
        .filter(|mapping| {
            mapping
                .provider
                .as_deref()
                .is_none_or(|owner| owner.eq_ignore_ascii_case(provider))
        })
        .map(|mapping| AliasEntry {
            alias: mapping.alias.clone(),
            target: mapping.target.clone(),
            provider: mapping.provider.clone(),
            enabled: mapping.enabled,
            reasoning_effort: mapping.reasoning.clone(),
        })
        .collect();
    let mut aliases: Vec<String> = alias_entries.iter().map(|e| e.alias.clone()).collect();
    aliases.sort_unstable();
    let mut alias_entries = alias_entries;
    alias_entries.sort_by(|a, b| a.alias.cmp(&b.alias));

    // ── 能力三态（T5）：用户覆盖 > 上游声明 > models.dev ───────────────
    //
    // 上游那一档是 `metadata` 合并后的标签集（内置推断 + 渠道类型 +
    // models.dev 只填空位的产物）；三态覆盖层再压在上面，且**出口标签集
    // 同步重算**（`apply_to_capabilities`）—— 于是「管理页看到的」与
    // 「/v1/models 声明的」是同一份值。
    let overrides = capability_overrides::lookup(provider, model);
    let mut capabilities = meta.capabilities.clone();
    if let Some(patch) = &overrides {
        capability_overrides::apply_to_capabilities(&mut capabilities, patch);
    }
    let capability_flags =
        capability_overrides::effective_object(&meta.capabilities, overrides.as_ref());
    let cap_overrides = override_keys(&overrides);

    ManageModel {
        provider: provider.to_string(),
        provider_label: provider_label(provider),
        id: model.to_string(),
        source: String::new(),
        enabled: true,
        default_enabled: true,
        sources: Vec::new(),
        owned_by: meta.owned_by,
        context_length: meta.context_length,
        capabilities,
        cap_sources,
        capability_flags,
        cap_overrides,
        tool_calling_mode: meta.tool_calling_mode,
        visibility: meta.visibility,
        price,
        model_ratio,
        aliases,
        alias_entries,
    }
}

/// 覆盖表 → 被改的键清单（按固定顺序，管理页「已改」标记用）。
fn override_keys(overrides: &Option<serde_json::Map<String, Value>>) -> Vec<String> {
    let Some(patch) = overrides else {
        return Vec::new();
    };
    capability_overrides::KEYS
        .iter()
        .copied()
        .chain([capability_overrides::AUDIO_KEY])
        .filter(|key| patch.contains_key(*key))
        .map(str::to_string)
        .collect()
}

/// 视图序列化为管理面响应信封里的 `data`。
///
/// 形状按 **Agent2API `manage_view`**（T5 任务要求 «增加 `mappings[]`、
/// `providers[].source/refreshedAt`、`dangling[]`»）：
/// - `mappings` = **逐条数组**（含 provider / reasoning / enabled / isDefault /
///   dangling / carried）；
/// - `dangling` = 未挂载条目数组；
/// - `providers[]` 自带 `source` / `refreshedAt`（结构体字段，自动透出）。
///
/// 旧扁平表 `{alias: target}` 改名 `mappingsFlat` 保留：老前端（ModelManage /
/// RatioPanel）此前读 `mappings` 的对象形态，改成数组后它们的
/// `Object.entries/keys` 会拿到下标串 —— 前端对齐（T8）把读取点切换到
/// `mappings`（数组）或 `mappingsFlat`（对象）后即可删除后者。
/// `dangling_mappings` 同样保留旧名，供旧前端直接走后端事实分支。
pub fn view_json(view: &ManageView) -> Value {
    let legacy_dangling: Vec<Value> = view
        .dangling
        .iter()
        .map(|m| json!({ "alias": m.alias, "target": m.target, "provider": m.provider }))
        .collect();
    json!({
        "providers": view.providers,
        "models": view.models,
        "mappings": view.mapping_entries,
        "mappingsFlat": view.mappings,
        "dangling": view.dangling,
        "dangling_mappings": legacy_dangling,
        "capabilityStoreReady": view.capability_store_ready,
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

    /// 造一行最小可用的模型（`compose_mapping_views` 只读 provider / id /
    /// default_enabled，其余字段给空值）。
    fn row(provider: &str, id: &str, default_enabled: bool) -> ManageModel {
        ManageModel {
            provider: provider.to_string(),
            provider_label: provider.to_string(),
            id: id.to_string(),
            source: String::new(),
            enabled: default_enabled,
            default_enabled,
            sources: Vec::new(),
            owned_by: "test".to_string(),
            context_length: None,
            capabilities: Vec::new(),
            cap_sources: Value::Null,
            capability_flags: Value::Null,
            cap_overrides: Vec::new(),
            tool_calling_mode: None,
            visibility: None,
            price: None,
            model_ratio: 1.0,
            aliases: Vec::new(),
            alias_entries: Vec::new(),
        }
    }

    fn mapping(
        alias: &str,
        target: &str,
        provider: Option<&str>,
        enabled: bool,
    ) -> crate::model::Mapping {
        crate::model::Mapping {
            alias: alias.to_string(),
            target: target.to_string(),
            provider: provider.map(str::to_string),
            reasoning: None,
            enabled,
        }
    }

    /// 正常映射挂到行上，不进 dangling。
    #[test]
    fn compose_attaches_alias_to_row() {
        let models = vec![row("workbuddy", "deepseek-v4-pro", true)];
        let entries = vec![mapping(
            "ds-pro",
            "deepseek-v4-pro",
            Some("workbuddy"),
            true,
        )];
        let (all, dangling) = compose_mapping_views(&models, &entries);
        assert!(dangling.is_empty(), "正常映射不该悬空: {dangling:?}");
        assert_eq!(all.len(), 2, "默认绑定 + 别名");
        assert!(all
            .iter()
            .any(|m| m.alias == "ds-pro" && !m.dangling && m.carried));
    }

    /// target 已下线的全局映射 → dangling（页面「未挂载的映射」入口）。
    #[test]
    fn compose_marks_dangling_when_target_missing() {
        let models = vec![row("workbuddy", "deepseek-v4-pro", true)];
        let entries = vec![
            mapping("ds-pro", "deepseek-v4-pro", Some("workbuddy"), true),
            mapping("ghost", "retired-model", None, true),
        ];
        let (_, dangling) = compose_mapping_views(&models, &entries);
        assert_eq!(dangling.len(), 1);
        assert_eq!(dangling[0].alias, "ghost");
        assert!(!dangling[0].carried, "目标不在清单 = carried=false");
    }

    /// provider 指向的家不在视图（无可用账号 / 渠道被删）→ dangling。
    #[test]
    fn compose_marks_dangling_when_provider_absent() {
        let models = vec![row("workbuddy", "deepseek-v4-pro", true)];
        // catpaw 这家不在视图里
        let entries = vec![mapping("x", "kimi-k3", Some("catpaw"), true)];
        let (_, dangling) = compose_mapping_views(&models, &entries);
        assert_eq!(dangling.len(), 1);
        assert_eq!(dangling[0].provider.as_deref(), Some("catpaw"));
        assert!(!dangling[0].carried);
    }

    /// 关闭的模型行仍有一条虚拟默认绑定（开关关着，但页面看得见）。
    #[test]
    fn compose_synthesizes_default_binding_for_disabled_row() {
        let models = vec![row("workbuddy", "deepseek-v4-pro", false)];
        let (all, dangling) = compose_mapping_views(&models, &[]);
        assert!(dangling.is_empty());
        assert_eq!(all.len(), 1);
        assert!(all[0].is_default);
        assert!(!all[0].enabled, "默认绑定跟随 model_rules 的关停状态");
        assert_eq!(all[0].provider.as_deref(), Some("workbuddy"));
    }

    /// provider 维度条目只挂自己家；别家行上看不到它。
    #[test]
    fn compose_provider_entry_attaches_only_to_own_row() {
        let models = vec![
            row("workbuddy", "kimi-k3", true),
            row("catpaw", "kimi-k3", true),
        ];
        let entries = vec![mapping("k3", "kimi-k3", Some("catpaw"), true)];
        let (all, dangling) = compose_mapping_views(&models, &entries);
        assert!(dangling.is_empty());
        let alias_views: Vec<&MappingView> = all.iter().filter(|m| m.alias == "k3").collect();
        assert_eq!(alias_views.len(), 1, "同一条映射不能在两行上各渲染一次");
        assert_eq!(alias_views[0].provider.as_deref(), Some("catpaw"));
    }

    /// 三态覆盖的出口：用户覆盖压过上游标签集（user > upstream），
    /// 未覆盖的键沿用上游 / models.dev 的合并结果。
    #[test]
    fn capability_override_priority_is_user_then_upstream() {
        // 上游声明（model）→ 模拟 `meta.capabilities`
        let upstream_tags = vec!["chat".to_string(), "tools".to_string()];
        let user_patch = serde_json::json!({
            "supportsToolCall": false,
            "supportsImages": true,
        });
        let patch = user_patch.as_object().unwrap().clone();
        let mut effective_tags = upstream_tags.clone();
        capability_overrides::apply_to_capabilities(&mut effective_tags, &patch);
        let flags = capability_overrides::effective_flags(&upstream_tags, Some(&patch));
        // 用户关掉工具调用：即使上游说支持，出口也是 false
        assert_eq!(flags["supportsToolCall"], serde_json::json!(false));
        // 用户打开图片：上游没说，出口 true
        assert_eq!(flags["supportsImages"], serde_json::json!(true));
        // 未覆盖的键沿用上游
        assert_eq!(flags["supportsChat"], serde_json::json!(true));
        // 标签集同源：tools 被移除、vision 被加上
        assert!(!effective_tags.iter().any(|t| t == "tools"));
        assert!(effective_tags.iter().any(|t| t == "vision"));
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

    /// 插件渠道的 `build_view`：provider 用 `plugin_provider` 而非字面量
    /// "plugin"；模型回落目录；provider 分组的 source/refreshedAt 取目录 meta。
    #[test]
    fn plugin_channel_rows_use_plugin_provider_and_catalog() {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let provider = format!("unit-manage-plugin-{}", SEQ.fetch_add(1, Ordering::Relaxed));
        crate::plugin_host::model_catalog::set_remote(
            &provider,
            vec![serde_json::json!({ "id": "cat-a" })],
            123,
        );
        let channel: Channel = serde_json::from_value(serde_json::json!({
            "id": "ch-plugin-view",
            "name": "plugin-view",
            "channel_type": "plugin",
            "plugin_provider": provider,
            "status": "enabled",
            "models": [],
        }))
        .unwrap();
        let store = temp_store("plugin-view");
        let sessions = AccountSessionStore::new(store.clone());
        let rules = ModelRules::new(store.clone());
        rules.load().unwrap();
        let metadata = ModelMetadataRegistry::new(store.clone());
        let pricing = PricingStore::new(store.clone());
        let mapper = ModelMapper::new(store);
        let view = build_view(
            std::slice::from_ref(&channel),
            &sessions,
            &rules,
            &metadata,
            &pricing,
            &mapper,
        );
        // 行：provider = plugin_provider（不出现 "plugin" 字面量）
        assert_eq!(view.models.len(), 1);
        assert_eq!(view.models[0].provider, provider);
        assert_eq!(view.models[0].id, "cat-a");
        // 分组：id/label 同样用 plugin_provider，source = remote + 目录时刻
        assert_eq!(view.providers.len(), 1);
        assert_eq!(view.providers[0].id, provider);
        assert_eq!(view.providers[0].source, "remote");
        assert_eq!(view.providers[0].refreshed_at, 123);
        crate::plugin_host::model_catalog::remove(&provider);

        // 无目录、无声明 → 不出现任何行（不编造）
        let empty: Channel = serde_json::from_value(serde_json::json!({
            "id": "ch-plugin-empty",
            "name": "plugin-empty",
            "channel_type": "plugin",
            "plugin_provider": "unit-manage-absent",
            "status": "enabled",
            "models": [],
        }))
        .unwrap();
        let view = build_view(
            std::slice::from_ref(&empty),
            &sessions,
            &rules,
            &metadata,
            &pricing,
            &mapper,
        );
        assert!(view.models.is_empty());
        assert!(view.providers.is_empty());
    }

    /// 逐模型清单来源 `source` 推导：声明=builtin、发现=remote、自定义登记=manual。
    ///
    /// 同一 `(provider, model)` 多路来源时按 `manual > remote > builtin` 取最强。
    #[test]
    fn per_model_source_derives_builtin_remote_manual() {
        let declared: Channel = serde_json::from_value(serde_json::json!({
            "id": "ch-src-declared",
            "name": "src-declared",
            "channel_type": "openai_compatible",
            "base_url": "https://example.com/v1",
            "status": "enabled",
            "models": ["declared-model"],
        }))
        .unwrap();
        // 声明清单为空时 `effective_models()` 回落到 discovered_models（remote）。
        let discovered: Channel = serde_json::from_value(serde_json::json!({
            "id": "ch-src-discovered",
            "name": "src-discovered",
            "channel_type": "openai_compatible",
            "base_url": "https://example.com/v1",
            "status": "enabled",
            "models": [],
            "discovered_models": ["found-model"],
        }))
        .unwrap();
        let store = temp_store("source-view");
        let sessions = AccountSessionStore::new(store.clone());
        let rules = ModelRules::new(store.clone());
        rules.load().unwrap();
        let metadata = ModelMetadataRegistry::new(store.clone());
        let pricing = PricingStore::new(store.clone());
        let mapper = ModelMapper::new(store);
        // 管理员自定义登记一个上游未发现的模型（manual 档）。
        mapper
            .add_custom_model("openai_compatible", "manual-model")
            .unwrap();

        let view = build_view(
            &[declared, discovered],
            &sessions,
            &rules,
            &metadata,
            &pricing,
            &mapper,
        );
        let by_id = |id: &str| {
            view.models
                .iter()
                .find(|m| m.id == id)
                .unwrap_or_else(|| panic!("缺少模型 {id}"))
        };
        assert_eq!(
            by_id("declared-model").source,
            "builtin",
            "渠道声明 = builtin"
        );
        assert_eq!(by_id("found-model").source, "remote", "上游发现 = remote");
        assert_eq!(
            by_id("manual-model").source,
            "manual",
            "自定义登记 = manual"
        );
    }

    /// 启停规则大小写不敏感（与 A2A `RuleEntry::matches` 同口径）：
    /// 任意大小写写法视为同一 `(provider, id)`，不产生重复条目。
    #[test]
    fn disabled_rule_match_is_case_insensitive() {
        let store = temp_store("case");
        let rules = ModelRules::new(store);
        rules.load().unwrap();
        assert!(rules
            .set_enabled("openai_compatible", "deepseek-v4.1-flash", false)
            .unwrap());
        assert!(rules.is_disabled("openai_compatible", "deepseek-v4.1-flash"));
        assert!(rules.is_disabled("OpenAI_Compatible", "Deepseek-v4.1-Flash"));
        // 大小写不同写法再次关闭：无变化，不产生第二条记录
        assert!(!rules
            .set_enabled("openai_compatible", "Deepseek-v4.1-Flash", false)
            .unwrap());
        assert_eq!(rules.all_disabled().len(), 1);
        // 任意大小写写法启用一次即清掉
        assert!(rules
            .set_enabled("openai_compatible", "DEEPSEEK-V4.1-FLASH", true)
            .unwrap());
        assert!(!rules.is_disabled("openai_compatible", "deepseek-v4.1-flash"));
    }
}
