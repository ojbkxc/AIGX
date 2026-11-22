# 自动维护全量与设置入口对齐审计（AIGX × Agent2API v3.5.0-beta.7）

> 审计日期：2026-11-22
> 上游真相源：`D:\GitHub\Agent2API` tag `v3.5.0-beta.7`
> AIGX：`D:\GitHub\AIGX`（工作区，含本轮未提交改动）
> 约束：只读审计；不改代码；不 commit；不碰 Agent 配置页；不写 provider 伪装。
> 范围：后端 `ALL_TASKS` 全量、`/api/maintenance*`、维护记录 types/storage、
> `scheduled_tasks` 任务表与页面自动刷新任务、前端维护面板/设置/概览/记录、
> 导航与设置入口。

---

## 0. 结论摘要

上游「自动维护」是**顶级页面**（导航文案 `自动维护`，`ui/app.js:160`），一项独立的
`MaintenancePanel`，把凭证/模型/软件/插件目录/Magpie 五类维护任务的**概览、设置、
执行记录、逐项明细、失败重试**集中在一处；页面自动刷新（日志/请求日志/报表）的设置
**不在**维护页，也不在设置页，而是**每个页面自带的 `page-auto-refresh` 控件**
（`ui-shared/page-auto-refresh.ts` + `page-auto-refresh-controls.tsx`，页面岛消费）。

AIGX 目前把全部六条间隔任务（含前端三条自动刷新）连同维护记录面板一起**嵌在
「系统设置 → 定时任务」分类**里（`frontend/src/pages/Settings.tsx:208,1304`、
`Sidebar.tsx:110`、`App.tsx:252`），没有顶级「自动维护」页；维护面板有概览/记录/明细/
重试，但**没有「维护设置」弹窗**；页面自动刷新的开关/间隔只能在定时任务页改。

| 维度 | 上游 beta.7 | AIGX 现状 | 结论 |
|---|---|---|---|
| 后端维护任务数 | 5（CREDENTIAL/MODEL/UPDATE_CHECK/PLUGIN_CATALOG/MAGPIE_PLUGIN） | 4（CREDENTIAL/MODEL/UPDATE_CHECK/PLUGIN_CATALOG） | 差 1，**合并裁剪**（见 §7） |
| `/api/maintenance*` | overview/detail/run/retry | 同形四端点 | 已对齐 |
| 维护记录 types | Item 含 current/latest/change_label | 已补三字段 | 已对齐 |
| 维护记录 storage latest | 遍历 ALL_TASKS | 遍历 ALL_TASKS | 已对齐 |
| scheduled_tasks 任务表 | 8 条（含插件两条） | 7 条（插件合并为一条） | 部分对齐 |
| 页面自动刷新设置 | 每页自带控件 | 只能在定时任务页改 | **需移植** |
| 维护设置弹窗 | `maintenance-settings.tsx` | 无（仅面板） | **需移植** |
| 维护顶级页/导航 | `自动维护` 顶级页 | 嵌在设置分类 | **需移植** |

---

## 1. 后端 `ALL_TASKS` 逐项状态

| 任务 | 上游 | AIGX 落点 | 状态 |
|---|---|---|---|
| 凭证维护 `credentialMaintenance` | `core/maintenance/mod.rs` + `account_session` | `src/maintenance/mod.rs:41`、`src/account_session/refresh.rs`（含维护提前量 `maintenance_lead_secs`） | 已对齐 |
| 模型目录 `modelRefresh` | `model_meta::refresh` | `src/maintenance/mod.rs` MODEL 分支、`scheduled_tasks::model_refresh_work` | 已对齐 |
| 软件版本 `updateCheck` | `core/maintenance/tasks.rs::complete_update_check` → `UpdateManager::run_claimed_check` | `src/maintenance/mod.rs:312 complete_update_check`（委托 `UpdateManager::check/check_scheduled`，不重复占租约） | 已对齐（实现方式不同：见备注） |
| 社区插件目录 `pluginCatalogCheck` | `plugin_host::catalog::run_claimed_refresh`（原生插件目录） | 合并进 `pluginCatalog`，刷新脚本目录（`src/maintenance/mod.rs:391 complete_plugin_catalog`） | 部分对齐（合并裁剪） |
| Magpie 插件 `magpiePluginCheck` | `core/maintenance/tasks.rs::complete_magpie_plugin`（脚本目录 + 更新检查 + 签名自动升级） | 合并进 `pluginCatalog`；**签名自动升级不移植** | 部分对齐（裁剪） |

常量对照：上游 `ALL_TASKS: [&str;5]`（`core/maintenance/mod.rs:24`） vs
AIGX `ALL_TASKS: [&str;4] = [CREDENTIAL, MODEL, UPDATE_CHECK, PLUGIN_CATALOG]`
（`src/maintenance/mod.rs:48`）。

**备注（updateCheck 实现差异）**：上游让维护模块 `claim_check` 并把 `Recorder` 交给
`run_claimed_check`，由维护侧写记录；AIGX 的 `UpdateManager::check_with_mode` 自己
claim/finish 同一排期键，故维护侧**不再占租约**，只把结果写一条记录，避免与更新管理器
撞成「已在跑」。效果等价（不重复记录由 credupdate-dev 的双记录去重保证）。

---

## 2. `/api/maintenance*` 接口

| 端点 | 上游 `api/maintenance.rs` | AIGX `src/api/admin/maintenance.rs` | 状态 |
|---|---|---|---|
| `GET /api/maintenance` | `tasks_by_ids(ALL_TASKS)` + latest + records | `for task_id in maintenance::ALL_TASKS`（`:40`） + latest + records | 已对齐 |
| `GET /api/maintenance/records/{id}` | `storage::detail` | 同 | 已对齐 |
| `POST /api/maintenance/run/{task}` | `ALL_TASKS.contains` 校验 | `is_maintenance_task`（4 项）校验 | 已对齐 |
| `POST /api/maintenance/records/{id}/retry` | `retry(..., itemId)` | `retry_async` | 已对齐 |

路由挂载：`src/main.rs:3533` `.merge(admin::maintenance::router())`。

**缺口**：上游 `overview` 返回 5 条任务；AIGX 返回 4 条。若 t8 前端按上游 `MAINTENANCE_TASKS`
（5 个 id：含 `pluginCatalogCheck` 与 `magpiePluginCheck`）渲染，会出现 2 个 AIGX 不存在的
id。**必须按 AIGX 的 4 个 id 对齐**（见 §9）。

---

## 3. 维护记录 types / storage

| 项 | 上游 | AIGX | 状态 |
|---|---|---|---|
| `Item.current_version/latest_version/change_label` | `core/maintenance/types.rs:30-35` | `src/maintenance/types.rs`（已补，`skip_serializing_if=None`） | 已对齐 |
| `item()` 映射 | 同上 | `src/maintenance/types.rs` `item()` 读 `currentVersion/latestVersion/changeLabel` | 已对齐 |
| `latest()` 遍历 | `storage.rs` `for task_id in ALL_TASKS` | `src/maintenance/storage.rs` `for task_id in super::ALL_TASKS` | 已对齐 |
| 过滤/分页/关键词 | SQL 子句 | FileStore 内存过滤（`run_matches`） | 已对齐（实现不同） |
| 中断恢复 | 心跳 180s | FileStore 心跳同阈值 | 已对齐 |

上游 `report()` 与 AIGX `report()` 均未覆盖新增三字段（上游亦然）；如需前端从 report
读取版本，应在后端补入——非本轮阻塞项。

---

## 4. `scheduled_tasks` 任务表与页面自动刷新

上游 `core/scheduled_tasks.rs` 任务表 8 条：
`credentialMaintenance / modelRefresh / updateCheck / pluginCatalogCheck / magpiePluginCheck /
logsAutoRefresh / requestsAutoRefresh / reportAutoRefresh`。
其中 `pluginCatalogCheck` 由 `plugin_host::catalog::spawn` 独立驱动（调度循环 spawn 时
**显式排除**它），`magpiePluginCheck` 在调度循环内执行。

AIGX `src/scheduled_tasks/mod.rs:91` 任务表 7 条：
`credentialMaintenance / modelRefresh / updateCheck / pluginCatalog / logsAutoRefresh /
requestsAutoRefresh / reportAutoRefresh`。
`pluginCatalog` 在调度循环内执行（AIGX 无独立原生目录循环），间隔读
`ScheduledSettings.magpie_plugin_check`（默认 360 分钟），任务 JSON 附
`checkMode:"follow"`、`autoUpdate:update.auto_apply`（`task_json` 内）。

| 子项 | 上游 | AIGX | 状态 |
|---|---|---|---|
| 任务表条数 | 8 | 7 | 合并裁剪（插件两合一） |
| pluginCatalog 读取插件设置 | `plugin_host::settings`（checkMode/autoUpdate/channels） | 从 `config.update` 投影（checkMode 恒 follow） | 部分对齐 |
| updateCheck 状态键 | `update::global_check_key()` | `update::check_key(&config.update)`（`:state_key`） | 已对齐（同口径） |
| 前端刷新任务 unit=seconds | 是 | 是 | 已对齐 |
| 配置字段 `magpiePluginCheck` | `config/types.rs:497,558` | `ScheduledSettings.magpie_plugin_check`（serde `magpiePluginCheck`，默认 360） | 已对齐 |

**页面自动刷新任务的上游处理**：上游把 `logsAutoRefresh/requestsAutoRefresh/reportAutoRefresh`
仍注册在 `scheduled_tasks` 里（作为前端 runner），但**设置入口挪到各页面自身**：
`ui-shared/page-auto-refresh.ts`（读写 `scheduled-tasks` 里对应任务的 enabled/interval）、
`page-auto-refresh-controls.tsx`（开关 + 秒数输入），页面岛在挂载时消费
（`ui-islands/.../logs-panel.tsx:37,46`）。设置页不再出现「定时任务」。

AIGX 目前：`useAutoRefresh.ts` 只**读**配置（`Logs.tsx:360`、`Dashboard.tsx:121` 消费），
开关/间隔只能在 `ScheduledTasks.tsx` 的「前端自动刷新」卡片里改（`:305-309`）。

---

## 5. 前端维护面板组件

| 上游文件 | 职责 | AIGX 对应 | 状态 |
|---|---|---|---|
| `ui-shared/maintenance-panel.tsx` | 页壳：页头 + 概览 + 记录 + 设置弹窗入口 | `frontend/src/components/MaintenancePanel.tsx:224`（概览+记录+明细+重试；**无设置弹窗/无独立页头**） | 部分对齐 |
| `maintenance-summary.tsx` | 每任务一行：状态/上次/计数/计划/立即执行 | 面板内概览段（`:462-511` 立即执行、nextRunAt 等） | 已对齐 |
| `maintenance-records.tsx` | 筛选条 + 表格 + 翻页 + 明细弹窗 | 面板内记录表（`:553-620`） | 已对齐 |
| `maintenance-details.tsx` | 明细弹窗 + 单条快照 + 逐项重试 | 面板内明细弹窗 + 版本列（`:69-77,945-975`） | 已对齐 |
| `maintenance-settings.tsx` | **维护设置弹窗**（开关/间隔/checkMode/autoUpdate） | **缺失** | **需移植** |
| `maintenance-types.ts` | 5 任务 id 的 TS 契约 | `frontend/src/types/a2a.ts:316+`（4 任务 id） | 部分对齐 |
| `maintenance-format/shared/css` | 标签/格式化/共享组件 | 面板内联函数 | 已对齐（内联实现） |

AIGX `MaintenancePanel` 注释仍写「两类维护任务」（`:5,221`），实际已渲染四类（凭证/模型/
软件/社区插件）；文案需随 t8 更新。

---

## 6. 导航与设置入口

| 项 | 上游 | AIGX | 状态 |
|---|---|---|---|
| 顶级导航 | `自动维护`（`ui/app.js:160`，page=`tasks`） | 无顶级入口；`/scheduled-tasks` 重定向到 `/settings?tab=scheduled`（`App.tsx:252`） | **需移植** |
| 设置页分类 | 无「定时任务」（`settings-page.tsx` 搜不到） | `Settings.tsx:208` 分类 + `:1304` 渲染 `ScheduledTasks` | **需下掉** |
| 维护设置入口 | 维护页头「维护设置」按钮 → 弹窗 | 无 | **需移植** |
| 更新检查计划入口 | 维护设置 + 更新页 | `UpdatePanel.tsx:552`「检查计划」（读 `scheduled_tasks.updateCheck`） | 已对齐（保留） |

---

## 7. MAGPIE_PLUGIN / 兼容插件检查移植评估

**上游两套插件目录系统：**
- `PLUGIN_CATALOG = "pluginCatalogCheck"` → `plugin_host::catalog`（**原生/WASM 插件**目录，
  由 `plugin_host::settings` 的 autoCheck/interval/checkMode/autoUpdate/channels 驱动，
  `plugin_host::catalog::spawn` 独立循环）。
- `MAGPIE_PLUGIN = "magpiePluginCheck"` → `plugin_host::script::*`（**Magpie 脚本插件**目录 +
  逐包更新检查 + 在 auto_update 时**签名自动升级**，`maintenance/tasks.rs::complete_magpie_plugin`）。

**AIGX 现实：** 只有一个目录——`src/plugin_host/script/catalog/*`（脚本/Magpie 目录），
没有上游那种由 `plugin_host::settings` 驱动的原生目录刷新循环；`plugin_settings_view`
（原生插件页）只是把 `update_check` 投影成 follow。

**可行裁剪方案（推荐，已按此实现 task #1）：**
- 只保留**一个**维护任务 `pluginCatalog`，承担「刷新脚本目录 + 逐包更新检查 + 落在维护
  记录」；间隔读 `ScheduledSettings.magpie_plugin_check`（360 分钟）。
- **不移植**签名自动升级（`apply_automatic_tracked` / 签名校验升级）：仅提示「可更新」，
  升级仍需运维在插件页手动执行。符合「Magpie 桌面专属签名升级不移植」红线。
- **不新增** `pluginCatalogCheck` 独立任务（AIGX 无对应原生目录，新增即死任务）。
- 逐包结果用 `operations::check_update_outcomes -> Vec<CheckOutcome>`
  （`src/plugin_host/script/operations.rs:504`），`CheckOutcome` 含
  id/name/current/latest/available/error/source_label；维护记录把它映射成
  `Item{currentVersion, latestVersion, changeLabel}`。
- 前端如需保持与上游字面 id 兼容，可在 t8 **只展示 4 个 id**；若必须显示「Magpie」独立行，
  需 t9 先决定是否把脚本目录检查再拆回一条独立任务（代价：与 pluginCatalog 重复刷新同一目录）。

**结论：** 推荐「合并一条 `pluginCatalog`」；不移植签名自动升级；不引入原生目录死任务。
若产品坚持 5 行概览，t9 需新增仅做脚本目录检查的 `magpiePluginCheck` 任务并把
`pluginCatalog` 降级为不执行/隐藏——**不推荐**。

---

## 8. 「下掉设置里的定时任务」后页面自动刷新设置落点方案

三条任务：`logsAutoRefresh`（日志页）/ `requestsAutoRefresh`（请求日志页）/
`reportAutoRefresh`（仪表盘报表）。它们本质是「页面可见才跑的本地轮询」，后端只存开关+间隔。

### 方案 A（推荐，对齐上游）：每页自带「页面自动刷新」控件
- 新建 `frontend/src/components/PageAutoRefreshControl.tsx`（对齐 `page-auto-refresh-controls.tsx`）：
  开关 + 秒数输入（1–600s），读写 `GET/PATCH /api/scheduled-tasks/{id}`。
- 放置：
  - 日志页（`Logs.tsx`）工具栏 → 管 `logsAutoRefresh`
  - 请求日志页（同 `Logs.tsx` 第二 Tab/请求面板）→ 管 `requestsAutoRefresh`
  - 仪表盘（`Dashboard.tsx`）工具栏 → 管 `reportAutoRefresh`
- `useAutoRefresh.ts` 已能读配置；控件保存后页面下次重读即生效（最多滞后一个周期，
  与现有行为一致）。**无需后端改动**。
- 优点：与上游一致、设置就近、设置页彻底瘦身。

### 方案 B（省事）：自动维护页新增「页面自动刷新」区
- 在维护页（新顶级页或嵌入面板）底部加一组三张卡片（复用现有 `renderTask` 的前端分支）。
- 优点：只在一个页面改；缺点：与上游不一致、用户要跳页。
- 适合 t8 时间紧时的过渡。

**推荐 A**；B 作为退路。无论 A/B，**设置页 `scheduled` 分类都应删除**
（`Settings.tsx:208,1304`、`Sidebar.tsx:110,117` 文案、`App.tsx:252` 重定向改为新页）。
`updateCheck` 的入口保持「更新页 → 检查计划」（`UpdatePanel.tsx:552`），维护设置弹窗里也可
再暴露一份（同源 `scheduled_tasks.updateCheck`，与上游一致）。

---

## 9. t8（前端）可执行清单

1. **顶级「自动维护」页**
   - 新增路由 `/maintenance`（或复用 `/scheduled-tasks`），侧栏「管理」组加入口，文案「自动维护」。
   - `App.tsx:252` 将 `/scheduled-tasks` 旧链接重定向到新页（防书签失效）。
   - 渲染 `MaintenancePanel`（去掉 `embedded`，启用自带页头）。
   - 从 `Settings.tsx:208,1304` 删除「定时任务」分类；更新 `Sidebar.tsx:110,117` 注释。
2. **维护设置弹窗**（移植 `maintenance-settings.tsx`）
   - 概览条上方加「维护设置」按钮 → 弹窗，按 4 个 AIGX 任务 id 渲染开关 + 间隔；
     保存走 `PATCH /api/scheduled-tasks/{id}`（`{enabled?, interval?}`）。
   - `pluginCatalog` 行显示 checkMode=follow（跟随软件计划）+ autoUpdate（映射 update.auto_apply）；
     由于 AIGX 后端暂只接受 enabled/interval，**如要编辑 checkMode/autoUpdate 需 t9 先扩 PATCH**，
     否则只读展示。
3. **页面自动刷新控件**（§8 方案 A）：三个页面各加 `PageAutoRefreshControl`，分别读写三条
   frontend 任务；`useAutoRefresh` 保持读取口径。
4. **任务 id 对齐**：维护面板/概览只认 AIGX 4 个 id
   （`credentialMaintenance/modelRefresh/updateCheck/pluginCatalog`）；
   `frontend/src/types/a2a.ts:316+` 的 `MAINTENANCE_TASKS` 保持一致，**不要**引入
   `pluginCatalogCheck/magpiePluginCheck`。
5. **文案/注释**：`MaintenancePanel.tsx:5,221`「两类维护任务」→「四类」；
   `ScheduledTasks.tsx:178-185` 的 `maintenanceManaged` 集合在设置页下掉后整段可删。
6. **i18n**：补「自动维护 / 维护设置 / 页面自动刷新 / 当前版本 / 最新版本 / 发现新版本」等
   zh+en 键（注意与 purge 并发改动的暂存边界）。

## 10. t9（后端）可执行清单

1. **确认 ALL_TASKS 口径**：保持 4 项（合并裁剪），或按产品决定拆出 `magpiePluginCheck`
   （需同时定义 `pluginCatalog` 的去留，见 §7）。默认保持 4 项。
2. **扩展 PATCH 支持 checkMode/autoUpdate（若 t8 要可编辑）**：
   `src/api/admin/scheduled_tasks.rs` 解析 `checkMode`/`autoUpdate`，`scheduled_tasks::configure`
   对 `pluginCatalog` 落 `update.auto_apply`（checkMode 因 AIGX 无独立通道恒 follow）。
   当前 `configure_handler` 只收 `enabled/interval`——这是 t8 弹窗可编辑的前置。
3. **维护设置保存语义**：上游 `pluginCatalogCheck` 的 follow 模式跳过 interval 校验；
   AIGX 若保留 follow，需在 configure 中对 `pluginCatalog` 接受 `{checkMode:"follow"}` 而不强制间隔。
4. **`report()` 补版本字段**（可选）：若前端从 `report` 读，需把 current/latest/changeLabel 补入。
5. **无需改动**：`/api/maintenance*` 四端点、`latest()`、`Item` 三字段、脚本目录 `refresh_now`
   + `CheckOutcome` 已就绪。
6. **回归**：`cargo test --bin aigx maintenance::` / `scheduled_tasks::` / `plugin_host::script::operations`。

---

## 11. 未决 / 风险

- **任务行数**：AIGX 概览 4 行 vs 上游 5 行，取决于是否坚持 Magpie 独立行（§7）。属产品决策。
- **checkMode/autoUpdate 可编辑**：当前后端 PATCH 只收 enabled/interval；t8 若要照上游弹窗
  做可编辑，需 t9 先扩接口，否则只能只读展示（推荐先只读，避免跨人返工）。
- **页面自动刷新方案 A vs B**：推荐 A（对齐上游），B 为过渡。需 t8 与产品确认。
- **并发边界**：`frontend/src/i18n/*` 存在 purge 会话未暂存改动，t8 提交时按 pathspec 只含本工作流 hunk。
- **Agent 配置页**：上游导航含「Agent 配置」，AIGX 本轮明确不移植，不在本审计范围。

---

## 12. 实施状态（目标版本 1.0.82，2026-11-22）

按本审计落地，`ALL_TASKS` 保持 4 项（pluginCatalog 合并脚本目录检查，不新增
`pluginCatalogCheck` / `magpiePluginCheck` 死任务，不移植签名自动升级）。

**t8 前端（已实现）**
- 新增顶级「自动维护」页 `frontend/src/pages/Maintenance.tsx`（page-header + 非 embedded
  `MaintenancePanel`）；`App.tsx` 增 `/maintenance`（AdminRoute）路由；`/scheduled-tasks`
  与 `/settings?tab=scheduled` 旧入口重定向到 `/maintenance`。
- `Sidebar.tsx` 管理组新增「自动维护」入口（CalendarClock，adminOnly）。
- `Settings.tsx` 删除 `scheduled` 分类（SettingsTab / SETTINGS_CATEGORIES / 渲染区 /
  import 全部移除），保留旧书签重定向。
- `MaintenancePanel.tsx` 新增「维护设置」弹窗：4 个 AIGX 任务 enabled/interval 可编辑；
  `pluginCatalog` 的 `checkMode` 只读「跟随软件」徽章、`autoUpdate` 可编辑（PATCH
  `{autoUpdate}`，落 `update.auto_apply`）；概览文案「两类→四类」。
- 页面自动刷新（§8 方案 A）：新增 `components/PageAutoRefreshControl.tsx`，日志页
  （请求 Tab=requestsAutoRefresh / 审计 Tab=logsAutoRefresh）与仪表盘（reportAutoRefresh）
  工具栏就地读写 `scheduled-tasks`。
- `types/index.ts` `ScheduledTaskPatch` 恢复 `checkMode?/autoUpdate?`；删除
  `pages/ScheduledTasks.tsx`（无残留引用）；i18n zh/en 各 +15（4841/4841）。

**t9 后端（已实现）**
- `scheduled_tasks/types.rs`：`IntervalTaskPatch` 去 `Copy`，增 `check_mode/auto_update`。
- `scheduled_tasks/mod.rs`：新增纯函数 `validate_patch`（checkMode/autoUpdate 仅限
  `pluginCatalog`；checkMode 仅接受 `follow`，大小写不敏感）；`configure` 的 follow
  分支跳过 interval 范围校验且不落 interval，`autoUpdate` 落 `config.update.auto_apply`；
  「无字段」判据含新增两字段。
- `api/admin/scheduled_tasks.rs`：`configure_handler` 解析 `checkMode`（字符串）/`autoUpdate`
  （布尔），类型错误返 400。
- `maintenance/types.rs`：`report()` 明细补 `currentVersion/latestVersion/changeLabel`
  并加正控单测。

**保留的偏离**
- `checkMode` 在 AIGX 无独立原生目录通道，恒 `follow`、前端只读；如需可切换需另立后端通道（本波不做）。
- `/api/maintenance` 概览 4 行（非上游 5 行），Magpie 签名自动升级不移植。
- 页面自动刷新用方案 A（各页工具栏），无需后端改动；`useAutoRefresh` 读取口径不变。
