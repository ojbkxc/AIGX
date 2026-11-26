# AIGX × Agent2API v3.5.0-beta.7 对齐审计与实施分工

> ⚠️ **结论已被推翻（2026-11-26 复核）**：本文把订阅 provider 判为「不适用（未移植）」，
> 按用户「AIGX 只能比 Agent2API 多、不能少」的口径该判定不成立，已列入待补缺口
> （见 [`V351-GAP-AUDIT-2026-11-26.md`](./V351-GAP-AUDIT-2026-11-26.md) 的 B-A1）。

> 审计日期：2026-11-22
> 上游：`D:\GitHub\Agent2API` tag `v3.5.0-beta.7`（`v3.5.0-beta.6..v3.5.0-beta.7`，120 文件）
> AIGX：`D:\GitHub\AIGX`（当前 HEAD 含 1.0.80）
> 红线：不写 provider 伪装/规避检测实现；不整段拷 AGPL 代码（净室重写）；不打包分发 AGPL。

## 0. 结论

| 工作流 | 项数 | 需移植 | 不适用 | 新缺口 | 已对齐 |
|---|---:|---:|---:|---:|---:|
| 维护与调度 | 8 | 8 | 0 | 0 | 0 |
| 脚本插件宿主 | 9 | 8 | 1 | 0 | 0 |
| WASM 宿主 | 6 | 1 | 4 | 0 | 1 |
| 凭证与更新 | 5 | 3 | 2 | 0 | 0 |
| 工具与其他 | 5 | 3 | 1 | 1 | 0 |
| 前端 | 5 | 3 | 1 | 1 | 0 |
| **合计** | **38** | **26** | **9** | **2** | **1** |

本轮决策：
- `core/upstream/web_tools` 执行链**本轮不做**（AIGX 目前只有开关，补齐需整模块）。
- `wasm/deepseek_pow`、`egress` 浏览器 UA、Kiro/CodeArts 兑换守卫**不做**（规避检测/红线）。
- Magpie 桌面专属签名升级不移植；社区插件目录检查可移植。

## 1. 维护与调度（maintenance-dev）

| 上游 | AIGX 落点 | 动作 |
|---|---|---|
| `core/maintenance/tasks.rs`（新）、`mod.rs::ALL_TASKS` | `src/maintenance/mod.rs:41,97,178` | 新增 updateCheck / pluginCatalog 等维护任务的执行与记录，复用 `execute` |
| `core/maintenance/types.rs` 的 `current_version/latest_version/change_label` | `src/maintenance/types.rs:20,154` | `Item` 增 3 字段并在 `item()` 映射 |
| `core/maintenance/storage.rs::ALL_TASKS` | `src/maintenance/storage.rs:406` | `latest()` 遍历新增任务 |
| `core/scheduled_tasks.rs` | `src/scheduled_tasks/mod.rs:85,186,306` | 任务表扩展；pluginCatalog 读取插件设置（checkMode/autoUpdate） |
| `config/{types,parse}.rs` `magpiePluginCheck` | `src/config.rs:921` | `ScheduledSettings` 增字段，默认 360 分钟 |
| `api/{maintenance,scheduled_tasks,plugins}.rs` | `src/api/admin/maintenance.rs:45`、`scheduled_tasks.rs:69`、`script_plugins.rs:584` | 新任务 id、extended patch、插件检查统一进维护记录 |
| `plugin_host/catalog.rs`、`script/catalog/refresh.rs` | `src/plugin_host/script/catalog/mod.rs:118`、`refresh.rs:118` | 暴露 `refresh_now/status` 并接入维护租约 |
| `script/updates.rs` | `src/plugin_host/script/operations.rs:473` | `check_updates` 返回逐包 `CheckOutcome` |

## 2. 脚本宿主 + WASM 收窄（scripthost-dev）

| 上游 | AIGX 落点 | 动作 |
|---|---|---|
| `plugin_host/operations.rs` wait handle | `src/plugin_host/operations.rs:244,353` | `start_cached_with_wait` 返回 JoinHandle |
| `plugin_host/rpc.rs` `check_credentials_expiring_within` | `src/plugin_host/script/rpc.rs:68`、`script/host/provider.mjs:169` | `minLeadMs` 透传 |
| `plugin_host/runtime.rs` shutdown 日志 | `src/plugin_host/process.rs:350`、`manager.rs:630` | 补 `plugin.shutdown` routine/pluginId 事件 |
| `script/host/reports.mjs` 配额窗口 | `src/plugin_host/script/host/reports.mjs:13` | `modelQuotas`/配额来源/池排序/routeAside（通用部分） |
| `script/host/allowance.mjs` routeAside | `host/allowance.mjs:7-13` | `applies` 排除 routingAside |
| `script/host/login-policy.mjs`、`provider.mjs`、`login.mjs` | `host/login-policy.mjs:9`、`provider.mjs:137` | 通用 `file_text/file_base64/importCredential`；具体 provider 分支不移植 |
| `host/local-provider.mjs` | — | 不适用（agy 路径探测为 provider 专属） |
| `wasm/manifest.rs` `desktop.launch.v1` | `src/plugin_host/wasm/manifest.rs:48,98,105` | requires 收窄为只保留 `accounts.cas` |
| `wasm/{deepseek_pow,http,codearts_login}.rs` | — | 不适用/红线 |

## 3. 凭证与更新（credupdate-dev）

| 上游 | AIGX 落点 | 动作 |
|---|---|---|
| `core/credential_maintenance.rs` lead +5min | `src/account_session/refresh.rs:237-264,304` | margin 取 max(provider 固定值, 维护间隔+5 分钟) |
| `providers/adapter.rs` min_lead | `src/account_session/refresh.rs:237` | 适配层补 min_lead 参数 |
| `core/update/check.rs` claim/run | `src/update/mod.rs:468` | 拆分 claim/run，`check_with_mode` 写维护记录 |
| `api/accounts_export.rs` known_provider | `src/auth_file.rs:150` | 拒绝把脚本插件 provider 当原生账号文件导入 |
| `logs_store/plugin_events.rs` | `src/api/admin/plugins.rs:1000` | 补 shutdown routine 归类 |
| `providers/chatgpt_subscription`、`codex_subscription` | — | 不适用（未移植订阅 provider） |

## 4. 前端（frontend-dev）

| 上游 | AIGX 落点 | 动作 |
|---|---|---|
| `ui-shared/maintenance-*`、`tasks-panel.tsx` | `frontend/src/pages/MaintenancePanel.tsx:386`、`types/a2a.ts:337` | 新任务/版本列/checkMode/autoUpdate |
| `ui-islands/tasks-panel.tsx` | `frontend/src/pages/ScheduledTasks.tsx:180` | 不再只过滤 updateCheck |
| `ui-shared/usage-readings.ts`、mobile usage | `frontend/src/pages/AccountSessions.tsx:91`、`types/index.ts:1615` | `modelQuotas` 思考等级合并 |
| `ui-shared/plugins-catalog-status.tsx`（新） | `frontend/src/components/plugins/PluginManagerPage.tsx:1236` | 可选：目录异常提示条 |
| i18n | `frontend/src/i18n/{zh,en}.json` | 补任务/维护/版本字段 |

**并发约束**：工作区存在另一会话未提交的 purge 改动（`src/purge/*`、`PurgeConsole.*`、`routing.ts`、i18n 2 个 key），
本轮一律不得 add/commit；i18n 提交前须确认只暂存本工作流的 hunk。

## 5. 验收

- 后端：`cargo fmt --check`、`cargo clippy --all-targets -- -D warnings`、定向单测（维护任务/调度/凭证 lead/更新 claim/脚本宿主）。
- 前端：`npm run build`、`npm run lint`（max-warnings 0）、`npm run i18n:check`、`npx vitest run`。
- 集成：`static/` 重建零漂移、六处版本号一致。
- 人工：维护页可见新任务与版本字段；插件页可触发目录检查；Usage 合并思考等级后不重复计数。

## 6. 产品边界（2026-11-22 用户明确）

AIGX **不做** Agent2API 的「Agent 配置」页：客户端配置写入、客户端 registry/agents 页、
模型 provider 配置管理、DSH 配置目标选择、预览令牌/客户端接入等一律不移植。
MCP & Skills 的「本机接入」安装路径（skill/mcp 写入客户端目录）不属于该页，保持现状。

## 7. 实施状态（目标版本 1.0.81，2026-11-22）

| 工作流 | 状态 | 落点与说明 |
|---|---|---|
| 维护与调度 | ✅ 已实现 | 维护任务 6→7（新增 `pluginCatalog`），`maintenance` 层新增 `updateCheck`/`pluginCatalog` 执行与 `current_version/latest_version/change_label`，`latest()` 遍历 `ALL_TASKS`，`ScheduledSettings.magpie_plugin_check`（默认 360 分钟），`/check` 统一进维护记录，`refresh_now(force)` + 串行闸门，`check_update_outcomes` 逐包结果。 |
| 脚本宿主 + WASM 收窄 | ✅ 已实现 | `start_*_with_wait` 返回 JoinHandle；`minLeadMs` 线表常量 + `provider.mjs` 取 max(refreshLead, minLeadMs)；`plugin.shutdown` 事件（进程/全局）；`reports.mjs` 通用配额合并/池排序/routeAside；`allowance.mjs` 排除 routingAside；`login-policy.mjs` 通用文件导入凭据；`wasm/manifest.rs` desktop.launch 依赖收窄为 `accounts.cas.v1`（CAPABILITIES 仍 54 项，deepseek-pow 按红线不加）。 |
| 凭证与更新 | ✅ 已实现 | `refresh.rs` 新增 `needs_refresh_within`/`maintenance_lead_secs`（margin=max(provider 窗口, 维护间隔+5 分钟)），手动批量端点与自动维护目标均接入；`update/mod.rs` 拆 `claim_check`/`run_claimed_check` 并写 updateCheck 维护记录（与维护层双记录去重）；`auth_file.rs` 拒绝脚本插件 provider 的原生导出文件；`/api/plugins` 顶层新增 `pluginEvents`（≤50，additive）。 |
| 前端 | ✅ 已实现 | 维护面板渲染 4 个维护任务（版本/检查结果列、checkMode/autoUpdate 只读徽章）、调度页开关过滤扩展到 pluginCatalog、Usage 思考等级窗口合并（`usageWindows.ts`，后端未下发时 no-op）、插件目录异常提示条、zh/en 各 +25 key。 |
| 未做（本轮决策） | — | web_tools 执行链、deepseek_pow/egress UA/Kiro·CodeArts 兑换守卫、chatgpt/codex 订阅 provider、Magpie 桌面专属签名升级、Agent 配置页（§6）。 |

验收补充：任务 id 用 `pluginCatalog`（非上游 `pluginCatalogCheck`），`checkMode` 固定 `follow`、`autoUpdate` 映射 `update.auto_apply`（后端派生，UI 只读）；凭证提前量同时作用于手动批量端点与自动维护目标筛选。
