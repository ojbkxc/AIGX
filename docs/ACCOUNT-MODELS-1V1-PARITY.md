# 账号池 / 模型管理 · 一比一差距审计（AIGX ↔ Agent2API）

> 审计日期：2026-11-23（UTC+8）
> 目标：把 **AIGX 账号池**、**AIGX 模型管理** 与 **Agent2API 桌面端** 做 1:1 差距审计，逐维度列「A2A 有 / AIGX 现状 / 差距 / 证据 file:line」，供前端实施与本轮复核。
> 纪律：只读，未改动任何代码（本文件是唯一产物，未提交）。AGPL 参考树仅比对 UI 结构与字段，**未拷贝任何源码**。
> 证据行号基于审计当日两侧工作区，后续改动可能漂移；不确定项标「待确认」。

## 0. 口径、文件简写与标签

- **A2A 根**：`D:\GitHub\Agent2API`
  - `islands/` = `desktop-tauri/ui-islands/src/islands/`（账号/模型桌面岛）
  - `mobile/` = `desktop-tauri/mobile/src/`（移动端，仅作「分档筛选」参考）
- **AIGX 根**：`D:\GitHub\AIGX`
  - `AS` = `frontend/src/pages/AccountSessions.tsx`（账号池页，5385 行）
  - `MM` = `frontend/src/pages/ModelManage.tsx`（模型管理页，3133 行）
  - `ADD` = `frontend/src/components/AddAccountDialog.tsx`；`ABD` = `AccountBatchDialog.tsx`；`ASD` = `AccountSortDialog.tsx`
  - `MTD` = `ModelTestDialog.tsx`；`MCD` = `ModelCapabilityDialog.tsx`；`MSE` = `components/ui/TableShell.tsx`
- **标签**：`[OK]` 语义等价；`[DIFF]` 两侧都有但口径/字段/格式不同；`[MISSING]` A2A 有而 AIGX 无（或反之，注「AIGX 独有」）；`[待确认]` 需实机/后端契约确认。
- 命名风格：A2A 后端 camelCase，AIGX 后端 snake_case；下表只按语义对齐，不逐处重复命名差异。
- 备注：AIGX 账号池 13 列 / 模型管理 7 列对 A2A 桌面 **无整列语义缺口**（差额集中在「是否该有」与「同名不同口径」），这是前几轮对齐波（SPEC-1.0.46/49、#50/#51）的既有成果；本轮聚焦**仍存的细节差**与**边角缺项**。

---

# 第 1 章 · 账号池

A2A 真相源：`islands/accounts-page.tsx`（页面）、`accounts-columns.ts`（列/宽）、`accounts-panels.tsx`（单元格/分组带）、`accounts-table-row.tsx`、`accounts-list-body.tsx`、`accounts-settings-dialog.tsx` + `accounts-settings-fields.tsx` + 各子对话框。
AIGX：`AS`（单文件承载页面/工具栏/单元格/设置弹窗/批量/导入）+ `ABD`/`ASD`/`ADD`。

## 1.1 字段

### 1.1.1 列表列（名称 / 顺序 / 可切换）

| 维度 | A2A | AIGX 现状 | 判定 | 证据 | 建议 |
|---|---|---|---|---|---|
| 列集与顺序 | pick / order / account / proxy / connections / status / limits / expiry / usage / actions（10 列） | pick / order / name / **provider** / proxy / conn / status / **limiter** / expiry / usage / **rate** / **cooldown** / act（13 列） | **[DIFF]** | A2A `islands/accounts-columns.ts:54-83`；AIGX `AS:388-403` | AIGX 多 provider/rate 两列（列表模式必需 + 后端有成功计数字段）；`limits` 被拆成 `limiter`+`cooldown`。保留分叉并标注 AIGX 特有；分组模式下 provider 列可关（已支持）。 |
| `limits` vs `limiter`+`cooldown` 语义 | 单列「限流」= 综合总览：模型限流 entries + 上游冷却 + 账号级 limiter 命中 + 余额/Token 跳过徽章，点开明细弹层可逐模型清除 | 拆两列：`limiter`=账号级拦截（limiter_state.account_skip / disable），`cooldown`=模型冷却+限制器跳过（点开表内展开面板可清除） | **[DIFF]** | A2A `islands/accounts-panels.tsx:348-394`；AIGX `AS:2952-3028`（limiter）/`AS:3263-3313`（cooldown）/`AS:4277-4336`（展开面板） | 语义已覆盖，但呈现从「单列+弹层」变「两列+行内面板」。若要 1:1，可把 `limiter`+`cooldown` 合并回单「限流」列 + 明细 popover（见 §1.5）。否则记录为有意分叉。 |
| 列可切换 / 锁列 | 全部列经 `wbColSettings` 可显隐/排序/对齐；末列（actions）无宽度把手 | pick/name/status/act 为 `locked`（恒显，仍可拖序），其余可关；列设置走公共层 `tableColumns v2` | **[DIFF]** | A2A `islands/accounts-page.tsx:205-234,621-640`；AIGX `AS:388-403`（lock 标记）`:425-431` | 锁列策略不同：AIGX 锁 4 列（选择/账号/状态/操作）避免误藏关键列。可接受；若严格对齐则放开锁列。 |
| 列宽预算 | 定宽合计 1241（pick47/order84/proxy186/conn56/status80/limits148/expiry132/usage308/actions200）+ account 弹性最小 300 | pick47/order84/name 弹性最小 220/provider128/proxy186/conn56/status80/limiter148/expiry132/usage308/rate64/cooldown128/act200 | **[DIFF]** | A2A `accounts-columns.ts:106-144`；AIGX `AS:388-408` | AIGX 弹性列下限 220 < A2A 300；新增 provider/rate/cooldown/act 后横向更宽。见 §1.2。 |

### 1.1.2 行详情 / 副行（name 单元格与悬停）

| 维度 | A2A | AIGX 现状 | 判定 | 证据 | 建议 |
|---|---|---|---|---|---|
| 主名 / 备注名 | `displayNameOf` 兜底链（nameCustom 优先），未命名「未命名账号」 | `s.name`（脱敏可选），无 displayName 兜底链 | **[DIFF]** | A2A `panels.tsx:1309-1316`；AIGX `AS:2797-2798` | AIGX 直接用 `s.name`；后端无 displayName 链。记录差异。 |
| 邮箱副行 | 有 email 且 ≠ 主名时副行显示 | 仅插件账号 `s.plugin.email` 有副行；原生账号无结构化 email 字段 | **[DIFF]** | A2A `panels.tsx:1331,1356-1360`；AIGX `AS:2626-2627,2793,2800-2804` | 原生账号后端无 email 字段 → 缺副行。需后端补 `email`（或插件公开投影）才能 1:1。 |
| 账号列其余行 | needsRelogin 注记、proxy.error 注记 | `s.api_key`、`s.proxy`、`s.last_error`（截断）、`modelStatusNode`（模型清单状态徽章） | **[DIFF]** | A2A `panels.tsx:1361-1365`；AIGX `AS:2805-2812,2651-2708` | AIGX 多 api_key/proxy/last_error/模型状态，少 needsRelogin（被挪到 status 列）。行更高更密。见 §1.2。 |
| 悬停详情气泡 | Tooltip 逐行：邮箱 / 标识(UID) / 备注名 / 昵称 / 桌面端实时 / 更新于 / 来源 | `title` 多行文本：账号 / 邮箱(插件) / 更新于 / 来源(插件 vs 原生) / 供应商 / 桌面端 | **[DIFF]** | A2A `panels.tsx:1318-1345`；AIGX `AS:2620-2640` | AIGX 有更新于/来源/供应商；缺 标识(identifier)、昵称(nickname)；A2A 无供应商行（家名在分组带）。可补 nickname/identifier（若后端有）。 |
| 脱敏（隐藏账号名） | 表头眼睛按钮切换整列打码，气泡里同样打码 | 表头眼睛按钮，`maskName`；气泡也打码 | **[OK]** | A2A `panels.tsx:1636-1643`（+`accounts-store` namesHidden）；AIGX `AS:4091-4108,196-201` | 一致。 |

### 1.1.3 新增 / 编辑表单字段

> A2A：编辑 = `accounts-settings-dialog.tsx` 六分组弹窗（basic/credential/query/limiter/network/provider）；新增走 `add-account` 流程（不在本次账号表岛屿内）。
> AIGX：新增 = `ADD`（供应商选择 + 高级折叠）；编辑 = `AS` 内五分组弹窗（basic/credential/proxy/limiter/advanced）。

| 字段 | A2A（设置弹窗） | AIGX（编辑弹窗） | AIGX（新增 ADD） | 判定 | 证据 |
|---|---|---|---|---|---|
| 备注名 name | 有（maxLength 100） | 无独立「名称」编辑？——`basic` 区不含 name 输入，名称只能在 ADD 设 | 有 | **[DIFF]** | A2A `accounts-settings-dialog.tsx:605-609`；AIGX 编辑 `AS:4390-4442`（无 name 输入）；ADD `:435-4440?`（name） |
| 账号 ID（只读） | 有 | 无只读回显（列表已有） | — | **[DIFF]** | A2A `:610`；AIGX 无 |
| 启用 enabled/status | Switch | advanced 区 status 下拉（启用/停用） | status 缺省 enabled | **[OK]** | A2A `:611-616`；AIGX `AS:4817-4824` |
| 失败后允许换号 allowRouteSkip | Switch（转发行为） | advanced 复选框 | 无（仅编辑） | **[OK]** | A2A `:619-624`；AIGX `AS:4724-4731` |
| 并发上限 maxConcurrent | Input（0~999） | advanced Input（0=不限）+ 行/⋯快捷 | 有 | **[OK]** | A2A `:625-631`；AIGX `AS:4739-4746,3491-3514`；ADD `:92` |
| 优先级 priority | 不可编辑（拖拽） | advanced Input（数字可编辑）+ 上移/下移/置顶 | 有 | **[DIFF]** | A2A `:645-647`（注释明确靠拖拽）；AIGX `AS:4709-4714,3475-3490` | AIGX 同时暴露数字与拖拽，双入口。 |
| 权重 weight | 无 | advanced Input | 有 | **[AIGX 独有]** | AIGX `AS:4715-4721`；ADD `:91` |
| User-Agent userAgent | 有（预置/自填） | advanced Input | 无（仅编辑） | **[DIFF]** | A2A `:635-644`；AIGX `AS:4732-4738` | AIGX 无预置下拉（A2A 有 presets）。 |
| 余额查询凭证 balanceToken（CatPaw） | 有（密码框 + 清除） | 无此字段 | — | **[MISSING]** | A2A `accounts-settings-fields.tsx:46-66`；AIGX 后端 `AccountSession` 无 balanceToken | 需后端字段 + UI。 |
| API Key / 无需鉴权 | 有（API Key 三态 + noAuth 单选） | credential 区凭证输入 + 「清除现有凭证」复选框 | 有 credential | **[DIFF]** | A2A `accounts-settings-fields.tsx:68-125`；AIGX `AS:4512-4535`（无 noAuth 单选，仅 clear_key） | AIGX 缺「无需鉴权」与「留空保留/替换」的显式三态说明。 |
| 套餐 zcodePlan | Select（Coding/Start Plan） | 无（页面有只读说明） | — | **[DIFF]** | A2A `islands/accounts-zcode-plan.tsx:151-168`；AIGX `AS:3996-4008`（只读 note） | AIGX 待后端 plan_channel 契约。 |
| 用量查询 enabled/间隔/重试/超时 | 「查询设置」段（开关/间隔+单位/整轮时限/重试） | advanced：间隔 Select + 重试 + 超时 Input | 无（仅编辑） | **[DIFF]** | A2A `islands/accounts-dialog-usage.tsx:112-168`；AIGX `AS:4753-4783` | AIGX 间隔只给固定档下拉（A2A 可任意数值+秒/分/时单位）；缺「整轮时限」独立校验文案之外的细节基本齐。 |
| 余额不足处理 lowBalance | 由「限制器」段承担（余额规则） | advanced：mode(缺省/off/skip/disable)+阈值 | 无 | **[OK/DIFF]** | A2A 走 limiter；AIGX `AS:4785-4809` | 模型不同（A2A=限制器余额规则；AIGX=独立 low_balance 字段）。语义覆盖。 |
| 余额查询凭证 balanceQuery（new_api/sensenova/zhipu…） | per-provider 方案凭证字段（密字段只写） | 无 | — | **[MISSING]** | A2A `accounts-settings-dialog.tsx:672-677`；AIGX 前端无 balanceQuery 配置 | 需后端方案 + UI（或明确用 credential.fields 承载）。 |
| 限制器 limiterGroups（enabled/join/action/target/condition） | 条件组（与/或、metric、comparison、threshold、模型范围 account/models） | 条件组（`LimiterGroupEditor`：metric/comparison/threshold/window/sourceId/models）+ 旧扁平规则 | 无（仅编辑） | **[OK]** | A2A `accounts-dialog-limiter.tsx:14-108`+`accounts-limiter-condition.tsx`；AIGX `AS:4577-4704`+`LimiterGroupEditor` | AIGX 条件编辑器为超集（含旧扁平兼容）。 |
| 代理 proxy | 全表单：none/pool/rotate/clash/custom + **测试出口** + Clash 同步 + 系统代理导入 | `ProxySelector`：direct/pool/rotateAll/rotateIds/custom（**无 clash、无测试出口**） | 有代理串 | **[DIFF]** | A2A `accounts-proxy-form.tsx:117-519`；AIGX `components/ProxySelector.tsx:113-204` | AIGX 缺 clash 档与「测试出口」按钮（见 §1.5）。 |
| baseUrl 覆盖 | 有（账号级覆盖 + 改回跟随） | proxy 区「上游地址」 | 有 | **[OK]** | A2A `accounts-settings-fields.tsx:184-198`；AIGX `AS:4562-4567` |
| 提供商共用配置（name/protocol/baseUrl/clientEmulation/删除提供商） | 「连接设置」分组（custom provider） | 无（provider 走 registry，无自定义 provider 体系） | — | **[AIGX 不适用]** | A2A `accounts-settings-fields.tsx:130-228`；AIGX 架构差异 |
| chatPath / 额外请求头 / 抓包导入 | 无（A2A 无此概念） | credential 区：对话路径 + 抓包 headers 解析 + 额外头 | 有 | **[AIGX 独有]** | AIGX `AS:4478-4551` |
| 模型白名单 models | 无（指向模型管理） | advanced：模型白名单逗号分隔 | 有 | **[AIGX 独有]** | AIGX `AS:4747-4752` |
| 到期 expires_at 编辑 | 只读展示（expiry 列） | advanced：凭证到期时间 Input | 有 | **[AIGX 独有]** | AIGX `AS:4810-4816` |

### 1.1.4 状态徽章与图标

| 维度 | A2A | AIGX 现状 | 判定 | 证据 | 建议 |
|---|---|---|---|---|---|
| 状态开关 | Switch（Patch enabled）+ accountTags 徽章 + tokenDisable 红标 | Switch（expired 禁用）+ 状态 badge（启用/凭证已过期/禁用）+ healthTags（未配置凭证/代理异常/Token限额已禁用/需要重登） | **[OK/DIFF]** | A2A `panels.tsx:302-331`；AIGX `AS:2876-2951` | AIGX 健康徽章更全；A2A 的 accountTags 是能力表驱动（如「未配置凭证」「代理不可用」）。语义等价。 |
| 供应商图标 | 分组带 `grp-logo`（img 或首字 monogram） | provider 列图标（`providerIcon`）+ 分组带同款 | **[OK]** | A2A `panels.tsx:248-252`；AIGX `AS:2819-2827,4185-4188` | 一致。 |

## 1.2 空间（列宽 / 密度 / 行高 / 间距 / 分区 / 分组 / 滚动 / 分页 / 工具条）

| 维度 | A2A | AIGX 现状 | 判定 | 证据 | 建议 |
|---|---|---|---|---|---|
| 表格布局 | `table-layout: fixed`，colgroup 逐列写宽，弹性列仅 account（吃剩余） | 同款：`table-layout: fixed` + colgroup，弹性列仅 name | **[OK]** | A2A `accounts-page.tsx:621-640`；AIGX `AS:4062-4068` | 一致（AIGX 弹性下限 220）。 |
| 横向滚动底线 | `tableMinWidth(visibleKeys)` = Σ可见列宽 + 弹性下限 300 | `tableMinWidth` = max(980, Σ可见列宽 + 弹性下限 220) | **[DIFF]** | A2A `accounts-columns.ts:201-216`；AIGX `AS:1850-1859` | 下限值不同（300 vs 220，AIGX 另加 980 地板）。 |
| 行高 / 虚拟滚动 | 行高实测（名字+邮箱+注记）；`@tanstack/react-virtual` 虚拟化，estimateSize 行 96 / 组带 52，overscan 6，连续滚动加载 | **无虚拟化**，普通 map 渲染；name 单元格最多 6 行（name/email/api_key/proxy/last_error/modelStatus）→ 行更高 | **[DIFF]** | A2A `accounts-list-body.tsx:28-184,106-124`；AIGX `AS:3563-3603`（普通 map） | AIGX 行更高且无虚拟化（大量账号时性能/密度劣势）。建议截断 name 副行（api_key/proxy 收进 tooltip）或引入虚拟滚动。 |
| 分组带 | 每 `<tbody class="acct-group">` + `GroupBandRow`：把手→顺位→图标→家名→统计(总/启/禁/限流)→合余额→排序标记→按钮(加账号/设置/⋯) | 同结构：把手→顺位→图标→家名→数量→统计(启/禁/冷却)→余额→折叠箭头→⋯ | **[OK/DIFF]** | A2A `panels.tsx:193-292`；AIGX `AS:4140-4266` | 统计口径不同（A2A「限流」/AIGX「冷却」）；AIGX 组带少「排序规则标记」按钮（在工具栏排序弹窗里）。 |
| 分组默认态 | 默认按 provider 分组（store 恒分组） | 默认 **列表模式**（`LS_GROUPED` 缺省 false），手动勾「分组」 | **[DIFF]** | A2A `accounts-page.tsx:106,145-151`（恒分组）；AIGX `AS:432,881,3759-3770` | 默认观感不同。若 1:1，AIGX 默认开分组。 |
| 分组折叠 | 每分组带可点折叠；折叠态跨启动保留 | 同款（collapsed 集合落 `LS_COLLAPSED`） | **[OK]** | A2A `panels.tsx:228-233`+`accounts-store`；AIGX `AS:1963-1971,4171-4177` | 一致。 |
| 分页 | 默认每页 50；「全部」档走连续滚动（useClientPaging + loadMore）；分组整组落页 | 默认 **「全部」**（usePageSize fallback 'all'）；客户端切片 + `TablePaginationFooter`；分组按页切块 | **[DIFF]** | A2A `accounts-page.tsx:127-139`（defaultSize 50）；AIGX `AS:907-908,2564-2617,4340-4347` | 默认档位不同（all vs 50）；AIGX 无连续滚动加载。 |
| 工具条布局 | 单行 `toolbar`：筛选组（provider/搜索/状态/限额）+ actions（刷新/添加/查询余额/取消）；窄屏筛选收进 ResponsiveSheet | ① 卡片 actions 区 **12 按钮**（列设置/查询全部余额/刷新临期/排序规则/导出/导单文件/导入JSON/账号路由/从本机导入/批量导入/添加账号）；② 独立 `.as-filters` 行 **十余个控件**（provider/搜索/分组/折叠/排序/自动余额/限额段/状态段/仅重登/仅过期/到期段/用量段/到期区间/已用%区间/清除） | **[DIFF]** | A2A `accounts-page.tsx:374-477`；AIGX `AS:3661-3738`（actions）、`AS:3741-3992`（filters） | AIGX 工具条显著更密、占两行且控件多。若严格 1:1，建议把冷门筛选（到期/用量区间）收进 ResponsiveSheet，并精简 actions 区。 |
| 批量栏 | 勾选后出现：全选本页/选择全部筛选结果(N)/取消选择/已选计数/分组汇总/清除筛选/隐藏提示/「调整提供商顺序·未显示账号·折叠·列设置·批量操作」 | 勾选后出现 `.batch-bar`：已选计数/批量启用/批量停用/批量删除/批量操作…/清除选择 | **[DIFF]** | A2A `accounts-page.tsx:488-548`；AIGX `AS:4010-4049` | AIGX 缺「选择全部筛选结果(N)」与「调整提供商顺序/未显示账号」入口（后者见 §1.5 抽屉）；列设置/折叠在卡片 actions 而非批量栏。 |
| 间距/工具条高度 | 紧凑单行、`dispatch` 组间距 | 两段式、按钮多 | **[DIFF]** | 同上 | 随工具条重排一并处理。 |

## 1.3 筛选 / 排序 / 搜索

| 维度 | A2A | AIGX 现状 | 判定 | 证据 | 建议 |
|---|---|---|---|---|---|
| 提供商筛选 | 单选 Combobox（含 0 账号家、带数） | 单选 Combobox（可搜、图标、计数、按 category 分组、插件标） | **[OK]** | A2A `accounts-page.tsx:394-417`；AIGX `AS:601-729,3745-3752` | AIGX 为超集（分类分组 + 插件标）。 |
| 搜索字段 | name / nickname / email / identifier / id | name / channel_name / provider / base_url | **[DIFF]** | A2A `accounts-domain.ts`（matchesFilter 注释 `accounts-page.tsx:420-428`）；AIGX `AS:1785-1797` | AIGX 无 nickname/email/identifier（后端无结构化字段）；多了 base_url/channel。 |
| 状态分段 | all / enabled / disabled（3 档） | all / enabled / disabled / cooling（4 档）+ 独立「仅需重登」「仅已过期」两个开关 | **[OK/超集]** | A2A `accounts-page.tsx:438-443`；AIGX `AS:3846-3864,3867-3884` | AIGX 覆盖更全。 |
| 限额分段 | all / normal / limited | all / normal / limited | **[OK]** | A2A `:448-453`；AIGX `AS:3827-3844` | 一致。 |
| 到期分档 | 桌面无；移动端 all/soon/longlived | all/soon(临期7天)/longlived + 到期日期区间 | **[OK/超集]** | A2A `mobile/.../AccountsPage.tsx`（soon/longlived）；AIGX `AS:3887-3910,3936-3955,1766-1775` | AIGX 超集（桌面无、比移动端多区间）。 |
| 用量分档 | 桌面无；移动端 all/has/unqueried/failed | all/has/unqueried/failed + 已用%区间 | **[OK/超集]** | AIGX `AS:3911-3935,3956-3981,1780-1783` | 同上。 |
| 双级计数 | 状态徽标随 provider+搜索收窄；限额徽标随 status 收窄 | 同款（statusScope/limitScope） | **[OK]** | A2A `accounts-page.tsx:108-111`（注释）+ data；AIGX `AS:1798-1814` | 一致。 |
| 持久化 | provider/status/limit 落盘；搜索不落 | provider/status/limit + relogin/expired/到期/用量分档全部落 `LS_FILTER` | **[OK/超集]** | A2A `accounts-store`；AIGX `AS:998-1024` | AIGX 更全。 |
| 列表排序（展示序） | 服务端排序规则（per-provider credit/balance/added）+ 拖拽；无「展示序」下拉 | 工具栏「列表排序」下拉：调度优先序 / 按到期 / 按添加时间（纯展示，落 `LS_SORTKEY`）+ 服务端排序弹窗 + 拖拽 | **[OK/超集]** | A2A `islands/accounts-group-settings.tsx:46-113`；AIGX `AS:3793-3806,1817-1831,5378-5382` | AIGX 多展示序下拉。 |
| 服务端排序弹窗 | 分组设置弹窗（credit/balance/added 三条规则，可调优先级 + 方向 + 手动兜底 + 启停） | `AccountSortDialog`：按 provider 选，三规则开关+方向，固定顺序（无可调优先级） | **[DIFF]** | A2A `accounts-group-settings.tsx:224-375`；AIGX `ASD:184-284` | AIGX 缺「规则优先级上移/下移」与「手动兜底说明」。 |
| 计数徽标位置 | 分段按钮内计数 | 分段按钮内计数 | **[OK]** | — | 一致。 |

## 1.4 操作（行内 / 批量 / ⋯ 菜单 / 导出）

| 操作 | A2A | AIGX 现状 | 判定 | 证据 | 建议 |
|---|---|---|---|---|---|
| 行操作按钮组 | DesktopLaunch + 余额(查询) + 设置 + ⋯ | 测试 + 目录同步 + 编辑 + 重置冷却 + 领套餐(checkin) + ⋯ | **[DIFF]** | A2A `panels.tsx:1473-1491`；AIGX `AS:3314-3555` | AIGX 多测试/目录同步/领套餐/冷却；A2A 多 desktop launch、余额查询按钮（AIGX 余额走 usage 格/查询全部）。 |
| ⋯ 菜单项 | 取消余额查询 / 禁用·启用 / 设为首选 / 并发上限 / 刷新 Token / 导出账号 / 删除账号 | 刷新 Token / 重新登录 / 设为首选 / 上移一位 / 下移一位 / 并发上限(input+应用) / 福利预览 / 导出该账号 / 删除账号 | **[DIFF]** | A2A `panels.tsx:1505-1555`；AIGX `AS:3421-3550` | AIGX 缺「取消余额查询(单行)」；多 重登/上下移/福利。启停 AIGX 走状态 Switch（不在菜单）。 |
| 单行取消余额查询 | 有（菜单项，usageInflight 时出现） | **无** | **[MISSING]** | A2A `panels.tsx:1518-1519`；AIGX 无 per-row cancel（grep `cancelUsage` 空） | 后端若有 per-id 取消能力，补菜单项。 |
| 单行导出 | 有（导出账号 JSON） | 有（`导出该账号`，前端按 id 过滤全量导出） | **[OK]** | A2A `panels.tsx:1538-1542`；AIGX `AS:3527-3538,2207-2231` | 一致（实现路径不同）。 |
| 整组操作（⋯ on group band） | 全部启用/禁用、批量操作本组、**导出本组账号**、折叠/展开 | 全部启用/停用、折叠/展开 | **[DIFF]** | A2A `panels.tsx:143-180`；AIGX `AS:4228-4264` | AIGX 缺「批量操作本组」「导出本组账号」。 |
| 批量栏（内联） | 启用/停用/删除/批量操作…（经 dialog）+ 选择全部筛选 | 批量启用/批量停用/批量删除/批量操作…/清除选择 | **[DIFF]** | A2A `accounts-page.tsx:488-548`；AIGX `AS:4010-4049` | AIGX 缺「选择全部筛选结果」。 |
| 批量操作弹窗动作 | 启用/禁用/刷新 Token/修改代理/设置并发/查询设置/限制器/删除（8） | enable/disable/set_proxy/set_max_concurrency/query_settings/limiter/delete（7） | **[DIFF]** | A2A `accounts-batch-dialog.tsx:25-34`；AIGX `ABD:32-40` | AIGX 缺「刷新 Token」批量；A2A 缺 AIGX 的「复制自账号模板」（A2A batch 有「复制自账号」下拉 `:279-294`）。 |
| 批量限制器模式 | append/replace/enable/disable/clear（5） | append/replace/enable/disable/clear（5） | **[OK]** | A2A `accounts-batch-dialog.tsx:36-42`；AIGX `ABD:42-48` | 一致。 |
| 导出（全局/分组/单行） | 分组导出（每账号一 JSON 文件）+ 单行 | 全量导出 + 按 provider 导单文件（导单）+ 单行导出 | **[OK/超集]** | A2A `panels.tsx:151,1538`；AIGX `AS:2158-2231,3699-3706` | AIGX 有全量导出；A2A 只有分组/单行。 |
| 刷新临期凭证 / 刷新列表 | 刷新列表；批量刷新 Token | 刷新临期凭证（refresh-expiring）+ 行刷新 Token | **[OK/超集]** | A2A `accounts-page.tsx:458-462`；AIGX `AS:3689-3693,2136-2155` | AIGX 多「一键刷新临期」。 |
| 拖拽排序（组内/整组） | 行首把手 + 分组带把手；自动排序启用时锁定；落整队 | 行拖拽（非分组）+ 组带拖拽；canonical `/accounts/reorder` 失败回退 PATCH | **[OK]** | A2A `accounts-page.tsx:297-359`；AIGX `AS:2313-2372` | 一致（AIGX 分组模式下关行拖）。 |
| 设为首选 / 上移下移 | 设为首选（菜单）；无上移下移菜单项 | 设为首选 + 上移一位/下移一位（`/accounts/:id/move`） | **[OK/超集]** | A2A `panels.tsx:1524-1528`；AIGX `AS:3475-3490,2402-2415` | AIGX 多 move。 |

## 1.5 弹窗 / 抽屉

| 弹窗 | A2A | AIGX 现状 | 判定 | 证据 | 建议 |
|---|---|---|---|---|---|
| 账号设置弹窗 | 六分组侧栏（basic/credential/query/limiter/network/**provider**）+ dirty 跟踪 + 底部保存 | 五分组侧栏（basic/credential/proxy/limiter/**advanced**）+ 底部保存（无 dirty 计数/未保存提示） | **[DIFF]** | A2A `accounts-settings-dialog.tsx:549-723`；AIGX `AS:4374-4838` | AIGX 分组划分不同（合并了 query/network→proxy/advanced，去掉 provider 共用）；缺 dirty-count 与「放弃未保存修改」二次确认。字段差异见 §1.1.3。 |
| 余额 / 额度明细弹层 | 积分明细 popover：逐份积分包（面额/已用/剩余 + 领取→到期）+「已用完/已过期」折叠组 + 到期徽标 + 订阅行 + 合计 | 余额格点击 → popover：`额度桶`(wallets) + `用量窗口`(windows) 两段；无逐份积分包分组、无「已用完/已过期」折叠、无到期徽标 | **[DIFF]** | A2A `accounts-credit-detail.tsx:328-465`；AIGX `AS:3194-3209,2710-2764` | AIGX 明细更简。若 1:1 需补逐份积分包 + spent 折叠 + 到期徽标。 |
| 限流 / 冷却明细 | 「限流」列点开 popover：逐模型（模型名/倒计时/绝对恢复时刻/原因/来源(上游时间|本机估算)/冷却进度线）+ 清除单条 + 全部清除 | 「冷却」列展开插入整行 `<tbody>` 面板：逐模型（冷却剩时 / 限制器跳过 / 清除标记）+ 全部清除；无倒计时/绝对时刻/原因/进度线 | **[DIFF]** | A2A `accounts-limit-detail.tsx:124-214`；AIGX `AS:4277-4336` | AIGX 用整行面板而非 popover；缺 per-model progress / absolute reset / reason。见 §6 数值格式。 |
| 用量窗口（分窗口 + 模型配额） | 专用 `UsageWindowsCell`：窗口组分栏 + 模型配额卡片（tab：模型配额/分组窗口）+ 重置/到期时刻 | 余额格内联窗口条（label + 进度 + 剩N%）；popover 仅有 wallets+windows 文本 | **[DIFF]** | A2A `accounts-usage-windows.tsx:193-310`；AIGX `AS:3219-3257` | AIGX 缺模型配额卡片与 tab 视图。 |
| 代理表单 | 全表单（none/pool/rotate/clash/custom）+ 测试出口 + Clash 同步 + 模系统代理；轮询限测 8 条 | `ProxySelector`（direct/pool/rotateAll/rotateIds/custom），**无 clash / 无测试出口 / 无系统代理导入** | **[DIFF]** | A2A `accounts-proxy-form.tsx:117-519`；AIGX `ProxySelector.tsx:113-204` | 补 clash 档与「测试出口」；系统代理导入可选。 |
| 新增账号 | add-account 流程（多方式：OAuth/粘贴/短信/客户端导入/访客） | `ADD`：供应商卡片选择 + 添加方式（manual/oauth/paste/sms/desktop/guest）+ 高级折叠（base_url/chat_path/priority/weight/concurrency/models/extra_headers/proxy/expires_at） | **[OK]** | A2A add-account（岛屿外）；AIGX `ADD:85-111,1565-1714` | 能力相当。 |
| 批量弹窗 | 见 §1.4（8 动作 + 复制自账号模板 + 逐条回执 + 危险二次确认） | 见 §1.4（7 动作 + 危险需勾选确认 + 逐条回执） | **[DIFF]** | A2A `accounts-batch-dialog.tsx:48-318`；AIGX `ABD:98-435` | AIGX 缺刷新 Token 动作与复制模板。 |
| 分组设置 / 排序规则 | 分组设置弹窗（credit/balance/added + 优先级 + 手动兜底 + 更多设置占位） | `AccountSortDialog`（三规则 + 方向 + 恢复默认） | **[DIFF]** | A2A `accounts-group-settings.tsx:224-375`；AIGX `ASD:184-284` | AIGX 无手动兜底说明与规则优先级调序。 |
| 调整提供商顺序 | 独立弹窗（拖 + 置顶/上移/下移/置底 + 各家账号数） | 无独立弹窗；分组带拖拽即可整组重排 | **[MISSING/DIFF]** | A2A `accounts-group-order.tsx:66-326`；AIGX `AS:2339-2372`（仅拖拽） | AIGX 缺键盘/按钮式顺序调整入口。 |
| 未显示账号抽屉 | Sheet：原因分段筛选（全部/待就绪/未安装/已停用/加载异常）+ 逐家行 + 移除旧账号/去插件 | 仅顶部提示条（家数 + 原因摘要 + 刷新按钮），无抽屉/筛选/逐家操作 | **[MISSING/DIFF]** | A2A `accounts-hidden-notice.tsx:37-141`；AIGX `AS:3628-3645` | 若需要，补 Sheet + 原因筛选。 |
| ZCode 套餐通道 | `PlanChannelField` Select（Coding/Start Plan） | 只读说明（未开放） | **[DIFF]** | A2A `accounts-zcode-plan.tsx:151-168`；AIGX `AS:3996-4008` | 待后端 plan_channel 契约。 |
| 账号路由弹窗 | 无（A2A 无此概念） | 「账号路由」弹窗：基础冷却/上限/认证冷却/最大换号 | **[AIGX 独有]** | AIGX `AS:5146-5209` | AIGX 网关特有。 |

## 1.6 数值 / 格式

| 维度 | A2A | AIGX 现状 | 判定 | 证据 | 建议 |
|---|---|---|---|---|---|
| 金额/额度读数 | `numberText`（两位小数上限）+ `compactNumber`（万/亿）+ token `formatTokenCount` | 裸数字拼单位（`${available}${unit}`）；组余额 `toFixed(2)` | **[DIFF]** | A2A `accounts-shared.ts` numberText / `panels.tsx:926-930`（compactNumber）；AIGX `AS:233-237,486-488` | AIGX 缺万/亿紧凑格式，长数字会撑格。建议引入 `fmtCompact`（MM 已有 `fmtCompact` 可复用）。 |
| 到期文案（临期/长期） | `N分钟后/N小时后(1位)/N天后`；「长期有效」；「—」表示读不到 | `expiryLeftText` 同分档；长期有效 badge；— 占位 | **[OK]** | A2A `panels.tsx:424-434`；AIGX `AS:250-255,3068-3096` | 分档一致。 |
| 临期阈值 | 48h 内高亮 | 48h 内 `soon` 高亮 | **[OK]** | A2A `panels.tsx:429`；AIGX `AS:3091` | 一致。 |
| 双凭证（Access/Refresh）到期两行 | 分两行显示各自到期 | 后端只有一个权威 `expires_at`，无法拆分；改为 title 里给「访问凭证/续期令牌」存在性 + 「长期有效」badge | **[DIFF]** | A2A `panels.tsx:415-455`；AIGX `AS:3048-3116` | 需后端提供 access/refresh 各自到期时刻才能两行；当前已知限制已如实标注。 |
| 待生效套餐 | 「待生效 N 份」徽章 + 悬停逐份（名字 + 生效时间） | 「待生效 N」徽章 + 悬停逐份 | **[OK]** | A2A `panels.tsx:521-545`；AIGX `AS:3097-3114,273-280` | 一致。 |
| Token 限额第二行读数 | 余额格第二行「周期Token 已用/上限 · 剩 N 分」(取最值得关注的一条规则) | 仅「Token 限额 · 已跳过」徽章 + 跳过模型数；**无周期性已用/上限读数** | **[DIFF]** | A2A `panels.tsx:1014-1047`；AIGX `AS:3186-3193,3214-3218` | AIGX 缺 usage 格里「已用/上限/倒计时」第二行；后端有 `limiter_state` 但不含周期用量读数。 |
| 百分比 / 进度条 | 剩余比例线（3px 语义色）；缺失不画 | 剩余百分比条（warn/ok）；缺失不画 | **[OK]** | A2A `panels.tsx:878-894`；AIGX `AS:3246-3251` | 一致。 |
| 冷却时长格式 | 「还有 N 分钟 / N 小时 M 分 / N 天」 | `fmtDuration`：`90s / 5m 30s / 1h 5m` | **[DIFF]** | A2A `accounts-limit-detail.tsx:65-75`；AIGX `AS:1642-1652` | 文案风格不同（人话 vs 紧凑）。 |
| 两位小数口径 | `numberText` 上限两位，`compactNumber` 先 toFixed(2) | 组余额已两位；单行读数未统一 | **[DIFF]** | A2A `panels.tsx:926-930`；AIGX `AS:486-488` | 随 §金额格式统一。 |

---

# 第 2 章 · 模型管理

A2A 真相源：`islands/models-page.tsx`（页面）、`models-cells.tsx`（单元格）、`models-dialogs.tsx`（映射/自定义）、`models-fetch-modal.tsx`（获取模型）、`models-batch-dialog.tsx`、`models-dangling.tsx`、`model-capability-dialog.tsx`/`model-test-dialog.tsx`、`models-panel-state.ts`。
AIGX：`MM`（单文件承载页面/表格/全部弹窗）+ `MTD`（测试）+ `MCD`（能力三态）。

## 2.1 列 / 字段

| 维度 | A2A | AIGX 现状 | 判定 | 证据 | 建议 |
|---|---|---|---|---|---|
| 列集与顺序 | check / model / **rate** / **visibility** / source / **budget(上下文/输出)** / caps / alias / act（9 列） | check / model / **price** / source / caps / alias / act（7 列） | **[DIFF]** | A2A `models-page.tsx:573-642`；AIGX `MM:82-96,124-134` | AIGX 无 visibility 列、无独立 budget 列（上下文/输出并入 caps 格）；rate↔price 量纲不同（见下）。 |
| rate（倍率） vs price（定价） | 只读 `credits` 文本（`x0.7 credits`），可排序 | 可编辑定价（token/次/免费 + `model_ratio` Nx），点击开定价弹窗 | **[AIGX 独有/有意分叉]** | A2A `models-cells.tsx:19-22,420-425`；AIGX `MM:199-213,1485-1518` | AIGX 有独立定价子系统，A2A 无；不实现 credits。 |
| 来源 source | per-model 清单来源三态：manual/remote/builtin（色点 + 文案），models.dev 补齐标记 | per-model「承载方」：account/channel tag（远程/内置/手动/modelsdev 色点已定义，但主展示是承载方名）；models.dev 内侧描边点 | **[DIFF]** | A2A `models-cells.tsx:187-204`；AIGX `MM:150-163,1519-1543` | AIGX `SOURCE_DOTS` 已含 remote/builtin/manual/modelsdev，但显示的是 sources[]（承载方）。若 1:1，需在行内展示「清单来源」而非承载方。 |
| 可见性 visibility | 独立列：不展示/可见/未声明/— + 标题说明 | 无独立列 | **[MISSING]** | A2A `models-cells.tsx:221-244`；AIGX 无 | 后端 `ManageModel` 无 visibility 字段（待确认）；有则补列或并入 caps。 |
| 上下文 / 输出（budget） | 独立列：maxInput/maxOutput 两数并列 + models.dev 补齐（虚线）/已覆盖标记 | 无独立列；caps 格内展示 `上下文/输出` + models.dev/覆盖标记（`capability_flags` + `cap_sources.context_length`） | **[DIFF]** | A2A `models-cells.tsx:259-292`；AIGX `MM:1580-1599` | AIGX 已展示输入+输出+来源，但挤在 caps 格；列结构不同。 |
| 能力 caps | 4 圆点（工具/图片/视频/思考）+ models.dev 补齐/覆盖标 | 5 圆点（工具/图像/视频/**音频**/思考）+ 上下文/输出 + models.dev 标 + 两个编辑入口（dot 区开元信息覆盖；slider 开能力三态） | **[DIFF]** | A2A `models-cells.tsx:303-326`；AIGX `MM:144-147,1544-1612` | AIGX 多 audio 圆点、多第二个编辑入口（见 §2.4）。 |
| 模型列 | id（点击复制）+ name 副行（name≠id） | id（点击复制）+ 副行 `provider_label · 上下文`；无 name 字段 | **[DIFF]** | A2A `models-cells.tsx:401-418`；AIGX `MM:1464-1483` | AIGX 后端 `ManagedModel` 无 `name`。需后端补 name 才能显示模型名副行。 |
| 映射 alias | chip + switch + 推理等级标 + × + 「＋映射」；target 藏在 title | chip + switch + **`→target` 常显** + 思考标 + × + 「映射」；含「默认绑定」id→id chip | **[OK/DIFF]** | A2A `models-cells.tsx:103-179`；AIGX `MM:1614-1704` | AIGX 常显 target 并显式化「默认绑定」；A2A 默认绑定是合成项不单列。语义等价。 |
| 操作 act | 测试 + 移除（仅 manual/自定义） | 测试 + 定价 + 删定价 + 删自定义 | **[OK/独有]** | A2A `models-cells.tsx:436-461`；AIGX `MM:1705-1753` | AIGX 多定价入口（自有子系统）。 |
| 列设置（显隐/拖序/对齐/宽度） | `wbColSettings` 重排 DOM th/col | 公共层 `tableColumns v2`（顺序/显隐/对齐/宽） | **[OK]** | A2A `models-page.tsx:177-189,573-644`；AIGX `MM:124-134,1147-1216` | 一致。 |
| 后端字段 `cap_sources` 消费 | 用（可信度分档） | 用（`capDotSource` + `CAP_SRC_LABEL` 中文档位） | **[OK]** | A2A `models-cells.tsx:9-11,303-326`；AIGX `MM:180-195,1554-1575` | 已消费（前轮 P1 已解决）。 |

## 2.2 筛选 / 排序 / 搜索 / 分页

| 维度 | A2A | AIGX 现状 | 判定 | 证据 | 建议 |
|---|---|---|---|---|---|
| 状态筛选 | all/enabled/disabled/mapped（SegmentedControl） | all/on/off/mapped（mm-seg 按钮） | **[OK]** | A2A `models-page.tsx:539-540`；AIGX `MM:2035-2053` | 一致（命名 on/off）。 |
| 可见性筛选 | all/visible/hidden/undeclared（SegmentedControl） | 无 | **[MISSING]** | A2A `models-page.tsx:545-546`；AIGX 无 | 依赖 §2.1 visibility 字段。 |
| 价格/来源/供应商筛选 | 无 | 有：定价(已/未)/来源(供应商/账号)/供应商下拉 | **[AIGX 独有]** | AIGX `MM:2082-2114` | AIGX 多（其定价/双承载方架构）。 |
| 排序 | default / rate-asc / rate-desc（按 credits） | default / ratio-asc / ratio-desc（按 model_ratio）+ 表头倍率点击 | **[DIFF]** | A2A `models-page.tsx:588-599`；AIGX `MM:2054-2071,1244-1250` | 排序键不同（credits vs ratio），与 §2.1 量纲分叉同源。 |
| 搜索字段 | id / name / aliases | id / owned_by / provider_label / aliases / sources[].name（无 name） | **[DIFF]** | A2A `models-page.tsx:551-556`；AIGX `MM:1237-1241,2072-2081` | AIGX 多 owned_by/来源名，缺 name。 |
| 左栏 provider 导航 | 内置组（全部+各家，带搜索框）+ 自定义组（各家 + 新建）；带计数/待获取标 | 渠道分组 + 账号池分组 + 自定义分组（三组）；无搜索框 | **[DIFF]** | A2A `models-page.tsx:263-369`；AIGX `MM:1861-1960` | AIGX 左栏无搜索、分三组（channel/account-pool/custom）；A2A 两家维度（builtin/custom）带搜索。 |
| 分组折叠 | 「全部」聚合视图按 provider 分组 + GROUP_LIMIT=8 折叠「展开其余」 | 「全部」聚合按 provider 分组 + GROUP_LIMIT=8 折叠 | **[OK]** | A2A `models-page.tsx:413-480`；AIGX `MM:56,2229-2281` | 一致。 |
| 分页档位 / 默认 | `[20,50,100,200,'all']` 默认 50 | `CLIENT_PAGE_SIZES=[20,50,100,200,'all']` 默认 50；分组模式隐藏页脚 | **[OK]** | A2A `table-shell.tsx`；AIGX `MSE:20`, `MM:53-54,432,2296-2306` | 已对齐（前轮已修）。 |
| 持久化 | filters + 列设置落 localStorage | filters(`aigx-mm-filters`) + 列设置(`LS_TCOLS`) + 每页 | **[OK]** | A2A `models-panel-state.ts:107-108`；AIGX `MM:59-66,416-428` | 一致。 |
| 批量条 | 勾选后 pill「已选 N · 批量操作」 | 勾选后 chip「已选 N · 批量操作 · 批量映射前缀 · ×」 | **[OK/独有]** | A2A `models-page.tsx:559-567`；AIGX `MM:2177-2194` | AIGX 多「批量映射前缀」（独有）。 |

## 2.3 测试弹窗（逐字段）

| 字段 | A2A（`model-test-dialog.tsx`） | AIGX（`MTD`） | 判定 | 证据 |
|---|---|---|---|---|
| 系统/用户提示词 | 有 | 有 | **[OK]** | A2A `model-test-dialog.tsx:9-11`；AIGX `MTD:239-260` |
| 思考等级 | 有 | 有 | **[OK]** | A2A `:71-99`；AIGX `MTD:264-278` |
| 流式 | 有 | 有 | **[OK]** | A2A `:76`；AIGX `MTD:280-289` |
| 对话协议 protocol | chat_completions/responses/anthropic | chat_completions/responses/anthropic | **[OK]** | A2A `:9-11,77-78`；AIGX `MTD:79,291-304` | 前轮 P1 已落地。 |
| 测试出口 proxy | 跟随账号 / direct / pool:<id> | 跟随账号 / direct / pool / custom(node/url) | **[OK/超集]** | A2A `:79-80`；AIGX `MTD:80-83,304-330` | AIGX 多 custom 节点/URL。 |
| 测试账号/来源选择 | 账号多选 | 来源多选（账号/渠道） | **[OK/超集]** | A2A 账号多选；AIGX `MTD:85,335-361` | AIGX 多渠道维度。 |
| 结果展示 | 延迟/token 等 | 延迟/token/上游模型/协议 | **[OK]** | A2A；AIGX `MTD:445-453` | 一致。 |

## 2.4 能力弹窗 / 元信息覆盖

| 维度 | A2A | AIGX 现状 | 判定 | 证据 | 建议 |
|---|---|---|---|---|---|
| 能力三态弹窗 | 6 键（maxInputTokens/maxOutputTokens/supportsToolCall/supportsImages/supportsVideo/supportsReasoning），inherit/on/off 分段 | `MCD`：同 6 键同三态（继承/是/否），token 1..1e8，null=清除 | **[OK]** | A2A `model-capability-dialog.tsx`；AIGX `MCD:24-126` | 已对齐。 |
| 元信息覆盖弹窗 | 无（能力来自目录 + models.dev） | **AIGX 独有**：「元信息覆盖」表单（模型 ID / 归属方 owned_by / 上下文 / 能力继承\|显式 5 开关 / 其他标签 / 思考等级候选）+ 覆盖列表弹窗（增/改/删） | **[AIGX 独有]** | AIGX `MM:2395-2522`（表单）、`MM:2313-2393`（列表） | 保留为增强；注意与能力三态弹窗职责重叠（两个入口）。 |
| 能力编辑入口 | 1 个（caps 格） | 2 个：dot 区 → 元信息覆盖；slider 小按钮 → 能力三态弹窗 | **[DIFF]** | A2A `models-page.tsx:641-642`；AIGX `MM:1549-1560,1600-1612` | 建议 UI 文案区分两入口，或合并（见下）。 |

## 2.5 自定义模型 / 映射

| 维度 | A2A | AIGX 现状 | 判定 | 证据 | 建议 |
|---|---|---|---|---|---|
| 添加自定义模型弹窗 | 单个：提供商（锁定/搜索下拉）+ 上游模型 ID + 预览 | **两个近似弹窗**：①「新建自定义模型」（id + provider 文本输入，默认 custom，左侧栏沉底入口）；②「添加模型」（provider 下拉 + id，标题栏入口）；meta 覆盖里另有 owned_by | **[DIFF]** | A2A `models-dialogs.tsx:287-388`；AIGX `MM:2743-2791`（customModal）、`MM:2965-3026`（addModelModal） | AIGX 入口重复（前轮 P2 已记录）；建议合并为一个（provider 搜索下拉 + id）。 |
| 归属方 owned_by 死字段 | 无 | 「新建自定义模型」曾采集 owned_by 但不提交 | **[DIFF]** | AIGX `MM:2776-2779`（注释「远程后端仅收 provider+id』） | 已移除提交；当前 customModal 仅 id+provider（注释说明），故实际为一致。 |
| 映射弹窗 | provider 固定（行内入口）；target 只能从该家清单下拉；alias 可填；推理等级 + 自定义；预览 | alias + target **combobox（可跨家任意目标）** + 推理等级（候选来自 overrides）+ 自定义；预览 | **[DIFF]** | A2A `models-dialogs.tsx:44-277`；AIGX `MM:2625-2741` | A2A target 锁定同家清单（避免建了调不通的映射）；AIGX 允许跨家/任意模型名。若 1:1 应将 target 约束到当前家清单。 |
| 推理等级候选 | 三级回落（模型声明 > 兜底表），自定义等级 | 三级回落（overrides 声明 > 兜底 `FALLBACK_REASONING_LEVELS`），自定义 `CUSTOM_LEVEL` | **[OK]** | A2A `models-reasoning.ts`；AIGX `MM:315-333,2665-2713` | 一致。 |
| 批量映射前缀 | 无 | 有（前缀 + 分隔符 + 同名合并） | **[AIGX 独有]** | AIGX `MM:2793-2864` | 保留。 |

## 2.6 获取模型弹窗 / 批量 / 悬空映射 / 空态 / 密度

| 维度 | A2A | AIGX 现状 | 判定 | 证据 | 建议 |
|---|---|---|---|---|---|
| 获取模型弹窗 | 逐 provider 行 + **「模型来源」账号下拉**（哪个账号去拉 + 落盘记忆）+ 逐行获取 + 全刷 + 三态结果(已刷新/失败/跳过)；内置家=刷新远程目录，自定义家=拉取→勾选→导入 | 逐渠道/供应商行 + 全刷/重试 + 结果 + 清理失效映射条数；**无「按 provider 选账号」**（账号池按账号内部逐号拉） | **[DIFF]** | A2A `models-fetch-modal.tsx:265-681,576-591`；AIGX `MM:2866-2963,1038-1145` | AIGX 缺「用哪个账号拉目录」下拉与「自定义家勾选导入」流程（AIGX 自定义家走 addCustomModel 手动）。 |
| 批量操作弹窗 | enable/disable/reasoning/delete（删模型登记，跳过非手动 + 二次确认） | enable/disable/set-reasoning/remove-price/remove-custom（删定价/删自定义，危险二次确认） | **[OK/DIFF]** | A2A `models-batch-dialog.tsx:54-57`；AIGX `MM:3028-3106` | AIGX 有「删除自定义模型登记」（前轮 P1 已补）；多删定价。思考等级清空语义一致。 |
| 悬空映射 | 以 `mappings[].dangling` 表达 | 独立「未挂载的模型映射」区块（列出别名→目标 + 删除） | **[OK/独有]** | A2A `models-dangling.tsx`；AIGX `MM:2117-2175,2139-2147`（取数 `dangling` 优先） | 一致（AIGX 独立区块）。前轮字段名 `dangling_mappings`→`dangling` 已兼容。 |
| 空态 | 自定义家/内置家/无模型分别文案 | 加载中/搜索空/筛选空/无模型分别文案 | **[OK]** | A2A `models-page.tsx:387-409`；AIGX `MM:2196-2208` | 一致。 |
| 布局/密度 | 两行卡片头 + 左栏（带搜索）+ 筛选行（状态/可见性 seg + 搜索 + 批量）+ 表格 + 悬空 + 页脚 | 左栏（渠道/账号池/自定义三组）+ 标题行 + 筛选行（状态 seg + 排序 seg + 搜索 + 来源/供应商/定价下拉）+ 悬空 + 批量 chip + 表格 + 页脚 | **[DIFF]** | A2A `models-page.tsx:485-669`；AIGX `MM:1817-2311` | AIGX 筛选行更满（下拉多）；缺可见性 seg；左栏多分组、无搜索。 |
| 定价倍率 Tab | 无 | 顶部 `Tabs`：模型目录 / 定价倍率（`RatioPanel`） | **[AIGX 独有]** | AIGX `MM:1846-1857` | AIGX 独有定价子系统。 |

---

# 3. 差距汇总与实施优先级（交 frontend-dev / frontend-models）

## 3.1 账号池（交 frontend-dev）

> 按「是否影响 1:1 视觉与字段」排序；均为纯前端可落地的优先，标「需后端」的另注。

| 优先 | 项 | 类型 | 动作 | 证据 |
|---|---|---|---|---|
| P0 | 金额/额度/组余额格式 | 格式 | 引入紧凑格式（万/亿，可复用 `MM.fmtCompact`）；统一两位小数上限 | A2A `panels.tsx:926-930` vs `AS:233-237,486-488` |
| P1 | 余额明细弹层缺逐份积分包/到期徽标/spent 折叠 | 补弹窗 | 余额 popover 补积分包行 + 「已用完/已过期」折叠 + 到期徽标 | `accounts-credit-detail.tsx:328-465` vs `AS:2710-2764` |
| P1 | 限流/冷却明细信息缺失 | 补弹窗 | 冷却展开面板补 倒计时/绝对恢复时刻/原因/来源/进度线 | `accounts-limit-detail.tsx:83-214` vs `AS:4277-4336` |
| P1 | 账号列副行/悬停字段 | 补渲染 | 补 nickname / identifier（后端有则）到 tooltip；email 副行待后端 email 字段 | `panels.tsx:1318-1360` vs `AS:2620-2640,2800-2804` |
| P1 | 用量窗口模型配额视图 | 补弹窗 | 余额窗口弹层补「模型配额/分组窗口」tab + 重置/到期 | `accounts-usage-windows.tsx:193-310` vs `AS:3219-3257` |
| P1 | Token 限额第二行读数 | 补渲染 | usage 格补「已用/上限 · 剩 N 分」第二行（需后端给周期用量读数） | `panels.tsx:1014-1047` vs `AS:3186-3218` |
| P2 | 代理表单缺 clash / 测试出口 | 补字段 | ProxySelector 补 clash 档 + 测试出口按钮 | `accounts-proxy-form.tsx` vs `ProxySelector.tsx:113-204` |
| P2 | 未显示账号抽屉 | 补抽屉 | 提示条升级为 Sheet + 原因分段 + 逐家操作 | `accounts-hidden-notice.tsx:37-141` vs `AS:3628-3645` |
| P2 | 单行取消余额查询 | 补操作 | ⋯ 菜单补「取消余额查询」（后端有 per-id 能力时） | `panels.tsx:1518-1519` vs 无 |
| P2 | 整组「批量操作本组/导出本组」 | 补操作 | 组带 ⋯ 补两项 | `panels.tsx:143-180` vs `AS:4228-4264` |
| P2 | 批量刷新 Token / 复制自账号模板 | 补操作 | 批量弹窗补 刷新Token + 复制模板 | `accounts-batch-dialog.tsx:25-34,279-294` vs `ABD:32-40` |
| P2 | 调整提供商顺序弹窗 | 补弹窗 | 补按钮/keyboard 顺序调整（当前仅拖拽） | `accounts-group-order.tsx:66-326` vs `AS:2339-2372` |
| P2 | 选择全部筛选结果 | 补操作 | 批量栏补「选择全部筛选结果(N)」 | `accounts-page.tsx:498-500` vs `AS:4010-4049` |
| P2 | 工具条密度/两行控件 | 布局 | 冷门筛选收进 ResponsiveSheet，精简 actions | `accounts-page.tsx:374-477` vs `AS:3661-3992` |
| P2 | 虚拟滚动/行高 | 布局 | 截断 name 副行或引入虚拟滚动 | `accounts-list-body.tsx:106-124` vs `AS:3563-3603` |
| 需后端 | email 字段 / balanceToken / balanceQuery / zcode plan_channel / access+refresh 各自到期 | 字段 | 后端补齐后再接 UI | §1.1.3 |
| 有意分叉 | provider/rate/cooldown 多列、列表默认模式、账号路由弹窗 | 记录 | 保留并标注 AIGX 特有 | §1.1.1,§1.2,§1.5 |

## 3.2 模型管理（交 frontend-models）

| 优先 | 项 | 类型 | 动作 | 证据 |
|---|---|---|---|---|
| P1 | 可见性 visibility 列/筛选缺失 | 补列/筛选 | 后端有 visibility 字段则补列（不展示/可见/未声明/—）+ 筛选 seg | `models-cells.tsx:221-244`,`models-page.tsx:545-546` vs 无 |
| P1 | 来源列语义（清单来源 vs 承载方） | 改口径 | 行内展示清单来源三态（manual/remote/builtin），承载方另处 | `models-cells.tsx:187-204` vs `MM:1519-1543` |
| P1 | 模型列缺 name 副行 | 补字段 | 后端补 `ManagedModel.name` 后在副行/悬浮展示 | `models-cells.tsx:401-418` vs `MM:1464-1483` |
| P2 | 映射 target 越家可选 | 收紧 | target 约束到当前家清单下拉（避免建调不通映射） | `models-dialogs.tsx:44-277` vs `MM:2625-2741` |
| P2 | 两能力编辑入口职责重叠 | 合并/区分 | 明确 dot=元信息覆盖 vs slider=能力三态，或合并 | `MM:1549-1612` |
| P2 | 两个自定义模型弹窗入口 | 合并 | 合并为一个（provider 搜索下拉 + id） | `MM:2743-2791,2965-3026` |
| P2 | 获取模型弹窗缺「按 provider 选账号」 | 补字段 | 补模型来源账号下拉 + 落盘 | `models-fetch-modal.tsx:576-591` vs `MM:2866-2963` |
| P2 | 左栏缺 provider 搜索 | 补控件 | 左栏补搜索框 | `models-page.tsx:279-309` vs `MM:1861-1960` |
| P2 | 排序键 credits vs ratio / 多 price 列 | 记录 | 与定价子系统量纲不同，记录为有意分叉 | §2.1,§2.2 |
| P2 | caps 多 audio 圆点 | 记录 | AIGX 扩展，确认后端 capability 键含 audio | `MM:146` |
| 基础设施/独有 | 定价子系统/定价倍率Tab/批量映射前缀/元信息覆盖/非管理员过滤 | 记录 | AIGX 独有，非差距 | §2.4,§2.5,§2.6 |

---

## 4. 说明与证据边界

- 「OK」指字段语义与交互等价，不要求逐字实现一致（AIGX 为独立 Rust/React 实现，A2A AGPL，仅审计未拷贝）。
- AIGX 账号池桌面筛选已是 A2A **桌面**超集（到期/用量分档、区间来自移动端/扩展）；差异集中在「明细弹层信息量」「工具条密度」「行密度/虚拟化」与若干需后端字段。
- AIGX 模型管理与 A2A 的最大结构性差异是 **定价子系统（AIGX 独有）** 与 **可见性列 / 来源列语义 / 模型 name 字段**（A2A 有、AIGX 缺或分叉）。前几轮已解决的 P1（测试弹窗 protocol/proxy、能力三态 6 键、批量删自定义、分页档位、cap_sources 消费、dangling 字段名）在本次审计确认**仍为已解决状态**。
- 标「待确认」/「需后端」的项依赖后端 schema；前端实施可在字段就绪后增量接入，不阻塞其余项。
- 行号基于 2026-11-23 工作区；`AS`/`MM` 为单文件大页，渲染 switch 行号区间较宽，具体改动以实施时实际代码为准。
