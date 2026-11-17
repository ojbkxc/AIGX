// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//!
//! 视觉兼容（上游称「图片兼容」）规则引擎 —— 对齐 Agent2API
//! `server/config/vision_compat.rs`。
//!
//! ── 这一层只做三件事 ────────────────────────────────────────
//! 1. **配置模型**：`VisionSettings { enabled, rules, assist, reroute,
//!    unknown_capability, timeout_seconds, mode, overrides }` —— 与上游字段同名；
//! 2. **规则裁决**：`rule_for` / `mode_for` / `decide` —— 命中哪条规则、走辅助识图
//!    还是整体接管、接管链是谁；
//! 3. **校验与迁移**：`validate`（上限、必填、思考等级）+ `fold_blocks` /
//!    `project_blocks`（两块设置 ↔ 规则列表，旧界面向后兼容）。
//!
//! 不碰图片编解码、不碰网络、不碰渠道 —— 那些留在 `image_compat` 与数据面。
//!
//! ── 为什么不新建顶层 crate 模块 ──────────────────────────────
//! 压缩与视觉兼容在 AIGX 共用 `[image_compat]` 配置段，且 `main.rs`（bin）
//! 与 `lib.rs` 各自声明模块。作为 `image_compat::vision` 子模块随父模块一起
//! 编译，两个 target 都无需额外 `mod` 声明 —— 也避免与压缩逻辑各持一份
//! 「视觉开关」。旧端点 `/api/image-compat/settings` 的扁平字段是**回退层**
//!（见 `legacy_from_flat`），新端点 `/api/vision-compatibility` 一旦写入
//! `Some(VisionSettings)` 即成为权威，二者不并存裁决。

//! ── 数据面消费哪些字段（别配了不生效）────────────────────────
//! - `timeout_seconds`：数据面用 `tokio::time::timeout` 包住**视觉上游调用**
//!   （不含图片下载，下载另有 20MB/30s 限制）；
//! - `reuse_results`：跨请求描述缓存（`(图片内容哈希, 视觉模型, 提示词)` → 文本，
//!   有容量上限的 LRU）；`false` 时既不读也不写；
//! - `unknown_capability` / 名单 / `fallback=designated` / `reasoning`：见 `decide`
//!   与 `image_compat::RequestImageContext`。
//!
//! ── 一个**结构性缺口**：`prefer_current_route` 不适用 ─────────────
//! AIGX 的视觉裁决发生在**选路前**（`RequestImageContext::build` 在
//! `resolve_bridges*` 之前），而「当前路由范围内的视觉模型」只有选路后才知道。
//! 要支持它必须把视觉决策挪到选路后并接入候选解析——属于结构性改动，本期不做。
//! 因此 [`validate`] 直接**拒绝**保存 `prefer_current_route = true`，管理面
//! GET 也一律回 `false`，避免出现「存了但不生效」的半死开关。

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

/// 与 `crate::api::admin::model_manage::REASONING_LEVELS` 同源
///（上游 `model_rules::REASONING_LEVELS`）。校验视觉目标的思考等级。
use crate::api::admin::model_manage::REASONING_LEVELS;

/// 每条规则最多几个接管/备选视觉模型（上游 `MAX_TARGETS`）。
pub const MAX_TARGETS: usize = 8;
/// 每种方式最多列多少模型（上游 `MAX_MODELS`）。
pub const MAX_MODELS: usize = 2000;

/// 处理方式：辅助识图 / 整体接管。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    #[default]
    Assist,
    Reroute,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Assist => "assist",
            Self::Reroute => "reroute",
        }
    }
}

/// 旧版「模型单独配置」用的处理方式（多一档「不做兼容」）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverrideMode {
    #[default]
    Assist,
    Reroute,
    Off,
}

/// 辅助识图的视觉模型来源：沿用工具服务的共享模型 / 本条自己指定。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelSource {
    #[default]
    Shared,
    Custom,
}

/// 接管失败时：停 / 按指定备选继续。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Fallback {
    #[default]
    Stop,
    Designated,
}

/// 模型图片能力未知时：先按原样发 / 直接按兼容设置处理。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnknownCapability {
    #[default]
    TryOriginal,
    Compat,
}

/// 一个视觉目标（提供商 + 模型 + 思考等级）。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VisionTarget {
    pub provider: String,
    pub model: String,
    #[serde(default)]
    pub reasoning: String,
}

/// 名单里的一项：哪个提供商的哪个模型走这种方式。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelRef {
    pub provider: String,
    pub model: String,
}

impl ModelRef {
    pub fn new(provider: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            model: model.into(),
        }
    }

    /// 精确匹配：provider 与 model 都大小写不敏感相等。
    pub fn matches(&self, provider: &str, model: &str) -> bool {
        self.provider.eq_ignore_ascii_case(provider) && self.model.eq_ignore_ascii_case(model)
    }

    /// 请求侧 provider 未知时退化为「只比模型名」（大小写不敏感）。
    ///
    /// AIGX 在选路前拿不到单一的 provider（请求只带模型名），数据面因此以
    /// `None` 调用；管理面/单测用 `Some(provider)` 走精确匹配。详见
    /// `VisionSettings::rule_for_ctx`。
    fn matches_ctx(&self, provider: Option<&str>, model: &str) -> bool {
        if !self.model.eq_ignore_ascii_case(model) {
            return false;
        }
        match provider {
            Some(provider) => self.provider.eq_ignore_ascii_case(provider),
            None => true,
        }
    }
}

fn lists_model_ctx(list: &[ModelRef], provider: Option<&str>, model: &str) -> bool {
    list.iter().any(|entry| entry.matches_ctx(provider, model))
}

/// 名单并集：投影时把同方式多条规则的模型合起来，同一个模型只留一次。
fn union_models<'a>(models: impl Iterator<Item = &'a ModelRef>) -> Vec<ModelRef> {
    let mut seen = HashSet::new();
    let mut result = Vec::new();
    for entry in models {
        if seen.insert((entry.provider.to_lowercase(), entry.model.to_lowercase())) {
            result.push(entry.clone());
        }
    }
    result
}

/// 辅助识图设置块（旧界面的读投影；`rules` 为空时也是配置输入）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct AssistSettings {
    /// 这些模型看不了图时走辅助识图。空 = 这种方式不参与。
    pub models: Vec<ModelRef>,
    pub source: ModelSource,
    pub target: Option<VisionTarget>,
    /// 旧版字段兼容保留，不再读写。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shared_target: Option<VisionTarget>,
    pub reuse_results: bool,
}

impl Default for AssistSettings {
    fn default() -> Self {
        Self {
            models: Vec::new(),
            source: ModelSource::Shared,
            target: None,
            shared_target: None,
            reuse_results: true,
        }
    }
}

/// 整体接管设置块。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct RerouteSettings {
    /// 这些模型的请求整体改交视觉模型。空 = 这种方式不参与。
    pub models: Vec<ModelRef>,
    pub prefer_current_route: bool,
    pub fallback: Fallback,
    /// 指定的接管模型与备选，按顺序尝试。
    pub targets: Vec<VisionTarget>,
}

impl Default for RerouteSettings {
    /// 手写而非 derive：`fallback` 需要 `Stop`（derive 也是），但
    /// `prefer_current_route` **有意与上游分叉为 `false`**。
    ///
    /// 上游 A2A 的默认值是 `true`（「优先当前路由范围内的视觉模型」）；AIGX 的
    /// 视觉裁决发生在**选路前**，没有这一层，且 [`validate`] 对 `true` 直接
    /// 拒绝（配置了不生效比不支持更糟）。若默认仍为 `true`，任何**不传**该
    /// 字段的请求体都会被容器级 `default` 补成 `true` 再被自己 400 ——
    /// 那会把「缺字段」变成必错。这里改 `false` 是有意分叉，不是抄错。
    fn default() -> Self {
        Self {
            models: Vec::new(),
            prefer_current_route: false,
            fallback: Fallback::Stop,
            targets: Vec::new(),
        }
    }
}

/// 一条图片兼容规则：哪些模型看不了图、怎么接。
///
/// `mode` 决定用哪一组字段：`Assist` 用 `source` / `target` / `reuse_results`；
/// `Reroute` 用 `prefer_current_route` / `fallback` / `targets`。另一组照样存着
///（用户把方式改回来时之前填的还在）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct VisionRule {
    pub enabled: bool,
    /// 这些模型看不了图时走这条规则。空 = 这条规则不生效。
    pub models: Vec<ModelRef>,
    pub mode: Mode,
    pub source: ModelSource,
    pub target: Option<VisionTarget>,
    pub reuse_results: bool,
    pub prefer_current_route: bool,
    pub fallback: Fallback,
    pub targets: Vec<VisionTarget>,
}

impl Default for VisionRule {
    /// 手写而非 derive：`reuse_results` 默认 true；`prefer_current_route` 默认
    /// **false**（有意与上游分叉 —— AIGX 在选路前裁决，该开关不适用且
    /// [`validate`] 拒 `true`，默认 true 会让「缺字段」的规则必被自己 400）。
    fn default() -> Self {
        Self {
            enabled: true,
            models: Vec::new(),
            mode: Mode::Assist,
            source: ModelSource::Shared,
            target: None,
            reuse_results: true,
            prefer_current_route: false,
            fallback: Fallback::Stop,
            targets: Vec::new(),
        }
    }
}

impl VisionRule {
    /// 从「辅助识图」那一块造一条规则。
    pub fn from_assist(assist: &AssistSettings) -> Self {
        Self {
            enabled: !assist.models.is_empty(),
            models: assist.models.clone(),
            mode: Mode::Assist,
            source: assist.source,
            target: assist.target.clone(),
            reuse_results: assist.reuse_results,
            ..Self::default()
        }
    }

    /// 从「整体接管」那一块造一条规则。
    pub fn from_reroute(reroute: &RerouteSettings) -> Self {
        Self {
            enabled: !reroute.models.is_empty(),
            models: reroute.models.clone(),
            mode: Mode::Reroute,
            prefer_current_route: reroute.prefer_current_route,
            fallback: reroute.fallback,
            targets: reroute.targets.clone(),
            ..Self::default()
        }
    }
}

/// 旧版「模型单独配置」的一条。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelOverride {
    pub provider: String,
    pub model: String,
    pub mode: OverrideMode,
}

/// 视觉兼容总配置（对齐上游 `VisionSettings`）。
///
/// `rules` 是权威形状；`assist` / `reroute` 两块是投影（见 `project_blocks`）。
/// `mode` / `overrides` 是上游旧形状，读到继续按旧语义生效（`legacy_mode`）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct VisionSettings {
    /// 旧配置缺少开关时仍按关闭读取（字段级 default）；只有整块未配置才用
    /// 结构体默认（`enabled = true`）。
    #[serde(default)]
    pub enabled: bool,
    /// 权威形状：按列表顺序匹配，第一条命中的生效。
    #[serde(default)]
    pub rules: Vec<VisionRule>,
    pub assist: AssistSettings,
    pub reroute: RerouteSettings,
    pub unknown_capability: UnknownCapability,
    pub timeout_seconds: u64,
    /// 上游旧版顶层字段：全局二选一（读到就继续按旧语义生效）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<Mode>,
    /// 上游旧版顶层字段：模型单独配置。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub overrides: Vec<ModelOverride>,
}

impl Default for VisionSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            rules: Vec::new(),
            assist: AssistSettings::default(),
            reroute: RerouteSettings::default(),
            unknown_capability: UnknownCapability::TryOriginal,
            timeout_seconds: 30,
            mode: None,
            overrides: Vec::new(),
        }
    }
}

impl VisionSettings {
    /// 还是旧版形状：全局二选一 + 模型单独配置。
    pub fn is_legacy(&self) -> bool {
        self.mode.is_some() || !self.overrides.is_empty()
    }

    /// 旧版语义：这个模型走哪种方式（`None` = 不做兼容）。不看 `enabled`。
    pub fn legacy_mode(&self, provider: &str, model: &str) -> Option<Mode> {
        match self
            .overrides
            .iter()
            .find(|entry| {
                entry.provider.eq_ignore_ascii_case(provider)
                    && entry.model.eq_ignore_ascii_case(model)
            })
            .map(|entry| entry.mode)
        {
            Some(OverrideMode::Off) => None,
            Some(OverrideMode::Assist) => Some(Mode::Assist),
            Some(OverrideMode::Reroute) => Some(Mode::Reroute),
            None => Some(self.mode.unwrap_or_default()),
        }
    }

    /// 这个模型命中的那条规则（顺序即优先级）；`None` = 不做图片兼容。
    ///
    /// 请求路径只认这一个入口：规则列表 → 旧版全局方式（+ 模型单独配置）
    /// → 还没折成规则的两块名单。
    pub fn rule_for(&self, provider: &str, model: &str) -> Option<VisionRule> {
        self.rule_for_ctx(Some(provider), model)
    }

    /// `rule_for` 的上下文版本：`provider = None` 表示请求侧 provider 未知，
    /// 名单按模型名匹配（provider 维度仍保留在配置里，供管理面精确罗列与未来接线）。
    pub fn rule_for_ctx(&self, provider: Option<&str>, model: &str) -> Option<VisionRule> {
        if !self.enabled {
            return None;
        }
        if !self.rules.is_empty() {
            return self
                .rules
                .iter()
                .filter(|rule| rule.enabled && !rule.models.is_empty())
                .find(|rule| lists_model_ctx(&rule.models, provider, model))
                .cloned();
        }
        if self.is_legacy() {
            let p = provider.unwrap_or("");
            let assist = || VisionRule {
                models: vec![ModelRef::new(p, model)],
                mode: Mode::Assist,
                source: self.assist.source,
                target: self.assist.target.clone(),
                reuse_results: self.assist.reuse_results,
                ..VisionRule::default()
            };
            let reroute = || VisionRule {
                models: vec![ModelRef::new(p, model)],
                mode: Mode::Reroute,
                prefer_current_route: self.reroute.prefer_current_route,
                fallback: self.reroute.fallback,
                targets: self.reroute.targets.clone(),
                ..VisionRule::default()
            };
            return self.legacy_mode(p, model).map(|mode| match mode {
                Mode::Assist => assist(),
                Mode::Reroute => reroute(),
            });
        }
        // 还没折成规则的两块名单：辅助识图优先（与旧口径一致）。
        if lists_model_ctx(&self.assist.models, provider, model) {
            return Some(VisionRule {
                models: vec![ModelRef::new(provider.unwrap_or(""), model)],
                ..VisionRule::from_assist(&self.assist)
            });
        }
        if lists_model_ctx(&self.reroute.models, provider, model) {
            return Some(VisionRule {
                models: vec![ModelRef::new(provider.unwrap_or(""), model)],
                ..VisionRule::from_reroute(&self.reroute)
            });
        }
        None
    }

    /// 这个模型走哪种方式。
    pub fn mode_for(&self, provider: &str, model: &str) -> Option<Mode> {
        self.rule_for(provider, model).map(|rule| rule.mode)
    }

    /// 两块设置折成规则列表（旧配置迁移用）。名单为空的那一块不折。
    pub fn fold_blocks(&mut self) {
        if !self.rules.is_empty() {
            return;
        }
        if !self.assist.models.is_empty() {
            self.rules.push(VisionRule::from_assist(&self.assist));
        }
        if !self.reroute.models.is_empty() {
            self.rules.push(VisionRule::from_reroute(&self.reroute));
        }
    }

    /// 规则 → 两块设置（投影）。名单取同方式所有规则的并集，其余字段取第一条。
    ///
    /// **空规则列表 = 不动两块设置**：旧版形状（`mode` / `overrides`，没有 `rules`）
    /// 的 assist/reroute 两块才是配置真身，投影会把它们清空 —— 那是丢配置。
    pub fn project_blocks(&mut self) {
        if self.rules.is_empty() {
            return;
        }
        let assists: Vec<&VisionRule> = self
            .rules
            .iter()
            .filter(|rule| rule.mode == Mode::Assist)
            .collect();
        let reroutes: Vec<&VisionRule> = self
            .rules
            .iter()
            .filter(|rule| rule.mode == Mode::Reroute)
            .collect();
        let first_assist = assists.first().copied();
        let first_reroute = reroutes.first().copied();
        self.assist = AssistSettings {
            models: union_models(assists.iter().flat_map(|rule| rule.models.iter())),
            source: first_assist.map(|rule| rule.source).unwrap_or_default(),
            target: first_assist.and_then(|rule| rule.target.clone()),
            shared_target: None,
            reuse_results: first_assist.map(|rule| rule.reuse_results).unwrap_or(true),
        };
        self.reroute = RerouteSettings {
            models: union_models(reroutes.iter().flat_map(|rule| rule.models.iter())),
            // 无 reroute 规则时投影出 `prefer_current_route = false`（AIGX 不适用；
            // `true` 会被 `validate` 拒，投影不能造出非法形状）。
            prefer_current_route: first_reroute
                .map(|rule| rule.prefer_current_route)
                .unwrap_or(false),
            fallback: first_reroute.map(|rule| rule.fallback).unwrap_or_default(),
            targets: first_reroute
                .map(|rule| rule.targets.clone())
                .unwrap_or_default(),
        };
    }

    /// 这种方式此刻有没有在用（接口用它决定要不要校验视觉模型是否可用）。
    pub fn uses(&self, mode: Mode) -> bool {
        if self.is_legacy() {
            return self.mode.unwrap_or_default() == mode
                || self.overrides.iter().any(|entry| {
                    matches!(
                        (entry.mode, mode),
                        (OverrideMode::Assist, Mode::Assist)
                            | (OverrideMode::Reroute, Mode::Reroute)
                    )
                });
        }
        if !self.rules.is_empty() {
            return self
                .rules
                .iter()
                .any(|rule| rule.enabled && !rule.models.is_empty() && rule.mode == mode);
        }
        match mode {
            Mode::Assist => !self.assist.models.is_empty(),
            Mode::Reroute => !self.reroute.models.is_empty(),
        }
    }

    /// 把旧版字段清掉，只留展开后的名单。
    pub fn clear_legacy(&mut self) {
        self.mode = None;
        self.overrides.clear();
    }
}

// ── 数据面裁决 ────────────────────────────────────────────────────────

/// 一次请求的视觉动作（数据面消费）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VisionAction {
    pub mode: Mode,
    /// assist：实际读图的视觉模型。
    pub assist_target: Option<VisionTarget>,
    /// reroute：接管模型链（首元素为主）。
    pub reroute_targets: Vec<VisionTarget>,
    /// assist：相同图片的识读结果是否跨请求复用（`reuse_results`）。
    pub reuse_results: bool,
    /// 视觉上游调用超时（秒；来自 `timeout_seconds`，数据面用它包一层 timeout）。
    pub timeout_seconds: u64,
}

/// `timeout_seconds` 生效范围（与 `validate` 同界；旧配置手改越界时在此夹紧）。
pub const TIMEOUT_MIN: u64 = 5;
pub const TIMEOUT_MAX: u64 = 120;

/// 生效超时（越界夹到 [5, 120]）。
pub fn effective_timeout_seconds(settings: &VisionSettings) -> u64 {
    settings.timeout_seconds.clamp(TIMEOUT_MIN, TIMEOUT_MAX)
}

/// 视觉能力三态 → 是否干预。
///
/// - `Some(true)`（模型本就支持看图）→ 不干预；
/// - `None`（能力未声明）→ 看 `unknown_capability`；
/// - `Some(false)`（明确不支持）→ 按规则干预。
fn should_intervene(settings: &VisionSettings, capability: Option<bool>) -> bool {
    match capability {
        Some(true) => false,
        None => settings.unknown_capability == UnknownCapability::Compat,
        Some(false) => true,
    }
}

/// 数据面裁决（纯函数）：返回 `None` = 保持原样（逐字节不变）。
pub fn decide(
    settings: &VisionSettings,
    provider: Option<&str>,
    model: &str,
    capability: Option<bool>,
    has_images: bool,
    shared_target: Option<VisionTarget>,
) -> Option<VisionAction> {
    if !has_images || !should_intervene(settings, capability) {
        return None;
    }
    let rule = settings.rule_for_ctx(provider, model)?;
    let timeout_seconds = effective_timeout_seconds(settings);
    match rule.mode {
        Mode::Assist => {
            let target = match rule.source {
                ModelSource::Shared => shared_target,
                ModelSource::Custom => rule.target,
            }?;
            Some(VisionAction {
                mode: Mode::Assist,
                assist_target: Some(target),
                reroute_targets: Vec::new(),
                reuse_results: rule.reuse_results,
                timeout_seconds,
            })
        }
        Mode::Reroute => {
            // AIGX 的视觉裁决发生在选路**前**，拿不到「当前路由范围内的视觉
            // 模型」；`prefer_current_route` 因此不适用（`validate` 直接拒绝
            // 保存 true）。接管一律用显式 targets（空 = 不干预）。
            if rule.targets.is_empty() {
                return None;
            }
            Some(VisionAction {
                mode: Mode::Reroute,
                assist_target: None,
                reroute_targets: rule.targets,
                reuse_results: false,
                timeout_seconds,
            })
        }
    }
}

/// 接管链的模型名（去空、大小写不敏感去重，首元素为主接管模型）。
pub fn target_chain_models(targets: &[VisionTarget]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for target in targets {
        let model = target.model.trim();
        if model.is_empty() || out.iter().any(|m| m.eq_ignore_ascii_case(model)) {
            continue;
        }
        out.push(model.to_string());
    }
    out
}

/// 旧扁平配置 → `VisionSettings`（旧端点契约的回退层）。
///
/// - `mode = "switch"` → `Mode::Reroute`，`vision_model` 为首个接管目标，
///   `fallback_models` 依次为备选（`Fallback::Designated`）；
/// - 其余 → `Mode::Assist`，`vision_model` 为识图目标（`source = Custom`）；
/// - 未配视觉模型 = 不生效（旧口径：`vision_enabled` 但模型空 → 保持原样）。
///
/// **两块设置里直接填目标**（`assist.target` / `reroute.targets`），`mode`
/// 同时保留旧语义 —— `rule_for` 在规则列表为空时走旧版分支（对任意模型
/// 生效，旧行为）；`project_blocks` 在规则列表为空时是 no-op，目标不会被清空。
/// 新端点 GET → PUT 往返因此稳定（旧形状原样回存 = 语义等价）。
///
/// `unknown_capability` 固定 `Compat`：旧实现只在「模型能力位明确不含 vision /
/// 能力未声明」时干预 —— `Compat` 正是「未声明也按设置兼容」，保住旧行为。
pub fn legacy_from_flat(
    vision_enabled: bool,
    mode: &str,
    vision_model: &str,
    fallback_models: &[String],
) -> VisionSettings {
    let reroute = mode.trim().eq_ignore_ascii_case("switch");
    let primary = vision_model.trim();
    let mut settings = VisionSettings {
        enabled: vision_enabled,
        unknown_capability: UnknownCapability::Compat,
        ..VisionSettings::default()
    };
    if reroute {
        settings.mode = Some(Mode::Reroute);
        let mut targets: Vec<VisionTarget> = Vec::new();
        if !primary.is_empty() {
            targets.push(VisionTarget {
                provider: String::new(),
                model: primary.to_string(),
                reasoning: String::new(),
            });
        }
        for model in fallback_models {
            let model = model.trim();
            if model.is_empty() || targets.iter().any(|t| t.model.eq_ignore_ascii_case(model)) {
                continue;
            }
            targets.push(VisionTarget {
                provider: String::new(),
                model: model.to_string(),
                reasoning: String::new(),
            });
        }
        settings.reroute.prefer_current_route = false;
        settings.reroute.fallback = if targets.is_empty() {
            Fallback::Stop
        } else {
            // 旧 switch 语义 = 「一律改交给配置的视觉模型」：即便只有一个目标也
            // 按 designated 表达，否则 `validate` 的 reroute 必填项会误拒它。
            Fallback::Designated
        };
        settings.reroute.targets = targets;
    } else {
        settings.mode = Some(Mode::Assist);
        if primary.is_empty() {
            // 旧口径：未配视觉模型 = 不生效（即使工具服务有共享视觉模型也不接管）。
            settings.enabled = false;
        } else {
            settings.assist.source = ModelSource::Custom;
            settings.assist.target = Some(VisionTarget {
                provider: String::new(),
                model: primary.to_string(),
                reasoning: String::new(),
            });
        }
    }
    settings
}

// ── 校验 ──────────────────────────────────────────────────────────────

fn valid_name(value: &str) -> bool {
    !value.is_empty()
        && value.trim() == value
        && value.chars().count() <= 256
        && !value.chars().any(char::is_control)
}

pub fn validate_target(target: &VisionTarget) -> Result<(), String> {
    if !valid_name(&target.provider) || !valid_name(&target.model) {
        return Err("请选择完整的提供商与模型，名称不能超过 256 个字符或含控制字符".into());
    }
    if !target.reasoning.is_empty() && !REASONING_LEVELS.contains(&target.reasoning.as_str()) {
        return Err("图片兼容的思考等级无效".into());
    }
    Ok(())
}

fn validate_model_list(label: &str, list: &[ModelRef]) -> Result<(), String> {
    let mut seen = HashSet::new();
    for entry in list {
        if !valid_name(&entry.provider) || !valid_name(&entry.model) {
            return Err(format!("{label}的模型名单里有不完整的条目"));
        }
        if !seen.insert((entry.provider.to_lowercase(), entry.model.to_lowercase())) {
            return Err(format!("{label}的模型名单里同一个模型只能出现一次"));
        }
    }
    Ok(())
}

pub fn validate(settings: &VisionSettings) -> Result<(), String> {
    if !(5..=120).contains(&settings.timeout_seconds) {
        return Err("图片兼容最长等待必须在 5 到 120 秒之间".into());
    }
    let listed: usize = settings.rules.iter().map(|rule| rule.models.len()).sum();
    if listed > MAX_MODELS {
        return Err(format!("图片兼容最多 {MAX_MODELS} 个模型"));
    }
    for (index, rule) in settings.rules.iter().enumerate() {
        let at = index + 1;
        validate_model_list(&format!("第 {at} 条规则"), &rule.models)?;
        if rule.targets.len() > MAX_TARGETS {
            return Err(format!("第 {at} 条规则最多 {MAX_TARGETS} 个备选视觉模型"));
        }
        match rule.mode {
            Mode::Assist => {
                if let Some(target) = &rule.target {
                    validate_target(target)?;
                }
            }
            Mode::Reroute => {
                let mut targets = HashSet::new();
                for target in &rule.targets {
                    validate_target(target)?;
                    if !targets
                        .insert((target.provider.to_lowercase(), target.model.to_lowercase()))
                    {
                        return Err(format!("第 {at} 条规则的备选视觉模型不能重复"));
                    }
                }
            }
        }
        // 只有真的会用上的规则才要求配全。
        if !settings.enabled || !rule.enabled || rule.models.is_empty() {
            continue;
        }
        match rule.mode {
            Mode::Assist => {
                if rule.source == ModelSource::Custom && rule.target.is_none() {
                    return Err(format!("第 {at} 条规则选了「指定视觉模型」，请先选一只"));
                }
            }
            Mode::Reroute => {
                if rule.prefer_current_route {
                    // AIGX 的视觉裁决发生在**选路前**，没有「当前路由范围内」
                    // 这一层 —— 保存它等于存了个永远不会生效的开关。直接拒绝，
                    // 逼调用方走受支持的「指定接管模型」。
                    return Err(format!(
                        "第 {at} 条规则：AIGX 的视觉兼容在选路前裁决，不支持「优先使用当前路由内的视觉模型」，请指定接管模型（targets）"
                    ));
                }
                // AIGX 的数据面只用 targets（链首接管），`fallback` 不参与裁决；
                // 因此这里只要求「有接管模型」，不再要求 fallback=designated ——
                // 否则缺 `fallback` 字段（默认 Stop）的合法规则会被自己 400。
                if rule.targets.is_empty() {
                    return Err(format!("第 {at} 条规则请指定接管回答的视觉模型（targets）"));
                }
            }
        }
    }
    if settings.assist.models.len() > MAX_MODELS
        || settings.reroute.models.len() > MAX_MODELS
        || settings.reroute.targets.len() > MAX_TARGETS
    {
        return Err(format!(
            "图片兼容每种方式最多 {MAX_MODELS} 个模型、{MAX_TARGETS} 个备选视觉模型"
        ));
    }
    validate_model_list("辅助识图", &settings.assist.models)?;
    validate_model_list("切换视觉模型", &settings.reroute.models)?;
    if let Some(target) = &settings.assist.target {
        validate_target(target)?;
    }
    if let Some(target) = &settings.assist.shared_target {
        validate_target(target)?;
    }
    let mut targets = HashSet::new();
    for target in &settings.reroute.targets {
        validate_target(target)?;
        if !targets.insert((target.provider.to_lowercase(), target.model.to_lowercase())) {
            return Err("备选视觉模型不能重复".into());
        }
    }
    let mut overrides = HashSet::new();
    for entry in &settings.overrides {
        if !valid_name(&entry.provider) || !valid_name(&entry.model) {
            return Err("模型单独配置的提供商和模型不完整".into());
        }
        if !overrides.insert((entry.provider.to_lowercase(), entry.model.to_lowercase())) {
            return Err("同一提供商的同一模型不能重复配置".into());
        }
    }
    // 以下「配全」检查只针对**两块名单形状**（旧界面）：规则列表非空时，
    // 逐条规则已在上面校验过，块级字段只是投影、不是配置真身 —— 在这里再查
    // 会拿空投影误拒合法规则（例如只发 rules 的请求，`reroute.targets` 为空）。
    if settings.rules.is_empty() {
        if settings.enabled
            && settings.uses(Mode::Assist)
            && settings.assist.source == ModelSource::Custom
            && settings.assist.target.is_none()
        {
            return Err("启用前请先选择辅助识图的视觉模型".into());
        }
        if settings.enabled && settings.uses(Mode::Reroute) {
            if settings.reroute.prefer_current_route {
                return Err(
                    "AIGX 的视觉兼容在选路前裁决，不支持「优先使用当前路由内的视觉模型」，请指定接管模型（targets）"
                        .into(),
                );
            }
            // 同逐条规则口径：只要求有接管模型（`fallback` 不参与 AIGX 裁决）。
            if settings.reroute.targets.is_empty() {
                return Err("启用前请指定接管回答的视觉模型（targets）".into());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(model: &str) -> VisionTarget {
        VisionTarget {
            provider: "p".into(),
            model: model.into(),
            reasoning: String::new(),
        }
    }

    // ── 名单匹配：大小写 + provider+model 精确 ──────────────────────
    #[test]
    fn model_ref_matches_provider_and_model_exactly_but_case_insensitively() {
        let entry = ModelRef::new("OpenAI", "GPT-4o");
        assert!(entry.matches("openai", "gpt-4o"));
        assert!(entry.matches("OPENAI", "GPT-4O"));
        // provider 不同 → 不命中（精确）
        assert!(!entry.matches("openai_compatible", "gpt-4o"));
        // 模型不同 → 不命中
        assert!(!entry.matches("openai", "gpt-4o-mini"));
        // 请求侧 provider 未知 → 只比模型名
        assert!(entry.matches_ctx(None, "gpt-4o"));
        assert!(entry.matches_ctx(Some("openai"), "gpt-4o"));
        assert!(!entry.matches_ctx(Some("anthropic"), "gpt-4o"));
    }

    #[test]
    fn rule_for_prefers_rules_then_legacy_then_blocks() {
        // 规则列表优先，按顺序取第一条命中
        let mut settings = VisionSettings {
            rules: vec![
                VisionRule {
                    models: vec![ModelRef::new("a", "m1")],
                    mode: Mode::Assist,
                    source: ModelSource::Custom,
                    target: Some(target("vision-1")),
                    ..VisionRule::default()
                },
                VisionRule {
                    models: vec![ModelRef::new("b", "m2")],
                    mode: Mode::Reroute,
                    targets: vec![target("vision-2")],
                    ..VisionRule::default()
                },
            ],
            ..VisionSettings::default()
        };
        assert_eq!(settings.mode_for("A", "M1"), Some(Mode::Assist));
        assert_eq!(settings.mode_for("b", "m2"), Some(Mode::Reroute));
        assert_eq!(settings.mode_for("c", "m3"), None);
        // 大小写不敏感
        assert_eq!(settings.mode_for("A", "m1"), Some(Mode::Assist));
        // 总开关关 → 一律 None
        settings.enabled = false;
        assert_eq!(settings.mode_for("a", "m1"), None);
    }

    #[test]
    fn legacy_and_block_shapes_still_resolve() {
        // 两块名单（没有 rules / legacy）
        let settings = VisionSettings {
            assist: AssistSettings {
                models: vec![ModelRef::new("x", "kimi")],
                source: ModelSource::Custom,
                target: Some(target("vision-x")),
                ..AssistSettings::default()
            },
            ..VisionSettings::default()
        };
        assert_eq!(settings.mode_for("x", "KIMI"), Some(Mode::Assist));

        // 旧版全局 mode + overrides
        let legacy = VisionSettings {
            mode: Some(Mode::Reroute),
            overrides: vec![ModelOverride {
                provider: "x".into(),
                model: "off-model".into(),
                mode: OverrideMode::Off,
            }],
            reroute: RerouteSettings {
                targets: vec![target("vision-r")],
                ..RerouteSettings::default()
            },
            ..VisionSettings::default()
        };
        assert_eq!(legacy.mode_for("x", "any"), Some(Mode::Reroute));
        assert_eq!(legacy.mode_for("X", "OFF-MODEL"), None);
    }

    // ── unknown_capability 两档 ─────────────────────────────────────
    #[test]
    fn unknown_capability_controls_intervention() {
        let mut settings = VisionSettings {
            rules: vec![VisionRule {
                models: vec![ModelRef::new("p", "plain")],
                mode: Mode::Reroute,
                prefer_current_route: false,
                targets: vec![target("vision")],
                ..VisionRule::default()
            }],
            unknown_capability: UnknownCapability::TryOriginal,
            ..VisionSettings::default()
        };
        // 明确不支持 → 干预
        assert!(decide(&settings, Some("p"), "plain", Some(false), true, None).is_some());
        // 明确支持 → 不干预
        assert!(decide(&settings, Some("p"), "plain", Some(true), true, None).is_none());
        // 未知 + try_original → 不干预
        assert!(decide(&settings, Some("p"), "plain", None, true, None).is_none());
        // 未知 + compat → 干预
        settings.unknown_capability = UnknownCapability::Compat;
        assert!(decide(&settings, Some("p"), "plain", None, true, None).is_some());
        // 无图 → 不干预（即使能力未知且 compat）
        assert!(decide(&settings, Some("p"), "plain", None, false, None).is_none());
    }

    // ── fallback=designated ────────────────────────────────────────
    #[test]
    fn reroute_designated_carries_target_chain() {
        let settings = VisionSettings {
            rules: vec![VisionRule {
                models: vec![ModelRef::new("p", "plain")],
                mode: Mode::Reroute,
                prefer_current_route: false,
                fallback: Fallback::Designated,
                targets: vec![target("vision-a"), target("vision-b")],
                ..VisionRule::default()
            }],
            ..VisionSettings::default()
        };
        validate(&settings).expect("designated with targets is valid");
        let action = decide(&settings, Some("p"), "plain", Some(false), true, None).unwrap();
        assert_eq!(action.mode, Mode::Reroute);
        assert_eq!(
            target_chain_models(&action.reroute_targets),
            vec!["vision-a".to_string(), "vision-b".to_string()]
        );
        // 去重 + 去空
        let deduped = target_chain_models(&[
            target("vision-a"),
            target(" VISION-A "),
            target(""),
            target("vision-b"),
        ]);
        assert_eq!(
            deduped,
            vec!["vision-a".to_string(), "vision-b".to_string()]
        );
        // designated 但没目标 → 校验拒绝
        let bad = VisionSettings {
            rules: vec![VisionRule {
                mode: Mode::Reroute,
                prefer_current_route: false,
                fallback: Fallback::Designated,
                models: vec![ModelRef::new("p", "plain")],
                targets: Vec::new(),
                ..VisionRule::default()
            }],
            ..VisionSettings::default()
        };
        assert!(validate(&bad).is_err());
    }

    #[test]
    fn assist_shared_source_uses_shared_target_and_custom_uses_rule_target() {
        let shared_settings = VisionSettings {
            rules: vec![VisionRule {
                models: vec![ModelRef::new("p", "plain")],
                mode: Mode::Assist,
                source: ModelSource::Shared,
                ..VisionRule::default()
            }],
            ..VisionSettings::default()
        };
        // shared 无共享目标 → 不干预
        assert!(decide(
            &shared_settings,
            Some("p"),
            "plain",
            Some(false),
            true,
            None
        )
        .is_none());
        // shared 有共享目标 → 用共享目标
        let action = decide(
            &shared_settings,
            Some("p"),
            "plain",
            Some(false),
            true,
            Some(target("shared-vision")),
        )
        .unwrap();
        assert_eq!(action.assist_target.unwrap().model, "shared-vision");

        let custom_settings = VisionSettings {
            rules: vec![VisionRule {
                models: vec![ModelRef::new("p", "plain")],
                mode: Mode::Assist,
                source: ModelSource::Custom,
                target: Some(target("own-vision")),
                ..VisionRule::default()
            }],
            ..VisionSettings::default()
        };
        let action = decide(
            &custom_settings,
            Some("p"),
            "plain",
            Some(false),
            true,
            Some(target("shared-vision")),
        )
        .unwrap();
        assert_eq!(action.assist_target.unwrap().model, "own-vision");
    }

    // ── 上限 ────────────────────────────────────────────────────────
    #[test]
    fn limits_reject_too_many_targets_and_models() {
        let too_many_targets: Vec<VisionTarget> = (0..=MAX_TARGETS)
            .map(|i| target(&format!("vision-{i}")))
            .collect();
        let settings = VisionSettings {
            rules: vec![VisionRule {
                models: vec![ModelRef::new("p", "plain")],
                mode: Mode::Reroute,
                targets: too_many_targets,
                prefer_current_route: false,
                ..VisionRule::default()
            }],
            ..VisionSettings::default()
        };
        assert!(validate(&settings).unwrap_err().contains("最多"));

        let too_many_models: Vec<ModelRef> = (0..=MAX_MODELS)
            .map(|i| ModelRef::new("p", format!("m-{i}")))
            .collect();
        let settings = VisionSettings {
            rules: vec![VisionRule {
                models: too_many_models,
                mode: Mode::Assist,
                source: ModelSource::Shared,
                ..VisionRule::default()
            }],
            ..VisionSettings::default()
        };
        assert!(validate(&settings).unwrap_err().contains("最多"));
    }

    #[test]
    fn validate_checks_reasoning_and_target_completeness() {
        let mut bad_reasoning = VisionRule {
            models: vec![ModelRef::new("p", "plain")],
            mode: Mode::Assist,
            source: ModelSource::Custom,
            target: Some(VisionTarget {
                provider: "p".into(),
                model: "vision".into(),
                reasoning: "ultra".into(),
            }),
            ..VisionRule::default()
        };
        let settings = VisionSettings {
            rules: vec![bad_reasoning.clone()],
            ..VisionSettings::default()
        };
        assert!(validate(&settings).unwrap_err().contains("思考等级"));
        bad_reasoning.target.as_mut().unwrap().reasoning = "high".into();
        let settings = VisionSettings {
            rules: vec![bad_reasoning],
            ..VisionSettings::default()
        };
        validate(&settings).expect("valid reasoning level");

        // custom 但没目标 → 拒绝
        let settings = VisionSettings {
            rules: vec![VisionRule {
                models: vec![ModelRef::new("p", "plain")],
                mode: Mode::Assist,
                source: ModelSource::Custom,
                target: None,
                ..VisionRule::default()
            }],
            ..VisionSettings::default()
        };
        assert!(validate(&settings).is_err());
    }

    #[test]
    fn timeout_bounds_are_enforced() {
        let mut settings = VisionSettings::default();
        settings.timeout_seconds = 4;
        assert!(validate(&settings).is_err());
        settings.timeout_seconds = 121;
        assert!(validate(&settings).is_err());
        settings.timeout_seconds = 30;
        validate(&settings).unwrap();
        // 生效值（手改越界配置也在数据面夹紧，不会变成 0s 立刻超时）
        settings.timeout_seconds = 0;
        assert_eq!(effective_timeout_seconds(&settings), TIMEOUT_MIN);
        settings.timeout_seconds = 999;
        assert_eq!(effective_timeout_seconds(&settings), TIMEOUT_MAX);
        settings.timeout_seconds = 45;
        assert_eq!(effective_timeout_seconds(&settings), 45);
    }

    /// `prefer_current_route` 是结构性缺口：保存 true 必须被拒（配了不生效
    /// 比不支持更糟）；规则与两块形状都要拦。
    #[test]
    fn prefer_current_route_is_rejected() {
        let rule_settings = VisionSettings {
            rules: vec![VisionRule {
                models: vec![ModelRef::new("p", "plain")],
                mode: Mode::Reroute,
                prefer_current_route: true,
                fallback: Fallback::Designated,
                targets: vec![target("vision")],
                ..VisionRule::default()
            }],
            ..VisionSettings::default()
        };
        let error = validate(&rule_settings).unwrap_err();
        assert!(error.contains("选路前"), "{error}");

        // 两块形状（旧界面）同样拦
        let block_settings = VisionSettings {
            reroute: RerouteSettings {
                models: vec![ModelRef::new("p", "plain")],
                prefer_current_route: true,
                fallback: Fallback::Designated,
                targets: vec![target("vision")],
            },
            ..VisionSettings::default()
        };
        assert!(validate(&block_settings).unwrap_err().contains("选路前"));

        // 指定接管模型（prefer=false + designated targets）→ 合法
        let ok = VisionSettings {
            rules: vec![VisionRule {
                models: vec![ModelRef::new("p", "plain")],
                mode: Mode::Reroute,
                prefer_current_route: false,
                fallback: Fallback::Designated,
                targets: vec![target("vision")],
                ..VisionRule::default()
            }],
            ..VisionSettings::default()
        };
        validate(&ok).expect("显式指定接管模型的 reroute 必须合法");
    }

    /// 回归：**缺 `preferCurrentRoute` 字段的请求体不得被自己 400**。
    ///
    /// 容器级 `#[serde(default)]` 会把缺字段补成 `Default`；若默认是 `true`
    /// 而 `validate` 拒 `true`，任何不传该字段的客户端都会稳定 400 —— 契约坏掉。
    /// 默认必须与 `validate` 一致（AIGX 有意与上游分叉为 `false`）。
    #[test]
    fn missing_prefer_current_route_defaults_to_false_and_validates() {
        // 1) 单独反序列化一条规则（缺全部字段）
        let rule: VisionRule = serde_json::from_str("{}").unwrap();
        assert!(!rule.prefer_current_route, "VisionRule::default 必须 false");
        assert_eq!(
            VisionRule::default().prefer_current_route,
            RerouteSettings::default().prefer_current_route,
            "两处默认值必须一致"
        );
        assert!(!RerouteSettings::default().prefer_current_route);

        // 2) 完整请求体：reroute 规则只给 models + targets，
        //    不传 preferCurrentRoute / fallback —— 必须能保存（不再 400）。
        let settings: VisionSettings = serde_json::from_str(
            r#"{
                "enabled": true,
                "rules": [{
                    "models": [{ "provider": "p", "model": "plain" }],
                    "mode": "reroute",
                    "targets": [{ "provider": "p", "model": "vision" }]
                }]
            }"#,
        )
        .unwrap();
        assert!(!settings.rules[0].prefer_current_route);
        assert_eq!(settings.rules[0].fallback, Fallback::Stop);
        validate(&settings).expect("缺 preferCurrentRoute / fallback 的 reroute 规则必须能通过");

        // 3) 两块形状同理：只给 reroute.models + targets，缺 preferCurrentRoute
        let blocks: VisionSettings = serde_json::from_str(
            r#"{
                "enabled": true,
                "reroute": {
                    "models": [{ "provider": "p", "model": "plain" }],
                    "targets": [{ "provider": "p", "model": "vision" }]
                }
            }"#,
        )
        .unwrap();
        validate(&blocks).expect("两块形状缺 preferCurrentRoute 也必须能通过");
    }

    // ── 旧面板写入的等价换算必须能通过新校验 ────────────────────────
    #[test]
    fn legacy_flat_shape_resolves_without_rules() {
        // 旧扁平形状没有模型名单：fold_blocks 折不出一条规则（名单空 = 不折），
        // 裁决必须靠旧版分支（`mode` + 块里的目标），这是旧行为的等价表达。
        let describe = legacy_from_flat(true, "describe", "qwen-vl", &[]);
        assert!(describe.rules.is_empty());
        assert_eq!(
            describe.mode_for("anywhere", "any-model"),
            Some(Mode::Assist)
        );
        validate(&describe).expect("旧 describe 形状本身合法");

        // 旧面板的 switch：一个主模型 + 两个备选，对任意模型生效
        let switch = legacy_from_flat(
            true,
            "switch",
            "qwen-vl",
            &["backup-a".to_string(), "backup-b".to_string()],
        );
        assert_eq!(
            switch.mode_for("anywhere", "any-model"),
            Some(Mode::Reroute)
        );
        validate(&switch).expect("旧 switch 形状本身合法");
        // designated + 单目标也要合法（否则单模型 switch 会被校验误拒）
        let single = legacy_from_flat(true, "switch", "qwen-vl", &[]);
        validate(&single).expect("单目标 designated 必须合法");
        // 关闭：允许留半截
        let off = legacy_from_flat(false, "switch", "", &[]);
        validate(&off).expect("关闭状态允许半截配置");
        // 开启但没配视觉模型 = 不生效（旧口径）
        assert_eq!(
            legacy_from_flat(true, "describe", "", &[])
                .mode_for("p", "m")
                .map(|_| ()),
            None
        );
    }

    /// 新端点 GET → PUT 往返：旧形状原样回存后语义不变。
    #[test]
    fn legacy_shape_round_trip_is_stable() {
        let original = legacy_from_flat(true, "switch", "qwen-vl", &["backup-a".to_string()]);
        // GET：fold（无规则 → 不变）→ project（无规则 → no-op）
        let mut served = original.clone();
        served.fold_blocks();
        served.project_blocks();
        assert_eq!(served, original, "GET 投影不能改写旧形状");
        // PUT：客户端原样回存；fold（无规则 → 不变）；project（no-op）
        let mut saved = served;
        saved.fold_blocks();
        saved.project_blocks();
        assert_eq!(saved, original, "PUT 回存必须语义等价");
        let before = decide(&original, None, "m", Some(false), true, None).unwrap();
        let after = decide(&saved, None, "m", Some(false), true, None).unwrap();
        assert_eq!(before, after);
    }

    /// 旧形状（没有 rules）投影不能清空 assist/reroute —— 那是配置真身。
    #[test]
    fn project_blocks_does_not_wipe_legacy_shape() {
        let mut legacy = legacy_from_flat(true, "describe", "qwen-vl", &[]);
        legacy.project_blocks();
        assert_eq!(
            legacy.assist.target.as_ref().map(|t| t.model.as_str()),
            Some("qwen-vl"),
            "空规则列表时投影必须是 no-op"
        );
        assert_eq!(legacy.mode, Some(Mode::Assist));
        // 有规则时投影照常生效（两块名单/字段由规则并集重建）
        let mut folded = VisionSettings {
            rules: vec![VisionRule {
                models: vec![ModelRef::new("p", "m")],
                mode: Mode::Assist,
                source: ModelSource::Custom,
                target: Some(target("vision-x")),
                ..VisionRule::default()
            }],
            ..VisionSettings::default()
        };
        folded.project_blocks();
        assert_eq!(folded.assist.models, vec![ModelRef::new("p", "m")]);
        assert_eq!(
            folded.assist.target.as_ref().map(|t| t.model.as_str()),
            Some("vision-x")
        );
    }

    /// 旧扁平 → 旧版形状 → 数据面裁决 ≡ 旧实现的判定（能力未声明也干预）。
    #[test]
    fn legacy_shape_keeps_old_unknown_capability_behavior() {
        let settings = legacy_from_flat(true, "describe", "qwen-vl", &[]);
        assert_eq!(settings.unknown_capability, UnknownCapability::Compat);
        // 能力未声明（None）仍干预 —— 旧实现「未知 = 不含 vision」的等价
        let action = decide(&settings, None, "mystery-model", None, true, None).unwrap();
        assert_eq!(action.mode, Mode::Assist);
        assert_eq!(action.assist_target.unwrap().model, "qwen-vl");
    }

    // ── serde 三态：缺字段 / null / 空数组 ──────────────────────────
    #[test]
    fn serde_handles_missing_null_and_empty_array() {
        // 空对象：`enabled` / `rules` 有字段级 `#[serde(default)]` → false / 空，
        // 其余字段走结构体默认（assist/reroute 默认、unknown=try_original、30s）。
        // 这正是上游口径：「块在、但缺总开关」按关闭读取。
        let empty: VisionSettings = serde_json::from_str("{}").unwrap();
        assert!(!empty.enabled);
        assert!(empty.rules.is_empty());
        assert!(empty.assist.target.is_none());
        assert_eq!(empty.unknown_capability, UnknownCapability::TryOriginal);
        assert_eq!(empty.timeout_seconds, 30);
        // 与「整块缺席」不同：那时调用方直接用 VisionSettings::default()（enabled=true）。
        assert!(VisionSettings::default().enabled);

        // 整块在、但 `enabled` 字段缺失：字段级 `default` = false
        //（上游刻意为之：旧配置文件没有总开关时按关闭读取）
        let block_without_enabled: VisionSettings =
            serde_json::from_str(r#"{ "assist": {}, "reroute": {} }"#).unwrap();
        assert!(!block_without_enabled.enabled, "块内缺 enabled → false");
        assert!(
            !block_without_enabled.reroute.prefer_current_route,
            "缺 preferCurrentRoute → false（AIGX 有意分叉，避免缺字段必 400）"
        );
        assert!(block_without_enabled.assist.reuse_results);

        // null 的 Option 字段 → None
        let with_null: VisionSettings = serde_json::from_str(
            r#"{ "rules": [], "assist": { "models": [], "source": "shared", "target": null, "reuseResults": true },
                 "reroute": { "models": [], "preferCurrentRoute": true, "fallback": "stop", "targets": [] },
                 "unknownCapability": "compat", "timeoutSeconds": 30 }"#,
        )
        .unwrap();
        assert!(with_null.assist.target.is_none());
        assert_eq!(with_null.unknown_capability, UnknownCapability::Compat);
        assert!(with_null.reroute.targets.is_empty());

        // 空数组 = 显式空名单
        let empty_arrays: VisionSettings = serde_json::from_str(
            r#"{ "rules": [], "assist": { "models": [], "reuseResults": true } }"#,
        )
        .unwrap();
        assert!(empty_arrays.rules.is_empty());
        assert!(empty_arrays.assist.models.is_empty());
        // 未知字段被拒绝（deny_unknown_fields）
        assert!(serde_json::from_str::<VisionSettings>(
            r#"{ "assist": { "models": [], "bogus": 1 } }"#
        )
        .is_err());
    }

    // ── fold / project 往返 ─────────────────────────────────────────
    #[test]
    fn fold_and_project_round_trip() {
        let mut settings = VisionSettings {
            assist: AssistSettings {
                models: vec![ModelRef::new("p", "a")],
                source: ModelSource::Custom,
                target: Some(target("vision-a")),
                ..AssistSettings::default()
            },
            reroute: RerouteSettings {
                models: vec![ModelRef::new("p", "r")],
                fallback: Fallback::Designated,
                targets: vec![target("vision-r")],
                prefer_current_route: false,
                ..RerouteSettings::default()
            },
            ..VisionSettings::default()
        };
        settings.fold_blocks();
        assert_eq!(settings.rules.len(), 2);
        settings.project_blocks();
        assert_eq!(settings.assist.models, vec![ModelRef::new("p", "a")]);
        assert_eq!(settings.reroute.models, vec![ModelRef::new("p", "r")]);
        assert_eq!(settings.reroute.fallback, Fallback::Designated);
    }

    #[test]
    fn legacy_from_flat_maps_describe_and_switch() {
        // describe
        let describe = legacy_from_flat(true, "describe", "qwen-vl", &[]);
        assert!(describe.is_legacy());
        assert_eq!(describe.mode, Some(Mode::Assist));
        assert_eq!(
            describe.assist.target.as_ref().map(|t| t.model.as_str()),
            Some("qwen-vl")
        );
        assert_eq!(
            decide(&describe, None, "any-model", Some(false), true, None)
                .unwrap()
                .assist_target
                .unwrap()
                .model,
            "qwen-vl"
        );
        // 未配模型 → 不生效
        assert!(decide(
            &legacy_from_flat(true, "describe", "", &[]),
            None,
            "m",
            Some(false),
            true,
            None
        )
        .is_none());
        // switch + fallbacks
        let switch = legacy_from_flat(
            true,
            "switch",
            "qwen-vl",
            &["backup-vl".to_string(), " Other-VL ".to_string()],
        );
        assert_eq!(switch.mode, Some(Mode::Reroute));
        assert_eq!(switch.reroute.fallback, Fallback::Designated);
        let action = decide(&switch, None, "any-model", Some(false), true, None).unwrap();
        assert_eq!(
            target_chain_models(&action.reroute_targets),
            vec![
                "qwen-vl".to_string(),
                "backup-vl".to_string(),
                "Other-VL".to_string()
            ]
        );
        // 关闭总开关 → 不干预
        assert!(decide(
            &legacy_from_flat(false, "switch", "qwen-vl", &[]),
            None,
            "m",
            Some(false),
            true,
            None
        )
        .is_none());
    }
}
