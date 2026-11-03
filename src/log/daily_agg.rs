//! **按天聚合**：记账时把每条请求日志折进「当天一行」的聚合行，
//! 报表读聚合而不是全量扫明细（对照 Agent2API `request_daily` 表）。
//!
//! ── 为什么 ────────────────────────────────────────────────
//! 改造前每个报表端点都 `all_sorted_asc()` 把全部请求日志逐条取回反序列化
//! 再内存聚合（O(全部历史)，日志越大越慢）。聚合行一天一条，报表代价与
//! 明细规模脱钩；明细可以按天数/容量放心清理而**历史报表不跟着消失**
//! ——聚合在记账那一刻就累计完成，与明细行是否还在无关（蓝本同语义，
//! 其 stats API 的 note 也如实告知「报表数字仍含已删请求」）。
//!
//! ── 双键布局（与蓝本的关键分歧）────────────────────────────
//! 蓝本是单管理员桌面端，`request_daily` 只有 model/provider/account 三个
//! 上游侧维度。AIGX 是多租户网关：**普通用户看个人报表的数字必须全精度**
//! （那是他自己的账单），不能容忍长尾被 Top 榜的 cap 折进「其他」。所以：
//!
//!   - `dailyagg:d:{YYYY-MM-DD}` **主档**（UTC 日，与现 handler 的
//!     `from_timestamp(created_at).format("%Y-%m-%d")` 口径逐字一致）：
//!     总量平铺 + 三个 Top 维度数组，各自带 cap 与 `__other__` 兜底组，
//!     **写侧**保证「Σ维度 = 总量」恒等式（修剪只搬运、不丢失）。
//!     by_user 维只服务管理员 Top 榜，尾部并入 `__other__`。
//!   - `dailyagg:u:{uid}:{YYYY-MM-DD}` **用户日行**：一个用户一天一行，
//!     无 cap（models 维除外，仅个人 Top5 用），个人报表全精度。
//!     uid 放 date 前，个人任意区间 = 一次 `dailyagg:u:{uid}:` 前缀枚举。
//!   - `user_id=None` 的请求（管理员级令牌）不建用户行、不进 by_user 维
//!     （与现 `user_ranking` 跳过 None 的口径一致）；主档总量照记。
//!     跨维对账时读侧用「总量 − Σby_user」把匿名流量兜进余量组
//!     （蓝本 `range_totals` 同款余量法）。
//!
//! ── 原子性 ────────────────────────────────────────────────
//! `FileStore::update` 是 get/put 两次独立拿锁、并非原子 RMW，门面也不
//! 暴露事务。这里用**一把进程级 `parking_lot::Mutex`** 串行化
//! [读主档→折→写主档→读用户行→折→写用户行]。依据：SQLite 后端本就
//! 单连接单锁串行全部写；锁序恒为「聚合锁→连接锁」，没有任何路径反向
//! 持连接锁再拿聚合锁，无死锁；PG/JSON 回退后端免费获得同一语义。
//! 失败 `tracing::warn` 吞掉——对齐蓝本「丢一条统计只是报表少一个数，
//! 绝不能影响已经成功的转发/计费」。
//!
//! ── cap 与滞回修剪 ────────────────────────────────────────
//! 每次 push 后线性找已有项（维度典型几十个，蓝本 `push_*_accum` 同款，
//! 不建索引）；仅当长度越过 `cap + cap/2` 才修剪回 top-cap：按主导指标
//! 降序保留，其余项的**全部累计值搬运**进 `__other__`（它常驻，不占 top
//! 名额）。滞回避免每次请求都做 top-K 排序。**有意分歧**：蓝本在**读侧**
//! 补余量组，这里在**写侧**就维持恒等式——写侧修剪后任何时刻读主档都是
//! 全量。
//!
//! ── 口径清单（与切换前的现算逐条对齐）─────────────────────
//! successful = `status_code < 400`（dashboard.rs 现口径）；
//! cache_hits = 命中请求数、cache_input = 命中行 input_tokens 和、
//! cache_billed = 命中行 cost 和（cache_savings.billed_cost 口径）；
//! top_sources 读侧按 by_channel 的展示名（channel_name，None→"unknown"）
//! 再聚合——与现口径一致；分组键用 channel_id（对齐 channel_health 按 id
//! join 渠道表），改名后两 id 同现名会自然合并；
//! 不引入蓝本 is_test 排除（AIGX 无此字段，26 个漏斗调用点即现有报表
//! 口径）；失败请求 token 不清零（AIGX 的日志本就记终态）。

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;

use super::RequestLog;
use crate::storage::FileStore;

/// 「其他」兜底组的固定名字。用 ASCII 且带双下划线：模型名/渠道名/用户 id
/// （uuid）都不可能自然长成这个样子，读侧按它排除即可。
pub const OTHER: &str = "__other__";

/// 主档键前缀（后接 `YYYY-MM-DD`）。
pub const MASTER_PREFIX: &str = "dailyagg:d:";
/// 用户日行键前缀（后接 `{uid}:YYYY-MM-DD`）。
pub const USER_PREFIX: &str = "dailyagg:u:";
/// 回填完成标记键。**不在**主档/用户行前缀族内，sweep/区间读不会误碰。
pub const BACKFILL_MARKER: &str = "dailyagg:backfill_done";

/// 主档 by_model 上限。
const MODEL_CAP: usize = 128;
/// 主档 by_channel 上限。
const CHANNEL_CAP: usize = 64;
/// 主档 by_user 上限（只服务管理员 Top 榜）。
const USER_CAP: usize = 256;
/// 用户行 models 维上限（只服务个人 Top5）。
const USER_MODEL_CAP: usize = 16;
/// 用户行渠道维上限（个人 Top 渠道榜）。
const USER_CHANNEL_CAP: usize = 16;

/// 主档的按模型累计项。字段并集覆盖 models/token_trend/cache 类全部端点。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelAccum {
    pub name: String,
    pub count: u64,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub cost: i64,
    /// 该模型的缓存命中请求数
    pub cache_hits: u64,
    /// 该模型命中行 input_tokens 和（saved 估算的被乘数）
    pub cache_input: u64,
    /// 该模型命中行 cost 和（saved 估算的减数）
    pub cache_billed: i64,
}

/// 主档的按渠道累计项（top_sources / channel_health）。
///
/// 分组键是 `channel_id`（对齐 channel_health 现口径——按 id join 渠道表），
/// `name` 只是最近一次记录的展示名快照（`top_sources` 按它聚合展示；
/// 渠道改名后读侧会按最新名合并展示行）。`channel_id=None` 的流量归
/// `__other__` 组（id 即 OTHER 字符串）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChannelAccum {
    pub id: String,
    pub name: Option<String>,
    pub count: u64,
    pub successful: u64,
    /// 延迟毫秒和（均值 = latency_sum / count）
    pub latency_sum: u64,
    pub cost: i64,
    /// 渠道维 token 用量（Top 渠道榜第五指标；serde default——上线前
    /// 的旧聚合行无此字段，读作 0，手动重建后补齐）。
    #[serde(default)]
    pub tokens_in: u64,
    #[serde(default)]
    pub tokens_out: u64,
}

/// 主档的按用户累计项（user_ranking，Top 榜用途，容忍并入其他）。
/// 数值三元组与现 handler 的 `(count, cost, tokens)` 完全对应。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UserAccum {
    pub id: String,
    pub count: u64,
    pub cost: i64,
    /// input+output token 和（user_ranking 的 tokens 列）
    pub tokens: u64,
}

/// 一天一行的主档。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DailyAgg {
    /// `YYYY-MM-DD`（UTC）
    pub date: String,
    pub requests: u64,
    pub successful: u64,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub cost: i64,
    pub channel_cost: i64,
    pub cache_hits: u64,
    pub cache_input: u64,
    pub cache_billed: i64,
    #[serde(default)]
    pub by_model: Vec<ModelAccum>,
    #[serde(default)]
    pub by_channel: Vec<ChannelAccum>,
    #[serde(default)]
    pub by_user: Vec<UserAccum>,
}

/// 用户行的按模型累计项：带分模型 token/成本明细，使**个人**报表
/// （report_models 用户支）与管理员同字段全精度，不受主档 cap 影响。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UserModelAccum {
    pub name: String,
    pub count: u64,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub cost: i64,
    /// 缓存分项：使个人缓存节省估算（report_scope 的 cache_savings 用户支）
    /// 与主档同算法（按模型查价 × cache_input − 实收）。旧行无这些字段读 0，
    /// rebuild 补齐。
    #[serde(default)]
    pub cache_hits: u64,
    #[serde(default)]
    pub cache_input: u64,
    #[serde(default)]
    pub cache_billed: i64,
}

/// 用户行的轻量渠道维（个人 Top 渠道榜；name 键、容忍并入 other）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChannelLite {
    pub name: String,
    pub count: u64,
    pub cost: i64,
}

/// 一个用户一天的全精度聚合行。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UserDailyRow {
    pub date: String,
    pub count: u64,
    pub successful: u64,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub cost: i64,
    pub cache_hits: u64,
    pub cache_input: u64,
    #[serde(default)]
    pub models: Vec<UserModelAccum>,
    /// 展示名口径（channel_name None→"unknown"），与主档 by_channel 键不同：
    /// 个人榜不 join 渠道表，按名字够用了。
    #[serde(default)]
    pub channels: Vec<ChannelLite>,
}

/// 回填标记值：as_of 记录回填覆盖到的最后一天（不含今天）。
///
/// `pub`：`DailyAggStore::backfill_marker`（pub fn）返回它，private 类型会触发
/// `private_interfaces`。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BackfillMarker {
    as_of: String,
}

/// unix 秒 → UTC 自然日键（与现 handler 的换算逐字一致）。
pub fn date_key(created_at: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp(created_at, 0)
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

/// 今天的 UTC 日键。
pub fn today_key() -> String {
    date_key(chrono::Utc::now().timestamp())
}

fn master_key(date: &str) -> String {
    format!("{MASTER_PREFIX}{date}")
}

fn user_key(uid: &str, date: &str) -> String {
    format!("{USER_PREFIX}{uid}:{date}")
}

/// N 个自然日保留（含今天）→ 最早应保留的日键；日键定长，字典序即时间序。
///
/// 对齐蓝本「保留 N 天 = 含今天在内的 N 个自然日」：cutoff = 今天 − (N−1)，
/// 严格小于 cutoff 的行才删。
pub fn cutoff_date_key(max_age_days: u64) -> String {
    let keep = max_age_days.saturating_sub(1) as i64;
    let now = chrono::Utc::now().timestamp();
    date_key(now - keep * 24 * 3600)
}

/// 名字型累计项的统一访问（三种维度结构 + 用户行的 models 共用泛型逻辑）。
/// `pub` 仅因 [`merge_dim`] 的公开泛型约束需要，方法仍是包内语义。
pub trait Named {
    fn name_ref(&self) -> &str;
    fn set_name_mut(&mut self, name: &str);
    /// 把 src 的**数值字段**（不含名字）累加进 self——cap 修剪搬运用。
    /// 字段全求和就是恒等式保持的定义，不需要知道来自哪条日志。
    fn merge_values_from(&mut self, src: &Self);
}

impl Named for ModelAccum {
    fn name_ref(&self) -> &str {
        &self.name
    }
    fn set_name_mut(&mut self, name: &str) {
        self.name = name.to_string();
    }
    fn merge_values_from(&mut self, src: &Self) {
        self.count += src.count;
        self.tokens_in = self.tokens_in.saturating_add(src.tokens_in);
        self.tokens_out = self.tokens_out.saturating_add(src.tokens_out);
        self.cost = self.cost.saturating_add(src.cost);
        self.cache_hits += src.cache_hits;
        self.cache_input = self.cache_input.saturating_add(src.cache_input);
        self.cache_billed = self.cache_billed.saturating_add(src.cache_billed);
    }
}

impl Named for ChannelAccum {
    fn name_ref(&self) -> &str {
        &self.id
    }
    fn set_name_mut(&mut self, name: &str) {
        self.id = name.to_string();
    }
    fn merge_values_from(&mut self, src: &Self) {
        self.count += src.count;
        self.successful += src.successful;
        self.latency_sum = self.latency_sum.saturating_add(src.latency_sum);
        self.cost = self.cost.saturating_add(src.cost);
        self.tokens_in = self.tokens_in.saturating_add(src.tokens_in);
        self.tokens_out = self.tokens_out.saturating_add(src.tokens_out);
        // 展示名搬运：self 缺名时采用被并入项的名字（修剪进 other 时不丢标签）。
        if self.name.is_none() {
            self.name = src.name.clone();
        }
    }
}

impl Named for UserAccum {
    fn name_ref(&self) -> &str {
        &self.id
    }
    fn set_name_mut(&mut self, name: &str) {
        self.id = name.to_string();
    }
    fn merge_values_from(&mut self, src: &Self) {
        self.count += src.count;
        self.cost = self.cost.saturating_add(src.cost);
        self.tokens = self.tokens.saturating_add(src.tokens);
    }
}

impl Named for UserModelAccum {
    fn name_ref(&self) -> &str {
        &self.name
    }
    fn set_name_mut(&mut self, name: &str) {
        self.name = name.to_string();
    }
    fn merge_values_from(&mut self, src: &Self) {
        self.count += src.count;
        self.tokens_in = self.tokens_in.saturating_add(src.tokens_in);
        self.tokens_out = self.tokens_out.saturating_add(src.tokens_out);
        self.cost = self.cost.saturating_add(src.cost);
        self.cache_hits += src.cache_hits;
        self.cache_input = self.cache_input.saturating_add(src.cache_input);
        self.cache_billed = self.cache_billed.saturating_add(src.cache_billed);
    }
}

impl Named for ChannelLite {
    fn name_ref(&self) -> &str {
        &self.name
    }
    fn set_name_mut(&mut self, name: &str) {
        self.name = name.to_string();
    }
    fn merge_values_from(&mut self, src: &Self) {
        self.count += src.count;
        self.cost = self.cost.saturating_add(src.cost);
    }
}

/// 把一条请求日志折进主档（总量 + 三维）。纯函数：记账与回填/重建共用同
/// 一段代码，口径永不分叉（蓝本 `fold_into_daily` 原则）。
pub fn fold_request(agg: &mut DailyAgg, log: &RequestLog) {
    agg.requests += 1;
    let ok = log.status_code < 400;
    if ok {
        agg.successful += 1;
    }
    agg.tokens_in = agg.tokens_in.saturating_add(log.input_tokens);
    agg.tokens_out = agg.tokens_out.saturating_add(log.output_tokens);
    agg.cost = agg.cost.saturating_add(log.cost);
    agg.channel_cost = agg.channel_cost.saturating_add(log.channel_cost);
    if log.cache_hit {
        agg.cache_hits += 1;
        agg.cache_input = agg.cache_input.saturating_add(log.input_tokens);
        agg.cache_billed = agg.cache_billed.saturating_add(log.cost);
    }

    push_accum(&mut agg.by_model, &log.model, |a| {
        a.count += 1;
        a.tokens_in = a.tokens_in.saturating_add(log.input_tokens);
        a.tokens_out = a.tokens_out.saturating_add(log.output_tokens);
        a.cost = a.cost.saturating_add(log.cost);
        if log.cache_hit {
            a.cache_hits += 1;
            a.cache_input = a.cache_input.saturating_add(log.input_tokens);
            a.cache_billed = a.cache_billed.saturating_add(log.cost);
        }
    });
    prune_accum(&mut agg.by_model, MODEL_CAP, |a| a.count);

    // 分组键 = channel_id（对齐 channel_health 现口径）；无 id 流量归 other。
    // 展示名每次覆盖为最近记录（渠道改名后报表跟新名）。
    let cid = log.channel_id.clone().unwrap_or_else(|| OTHER.to_string());
    push_accum(&mut agg.by_channel, &cid, |a| {
        a.count += 1;
        if ok {
            a.successful += 1;
        }
        a.latency_sum = a.latency_sum.saturating_add(log.latency_ms);
        a.cost = a.cost.saturating_add(log.cost);
        a.tokens_in = a.tokens_in.saturating_add(log.input_tokens);
        a.tokens_out = a.tokens_out.saturating_add(log.output_tokens);
        if log.channel_name.is_some() {
            a.name = log.channel_name.clone();
        }
    });
    prune_accum(&mut agg.by_channel, CHANNEL_CAP, |a| a.count);

    if let Some(uid) = &log.user_id {
        push_accum(&mut agg.by_user, uid, |a| {
            a.count += 1;
            a.cost = a.cost.saturating_add(log.cost);
            a.tokens = a
                .tokens
                .saturating_add(log.input_tokens + log.output_tokens);
        });
        prune_accum(&mut agg.by_user, USER_CAP, |a| a.count);
    }
}

/// 把一条请求日志折进用户日行。
pub fn fold_user_row(row: &mut UserDailyRow, log: &RequestLog) {
    row.count += 1;
    if log.status_code < 400 {
        row.successful += 1;
    }
    row.tokens_in = row.tokens_in.saturating_add(log.input_tokens);
    row.tokens_out = row.tokens_out.saturating_add(log.output_tokens);
    row.cost = row.cost.saturating_add(log.cost);
    if log.cache_hit {
        row.cache_hits += 1;
        row.cache_input = row.cache_input.saturating_add(log.input_tokens);
    }
    push_accum(&mut row.models, &log.model, |a| {
        a.count += 1;
        a.tokens_in = a.tokens_in.saturating_add(log.input_tokens);
        a.tokens_out = a.tokens_out.saturating_add(log.output_tokens);
        a.cost = a.cost.saturating_add(log.cost);
        if log.cache_hit {
            a.cache_hits += 1;
            a.cache_input = a.cache_input.saturating_add(log.input_tokens);
            a.cache_billed = a.cache_billed.saturating_add(log.cost);
        }
    });
    prune_accum(&mut row.models, USER_MODEL_CAP, |a| a.count);

    let ch = log
        .channel_name
        .clone()
        .unwrap_or_else(|| "unknown".to_string());
    push_accum(&mut row.channels, &ch, |a| {
        a.count += 1;
        a.cost = a.cost.saturating_add(log.cost);
    });
    prune_accum(&mut row.channels, USER_CHANNEL_CAP, |a| a.count);
}

/// 在累计数组里找/建 name 项并应用累加闭包。
fn push_accum<T, F>(list: &mut Vec<T>, name: &str, f: F)
where
    F: FnOnce(&mut T),
    T: Named + Default,
{
    if let Some(i) = list.iter().position(|item| item.name_ref() == name) {
        f(&mut list[i]);
        return;
    }
    let mut item = T::default();
    item.set_name_mut(name);
    f(&mut item);
    list.push(item);
}

/// 超阈滞回修剪：按 metric 降序保留 top-cap，其余**整体搬运**进
/// `__other__`；other 常驻、不占 top 名额、也永不进 top 排序结果。
fn prune_accum<T, M>(list: &mut Vec<T>, cap: usize, metric: M)
where
    M: Fn(&T) -> u64,
    T: Named + Default,
{
    if list.len() <= cap + cap / 2 {
        return;
    }
    // 摘出常驻 other（若有），排序、截断、搬运、放回。
    let other_pos = list.iter().position(|item| item.name_ref() == OTHER);
    let mut other = other_pos.map(|i| list.remove(i));
    list.sort_by_key(|item| std::cmp::Reverse(metric(item)));
    if list.len() <= cap {
        if let Some(o) = other {
            list.push(o);
        }
        return;
    }
    let overflow: Vec<T> = list.split_off(cap);
    for gone in overflow {
        match other.as_mut() {
            Some(o) => o.merge_values_from(&gone),
            None => {
                let mut o = T::default();
                o.set_name_mut(OTHER);
                o.merge_values_from(&gone);
                other = Some(o);
            }
        }
    }
    if let Some(o) = other {
        list.push(o);
    }
}

/// 从一批明细日志构建「日 → 主档」映射（回填/重建端点用；与记账共用
/// [`fold_request`]，口径同一条代码路径）。只产出数据，不做写库。
pub fn aggregate_logs(logs: &[RequestLog]) -> BTreeMap<String, DailyAgg> {
    let mut out: BTreeMap<String, DailyAgg> = BTreeMap::new();
    for log in logs {
        let day = date_key(log.created_at);
        if day.is_empty() {
            continue;
        }
        let agg = out.entry(day.clone()).or_insert_with(|| DailyAgg {
            date: day.clone(),
            ..Default::default()
        });
        fold_request(agg, log);
    }
    out
}

/// 同一批明细构建「uid → 日 → 用户行」。
pub fn aggregate_user_rows(
    logs: &[RequestLog],
) -> BTreeMap<String, BTreeMap<String, UserDailyRow>> {
    let mut out: BTreeMap<String, BTreeMap<String, UserDailyRow>> = BTreeMap::new();
    for log in logs {
        let Some(uid) = log.user_id.as_deref() else {
            continue;
        };
        let day = date_key(log.created_at);
        if day.is_empty() {
            continue;
        }
        let users = out.entry(uid.to_string()).or_default();
        let row = users.entry(day.clone()).or_insert_with(|| UserDailyRow {
            date: day.clone(),
            ..Default::default()
        });
        fold_user_row(row, log);
    }
    out
}

/// 读侧跨日维度合并：多天的行按键求和（复用 merge_values_from，与写侧
/// 搬运同一语义），`__other__` 组原样保留为一行。主档与用户行通用
/// （R 是任意含该维度的行类型）。排序由调用方按各端点现口径决定。
pub fn merge_dim<T, R, G>(rows: &[R], pick: G) -> Vec<T>
where
    T: Named + Default,
    G: Fn(&R) -> &[T],
{
    let mut map: std::collections::HashMap<String, T> = std::collections::HashMap::new();
    for row in rows {
        for item in pick(row) {
            let key = item.name_ref().to_string();
            // 必须先写回分组键：or_default() 造出的空项只承载数值，
            // 不 set_name_mut 的话合并结果全是没有名字的空行
            //（读侧 Top 榜会显示成空标题）。
            let e = map.entry(key.clone()).or_default();
            if e.name_ref().is_empty() {
                e.set_name_mut(&key);
            }
            e.merge_values_from(item);
        }
    }
    map.into_values().collect()
}

/// Σby_model，按 count 降序（tie：总 token 降序）——top_models/model_distribution 口径。
pub fn merge_models(rows: &[DailyAgg]) -> Vec<ModelAccum> {
    let mut v = merge_dim(rows, |r| &r.by_model);
    v.sort_by(|a, b| {
        b.count
            .cmp(&a.count)
            .then((b.tokens_in + b.tokens_out).cmp(&(a.tokens_in + a.tokens_out)))
    });
    v
}

/// Σ 用户行 models，按 count 降序——个人缓存节省估算口径（与主档 merge_models
/// 同构，仅行类型不同；OTHER 长尾组由调用方判 approx）。
pub fn merge_user_models(rows: &[UserDailyRow]) -> Vec<UserModelAccum> {
    let mut v = merge_dim(rows, |r| &r.models);
    v.sort_by(|a, b| {
        b.count
            .cmp(&a.count)
            .then((b.tokens_in + b.tokens_out).cmp(&(a.tokens_in + a.tokens_out)))
    });
    v
}

/// Σby_channel，按 count 降序——channel_health/top_sources 口径。
pub fn merge_channels(rows: &[DailyAgg]) -> Vec<ChannelAccum> {
    let mut v = merge_dim(rows, |r| &r.by_channel);
    v.sort_by_key(|c| std::cmp::Reverse(c.count));
    v
}

/// Σby_user，按 cost 降序——user_ranking 现口径（cost 主排）。
pub fn merge_users(rows: &[DailyAgg]) -> Vec<UserAccum> {
    let mut v = merge_dim(rows, |r| &r.by_user);
    v.sort_by(|a, b| b.cost.cmp(&a.cost).then(b.count.cmp(&a.count)));
    v
}

/// 当前连续活跃天数：含今天往前逐日计数，断档即停；今天无请求 = 0
/// （A2A overview「当前连续天数（含今天往前）」口径）。
pub fn streak_days(present: &std::collections::BTreeSet<String>, today: &str) -> u64 {
    let Ok(mut d) = chrono::NaiveDate::parse_from_str(today, "%Y-%m-%d") else {
        return 0;
    };
    let mut n = 0u64;
    while present.contains(&d.format("%Y-%m-%d").to_string()) {
        n += 1;
        d -= chrono::Duration::days(1);
    }
    n
}

/// 按天聚合存储。挂在 `LogStore` 内，`record_request` 漏斗调用。
pub struct DailyAggStore {
    store: Arc<FileStore>,
    /// 串行化主档/用户行的读-改-写（见模块头「原子性」）。
    lock: Mutex<()>,
}

impl DailyAggStore {
    pub fn new(store: Arc<FileStore>) -> Self {
        Self {
            store,
            lock: Mutex::new(()),
        }
    }

    /// 记账挂点：折一条日志进主档与用户行。错误只记 warn 不外抛——
    /// 报表少一条统计绝不能演变成请求失败（蓝本「丢统计不丢计费」）。
    pub fn record(&self, log: &RequestLog) {
        let day = date_key(log.created_at);
        if day.is_empty() {
            return;
        }
        let _guard = self.lock.lock();
        let result = (|| -> anyhow::Result<()> {
            let mut agg: DailyAgg =
                self.store
                    .get(&master_key(&day))?
                    .unwrap_or_else(|| DailyAgg {
                        date: day.clone(),
                        ..Default::default()
                    });
            fold_request(&mut agg, log);
            self.store.put(&master_key(&day), &agg)?;
            if let Some(uid) = &log.user_id {
                let mut row: UserDailyRow =
                    self.store
                        .get(&user_key(uid, &day))?
                        .unwrap_or(UserDailyRow {
                            date: day.clone(),
                            ..Default::default()
                        });
                fold_user_row(&mut row, log);
                self.store.put(&user_key(uid, &day), &row)?;
            }
            Ok(())
        })();
        if let Err(e) = result {
            tracing::warn!("按天聚合记账失败（{day}）：{e}");
        }
    }

    /// 读单日主档。
    pub fn load_day(&self, date: &str) -> Option<DailyAgg> {
        self.store.get(&master_key(date)).ok().flatten()
    }

    /// 读主档区间（含两端）。主档一天一键、量极小，前缀枚举即可。
    pub fn load_range(&self, from_date: &str, to_date: &str) -> Vec<DailyAgg> {
        self.prefix_rows::<DailyAgg>(MASTER_PREFIX)
            .into_iter()
            .filter(|a| {
                !a.date.is_empty() && a.date.as_str() >= from_date && a.date.as_str() <= to_date
            })
            .collect()
    }

    /// 读某用户日行区间（含两端）。键 `dailyagg:u:{uid}:` 前缀命中主键索引。
    pub fn load_user_range(&self, uid: &str, from_date: &str, to_date: &str) -> Vec<UserDailyRow> {
        let prefix = format!("{USER_PREFIX}{uid}:");
        let mut rows = self.prefix_rows::<UserDailyRow>(&prefix);
        rows.retain(|r| r.date.as_str() >= from_date && r.date.as_str() <= to_date);
        rows.sort_by(|a, b| a.date.cmp(&b.date));
        rows
    }

    /// 主档是否已有任何数据（回填前判空）。
    pub fn has_master_data(&self) -> bool {
        self.store
            .list(MASTER_PREFIX)
            .map(|v| !v.is_empty())
            .unwrap_or(false)
    }

    /// 报表读路径的切换闸门：回填标记存在才走聚合——聚合覆盖「历史（回填）
    /// 与今天（记账）」的完整链条从此闭合；标记缺失（新装未回填、或手动
    /// 清掉触发的重建窗口）时各 handler 走旧全量现算路径，数字永不为假。
    pub fn agg_available(&self) -> bool {
        self.backfill_marker().is_some()
    }

    fn prefix_rows<T>(&self, prefix: &str) -> Vec<T>
    where
        T: for<'de> Deserialize<'de>,
    {
        let Ok(keys) = self.store.list(prefix) else {
            return Vec::new();
        };
        keys.into_iter()
            .filter_map(|k| self.store.get::<T>(&k).ok().flatten())
            .collect()
    }

    /// 保留清理：删除日键**严格早于** cutoff 的主档与用户行，返回删除条数。
    ///
    /// cutoff 语义见 [`cutoff_date_key`]（含今天 N 个自然日 → 删 < cutoff）。
    pub fn prune_before(&self, cutoff: &str) -> anyhow::Result<usize> {
        let _guard = self.lock.lock();
        let mut removed = 0usize;
        for key in self.store.list(MASTER_PREFIX)? {
            if let Some(date) = key.strip_prefix(MASTER_PREFIX) {
                if date < cutoff {
                    self.store.delete(&key)?;
                    removed += 1;
                }
            }
        }
        for key in self.store.list(USER_PREFIX)? {
            let Some(rest) = key.strip_prefix(USER_PREFIX) else {
                continue;
            };
            // uid 是 uuid（无冒号），日期是尾段 10 字符。
            if let Some(date) = rest.rsplit(':').next() {
                if date.len() == 10 && date < cutoff {
                    self.store.delete(&key)?;
                    removed += 1;
                }
            }
        }
        Ok(removed)
    }

    /// 回填：把聚合行**整行覆盖**写库（今天之前的日子，调用方保证）。
    /// shrink 守卫（「重算 requests 不得小于既有行」）在调用侧做，这里
    /// 只写。逐日持锁，与运行期 RMW 串行。
    pub fn overwrite_day(&self, agg: &DailyAgg) -> anyhow::Result<()> {
        let _guard = self.lock.lock();
        self.store.put(&master_key(&agg.date), agg)
    }

    pub fn overwrite_user_row(&self, uid: &str, row: &UserDailyRow) -> anyhow::Result<()> {
        let _guard = self.lock.lock();
        self.store.put(&user_key(uid, &row.date), row)
    }

    /// 读回填标记（None = 从未回填过）。
    pub fn backfill_marker(&self) -> Option<BackfillMarker> {
        self.store.get(BACKFILL_MARKER).ok().flatten()
    }

    pub fn set_backfill_marker(&self, as_of: &str) -> anyhow::Result<()> {
        self.store.put(
            BACKFILL_MARKER,
            &BackfillMarker {
                as_of: as_of.to_string(),
            },
        )
    }

    /// 手动重建端点用：清掉标记（允许下一次自动回填重跑）。
    pub fn clear_backfill_marker(&self) -> anyhow::Result<()> {
        self.store.delete(BACKFILL_MARKER)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn log(
        user: Option<&str>,
        model: &str,
        channel: Option<&str>,
        cost: i64,
        ts: i64,
    ) -> RequestLog {
        let mut l = RequestLog::new();
        l.user_id = user.map(str::to_string);
        l.model = model.to_string();
        l.channel_name = channel.map(str::to_string);
        l.input_tokens = 10;
        l.output_tokens = 5;
        l.cost = cost;
        l.channel_cost = cost / 2;
        l.status_code = 200;
        l.latency_ms = 100;
        l.created_at = ts;
        l
    }

    /// 折叠恒等式：总量 = Σby_model（未触 cap 时精确）。
    #[test]
    fn fold_identity_sums_match_totals() {
        let mut agg = DailyAgg::default();
        fold_request(&mut agg, &log(Some("u1"), "gpt", Some("ch"), 100, 0));
        fold_request(&mut agg, &log(Some("u2"), "claude", None, 60, 1));
        fold_request(&mut agg, &log(None, "gpt", Some("ch"), 30, 2));
        assert_eq!(agg.requests, 3);
        assert_eq!(agg.successful, 3);
        let msum: i64 = agg.by_model.iter().map(|m| m.cost).sum();
        assert_eq!(msum, agg.cost);
        let csum: u64 = agg.by_channel.iter().map(|c| c.count).sum();
        assert_eq!(csum, agg.requests);
        // user_id=None 不进 by_user，主档 by_user 总量 = 2（读侧余量兜 1）。
        let usum: u64 = agg.by_user.iter().map(|u| u.count).sum();
        assert_eq!(usum, 2);
        assert_eq!(agg.requests - usum, 1);
    }

    /// 非法时间戳 → 空日键（record 侧跳过，不产生脏行）。
    #[test]
    fn invalid_timestamp_yields_empty_key() {
        assert_eq!(date_key(i64::MIN), "");
    }

    /// 跨 UTC 午夜的日键换算；定长日键字典序即时间序。
    #[test]
    fn day_keys_split_at_midnight() {
        let day0 = 1_735_689_600_i64; // 2025-01-01T00:00:00Z
        assert_eq!(date_key(day0), "2025-01-01");
        assert_eq!(date_key(day0 - 1), "2024-12-31");
        assert!("2024-12-31" < "2025-01-01");
    }

    /// cap 滞回修剪：超阈才动，残量搬运进 __other__，恒等式不破。
    ///
    /// 断言的是**滞回带**而不是精确长度：越过 `cap + cap/2` 才折叠一次、
    /// 折到 cap 条 top + 1 条 other，之后继续累加。所以任意时刻
    /// `cap + 1 <= len <= cap + cap/2 + 1`，不等于 `cap + 1`
    ///（写成精确值是误解了滞回，会随条目数涨落而假红）。
    #[test]
    fn prune_moves_overflow_into_other_without_loss() {
        let mut agg = DailyAgg::default();
        // 200 个不同模型 > MODEL_CAP + MODEL_CAP/2 = 192，必定触发过折叠。
        for i in 0..200u64 {
            fold_request(
                &mut agg,
                &log(None, &format!("model-{i:03}"), None, 1, i as i64),
            );
        }
        let hi = MODEL_CAP + MODEL_CAP / 2 + 1;
        assert!(
            agg.by_model.len() <= hi,
            "滞回上限 {hi}，实际 {}",
            agg.by_model.len()
        );
        assert!(agg.by_model.iter().any(|m| m.name == OTHER));
        let msum: u64 = agg.by_model.iter().map(|m| m.count).sum();
        assert_eq!(msum, agg.requests, "修剪必须搬运而非丢弃");
        let csum: i64 = agg.by_model.iter().map(|m| m.cost).sum();
        assert_eq!(csum, agg.cost);
    }

    /// 未触 cap 的维度不产生 __other__ 项（读侧 Top 榜干净）。
    #[test]
    fn no_other_group_below_cap() {
        let mut agg = DailyAgg::default();
        for i in 0..5u64 {
            fold_request(&mut agg, &log(None, &format!("m{i}"), None, 1, i as i64));
        }
        assert!(!agg.by_model.iter().any(|m| m.name == OTHER));
    }

    /// 缓存三字段口径与现 handler 对齐：hits/input/billed 各按命中行求和。
    #[test]
    fn cache_fields_match_existing_handler_semantics() {
        let mut agg = DailyAgg::default();
        let mut hit = log(None, "m", None, 40, 0);
        hit.cache_hit = true;
        hit.input_tokens = 1000;
        let mut miss = log(None, "m", None, 90, 1);
        miss.input_tokens = 2000;
        fold_request(&mut agg, &hit);
        fold_request(&mut agg, &miss);
        assert_eq!(agg.cache_hits, 1);
        assert_eq!(agg.cache_input, 1000);
        assert_eq!(agg.cache_billed, 40);
        let m = &agg.by_model[0];
        assert_eq!((m.cache_hits, m.cache_input, m.cache_billed), (1, 1000, 40));
    }

    /// 保留下界：N=1 时 cutoff=今天（什么都不删得掉严格 < 今天的今天行）；
    /// N=2 → 昨天。cutoff 恒为定长日键（网格对齐，prune 用严格小于）。
    #[test]
    fn cutoff_includes_today() {
        let today = today_key();
        assert_eq!(cutoff_date_key(1), today);
        let y = date_key(chrono::Utc::now().timestamp() - 86_400);
        assert_eq!(cutoff_date_key(2), y);
        assert_eq!(cutoff_date_key(0).len(), 10);
    }

    /// 用户行 models cap 修剪后 count 总和守恒。
    #[test]
    fn user_row_model_prune_conserves_count() {
        let mut row = UserDailyRow::default();
        for i in 0..30u64 {
            // 前 20 条同模型（hot），后 10 个新模型把长度推过 16+8=24 触发修剪。
            let name: String = if i < 20 {
                "hot".to_string()
            } else {
                format!("cold-{i}")
            };
            fold_user_row(&mut row, &log(Some("u"), &name, None, 1, i as i64));
        }
        let sum: u64 = row.models.iter().map(|m| m.count).sum();
        assert_eq!(sum, row.count);
        assert_eq!(row.count, 30);
    }

    /// aggregate_logs / aggregate_user_rows：按天分桶 + user None 只进主档
    ///（回填与记账同代码路径的回归锚）。
    #[test]
    fn aggregate_logs_buckets_by_day() {
        let logs = vec![
            log(Some("a"), "m1", Some("c1"), 10, 1_735_689_600),
            log(Some("b"), "m1", Some("c1"), 20, 1_735_689_600 + 100),
            log(Some("a"), "m2", Some("c2"), 30, 1_735_689_600 + 86_400),
        ];
        let days = aggregate_logs(&logs);
        assert_eq!(days.len(), 2);
        let d0 = &days["2025-01-01"];
        assert_eq!(d0.requests, 2);
        assert_eq!(d0.cost, 30);
        assert_eq!(d0.by_user.len(), 2);
        assert_eq!(d0.date, "2025-01-01");
        let users = aggregate_user_rows(&logs);
        assert_eq!(users["a"]["2025-01-01"].count, 1);
        assert_eq!(users["a"]["2025-01-02"].count, 1);
        assert_eq!(users["a"]["2025-01-02"].date, "2025-01-02");
        assert_eq!(users.len(), 2); // a 与 b，None 不建行
    }

    /// 读侧合并：同键跨日求和、id 键渠道聚合、users 按 cost 主排
    /// （user_ranking 现口径）、个人行 models 带分模型明细。
    #[test]
    fn merge_dim_sums_across_days() {
        let mk = |uid: &str, model: &str, ch: (&str, &str), cost: i64, ts: i64| {
            let mut l = log(Some(uid), model, Some(ch.1), cost, ts);
            l.channel_id = Some(ch.0.to_string());
            l
        };
        let logs = vec![
            mk("a", "m1", ("c1", "chan"), 10, 1_735_689_600),
            mk("a", "m1", ("c1", "chan"), 20, 1_735_689_690),
            mk("b", "m2", ("c2", "other"), 5, 1_735_776_000),
        ];
        let days: Vec<DailyAgg> = aggregate_logs(&logs).into_values().collect();
        let models = merge_models(&days);
        assert_eq!(
            (models[0].name.as_str(), models[0].count, models[0].cost),
            ("m1", 2, 30)
        );
        let chans = merge_channels(&days);
        assert_eq!(chans.len(), 2);
        let c1 = chans.iter().find(|c| c.id == "c1").unwrap();
        assert_eq!((c1.count, c1.successful, c1.latency_sum), (2, 2, 200));
        assert_eq!(c1.name.as_deref(), Some("chan"));
        let users = merge_users(&days);
        assert_eq!(users[0].id, "a"); // cost 30 > b 5：user_ranking 主排
        assert_eq!(users[0].tokens, 30); // (10+5)×2 条
                                         // 用户行 models 全精度明细（个人 report_models 数据源）
        let urows: Vec<UserDailyRow> = aggregate_user_rows(&logs)
            .get("a")
            .map(|m| m.values().cloned().collect())
            .unwrap();
        // 走生产合并函数（勿在测试里另写一份：曾经内联实现漏设 name，
        // 断言空名字失败，掩盖了 merge_dim 的同款 bug）
        let a_models: Vec<UserModelAccum> = merge_user_models(&urows);
        assert_eq!(a_models[0].name, "m1");
        assert_eq!(
            (
                a_models[0].tokens_in,
                a_models[0].tokens_out,
                a_models[0].cost
            ),
            (20, 10, 30)
        );
    }

    /// streak：含今天往前连算到断档；今天无请求 = 0（A2A 口径）。
    #[test]
    fn streak_counts_back_to_gap() {
        use std::collections::BTreeSet;
        let today = chrono::NaiveDate::from_ymd_opt(2026, 11, 2).unwrap();
        let fmt = |d: chrono::NaiveDate| d.format("%Y-%m-%d").to_string();
        let mut present: BTreeSet<String> = BTreeSet::new();
        for back in [0, 1, 3] {
            present.insert(fmt(today - chrono::Duration::days(back)));
        }
        assert_eq!(streak_days(&present, &fmt(today)), 2);
        assert_eq!(
            streak_days(&present, &fmt(today - chrono::Duration::days(3))),
            1
        );
        present.remove(&fmt(today));
        assert_eq!(streak_days(&present, &fmt(today)), 0);
    }
}
