# 自动维护落地审计（AIGX「自动维护」页实际功能为何「没落地」）

> 审计日期：2026-11-23（UTC+8）
> 触发：用户反馈「**自动维护这一列的实际功能没有落地**」。
> 纪律：**只读，未改动任何代码**（本文件是唯一产物，未提交）。行号基于审计当日工作区，后续改动可能漂移。

---

## 0. 一句话结论（TL;DR）

用户说的「自动维护这一列」= 顶级菜单「**自动维护**」页（`/maintenance`，渲染 `MaintenancePanel`）。
**它根本不是「某一列没落地」，而是整页在线上是死的**——因为**响应信封不匹配**：

- AIGX 后端 `/api/maintenance*`（overview / records / run / retry）与 `/api/scheduled-tasks*`（list / PATCH / run）handler **全部返回扁平 JSON**（如 `{tasks,latest,records,...}`、`{...detail}`、`{...task}`），**没有 `{success, data}` 信封**。
- 前端（从 Agent2API 移植，A2A 的成功响应恒为 `ok_json = {success:true,data}`）**一律读 `res.data` / `res.success`**。
- 结果：`res.data` 恒为 `undefined` → **overview 恒 null**、明细恒取不到、设置保存恒判失败。于是任务概览条每行显示「没有该任务的状态」、执行记录永远「暂无」、点「上次明细」弹窗报「维护记录不存在或已清理」、「维护设置」按钮恒禁用、即便打开保存也永远弹「部分设置可能已保存」。

> 后端**执行逻辑本身是真的**（4 个任务 `run_backend` 都真跑，无 `todo!`/空实现）；坏的是**前后端响应契约**这一层。所以用户在页面上一眼看去「啥都没有 / 点了没反应」，与「接口尚未就绪」的观感一致，但根因不是接口没上线，而是**上线了却没套上 `{success,data}` 信封**。

---

## 1. 根因证据（前后端各一行对不上）

### 1.1 后端：维护 4 个 handler 返回扁平 JSON

`src/api/admin/maintenance.rs`：

| 端点 | 返回写法 | 行 |
|---|---|---|
| `GET /api/maintenance`（overview） | `Ok(Json(json!({"tasks","latest","records","retentionDays","maxRecords"})).into_response())` —— **顶层直出，无 `success`/`data`** | `maintenance.rs:43-50` |
| `GET /api/maintenance/records/{id}` | `Ok(Json(json!(detail)).into_response())` | `maintenance.rs:61` |
| `POST /api/maintenance/run/{task}` | `Ok(Json(json!(detail)).into_response())` | `maintenance.rs:87` |
| `POST /api/maintenance/records/{id}/retry` | `Ok(Json(json!(detail)).into_response())` | `maintenance.rs:112` |

`src/api/admin/scheduled_tasks.rs`（维护设置弹窗走这里）：

| 端点 | 返回写法 | 行 |
|---|---|---|
| `GET /api/scheduled-tasks` | `Ok(Json(scheduled_tasks::list(&state).await).into_response())` → `{tasks:[...]}` 扁平 | `scheduled_tasks.rs:40` |
| `PATCH /api/scheduled-tasks/{id}` | `Ok(Json(task).into_response())` → 任务对象扁平（**无 `success` 字段**，configure 里只塞了 `saved:true`） | `scheduled_tasks.rs:112` + `scheduled_tasks/mod.rs:428` |
| `POST /api/scheduled-tasks/{id}/run` | `{summary, task}` 扁平 | `scheduled_tasks.rs:128-135` |

### 1.2 A2A 参考实现：成功响应恒包 `{success,data}`

`D:\GitHub\Agent2API\desktop-tauri\src-tauri\server\src\server\http.rs:1347`：

```rust
pub fn ok_json(data: Value) -> Response {
    Json(json!({ "success": true, "data": data })).into_response()
}
```

A2A 的 `api/maintenance.rs` overview 用 `ok_json(value)`（包 data），账号页 `handle_account_balance` 也用 `{success:true,data}`。**AIGX 移植时保留了前端 `.data` 读取，却没给这几个 handler 套 `ok_json`。**

### 1.3 前端：恒读 `res.data` / `res.success`

`frontend/src/components/MaintenancePanel.tsx`：

| 位置 | 读取 | 行 |
|---|---|---|
| overview 加载 | `setOverview(res?.data ?? null)` | `:273` |
| 单轮明细 | `const data = res?.data ?? null; if (!data) throw new Error('维护记录不存在或已清理')` | `:331-333` |
| 立即执行/重试 | `const d = res?.data;` | `:378` |
| 维护设置保存 | `if (!res?.success) failed += 1;` | `:1123` |

`request()`（`frontend/src/api/client.ts:119`）返回的是**解析后的原始 JSON**，不会替你再包/剥 `data`。所以扁平响应下 `res.data === undefined`。

### 1.4 症状链（为什么看起来「整页没落地」）

1. `load()` 拿到 `{tasks,...}` 但取 `res.data` → `overview=null`（`notReady` 只在 **404** 时才真，见 §1.5）→ 面板渲染成「空壳」：
   - 概览条 4 行：`overview?.tasks?.find(...)` 恒 null → 每行「**没有该任务的状态**」、无 lastRun、无「上次明细」按钮；
   - 执行记录：`overview?.records?.items ?? []` → 空 →「**暂无维护执行记录**」；
   - 「维护设置」按钮：`disabled={loading || !overview?.tasks?.length}` → **恒禁用**（进不去设置）。
2. 点「立即执行」：`api.runMaintenance` 拿到扁平 detail，`res.data` 为 undefined → 不打开新明细、不弹成功 toast（`execute` `:378-394`）。
3. 点「上次明细」/记录「明细」：`fetchDetail` → `res.data` undefined → 抛「**维护记录不存在或已清理**」（`fetchDetail` `:331-332`）。
4. 维护设置（若能进）：`PATCH` 扁平 task 无 `success` → 每次 `!res.success` 判失败 → **恒弹「部分设置可能已保存，请核对后重试」**，即便写盘成功。

### 1.5 关于 `MaintenancePanel.tsx:446`「维护任务接口尚未就绪」

这句**不是误报触发点，也不会在线上显示**——它只在 `notReady===true` 时渲染，而 `notReady` 仅当请求抛错且消息含 `/404|not found/` 时置真（`:279-280`）。当前后端**路由已挂载**（`main.rs:3522` merge `admin::maintenance::router()`），返回 200，所以 `notReady` 恒 false。
=> 用户看到的**不是**这句「未就绪」文案，而是「接口其实在、但页面空壳」。这句 fallback 文案本身容易误导排查方向（让人以为「后端没上线」），建议修契约时一并删掉或改写。

---

## 2. 逐任务核对：后端 `run_backend` 是真执行还是 stub

调度链：`scheduled_tasks::spawn` 循环 → 到点 → `run_backend(state,task,manual)`（`src/scheduled_tasks/mod.rs:458`）→ 对 4 个后端任务统一委托 `crate::maintenance::execute(Request::full(task_id,manual))`（`mod.rs:462-470`），真正的活在各任务分支里。

`grep todo!|unimplemented!` 于 `src/maintenance`、`src/scheduled_tasks`：**零命中**。

| 任务 id | 名称 | Runner | `run_backend` 实际做什么 | 判定 |
|---|---|---|---|---|
| `credentialMaintenance` | 凭证自动维护 | Backend | `complete_prepared`→`select_accounts` 选临期/点名账号→逐账号 `account_session::refresh::refresh_one`，一条账号一条明细（成功/跳过/失败+invalid/forced/expiresBefore-After） | **OK（真执行）** `maintenance/mod.rs:528-586` |
| `modelRefresh` | 模型目录刷新 | Backend | `scheduled_tasks::model_refresh_work`→models.dev 刷新 + 逐启用 openai_compat 渠道拉 `/models`+目录对账+悬空映射清理，逐渠道一条明细 | **OK（真执行）** `mod.rs:587-611` + `scheduled_tasks/mod.rs:526-797` |
| `updateCheck` | 软件版本检查 | Backend | `complete_update_check`→`state.update.check(_scheduled)` 查 GitHub，写一条明细；`auto_apply` 时触发自动应用 | **OK（真执行）** `maintenance/mod.rs:312-386` |
| `pluginCatalog` | 社区插件目录检查 | Backend | `complete_plugin_catalog`→`catalog.refresh_now(true)` + `check_update_outcomes` 逐包更新检查，逐包一条明细 | **OK（真执行）** `maintenance/mod.rs:391-495` |
| `logsAutoRefresh` | 日志页自动刷新 | Frontend | 定时器长在页面（`PageAutoRefreshControl`），后端只存开关/间隔 | 后端不执行（设计如此） |
| `requestsAutoRefresh` | 请求日志页自动刷新 | Frontend | 同上 | 后端不执行（设计如此） |
| `reportAutoRefresh` | 报表自动刷新 | Frontend | 同上 | 后端不执行（设计如此） |

**结论：后端 7 个任务里 4 个 backend 任务全部真执行，无 stub；3 个 frontend 任务本就该由页面自己跑。所以「没落地」不在后端执行层。**

> 附：`memory_refresh_work` 里联动做了「目录对账 / 悬空映射清理 / models.dev 关闭即跳过」等，属于实打实的工作（不是占位）。

---

## 3. 逐列核对：overview / records / 明细 的字段映射

先重申：**字段名本身是对齐的**——坏的只有「信封」这一层。下表是「假如信封修好后各列是否还有缺口」的结论。

### 3.1 overview 顶层字段（`GET /api/maintenance`）vs 前端消费

| 后端字段（`maintenance.rs:43-50`） | 前端消费 | 判定 |
|---|---|---|
| `tasks: [taskJson...]` | `overview.tasks.find(id)`（`:478`） | **OK**（任务对象含 id/label/runner/enabled/interval/min/max/nextRunAt/lastRunAt…*`task_json` `scheduled_tasks/mod.rs:203-246`*） |
| `latest: {taskId: run}` | `latestByTask[taskId]`（`:438,479`） | **OK**（`storage::latest` 返回按 taskId 的 map，`storage.rs:406-415`） |
| `records: {items,total,offset,limit}` | `overview.records.items` / `.total`（`:433-435`） | **OK**（`storage::list`，`storage.rs:332`） |
| `retentionDays` / `maxRecords` | 页脚保留期说明 | **OK** |

### 3.2 执行记录表列（`MaintenancePanel.tsx:575-590`）vs `Run` 字段

| 列 | 读字段 | 判定 |
|---|---|---|
| 开始时间 / 任务 / 来源 / 状态 | `startedAt`/`taskId`/`source`/`status` | OK |
| 刷新·跳过·失败·耗时 | `refreshed`/`skipped`/`failed`/`durationMs` | OK |
| 失败重试标记 | `retryOf` | OK |
| 操作（明细/重试） | `id`/`status` | OK |

### 3.3 明细弹窗逐项列（`MaintenancePanel.tsx:855-880`）vs `Item` 字段

| 列（随任务标题变化） | 读字段 | 判定 |
|---|---|---|
| 提供商 / 检查对象 / 插件 | `providerLabel`/`provider` | OK |
| 账号 / 更新通道 / 当前版本 | `accountName`/`accountId`/`currentVersion` | OK |
| 结果 | `status` | OK |
| 凭证变化 / 模型目录 / 检查结果 | `itemChange`（count/credentialChanged/changeLabel） | OK |
| 说明 / 耗时 / 操作 | `message`/`durationMs`/单条重试 | OK |
| 单条快照（点行） | `expiresBefore/After`、`currentVersion/latestVersion` 等 | OK |

**=> 列/字段层面**没有缺失；**唯一阻塞就是 §1 的信封**。修好信封后这一页立刻可用。

---

## 4. 与 A2A 的差距总结

| 维度 | A2A | AIGX 现状 | 判定 |
|---|---|---|---|
| 成功响应信封 | `ok_json={success:true,data}` | 维护/定时任务 handler 扁平（无信封） | **DIFF（=根因）** |
| 任务集 | 维护 4 类（凭证/模型/软件/社区插件）+ 3 前端刷新 | 同 4 类 + 3 前端刷新 | OK |
| run_backend 执行 | 真执行 | 真执行 | OK |
| overview 字段 | tasks/latest/records/retentionDays/maxRecords | 同 | OK |
| 前端消费 | `res.data` / `res.success` | 同（但后端没给） | **DIFF（=根因）** |
| OpenAPI 描述 | —— | `openapi.rs:607-615` 仍写「凭证维护 / 模型目录刷新」两类，实际已 4 类 | 小 DIFF（文档陈旧，非功能） |
| 446「接口尚未就绪」文案 | 无 | 有（404 fallback，线上不触发但误导） | 小 DIFF（建议删/改写） |

---

## 5. 修法（交实施，二选一，推荐 A）

### 方案 A（推荐）：后端补 `{success:true,data}` 信封，对齐 A2A
把这 7 个 handler 的成功分支统一包一层 `{ "success": true, "data": <原 payload> }`（错误分支已是 `{success:false,error}`，语义一致）：

- `src/api/admin/maintenance.rs`：`overview`(:43-50)、`record_detail`(:61)、`run_task`(:87)、`retry_record`(:112)。
- `src/api/admin/scheduled_tasks.rs`：`list_handler`(:40)、`configure_handler`(:112)、`run_handler`(:128-135)。

优点：前端零改、与 A2A 契约一致、一次性把「自动维护页 + 页面自动刷新控件 + 更新检查计划」一起救活（三者都被同一 scheduled-tasks/maintenance 信封问题拖死，见 §6）。
注意：这是**会改变现有响应体**的改动——需确认没有别的调用方依赖扁平形态（前端全部按 `.data` 读，改后正好对上；`scheduled_tasks` 若被其它后端/工具直接消费需核查）。

### 方案 B：前端改读扁平
把 `MaintenancePanel.tsx` 的 `res?.data`/`res?.success` 改为读扁平（`res` 直接用），`PageAutoRefreshControl`/`UpdatePanel` 同理。缺点：与全站其它 `{success,data}` 端点口径不一致，后续易再错；工作量大且分散。

---

## 6. 连带（同一根因）——不在本条需求内，但建议一并修

`GET /api/scheduled-tasks` 扁平、但被这些前端按 `.data.tasks` 读，**同样恒取不到**：

- `frontend/src/components/PageAutoRefreshControl.tsx:42-45`：`res?.data?.tasks?.find(...)` → 恒 null → 自动刷新控件不渲染（各页面工具栏的「页面自动刷新」静默失效）。
- `frontend/src/components/UpdatePanel.tsx:142-143`：`res?.data?.tasks?.find(id==='updateCheck')` → 恒 undefined → 「检查计划」区块不显示。

所以：**「页面自动刷新」与「更新检查计划」很可能也一起没落地**（同一信封 bug）。修方案 A 的 scheduled_tasks 三个 handler 时，这三个前端功能同时恢复。

---

## 7. 给 lead 的「没落地清单 + 根因」

- **用户说的「那一列」= 不是一个列，是整页「自动维护」。**
- **根因（唯一、且致命）**：维护/定时任务后端 handler 未套 `{success,data}` 信封，前端按信封读 → overview/明细/成败全 undefined。
- **不需动的地方**：后端 4 个任务的执行逻辑全是真的（凭证/模型/版本/插件都真跑，无 stub）；overview/records/明细 的字段名与前端一一对应；DB 保留期/分页逻辑都在。
- **修法**：优先「方案 A 后端补信封」（7 个 handler，前端零改），一次修好自动维护页 + 自动刷新控件 + 更新检查计划。
- **次要**：`openapi.rs` 维护描述仍是「2 类」（陈旧）；`MaintenancePanel.tsx:446`「接口尚未就绪」fallback 文案误导（线上不会触发），建议连同修复一起删/改写。
