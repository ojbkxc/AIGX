//! 异源递进的候选推荐与「填了也不生效」审计。
//!
//! ── 这个模块解决什么 ───────────────────────────────────────────
//! `cross_source_fallback = true` 只打开了一条**空管道**：数据面
//! （`api/openai/chat.rs::build_attempt_plan`）把 `fallback_models` 里的模型
//! 依次解析成候选，条件是「已定价（`ensure_model_priced` 通过）且有可用承载方」。
//! 列表为空 / 填了未定价模型 / 填了与主模型同厂的型号时，管道全是空洞——
//! 开关开着、体检只有一句「列表为空」，管理员不知道该填什么。
//!
//! 本模块从**模型管理视图**（`model::manage::build_view`：启用渠道 + 账号池 +
//! 手动登记的合流结果，与模型管理页同一份数据）里挑出真正能接力的候选，并把
//! 「哪些为什么不推荐」一起返回——不是扔一句「填点东西」，而是给一张可核对的账。
//!
//! ── 什么叫「真异源」──────────────────────────────────────────
//! 判据不是 `provider` id 不同。AIGX 的 provider 有 `openai_compatible` 这一档，
//! 底下可能同时挂着 DeepSeek、Moonshot、SiliconFlow……两个都叫
//! `openai_compatible` 的模型完全可能来自两个不同厂商，也可能来自同一家。
//! 所以异源判据取 **`owned_by`（厂商归属）**：它由渠道类型（`anthropic` /
//! `gemini` / `zai`）或模型名前缀（`model::metadata::infer_owned_by`）推出，
//! 是管理页「归属」列显示的那个值——界面看到什么，判定就用什么，不留第二套口径。
//! `owned_by` 拿不到时才退回 provider id（宁可判成「同源」而少推荐，也不能
//! 把同厂型号当成异源递进推荐出去——那是把「换个型号」当成「换个厂商」）。
//!
//! ── 排序（`rank`）────────────────────────────────────────────
//! 依次比较，前档相同才看下一档：
//! 1. **厂商多样性**：先给每个未用过的厂商各挑 1 个（一轮只出 1 个/家），再回头
//!    补第二梯队。理由：主模型失败时最想换的是「另一家的引擎」，同家换型号对
//!    拒绝/限流这类失败几乎无效（同一套内容策略、同一条限流池）。
//! 2. 非预览版优先（`preview`/`beta`/`alpha`/`exp`/`canary`/`nightly`/`rc`）。
//! 3. 承载方数量多者优先（渠道声明 + 账号池；承载方越多，单条挂掉后的存活率越高）。
//! 4. 已定价（硬门槛）+ 免费档靠后（免费档常常限流更狠，做接力不如付费档稳）。
//! 5. `(provider, model)` 字典序兜底，保证同一份数据每次推荐结果一致（可复现）。
//!
//! ── 性能 ────────────────────────────────────────────────────
//! 本模块纯函数、零 I/O：调用方（`api/admin/purge.rs`）负责建视图与缓存。
//! 视图构建是唯一的重活（全量渠道 × 账号 × 定价 × 映射），必须被缓存挡在
//! 页面请求之外——缓存键与语义都在调用方，本模块不持有全局状态。

use std::collections::{BTreeMap, HashMap};

use serde::Serialize;

use crate::model::manage::{ManageModel, ManageView};

/// 推荐默认条数（一次失败接力真正用得上的是「再换一家」，3 家已经覆盖绝大多数）。
pub const DEFAULT_RECOMMEND: usize = 3;
/// 推荐上限（再多也追不过 `retry_limit` 与 90s 总预算）。
pub const MAX_RECOMMEND: usize = 8;

/// 预览/试验版标记：这些型号做主力可以，做**兜底**不合适（随时下线）。
const PREVIEW_MARKERS: [&str; 7] = ["preview", "beta", "alpha", "exp", "canary", "nightly", "rc"];

/// 非对话能力标签：只有这些标签的模型接不了 chat 接力。
const NON_CHAT_TAGS: [&str; 2] = ["embedding", "rerank"];

/// 一个模型在**某一个承载方**下的可用性快照。
#[derive(Debug, Clone, PartialEq)]
pub struct ModelFacts {
    pub provider: String,
    /// 厂商归属（`owned_by`；空则回落 provider）
    pub vendor: String,
    /// 定价条目（`None` = 未定价，会被 `ensure_model_priced` 挡掉）
    pub price: Option<PriceFacts>,
    /// 渠道型承载方个数（`sources[].kind == "channel"`）
    pub channels: usize,
    /// 账号型承载方个数（`sources[].kind == "account"`）
    pub accounts: usize,
    /// 能否接 chat 接力
    pub chat_capable: bool,
    /// 是否预览/试验版
    pub preview: bool,
    pub context_length: Option<u32>,
}

/// 定价条目的展示与判定摘要（不持有 `ModelPrice`：`Eq` 无害、少一份依赖）。
#[derive(Debug, Clone, PartialEq)]
pub struct PriceFacts {
    pub input: f64,
    pub output: f64,
    pub price_type: String,
}

impl PriceFacts {
    /// 免费档：`price_type == "free"`，或按量计价但输入输出都 0。
    pub fn is_free(&self) -> bool {
        self.price_type == "free" || (self.input == 0.0 && self.output == 0.0)
    }

    /// 人话价格（每 1M token 或按次）。
    pub fn label(&self) -> String {
        if self.price_type == "free" {
            return "免费".to_string();
        }
        if self.price_type == "count" {
            return format!("按次 {}", trim_num(self.input));
        }
        format!(
            "in {} / out {} 每 1M",
            trim_num(self.input * 1000.0),
            trim_num(self.output * 1000.0)
        )
    }
}

/// 去掉浮点尾巴（`1.5` → `1.5`，`1.50` → `1.5`），避免界面出现 `0.30000000000000004`。
///
/// **只在有小数位时才裁零**：`1000` 若也 `trim_end_matches('0')` 会变成 `1`。
fn trim_num(value: f64) -> String {
    let rounded = (value * 1_000_000.0).round() / 1_000_000.0;
    let text = format!("{rounded}");
    match text.split_once('.') {
        Some((int_part, frac)) => {
            let frac = frac.trim_end_matches('0');
            if frac.is_empty() {
                int_part.to_string()
            } else {
                format!("{int_part}.{frac}")
            }
        }
        None => text,
    }
}

impl ModelFacts {
    /// 承载方总数（渠道 + 账号）。
    pub fn carriers(&self) -> usize {
        self.channels + self.accounts
    }

    /// 定价是否可查（`ensure_model_priced` 的口径）。
    pub fn priced(&self) -> bool {
        self.price.is_some()
    }

    /// 免费档（未定价也算 false —— 未定价压根不生效）。
    pub fn free(&self) -> bool {
        self.price.as_ref().is_some_and(PriceFacts::is_free)
    }
}

/// 单个模型在整体上是否「能接力」：任一承载方满足「启用 + 已定价 + 有承载方」。
#[derive(Debug, Clone, PartialEq)]
pub struct ModelAggregate {
    pub model: String,
    pub vendors: Vec<String>,
    pub best: ModelFacts,
    pub priced_anywhere: bool,
    pub carriers_total: usize,
}

/// 从管理视图抽「模型 → 事实」聚合表（所有模型，含未启用行）。
///
/// 只读视图，不再回查定价/渠道：视图里的 `price` / `sources` 就是同一份数据
/// （`build_view` 的入参是渠道存储、账号池、规则、定价、映射——模型管理页
/// 看到什么，这里就是什么）。
pub fn aggregate(view: &ManageView) -> HashMap<String, ModelAggregate> {
    // 模型名 → provider → 事实（同一模型可能由多家承载，结论取「最好的那一家」）
    let mut by_model: BTreeMap<String, BTreeMap<String, ModelFacts>> = BTreeMap::new();
    for row in &view.models {
        let facts = facts_of(row);
        by_model
            .entry(row.id.clone())
            .or_default()
            .insert(row.provider.clone(), facts);
    }
    let mut out = HashMap::with_capacity(by_model.len());
    for (model, per_provider) in by_model {
        let mut vendors: Vec<String> = per_provider
            .values()
            .map(|f| f.vendor.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        vendors.sort();
        let priced_anywhere = per_provider.values().any(ModelFacts::priced);
        let carriers_total = per_provider
            .values()
            .map(ModelFacts::carriers)
            .max()
            .unwrap_or(0);
        // 「最好的那一家」= 可接力优先，其次承载方多，最后 provider 字典序
        let best = per_provider
            .iter()
            .filter(|(_, f)| f.chat_capable && f.priced() && f.carriers() > 0)
            .max_by(|(pa, a), (pb, b)| {
                a.carriers()
                    .cmp(&b.carriers())
                    .then(usize::from(b.free()).cmp(&usize::from(a.free())))
                    // max_by 里反向比较 provider，得到「字典序最小者胜」
                    .then_with(|| pb.cmp(pa))
            })
            .map(|(_, f)| f.clone())
            .or_else(|| {
                per_provider
                    .iter()
                    .max_by(|(pa, _), (pb, _)| pb.cmp(pa))
                    .map(|(_, f)| f.clone())
            });
        let Some(best) = best else { continue };
        out.insert(
            model.clone(),
            ModelAggregate {
                model,
                vendors,
                best,
                priced_anywhere,
                carriers_total,
            },
        );
    }
    out
}

fn facts_of(row: &ManageModel) -> ModelFacts {
    let vendor = vendor_of(row);
    let channels = row.sources.iter().filter(|s| s.kind == "channel").count();
    let accounts = row.sources.iter().filter(|s| s.kind == "account").count();
    ModelFacts {
        provider: row.provider.clone(),
        vendor,
        price: row.price.as_ref().map(|p| PriceFacts {
            input: p.input_price,
            output: p.output_price,
            price_type: p.price_type.clone(),
        }),
        channels,
        accounts,
        chat_capable: chat_capable(row),
        preview: is_preview(&row.id),
        context_length: row.context_length,
    }
}

/// 厂商归属：`owned_by` 小写归一，空则回落 provider。
///
/// 不直接比较 `provider`：`openai_compatible` 一档下混着多家上游（见模块头）。
pub fn vendor_of(row: &ManageModel) -> String {
    let owned = row.owned_by.trim().to_ascii_lowercase();
    if !owned.is_empty() && owned != "aigx" && owned != "unknown" {
        return owned;
    }
    let provider = row.provider.trim().to_ascii_lowercase();
    if let Some(user) = provider.strip_prefix("user/") {
        return user.to_string();
    }
    // `plugin:<id>` / `account` 这类容器名不表达厂商，保留原样（至少能区分容器）
    provider
}

/// 能否接 chat 接力。
///
/// 判据取「能力三态出口」优先（`capability_flags`：`{supportsEmbedding,
/// supportsRerank, ...}`），缺失时退回标签集。只有「带 embedding/rerank、
/// 且不带任何对话侧标签」的模型会被排除——宁纵勿枉：接力的失败代价是 503，
/// 误排除的代价是这个候选永远不出现在推荐里。
fn chat_capable(row: &ManageModel) -> bool {
    if let Some(flags) = row.capability_flags.as_object() {
        let embedding = flags.get("supportsEmbedding").and_then(|v| v.as_bool()) == Some(true);
        let rerank = flags.get("supportsRerank").and_then(|v| v.as_bool()) == Some(true);
        if embedding || rerank {
            // 只声明了非对话能力 → 排除；但同时声明了对话能力（少见的多模态端点）则放行
            return flags.get("supportsChat").and_then(|v| v.as_bool()) == Some(true);
        }
    }
    let tags: Vec<String> = row
        .capabilities
        .iter()
        .map(|t| t.to_ascii_lowercase())
        .collect();
    if tags.is_empty() {
        return true;
    }
    let non_chat = tags.iter().any(|t| NON_CHAT_TAGS.contains(&t.as_str()));
    if !non_chat {
        return true;
    }
    // 带非对话标签：只有同时带对话侧标签时才认为可接力
    tags.iter().any(|t| {
        matches!(
            t.as_str(),
            "chat" | "tools" | "tool" | "vision" | "reasoning" | "video"
        )
    })
}

pub fn is_preview(model: &str) -> bool {
    let lower = model.to_ascii_lowercase();
    PREVIEW_MARKERS.iter().any(|k| lower.contains(k))
}

/// 被排除的原因（前端展示「为什么这些没被推荐」，避免列表看起来像漏了）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Exclusion {
    /// 与主模型同厂商（换个型号不叫异源）
    SameVendor,
    /// 与主模型同名（自己不能兜自己）
    SameModel,
    /// 未定价（`ensure_model_priced` 会挡掉）
    Unpriced,
    /// 没有启用的承载方（渠道声明 / 账号池都没有）
    NoCarrier,
    /// 不是对话模型（embedding / rerank）
    NotChat,
    /// 管理页里这一行是关的
    Disabled,
}

impl Exclusion {
    pub fn label(self) -> &'static str {
        match self {
            Self::SameVendor => "与主模型同厂商",
            Self::SameModel => "与主模型同名",
            Self::Unpriced => "未定价（写进列表也不会生效）",
            Self::NoCarrier => "无启用的渠道/账号",
            Self::NotChat => "非对话模型",
            Self::Disabled => "模型管理里已关闭",
        }
    }
}

/// 一条推荐。
#[derive(Debug, Clone, Serialize)]
pub struct Candidate {
    pub model: String,
    pub provider: String,
    pub vendor: String,
    /// 价格摘要（`in/out` 每 1M token，或「免费」）；未定价为 `None`
    pub price: Option<String>,
    pub channels: usize,
    pub accounts: usize,
    pub free: bool,
    pub preview: bool,
    pub context_length: Option<u32>,
    /// 人话理由（界面直接显示，便于管理员核对而不是盲信排序）
    pub reason: String,
}

/// 一条排除记录（模型 + 原因；同模型多原因只记第一个命中的）。
#[derive(Debug, Clone, Serialize)]
pub struct Excluded {
    pub model: String,
    pub vendor: String,
    pub code: Exclusion,
    pub reason: String,
}

/// 推荐结果。
#[derive(Debug, Clone, Serialize)]
pub struct Recommendation {
    /// 主模型（判定异源的基准）
    pub main_model: String,
    /// 主模型的厂商（判定基准；空 = 未知，此时不做同厂商排除）
    pub main_vendor: Option<String>,
    /// 主模型是否在启用清单里（不在 = 推荐按「无敌厂商基准」走，界面要提示）
    pub main_known: bool,
    /// 推荐候选（已按 `rank` 排序）
    pub candidates: Vec<Candidate>,
    /// 一键填入用的模型名清单（= `candidates[].model`，单列出来让前端零拼装）
    pub models: Vec<String>,
    /// 被排除的（同类只留前 8 条，够解释「为什么没它」）
    pub excluded: Vec<Excluded>,
    /// 池子统计（界面一句话说清「有多少可挑」）
    pub pools: Pools,
    pub limit: usize,
}

/// 池子统计。
#[derive(Debug, Clone, Default, Serialize)]
pub struct Pools {
    /// 视图里的模型行总数
    pub models_total: usize,
    /// 其中启用的行数
    pub enabled: usize,
    /// 启用且已定价
    pub enabled_priced: usize,
    /// 启用 + 已定价 + 有承载方 + 可对话（= 真正可进候选池）
    pub eligible: usize,
    /// 其中与主模型**不同厂商**的条数（= 可做真异源的池）
    pub cross_vendor: usize,
}

/// 生成推荐（纯函数）。
///
/// `main_model` 空串 = 不知道主模型：此时只按「已定价 + 有承载 + 可对话」挑，
/// 不做同厂商排除（`main_known = false`），并在理由里说明。
pub fn recommend(view: &ManageView, main_model: &str, limit: usize) -> Recommendation {
    let limit = limit.clamp(1, MAX_RECOMMEND);
    let main = main_model.trim();
    let agg = aggregate(view);
    let main_row = agg.get(main);
    let main_vendor = main_row
        .map(|a| a.best.vendor.clone())
        .filter(|v| !v.is_empty());
    let main_known = main_row.is_some();

    let mut pools = Pools {
        models_total: view.models.len(),
        ..Pools::default()
    };
    let mut excluded: Vec<Excluded> = Vec::new();
    let mut eligible: Vec<ModelAggregate> = Vec::new();
    // 「这一行启用了吗」问 O(模型数) 次：先建集合，别在循环里 `iter().any()`。
    // 196 行 × 196 次比较看着不大，但每行还要比对 String——这是页面级开销里
    // 唯一还随行数平方增长的地方（视图聚合本身是线性的）。
    let enabled: std::collections::HashSet<&str> = view
        .models
        .iter()
        .filter(|row| row.enabled)
        .map(|row| row.id.as_str())
        .collect();

    for row in &view.models {
        if row.enabled {
            pools.enabled += 1;
            if row.price.is_some() {
                pools.enabled_priced += 1;
            }
        }
    }

    for aggregate in agg.values() {
        let row_enabled = enabled.contains(aggregate.model.as_str());
        let best = &aggregate.best;
        if !row_enabled {
            push_exclusion(&mut excluded, aggregate, Exclusion::Disabled);
            continue;
        }
        if !best.chat_capable {
            push_exclusion(&mut excluded, aggregate, Exclusion::NotChat);
            continue;
        }
        if !aggregate.priced_anywhere {
            push_exclusion(&mut excluded, aggregate, Exclusion::Unpriced);
            continue;
        }
        if aggregate.carriers_total == 0 {
            push_exclusion(&mut excluded, aggregate, Exclusion::NoCarrier);
            continue;
        }
        if !main.is_empty() && aggregate.model.eq_ignore_ascii_case(main) {
            push_exclusion(&mut excluded, aggregate, Exclusion::SameModel);
            continue;
        }
        let same_vendor = match &main_vendor {
            Some(vendor) => aggregate
                .vendors
                .iter()
                .all(|v| v.eq_ignore_ascii_case(vendor)),
            None => false,
        };
        if same_vendor {
            push_exclusion(&mut excluded, aggregate, Exclusion::SameVendor);
            continue;
        }
        pools.eligible += 1;
        if main_vendor.is_some() {
            pools.cross_vendor += 1;
        }
        eligible.push(aggregate.clone());
    }

    // 排序（模块头 1–5 档）
    eligible.sort_by(|a, b| {
        let fa = &a.best;
        let fb = &b.best;
        fa.preview
            .cmp(&fb.preview)
            .then_with(|| fb.carriers().cmp(&fa.carriers()))
            .then_with(|| usize::from(fa.free()).cmp(&usize::from(fb.free())))
            .then_with(|| fa.provider.cmp(&fb.provider))
            .then_with(|| a.model.cmp(&b.model))
    });

    // 第一轮：每家厂商只取 1 个（厂商多样性）；第二轮：用剩下的补齐。
    let mut picked: Vec<ModelAggregate> = Vec::new();
    let mut used_vendors: Vec<String> = Vec::new();
    for aggregate in &eligible {
        if picked.len() >= limit {
            break;
        }
        let vendor = &aggregate.best.vendor;
        if used_vendors.iter().any(|v| v.eq_ignore_ascii_case(vendor)) {
            continue;
        }
        used_vendors.push(vendor.clone());
        picked.push(aggregate.clone());
    }
    if picked.len() < limit {
        for aggregate in &eligible {
            if picked.len() >= limit {
                break;
            }
            if picked.iter().any(|p| p.model == aggregate.model) {
                continue;
            }
            picked.push(aggregate.clone());
        }
    }

    let candidates: Vec<Candidate> = picked
        .iter()
        .map(|a| candidate_from(a, &main_vendor))
        .collect();
    let models = candidates.iter().map(|c| c.model.clone()).collect();
    excluded.truncate(32);

    Recommendation {
        main_model: main.to_string(),
        main_vendor,
        main_known,
        candidates,
        models,
        excluded,
        pools,
        limit,
    }
}

fn push_exclusion(out: &mut Vec<Excluded>, aggregate: &ModelAggregate, code: Exclusion) {
    if out.iter().any(|e| e.model == aggregate.model) {
        return;
    }
    let vendor = aggregate.best.vendor.clone();
    let reason = format!(
        "{}{}",
        code.label(),
        if aggregate.best.vendor.is_empty() {
            String::new()
        } else {
            format!("（{}）", vendor)
        }
    );
    out.push(Excluded {
        model: aggregate.model.clone(),
        vendor,
        code,
        reason,
    });
}

fn candidate_from(aggregate: &ModelAggregate, main_vendor: &Option<String>) -> Candidate {
    let best = &aggregate.best;
    let free = best.free();
    let price = best.price.as_ref().map(PriceFacts::label);
    let mut reasons: Vec<String> = Vec::new();
    match main_vendor {
        Some(vendor) => reasons.push(format!("异源厂商 {} → {}", vendor, best.vendor)),
        None => reasons.push("主模型未知，按「已定价 + 有承载 + 可对话」挑选".to_string()),
    }
    reasons.push(match (best.channels, best.accounts) {
        (0, 0) => "无承载方".to_string(),
        (c, 0) => format!("{c} 条启用渠道"),
        (0, a) => format!("{a} 个启用账号"),
        (c, a) => format!("{c} 条渠道 + {a} 个账号"),
    });
    reasons.push(if free {
        "免费档".to_string()
    } else {
        "已定价".to_string()
    });
    if best.preview {
        reasons.push("预览/试验版".to_string());
    }
    Candidate {
        model: aggregate.model.clone(),
        provider: best.provider.clone(),
        vendor: best.vendor.clone(),
        price,
        channels: best.channels,
        accounts: best.accounts,
        free,
        preview: best.preview,
        context_length: best.context_length,
        reason: reasons.join(" · "),
    }
}

/// 体检用：一条 `fallback_models` 文案的判定结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuditVerdict {
    /// 可接力（任一承载方满足启用 + 已定价 + 有承载 + 可对话）
    Ok,
    /// 视图里没有这个模型（写错了 / 上游已下线）
    Unknown,
    /// 有行但都不满足（同厂商 / 未定价 / 无承载 / 非对话 / 全关）
    Rejected(Exclusion),
}

/// 逐条审计 `fallback_models`（顺序与输入一致）。
///
/// `main` 为判定「同厂商」的基准（空 = 不判）。
pub fn audit(
    view: &ManageView,
    fallback_models: &[String],
    main: &str,
) -> Vec<(String, AuditVerdict)> {
    let agg = aggregate(view);
    let main_vendor = agg
        .get(main.trim())
        .map(|a| a.best.vendor.clone())
        .filter(|v| !v.is_empty());
    // 已填列表通常 3–16 条，这里的 `iter().any` 只跑在这几条上，不构成热点
    let enabled: std::collections::HashSet<&str> = view
        .models
        .iter()
        .filter(|row| row.enabled)
        .map(|row| row.id.as_str())
        .collect();
    fallback_models
        .iter()
        .map(|model| {
            let key = model.trim();
            let Some(aggregate) = agg.get(key) else {
                return (key.to_string(), AuditVerdict::Unknown);
            };
            let row_enabled = enabled.contains(aggregate.model.as_str());
            let best = &aggregate.best;
            let verdict = if !row_enabled {
                AuditVerdict::Rejected(Exclusion::Disabled)
            } else if !best.chat_capable {
                AuditVerdict::Rejected(Exclusion::NotChat)
            } else if !aggregate.priced_anywhere {
                AuditVerdict::Rejected(Exclusion::Unpriced)
            } else if aggregate.carriers_total == 0 {
                AuditVerdict::Rejected(Exclusion::NoCarrier)
            } else if main_vendor.as_ref().is_some_and(|vendor| {
                aggregate
                    .vendors
                    .iter()
                    .all(|v| v.eq_ignore_ascii_case(vendor))
            }) {
                AuditVerdict::Rejected(Exclusion::SameVendor)
            } else {
                AuditVerdict::Ok
            };
            (key.to_string(), verdict)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn price(inp: f64, out: f64) -> crate::pricing::ModelPrice {
        crate::pricing::ModelPrice::new("m", inp, out)
    }

    fn row(provider: &str, id: &str, owned_by: &str) -> ManageModel {
        ManageModel {
            provider: provider.to_string(),
            provider_label: provider.to_string(),
            id: id.to_string(),
            source: "builtin".to_string(),
            enabled: true,
            default_enabled: true,
            sources: vec![crate::model::manage::ModelSource {
                kind: "channel".to_string(),
                channel_id: format!("ch-{provider}"),
                name: provider.to_string(),
                account_id: None,
            }],
            owned_by: owned_by.to_string(),
            context_length: Some(128_000),
            max_output_tokens: None,
            capabilities: vec!["chat".to_string(), "tools".to_string()],
            cap_sources: Value::Null,
            capability_flags: Value::Null,
            cap_overrides: Vec::new(),
            tool_calling_mode: None,
            visibility: None,
            price: Some(price(1.0, 2.0)),
            model_ratio: 1.0,
            aliases: Vec::new(),
            alias_entries: Vec::new(),
        }
    }

    fn view(rows: Vec<ManageModel>) -> ManageView {
        ManageView {
            providers: Vec::new(),
            models: rows,
            mappings: HashMap::new(),
            mapping_entries: Vec::new(),
            dangling: Vec::new(),
            capability_store_ready: true,
            unpriced: 0,
        }
    }

    /// 真异源：同 `openai_compatible` 但 `owned_by` 不同 → 推荐；同厂只换型号 → 排除。
    ///
    /// 这条是本模块的核心判据。只比 provider 时 deepseek-v4 与 kimi-k3 都是
    /// `openai_compatible`，会被判成「同源」而不推荐（反过来把同厂的另两个型号
    /// 推荐出去也一样错）。
    #[test]
    fn recommend_uses_vendor_not_provider() {
        let mut kimi = row("openai_compatible", "kimi-k3", "moonshot");
        kimi.price = Some(price(4.0, 12.0));
        let mut deepseek_alt = row("openai_compatible", "deepseek-v4-lite", "deepseek");
        deepseek_alt.price = Some(price(0.5, 1.5));
        let main = row("openai_compatible", "deepseek-v4-pro", "deepseek");
        let rec = recommend(
            &view(vec![main, deepseek_alt, kimi.clone()]),
            "deepseek-v4-pro",
            3,
        );

        assert!(rec.main_known);
        assert_eq!(rec.main_vendor.as_deref(), Some("deepseek"));
        assert_eq!(rec.models, vec!["kimi-k3".to_string()], "只推荐异厂型号");
        assert!(
            rec.excluded
                .iter()
                .any(|e| e.model == "deepseek-v4-lite" && e.code == Exclusion::SameVendor),
            "同厂换型号必须被排除并给出原因：{:?}",
            rec.excluded
        );
        assert_eq!(rec.pools.cross_vendor, 1);
    }

    /// 未定价 / 无承载 / 非对话 三条硬门槛各自挡住候选（并且都能报出原因）。
    #[test]
    fn recommend_filters_unpriced_carrierless_and_non_chat() {
        let main = row("anthropic", "claude-sonnet-4-6", "anthropic");

        let mut unpriced = row("zai", "glm-5.3", "zhipu");
        unpriced.price = None;

        let mut no_carrier = row("gemini", "gemini-2.5-pro", "google");
        no_carrier.sources.clear();

        let mut embed = row("openai_compatible", "bge-large", "baai");
        embed.capabilities = vec!["embedding".to_string()];
        embed.capability_flags = serde_json::json!({ "supportsEmbedding": true });

        let ok = row("openai_compatible", "kimi-k3", "moonshot");

        let rec = recommend(
            &view(vec![main, unpriced, no_carrier, embed, ok]),
            "claude-sonnet-4-6",
            3,
        );
        assert_eq!(rec.models, vec!["kimi-k3".to_string()]);
        let codes: Vec<(String, Exclusion)> = rec
            .excluded
            .iter()
            .map(|e| (e.model.clone(), e.code))
            .collect();
        assert!(codes.contains(&("glm-5.3".to_string(), Exclusion::Unpriced)));
        assert!(codes.contains(&("gemini-2.5-pro".to_string(), Exclusion::NoCarrier)));
        assert!(codes.contains(&("bge-large".to_string(), Exclusion::NotChat)));
    }

    /// 厂商多样性：同厂有多个候选时，先给每家 1 个，再回头补齐。
    #[test]
    fn recommend_spreads_one_per_vendor_first() {
        let main = row("anthropic", "claude-sonnet-4-6", "anthropic");
        // moonshot 三个（承载方各不相同）+ google 一个
        let mut kimi_a = row("openai_compatible", "kimi-k3", "moonshot");
        kimi_a.sources.push(crate::model::manage::ModelSource {
            kind: "channel".to_string(),
            channel_id: "ch-2".to_string(),
            name: "bk".to_string(),
            account_id: None,
        });
        let kimi_b = row("openai_compatible", "kimi-k2", "moonshot");
        let kimi_c = row("openai_compatible", "kimi-k1", "moonshot");
        let gemini = row("gemini", "gemini-2.5-pro", "google");

        let rec = recommend(
            &view(vec![main, kimi_a, kimi_b, kimi_c, gemini]),
            "claude-sonnet-4-6",
            3,
        );
        assert_eq!(rec.models.len(), 3);
        assert_eq!(rec.candidates[0].model, "kimi-k3", "承载方最多的先出");
        assert!(
            rec.candidates.iter().any(|c| c.model == "gemini-2.5-pro"),
            "第二轮才轮得到同厂第二个：{:?}",
            rec.models
        );
        // 第一轮必然一家一个：前两个候选厂商必须不同
        assert_ne!(rec.candidates[0].vendor, rec.candidates[1].vendor);
    }

    /// 主模型未知（空串 / 不在清单）：不做同厂商排除，且 `main_known=false`。
    #[test]
    fn recommend_without_known_main_still_returns_pool() {
        let rows = vec![
            row("openai_compatible", "kimi-k3", "moonshot"),
            row("gemini", "gemini-2.5-pro", "google"),
        ];
        for main in ["", "not-in-list"] {
            let rec = recommend(&view(rows.clone()), main, 2);
            assert!(!rec.main_known, "main={main:?} 应判未知");
            assert_eq!(rec.models.len(), 2, "主模型未知时池子照给");
            assert!(rec.candidates[0].reason.contains("主模型未知"));
        }
    }

    /// 排除清单去重：同一模型只出现一次（原因取最先命中的那档）。
    #[test]
    fn excluded_list_is_deduped_by_model() {
        let main = row("anthropic", "claude-sonnet-4-6", "anthropic");
        let mut dup = row("openai_compatible", "x-model", "deepseek");
        dup.price = None;
        let rec = recommend(&view(vec![main, dup.clone(), dup]), "claude-sonnet-4-6", 3);
        let hits: Vec<&Excluded> = rec
            .excluded
            .iter()
            .filter(|e| e.model == "x-model")
            .collect();
        assert_eq!(hits.len(), 1, "同模型只记一条：{:?}", rec.excluded);
        assert_eq!(hits[0].code, Exclusion::Unpriced);
    }

    /// 审计：已填列表逐条判定（体检用），顺序与输入一致。
    #[test]
    fn audit_reports_per_entry_verdict() {
        let main = row("anthropic", "claude-sonnet-4-6", "anthropic");
        let kimi = row("openai_compatible", "kimi-k3", "moonshot");
        let mut same_vendor = row("anthropic", "claude-haiku-4", "anthropic");
        same_vendor.price = Some(price(0.3, 1.2));
        let v = view(vec![main, kimi, same_vendor]);
        let fallbacks = vec![
            "kimi-k3".to_string(),
            "claude-haiku-4".to_string(),
            "ghost-model".to_string(),
        ];
        let verdicts = audit(&v, &fallbacks, "claude-sonnet-4-6");
        assert_eq!(
            verdicts[0],
            ("kimi-k3".to_string(), AuditVerdict::Ok),
            "异厂 + 已定价 + 有渠道 = 可接力"
        );
        assert_eq!(
            verdicts[1].1,
            AuditVerdict::Rejected(Exclusion::SameVendor),
            "同厂另一型号不算异源"
        );
        assert_eq!(verdicts[2].1, AuditVerdict::Unknown);
    }

    /// 排序稳定性：同一份数据两次推荐结果逐项一致（可复现，不靠 HashMap 迭代序）。
    #[test]
    fn recommend_is_deterministic() {
        let main = row("anthropic", "claude-sonnet-4-6", "anthropic");
        let v = view(vec![
            main,
            row("openai_compatible", "kimi-k3", "moonshot"),
            row("gemini", "gemini-2.5-pro", "google"),
            row("zai", "glm-5.3", "zhipu"),
            row("openai_compatible", "qwen-max", "alibaba"),
        ]);
        let first = recommend(&v, "claude-sonnet-4-6", 4);
        for _ in 0..3 {
            assert_eq!(recommend(&v, "claude-sonnet-4-6", 4).models, first.models);
        }
        assert_eq!(first.models.len(), 4);
    }

    /// 价格数字的浮点尾巴：只在有小数位时裁零（`1000` 不能变成 `1`）。
    ///
    /// 这条是修一个真实缺陷：旧实现无条件 `trim_end_matches('0')`，
    /// `input_price = 1.0`（每 1k）换算成每 1M 是 `1000`，被裁成 `1`——
    /// 界面上显示「in 1 每 1M」，差三个数量级。
    #[test]
    fn price_labels_trim_only_fraction_zeros() {
        assert_eq!(trim_num(1000.0), "1000");
        assert_eq!(trim_num(1.5), "1.5");
        assert_eq!(trim_num(0.3), "0.3");
        assert_eq!(trim_num(0.0), "0");
        assert_eq!(trim_num(12.25), "12.25");
        assert_eq!(trim_num(2.0), "2");

        // 端到端：1.0/2.0 每 1k → 每 1M 的 1000/2000，不是 1/2
        let facts = PriceFacts {
            input: 1.0,
            output: 2.0,
            price_type: "token".to_string(),
        };
        assert_eq!(facts.label(), "in 1000 / out 2000 每 1M");
        // 免费档与按次计价的措辞
        assert_eq!(
            PriceFacts {
                input: 0.0,
                output: 0.0,
                price_type: "free".to_string(),
            }
            .label(),
            "免费"
        );
    }

    /// `limit` 夹紧：0 → 1，超上限按 `MAX_RECOMMEND`。
    #[test]
    fn limit_is_clamped() {
        let main = row("anthropic", "claude-sonnet-4-6", "anthropic");
        let mut rows = vec![main];
        for i in 0..12 {
            rows.push(row(
                "openai_compatible",
                &format!("m-{i:02}"),
                &format!("vendor-{i}"),
            ));
        }
        let v = view(rows);
        assert_eq!(recommend(&v, "", 0).models.len(), 1);
        assert_eq!(recommend(&v, "", 99).models.len(), MAX_RECOMMEND);
    }
}
