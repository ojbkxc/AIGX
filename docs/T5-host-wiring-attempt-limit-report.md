# T5 交付报告 —— 插件宿主接线 / 启动预热 / 尝试上限口径

> 负责人：host-owner｜任务 T5｜日期：2026-11-24
> 上游锚点：`D:\GitHub\Agent2API` tag **v3.5.1** = `33f5e797`（对齐 v3.5.0 = 上一跳）
> 本机**未跑 cargo**（团队口径：禁止本地编译，验证走 CI）；格式门禁用 `rustfmt --edition 2021 --check` 自查。

## 一、结论速览

| # | 上游 v3.5.0→v3.5.1 增量 | 判定 | AIGX 落点 |
|---|---|---|---|
| 1 | 插件宿主接线：`usage_policy` 接进**两个**宿主（`plugin_host/lifecycle.rs` 通用宿主 + `plugin_host/wasm/host.rs` wasm 宿主） | **已接线（wasm 面）**；通用/脚本宿主面**判定为不扩散**，理由见 §三 | `src/plugin_host/usage_policy.rs` + `src/plugin_host/wasm/host.rs:173/219/708`（既有 `7a5c164b`）；本轮仅核对，未改 |
| 2 | 启动预热：`shutdown.rs::start()` 里 `spawn_blocking(agents::warm_up)`（`#[cfg(not(test))]`） | **不移植**（AIGX 无该子系统，无可预热的探测） | 无（理由见 §四） |
| 3 | 尝试上限口径：换号默认 5→**32**、上界 10→**100**（`RETRY_MAX_SWITCH_COUNT` / `MAX_SWITCHES`）、转发层 `MAX_ROUTE_ATTEMPTS` 32→**100** | **对齐（本轮已落地）** | `src/config.rs`、`src/error_policy.rs`、`src/api/admin/settings.rs`、`src/account_session/rotate.rs`、`src/main.rs` |

**顺带修掉一个既存口径裂缝**：错误处理页的「换账号上限」在 AIGX 是**悬空字段**——前端早就是 0–100（`bbc80d05` 里改的），后端 `validate` 却卡在 ≤10，且该值**从未进入数据面**（数据面读 `retry_switch_count` → `RotationPolicy.max_attempts`）。本轮三处一并收口（§二.4）。

---

## 二、尝试上限口径：上游口径与 AIGX 落点

### 1. 上游怎么改的（证据文件）

```text
config/types.rs          DEFAULT_RETRY_SWITCH_COUNT  5 → 32
                         + RETRY_MAX_SWITCH_COUNT   100（新常量，注释：与转发层 MAX_ROUTE_ATTEMPTS 对齐）
config/parse.rs          retry_from(): account_switch_count 的上界 RETRY_MAX_COUNT(10) → RETRY_MAX_SWITCH_COUNT(100)
config/error_policy_types.rs  + MAX_SWITCHES = 100；ErrorSettings::default().switches ← DEFAULT_RETRY_SWITCH_COUNT
                         validate(): 换账号上限 0–10 → 0–MAX_SWITCHES
core/upstream/mod.rs     MAX_ROUTE_ATTEMPTS 32 → 100（注释：让高账号数部署配置到 100 时不被转发层提前截断）
core/upstream/provider_loop.rs  'accounts: for _ in 0..=MAX_ROUTE_ATTEMPTS
core/upstream/account_admission.rs  continue 闸 32 → MAX_ROUTE_ATTEMPTS
api/retry_api.rs         写入口上界 RETRY_MAX_COUNT → RETRY_MAX_SWITCH_COUNT
config/mod.rs            两个回归测试：默认 32/32、上界 100 可保存、101 拒绝、越界回落默认
```

一句话口径：**「换号次数」默认 32、上界 100，且这个 100 必须与转发层循环上界同源**——否则设置页能填、转发层提前截断。

### 2. AIGX 改前现状（三处互不一致）

| 位置 | 改前 | 问题 |
|---|---|---|
| `config.rs` `default_retry_switch_count()` / 进程快照 / setter | 5 / 5 / `min(10)` | 默认值停在 v3.5.0 |
| `api/admin/settings.rs:267` 写入口 | `v.min(10)` | 用户填 50 静默变 10 |
| `api/admin/settings.rs:381`、`main.rs:492` 派生 | `(count+1).clamp(1, 20)` | 与写入口的 10 不自洽（10+1 也进不了 20 之外） |
| `account_session/rotate.rs` `RotationPolicy` | 默认 5、`clamp(1, 20)` | **真正生效**的尝试上限；上界与配置面无关 |
| `error_policy.rs` `switches` | 默认 5、`validate` 0–10 | 前端已是 0–100 → 填 11 直接 400；且**数据面无消费者** |

### 3. 本轮改动

```text
src/config.rs
  + pub const DEFAULT_RETRY_SWITCH_COUNT: u32 = 32;   // 对齐上游同名常量
  + pub const RETRY_MAX_SWITCH_COUNT: u32 = 100;      // 对齐上游同名常量
  + pub const MAX_SWITCH_ATTEMPTS: u32 = RETRY_MAX_SWITCH_COUNT + 1;  // = 上游 MAX_ROUTE_ATTEMPTS + 1
  default_retry_switch_count() / 静态快照初值 → 用常量（不再写死 5）
  set_retry_switch_count → min(RETRY_MAX_SWITCH_COUNT)
src/error_policy.rs
  + pub const MAX_SWITCHES: u64 = 100;                // 对齐上游 MAX_SWITCHES
  ErrorPolicyConfig::default().switches → config::DEFAULT_RETRY_SWITCH_COUNT as u64
  validate(): 换账号上限 0–10 → 0–MAX_SWITCHES
  build_snapshot(): legacy 判定改比对常量（原写死 != 5，改默认值后会把默认配置误判成 legacy）
src/api/admin/settings.rs
  写入口 min(10) → min(RETRY_MAX_SWITCH_COUNT)
  派生 max_attempts clamp(1,20) → clamp(1, MAX_SWITCH_ATTEMPTS)   ← 两处（limits 保存 / 错误处理保存）
src/main.rs  开机派生 clamp(1,20) → clamp(1, MAX_SWITCH_ATTEMPTS)
src/account_session/rotate.rs
  DEFAULT_MAX_ATTEMPTS 5 → DEFAULT_RETRY_SWITCH_COUNT + 1（= 33）
  clamp(1,20) → clamp(1, MAX_ATTEMPTS_CEILING)（= 101）
```

新增/更新的回归测试（CI 上跑）：

- `config.rs::retry_switch_count_defaults_and_clamps_to_v351_bounds` —— 三个常量 + 快照越界钳制
- `error_policy.rs::switch_limit_follows_v351_defaults_and_bound` —— 默认 32、上界 100 可保存、101 拒绝、转发层上界 > 配置上界
- `account_session/rotate.rs::attempt_budget_follows_v351_defaults_and_ceiling` —— 默认 33、上界 101、上界不被 clamp 缩水
- `rotate.rs::clamped_bounds_admin_input` 更新（99 → 101 口径）

### 4. 顺带收口：错误处理页的「换账号上限」不再悬空

改前：`PUT /api/routing/errors` 只写 `config.error_policy` + 装快照，**没有任何地方把 `switches` 送进数据面**（grep `.switches` 只命中 validate / legacy 投影 / `ErrorDecision::switches()`）。等价于「页面上能改、实际不生效」。

改法（对齐上游 `config/error_policy.rs::write_settings` 的双写行为）：

```rust
// 错误处理页保存时回写旧重试字段 → 成为换号上限的唯一真源
config.usage.retry_switch_count = switches.min(RETRY_MAX_SWITCH_COUNT);
// 立即生效（不等重启）
set_retry_switch_count(config.usage.retry_switch_count);
policy.max_attempts = (config.usage.retry_switch_count + 1).clamp(1, MAX_SWITCH_ATTEMPTS);
```

上游同样是「error settings ↔ `KEY_RETRY_ACCOUNT_SWITCH_COUNT` 双向同步」（`write_settings` 回写 legacy 键、`sync_legacy` 反向投影），所以这不是新造语义，是把已有的单向 legacy 投影补成双向。

---

## 三、插件宿主接线：判定「wasm 面已接、通用宿主面不扩散」

### 已接线（核对，未改）

上游把 `usage_policy` 接进两个宿主：

| 上游 | 动作 |
|---|---|
| `plugin_host/lifecycle.rs`（通用/进程宿主） | 握手应答里 `usage_policy::parse` → `Host.usage_policies` |
| `plugin_host/wasm/host.rs`（wasm 宿主） | 同上 + `generation` 提升 |

AIGX 侧（`7a5c164b`）只接了 wasm：

```text
src/plugin_host/wasm/host.rs:173   usage_policies: Mutex<BTreeMap<String, UsageQueryPolicy>>
src/plugin_host/wasm/host.rs:219   usage_query_policy(provider)   // 读取口
src/plugin_host/wasm/host.rs:708   usage_policy::parse(&declared) // 握手成功后落表
src/plugin_host/usage_policy.rs:76 for_provider() → wasm::host_for_provider() → 上面那个读取口
消费者：src/account_session/usage_query.rs:145（sweep 按家限流，T3 面）
```

### 为什么不再扩散到脚本宿主

1. **申报面本来就只有 wasm 一套**。beta.8 的 `error_rules`、`provider_settings` 都是同一形状：`error_rules::for_provider` / `usage_policy::for_provider` 的第一行都是 `wasm::host_for_provider(...)`，脚本宿主（`src/plugin_host/script/runtime.rs`）只注册 `provider_settings`（:537），从不收 `errorRules` / `usageQuery`。单补 `usageQuery` 会造出「限流收、错误规则不收」的新不对称，比现状更难解释。
2. **实际影响面比看起来小**。余额巡检只跑 AIGX 原生支持的 provider（`account_session/balance.rs:33 supports_balance`，硬编码 22 家）；`sweep` 不覆盖插件独有 provider。真正漏的只有一种组合：**脚本插件接管了这 22 家之一**（如 workbuddy / codearts / zcode），其申报的查询并发/间隔被忽略 → 该家余额接口可能吃到 429。
3. 缺的只是**限流增强**，不是功能中断：未申报策略时宿主用默认（组内 3 并发、0 间隔），与 v3.5.0 行为逐字节一致。

**建议**：作为独立条目「脚本宿主申报面（errorRules + usageQuery + settings schema）对齐」立项，一次把三类申报补齐；单点补 usage 不建议。

---

## 四、启动预热：判定不移植

上游 v3.5.1 在启动装配完成后加了一行：

```rust
// desktop-tauri/.../server/shutdown.rs
if !shared.stopping() {
    state.start_background_maintenance();
    #[cfg(not(test))]
    drop(tokio::task::spawn_blocking(crate::server::core::agents::warm_up));   // ← 新增
}
startup.complete();
```

`agents::warm_up()` 预热的是两样**昂贵且易变**的东西（`core/agents/mod.rs`）：

```rust
pub fn warm_up() {
    let binaries = paths::binary_index(true);          // 扫 PATH 建客户端二进制索引（跨盘 IO）
    let dsh_bin = find("dsh").and_then(|def| binaries.find(def));
    let _ = dsh_installation::desktop();               // dsh 客户端安装态探测
    if dsh_bin.is_some() { let _ = dsh_installation::cli(dsh_bin.as_deref()); }
}
```

判定 **不移植**，理由：

1. **AIGX 没有 `core/agents` 这个面**。`agents` 是桌面客户端接管子系统（`CLIENTS` 检测 / 配置 apply / backup / dsh 安装器 / `binary_index`），AIGX 侧不存在；`src/agent/` 是 AI 助手会话，`src/agent2api` 侧近邻是 `tool_services`（MCP/Skill 安装）。
2. **没有可预热的昂贵探测**。AIGX 的客户端落点是 `src/tool_services/config_files.rs` 里的**固定路径表**（10 个目标的 `~/.claude.json` 等），不扫 PATH、不探测二进制、无跨盘 IO，也就没有「首次请求付 1 次冷启动代价」这件事。
3. 全仓 PVS 证据：`grep -rn "env::var(\"PATH\")|which::|fn detect" src/` 只命中 `checkin/benefits/umid.rs`、`purge/skill_import.rs`、`route_special/{compact,title}.rs`——没有客户端二进制探测。
4. 若将来 AIGX 引入客户端接管面，预热点应落在**该面自己的启动装配处**（AIGX 的对应位置是 `main.rs` 后台任务区），不是照抄 `shutdown::start`。

---

## 五、发现但本轮未改（交 lead 定夺）

1. **P2 脚本宿主申报面整体缺失**（§三.3）：errorRules / usageQuery / settings schema 三类申报只在 wasm 宿主生效。建议立项，不单点补。
2. **P2 账号路由页的 `max_attempts` 重启后被开机派生覆盖**：`main.rs:492` 每次启动都用 `config.usage.retry_switch_count + 1` 覆盖 `RotationPolicy.max_attempts`，而「账号路由」弹窗保存的值只进 `RotationPolicy`（落 KV）、不回写 `config.usage.retry_switch_count` → 重启即丢。改法一行：`handle_set_rotation_policy` 把 `max_attempts - 1` 回写 `config.usage.retry_switch_count`。本轮未做，因为要动 `update_admin_config` 的写路径（与 settings 面耦合），属另一个口径决策。
3. **上游 `plugin_host/wasm/host.rs` 的 alicom4 挑战分支**（`sms_captcha` 收敛 + codar 闸）已在 `9b883367` 判定为「能力声明放行、流程不引入」，本轮复核结论不变（AIGX 无 `login/sms` 子系统）。
4. 未移植项照旧：`codex_subscription`、`model_pricing`、`desktop_launch/windows`、`providers/readiness` 1ms 修正（理由见 `docs/V351-ALIGN.md` §2）。

---

## 六、验证口径

- 本机**未编译**：`cargo` 在本机报 `NotSpecified/RemoteException`（见仓库根 `cl.tmp`），按团队口径不走本地编译。
- 已做：`rustfmt --edition 2021 --check` 对本人 5 个文件**零 diff**（仓库里 `src/channel/mod.rs` 存在他人未格式化的既存 diff，未顺手改）。
- CI 需要看的用例：`retry_switch_count_defaults_and_clamps_to_v351_bounds`、`switch_limit_follows_v351_defaults_and_bound`、`attempt_budget_follows_v351_defaults_and_ceiling`、`clamped_bounds_admin_input`，以及既有 `error_policy::` / `account_session::rotate::` 全量。

## 七、归属

- 本轮只改：`src/config.rs`、`src/error_policy.rs`、`src/api/admin/settings.rs`、`src/account_session/rotate.rs`、`src/main.rs` + 本文档。
- 同工作区当时另有他人在改 `src/api/openai/resolve.rs`、`src/bridge/mod.rs`、`src/channel/mod.rs`、`src/error_translate.rs`、`src/purge/presets.rs` —— **不属于本轮**，提交时按文件挑拣。
