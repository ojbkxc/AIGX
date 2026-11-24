# v3.5.1 上游粘滞失效与失败路径核对矩阵（T4）

> 上游锚点：`D:\GitHub\Agent2API` tag **v3.5.1** = `33f5e797`。
> 上游源文件取自 `git show v3.5.1:desktop-tauri/src-tauri/server/src/server/core/upstream/*.rs`
> （**注意**：`D:\GitHub\Agent2API-upstream-main` 的**工作树停在 3.4.2**，直接读工作树会看不到
> 这些改动，必须走 `git show v3.5.1:…`）。
> AIGX 侧：基线 v1.0.94 + 本轮改动。
> 本地不跑 cargo（团队红线），验证走 CI。

## 0. 结论摘要

| 结论 | 数量 | 说明 |
|---|---|---|
| 已对齐（旧口径即覆盖） | 5 类 | 5xx / 超时 / 连接失败 / 插件忙族码（503/502 → ServerError）/ 并发窗口条件删除 |
| **真缺口（本单已补）** | 1 处 | AIGX 只在 `ServerError\|Timeout\|ConnectionError` 解粘滞，其余失败类型**保留粘性**；其中 `ModelNotFound` 与本地构造失败（`BridgeError::Config` → `ClientError`）在 AIGX **不可重试且不冷却**，渠道留在候选池里，粘性会把该会话反复按回这条坏渠道 |
| 不适用（AIGX 无该设施） | 6 处 | replay_guard / subscriptions / model_cooling_down / 556 本地前置 / `pinned_account` / 世代号 |
| 有意差异（不补，写明理由） | 2 处 | 400/413/422 的断路器口径（AIGX 不计入，上游照记）；手动终止时上游**不**解绑、AIGX 解绑 |
| 超范围观察项 | 1 处 | 粘性**读**路径的 model 键与写路径不一致（有 `model_mapping` 的渠道粘性实际不命中），需另立任务 |

`docs/V351-ALIGN.md` §1 第 3 行的说法（"已对齐"）需要按本文收窄为：
**候选匹配语义已对齐，解粘滞的场景集合此前未对齐**（本单补齐）。

## 1. 上游事实：`forget_sticky` 的 9 个入口

上游 v3.5.1 新增 `error_policy_finish::StickyContext` + `forget_sticky()`，
判据是**「这个候选这一轮失败了」**，与错误类型无关；统一护栏有三条：

| 护栏 | 上游实现 | 含义 |
|---|---|---|
| pinned 排除 | `if ctx.pinned_account.is_some() { return; }` | 固定指定账号测试时不动正常会话粘性 |
| 候选匹配 | `route_sticky::forget` 内 `entry.account_id == candidate` | 只解绑「当前绑定的正是这个候选」；并发下别的请求刚写入的健康绑定不能被旧失败抹掉 |
| 世代校验 | `table.generation != generation` → 直接返回 | 清空/重载后，在途旧请求的 forget 不得误删新一代绑定 |
| 空键防御 | `key.is_empty() \|\| candidate.is_empty()` → 返回 | 无会话键（无状态请求）不产生动作 |

调用点（`provider_loop.rs` 行号 = v3.5.1）：

| # | 位置 | 触发场景 | 备注 |
|---|---|---|---|
| 1 | `error_policy_finish.rs:58`（`finish_failure` 入口，`provider_loop` 5 处调用：807 / 918 / 1185 / 1597 / 1702） | 所有走「换号或终止」判定的失败 | 这是主路径：5xx、429 限额记账后、401（无续期家）、以及 `failure.stop` 的终态错 |
| 2 | 896 | 会话式路径 + 8 个错误码：`plugin_busy` `gateway_busy` `account_busy` `plugin_resource_limit` `plugin_protocol_error` `plugin_trap` `plugin_response_started` `web_tools_invalid_request` | 这批码**不换号**直接返回，所以必须显式解绑 |
| 3 | 992 | 凭证准备失败 + `replay_guard().blocked()` | 回放守护阻断 |
| 4 | 1000 | 凭证准备失败 + 订阅类 provider（`subscriptions::contains`） | — |
| 5 | 1164 | **构造请求期本地失败**（`build_*_request` 出错，任意状态码，含 556 与 `account_model_unavailable`） | 一次上游往返都没发生也解绑 |
| 6 | 1232 | 模型冷却中（`model_cooling_down` 429，**未重发上游**） | — |
| 7 | 1427 | 发送后失败 + `replay_guard` 阻断 | 带 `!is_cancelled()` 守卫 |
| 8 | 1468 | 发送后失败 + `failure.stop`（策略判定终止）或 replay 阻断 | 带 `!is_cancelled()` 守卫 |
| 9 | 1522 | 401 刷新成功后重试预算不足（`take_retry` 失败） | — |

**不解粘滞**（同样是设计，不是遗漏）：会话式路径的早退（无下一账号 / 换号预算耗尽 /
`strict_runtime`）、用户手动终止（`!is_cancelled()` 的三处守卫）、`vision_compat` 接管的错误。

## 2. AIGX 事实：唯一落点是 `record_channel_failure`

| 环节 | AIGX 落点 | 语义 |
|---|---|---|
| 写入粘性 | `record_channel_success_with_sticky` → `AffinityCache::insert` | 成功才写；`StickyMode::off/turn` 不写 |
| 读取粘性 | `resolve_bridges_with_affinity_needs`、各 handler 的 `affinity_hit_id` → `AffinityCache::lookup` | 只做**候选置顶**，不做强制选定；候选池已被 `select_for_model_filtered` 按启用/模型/冷却/断路器/AIMD 过滤 |
| **解除粘性** | `ChannelStore::record_channel_failure` → `AffinityCache::evict_if`（`src/channel/mod.rs:1903` / `affinity.rs:148`） | 条件删除：仅当当前绑定正是本渠道 |

`record_channel_failure` 的 8 个调用方（`session_id` 全为请求级会话键）：

- `src/api/openai/chat.rs`：流式 783 附近、非流式 1659 附近（另有 fake-reply 1513 用 `BridgeError::Config` 复用分类）
- `src/api/anthropic.rs`：流式 1056 附近、非流式 1970 附近（fake-reply 912 / 1831 同上）
- `src/api/openai/responses.rs:323`
- `src/agent/llm.rs:417` —— 传 `session_id = None`，**故意不参与**粘性（agent 面无会话键）

本轮改动前的口径（旧 burncloud P0-1）：只有 `ServerError | Timeout | ConnectionError` 调
`evict_if`；`AuthFailed / PaymentRequired / RateLimited / ModelNotFound / EmptyResponse /
ClientError` 一律**保留**粘性。

## 3. 核对矩阵（错误码 × 上游行为 × AIGX 落点 × 结论）

### 3.1 按失败类型

| 失败类型（AIGX） | 触发来源 | 上游行为 | AIGX 旧行为 | AIGX 现行为 | 结论 |
|---|---|---|---|---|---|
| `ServerError` 5xx | 500/502/503/504、`UpstreamDecode`、`InvalidUpstreamConfig`、`AllAccountsFailed` | 解绑（#1） | 解绑 | 解绑 | 已对齐 |
| `Timeout` | `BridgeError::Timeout` | 解绑（#1） | 解绑 | 解绑 | 已对齐 |
| `ConnectionError` | `Transport` / `StreamAborted` | 解绑（#1）；但**手动终止不解绑** | 解绑 | 解绑 | 已对齐（终止方向差异见 §5） |
| 插件忙族码 | `plugin_busy` → 503、`plugin_unavailable` → 503、`plugin_resource_limit` → 503、`plugin_trap` → 502、`plugin_protocol_error` → 502（`wire::http_status_for_code` + `wire_error_to_bridge` 兜底 502） | 解绑（#2，直接 return 分支）；另 2 码 AIGX 无产出方（`plugin_response_started` / `web_tools_invalid_request`），`gateway_busy`/`account_busy` 见 §3.2 | 解绑（经 5xx → `ServerError`） | 解绑 | 已对齐（措辞不同：上游「不换号」，AIGX「冷却+换号」，属 error-policy 范畴，非粘性差异） |
| `ModelNotFound` 404 | 上游 404 / `ModelNotFound(..)` | 解绑（#1 / #8，`failure.stop`） | **保留**，且 `is_retryable_bridge_error` 判 false → 直接 break、**不冷却** | 解绑 | ✅ **真缺口已补** |
| `ClientError` 400/413/422 | 参数非法/超限 | 解绑（`failure.stop` → #8，上游这里是钝口径） | **保留**，且不冷却 | 解绑 | 见 §5「有意差异」说明（断路器口径未变） |
| 本地构造失败（`BridgeError::Config`：未知 provider / 缺 base_url / 缺凭证） | 分类器映射为 `ClientError` | 解绑（#5，构造期本地失败显式解绑） | **保留**，且不冷却 → 会话每次先撞它 | 解绑 | ✅ **真缺口已补** |
| `AuthFailed` 401/403 | 上游 401/403、`AuthError`、`InvalidUpstreamCredentials` | 解绑（#1；但「刷新成功 + 原账号重试成功」时**不**解绑 → 成功后 `sticky_bind` 重写） | 冷却（60s+）+ **保留** | 解绑 | 差异已消除；AIGX 的续期在**发送前**的账号层（`account_session/refresh.rs`），failover 循环里没有上游「401 → 刷新同账号重试一次」那一步，也就没有那个「保留」语义要迁 |
| `PaymentRequired` 402 | 上游 402 | 解绑（#1） | 冷却 + 保留 | 解绑 | 差异已消除 |
| `RateLimited` 429 | 上游 429、`BridgeError::RateLimited` | 解绑（限额记账后仍走 `finish_failure`） | 冷却（`Retry-After`.max(60)）+ **保留** | 解绑 | 差异已消除（旧口径的实际后果：冷却到期后粘性把会话按回刚 429 的渠道） |
| `EmptyResponse` | 连续空响应达阈值后由调用方记 `EmptyResponse` | 解绑（空响应属失败，走 #1/#8） | 保留 | 解绑 | 差异已消除 |

### 3.2 按上游分支（含 AIGX 无对应设施的行）

| 上游分支 | 上游行为 | AIGX 落点 | 结论 |
|---|---|---|---|
| #1 `finish_failure`（换号/终止主路径） | 解绑 | `record_channel_failure` ↔ `evict_if` | 对齐（本单把口径从「按类型」改成「凡失败」） |
| #2 8 码直接 return | 解绑 | 同上（5 码经 5xx 落 `ServerError`；`plugin_response_started` / `web_tools_invalid_request` AIGX 根本没产出；`gateway_busy` 是网关自身过载直接回 503，不经候选失败路径；`account_busy` 由并行的 account 池 owner 新增 `BridgeError::AccountBusy`，本仓现按 `ClientError` 分档 → 在本单口径下同样解绑，与上游 #2 语义一致） | 对齐 / 部分 N/A |
| #3 replay_guard 阻断 | 解绑 | **AIGX 无 replay_guard 设施** | N/A |
| #4 订阅类 provider 凭证失败 | 解绑 | **AIGX 无 subscriptions 子系统**（见 `docs/V351-ALIGN.md` §2 豁免） | N/A |
| #5 构造期本地失败（556 等） | 解绑 | 无 556 前置状态；等价形态是 `BridgeError::Config` → 已覆盖 | 对齐（等价形态） |
| #6 `model_cooling_down` 429（未发上游） | 解绑 | **AIGX 无该前缀判定**（无 `model_cooling_down` 码）；冷却由渠道级 `mark_cooldown` + 模型级冷却在候选过滤处处理 | N/A（形态不同） |
| #7 发送后失败 + replay 阻断 | 解绑（`!is_cancelled`） | 无 replay_guard | N/A |
| #8 `failure.stop` 终态 + 手动终止守卫 | 解绑；**手动终止不解绑** | 终态错 → 现已解绑；手动终止方向相反（见 §5） | 部分对齐 |
| #9 401 刷新后预算不足 | 解绑 | AIGX 无「同账号刷新重试」分支（bridge 内部刷新失败即返回 401） | N/A |
| `pinned_account` 护栏 | 不解绑 | AIGX 无固定指定账号概念 | N/A |
| 世代号护栏 | 世代不符则不解绑 | AIGX 无世代号；`clear_all` / `evict_session` 是整条删除，条件删除最坏只删掉「重新绑到同一渠道」的条目 | 等价可接受（影响仅少粘一次，不产生故障） |
| 会话式早退（无下一账号 / 换号预算耗尽 / strict_runtime） | 不解绑 | 无对应分支（AIGX 的 failover 预算耗尽发生在循环入口，不记 `record_channel_failure`） | N/A |

## 4. 本单改动

`src/channel/mod.rs`

| 位置 | 改动 | 行为变化 |
|---|---|---|
| `record_channel_failure`（1903 起） | 把 `evict_if` 提到方法最前、**所有早退分支之前**；删掉 `should_evict = matches!(ServerError \| Timeout \| ConnectionError)` 白名单 | 任何被记录的渠道失败都会解除「绑定正是本渠道」的会话粘性；断路器/健康/冷却/AIMD 口径**逐字未变**（`ClientError` 仍不计断路器、不冷却、不进失败归档） |
| 方法文档注释 | 重写为上游 `forget_sticky` 口径 + 三条护栏对照 + 旧口径为何不够 | — |
| `mod tests::failure_evicts_sticky_regardless_of_failure_type` | 新增纯函数单测：7 种 `FailureType` 全覆盖解绑；「绑定不是本渠道」时不删；`session_id`/`model` 缺失时无动作 | CI 可跑（`cargo test -p aigx channel::`），本地未编译 |

影响面：粘性只在失败时被**多删**，不新增写路径；粘性本就是「下次选路的置顶偏好」，
丢失后果是多一次按 priority/weight 重新选路，不会造成请求失败。冷却中的渠道本来就被
`is_dispatch_allowed` 挡在候选之外，所以 401/402/429 这几行在冷却期内行为完全不变，
改变的只是冷却到期后的「是否还粘回原渠道」。

## 5. 有意差异（不补，但需知情）

1. **400/413/422 的断路器口径**：上游把 client error 也当失败记账并解绑；AIGX 明确
   「用户请求本身的问题，不代表渠道不健康」，不计断路器、不冷却、不进失败归档
   （v1.0.94 修过：脏请求熔断健康渠道）。粘性解绑已按上游补齐，断路器口径**保持 AIGX 现状**。
2. **手动终止时反向差异**：上游用 `!is_cancelled()` 明确「用户取消不算候选的错，不解绑」；
   AIGX 里取消/断流统一收敛成 `BridgeError::StreamAborted` → `ConnectionError` → 解绑。
   要区分「客户端取消」与「上游断流」需要把取消标志一路传到 `record_channel_failure`
   （改签名 + 8 个调用点），收益只是少丢一次缓存亲和，**本单不动**，记为已知差异。

## 6. 超范围观察项（建议另立任务）

粘性键的**写/读模型名不一致**（与 v3.5.1 无关的既有问题，核对时发现）：

- 写：`record_channel_success_with_sticky(.., Some(&upstream), ..)` —— `upstream` 是**映射后**模型名
- 解绑：`record_channel_failure(.., Some(&upstream), ..)` —— 同上，一致
- 读：`resolve.rs:240`、`chat.rs:513`、`anthropic.rs:808` —— 传的是**请求模型名** `model`

后果：渠道配了 `model_mapping` 时写键 ≠ 读键，粘性**永不命中**（表现为「有映射的渠道偶发不粘」）。
修法二选一（需产品口径）：读出侧改用与写入一致的映射后名字，或写两侧都按请求模型名落键。
未在本单改：涉及 4 个 handler 的读路径 + 语义决策，超出「粘滞失效」范围。

## 7. 验证方式与未验证项

- 静态对照：本文所有上游行号来自 `git show v3.5.1:…/upstream/provider_loop.rs`、
  `error_policy_finish.rs`、`route_sticky.rs`；AIGX 行号为本轮改动后文件行号。
- CI（本地禁 cargo）：`cargo test -p aigx channel::` 应通过；新增单测与既有
  `cache_evict_if_only_removes_matching_binding` 覆盖同一不变式（条件删除）。
- **未验证**：本地未编译、未跑测试；`FailureType` 各分支的 `record_channel_failure`
  副作用（health_tracker / health_archive / AIMD）在本单未改动，行为应与改动前一致。
- 共享文件冲突风险：`src/channel/mod.rs` 共用文件里 **存在两个 owner 的改动**：
  本单 4 个 hunk（`record_channel_failure` 文档/解绑位置、删 `should_evict`、新增单测）
  与 account 池 owner 新增的 `BridgeError::AccountBusy → ClientError` 一行（`classify_bridge_error`）。
  两者不重叠、语义相容（`AccountBusy` 走 `ClientError` 分支，仍在加强后的解绑路径上）。
  `src/account_session/rotate.rs`、`src/error_policy.rs` 等由其它 owner 并行改动，本单未触碰。
