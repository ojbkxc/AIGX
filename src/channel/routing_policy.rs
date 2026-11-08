//! 路由策略配置 — 对照 Agent2API「设置 → 路由」（RoutingPane / `api/routing_api.rs`）。
//!
//! AIGX 的选路组件已齐（priority+weight 分组、RoundRobin、多因子调度、亲和粘性、
//! 断路器、AIMD），但全部**硬编码**：策略固定走「加权随机 + 多因子重排」，
//! 粘性 TTL 固定 5min/30min，断路器固定 5 次/30s。本模块把这些旋钮收敛成
//! 一份可持久化、可热更新的 `RoutingPolicy`（FileStore 键 `routing_policy`），
//! 并提供按策略分派的选路入口，供 `resolve_bridges` 替换硬编码路径。
//!
//! 与 A2A 的对照：
//! - `strategy`：A2A order/rotate/provider → AIGX weighted（加权随机，现状默认）/
//!   round_robin（确定性轮询）/ ranked（多因子 health×cost×rpm）
//! - `sticky`：A2A auto/session/turn/off → AIGX 通过亲和 TTL 表达：
//!   auto=5min/30min、session=6h/6h、off=关闭亲和
//! - `failover_backoff` / `sink`：AIGX 已有冷却/断路器/健康归档承担，不做开关
//! - 熔断参数：A2A breaker threshold/timeout/success_threshold → AIGX 阈值/冷却
//!   （AIGX 断路器 HalfOpen 成功即恢复，无独立 success_threshold，不强造）
//! - 调度权重：A2A 无此项；AIGX 把闲置的 `set_scheduler_config()` 接上

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::channel::scheduler::SchedulerPolicyConfig;
use crate::storage::FileStore;

/// 持久化键。
pub const ROUTING_POLICY_KEY: &str = "routing_policy";

/// 选路策略。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteStrategy {
    /// 加权随机 + 同优先级分组（`select_for_model` 现状，默认）
    #[default]
    Weighted,
    /// 确定性轮询（`select_for_model_round_robin`，均匀分布）
    RoundRobin,
    /// 多因子调度（health × cost × rpm 几何加权，`select_for_model_ranked`）
    Ranked,
}

impl RouteStrategy {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Weighted => "weighted",
            Self::RoundRobin => "round_robin",
            Self::Ranked => "ranked",
        }
    }

    pub fn from_str_opt(s: &str) -> Option<Self> {
        match s {
            "weighted" => Some(Self::Weighted),
            "round_robin" => Some(Self::RoundRobin),
            "ranked" => Some(Self::Ranked),
            _ => None,
        }
    }
}

/// 会话粘性档位（决定亲和缓存 TTL；对照 A2A routingSticky 四档）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StickyMode {
    /// 5 分钟粘性 / 30 分钟硬过期（A2A auto，多数场景默认）
    #[default]
    Auto,
    /// 6 小时粘性（长会话吃满上游上下文缓存；A2A session）
    Session,
    /// 每轮重选：与 Off 同为不粘（A2A turn；AIGX 无独立会话记录面，
    /// 语义上与 Off 等效，保留该档以对齐 API 契约）
    Turn,
    /// 关闭亲和，每请求独立选路（A2A off）
    Off,
}

impl StickyMode {
    /// 返回 `(sticky_ttl_secs, hard_ttl_secs)`；Turn/Off 返回 None（不粘）。
    pub fn ttls(&self) -> Option<(u64, u64)> {
        match self {
            Self::Auto => Some((5 * 60, 30 * 60)),
            Self::Session => Some((6 * 3600, 6 * 3600)),
            Self::Turn | Self::Off => None,
        }
    }

    /// 是否参与亲和（读取与写入两侧统一判据）。
    pub fn sticky_enabled(&self) -> bool {
        self.ttls().is_some()
    }

    /// 数据面便捷判据：从策略快照取「本请求是否写/读亲和」。
    pub fn sticky_enabled_from(policy: &RoutingPolicy) -> bool {
        policy.sticky.sticky_enabled()
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Session => "session",
            Self::Turn => "turn",
            Self::Off => "off",
        }
    }

    pub fn from_str_opt(s: &str) -> Option<Self> {
        match s {
            "auto" => Some(Self::Auto),
            "session" => Some(Self::Session),
            "turn" => Some(Self::Turn),
            "off" => Some(Self::Off),
            _ => None,
        }
    }
}

/// 断路器参数（对照 A2A routingBreaker*；A2A 默认关 / 5 次 / 30s / 半开成功 2 次）。
///
/// **不可 derive(Default)**：`#[serde(default = "...")]` 只作用于**反序列化**；
/// derive 出来的 `Default::default()` 给的是各字段的类型零值
/// （threshold=0 / cooldown_secs=0 / success_threshold=0），而非这里的业务默认
/// （5 / 30 / 2）。阈值 0 意味着断路器在**首次失败**就打开 —— 那等于把整条
/// 渠道链路停掉。手写 impl 见下。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BreakerPolicy {
    /// 断路器开关（false = 不熔断，只保留冷却/AIMD）
    #[serde(default = "default_breaker_enabled")]
    pub enabled: bool,
    /// 连续失败多少次打开断路器
    #[serde(default = "default_breaker_threshold")]
    pub threshold: u32,
    /// 打开后冷却秒数（HalfOpen 试探前）
    #[serde(default = "default_breaker_cooldown")]
    pub cooldown_secs: u64,
    /// HalfOpen 连续成功多少次才真正关闭（A2A routingBreakerSuccessThreshold；
    /// 1 = 单次试探成功即恢复现状行为）
    #[serde(default = "default_breaker_success_threshold")]
    pub success_threshold: u32,
}

fn default_breaker_enabled() -> bool {
    false
}
fn default_breaker_threshold() -> u32 {
    5
}
fn default_breaker_cooldown() -> u64 {
    30
}
fn default_breaker_success_threshold() -> u32 {
    2
}

/// 手写（非 derive）：默认值走上面那组 `default_breaker_*`，与反序列化口径一致。
impl Default for BreakerPolicy {
    fn default() -> Self {
        Self {
            enabled: default_breaker_enabled(),
            threshold: default_breaker_threshold(),
            cooldown_secs: default_breaker_cooldown(),
            success_threshold: default_breaker_success_threshold(),
        }
    }
}

// ── #40 路由多规则体系（契约冻结稿：20-routing-contract-freeze.md，仓库外）──
//
// 三条不可动摇的形态（契约 §0/§2.1/§2.3）：
// 1. **不新建平行体系**：规则档案是 `RoutingPolicy` 的**字段**，从而复用
//    `RoutingPolicyStore` 的单次 `snapshot()` —— 每请求仍只一次 Arc 指针拷贝，
//    无 IO / 无 DB / 无 KV / 无新锁。
// 2. **逐字段三态覆盖**（不是 A2A 的「档案整体替换」）：`None` = 继承下层，
//    `Some(默认值)` = 显式置为该值，`Some(空集合)` = 显式清空。与本仓
//    `model::ModelMapper` 的 `reasoning` 字段既有三态口径一致 —— 否则同一个
//    仓库里会并存两套覆盖语义，后来者无法从仓库自身推断该用哪套。
// 3. **档案以「请求名」为键、以「全局映射名」兜底**（同 `pricing` 的
//    `resolve_priced_name(requested, upstream)` 范式）。不用渠道级映射名为键：
//    解析发生在候选渠道选择**之前**，此刻不存在唯一上游名，且多对一无反向索引。

/// 路由档案各项上限（契约 §1/§3：**超限拒绝写入**，不静默截断）。
pub mod caps {
    /// 模型级档案数上限
    pub const MAX_MODELS: usize = 500;
    /// 参与 / 排除提供商名单长度上限
    pub const MAX_PROVIDERS: usize = 256;
    /// 账号顺序 / 排除账号名单长度上限
    pub const MAX_ACCOUNTS: usize = 10_000;
    /// 路由组数上限
    pub const MAX_GROUPS: usize = 500;
    /// 单组成员数上限
    pub const MAX_MEMBERS: usize = 256;
    /// 单层排序规则条数上限（对齐 A2A）
    pub const MAX_SORT_RULES: usize = 2;
}

/// 提供商作用域。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderScope {
    /// 自动并入新出现的提供商（A2A providerScope=all 的语义）
    #[default]
    All,
    /// 只用 `providers` 列出的
    Listed,
}

/// 参与名单（AIGX 口径的 provider / 账号标识）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RoutingScope {
    #[serde(default)]
    pub provider_scope: ProviderScope,
    /// 上限 [`caps::MAX_PROVIDERS`]
    #[serde(default)]
    pub providers: Vec<String>,
    /// 上限 [`caps::MAX_PROVIDERS`]
    #[serde(default)]
    pub disabled_providers: Vec<String>,
    /// 账号 id 优先级顺序（先出现的先试）；上限 [`caps::MAX_ACCOUNTS`]
    #[serde(default)]
    pub account_order: Vec<String>,
    /// 上限 [`caps::MAX_ACCOUNTS`]
    #[serde(default)]
    pub excluded_accounts: Vec<String>,
}

/// 能力需求（请求体推出：tools / images / video）。
///
/// AIGX 无需扩数据模型：账号目录已产出 `tool` / `vision` / `video` 标签，
/// 直接做集合包含判定（映射处只有 [`RequestNeeds::required_tags`] 一处）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestNeeds {
    #[serde(default)]
    pub tools: bool,
    #[serde(default)]
    pub images: bool,
    #[serde(default)]
    pub video: bool,
}

impl RequestNeeds {
    /// 与本 crate 能力标签对齐的**唯一**映射处（别在别处再写一遍）。
    pub fn required_tags(&self) -> impl Iterator<Item = &'static str> {
        [
            self.tools.then_some("tool"),
            self.images.then_some("vision"),
            self.video.then_some("video"),
        ]
        .into_iter()
        .flatten()
    }

    pub fn is_empty(&self) -> bool {
        !self.tools && !self.images && !self.video
    }
}

/// 排序方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortDir {
    Asc,
    Desc,
}

/// 账号排序规则（**AIGX 口径**，数据源见 `account_session` 的
/// `created_at` / `expires_at` / `last_balance` / `limiters`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "on", rename_all = "snake_case")]
pub enum AccountSortRule {
    /// 按余额（`last_balance`；形态随 provider 而异，无法归一者不参与排序）
    Balance { dir: SortDir },
    /// 按加入时间（`created_at`）
    Added { dir: SortDir },
    /// 按凭证到期（`expires_at`）
    Expires { dir: SortDir },
    /// 按配额重置时刻（limiter 的 reset）
    QuotaReset { dir: SortDir },
}

/// 单层档案。**每个字段都是 `Option`**（三态，见模块头注释第 2 条）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RoutingProfile {
    #[serde(default)]
    pub scope: Option<RoutingScope>,
    #[serde(default)]
    pub strategy: Option<RouteStrategy>,
    #[serde(default)]
    pub sticky: Option<StickyMode>,
    #[serde(default)]
    pub needs: Option<RequestNeeds>,
    /// 上限 [`caps::MAX_SORT_RULES`] 条；超限拒绝写入
    #[serde(default)]
    pub sort_rules: Option<Vec<AccountSortRule>>,
    /// 该层指定的默认路由组 id（§6.1 优先级链第 3 环）
    #[serde(default)]
    pub default_group: Option<String>,
}

/// 字段 → 生效层。用枚举而非 String：热路径零分配。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Layer {
    Global,
    Model,
    Group,
}

/// 逐字段来源，供管理面 / 回放显示（长度 ≤ 字段数 × 层数，静态枚举，零 String）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileProvenance {
    pub scope: Option<Layer>,
    pub strategy: Option<Layer>,
    pub sticky: Option<Layer>,
    pub needs: Option<Layer>,
    pub sort_rules: Option<Layer>,
    pub default_group: Option<Layer>,
}

impl ProfileProvenance {
    /// 本次实际参与覆盖的层（界面显示「规则命中在哪一层」用）。
    ///
    /// 只在管理面/预演调用（热路径不调），故此处允许一次小 `Vec` 分配。
    pub fn applied_layers(&self) -> Vec<Layer> {
        let mut out = Vec::with_capacity(3);
        for l in [
            self.scope,
            self.strategy,
            self.sticky,
            self.needs,
            self.sort_rules,
            self.default_group,
        ]
        .into_iter()
        .flatten()
        {
            if !out.contains(&l) {
                out.push(l);
            }
        }
        out
    }
}

/// 解析结果：逐字段生效值 + provenance。
///
/// 为什么要 provenance：分层覆盖一旦上线，「用户改了 global 的策略但某模型
/// 没生效」这类问题只能靠「这个字段来自哪一层」解释；没有它界面上只能显示
/// 结果值，管理员会反复怀疑配置没保存。
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedProfile {
    pub scope: RoutingScope,
    pub strategy: RouteStrategy,
    pub sticky: StickyMode,
    pub needs: RequestNeeds,
    pub sort_rules: Vec<AccountSortRule>,
    pub default_group: Option<String>,
    pub provenance: ProfileProvenance,
}

/// 路由组成员（映射 A2A `config/routing_groups.rs`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoutingMember {
    pub provider: String,
    pub model: String,
    #[serde(default = "default_true")]
    pub included: bool,
    /// 上限 [`caps::MAX_ACCOUNTS`]
    #[serde(default)]
    pub accounts: Vec<String>,
}

fn default_true() -> bool {
    true
}

/// 路由组（AIGX 用 `id` 查找，故容器用 `Vec`，不照搬 A2A 的 `HashMap<id,_>`）。
///
/// **`strategy`/`sticky` 是 `Option`（三态），与 [`RoutingProfile`] 同一套语义**
/// （`None`=继承上层 / `Some(值)`=显式设置）。组最常见的用法是「只改参与名单、
/// 不改策略」——若沿用非 Option，「组不设 strategy」会回落 `Weighted` **覆盖掉**
/// 上层的 `Ranked`（静默回退，管理员无感知）。单测
/// `group_optional_fields_inherit_upper_layers` 钉住。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoutingGroup {
    pub id: String,
    pub name: String,
    /// 该组服务的模型（A2A 的组是 per-model 的）
    pub model: String,
    #[serde(default)]
    pub strategy: Option<RouteStrategy>,
    #[serde(default)]
    pub sticky: Option<StickyMode>,
    /// 上限 [`caps::MAX_MEMBERS`]
    #[serde(default)]
    pub members: Vec<RoutingMember>,
}

/// 三层容器：global / 模型级 / 路由组。
///
/// `models` 用 `BTreeMap` 而非 `HashMap`：序列化稳定（整份配置会写进 FileStore
/// KV 与磁盘备份），且解析是按名精确查找。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RoutingProfiles {
    #[serde(default)]
    pub global: RoutingProfile,
    /// 模型级档案；上限 [`caps::MAX_MODELS`]
    #[serde(default)]
    pub models: std::collections::BTreeMap<String, RoutingProfile>,
    /// 上限 [`caps::MAX_GROUPS`]
    #[serde(default)]
    pub groups: Vec<RoutingGroup>,
}

impl RoutingProfiles {
    /// 上限校验（写入前调用；超限**拒绝写入**，不静默截断）。
    pub fn validate(&self) -> Result<(), String> {
        if self.models.len() > caps::MAX_MODELS {
            return Err(format!(
                "模型档案数 {} 超上限 {}",
                self.models.len(),
                caps::MAX_MODELS
            ));
        }
        if self.groups.len() > caps::MAX_GROUPS {
            return Err(format!(
                "路由组数 {} 超上限 {}",
                self.groups.len(),
                caps::MAX_GROUPS
            ));
        }
        check_profile("global", &self.global)?;
        for (name, p) in &self.models {
            check_profile(name, p)?;
        }
        for g in &self.groups {
            if g.members.len() > caps::MAX_MEMBERS {
                return Err(format!(
                    "路由组 `{}` 成员数 {} 超上限 {}",
                    g.id,
                    g.members.len(),
                    caps::MAX_MEMBERS
                ));
            }
            for m in &g.members {
                if m.accounts.len() > caps::MAX_ACCOUNTS {
                    return Err(format!(
                        "路由组 `{}` 成员 `{}/{}` 账号数 {} 超上限 {}",
                        g.id,
                        m.provider,
                        m.model,
                        m.accounts.len(),
                        caps::MAX_ACCOUNTS
                    ));
                }
            }
        }
        Ok(())
    }

    /// 按 id 找组（组是 per-model 的：`model` 非空且不匹配则该组不适用）。
    pub fn find_group(&self, id: &str, requested_model: &str) -> Option<&RoutingGroup> {
        self.groups
            .iter()
            .find(|g| g.id == id && (g.model.is_empty() || g.model == requested_model))
    }

    /// 按 id 取组（单组 CRUD 读侧；不过 per-model 判定 —— 管理面要看原样）。
    pub fn group(&self, id: &str) -> Option<&RoutingGroup> {
        self.groups.iter().find(|g| g.id == id)
    }

    /// 单组写入（upsert：同 id 替换，否则追加）。
    ///
    /// 上限校验（组数 / 成员 / 账号）交由调用方跑 [`Self::validate`] ——
    /// 校验必须看**整份**配置（组数上限是容器属性），不在单组方法里重复。
    pub fn upsert_group(&mut self, group: RoutingGroup) {
        match self.groups.iter_mut().find(|g| g.id == group.id) {
            Some(slot) => *slot = group,
            None => self.groups.push(group),
        }
    }

    /// 删除组：返回是否真的删了；同时把各档案层指向该组的
    /// `default_group` 引用清成 `None` —— 不留悬空引用（否则解析时静默
    /// 忽略，界面看不出「配置的组已经不存在」）。
    pub fn remove_group(&mut self, id: &str) -> bool {
        let before = self.groups.len();
        self.groups.retain(|g| g.id != id);
        if self.groups.len() == before {
            return false;
        }
        if self.global.default_group.as_deref() == Some(id) {
            self.global.default_group = None;
        }
        for p in self.models.values_mut() {
            if p.default_group.as_deref() == Some(id) {
                p.default_group = None;
            }
        }
        true
    }

    /// 各档案层 `default_group` 指向 `id` 的引用数（管理面删除确认回执用）。
    pub fn default_group_refs(&self, id: &str) -> usize {
        usize::from(self.global.default_group.as_deref() == Some(id))
            + self
                .models
                .values()
                .filter(|p| p.default_group.as_deref() == Some(id))
                .count()
    }
}

/// 单层档案的上限校验。
fn check_profile(name: &str, p: &RoutingProfile) -> Result<(), String> {
    if let Some(s) = &p.scope {
        for (field, len, cap) in [
            ("providers", s.providers.len(), caps::MAX_PROVIDERS),
            (
                "disabled_providers",
                s.disabled_providers.len(),
                caps::MAX_PROVIDERS,
            ),
            ("account_order", s.account_order.len(), caps::MAX_ACCOUNTS),
            (
                "excluded_accounts",
                s.excluded_accounts.len(),
                caps::MAX_ACCOUNTS,
            ),
        ] {
            if len > cap {
                return Err(format!("档案 `{name}` 的 {field} 数 {len} 超上限 {cap}"));
            }
        }
    }
    if let Some(r) = &p.sort_rules {
        if r.len() > caps::MAX_SORT_RULES {
            return Err(format!(
                "档案 `{name}` 的排序规则 {} 条超上限 {}",
                r.len(),
                caps::MAX_SORT_RULES
            ));
        }
    }
    Ok(())
}

/// 路由策略整份配置。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RoutingPolicy {
    /// 选路策略
    #[serde(default)]
    pub strategy: RouteStrategy,
    /// 会话粘性档位
    #[serde(default)]
    pub sticky: StickyMode,
    /// 断路器参数
    #[serde(default)]
    pub breaker: BreakerPolicy,
    /// 多因子调度权重（strategy=ranked 时生效；健康/成本/RPM）
    #[serde(default)]
    pub scheduler_weights: SchedulerPolicyConfig,
    /// 多规则档案总开关。**默认 false**：所有新代码路径短路 = 与历史逐字节
    /// 等价（老 KV 数据无该字段 → 反序列化即 false，零迁移）。
    #[serde(default)]
    pub enabled: bool,
    /// 多规则档案（global / 模型级 / 路由组）
    #[serde(default)]
    pub profiles: RoutingProfiles,
}

/// 路由策略运行时持有者：进程级快照，保存即生效。
///
/// 数据面每请求 `snapshot()`（Arc clone），读写均无锁竞争。
///
/// **「档案是否启用」只读 `policy.enabled`（快照字段）**，不另设进程级
/// 静态镜像：`RoutingPolicy.enabled` 运行期可热更（[`Self::update`]），
/// 静态镜像一旦与快照脱节就是两份真相（`config::assistant_tools_enabled`
/// 那种模式只适用于「启动时定格、不变」的配置）。
pub struct RoutingPolicyStore {
    current: parking_lot::RwLock<Arc<RoutingPolicy>>,
    store: Option<Arc<FileStore>>,
}

impl RoutingPolicyStore {
    /// 无持久化构建（测试用；配置不落盘）。
    pub fn in_memory(policy: RoutingPolicy) -> Self {
        Self {
            current: parking_lot::RwLock::new(Arc::new(policy)),
            store: None,
        }
    }

    /// 带持久化构建（启动时调用，自动加载已存配置）。
    pub fn with_store(store: Arc<FileStore>) -> Self {
        let policy = store
            .get::<RoutingPolicy>(ROUTING_POLICY_KEY)
            .ok()
            .flatten()
            .unwrap_or_default();
        Self {
            current: parking_lot::RwLock::new(Arc::new(policy)),
            store: Some(store),
        }
    }

    /// 当前生效配置快照（每请求调用，Arc clone 廉价）。
    pub fn snapshot(&self) -> Arc<RoutingPolicy> {
        self.current.read().clone()
    }

    /// 更新并持久化（返回生效值）。
    ///
    /// 写入前做档案上限校验（契约 §1/§3）：**超限拒绝写入**（返回 Err），
    /// 不静默截断 —— 静默截断会让界面显示的配置与实际生效的不一致。
    pub fn update(&self, policy: RoutingPolicy) -> anyhow::Result<RoutingPolicy> {
        policy
            .profiles
            .validate()
            .map_err(|e| anyhow::anyhow!("路由档案超限：{e}"))?;
        if let Some(store) = &self.store {
            store.put(ROUTING_POLICY_KEY, &policy)?;
        }
        *self.current.write() = Arc::new(policy.clone());
        Ok(policy)
    }
}

// ── 分层解析（契约 §2.2）──────────────────────────────────────────────

/// 解析某请求名当前生效的档案：**逐字段三态覆盖**，基座是 `RoutingPolicy`
/// 自身（`strategy` / `sticky` 取策略现值，新字段取内置默认）。
///
/// 层序（自左向右，最右侧「字段不是 None」的层生效）：
/// `global` → `profiles.models[<档案键>]` → `profiles.groups[<生效组 id>]`
///
/// 档案键（契约 §2.3）：`profiles.models` 命中请求名优先；未命中则用
/// **全局映射名**再查一次（同 `pricing::resolve_priced_name` 的范式）。
///
/// `enabled=false` 时直接返回基座（不动档案、零额外分配）= 与历史逐字节等价。
pub fn resolve_profile(
    policy: &RoutingPolicy,
    requested_model: &str,
    global_mapper: &crate::model::ModelMapper,
    group_id: Option<&str>,
) -> ResolvedProfile {
    // 基座 = 策略现值（不是「内置默认」：否则启用档案会静默重置管理员配的
    // strategy/sticky —— 那正是契约要避免的「规则漂移」）
    let mut out = ResolvedProfile {
        scope: RoutingScope::default(),
        strategy: policy.strategy,
        sticky: policy.sticky,
        needs: RequestNeeds::default(),
        sort_rules: Vec::new(),
        default_group: None,
        provenance: ProfileProvenance::default(),
    };
    if !policy.enabled {
        return out;
    }
    let profiles = &policy.profiles;

    // 档案键查找：请求名优先 → 全局映射名兜底（借用查找，不为此分配 String）
    let mut model_layer = profiles.models.get(requested_model);
    if model_layer.is_none() && !profiles.models.is_empty() {
        model_layer = global_mapper.resolve_with(requested_model, |mapped| {
            if mapped == requested_model {
                None
            } else {
                profiles.models.get(mapped)
            }
        });
    }

    // 参与解析的档案层（有序小数组，最多 2 层：global + 模型级）
    let layers: [Option<(Layer, &RoutingProfile)>; 2] = [
        Some((Layer::Global, &profiles.global)),
        model_layer.map(|p| (Layer::Model, p)),
    ];

    // 生效组 id（契约 §6.1）：显式 group_id（第 1/2 环）优先；否则自左向右扫，
    // 最后一个提供了 `default_group` 的档案层胜出（第 3 环；与逐字段合并同一
    // 把尺 ——「最右侧不是 None 的层」生效）
    let mut effective_group = group_id;
    if effective_group.is_none() {
        for (_, p) in layers.iter().flatten() {
            if let Some(g) = p.default_group.as_deref() {
                effective_group = Some(g);
            }
        }
    }
    let group = effective_group.and_then(|gid| profiles.find_group(gid, requested_model));

    // 逐字段三态覆盖：只记「谁提供」，循环外物化（避免逐层 clone）
    let mut scope_ref: Option<(&Layer, &RoutingScope)> = None;
    let mut sort_rules_ref: Option<(&Layer, &[AccountSortRule])> = None;
    let mut default_group_ref: Option<(&Layer, &str)> = None;
    for (layer, p) in layers.iter().flatten() {
        if let Some(v) = p.scope.as_ref() {
            scope_ref = Some((layer, v));
        }
        if let Some(v) = p.strategy {
            out.strategy = v;
            out.provenance.strategy = Some(*layer);
        }
        if let Some(v) = p.sticky {
            out.sticky = v;
            out.provenance.sticky = Some(*layer);
        }
        if let Some(v) = p.needs {
            out.needs = v;
            out.provenance.needs = Some(*layer);
        }
        if let Some(v) = p.sort_rules.as_deref() {
            sort_rules_ref = Some((layer, v));
        }
        if let Some(v) = p.default_group.as_deref() {
            default_group_ref = Some((layer, v));
        }
    }
    if let Some((layer, v)) = scope_ref {
        out.scope = v.clone();
        out.provenance.scope = Some(*layer);
    }
    if let Some((layer, v)) = sort_rules_ref {
        out.sort_rules = v.to_vec();
        out.provenance.sort_rules = Some(*layer);
    }
    if let Some((layer, v)) = default_group_ref {
        out.default_group = Some(v.to_string());
        out.provenance.default_group = Some(*layer);
    }
    // 组层：与档案层同一套三态语义（组字段 `None` = 继承上层，不覆盖）。
    if let Some(g) = group {
        if let Some(v) = g.strategy {
            out.strategy = v;
            out.provenance.strategy = Some(Layer::Group);
        }
        if let Some(v) = g.sticky {
            out.sticky = v;
            out.provenance.sticky = Some(Layer::Group);
        }
    }
    out
}

// ── 判定统一：plan_route（契约 §3）────────────────────────────────────

/// 候选渠道的只读视图（契约 §3 `CandidateRef`）：`plan_route` 只读渠道本体，
/// 不 clone 渠道（含凭证字段），热路径上零 String 分配。
#[derive(Debug, Clone, Copy)]
pub struct CandidateRef<'a> {
    pub channel: &'a crate::channel::Channel,
}

impl<'a> CandidateRef<'a> {
    pub fn id(&self) -> &'a str {
        self.channel.id.as_str()
    }

    /// 提供商口径 = `ChannelType::as_str()`（与 Key 级 `allowed_providers`
    /// 过滤同一命名空间，见 `api/openai/resolve.rs`）。
    pub fn provider(&self) -> &'a str {
        self.channel.channel_type.as_str()
    }
}

/// 参与候选集合：要么原切片直通（零分配），要么裁剪后的下标视图。
#[derive(Debug)]
pub enum PlanCandidates<'a> {
    /// 档案未启用 / 无需裁剪：原切片直通（零分配、零拷贝）
    All(&'a [crate::channel::Channel]),
    /// 按档案 scope 裁剪后的参与候选（仅此时分配一次小 Vec）
    Kept(Vec<CandidateRef<'a>>),
}

impl<'a> PlanCandidates<'a> {
    pub fn iter(&self) -> PlanIter<'a, '_> {
        match self {
            Self::All(s) => PlanIter::All(s.iter()),
            Self::Kept(v) => PlanIter::Kept(v.iter()),
        }
    }

    pub fn len(&self) -> usize {
        match self {
            Self::All(s) => s.len(),
            Self::Kept(v) => v.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// 统一两臂的迭代器（两臂 item 都是 `&Channel`，故可共用一个类型）。
#[derive(Debug)]
pub enum PlanIter<'a, 's> {
    All(std::slice::Iter<'a, crate::channel::Channel>),
    Kept(std::slice::Iter<'s, CandidateRef<'a>>),
}

impl<'a, 's> Iterator for PlanIter<'a, 's> {
    type Item = &'a crate::channel::Channel;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::All(it) => it.next(),
            Self::Kept(it) => it.next().map(|c| c.channel),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        match self {
            Self::All(it) => it.size_hint(),
            Self::Kept(it) => it.size_hint(),
        }
    }
}

/// 被裁掉的原因计数。原因码词汇沿用既有枚举（不新增 `format!`）：
/// `provider_not_allowed`（对照 `api/openai/resolve.rs` 的
/// `REASON_KEY_PROVIDER_NOT_ALLOWED`）、`provider_disabled`。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkipSummary {
    /// 渠道提供商不在档案 `providers` 名单内
    pub provider_not_allowed: usize,
    /// 渠道提供商落在档案 `disabled_providers` 内
    pub provider_disabled: usize,
}

/// 一次选路判定结果（转发与预演共用同一份）。
#[derive(Debug)]
pub struct RoutePlan<'a> {
    /// 参与候选（已按档案 scope 裁剪，保持调用方给的顺序）
    pub candidates: PlanCandidates<'a>,
    /// 裁掉的原因计数
    pub skipped: SkipSummary,
}

impl<'a> RoutePlan<'a> {
    /// 按序产出参与候选的渠道本体（直通时零分配）。
    pub fn iter(&self) -> PlanIter<'a, '_> {
        self.candidates.iter()
    }

    pub fn len(&self) -> usize {
        self.candidates.len()
    }

    pub fn is_empty(&self) -> bool {
        self.candidates.is_empty()
    }

    /// 队首（= 实际首挑预测；预演面板的 `pickedId` 用它，避免 #31 那类
    /// 「回放投影与真实形状不匹配」）。
    pub fn first(&self) -> Option<&'a crate::channel::Channel> {
        self.candidates.iter().next()
    }
}

/// **唯一选路判定入口**：转发（`api/openai/resolve.rs` → `channel/mod.rs`）与
/// 预演（`api/admin/routing.rs::handle_routing_queue`）都必须调它。
///
/// 为什么先 [`resolve_profile`] 再本函数（而不是像契约 §3 草稿那样在
/// `plan_route` 内部一并解析）：候选选取（`select_for_model_by_strategy`）
/// 需要**先知道生效 strategy**，故两个调用点走同一序列
/// 「resolve_profile → 按生效 strategy 选候选 → plan_route 裁剪」；
/// 判定规则只有这一份，预演的队首因此必然等于转发的首挑。
///
/// 性能：`enabled=false` 或无需裁剪时返回原切片直通（**零分配**）；裁剪时
/// **候选循环内零分配**（只做字符串比较，无 `format!`/`to_string`/`Vec` 扩容）。
///
/// 账号级规则（`account_order` / `excluded_accounts` / `sort_rules`）不在本
/// 函数处理：候选是渠道级，账号身份在 `AccountSessionStore::acquire` 才确定
/// （契约 §2.4 的两级「粗筛/精筛」分离），那一级在账号侧统一实现。
pub fn plan_route<'a>(
    policy: &RoutingPolicy,
    profile: &ResolvedProfile,
    candidates: &'a [crate::channel::Channel],
) -> RoutePlan<'a> {
    // 总开关关闭：短路（与历史逐字节等价，零分配）
    if !policy.enabled {
        return RoutePlan {
            candidates: PlanCandidates::All(candidates),
            skipped: SkipSummary::default(),
        };
    }
    let scope = &profile.scope;
    let listed = matches!(scope.provider_scope, ProviderScope::Listed);
    if !listed && scope.disabled_providers.is_empty() {
        // 无裁剪可做：直接直通，不为「没规则」分配
        return RoutePlan {
            candidates: PlanCandidates::All(candidates),
            skipped: SkipSummary::default(),
        };
    }
    let mut kept: Vec<CandidateRef<'a>> = Vec::with_capacity(candidates.len());
    let mut skipped = SkipSummary::default();
    for ch in candidates {
        let provider = ch.channel_type.as_str();
        if scope
            .disabled_providers
            .iter()
            .any(|p| p.as_str() == provider)
        {
            skipped.provider_disabled += 1;
            continue;
        }
        if listed && !scope.providers.iter().any(|p| p.as_str() == provider) {
            skipped.provider_not_allowed += 1;
            continue;
        }
        kept.push(CandidateRef { channel: ch });
    }
    RoutePlan {
        candidates: PlanCandidates::Kept(kept),
        skipped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::Channel;
    use crate::storage::FileStore;
    use std::sync::Arc;
    use tempfile::TempDir;

    /// 一条最小渠道（其余字段全走 serde 默认，避免夹具随 `Channel` 加字段漂移）。
    fn ch(id: &str, provider: &str) -> Channel {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "name": id,
            "channel_type": provider,
        }))
        .unwrap()
    }

    /// 空的全局映射表（档案键兜底查找用）。
    fn mapper(dir: &TempDir) -> crate::model::ModelMapper {
        crate::model::ModelMapper::new(Arc::new(FileStore::new(dir.path().to_path_buf())))
    }

    #[test]
    fn defaults_match_a2a_semantics() {
        let p = RoutingPolicy::default();
        assert_eq!(p.strategy, RouteStrategy::Weighted);
        assert_eq!(p.sticky, StickyMode::Auto);
        // A2A 断路器默认关（routingBreakerEnabled=false），AIGX 对齐
        assert!(!p.breaker.enabled);
        assert_eq!(p.breaker.threshold, 5);
        assert_eq!(p.breaker.cooldown_secs, 30);
    }

    #[test]
    fn sticky_ttls() {
        assert_eq!(
            StickyMode::Auto.ttls(),
            Some((5 * 60, 30 * 60)),
            "auto = 5min 粘性 / 30min 硬过期"
        );
        assert_eq!(StickyMode::Session.ttls(), Some((6 * 3600, 6 * 3600)));
        assert_eq!(StickyMode::Off.ttls(), None);
    }

    #[test]
    fn serde_roundtrip_strategy_names() {
        // 蛇形枚举名 = API 契约（前端 / api 均用 snake_case）
        let json = serde_json::to_value(RouteStrategy::RoundRobin).unwrap();
        assert_eq!(json, serde_json::json!("round_robin"));
        let s: RouteStrategy = serde_json::from_value(json).unwrap();
        assert_eq!(s, RouteStrategy::RoundRobin);
        assert_eq!(
            RouteStrategy::from_str_opt("ranked"),
            Some(RouteStrategy::Ranked)
        );
        assert_eq!(RouteStrategy::from_str_opt("bogus"), None);
    }

    #[test]
    fn policy_roundtrip_defaults() {
        // 旧数据（缺字段）反序列化回默认值
        let v: RoutingPolicy =
            serde_json::from_str(r#"{"strategy":"ranked","sticky":"session"}"#).unwrap();
        assert_eq!(v.strategy, RouteStrategy::Ranked);
        assert_eq!(v.sticky, StickyMode::Session);
        assert_eq!(v.breaker, BreakerPolicy::default());
        assert_eq!(v.scheduler_weights, SchedulerPolicyConfig::default());
    }

    #[test]
    fn store_update_hot_swap() {
        let store = RoutingPolicyStore::in_memory(RoutingPolicy::default());
        let mut p = store.snapshot();
        assert_eq!(p.strategy, RouteStrategy::Weighted);
        let next = RoutingPolicy {
            strategy: RouteStrategy::RoundRobin,
            ..RoutingPolicy::default()
        };
        store.update(next).unwrap();
        p = store.snapshot();
        assert_eq!(p.strategy, RouteStrategy::RoundRobin);
    }

    #[test]
    fn turn_mode_is_non_sticky() {
        // turn：不粘（TTL None）、sticky_enabled false；序列化为 "turn"
        assert_eq!(StickyMode::Turn.ttls(), None);
        assert!(!StickyMode::Turn.sticky_enabled());
        assert_eq!(StickyMode::from_str_opt("turn"), Some(StickyMode::Turn));
        let json = serde_json::to_value(StickyMode::Turn).unwrap();
        assert_eq!(json, serde_json::json!("turn"));
        // 旧档位不受影响
        assert!(StickyMode::Auto.sticky_enabled());
        assert!(StickyMode::Session.sticky_enabled());
        assert!(!StickyMode::Off.sticky_enabled());
    }

    #[test]
    fn breaker_policy_success_threshold_default() {
        // A2A 对齐：默认关 / 阈值 5 / 冷却 30s / 半开成功 2 次
        let d = BreakerPolicy::default();
        assert!(!d.enabled);
        assert_eq!(d.threshold, 5);
        assert_eq!(d.cooldown_secs, 30);
        assert_eq!(d.success_threshold, 2);
        // 旧数据（无 success_threshold 字段）回落默认 2
        let v: BreakerPolicy = serde_json::from_str(r#"{"enabled":true}"#).unwrap();
        assert!(v.enabled);
        assert_eq!(v.success_threshold, 2);
    }

    #[test]
    fn sticky_enabled_from_policy() {
        let mut p = RoutingPolicy::default();
        assert!(StickyMode::sticky_enabled_from(&p));
        p.sticky = StickyMode::Turn;
        assert!(!StickyMode::sticky_enabled_from(&p));
        p.sticky = StickyMode::Off;
        assert!(!StickyMode::sticky_enabled_from(&p));
        p.sticky = StickyMode::Session;
        assert!(StickyMode::sticky_enabled_from(&p));
    }

    // ── #40 路由多规则体系 ─────────────────────────────────────────────

    /// 门槛用例（契约 §0）：**总开关关闭时与历史逐字节等价** —— 策略基座
    /// 原样返回、档案一律不参与、候选原样直通（预演队首因此与历史一致）。
    #[test]
    fn profiles_disabled_is_byte_identical_to_legacy() {
        let dir = TempDir::new().unwrap();
        let m = mapper(&dir);
        let mut policy = RoutingPolicy {
            strategy: RouteStrategy::RoundRobin,
            sticky: StickyMode::Off,
            ..RoutingPolicy::default()
        };
        assert!(!policy.enabled, "契约 §0：默认必须是关");
        // 塞一份「看起来会生效」的档案
        policy.profiles.global.strategy = Some(RouteStrategy::Ranked);
        policy.profiles.global.sticky = Some(StickyMode::Session);
        policy.profiles.global.scope = Some(RoutingScope {
            provider_scope: ProviderScope::Listed,
            providers: vec!["anthropic".to_string()],
            ..RoutingScope::default()
        });
        policy.profiles.models.insert(
            "gpt-4o".to_string(),
            RoutingProfile {
                strategy: Some(RouteStrategy::Weighted),
                ..RoutingProfile::default()
            },
        );

        let r = resolve_profile(&policy, "gpt-4o", &m, None);
        assert_eq!(r.strategy, RouteStrategy::RoundRobin, "关闭时不动策略");
        assert_eq!(r.sticky, StickyMode::Off);
        assert!(r.scope.providers.is_empty(), "关闭时不用档案名单");
        assert_eq!(r.provenance, ProfileProvenance::default());
        assert_eq!(r.provenance.applied_layers(), Vec::new());

        let cands = vec![ch("a", "anthropic"), ch("b", "openai_compatible")];
        let plan = plan_route(&policy, &r, &cands);
        assert!(matches!(plan.candidates, PlanCandidates::All(_)));
        assert_eq!(plan.len(), 2, "关闭时零裁剪");
        assert_eq!(plan.skipped, SkipSummary::default());
        assert_eq!(plan.first().unwrap().id, "a", "顺序与输入一致");
        assert!(!plan.is_empty());
    }

    /// 三态覆盖 + provenance：`None` 继承下层、`Some(值)` 覆盖。
    #[test]
    fn profile_tri_state_override_and_provenance() {
        let dir = TempDir::new().unwrap();
        let m = mapper(&dir);
        let mut policy = RoutingPolicy {
            enabled: true,
            ..RoutingPolicy::default()
        };
        policy.profiles.global.strategy = Some(RouteStrategy::Ranked);
        policy.profiles.global.needs = Some(RequestNeeds {
            images: true,
            ..RequestNeeds::default()
        });
        policy.profiles.models.insert(
            "gpt-4o".to_string(),
            RoutingProfile {
                sticky: Some(StickyMode::Session),
                ..RoutingProfile::default()
            },
        );

        let r = resolve_profile(&policy, "gpt-4o", &m, None);
        assert_eq!(
            r.strategy,
            RouteStrategy::Ranked,
            "模型级未设 → 继承 global"
        );
        assert_eq!(r.provenance.strategy, Some(Layer::Global));
        assert_eq!(r.sticky, StickyMode::Session, "模型级覆盖");
        assert_eq!(r.provenance.sticky, Some(Layer::Model));
        assert!(r.needs.images);
        assert_eq!(r.provenance.needs, Some(Layer::Global));
        assert_eq!(r.provenance.scope, None);
        assert_eq!(r.provenance.sort_rules, None);
        assert_eq!(
            r.provenance.applied_layers(),
            vec![Layer::Global, Layer::Model]
        );

        // 未命中档案的模型：只剩 global 一层；sticky 落策略基座
        let r2 = resolve_profile(&policy, "claude-3-5", &m, None);
        assert_eq!(r2.strategy, RouteStrategy::Ranked);
        assert_eq!(r2.provenance.strategy, Some(Layer::Global));
        assert_eq!(r2.sticky, RoutingPolicy::default().sticky);
        assert_eq!(r2.provenance.sticky, None);
    }

    /// `Some(空集合)` = **显式清空**（模型级「不用 global 的参与名单」）。
    #[test]
    fn profile_explicit_clear_overrides_inherited() {
        let dir = TempDir::new().unwrap();
        let m = mapper(&dir);
        let mut policy = RoutingPolicy {
            enabled: true,
            ..RoutingPolicy::default()
        };
        policy.profiles.global.scope = Some(RoutingScope {
            providers: vec!["anthropic".to_string()],
            excluded_accounts: vec!["acct-1".to_string()],
            ..RoutingScope::default()
        });
        policy.profiles.models.insert(
            "gpt-4o".to_string(),
            RoutingProfile {
                scope: Some(RoutingScope::default()),
                ..RoutingProfile::default()
            },
        );

        let r = resolve_profile(&policy, "gpt-4o", &m, None);
        assert!(
            r.scope.providers.is_empty(),
            "显式空 scope 覆盖 global 名单"
        );
        assert!(r.scope.excluded_accounts.is_empty());
        assert_eq!(r.provenance.scope, Some(Layer::Model));

        let r2 = resolve_profile(&policy, "other", &m, None);
        assert_eq!(r2.scope.providers, vec!["anthropic".to_string()]);
        assert_eq!(r2.provenance.scope, Some(Layer::Global));
    }

    /// 档案键（契约 §2.3）：请求名优先；未命中用**全局映射名**兜底；
    /// 被关掉的别名与 `ModelMapper::resolve` 同口径（不改名）。
    #[test]
    fn profile_lookup_falls_back_to_global_mapping() {
        let dir = TempDir::new().unwrap();
        let m = mapper(&dir);
        m.set_custom("gpt-4o".to_string(), "gpt-4o-2024-08-06".to_string())
            .unwrap();
        let mut policy = RoutingPolicy {
            enabled: true,
            ..RoutingPolicy::default()
        };
        policy.profiles.models.insert(
            "gpt-4o-2024-08-06".to_string(),
            RoutingProfile {
                strategy: Some(RouteStrategy::Ranked),
                ..RoutingProfile::default()
            },
        );

        let r = resolve_profile(&policy, "gpt-4o", &m, None);
        assert_eq!(
            r.strategy,
            RouteStrategy::Ranked,
            "别名兜底命中上游真名的档案"
        );
        assert_eq!(r.provenance.strategy, Some(Layer::Model));

        let r2 = resolve_profile(&policy, "gpt-3.5", &m, None);
        assert_eq!(r2.provenance.strategy, None, "无映射 → 只有 global");

        assert!(m.set_enabled("gpt-4o", false).unwrap(), "别名存在才可切换");
        let r3 = resolve_profile(&policy, "gpt-4o", &m, None);
        assert_eq!(r3.provenance.strategy, None, "关掉的别名不参与改名");
    }

    /// 组层（契约 §6.1 第 3 环）：`default_group` 命中的组提供
    /// strategy/sticky（provenance=Group）；组是 per-model 的。
    #[test]
    fn profile_group_layer_and_per_model_scope() {
        let dir = TempDir::new().unwrap();
        let m = mapper(&dir);
        let mut policy = RoutingPolicy {
            enabled: true,
            ..RoutingPolicy::default()
        };
        policy.profiles.global.strategy = Some(RouteStrategy::Ranked);
        policy.profiles.models.insert(
            "gpt-4o".to_string(),
            RoutingProfile {
                default_group: Some("g1".to_string()),
                ..RoutingProfile::default()
            },
        );
        policy.profiles.groups.push(RoutingGroup {
            id: "g1".to_string(),
            name: "组一".to_string(),
            model: "gpt-4o".to_string(),
            strategy: Some(RouteStrategy::RoundRobin),
            sticky: Some(StickyMode::Session),
            members: Vec::new(),
        });

        let r = resolve_profile(&policy, "gpt-4o", &m, None);
        assert_eq!(
            r.strategy,
            RouteStrategy::RoundRobin,
            "组层覆盖模型层/global"
        );
        assert_eq!(r.provenance.strategy, Some(Layer::Group));
        assert_eq!(r.sticky, StickyMode::Session);
        assert_eq!(r.provenance.sticky, Some(Layer::Group));
        assert_eq!(r.default_group.as_deref(), Some("g1"));
        assert_eq!(r.provenance.default_group, Some(Layer::Model));

        // 别的模型：组是 per-model 的 → 不套用
        let r2 = resolve_profile(&policy, "other", &m, None);
        assert_eq!(r2.provenance.strategy, Some(Layer::Global));
        // 显式指定的组同样过 per-model 判定
        let r3 = resolve_profile(&policy, "other", &m, Some("g1"));
        assert_eq!(r3.provenance.strategy, Some(Layer::Global));
        // 显式指定的组对匹配模型生效
        let r4 = resolve_profile(&policy, "gpt-4o", &m, Some("g1"));
        assert_eq!(r4.provenance.strategy, Some(Layer::Group));
    }

    /// 组的 `strategy`/`sticky` 是三态（与 RoutingProfile 同套语义）：
    /// **组不设 strategy 时生效 strategy 仍为上层的值** —— 若按 A2A 的非
    /// Option 形态，这里会回落 Weighted 静默覆盖掉上层的 Ranked。
    #[test]
    fn group_optional_fields_inherit_upper_layers() {
        let dir = TempDir::new().unwrap();
        let m = mapper(&dir);
        let mut policy = RoutingPolicy {
            strategy: RouteStrategy::Ranked,
            sticky: StickyMode::Off,
            enabled: true,
            ..RoutingPolicy::default()
        };
        policy.profiles.groups.push(RoutingGroup {
            id: "g1".to_string(),
            name: "只改名单".to_string(),
            model: String::new(),
            strategy: None,
            sticky: None,
            members: vec![RoutingMember {
                provider: "anthropic".to_string(),
                model: "gpt-4o".to_string(),
                included: true,
                accounts: Vec::new(),
            }],
        });

        let r = resolve_profile(&policy, "gpt-4o", &m, Some("g1"));
        assert_eq!(
            r.strategy,
            RouteStrategy::Ranked,
            "组未设 strategy → 继承策略基座"
        );
        assert_eq!(r.provenance.strategy, None, "组未提供 → 不记 Group 来源");
        assert_eq!(r.sticky, StickyMode::Off);
        assert_eq!(r.provenance.sticky, None);

        // 组只显式设其中一个字段：另一个仍继承
        policy.profiles.groups[0].sticky = Some(StickyMode::Session);
        let r2 = resolve_profile(&policy, "gpt-4o", &m, Some("g1"));
        assert_eq!(r2.strategy, RouteStrategy::Ranked);
        assert_eq!(r2.provenance.strategy, None);
        assert_eq!(r2.sticky, StickyMode::Session);
        assert_eq!(r2.provenance.sticky, Some(Layer::Group));
    }

    /// 单组 CRUD：upsert 同 id 替换不重复；删除连带清 `default_group`
    /// 悬空引用（global 与模型级两层都要清）。
    #[test]
    fn group_crud_upsert_and_remove_clears_refs() {
        let mut profiles = RoutingProfiles::default();
        let mk = |id: &str, name: &str| RoutingGroup {
            id: id.to_string(),
            name: name.to_string(),
            model: String::new(),
            strategy: None,
            sticky: None,
            members: Vec::new(),
        };

        profiles.upsert_group(mk("g1", "一"));
        profiles.upsert_group(mk("g2", "二"));
        assert_eq!(profiles.groups.len(), 2);
        // 同 id 再写 = 替换（不产生第二条）
        profiles.upsert_group(mk("g1", "一改"));
        assert_eq!(profiles.groups.len(), 2);
        assert_eq!(profiles.group("g1").unwrap().name, "一改");
        assert!(profiles.group("missing").is_none());

        // 档案层引用 g1 后删除：引用被清、g2 不受影响
        profiles.global.default_group = Some("g1".to_string());
        profiles.models.insert(
            "gpt-4o".to_string(),
            RoutingProfile {
                default_group: Some("g1".to_string()),
                ..RoutingProfile::default()
            },
        );
        profiles.models.insert(
            "other".to_string(),
            RoutingProfile {
                default_group: Some("g2".to_string()),
                ..RoutingProfile::default()
            },
        );
        assert_eq!(profiles.default_group_refs("g1"), 2);
        assert!(profiles.remove_group("g1"));
        assert!(!profiles.remove_group("g1"), "重复删除返回 false");
        assert_eq!(profiles.global.default_group, None);
        assert_eq!(profiles.models["gpt-4o"].default_group, None);
        assert_eq!(
            profiles.models["other"].default_group.as_deref(),
            Some("g2")
        );
    }

    /// `plan_route`：scope 裁剪（白名单 / 排除名单）+ 直通路径 + 计数。
    #[test]
    fn plan_route_filters_by_provider_scope() {
        let dir = TempDir::new().unwrap();
        let m = mapper(&dir);
        let cands = vec![
            ch("a", "anthropic"),
            ch("b", "openai_compatible"),
            ch("c", "anthropic"),
        ];
        let mut policy = RoutingPolicy {
            enabled: true,
            ..RoutingPolicy::default()
        };

        // 1) 未配 scope：直通（零分配路径）
        let r = resolve_profile(&policy, "m", &m, None);
        let p = plan_route(&policy, &r, &cands);
        assert!(matches!(p.candidates, PlanCandidates::All(_)));
        assert_eq!(p.len(), 3);

        // 2) Listed 白名单
        policy.profiles.global.scope = Some(RoutingScope {
            provider_scope: ProviderScope::Listed,
            providers: vec!["anthropic".to_string()],
            ..RoutingScope::default()
        });
        let r = resolve_profile(&policy, "m", &m, None);
        let p = plan_route(&policy, &r, &cands);
        assert_eq!(
            p.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            vec!["a", "c"]
        );
        assert_eq!(p.skipped.provider_not_allowed, 1);
        assert_eq!(p.skipped.provider_disabled, 0);
        assert_eq!(p.first().unwrap().id, "a");

        // 3) All 口径下的排除名单
        policy.profiles.global.scope = Some(RoutingScope {
            disabled_providers: vec!["openai_compatible".to_string()],
            ..RoutingScope::default()
        });
        let r = resolve_profile(&policy, "m", &m, None);
        let p = plan_route(&policy, &r, &cands);
        assert_eq!(p.len(), 2);
        assert_eq!(p.skipped.provider_disabled, 1);

        // 4) 关闭总开关：即便名单还在也直通（= 历史）
        policy.enabled = false;
        let r = resolve_profile(&policy, "m", &m, None);
        let p = plan_route(&policy, &r, &cands);
        assert!(matches!(p.candidates, PlanCandidates::All(_)));
        assert_eq!(p.len(), 3);
    }

    /// 上限校验（契约 §1/§3）：**超限拒绝写入**，不静默截断。
    #[test]
    fn caps_reject_over_limit_writes() {
        let mut profiles = RoutingProfiles::default();
        assert!(profiles.validate().is_ok());

        // 排序规则：2 条 ok / 3 条拒
        profiles.global.sort_rules = Some(vec![
            AccountSortRule::Balance { dir: SortDir::Asc },
            AccountSortRule::Added { dir: SortDir::Desc },
        ]);
        assert!(profiles.validate().is_ok());
        profiles.global.sort_rules = Some(vec![
            AccountSortRule::Balance { dir: SortDir::Asc },
            AccountSortRule::Added { dir: SortDir::Desc },
            AccountSortRule::Expires { dir: SortDir::Asc },
        ]);
        let err = profiles.validate().unwrap_err();
        assert!(err.contains("排序规则"), "{err}");
        profiles.global.sort_rules = None;

        // providers 超上限
        profiles.global.scope = Some(RoutingScope {
            providers: (0..=caps::MAX_PROVIDERS).map(|i| format!("p{i}")).collect(),
            ..RoutingScope::default()
        });
        assert!(profiles.validate().is_err());
        profiles.global.scope = None;

        // 组数超上限
        profiles.groups = (0..=caps::MAX_GROUPS)
            .map(|i| RoutingGroup {
                id: format!("g{i}"),
                name: String::new(),
                model: String::new(),
                strategy: None,
                sticky: None,
                members: Vec::new(),
            })
            .collect();
        assert!(profiles.validate().is_err());
        // 单组成员数超上限
        profiles.groups = vec![RoutingGroup {
            id: "g".to_string(),
            name: String::new(),
            model: String::new(),
            strategy: None,
            sticky: None,
            members: (0..=caps::MAX_MEMBERS)
                .map(|i| RoutingMember {
                    provider: "p".to_string(),
                    model: format!("m{i}"),
                    included: true,
                    accounts: Vec::new(),
                })
                .collect(),
        }];
        assert!(profiles.validate().is_err());

        // 单写入路径（store.update）必须同样拦住
        let store = RoutingPolicyStore::in_memory(RoutingPolicy::default());
        let bad = RoutingPolicy {
            enabled: true,
            profiles,
            ..RoutingPolicy::default()
        };
        assert!(store.update(bad).is_err());
        assert_eq!(
            store.snapshot().profiles.groups.len(),
            0,
            "拒绝写入后现值不变"
        );
    }

    /// 老 KV 数据（无 `enabled` / `profiles`）零迁移；三态序列化往返不丢语义。
    #[test]
    fn legacy_kv_without_profiles_loads_disabled() {
        let v: RoutingPolicy = serde_json::from_str(r#"{"strategy":"ranked"}"#).unwrap();
        assert!(!v.enabled);
        assert_eq!(v.profiles, RoutingProfiles::default());
        assert!(v.profiles.models.is_empty());

        // None = 继承（缺失或 null 同义）；空对象 = Some(全默认) = 显式清空
        let p: RoutingProfile = serde_json::from_value(
            serde_json::json!({"scope": {}, "strategy": null, "sticky": "session"}),
        )
        .unwrap();
        assert_eq!(p.strategy, None);
        assert_eq!(p.sticky, Some(StickyMode::Session));
        assert_eq!(p.scope, Some(RoutingScope::default()));
        let j = serde_json::to_value(&p).unwrap();
        assert_eq!(j["sticky"], serde_json::json!("session"));
        assert!(j["strategy"].is_null());
        let back: RoutingProfile = serde_json::from_value(j).unwrap();
        assert_eq!(back, p);

        let empty: RoutingProfile = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(empty, RoutingProfile::default());
    }

    /// `needs` 标签映射只有一处（`required_tags`），别处不得再写一遍。
    #[test]
    fn request_needs_tags() {
        let n = RequestNeeds {
            tools: true,
            images: true,
            video: false,
        };
        assert_eq!(
            n.required_tags().collect::<Vec<_>>(),
            vec!["tool", "vision"]
        );
        assert!(!n.is_empty());
        let all = RequestNeeds {
            tools: true,
            images: true,
            video: true,
        };
        assert_eq!(all.required_tags().count(), 3);
        assert!(RequestNeeds::default().is_empty());
        assert_eq!(RequestNeeds::default().required_tags().count(), 0);
    }

    /// `enabled` 的唯一真相是快照字段：store 热更后 `snapshot().enabled` 翻转
    /// （不设进程级静态镜像 —— 那是与快照漂移的第二份真相）。
    #[test]
    fn enabled_flag_reads_policy_snapshot() {
        let store = RoutingPolicyStore::in_memory(RoutingPolicy::default());
        assert!(!store.snapshot().enabled, "默认关");
        store
            .update(RoutingPolicy {
                enabled: true,
                ..RoutingPolicy::default()
            })
            .unwrap();
        assert!(store.snapshot().enabled);
        store.update(RoutingPolicy::default()).unwrap();
        assert!(!store.snapshot().enabled);
    }
}
