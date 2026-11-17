//! 能力位覆盖（三态）：`(provider, model_id)` × 六键 × {inherit / on / off}。
//!
//! Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//! 对应上游 `core/capability.rs`（六键与归一）+ `core/model_rules::CapabilityOverride`
//! （按 `(provider, id)` 存储 + 稀疏覆盖表）。
//!
//! ── 与 AIGX 既有 `ModelMetadata.capabilities: Vec<String>` 的关系 ──────
//! 既有覆盖是**全局按模型名**的一整份替换（`model_meta:{name}`）：它回答
//! 「这个模型有哪些能力标签」，但表达不了三件 A2A 那边必须能表达的事：
//!   1. **按提供商**：同一个 `kimi-k3` 在 A 家支持工具调用、在 B 家不支持；
//!   2. **逐键三态**：「明确不支持」（off）与「没声明 / 继承上游」（inherit）
//!      是两件事 —— `Vec<String>` 里缺某个标签可能是「不支持」也可能是
//!      「不知道」，下游据此构造请求的动作完全不同（发不发图片 / 按多大窗口
//!      堆历史）；
//!   3. **撤销单个键的覆盖**：整份替换下想「只恢复上下文窗口这一项」只能
//!      全量重填。
//!
//! 因此本模块是**新增的一层**，不替换既有 `Vec<String>`：既有标签集继续服务
//! 路由能力过滤（`CapabilityMask`）与旧前端；本层在三态可表达的出口
//! （`/v1/models` 的布尔能力键、Anthropic 列表、管理面 `capOverrides`）
//! 统一应用，优先级 **user（本层） > upstream（目录/内置推断） > models.dev**。
//!
//! ── 三态口径 ──────────────────────────────────────────────────────
//! | 存储值 | 含义 | 出口表现 |
//! |---|---|---|
//! | 键不存在 | `inherit`（继承：上游声明 > models.dev > 未声明） | 各出口原值 |
//! | `Bool(true)` / `Bool(false)` | 显式 on / off | 覆盖原值 |
//! | `Number` | token 数值（`max_input_tokens` / `max_output_tokens`） | 覆盖原值 |
//!
//! 存储是**稀疏表**：只存被覆盖的键；空表不存在（清空即删条目）。
//!
//! ── 键名 ─────────────────────────────────────────────────────────
//! 采用 A2A `capability::KEYS` 的出口键名（驼峰），并把 AIGX 既有的六个标签
//! 归一过来：`tool` / `tools` → `supportsToolCall`，`vision` → `supportsImages`，
//! `video` → `supportsVideo`，`reasoning` → `supportsReasoning`，
//! `embedding` / `rerank` / `audio` 是 AIGX 独有（A2A 的六键里没有），
//! 保留为 `supportsEmbedding` / `supportsRerank` / `supportsAudio`。
//! 出口时两套键都给（`capabilities: Vec<String>` 与布尔键并存，见
//! `apply_to_capabilities`）。

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::sync::{Arc, OnceLock};

use crate::storage::FileStore;

/// 覆盖条目在存储里的键。
const CAPABILITIES_KEY: &str = "model_capability_overrides";

/// 六键（A2A `capability::KEYS` 的子集 + AIGX 独有的三项）。
///
/// 顺序即管理页弹窗字段与能力徽章的展示顺序（前端按同一顺序渲染）。
pub const KEYS: [&str; 6] = [
    "supportsToolCall",
    "supportsImages",
    "supportsVideo",
    "supportsReasoning",
    "supportsEmbedding",
    "supportsRerank",
];

/// AIGX 既有 `Vec<String>` 标签 → 出口布尔键（唯一归一表，别处不得再写）。
///
/// `tool` 与 `tools` 是同一个能力的两种拼法（目录词表 vs models.dev 词表，
/// 见 `model::capability` 模块头）；`image`（文生图）**不**映射到
/// `supportsImages`（那是图片**输入**，与文生图是两回事）。
const TAG_TO_KEY: &[(&str, &str)] = &[
    ("tool", "supportsToolCall"),
    ("tools", "supportsToolCall"),
    ("vision", "supportsImages"),
    ("video", "supportsVideo"),
    ("reasoning", "supportsReasoning"),
    ("embedding", "supportsEmbedding"),
    ("rerank", "supportsRerank"),
    ("audio", "supportsAudio"),
    ("chat", "supportsChat"),
];

/// 出口布尔键 → AIGX 既有 `Vec<String>` 标签（`TAG_TO_KEY` 的反向首选）。
const KEY_TO_TAG: &[(&str, &str)] = &[
    ("supportsToolCall", "tools"),
    ("supportsImages", "vision"),
    ("supportsVideo", "video"),
    ("supportsReasoning", "reasoning"),
    ("supportsEmbedding", "embedding"),
    ("supportsRerank", "rerank"),
    ("supportsAudio", "audio"),
];

/// `audio` 是布尔键但不在 A2A 六键里（AIGX 既有标签集有它），单列一个常量。
pub const AUDIO_KEY: &str = "supportsAudio";
pub const CHAT_KEY: &str = "supportsChat";

/// token 数值键的上限（1 亿）：再大就是手滑多敲了几个零，而虚高的窗口会让
/// 下游按错误量级堆历史 —— 那正是这层覆盖要修的毛病。下限 1。
pub const MAX_TOKEN_VALUE: f64 = 100_000_000.0;

/// 合法键名（六键 + audio / chat；A2A 的 token 键在这里不单列，
/// AIGX 的上下文长度走既有 `ModelMetadata.context_length`）。
pub fn is_known_key(key: &str) -> bool {
    KEYS.contains(&key) || key == AUDIO_KEY || key == CHAT_KEY
}

/// 该键是不是布尔能力键。
pub fn is_bool_key(key: &str) -> bool {
    is_known_key(key)
}

/// 单值归一：布尔键收 bool；其余键 / 非法值 `None`（调用方按「丢弃」处理）。
///
/// `null` **不是**合法值 —— 存储层不存在「显式 null」这一档，清除覆盖靠删键
/// 表达（与 A2A `normalize_object` 同口径）。
pub fn normalize_value(key: &str, value: &Value) -> Option<Value> {
    if is_bool_key(key) {
        return value.as_bool().map(Value::Bool);
    }
    None
}

/// 从任意 JSON 对象里挑出合法覆盖键，返回**稀疏表**。
///
/// 非法键名 / 非法值 / null 一律丢弃；空对象 = 没有覆盖。
pub fn normalize_object(value: Option<&Value>) -> Map<String, Value> {
    let mut result = Map::new();
    let Some(object) = value.and_then(Value::as_object) else {
        return result;
    };
    for (key, item) in object {
        if let Some(value) = normalize_value(key, item) {
            result.insert(key.clone(), value);
        }
    }
    result
}

/// 把 AIGX 既有 `Vec<String>` 标签集归一成出口布尔键的**稀疏表**。
///
/// 仅用于「上游声明」这一档（项目里既有标签集就是上游/内置推断的合并结果）；
/// 未知标签（`image` 文生图等）不进表 —— 它们没有对应布尔键。
pub fn flags_from_tags(tags: &[String]) -> Map<String, Value> {
    let mut flags = Map::new();
    for tag in tags {
        if let Some((_, key)) = TAG_TO_KEY
            .iter()
            .find(|(known, _)| tag.eq_ignore_ascii_case(known))
        {
            flags.insert((*key).to_string(), Value::Bool(true));
        }
    }
    flags
}

/// 出口布尔键的生效值：`user` 覆盖 > 上游声明 > `false`（未声明 = 不支持，
/// 与 A2A `list_item` 的 `js_truthy` 缺省同口径）。
pub fn effective_flags(
    tags: &[String],
    overrides: Option<&Map<String, Value>>,
) -> Map<String, Value> {
    let mut flags = flags_from_tags(tags);
    if let Some(overrides) = overrides {
        for (key, value) in overrides {
            if is_bool_key(key) {
                flags.insert(key.clone(), value.clone());
            }
        }
    }
    flags
}

/// 把生效覆盖反写进 AIGX 既有标签集（出口 `capabilities: Vec<String>` 的
/// **唯一**应用点）。
///
/// 语义：
/// - `true` 且标签里没有 → 追加（标签名取得 `KEY_TO_TAG` 的首选）；
/// - `false` 且标签里有 → 移除；
/// - `false` 且标签里没有 → 补一个**显式否定标记** `!key`（`Vec<String>` 没有
///   「明确不支持」的表示位，用 `!` 前缀表达；下游按标签集判定的地方只认
///   正向标签，`!key` 只是一个可展示的事实，不参与选路 —— 这与 `CapabilityMask`
///   的「只认三位正向标签」完全兼容）。
///
/// 幂等：每次从**上游标签集**重算，绝不在上一次的结果上叠加（否则撤销覆盖
/// 撤不掉）。
pub fn apply_to_capabilities(tags: &mut Vec<String>, overrides: &Map<String, Value>) {
    for (key, value) in overrides {
        let Some(tag) = KEY_TO_TAG
            .iter()
            .find(|(known, _)| known == key)
            .map(|(_, tag)| *tag)
        else {
            continue;
        };
        let explicit_off = format!("!{key}");
        match value.as_bool() {
            Some(true) => {
                tags.retain(|existing| existing != &explicit_off);
                if !tags
                    .iter()
                    .any(|existing| existing.eq_ignore_ascii_case(tag))
                {
                    tags.push(tag.to_string());
                }
            }
            Some(false) => {
                tags.retain(|existing| !existing.eq_ignore_ascii_case(tag));
                if !tags.iter().any(|existing| existing == &explicit_off) {
                    tags.push(explicit_off);
                }
            }
            None => {}
        }
    }
}

/// 一条能力位覆盖：`(provider, id)` 的稀疏三态表。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CapabilityOverrideEntry {
    pub provider: String,
    pub id: String,
    /// 稀疏表：只存被覆盖的键（键名见 [`KEYS`]）。
    #[serde(default)]
    pub values: Map<String, Value>,
}

impl CapabilityOverrideEntry {
    /// 是否命中 `(provider, id)`（provider 忽略大小写，id 精确）。
    pub fn matches(&self, provider: &str, id: &str) -> bool {
        self.provider.eq_ignore_ascii_case(provider) && self.id == id
    }
}

/// 能力位覆盖存储（KV `model_capability_overrides`）。
///
/// 与 `ModelMetadataRegistry` 的分工见模块头：本层三态、按 `(provider, id)`；
/// 那一层整份替换、按模型名（`/api/models/capabilities` 的老入口继续写它）。
///
/// ── 进程级单例（而不是 AppState 字段）──────────────────────────
/// 与 `model::models_dev` 同一形态：`install(store)` 在启动时装入库句柄，
/// 之后 [`current`] 取只读快照。这样数据面（`api::openai::models`）不需要
/// `AppState` 新增字段就能读到覆盖 —— `state.rs` / `main.rs` 是集成方的
/// 共享文件，本轮不在这里加字段（接线写进交付报告由 T9 统一 patch）。
/// 未 install 时一切读取返回「无覆盖」，功能仍在、只是三态全部 inherit。
pub struct CapabilityOverrides {
    entries: RwLock<Vec<CapabilityOverrideEntry>>,
    store: Arc<FileStore>,
}

/// 进程级单例槽（库句柄）。
static STORE: OnceLock<Arc<FileStore>> = OnceLock::new();
/// 单例本体（首次 `install` 时装入）。
static INSTANCE: OnceLock<Arc<CapabilityOverrides>> = OnceLock::new();

/// 启动时装入库句柄并加载覆盖表。重复调用忽略。
///
/// 调用点（T9 统一接线）：`src/main.rs` 里紧邻 `model::models_dev::init(&store)`
/// 之后加一行 `model::capability_overrides::install(store.clone());`。
pub fn install(store: Arc<FileStore>) {
    if STORE.set(store).is_err() {
        return;
    }
    let Some(store) = STORE.get().cloned() else {
        return;
    };
    let instance = Arc::new(CapabilityOverrides::new(store));
    if let Err(error) = instance.load() {
        tracing::warn!("capability overrides load failed: {error}");
    }
    let _ = INSTANCE.set(instance);
}

/// 当前的单例（未装入时返回 `None` —— 所有读路径据此按「无覆盖」处理）。
pub fn current() -> Option<&'static Arc<CapabilityOverrides>> {
    INSTANCE.get()
}

/// 某 `(provider, id)` 的覆盖表（未装入 / 无条目 → `None`）。
pub fn lookup(provider: &str, id: &str) -> Option<Map<String, Value>> {
    current()?.get(provider, id)
}

impl CapabilityOverrides {
    pub fn new(store: Arc<FileStore>) -> Self {
        Self {
            entries: RwLock::new(Vec::new()),
            store,
        }
    }

    pub fn load(&self) -> anyhow::Result<()> {
        let raw = self
            .store
            .get::<Vec<CapabilityOverrideEntry>>(CAPABILITIES_KEY)?;
        let mut guard = self.entries.write();
        guard.clear();
        if let Some(list) = raw {
            guard.extend(list.into_iter().filter(|e| !e.values.is_empty()));
        }
        Ok(())
    }

    fn persist(&self) -> anyhow::Result<()> {
        let entries = self.entries.read().clone();
        self.store.put(CAPABILITIES_KEY, &entries)
    }

    /// 某 `(provider, id)` 的覆盖表（`None` = 没有任何覆盖 = 全部 inherit）。
    pub fn get(&self, provider: &str, id: &str) -> Option<Map<String, Value>> {
        self.entries
            .read()
            .iter()
            .find(|entry| entry.matches(provider, id))
            .map(|entry| entry.values.clone())
    }

    /// 全量条目（管理面 `capOverrides` 列用）。
    pub fn all(&self) -> Vec<CapabilityOverrideEntry> {
        self.entries.read().clone()
    }

    /// 应用补丁（**三态协议**，与 A2A `model_rules::set_capabilities` 逐条对齐）：
    /// - 键缺失 → 不改这一项（由调用方保证：patch 里没有的键不进循环）；
    /// - 键给 `null` → 清除这一项的覆盖（回到 inherit）；
    /// - 键给值 → 覆盖（布尔键要 bool）。
    ///
    /// 归一后为空的条目整条移除（它等价于「没有覆盖」）。
    /// 返回保存后的覆盖表（空表 = 已清空）。
    pub fn set(&self, provider: &str, id: &str, patch: &Map<String, Value>) -> anyhow::Result<()> {
        let provider = provider.trim();
        let id = id.trim();
        {
            let mut entries = self.entries.write();
            let index = entries.iter().position(|entry| entry.matches(provider, id));
            let mut values = index.map(|i| entries[i].values.clone()).unwrap_or_default();
            apply_patch(&mut values, patch);
            match index {
                Some(i) if values.is_empty() => {
                    entries.remove(i);
                }
                Some(i) => entries[i].values = values,
                None if values.is_empty() => {}
                None => entries.push(CapabilityOverrideEntry {
                    provider: provider.to_string(),
                    id: id.to_string(),
                    values,
                }),
            }
        }
        self.persist()
    }

    /// 删除某 `(provider, id)` 的全部覆盖（等价于逐键 restore）。
    pub fn remove(&self, provider: &str, id: &str) -> anyhow::Result<()> {
        {
            let mut entries = self.entries.write();
            entries.retain(|entry| !entry.matches(provider, id));
        }
        self.persist()
    }

    /// 模型行被移除 / 自定义登记被删时的连带清理（与 A2A `remove_custom` 的
    /// `capabilities.retain` 同语义）。
    pub fn remove_all_for(&self, provider: &str, id: &str) -> anyhow::Result<()> {
        self.remove(provider, id)
    }

    /// 覆盖了哪些键（按 [`KEYS`] 顺序，管理页「已改」标记用）。
    pub fn overridden_keys(&self, provider: &str, id: &str) -> Vec<String> {
        let Some(values) = self.get(provider, id) else {
            return Vec::new();
        };
        KEYS.iter()
            .filter(|key| values.contains_key(**key))
            .map(|key| (*key).to_string())
            .chain(
                [AUDIO_KEY, CHAT_KEY]
                    .into_iter()
                    .filter(|key| values.contains_key(*key))
                    .map(str::to_string),
            )
            .collect()
    }
}

/// 把补丁应用到稀疏表（`set` 的实现体；纯函数便于测试）。
fn apply_patch(values: &mut Map<String, Value>, patch: &Map<String, Value>) {
    for (key, value) in patch {
        if value.is_null() {
            values.remove(key);
        } else if let Some(normalized) = normalize_value(key, value) {
            values.insert(key.clone(), normalized);
        }
    }
}

/// 管理面 / 出口用的能力视图：六键齐全（未声明给 `null`，与 A2A
/// `capability::effective` 同口径 —— 管理页不能把「未知」显示成「不支持」）。
pub fn effective_object(tags: &[String], overrides: Option<&Map<String, Value>>) -> Value {
    let flags = effective_flags(tags, overrides);
    let mut result = Map::new();
    for key in KEYS {
        let value = flags.get(key).cloned().unwrap_or(Value::Null);
        result.insert(key.to_string(), value);
    }
    for key in [AUDIO_KEY, CHAT_KEY] {
        let value = flags.get(key).cloned().unwrap_or(Value::Null);
        result.insert(key.to_string(), value);
    }
    Value::Object(result)
}

/// 逐键**来源标记**（`user` / `upstream` / `modelsdev` / `null`）。
///
/// 三态优先级在上层合并（见 `model::metadata` 与出口应用点），这里只把
/// 「有没有覆盖」翻译成来源，供管理页把「用户改的」与「上游说的」分开显示。
pub fn sources_object(
    flags_upstream: &Map<String, Value>,
    flags_modelsdev: &Map<String, Value>,
    overrides: Option<&Map<String, Value>>,
) -> Value {
    let mut sources = Map::new();
    for key in KEYS.iter().copied().chain([AUDIO_KEY, CHAT_KEY]) {
        let source = if overrides.is_some_and(|values| values.contains_key(key)) {
            json!("user")
        } else if flags_upstream.contains_key(key) {
            json!("upstream")
        } else if flags_modelsdev.contains_key(key) {
            json!("modelsdev")
        } else {
            Value::Null
        };
        sources.insert(key.to_string(), source);
    }
    Value::Object(sources)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store(tag: &str) -> Arc<FileStore> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "aigx-cap-overrides-{tag}-{}-{seq}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Arc::new(FileStore::new(dir))
    }

    fn patch(pairs: &[(&str, Value)]) -> Map<String, Value> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    #[test]
    fn tri_state_set_clear_and_persist() {
        let store = temp_store("tri");
        let caps = CapabilityOverrides::new(store.clone());
        caps.load().unwrap();
        // inherit：没有任何覆盖
        assert!(caps.get("workbuddy", "kimi-k3").is_none());

        // on
        caps.set(
            "workbuddy",
            "kimi-k3",
            &patch(&[("supportsToolCall", json!(true))]),
        )
        .unwrap();
        assert_eq!(
            caps.get("workbuddy", "kimi-k3").unwrap()["supportsToolCall"],
            json!(true)
        );

        // 稀疏：只覆盖了一键，其余仍然 inherit
        assert_eq!(
            caps.overridden_keys("workbuddy", "kimi-k3"),
            vec!["supportsToolCall"]
        );

        // 再补一键（键缺失的项不动）
        caps.set(
            "workbuddy",
            "kimi-k3",
            &patch(&[("supportsImages", json!(false))]),
        )
        .unwrap();
        let values = caps.get("workbuddy", "kimi-k3").unwrap();
        assert_eq!(values.len(), 2);

        // restore（null）清除单项
        caps.set(
            "workbuddy",
            "kimi-k3",
            &patch(&[("supportsToolCall", Value::Null)]),
        )
        .unwrap();
        assert_eq!(caps.get("workbuddy", "kimi-k3").unwrap().len(), 1);

        // 清空最后一键 → 整条移除
        caps.set(
            "workbuddy",
            "kimi-k3",
            &patch(&[("supportsImages", Value::Null)]),
        )
        .unwrap();
        assert!(caps.get("workbuddy", "kimi-k3").is_none());

        // 落盘：重新装载仍是空的
        let reloaded = CapabilityOverrides::new(store);
        reloaded.load().unwrap();
        assert!(reloaded.get("workbuddy", "kimi-k3").is_none());
    }

    #[test]
    fn overrides_are_provider_scoped() {
        let store = temp_store("scope");
        let caps = CapabilityOverrides::new(store);
        caps.load().unwrap();
        caps.set(
            "workbuddy",
            "kimi-k3",
            &patch(&[("supportsToolCall", json!(false))]),
        )
        .unwrap();
        assert_eq!(caps.get("workbuddy", "kimi-k3").unwrap().len(), 1);
        // 同名模型在别家没有任何覆盖
        assert!(caps.get("catpaw", "kimi-k3").is_none());
    }

    #[test]
    fn effective_flags_user_over_upstream() {
        let tags = vec!["chat".to_string(), "tools".to_string()];
        // 上游声明 tools=true；用户覆盖成 false
        let overrides = patch(&[("supportsToolCall", json!(false))]);
        let flags = effective_flags(&tags, Some(&overrides));
        assert_eq!(
            flags["supportsToolCall"],
            json!(false),
            "user 覆盖必须压过上游"
        );
        // 没被覆盖的键沿用上游标签集
        assert_eq!(flags["supportsChat"], json!(true));
        // 上游与覆盖都没有的键 = 未出现（调用方按 false 处理）
        assert!(!flags.contains_key("supportsImages"));
    }

    #[test]
    fn apply_to_capabilities_is_idempotent_and_reversible() {
        let mut tags = vec!["chat".to_string(), "tools".to_string()];
        let overrides = patch(&[
            ("supportsToolCall", json!(false)),
            ("supportsImages", json!(true)),
        ]);
        apply_to_capabilities(&mut tags, &overrides);
        assert!(tags.contains(&"vision".to_string()));
        assert!(tags.contains(&"!supportsToolCall".to_string()));
        assert!(!tags.contains(&"tools".to_string()));

        // 幂等：再应用一次结果不变（不叠加）
        let mut again = tags.clone();
        apply_to_capabilities(&mut again, &overrides);
        assert_eq!(again, tags);

        // 撤销覆盖：从上游标签集重算 → 回到原样
        let mut restored = vec!["chat".to_string(), "tools".to_string()];
        apply_to_capabilities(&mut restored, &Map::new());
        assert_eq!(restored, vec!["chat".to_string(), "tools".to_string()]);
    }

    #[test]
    fn sources_rank_user_over_upstream_over_modelsdev() {
        let upstream = flags_from_tags(&["chat".to_string(), "tools".to_string()]);
        let modelsdev = flags_from_tags(&["vision".to_string()]);
        let overrides = patch(&[("supportsToolCall", json!(false))]);
        let sources = sources_object(&upstream, &modelsdev, Some(&overrides));
        assert_eq!(sources["supportsToolCall"], json!("user"));
        assert_eq!(sources["supportsChat"], json!("upstream"));
        assert_eq!(sources["supportsImages"], json!("modelsdev"));
        assert_eq!(sources["supportsVideo"], Value::Null);
    }

    #[test]
    fn effective_object_is_complete_with_nulls() {
        let tags = vec!["chat".to_string()];
        let object = effective_object(&tags, None);
        // 六键 + audio + chat 全部出现；未声明给 null（不是 false）
        for key in KEYS.iter().copied().chain([AUDIO_KEY, CHAT_KEY]) {
            assert!(object.get(key).is_some(), "缺键 {key}");
        }
        assert_eq!(object["supportsImages"], Value::Null);
        assert_eq!(object["supportsChat"], json!(true));
    }

    #[test]
    fn invalid_keys_and_values_are_dropped() {
        assert!(normalize_value("supportsToolCall", &json!("yes")).is_none());
        assert!(normalize_value("unknownKey", &json!(true)).is_none());
        assert!(normalize_value("supportsToolCall", &Value::Null).is_none());
        let raw = json!({
            "supportsToolCall": true,
            "supportsImages": "not-a-bool",
            "bogus": 1,
            "supportsVideo": null,
        });
        let normalized = normalize_object(Some(&raw));
        assert_eq!(normalized.len(), 1);
        assert!(normalized.contains_key("supportsToolCall"));
    }
}
