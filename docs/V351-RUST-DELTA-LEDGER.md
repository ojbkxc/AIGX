# v3.5.0 → v3.5.1 全量 Rust 增量台账（T1）

> 上游：`D:\GitHub\Agent2API` tag **v3.5.0**（`cc5d8b9b`）→ tag **v3.5.1**（`33f5e797`，
> 发布 2026-11-23T17:46Z，`prerelease=false`）。
> 区间内只有 3 个提交：`811a4f1d`（就绪闸门 1ms 边界）、`b9c8fa05`（合入 3.5.1 待发布代码）、
> `33f5e797`（版本号 + 交互/多语言产物）。
> AIGX：基线 **v1.0.95**（`4269a05f`）。
>
> 本文是 **T1 交付物**：把 `git diff v3.5.0 v3.5.1 -- '*.rs'` 的 **63 个 Rust 文件
> （+2462 / −317）逐文件落成台账**，给出「上游改了什么 / AIGX 现在是什么样 / 归谁处置」。
> T2–T5 的处置结论以各 owner 的落地为准，本文按**工作区真实状态**汇总（见下方快照）。
>
> 本文取代 [`V351-ALIGN.md`](V351-ALIGN.md) 中「已对齐 3 项 / 待办无阻塞项」的旧口径 ——
> 旧文只覆盖了 3 个面，漏掉了本轮真正在追的**并发/调度/启动/上限/粘滞**这一组。

### 状态快照（2026-11-24，工作区）

> 本台账的「处置」栏按 **HEAD(v1.0.95) + 当前工作区未提交改动** 的**真实**状态写，
> 不按计划口径。核过工作区（`git status --short src/`）：

| 处 | 基线 HEAD | 当前工作区 | 对应提交/负责人 |
|---|---|---|---|
| T2 路由并发排队 | 待办 | **已实现**（`queued` 队尾兜底 + `only_concurrency_blocked` + `AccountBusy`→503 `account_busy`；见 §2.1） | routing-owner |
| T3 用量查询调度 | 待办/判定 | **仍待办**（`account_session/usage_query.rs` 未改） | usage-owner |
| T4 粘滞失效 | **部分对齐**（只在 3 类失败解绑） | **已补齐**（`channel/mod.rs` 改为凡失败即解绑） | upstream-owner，见 [`V351-STICKY-FAILURE-MATRIX.md`](V351-STICKY-FAILURE-MATRIX.md) |
| T5 插件宿主接线 | 已对齐 | 已对齐 | — |
| T5 尝试上限 | 待办（5/20 封顶） | **已实现**（`config.rs`/`error_policy.rs`/`rotate.rs`/`settings.rs`/`main.rs` 改为 32/100） | host-owner |
| T5 启动预热 | 不适用 | 不适用（随 Agent 后端缺口） | — |

> 读法：下文 §1.C 的「现状」描述的是**工作区已改后**的代码；基线差异在 §2.3 单列。


---

## 0. 口径与方法

```bash
# 锚点（别信本地缓存 tag）
git -C D:/GitHub/Agent2API fetch origin --tags
git -C D:/GitHub/Agent2API ls-remote --tags origin | grep 'v3\.5'
git -C D:/GitHub/Agent2API log --oneline v3.5.0..v3.5.1

# 全量 Rust 增量
git -C D:/GitHub/Agent2API diff --numstat v3.5.0 v3.5.1 -- '*.rs'
```

**分类口径**（四档，互斥）：

| 档 | 含义 |
|---|---|
| ✅ 已对齐 | AIGX 已有等价实现，本轮只需复核 |
| 🔧 待办 | 上游有真实行为增量，AIGX 缺，归 T2–T5 |
| ➖ 不适用 | AIGX 无对应产品面（桌面宿主 / 订阅 provider / 独立计价 / SMS 子系统），不移植并写理由 |
| ⚠️ 独立缺口 | 是真实缺口，但**不在本轮「5 处」范围**，另立项跟踪（Agent 客户端配置后端） |

---

## 1. 全量文件台账（63 个 .rs）

### A. 路由并发排队（→ T2，`routing-owner`）

| 上游文件 | 变更 | AIGX 现状 / 落点 | 处置 |
|---|---|---|---|
| `core/routing.rs` (+73/−12) | 「并发已满 ≠ 不可用」：账号在**其它条件全通过**后，若并发满则从 `blocked` 改收进 `queued`，排序后**追加到 `ordered` 末尾**（state=`concurrency`），不再直接丢弃。附新测 `busy_usable_account_queues_at_tail_while_unusable_busy_account_stays_blocked` | 账号级选号在 [`src/account_session/store.rs`](../src/account_session/store.rs) `acquire`/`pick`：`max_concurrency>0 && in_flight>=max` → `reasons.concurrency += 1` + `skip_detail("concurrency")` + `continue`（旧口径**直接丢**）→ **工作区已改**：改为收进 `queued` 并做队尾兜底，配 `SkipReasons::only_concurrency_blocked` + `bridge/account.rs::pick_account_or_wait`，全忙回 503 `account_busy`。**渠道级** [`channel/routing_policy.rs`](../src/channel/routing_policy.rs) `plan_route` 不含并发概念（AIGX 面板是渠道级，账号级无 state 投影） | ✅ **工作区已实现**（见 §2.1） |

### B. 用量查询队列调度（→ T3，`usage-owner`）

| 上游文件 | 变更 | AIGX 现状 / 落点 | 处置 |
|---|---|---|---|
| `core/usage_query/queue.rs` (+52/−4) | 全局 `Mutex<Queue>` 的 worker 改为：①按 provider 取 `usage_policy::for_provider` 的 `concurrency` 收紧 `background_per_provider`；②引入 `next_ready: HashMap<provider, Instant>` 做「相邻启动最小间隔」；③新增 `Notify` 唤醒（`submit` 时 `notify_one`，worker `select!` 里 `notified()`），把固定 `sleep(100ms)` 换成 `next_wake` 计算的 `10..=100ms` 自适应睡眠 | AIGX 无 `usage_query/queue.rs` 这套全局队列，走的是**按账号独立间隔巡检** [`src/account_session/usage_query.rs`](../src/account_session/usage_query.rs)：`sweep()`（`usage_query.rs:123`）按 provider 分组，`ProviderGate`（`:33`）保证「相邻启动间隔 ≥ 申报值」，并发 = `min(宿主 CONCURRENCY=3, 申报 concurrency)`（`:145-148`）。**宿主无 `Notify` 唤醒**（巡检由 cron/间隔驱动，无排队提交入口） | 🔧 **待办 / 立项判定** |

> **判定要点（给 usage-owner）**：上游 `queue.rs` 的三件事里，**并发收紧**与**最小间隔** AIGX 的 `sweep` 已等价具备（`ProviderGate` + `min()`）；真正的结构差异是**「提交-唤醒」模型** —— AIGX 的余额查询没有「排队等槽位」的入口，是定频巡检。因此 `Notify` 唤醒与 `next_ready` 自适应睡眠**在 AIGX 没有对应物**。建议结论：`concurrency`/`min_interval` 记为「已等价」，`Notify` 唤醒记为「结构不适用（无提交队列）」，除非要做「手动查余额排队」才需要引入。

### C. 上游粘滞失效与失败路径（→ T4，`upstream-owner`）

| 上游文件 | 变更 | AIGX 现状 / 落点 | 处置 |
|---|---|---|---|
| `core/upstream/route_sticky.rs` (+32) | 新增 `StickySessions::forget(key, model, candidate, generation)`：仅当「绑定正是该候选」且「世代未变」才解绑；附单测 | [`src/channel/affinity.rs`](../src/channel/affinity.rs) `evict_if(session, model, channel_id)`（`:148`）：仅当当前绑定正是该渠道才删；AIGX 无 generation 概念（用 channel_id 匹配等价替代，最坏只少粘一次）；附单测 `cache_evict_if_only_removes_matching_binding` | ✅ **已对齐**（候选匹配语义） |
| `core/upstream/route_state.rs` (+4) | `RouteState::sticky_forget` 转发 | `AffinityCache` 直连调用（无中间层） | ✅ **已对齐** |
| `core/upstream/error_policy_finish.rs` (+25/−1) | 新增 `forget_sticky(service, ctx, sticky, candidate)`：`pinned_account` 不解绑，否则解绑；在 `finish_failure` 入口调用 | [`src/channel/mod.rs`](../src/channel/mod.rs) `record_channel_failure`（`:1903`）**工作区已改为**在所有早退分支之前无条件 `evict_if`（`:1918`），注释明确「判据是这一轮失败，不是失败类型」；AIGX 无 pinned 概念 | ✅ **工作区已补齐**（基线只按 3 类失败解绑，见 §2.3） |
| `core/upstream/provider_loop.rs` (+28/−7) | 在 8 个「直接 return」的失败分支（replay_guard blocked / subscription / plugin_busy / 429 / 本地 556 构造期失败 / 手动终止前 / token 刷新预算耗尽 / finish_failure）逐一补 `forget_sticky`，例外：`telemetry.is_cancelled()` 与 pinned 不解绑 | AIGX 的对应失败分支最终都汇入 `ChannelStore::record_channel_failure`（渠道级）→ 工作区已改为无条件 `evict_if`。**真缺口**是基线只在 `ServerError/Timeout/ConnectionError` 解绑，`ModelNotFound`/`BridgeError::Config`/401/402/429/空响应都**保留粘性**（其中前两者不可重试且不冷却 → 会话反复撞坏渠道） | ✅ **工作区已补齐**（逐码核对见 T4 矩阵 §3.1） |

> **已核对结论**（详见 [`V351-STICKY-FAILURE-MATRIX.md`](V351-STICKY-FAILURE-MATRIX.md)）：AIGX 无 replay_guard / subscriptions / pinned / 世代号 / `model_cooling_down` 前缀，对应 5 处 N/A；两处**有意差异**保留并写明理由：① 400/413/422 断路器口径（AIGX 不计入，上游照记）；② 手动终止时上游**不**解绑、AIGX 解绑。另有一条**超范围观察项**：粘性**读**路径的 model 键与写路径不一致（有 `model_mapping` 的渠道粘性实际不命中），需另立任务。

### D. 尝试上限口径（→ T5，`host-owner`）

| 上游文件 | 变更 | AIGX 现状 / 落点 | 处置 |
|---|---|---|---|
| `config/types.rs` (+4/−2) | `DEFAULT_RETRY_SWITCH_COUNT` **5 → 32**；新增 `RETRY_MAX_SWITCH_COUNT = 100`（注释：与转发层 `MAX_ROUTE_ATTEMPTS` 对齐） | [`src/config.rs`](../src/config.rs) 工作区新增 `DEFAULT_RETRY_SWITCH_COUNT=32`、`RETRY_MAX_SWITCH_COUNT=100`、`MAX_SWITCH_ATTEMPTS=101`；`default_retry_switch_count()` 改为引用常量（原为 `5`） | ✅ **已实现**（工作区） |
| `config/error_policy_types.rs` (+8/−2) | `MAX_SWITCHES = 100`；`ErrorSettings::default()` 改为引用 config 常量；validate 上界 10 → `MAX_SWITCHES` | [`src/error_policy.rs`](../src/error_policy.rs) 工作区新增 `MAX_SWITCHES=100`，`switches` 默认改引 `config::DEFAULT_RETRY_SWITCH_COUNT`（原 5），validate 上界 `(switches,0,10)` → `MAX_SWITCHES`（原 `:352`），并加测 `switch_limit_follows_v351_defaults_and_bound` | ✅ **已实现**（工作区） |
| `config/parse.rs` (+1/−1) | 换号次数解析上界 `RETRY_MAX_COUNT` → `RETRY_MAX_SWITCH_COUNT` | AIGX 无独立 parse；等价在 `error_policy.rs` 的 `build_snapshot` legacy 判定与 `config.rs` 常量 | ✅ **已实现** |
| `config/mod.rs` (+55) | 新增 `retry_and_error_policy_tests`：默认 3/32、用户值保留、100 合法、101 回落 32 | 工作区测试改在 `error_policy.rs::switch_limit_follows_v351_defaults_and_bound`（断言默认 32、上界 100） | ✅ **已实现** |
| `api/retry_api.rs` (+3/−3) | 换号次数 PUT 上界 → `RETRY_MAX_SWITCH_COUNT` | AIGX 无 `retry_api.rs`；等价在 [`src/api/admin/settings.rs`](../src/api/admin/settings.rs) 工作区 `v.min(crate::config::RETRY_MAX_SWITCH_COUNT)`（原 `.min(10)`） | ✅ **已实现**（工作区） |
| `core/upstream/mod.rs` (+3/−4) | `MAX_ROUTE_ATTEMPTS` **32 → 100**（与 `MAX_SWITCHES` 对齐） | AIGX 无同名常量；等价物 [`src/config.rs`](../src/config.rs) 工作区 `MAX_SWITCH_ATTEMPTS=101`，[`src/account_session/rotate.rs`](../src/account_session/rotate.rs) `DEFAULT_MAX_ATTEMPTS` 改引它、`clamp(1,20)` → `clamp(1, MAX_SWITCH_ATTEMPTS)`（原 `:46`）；[`src/main.rs`](../src/main.rs) 与 [`settings.rs`](../src/api/admin/settings.rs) 的 `policy.max_attempts` clamp 上界同步改 `MAX_SWITCH_ATTEMPTS`（原 20） | ✅ **已实现**（工作区） |
| `core/upstream/account_admission.rs` (+1/−1) | 硬编码 `32` → `MAX_ROUTE_ATTEMPTS` | 无对应（AIGX `account_admission` 走 `try_acquire_runtime`） | ➖ 随上限 |
| `core/upstream/rotate.rs` (+2/−2) | `excluded` 用 `attempt_key` 而非 `id`；`all_busy` 用 `excluded` | AIGX `rotate.rs` 用 session id 排除（无 routing_groups 复合键） | ➖ 随上限 |
| `request_stats/record.rs` (+1/−1) | 注释 `32+1` → `100+1` | 注释级 | ➖ 随上限 |

> **已核对结论**：AIGX 原为 **5/20 三层封顶**（`FAILOVER_MAX_ATTEMPTS=5`、`rotate.max_attempts clamp(1,20)`、`settings.rs clamp(1,20)`）。工作区已把配置面与轮换层对齐到 **默认 32 / 上界 100（单请求最多 101 次）**，并把三层 clamp 统一引用 `config::MAX_SWITCH_ATTEMPTS`，消除「配置能填、路由层提前截断」的不一致。**唯一未动的**是 [`src/api/failover.rs`](../src/api/failover.rs) `FAILOVER_MAX_ATTEMPTS=5`（这是另一条 failover 预算路径，非账号轮换；是否也提到 100 需 team-lead 判定，否则两条路径口径仍不一致）。

### E. 插件宿主接线 + 启动预热（→ T5，`host-owner`）

| 上游文件 | 变更 | AIGX 现状 / 落点 | 处置 |
|---|---|---|---|
| `core/plugin_host/usage_policy.rs` (+61 新) | 新文件：解析 `usage_query`/`usageQuery`，钳 1–64 / 0–10s | [`src/plugin_host/usage_policy.rs`](../src/plugin_host/usage_policy.rs) 已移植（同钳制口径 + 中文注释） | ✅ **已对齐** |
| `core/plugin_host/mod.rs` (+3) | `Host` 增 `usage_policies` 字段 + `pub(crate) mod usage_policy` | [`src/plugin_host/mod.rs`](../src/plugin_host/mod.rs) `pub mod usage_policy`（`:63`）+ wasm `Host` 持 `usage_policies` | ✅ **已对齐** |
| `core/plugin_host/lifecycle.rs` (+4/−2) | 握手后把 `usage_policies` 写回 Host | AIGX 无 `lifecycle.rs`，等价接线在 [`src/plugin_host/wasm/host.rs`](../src/plugin_host/wasm/host.rs) 握手分支 `*host.usage_policies = parse(&declared)`（`:706-711`） | ✅ **已对齐** |
| `core/plugin_host/wasm/host.rs` (+15/−6) | usage_policies parse + alicom4 sms 闸 | usage_policies 部分已对齐；alicom4 见 F 组 | ✅ / ➖ |
| `shutdown.rs` (+2) | 启动装配完成后 `spawn_blocking(core::agents::warm_up)` | AIGX **无 `agents` 后端**（`src/agent/` 是「AI 运维 Agent 工作台」，与上游 `core/agents/` 的「本机 Agent 客户端配置接管」不是同一物） | ➖ **不适用（依赖 agents 后端立项）** |

> **判定要点（给 host-owner）**：「插件宿主接线」这半已经**完成**（usage_policy 已移植并接到握手）。真正的待判定是「**启动预热**」= 上游 `agents::warm_up()`，而 AIGX 没有 `core/agents/` 这套后端 —— 见 §3 独立缺口。结论：启动预热**随 agents 后端一起不适用**，不单独移植。

### F. 登录 / 短信 alicom4（无 SMS 子系统）

| 上游文件 | 变更 | AIGX 现状 | 处置 |
|---|---|---|---|
| `core/plugin_host/wasm/manifest.rs` (+5/−1) | 新增能力 `login.sms.alicom4.v1` + 依赖 + codar 专属闸 | `src/plugin_host/wasm/manifest.rs` 已含该能力与闸（`9b883367`，parity 55:55） | ✅ **已对齐** |
| `api/session.rs` (+7) | sms challenge 能力闸 | AIGX 无 `login/sms` 子系统 | ➖ **不适用** |
| `core/login/sms.rs` (+114/−4)、`sms/challenge.rs` (+19/−2)、`sms/limits.rs` (+9)、`sms_contract.rs` (+8) | alicom4 挑战处理 / 常量 / 契约 | AIGX 无 SMS 登录链（`sms_verify`/`sms_action` 零命中）；能力声明按上游放行（前向兼容） | ➖ **不适用** |
| `core/plugin_host/wasm/login.rs` (+3/−5) | alicom4 响应校验抽到 `sms::check_alicom4_response` | 同上 | ➖ **不适用** |
| `core/plugin_host/wasm/host.rs` 的 alicom4 闸 | 见 E | 同上 | ➖ **不适用** |
| `core/plugin_host/wasm/codearts_login.rs` (+3/−1) | 华为云登录 `allowed_hosts` 置空放行多域 SSO | AIGX 有 `src/account_login/codearts.rs`（账号登录，非 wasm 登录桥）；需核对是否需同样放行多域 | ➖ / **待核** |

### G. 签到（非 5 处，微对齐）

| 上游文件 | 变更 | AIGX 现状 | 处置 |
|---|---|---|---|
| `core/auto_checkin.rs` (+35/−8) | 新增 `plugin_supports_daily_checkin`：优先看 `CheckinInfo.daily`，否则回落 `plugin_checkin_capability`；`checkin_providers`/`available_checkin_providers` 按此过滤 | [`src/checkin/claim.rs`](../src/checkin/claim.rs) 用 `CHECKIN_PROVIDERS` 白名单 + `UNSUPPORTED_PROVIDERS` 黑名单 + 库内是否有账号（`is_implemented`/`available_checkin_providers`，`:295-330`），**无 `daily:false` 语义** | ⚠️ 微对齐（非 5 处，可另立小项） |
| `core/checkin_schedules.rs` (+18/−3) | `extend_plugin_providers_once` 清理「已上线但明确关闭每日签到」的历史留存 | `src/checkin/schedules.rs` 需核对是否有等价清理 | ⚠️ 微对齐 |
| `api/checkin_center.rs` (+1/−1) | 文案「福利在下方对应卡片里」→「请到对应活动或任务标签查看」 | `src/checkin/center.rs` | ⚠️ 文案 |

### H. 模型计价 `model_pricing/*`（+~600 行，含新增 `cn_prices.rs`）

| 上游文件 | AIGX 现状 | 处置 |
|---|---|---|
| `model_pricing/{calc,catalog,cn_prices,fx,mod,types}.rs` | AIGX 的计价面是 [`src/pricing/`](../src/pricing)（exchange_rate / price_sync，与 `PricingStore` 直连），**不是**上游这套 `model_pricing` 快照体系。`cn_prices.rs`（人民币官方基准价）AIGX 无对应 | ➖ **不适用**（并存第二套计价 = 双写不一致风险；需产品决策才引入） |

### I. 订阅 provider `codex_subscription/*`（+~460 行）

| 上游文件 | AIGX 现状 | 处置 |
|---|---|---|
| `codex_subscription/{errors,http,images,mod,models,request}.rs` | 订阅类 provider，依赖订阅登录链 + Responses 线协议。`docs/BETA7-ALIGN-PARITY.md` §3 既定结论：AIGX 未引入该产品面；`sync_agent2api_capabilities.py` 的 `CATALOG_EXEMPT` 已登记豁免 | ➖ **不适用** |
| `core/providers/adapter.rs` (+17) `ChatRequestPlan::images` | 服务订阅 provider 的图片端点，随 codex_subscription 同族 | ➖ **不适用** |

### J. Agent 客户端配置 `core/agents/*`（+607/−124）—— ⚠️ 独立缺口

| 上游文件 | 变更 | AIGX 现状 | 处置 |
|---|---|---|---|
| `core/agents/{mod,paths,backups,apply,apply_tests,dsh_installation,dsh_paths,resources/mod,resources/paths}.rs` | 本机 Agent 客户端（Claude Code / Codex / DeepSeek-dsh 等）配置的**检测、路径解析、备份、写入、DeepSeek 安装**全后端 + `warm_up()` 缓存预热 | AIGX 有 `src/agent/`（**AI 运维 Agent 工作台**，语义完全不同）。提交 `12f0a5b10` 只落了 **前端结构**（agent-a2a-panel 等 9 文件 +4001 行），并明确「**数据面仍缺**：无 `/agent/clients` 检测接口、无配置保存/预览/应用/路径保存接口，占位空列表、写入如实报接口未就绪」 | ⚠️ **独立缺口**（不在本轮 5 处，另立项：Agent 客户端配置后端） |
| `shutdown.rs` 的 `agents::warm_up` | 见 E | 随本缺口 | ⚠️ 随缺口 |

### K. 桌面宿主 `desktop-tauri/*`（tauri 应用层，非网关）

| 上游文件 | AIGX 现状 | 处置 |
|---|---|---|
| `src-tauri/src/{bridge,commands,lib,login,login/managed_window}.rs`（+58/−5） | AIGX 是**网关**，无 Tauri 桌面宿主；登录窗口/命令面不在范围 | ➖ **不适用** |
| `web_shim.rs` (+2/−1) | 同上 | ➖ **不适用** |
| `plugin_host/wasm/desktop_launch/windows/platform.rs` (+180/−42) | 桌面客户端启动/热切换的 Windows 进程句柄状态机（`HandleState::{Active,Exiting,Exited}`）。AIGX 无桌面宿主，`desktop_launch` 一直是「不适用」项 | ➖ **不适用** |

### L. 杂项

| 上游文件 | 变更 | AIGX 现状 | 处置 |
|---|---|---|---|
| `core/providers/readiness.rs` (+4/−2) | 1ms 边界修正：写侧也用 `hold_at(..., base)` 避免跨毫秒 | AIGX 无对应 gauge 测试夹具，等价修正已在其自身测试成立 | ➖ **不适用（等价）** |
| `core/billing/keepalive.rs` (+13/−4) | `update_raw_field` → `try_update_raw_field`（配置库不可用时报错而非静默） | AIGX 的 WorkBuddy 保活配置面不同，需核对是否有「静默吞错」的同类点 | ➖ / **待核** |

---

## 2. 五处剩余增量：上游 diff 精要 × AIGX 落点

> 下面五条是本轮真正在追的「并发 / 调度 / 启动 / 上限 / 粘滞」。每条给出
> **上游做了什么 → AIGX 现在什么样 → 结论/归属**。

### 2.1 并发：路由并发满的账号改「排队」而非「丢弃」（T2）

- **上游**（`core/routing.rs`）：把 `at_concurrency_limit` 的判断从「进 `blocked`」改为
  「先确认账号其它条件全通过，再收进 `queued`」，`queued` 排序后 `chain` 到 `ordered` 末尾，
  state 仍是 `concurrency`，理由文案改成「排队等一个槽位空出来」。语义：**并发满 ≠ 不可用**，
  前面的候选都发不出去时它接着上。配套把「软上限」注释改成「实际发送前原子预占、不能超额」，
  并由 `core/upstream/account_admission.rs::select` 兜住等待：全忙时在 `queueWaitSeconds` 内
  循环重选，超时回 503 `account_busy` + `Retry-After: 1`。
- **AIGX 基线**：账号级 `acquire`（`src/account_session/store.rs:911` 附近）对并发满的账号
  `continue` 直接丢；全部抢光返回 `None`，上层只报 `all_accounts_failed`（状态码同为 503，
  但错误码是「账号耗尽」，客户端分不出「稍后重试」）。渠道级
  `channel/routing_policy.rs::plan_route` 不含并发维度 —— AIGX 的队列面板是**渠道级**，
  账号级并发没有面板投影，故「state=concurrency 的队尾展示」这一半在 AIGX 无对应物。

**落点（已实现，routing-owner）** 三处，都保持在 `acquire` 之外不破坏原子占位：

1. **队尾兜底**（`store.rs::acquire_with_reasons_detail`）：并发满但其余档位全过的账号收进
   `queued`，不再当「不可用」丢弃；空闲候选一个都占不到时，用**同一套** `try_claim` 闭包
   从 `queued` 兜底再试一次（快照过滤与原子占位之间那个窗口里，槽位可能刚回落）。
   `try_acquire_runtime` 的「检查 + 自增」仍不可拆分，**兜底不越过硬上限**。
2. **全忙判定**（`store.rs::SkipReasons::only_concurrency_blocked`）：只有「并发满」这一个档位
   非零才算「只是忙」；掺了冷却/过期/禁用这类「等多久都不会好」的原因就不算 —— 否则用
   「忙」掩盖真因，客户端会去重试一条永远不会成功的请求。
3. **排队等待 + 可重试错误**（`bridge/account.rs::pick_account_or_wait` / `exhausted`）：全忙时
   按 `/api/queue` 的 `queueMaxWaits × queueWaitSeconds` 等槽位（总时长封顶 30s，`BUSY_WAIT_CAP`），
   等不到返回新增的 `BridgeError::AccountBusy` → 503 + 错误码 `account_busy`。
   默认 `queueWaitSeconds=0`（= 不排队）即不等待，选号延迟与接线前逐字节一致，只修正错误码语义。
   另外五处穷尽匹配已同步补齐：`bridge/mod.rs`（`http_status`/`error_type`）、
   `error_translate.rs`（`kind`/`code`）、`api/openai/resolve.rs`（记为可重试：选号阶段没发出过
   上游请求，换渠道不会重复计费）、`channel/mod.rs`（记为 `FailureType::ClientError`：渠道本身
   健康，只是账号槽位占满，**不熔断、不冷却**）。

- **结论**：✅ **工作区已实现**。测试四处：`busy_only_pool_keeps_the_hard_cap_and_reports_busy`
  （兜底不越上限 + 上层能认出全忙）、`only_concurrency_blocked_separates_busy_from_truly_unavailable`、
  `only_busy_reasons_report_account_busy`、`account_busy_maps_to_503_with_its_own_code`。
  **残留差异 1 处（判定不移植）**：上游 busy 响应带 `Retry-After: 1`；AIGX 的
  `bridge_error_response` 返回 `(StatusCode, Json<Value>)`，不带自定义响应头，要加必须改签名并
  动全部调用点。本轮按「错误码已足以让客户端判定可重试」收口，不为此扩签名 —— 若后续要做严格的
  头部对齐，需单独立项（改 `bridge_error_response` 返回 `Response`）。

### 2.2 调度：用量查询队列的并发/间隔/唤醒（T3）

- **上游**（`core/usage_query/queue.rs`）：worker 从「固定 100ms 轮询」改为
  ①按 `usage_policy.concurrency` 收紧 per-provider 并发；②`next_ready` 记每家下次可启动时刻；
  ③`Notify` 在 `submit` 时唤醒 worker，睡眠时长按 `next_wake` 自适应 10–100ms。
- **AIGX**：无全局队列，走定频巡检 `src/account_session/usage_query.rs::sweep`
  + `ProviderGate`（相邻启动间隔 ≥ 申报值）+ `min(宿主 CONCURRENCY=3, 申报 concurrency)`。
- **结论**：🔧 **待办 / 立项判定**。并发收紧与最小间隔**已等价**；`Notify` 唤醒与自适应睡眠
  **无对应物**（AIGX 没有「提交排队」入口）。建议记为「结构不适用」，除非新增「手动查余额排队」。

### 2.3 粘滞：候选失败后解绑会话（T4）

- **上游**（`route_sticky.rs` + `error_policy_finish.rs` + `provider_loop.rs`）：新增
  `forget(key, model, candidate, generation)`，在所有失败分支调用，例外 pinned / 手动取消。
- **AIGX**：`AffinityCache::evict_if`（`src/channel/affinity.rs:148`，仅删匹配绑定）+ `ChannelStore::record_channel_failure`。**基线 HEAD** 只在 `ServerError/Timeout/ConnectionError` 解绑（burncloud P0-1 旧口径），其余失败类型**保留粘性**；**当前工作区**已改为「凡失败即解绑」（在 `record_channel_failure` 所有早退分支之前无条件 `evict_if`，`src/channel/mod.rs:1918`）。
- **结论**：✅ **工作区已补齐**。真缺口是基线把 `ModelNotFound`/`BridgeError::Config`（不可重试且不冷却，渠道留候选池）/401/402/429/空响应都保留粘性 → 会话反复撞坏渠道。逐码核对见 [`V351-STICKY-FAILURE-MATRIX.md`](V351-STICKY-FAILURE-MATRIX.md)；两处有意差异（400/413/422 断路器口径、手动终止解绑方向）已保留并写明理由；另有一条**超范围观察项**：粘性**读**路径的 model 键与写路径不一致（有 `model_mapping` 的渠道粘性实际不命中），需另立任务。

### 2.4 上限：换账号次数 5→32、硬上限 100（T5）

- **上游**：`DEFAULT_RETRY_SWITCH_COUNT 5→32`、`RETRY_MAX_SWITCH_COUNT=100`、
  `MAX_SWITCHES=100`、`MAX_ROUTE_ATTEMPTS 32→100`，`ErrorSettings::default` 引用 config 常量。
- **AIGX**：**基线 HEAD** 是 5/20 封顶（`switches:5`@`error_policy.rs:160`、validate 上界 10@`:352`、`default_retry_switch_count()=5`@`config.rs:831`、`FAILOVER_MAX_ATTEMPTS=5`@`api/failover.rs:17`、`rotate.max_attempts clamp(1,20)`@`rotate.rs:46`、`settings.rs:381 clamp(1,20)`）。**当前工作区**已实现：`config.rs` 新增 `DEFAULT_RETRY_SWITCH_COUNT=32` / `RETRY_MAX_SWITCH_COUNT=100` / `MAX_SWITCH_ATTEMPTS=101`；`error_policy.rs` `MAX_SWITCHES=100` 且默认改引常量；`rotate.rs`/`main.rs`/`settings.rs` 三层 clamp 统一引 `MAX_SWITCH_ATTEMPTS`。
- **结论**：✅ **工作区已实现**（host-owner）。唯一未动的 `FAILOVER_MAX_ATTEMPTS=5` 是另一条 failover 预算路径，与账号轮换不同源 —— 是否也提到 100 需 team-lead 判定，否则两条路径口径仍不一致。

### 2.5 启动：装配完成后预热（T5）

- **上游**（`shutdown.rs`）：`start()` 里启动装配完成后 `spawn_blocking(agents::warm_up)`。
- **AIGX**：无 `core/agents/` 后端（`src/agent/` 是 AI 运维工作台，不同物），故无 `warm_up`。
- **结论**：➖ **不适用**（随 Agent 客户端配置后端缺口立项，见 §3）。插件宿主本身的接线已完成（§1.E）。

---

## 3. 独立缺口（不在本轮 5 处内，另立项）

| 缺口 | 证据 | 建议 |
|---|---|---|
| **Agent 客户端配置后端** | 上游 `core/agents/*`（9 文件 +607/−124）是「本机 Agent 客户端检测/配置写入」全后端；AIGX 只有前端结构（`12f0a5b10`，明确「数据面仍缺」），后端接口 `/agent/clients` 等均无 | 单独立项：先定「AIGX 是否做本机 Agent 客户端接管」产品决策，再按上游逐文件重建后端；`shutdown.rs` 的 `warm_up` 随此项一起 |
| **人民币计价 `cn_prices`** | 上游 `model_pricing/cn_prices.rs`（+169）是国内渠道人民币官网基准价 | 若 AIGX 要支持「国内渠道按人民币官方价结算」再立项，否则维持 `src/pricing/` 单一体系 |
| **签到 `daily:false` 语义** | 上游 `plugin_supports_daily_checkin`；AIGX 只有白名单+黑名单 | 小项，可随下一轮签到对齐一并做 |

---

## 4. 验证门禁与复核方法

**禁止本地 `cargo build`**（本轮约定），验证走 CI。静态自检可用：

```bash
# 上游全量增量复核（任何时候上游发新版都重跑）
git -C D:/GitHub/Agent2API fetch origin --tags
git -C D:/GitHub/Agent2API diff --numstat v3.5.1 <新tag> -- '*.rs'

# AIGX 侧落点是否已存在（本台账引用的符号）
grep -rn "evict_if\|usage_query_policy\|FAILOVER_MAX_ATTEMPTS\|DEFAULT_RETRY_SWITCH_COUNT" src/

# 宿主面 parity（人读 + 与基线比对）
node tools/account-pool-plugin-parity.mjs
node tools/account-pool-plugin-parity.mjs --check
```

**教训（已入库）**：`--check` 以基线文件为参照做增量判定，**上游新增能力时不会自动变红** ——
`login.sms.alicom4.v1` 就是靠人工全集比对才发现的（修复见 `9171a2dc`）。同理，本台账必须
**逐文件**而非只看计数，否则「并发/调度/启动/上限/粘滞」这类**行为增量**会整组漏掉（旧
`V351-ALIGN.md` 正是这么漏的）。

---

## 5. 与其他文档的关系

- [`V350-ALIGN.md`](V350-ALIGN.md)：v3.5.0 正式版（beta.8.2→v3.5.0 的 9 项判定）。
- [`V351-ALIGN.md`](V351-ALIGN.md)：v3.5.1 旧口径（只覆盖 3 项 + 6 项不移植）。**本文补全其遗漏**：
  旧文「待办无阻塞项」不成立 —— 并发/调度/启动/上限这一组行为增量当时未纳入。
- [`V351-STICKY-FAILURE-MATRIX.md`](V351-STICKY-FAILURE-MATRIX.md)：T4 粘滞失效的逐错误码×上游分支核对矩阵（本文 §1.C 的事实来源）。
- [`docs/a2a-alignment/README.md`](a2a-alignment/README.md)：v3.5.0-beta.1 的逐文件移植总表（历史基线）。
- [`BETA7-ALIGN-PARITY.md`](BETA7-ALIGN-PARITY.md) / [`BETA8-ACCOUNT-POOL-ERROR-RULES.md`](BETA8-ACCOUNT-POOL-ERROR-RULES.md)：
  beta.7/beta.8 的宿主面审计（订阅 provider / 计价 豁免理由的出处）。
- 本文只覆盖 **v3.5.0 → v3.5.1** 这一跳的 **Rust** 增量；前端/多语言/手机端产物不在本台账范围。
