# 账号池功能差距审计：AIGX × Agent2API v3.5.0-beta.7

> ⚠️ **已过时（2026-11-26 复核）**：本文列为「待补」的 4 项（relogin UI、rich filters、
> 服务端分页、导出 v2）**实际均已实现**，不要再据此排期。重新核实后的差距清单见
> [`V351-GAP-AUDIT-2026-11-26.md`](./V351-GAP-AUDIT-2026-11-26.md)。

> 审计日期：2026-11-22
> A2A 真相源：`D:\GitHub\Agent2API` tag `v3.5.0-beta.7`（只读；AGPL，仅审计行为，未拷贝源码）
> AIGX：`D:\GitHub\AIGX`（工作区）
> 约束：只读审计；仅新增本文件；每行两侧均有代码证据（文件:行）；结论只取「已对齐 / 待补 / 不适用」。
> 路径约定：A2A 桌面 UI = `desktop-tauri/ui-islands/src/islands/…`；A2A 服务端 =
> `desktop-tauri/src-tauri/server/src/server/…`；AIGX 前端 = `frontend/src/…`；AIGX 后端 = `src/…`。

## 0. 结论统计

| 结论 | 数量 |
|---|---:|
| 已对齐 | 43 |
| 待补 | 4 |
| 不适用 | 3 |
| 合计 | 50 |

AIGX 账号池在「添加/列表/列/筛选/排序拖拽/分组/隐藏/批量/限制器/用量/签到/代理/实时轮询/分页/导入导出/错误提示/凭证维护调度」上与 A2A 功能面基本齐平；
实质缺口集中在 **显式重登交互、更丰富的筛选维度、服务端分页、全量导出格式版本**（见 §2 待补清单）；
ZCode 专属套餐 UI 经复核判为「不适用（后续）」，理由见 §1 L 节。

## 1. 逐功能差距表

### A. 添加账号

| 功能项 | A2A 行为（文件:行） | AIGX 现状（文件:行） | 差距 | 结论 | 工作量 |
|---|---|---|---|---|---|
| 登录流（OAuth/SMS/guest/设备码/回调） | `desktop-tauri/ui-islands/src/islands/add-dynamic-provider.tsx:225-420,507-579` | `frontend/src/components/AddAccountDialog.tsx:489-570`（OAuth/SMS）`1362-1497`（guest/oauth/file） | 实现路径不同，能力等价 | 已对齐 | — |
| 粘贴凭证 | `add-account-configs.ts:173-179` + `add-provider-blocks.tsx:133-177` | `AddAccountDialog.tsx:603-635` | 无 | 已对齐 | — |
| 动态 provider 专属表单 | `add-account-configs.ts:89-133` + `add-account-dynamic-fields.ts:90-250` | `AddAccountDialog.tsx:1315-1324` + `frontend/src/lib/addAccountConfigs.ts` | 无 | 已对齐 | — |
| 凭证文件导入 | `add-provider-file.tsx:30-151` | `AddAccountDialog.tsx:1362-1439` + `frontend/src/pages/AccountSessions.tsx:1875-1922` | 无 | 已对齐 | — |
| 批量添加/批量导入 | `desktop-tauri/ui-islands/src/islands/add-provider-import.tsx:94-117,176-225`（cc-switch，入口默认禁用 `add-provider-pick.tsx:53-60`）；A2A 服务端批量新增未发现（`api/accounts.rs:645` 仅单建 + `:972` import） | 前端 `AccountSessions.tsx:1117-1155,3233-3235`；后端批量导入 `src/api/admin/account_sessions.rs:1956` | AIGX 直接提供批量导入端点，A2A 仅本地扫描导入 | 已对齐 | — |
| 桌面登录态导入 | `desktop-tauri/src-tauri/server/src/server/api/accounts.rs:161`（desktop import） | `AddAccountDialog.tsx:780-817` + `src/account_session/desktop_import.rs:195` | 无 | 已对齐 | — |

### B. 列表与列

| 功能项 | A2A 行为（文件:行） | AIGX 现状（文件:行） | 差距 | 结论 | 工作量 |
|---|---|---|---|---|---|
| 列定义/顺序/对齐 | `accounts-columns.ts:54-83` | `AccountSessions.tsx:361-395`（13 列） | 无 | 已对齐 | — |
| 列宽拖拽/双击重置 | `accounts-columns.ts:153-173,254-313` | `AccountSessions.tsx:1565-1590` | 无 | 已对齐 | — |
| 显示/隐藏/排序/对齐列（设置面板） | `accounts-page.tsx:205-234` + `accounts-columns.ts:106-144` | `AccountSessions.tsx:3164-3185,1551-1563` | 无 | 已对齐 | — |
| 列设置持久化 | `accounts-columns.ts:254-313`（localStorage） | `AccountSessions.tsx:404-405,1555` | 无 | 已对齐 | — |

### C. 筛选与搜索

| 功能项 | A2A 行为（文件:行） | AIGX 现状（文件:行） | 差距 | 结论 | 工作量 |
|---|---|---|---|---|---|
| provider/状态/限额段筛选 | `accounts-page.tsx:394-454` | `AccountSessions.tsx:3245-3268,3335-3374` | AIGX 状态维度为 全部/启用/停用/冷却 | 已对齐 | — |
| 关键词搜索 | `accounts-page.tsx:423-428` | `AccountSessions.tsx:3245-3268` | 无 | 已对齐 | — |
| 筛选持久化 | `accounts-store.ts:112-121`（搜索不持久化） | `AccountSessions.tsx:431-432,742-745`（搜索不持久化） | 无 | 已对齐 | — |
| 更多筛选维度（relogin/expired/到期范围/用量范围） | `desktop-tauri/src-tauri/server/src/server/core/accounts_list.rs:11-29,142-145`（status/expiry/usage/relogin） | `src/api/admin/account_sessions.rs:985-1031` 仅 channel_id/search/include_hidden | AIGX 缺 relogin/expired、到期与用量范围筛选 | 待补 | S |
| 服务端分页 | `api/accounts_list.rs:16-32` + `core/accounts_list.rs:23-24,176-194` | `src/api/admin/account_sessions.rs:985`（返回全量）；前端客户端分页 `AccountSessions.tsx:2182-2235` | AIGX 无服务端 limit/offset，大账号池整表下发 | 待补 | S |

### D. 排序与拖拽

| 功能项 | A2A 行为（文件:行） | AIGX 现状（文件:行） | 差距 | 结论 | 工作量 |
|---|---|---|---|---|---|
| 每 provider 调度排序规则 | `api/accounts_sort.rs:9,14` + `config/account_sort.rs:70` | `frontend/src/components/AccountSortDialog.tsx:143-296` + `src/account_session/sorting.rs:3963,4062` | 无 | 已对齐 | — |
| 行/队列拖拽排序 | `accounts-queue-drag.ts:219-277` + `accounts-page.tsx:289-359` | `AccountSessions.tsx:3063-3091,1955-1978`（后端 `store.rs:1192,1226`） | 无 | 已对齐 | — |
| 置顶/前移 | `api/accounts.rs:1007,1612` | `AccountSessions.tsx:2016-2057` + `src/api/admin/account_sessions.rs:1138` | 无 | 已对齐 | — |

### E. 分组与组顺序

| 功能项 | A2A 行为（文件:行） | AIGX 现状（文件:行） | 差距 | 结论 | 工作量 |
|---|---|---|---|---|---|
| 按 provider 分组 + 组带统计 | `accounts-domain.ts:1127-1169` + `accounts-panels.tsx:143-292` | `AccountSessions.tsx:3269-3280,3502-3648` | 无 | 已对齐 | — |
| 组设置/组排序规则 | `accounts-group-settings.tsx:46-97,224-375` | `AccountSortDialog.tsx:184-296` | AIGX 以排序弹窗承载组规则，无同名「组设置」弹窗但能力等价 | 已对齐 | — |
| 组拖拽排序 | `accounts-group-order.tsx:66-255` + `accounts-data.ts:171-183` | `AccountSessions.tsx:1980-2014` | 无 | 已对齐 | — |
| 折叠状态持久化 | `accounts-store.ts:123-131,415-449` | `AccountSessions.tsx:427-428,665-667` | 无 | 已对齐 | — |

### F. 隐藏账号

| 功能项 | A2A 行为（文件:行） | AIGX 现状（文件:行） | 差距 | 结论 | 工作量 |
|---|---|---|---|---|---|
| 隐藏账号提示 + 原因 | `accounts-hidden-notice.tsx:37-141` + `core/account_store/hidden_accounts.rs:10-96` | `AccountSessions.tsx:3129-3146,436-451` + `src/api/admin/account_sessions.rs:1062-1092` | 无 | 已对齐 | — |
| 隐藏原因枚举/动作 | `ui-shared/plugin-hidden.ts:20-72` | `src/api/admin/account_sessions.rs:1092`（pack_disabled/provider_disabled/not_loaded/not_installed） | 无 | 已对齐 | — |

### G. 批量操作

| 功能项 | A2A 行为（文件:行） | AIGX 现状（文件:行） | 差距 | 结论 | 工作量 |
|---|---|---|---|---|---|
| 批量启停/删除 | `api/accounts_batch.rs:39-40` + `accounts-batch-dialog.tsx:25-34` | `frontend/src/components/AccountBatchDialog.tsx:32-48` + `src/api/admin/account_sessions.rs:1748` | 无 | 已对齐 | — |
| 批量刷新（单账号/临期/令牌） | `accounts-batch-refresh.ts:18-36` + `api/accounts.rs:1052,1311` | `AccountSessions.tsx:3187-3194,2954-2969` + `account_sessions.rs:2322,4115` | AIGX 批量弹窗无刷新项，但工具栏有「查询全部余额/刷新临期/逐行令牌刷新」 | 已对齐 | — |
| 批量导入 | `api/accounts.rs:972` + `add-provider-import.tsx:94-117` | `AccountSessions.tsx:3208-3232,1956` + `account_sessions.rs:1956,3314,3595` | 无 | 已对齐 | — |
| 批量导出 | `api/accounts_export.rs:15-54` + `core/account_transfer.rs:151` | `AccountSessions.tsx:3200-3207,1804-1873` + `account_sessions.rs:3519,4361` | 无 | 已对齐 | — |
| 全量导出格式对齐（version/字段） | `core/account_transfer.rs:151`（version 2） | `src/api/admin/account_sessions.rs:3519`（version 1 + `accounts[]`） | AIGX 全量导出为 v1；单文件导出已是 A2A account-file 形状 | 待补 | S |

### H. 限制器

| 功能项 | A2A 行为（文件:行） | AIGX 现状（文件:行） | 差距 | 结论 | 工作量 |
|---|---|---|---|---|---|
| 余额限制/低余额模式 | `accounts-dialog-limiter.tsx:14-108` | `AccountSessions.tsx:4204-4331,4145-4170` | 无 | 已对齐 | — |
| Token/请求规则 | `accounts-limiter-condition.tsx:70-260` | `AccountSessions.tsx:3980-4030` + `frontend/src/components/LimiterGroupEditor.tsx` | 无 | 已对齐 | — |
| 条件组 AND/OR | `accounts-limiter-condition.tsx:55-114` + `accounts-limiter-model.ts` | `LimiterGroupEditor.tsx` + `src/account_session/limiter.rs:26-120` | 无 | 已对齐 | — |
| 按模型限制/清限流 | `accounts-limiter-model.ts:21-144` + `api/accounts.rs:1648` | `AccountSessions.tsx:2538-2570,2743-2747` + `account_sessions.rs:4456` | 无 | 已对齐 | — |
| 限制器状态/详情 | `accounts-limiter-status.tsx:7-18` + `accounts-limit-detail.tsx:124-215` | `AccountSessions.tsx:2792-2828,3652-3713` + `account_sessions.rs:814` | 无 | 已对齐 | — |

### I. 用量窗口与用量任务

| 功能项 | A2A 行为（文件:行） | AIGX 现状（文件:行） | 差距 | 结论 | 工作量 |
|---|---|---|---|---|---|
| 用量窗口展示（多窗口/合并/重置） | `accounts-usage-windows.tsx:58-310` | `AccountSessions.tsx:2748-2786` + `frontend/src/lib/usageWindows.ts` | 无 | 已对齐 | — |
| 用量异步任务（启动/取消/状态） | `accounts-usage-tasks.ts:24-161` + `api/accounts_usage.rs:36-85` | `src/account_session/usage_jobs.rs` + `account_sessions.rs:4182-4239` | 无 | 已对齐 | — |
| 自动查询间隔/重试/超时 | `accounts-dialog-usage.tsx:62-168` | `AccountSessions.tsx:4108-4144` + `account_sessions.rs:4189` | 无 | 已对齐 | — |

### J. 签到 / 新手任务

| 功能项 | A2A 行为（文件:行） | AIGX 现状（文件:行） | 差距 | 结论 | 工作量 |
|---|---|---|---|---|---|
| 账号签到 | `api/accounts.rs:1344` + `core/account_store/checkin_state.rs:47-89` | `AccountSessions.tsx:2925-2937,2073-2087` + `src/account_session/checkin_center.rs:64,230` | 无 | 已对齐 | — |
| 新手/福利任务（onboarding/campaign） | `api/onboarding.rs:128-139` + `accounts-panels.tsx:1466-1471` | `AccountSessions.tsx:4363-4405` + `checkin/center.rs:385`（campaigns shadow 路由 `checkin_schedules.rs:78-82`） | 无 | 已对齐 | — |

### K. 代理

| 功能项 | A2A 行为（文件:行） | AIGX 现状（文件:行） | 差距 | 结论 | 工作量 |
|---|---|---|---|---|---|
| 代理表单/池/轮换/测试 | `accounts-proxy-form.tsx:57-519` + `core/proxies.rs:105,425,600` | `AccountSessions.tsx:2436-2478,3930-3951` + `frontend/src/components/ProxySelector.tsx` | 无 | 已对齐 | — |

### L. ZCode / 套餐

| 功能项 | A2A 行为（文件:行） | AIGX 现状（文件:行） | 差距 | 结论 | 工作量 |
|---|---|---|---|---|---|
| ZCode 套餐/凭据选择 UI（含 captcha 池） | `accounts-zcode-plan.tsx:36-167`；但 CLI 轮询登录、编码套餐凭证换取、活动套餐通道（Anthropic 信封 + 官方三段系统提示词）、`start-plan`、验证码令牌池等已整体搬入其**未随仓库发布的 `agent2api-providers` 插件**（`desktop-tauri/src-tauri/server/src/server/core/providers/zcode/mod.rs:1-25` 自述），参考树只剩地区身份/字段值域，无协议契约 | `AccountSessions.tsx:160-166`（仅 provider 白名单）；`zcode`/`zcode-intl` 为 provider 级 `Protocol::Openai`（`src/account_session/registry.rs:420-428`），无账号级协议覆盖 | 真实分叉需 bridge/adapter 改造（L 量级）；仅落字段、不接数据面属半死功能，故本轮不做。另：`wbZcodeCaptchaPool` 是桌面端**本地**验证码池，网关形态不适用（平台级「验证码令牌池」是另一能力，不搬入账号行） | 不适用（后续） | L |
| 订阅/套餐展示 | `accounts-panels.tsx:491-511` + `core/account_store/zcode_accounts.rs:572-576` | `AccountSessions.tsx:214-277,2626-2643` + `balance.rs:524,704-898` | 无 | 已对齐 | — |

### M. 桌面客户端

| 功能项 | A2A 行为（文件:行） | AIGX 现状（文件:行） | 差距 | 结论 | 工作量 |
|---|---|---|---|---|---|
| 桌面登录态导入 | `api/accounts.rs:161`（desktop import） | `src/account_session/desktop_import.rs:195` + `AddAccountDialog.tsx:780-817` | 无 | 已对齐 | — |
| 桌面客户端启动/恢复 | `accounts-desktop-launch.tsx:19-46` + `accounts-desktop-recovery.tsx:10-53` + `api/plugin_desktop_launch.rs:45-101` | `src/account_session/desktop_launch.rs:34`（模块存在但未接路由，`src/main.rs:67` 仅声明） | AIGX 是网关服务端，无桌面客户端可启动/恢复 | 不适用 | — |

### N. 实时刷新

| 功能项 | A2A 行为（文件:行） | AIGX 现状（文件:行） | 差距 | 结论 | 工作量 |
|---|---|---|---|---|---|
| 账号池实时刷新（轮询） | 轮询：`accounts-usage-tasks.ts:24-45`；用量快照 `api/accounts_usage.rs:85` | 轮询：`AccountSessions.tsx:785-805`（连接数 2s）、`775-783`（冷却倒计时） | 均以轮询实现；A2A 无 accounts SSE（`api/panel_events.rs:46-59` 仅 logs/requests） | 已对齐 | — |
| 账号 SSE 事件流 | `api/panel_events.rs:46-59` 无 accounts 主题 | 仅聊天调试 SSE `frontend/src/api/stream.ts:83-152`，账号池无 | 两侧均无账号 SSE | 不适用 | — |

### O. 分页

| 功能项 | A2A 行为（文件:行） | AIGX 现状（文件:行） | 差距 | 结论 | 工作量 |
|---|---|---|---|---|---|
| 分页控件 + 页大小持久化 | `accounts-list-hook.ts:48-120` + `table-shell.tsx:56,120-162` | `AccountSessions.tsx:2182-2235,433-434` | 客户端分页具备；服务端分页差异见 §C「服务端分页」 | 已对齐 | — |

### P. 错误 / 重登提示

| 功能项 | A2A 行为（文件:行） | AIGX 现状（文件:行） | 差距 | 结论 | 工作量 |
|---|---|---|---|---|---|
| 错误横幅/过期徽章 | `accounts-page.tsx:479-485` + `accounts-authorization.ts:4-7` | `AccountSessions.tsx:3121,2599-2645` | 无 | 已对齐 | — |
| 显式「需要重登」提示/一键重登 | `accounts-authorization.ts:4-7` + `accounts-panels.tsx:1327,1361` | 仅 `relogin_due_at` tooltip `AccountSessions.tsx:2237-2249`（后端已有 `needs_relogin`/`runtime_status` `src/account_session/model.rs:639,761,848`） | 后端具备，前端缺显式重登入口/提示 | 待补 | S |

### Q. 批量刷新调度

| 功能项 | A2A 行为（文件:行） | AIGX 现状（文件:行） | 差距 | 结论 | 工作量 |
|---|---|---|---|---|---|
| 凭证维护定时调度（间隔/提前量/退避） | `core/scheduled_tasks.rs:174-197,445` + `config/types.rs:551-567` + `core/credential_maintenance.rs:63-74` | `src/scheduled_tasks/mod.rs:93` + `src/account_session/refresh.rs:67` + `src/account_session/maintenance.rs:26` | 无 | 已对齐 | — |

## 2. 「待补」清单（供 #36 后端 / #37 前端分工）

> ZCode 专属套餐 UI 经复核改为「不适用（后续）」（证据见 §1 L 节），不再列入下表。

| # | 缺口 | 证据 | 建议归属 | 工作量 |
|---|---|---|---|---|
| 1 | 显式「需要重登」提示 + 一键重登/去处理入口 | A2A `accounts-authorization.ts:4-7`、`accounts-panels.tsx:1327,1361`；AIGX 后端 `model.rs:639,761,848` 已备，前端仅 tooltip `AccountSessions.tsx:2237-2249` | 前端（#37） | S |
| 2 | 更丰富的筛选维度：status=relogin/expired、到期范围、用量范围 | A2A `core/accounts_list.rs:11-29,142-145`；AIGX `account_sessions.rs:985-1031` 仅 channel/search/include_hidden | 后端（#36）+ 前端 | S |
| 3 | 服务端分页（limit/offset）与 ids-only 选择 | A2A `api/accounts_list.rs:16-32`、`core/accounts_list.rs:23-24`；AIGX 列表返回全量 `account_sessions.rs:985` | 后端（#36） | S |
| 4 | 全量导出格式对齐 A2A v2（版本/字段） | A2A `core/account_transfer.rs:151`（version 2）；AIGX `account_sessions.rs:3519`（version 1） | 后端（#36） | S |

## 3. 说明

- 「已对齐」指**能力等价**，不要求实现逐字一致（AIGX 为 Rust/React 独立实现，A2A 代码为 AGPL，仅审计行为未拷贝）。
- 「不适用」三项均因产品形态/上游发布形态差异：AIGX 是服务端网关，无桌面客户端可「启动/恢复」；账号 SSE 上游本身也未提供（两侧都用轮询）；ZCode 账号级协议（`start-plan` / Anthropic 信封 / 官方三段提示词 / 验证码令牌池）已移入上游**未发布**的 `agent2api-providers` 插件（`core/providers/zcode/mod.rs:1-25` 自述），参考树无契约，且 AIGX `zcode`/`zcode-intl` 为 provider 级 `Protocol::Openai`（`registry.rs:420-428`），真实分叉需 bridge/adapter 改造（L，后续）。
- `wbZcodeCaptchaPool` 为桌面端本地验证码池，网关形态不适用；平台级「验证码令牌池」属另一能力，不搬入账号行。
- 证据行号基于审计当日工作区；后续改动可能漂移，复核以实际代码为准。
