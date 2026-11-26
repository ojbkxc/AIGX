# v3.5.1 对齐台账（AIGX × Agent2API 最新正式版）

> 上游：`D:\GitHub\Agent2API` tag **v3.5.1** = `33f5e797`（发布 2026-11-23T17:46Z，
> `prerelease=false`）。`origin/main` 与 `origin/beta` 同指向它；
> `git ls-remote --tags origin` 无更新 tag —— 这就是当前最新正式版。
> AIGX：基线 v1.0.94。

## 1. 已对齐（3 项，均有测试）

| 上游变更 | AIGX 落点 | 提交 |
|---|---|---|
| `wasm/manifest.rs`：新增能力 `login.sms.alicom4.v1` + 依赖 `login.sms.v1`/`login.captcha.alicom4.v1` + codar 专属闸 | `src/plugin_host/wasm/manifest.rs`：CAPABILITIES 54→55、依赖表、provider 闸；parity 集合 55:55 双向零差集 | `9b883367` |
| 新增 `plugin_host/usage_policy.rs`：插件申报 `usageQuery{concurrency,minIntervalMs}`，宿主按家限流 | `src/plugin_host/usage_policy.rs`（解析/钳制 1–64、0–10s）+ `WasmHost.usage_policies` + `sweep` 分组限流（组内 `min(3, 申报值)`、相邻启动间隔 ≥ 申报值；未申报走 0 间隔，行为不变） | `7a5c164b` |
| `upstream/error_policy_finish.rs` + `route_sticky.rs`：`forget_sticky` —— 候选失败后**解除该会话绑定**，但仅当「绑定的正是该候选」且世代未变、且非 pinned | `AffinityCache::evict_if(session, model, channel_id)`；`mark_cooldown` 的亲和清除改为条件删除（AIGX 无 pinned/世代概念，用 channel_id 匹配等价替代） | `bbc80d05` |

## 2. 不移植（逐项写理由）

| 上游变更 | 不移植理由 |
|---|---|
| `core/providers/codex_subscription/{mod,http,images,errors,models,request}.rs`（+ 大量） | 订阅类 provider。`docs/BETA7-ALIGN-PARITY.md` §3 既定结论：chatgpt/claude/codex_subscription 依赖订阅登录链 + Responses 线协议，AIGX 未引入该产品面。`sync_agent2api_capabilities.py` 的 `CATALOG_EXEMPT` 已登记豁免 |
| `core/model_pricing/{mod,types,fx}.rs`（模型计价与费用估算体系） | AIGX 的计价面是 `src/pricing/`（exchange_rate / price_sync，与 `PricingStore` 直连），不是上游这套 `model_pricing` 快照体系。搬过来等于并存第二套计价模型，收益是重复、风险是双写不一致。需要先有「AIGX 引入 model_pricing」的产品决策 |
| `plugin_host/wasm/desktop_launch/windows/platform.rs`（+222） | 桌面客户端启动/恢复链。AIGX 是网关，没有桌面宿主；`desktop_launch` 在 AIGX 一直是「不适用」项（见 §3 的 parity 基线） |
| `plugin_host/wasm/{login.rs,codearts_login.rs}` 的 alicom4 挑战处理 | AIGX 无 `login/sms` 子系统（`sms_verify`/`sms_action` 在 AIGX 侧零命中），连短信登录链都没有；能力声明按上游放行（前向兼容），实际流程不在范围内 |
| `core/providers/adapter.rs` 的 `UpstreamResponse::Images` 构造 | 服务订阅 provider 的图片端点（与 codex_subscription 同族），随其一起不引入 |
| `core/providers/readiness.rs` 的 1ms 边界修正 | 上游把 `base + GATE_TTL_MS - 1` 的边界测试改成 `hold_at(..., base)` 避免跨毫秒。AIGX 没有对应 gauge 测试夹具，等价修正已在其自身测试里成立 |
| `core/usage_query/queue.rs`（+52/−4，v3.5.1 里唯一的调度改动） | **队列底座不适用，策略语义已在 §1 落库。** AIGX 没有「全局后台查询队列」这一层：手动任务由 `usage_jobs::spawn_job` 直接 `tokio::spawn`，自动巡检由 cron 每 60s 跑一轮 `sweep`（`main.rs` 的 `account-session-usage-query`）。queue.rs 的四项改动逐项判定：① 按家并发上限（`min(limits.background_per_provider, 申报 concurrency)`）与 ② `next_ready` 最小间隔闸 —— 语义已由 §1 第 2 行的 `sweep` 分组 + `ProviderGate` 承接；③ `wake_notify()`（`Notify` 唤醒替代 100ms 空转）与 ④ `sleep_for` 的 10–100ms 钳制 —— 都是常驻 worker 循环的实现细节，AIGX 无该循环，直接 spawn 的启动延迟本来就低于排队 → 无宿主，不移植。**残留的行为差异见 §3.1 立项 U-1/U-2** |

## 3. 待办 / 复核方法

**待办**：无阻塞项（`queue.rs` 的判定见 §2 末行）。§3.1 的两项残留差异已立项、未实施，不影响本次对齐收口。若上游再发新版本，按下面流程复核：

```bash
# 1. 版本锚点（别信本地缓存的 tag 列表）
git -C D:/GitHub/Agent2API fetch origin --tags
git -C D:/GitHub/Agent2API ls-remote --tags origin | Select-String 'v3\.'
curl -s https://api.github.com/repos/aimod-cc/Agent2API/releases?per_page=5

# 2. 宿主面全量比对（parity 工具必须跑，且要逐项而非只看计数）
node tools/account-pool-plugin-parity.mjs            # 人读报告
node tools/account-pool-plugin-parity.mjs --check    # 与基线比对

# 3. 能力表逐项（--check 曾因基线增量判定而漏报 alicom4，见 9171a2dc）
node -e "…比对两侧 CAPABILITIES 全集…"               # 见该提交的复现脚本思路
```

**教训（已入库）**：`--check` 以基线文件为参照做增量判定，**上游新增能力时不会自动变红**——
`login.sms.alicom4.v1` 就是靠人工全集比对才发现的（修复见 `9171a2dc`：四项差集进 baselineShape）。

## 3.1 立项（**已实施 2026-11-26**）：U-1 / U-2 余额查询的按家限流只盖住了自动巡检

> 实施结果（`A2A-V351-FULL-AUDIT-2026-11-26.md` §7）：`ProviderGate` 提为进程级
> `gate_for(provider)`（`usage_query.rs`），`sweep` / `query_manual` / `usage_jobs`
> 三条路径共用；`wait_turn` 改为锁内预占 + 锁外 sleep；`usage_jobs::spawn_job`
> 先过全局 `background` 与单家 `background_per_provider` 令牌，排队期间
> `status="queued"`。非目标（`queue.rs` 常驻 worker + `Notify`）维持 §2 判定不引入。

上游 v3.5.1 把手动查询和自动巡检**收进了同一道闸**：`usage_query::query_all`（同步手动）与
`run::start`（异步任务）都走 `queue::submit`，所以按家并发上限与 `minIntervalMs` 对
**两条手动路径同样生效**，总并发另有 `limits.background`（默认 32）兜底。

AIGX 目前只有自动巡检过了闸（`7a5c164b`）。

### U-1（主项）：两条手动查询路径绕开了按家闸

| 路径 | 现状 | 缺口 |
|---|---|---|
| `POST /api/account-sessions/usage/start` → `usage_jobs::spawn_job` | 注册表只做同账号去重（`replace`），查询体直接 `tokio::spawn`；`start(None)` 遍历**所有**支持余额的账号 → N 个账号 = N 路并发 | 无总并发上限、无按家分组、不看 `usage_policy` |
| `usage_query::query_manual` | `buffer_unordered(CONCURRENCY=3)`，有总并发上限 | 不按 provider 分组、不受 `min_interval_ms` 约束 |
| `usage_query::sweep`（自动巡检） | 已按家分组 + `ProviderGate` 最小间隔 | —（唯一消费点） |

**为什么这不是「已对齐」而是立项**：后果是可观测的。一个 200 账号的池子点「查询全部余额」
＝ 200 路并发 × 最长 4 次尝试（默认 `retries=3`）× 60s 超时（`DEFAULT_USAGE_QUERY_TIMEOUT_SECS`）；
余额接口普遍是按家限流的第三方接口，这等于用「用户顺手点一下」把上游打挂。
而且与自身口径自相矛盾：定时巡检已经按家限流，手动反而更凶 —— 上游没有这个反差。

### U-2（同批顺手）：巡检的并发基数没走 `runtime_capacity`

`sweep` 的组内并发写死 `CONCURRENCY = 3`，上游用的是
`runtime_capacity::limits().background_per_provider`（默认 8，容量页可配）。
AIGX 已有同一套 `runtime_capacity`（`max_background=32` / `background_per_provider=8`，
与上游默认值逐字相同），但 `account_session` 一次都没引用它 ——
**用户在「运行时容量」页调「单提供商后台并发」对余额巡检无效**。
纯对齐会把每家在飞从 3 抬到 8（默认行为变更），所以这是个需要显式拍板的口径：
（a）照上游用 `limits().background_per_provider`；（b）保留 3 作为 AIGX 更保守的宿主值，
但把「为什么偏离」写进本文并让容量页对该项显式说明。

### 实施建议（谁做都一样，照这个口径）

1. 把 `ProviderGate` 从「`sweep` 内每轮 new 一个」提为进程级注册表
   （`OnceLock<Mutex<HashMap<String, Arc<ProviderGate>>>>`）：一处实现，`sweep` / `query_manual` / `usage_jobs` 共用。
   顺带消掉当前一个已知偏差：现 `ProviderGate` 每轮重置 `last`，`min_interval` 算的是「本轮内相邻启动」；
   上游 `next_ready` 是 worker 级、跨轮保持。当前 `min_interval ≤ 10s < 巡检周期 60s`，实际不可观测，
   但提到进程级后两者语义完全一致。
   **顺手修一处注释与实现不符**：`ProviderGate::wait_turn` 在 `let mut guard = self.last.lock().await`
   跨着 `tokio::time::sleep(...).await` 才算完，锁被持有整段等待（不是模块注释说的「几十微秒」）。
   启动错峰语义是对的（tokio Mutex 公平排队，仍是 0/120/240… 的间隔），但等待方串行堵在锁上；
   提到进程级时把 sleep 挪出锁（先算出 `ready_at` 再释放，然后 sleep）更干净。
2. `query_manual` 改为先按 provider 分组，组内 `min(CONCURRENCY, 申报 concurrency)` + 最小间隔（与 `sweep` 同口径）。
3. `usage_jobs`：查询体在 `ProviderGate` + 进程级后台并发信号量（`limits().background` / `.background_per_provider`）内执行；
   `Job::state` 增加 `queued` 态 —— 上游用 `execution` 时间戳区分 `queued`/`running`，
   AIGX 的 `Job` 只有 `started_at_ms`，得补一个「真正开跑」时间戳。
4. **非目标**：不引入 `queue.rs` 的常驻 worker 与 `Notify`（见 §2 理由）。

### 验收

- 单测：`query_manual` 对同 provider 的 3 个账号、`minIntervalMs=120` —— 相邻启动间隔 ≥120ms，但三个查询有重叠（限的是启动间隔，不是串行化整轮）；
  未申报策略的家行为与现状逐字节一致。
- 单测：`usage_jobs::start(None)` 在 50 个账号、`background=8` 下同时在飞 ≤8（用可计数的假查询体）。
- `cargo test --bin aigx` 全绿（CI 跑，本机不编译）。

### 风险

- `Job::state` 新增 `queued` 会动 `GET /usage` 的对外字段（前端已按 `status` 分支，
  现存取值 `running`/`success`/`failed`/`timeout`/`cancelled`/`superseded`/`config_changed`），前端需容忍新值。
- `query_manual` 加分组后，大批量手动查询耗时变长（`3 路总并发` → `按家 3 路、组间顺序`）—— 正确性换来的代价，UI 上要能说清。

## 4. 与其它文档的关系

- v3.5.0 正式版的对齐记录在 [`V350-ALIGN.md`](V350-ALIGN.md)（含 beta.8.2→v3.5.0 的 9 项判定）。
- beta.7/beta.8 的宿主面审计在 [`BETA8-ACCOUNT-POOL-ERROR-RULES.md`](BETA8-ACCOUNT-POOL-ERROR-RULES.md)
  与 `ACCOUNT-POOL-PLUGIN-PARITY.md`。
- 本文只覆盖 **v3.5.0 → v3.5.1** 这一跳；更早的判定不重复。
