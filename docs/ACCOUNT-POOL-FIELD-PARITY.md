# 账号池字段级一对一审计：AIGX × Agent2API（desktop + mobile）

> 审计日期：2026-11-22
> A2A 真相源：`D:\GitHub\Agent2API`（只读；AGPL，仅审计行为，未拷贝源码）
> AIGX：`D:\GitHub\AIGX`（工作区，只读审计，仅新增本文件）
> 结论标签：**一致 / 改名 / 补列 / 补渲染字段 / 隐藏 / 新增 / 不适用**
> 路径约定：A2A 桌面 = `desktop-tauri/ui-islands/src/islands/`（下表简写 `islands/`）；A2A 移动端 = `desktop-tauri/mobile/src/pages/AccountsPage.tsx`；AIGX 前端 = `frontend/src/`；AIGX 号池页 = `frontend/src/pages/AccountSessions.tsx`（简写 `AS.tsx`）。

## 0. 结论与计数

| 维度 | 一致 | 差异（需处理） | 不适用 | 主要差异 |
|---|---:|---:|---:|---|
| 列（10 A2A vs 13 AIGX） | 8 | 5 | 1 | AIGX 多 `provider`/`rate` 两列；`限流` 列名语义分叉（AIGX 拆成 `限制器`+`限流`）；`account`/`limits`/`expiry`/`proxy` 渲染字段缺 |
| 筛选 | 3 | 4 | 0 | AIGX 桌面已是 A2A 桌面超集；对 A2A **移动端**缺「到期/用量分档」与「多选 provider」；新筛选未持久化 |
| 账号字段（设置/详情） | 9 | 6 | 3 | AIGX 缺 `allowRouteSkip`/`userAgent` UI、`balanceToken`、per-provider `balanceQuery`；AIGX 多 `weight`/`models`/`extraHeaders`/`chatPath`/`expiresAt` |
| 操作 / 批量 | 12 | 2 | 1 | AIGX 缺「每行导出」「取消查询（单行）」；`desktop launch` 不适用（网关） |

> 说明：AIGX 13 列对 A2A 桌面 10 列**无缺列**（语义全覆盖），差异集中在「多列是否该有」「同名列内容口径」与「列内渲染字段」。

## 1. 列对照（A2A 桌面 10 列 vs AIGX 13 列）

A2A 列集与顺序：`islands/accounts-columns.ts:54-83`（pick/order/account/proxy/connections/status/limits/expiry/usage/actions）；表头 `islands/accounts-panels.tsx:1581-1625`；行渲染 `islands/accounts-table-row.tsx:44-45`。
AIGX 列集与顺序：`AS.tsx:377-395`（pick/order/name/provider/proxy/conn/status/limiter/expiry/usage/rate/cooldown/act）；渲染 switch `AS.tsx:2462-3181`。

| AIGX 列(key/label) | A2A 对应列(label) | A2A 渲染字段（file:line） | AIGX 渲染字段（file:line） | 差异 | 建议 |
|---|---|---|---|---|---|
| pick「选择」`AS.tsx:381` | pick「选择」`accounts-columns.ts:55` | 复选框 `account.id`/`store.selected`，`panels.tsx:1581-1586` | 复选框 `picked.has(s.id)`，`AS.tsx:2464-2474` | 一致 | 一致 |
| order「顺序」`AS.tsx:382` | order「顺序」`:56-60` | 组内序号 `#rank`+拖动把手，`panels.tsx:108-126`（rank 来自 `accounts-page.tsx:364-368`） | `s.sort_order`（缺省「—」），`AS.tsx:2475-2483`；拖拽在 `AS.tsx:2020,2045` | A2A 是「组内 #N + 把手同格」；AIGX 是原始 sort_order 数字 | 补列：分组模式下渲染组内 #N；否则保留 |
| name「账号」`AS.tsx:384` | account「账号」`:61` | `displayNameOf||未命名账号`、`email`、`needsRelogin`、`proxy.error`，tooltip 含 identifier/name/nickname/desktop/updatedAt/source，`panels.tsx:1309-1368` | `s.name`(mask)、`s.api_key`、`s.proxy`、`s.last_error`、模型状态徽章 `modelStatusNode`，`AS.tsx:2485-2502,2322-2335,2346-2403` | AIGX 缺 `email` 副行、`source`、`updatedAt`；`needsRelogin` 在 AIGX 挪到状态列（`AS.tsx:2564-2638`）；AIGX 多模型目录徽章 | 补渲染字段：email 副行 + source/updatedAt tooltip；模型徽章保留 |
| provider「Provider」`AS.tsx:385` | （无；分组模式已删，`accounts-columns.ts:30-37`；移动端上移分组头 `AccountsPage.tsx:43-46`） | — | 图标+`s.provider`+`s.base_url`，`AS.tsx:2504-2518` | AIGX 多列（默认列表模式，非分组） | 保留；分组模式可关（不适用对齐 A2A 删除） |
| proxy「代理」`AS.tsx:386` | proxy「代理」`:62-66` | `proxy.source/proxyId/label/error`，选择 direct/pool/custom，`panels.tsx:1371-1455` | `s.proxy`+`proxyPool`+`rotate`，`proxy_state.ok/error`，`AS.tsx:2520-2562` | A2A 选择器含 rotate/clash 与 error 提示；AIGX 有 rotate 但无 clash 模式入口 | 补渲染字段：clash/rotate 全部（若后端支持） |
| conn「连接数」`AS.tsx:387` | connections「连接数」`:67-71` | `connectionsOf(id)`，0 时空，`panels.tsx:464-472` | `connCounts[id]??in_flight`，除 `max_concurrency`，`AS.tsx:2682-2700` | AIGX 显示占用/上限；A2A 0 时留空 | 一致（可对齐 0 留空，可选） |
| status「状态」`AS.tsx:388` | status「状态」`:72` | Switch `isEnabled`+`accountTags`+`tokenDisableTriggered`，`panels.tsx:302-331` | Switch `status==='enabled'`（expired 禁用）+健康标签（api_key/proxy/limiter.disable/needsRelogin），`AS.tsx:2564-2638` | AIGX 健康标签更全；A2A 有 tokenDisableTriggered | 一致 |
| limiter「限制器」`AS.tsx:389` | limits「限流」`:73-77` | `upstreamSuspendedReason`、`rateLimits`、`upstreamCooldown`、`limiterHit`、`balanceBlocked`、`tokenBlocked`、成功徽章、明细/清限按钮，`panels.tsx:348-394` | 仅 `limiter_state.account_skip`/`disable`，`AS.tsx:2640-2680` | **列名不一致 + 内容分叉**：A2A「限流」= 综合限流总览；AIGX「限制器」只表账号级判据，另设「限流」列 | 改名/合并：AIGX `限制器` 补齐 upstream/余额/令牌/成功徽章与明细清限，或与 `cooldown` 合并为 A2A「限流」语义 |
| expiry「有效期」`AS.tsx:390` | expiry「有效期」`:78` | `expiresAt`/`tokenExpiry`，双凭证 Access/Refresh 两行，长期有效，过期徽章，`panels.tsx:415-455` | `accountExpiryOf`（credential `expires_at` 或 `last_balance.subscription.expireAt`）+ 倒计时，`AS.tsx:252-274,2701-2747` | AIGX 缺双凭证（Access/Refresh）两行与「长期有效」标签（CodeArts 等双凭证家有实物） | 补渲染字段：双凭证行 + long-lived |
| usage「余额」`AS.tsx:391` | usage「余额」`:79-81` | 额度窗口/credits/blocked/pending/token 徽章，`panels.tsx:1049-1207`；明细 `usage-windows.tsx:58-110` | `balances`/`usagePoolOf`（wallets/available/unit/subscription.plan）+windows+skip_models 徽章，`AS.tsx:2749-2890,214-232,2850-2888` | 近似；A2A 有 credit 明细 popover（`credit-detail.tsx:136-296`） | 一致；可补 credit 明细（可选） |
| rate「成功率」`AS.tsx:392` | （无） | — | `success_count/(success+error)`，`AS.tsx:2892-2893,1480-1486` | AIGX 多列（后端有计数，A2A 无此列） | 保留（AIGX 特有） |
| cooldown「限流」`AS.tsx:393` | （合并在 limits） | 见 limits（`panels.tsx:348-394`） | `activeCooldowns`+`limiter_state.skip_models`，`AS.tsx:2894-2945,1395-1399` | AIGX 多列；标签「限流」与 A2A limits 同名但内容=冷却明细 | 改名「冷却」，或与上表 `限制器` 合并 |
| act「操作」`AS.tsx:394` | actions「操作」`:82` | DesktopLaunchButton/余额/设置/⋯，`panels.tsx:1473-1491`；⋯菜单 cancel query/enable/首选/并发/refresh token/export/delete，`:1505-1555` | 测试/目录同步/编辑/清冷却/签到/⋯（refresh token/重登/置顶/上下移/并发/福利/删除），`AS.tsx:2959-3177` | AIGX 多测试/目录同步/签到/重登/移动；A2A 多 desktop launch/每行导出/取消查询 | 补列操作：每行导出、单行取消查询；desktop launch 不适用 |

## 2. 筛选对照（含持久化）

| 筛选/控件 | A2A 桌面 | A2A 移动端 | AIGX | 差异 | 建议 |
|---|---|---|---|---|---|
| provider | 单选 Combobox `accounts-page.tsx:394-417` | **多选** `AccountsPage.tsx:210-213,189-199` | 单选 Select `AS.tsx:3365-3382` | AIGX 单选；移动端支持多选 | 新增：多选 provider（可选） |
| 关键词搜索 | 匹配 name/nickname/email/identifier/id `accounts-page.tsx:423-428`（`accounts-domain.ts:892-904`） | 匹配 name/nickname/email/id/provider `AccountsPage.tsx:137-144,227-231` | 匹配（`AS.tsx:3383-3388,1525-1530`） | 需确认 AIGX 是否含 email/nickname/identifier | 补渲染字段：确认覆盖 email/nickname/identifier |
| 状态 | all/enabled/disabled `accounts-page.tsx:438-443` | all/enabled/disabled/**limited/relogin/expired** `AccountsPage.tsx:162-169,204-209` | all/enabled/disabled/**cooling** `AS.tsx:3476-3494` + 独立 relogin-only `:3498-3505` + expired-only `:3506-3513` | AIGX 用布尔开关替代下拉枚举；多 `cooling` | 一致（能力齐平）；可合并为枚举下拉（可选） |
| 限额段 | all/normal/limited `accounts-page.tsx:448-453` | — | all/normal/limited `AS.tsx:3457-3474` | 一致 | 一致 |
| 有效期 | 无 | all/soon/expired/longlived `AccountsPage.tsx:170-175,214-222` | from/to 日期区间 `AS.tsx:3515-3534`（params `expiry_from/to` `:729-736`） | 区间 vs 分档 | 新增：soon/longlived 分档（对齐移动端口径） |
| 用量/余额 | 无 | all/has/unqueried/failed `AccountsPage.tsx:176-181,223-224` | min/max 数值区间 `AS.tsx:3535-3560`（`usage_min/max` `:737-740`） | 区间 vs 分档 | 新增：has/unqueried/failed 分档（对齐移动端口径） |
| 隐藏账号 | 批量/提示条 `accounts-page.tsx:527`；抽屉原因筛选 `accounts-hidden-notice.tsx:105-111` | HiddenAccountsNotice | 提示条 `AS.tsx:3249-3258`（state `:563,757`） | A2A 抽屉可按住原因筛选；AIGX 仅提示条 | 新增：隐藏原因筛选（可选） |
| 持久化 | provider/status/limit 持久化 `accounts-store.ts:56,112-121`；搜索不持久化 | 不持久化 | provider/status/limit 持久化（`LS_FILTER` `AS.tsx:432,788`）；搜索/relogin/expired/到期/用量不持久化（`AS.tsx:786`） | AIGX 新筛选未持久化 | 新增：把 relogin/expired/到期/用量并入 `LS_FILTER` |

## 3. 账号字段对照（设置/详情对话框）

A2A 账号设置：`islands/accounts-settings-dialog.tsx`（简写 ASD）+ `accounts-settings-fields.tsx`（fields）+ 子对话框。
AIGX 新增/编辑：`frontend/src/components/AddAccountDialog.tsx`（简写 ADD，字段 state `:88-111`，payload `:732-737`）+ `LimiterGroupEditor.tsx`。

| 字段 | A2A（file:line） | AIGX（file:line） | 差异 | 建议 |
|---|---|---|---|---|
| 备注名 name | ASD:605-609 | ADD:88; payload `:731` | 一致 | 一致 |
| 账号 ID（只读） | ASD:610 | 列表回显（不可编辑） | 一致 | 一致 |
| 启用 enabled/status | ASD:611-616 | ADD `:1716`（status）/行开关 `AS.tsx:388` | 一致（AIGX 用 status 字符串） | 一致 |
| 失败后允许换号 allowRouteSkip | ASD:619-624 | 后端 `model.rs:646`，**前端无入口**（grep 无） | AIGX 缺 UI | 新增：补开关 |
| 并发上限 maxConcurrent | ASD:625-631 | ADD:92,1685 | 一致 | 一致 |
| User-Agent userAgent | ASD:635-644 | 后端 `model.rs:652`，**账号级前端无入口** | AIGX 缺 UI | 新增：补账号级 UA 输入 |
| 优先级 priority | ASD:645-647（**明确不可编辑**，靠拖拽） | ADD:90,1671（数字可编辑） | AIGX 暴露数字；A2A 改拖拽 | 保留（AIGX 另有排序/置顶）；或收敛为拖拽 |
| CatPaw 余额凭证 balanceToken | ASD:652-653 | `AccountSession` 无此字段（`model.rs` 无 balanceToken） | AIGX 缺 | 新增：balanceToken（或用 credential.fields 约定键） |
| 无需鉴权 noAuth | fields:83-94 | ADD:202-203,323,751-755 | 一致 | 一致 |
| API Key apiKey | fields:95-115 | ADD:1590（credential） | 一致 | 一致 |
| 套餐 zcodePlan | ASD:658-660 | 前端无（grep 无）；ZCode 协议见 #35「后续」 | AIGX 缺（ZCode 后续） | 不适用（本轮） |
| 用量查询 enabled/interval/timeout/retries | `accounts-dialog-usage.tsx:122-165` | 后端 `model.rs:655,586`；前端仅间隔（需确认 timeout/retries UI） | 需确认 AIGX 暴露 timeout/retries | 补渲染字段：确认/补 timeout+retries |
| 余额查询凭证 balanceQuery（new_api/sensenova/zhipu…） | ASD:672-677；`balance-query.ts` keys `:45,94-164` | AIGX 前端无 balanceQuery 配置（grep 无） | AIGX 缺 per-provider 余额查询凭证 | 新增：balanceQuery 配置（或明确用 credential.fields 承载） |
| 限制器 limiterGroups（enabled/join/action/target/condition） | ASD:681-685；`accounts-dialog-limiter.tsx:65-113`；condition `accounts-limiter-condition.tsx:156-245` | `LimiterGroupEditor.tsx:141-302`（enabled/action/target/join/metric/comparison/threshold/window/sourceId/models） | AIGX 条件编辑器更细；能力齐平 | 一致（AIGX 为超集） |
| 代理 proxy（none/pool/rotate/clash/custom） | ASD:687-695；`accounts-proxy-form.tsx:326-509` | ADD:1705；`ProxySelector.tsx`；行内 `AS.tsx:2520-2562` | AIGX 无 clash/custom host-port 表单入口（需确认 ProxySelector） | 补渲染字段：clash/custom（若后端支持） |
| 自定义 provider（name/protocol/baseUrl/clientEmulation/删除） | ASD:696-701；fields:172-226 | AIGX 走 registry 预置 + ADD protocol `:1534`/base_url `:1640`；无自定义 provider 体系 | 架构不同 | 不适用（AIGX 无自定义 provider；导入的 custom-* 见 #44） |
| baseUrl 覆盖 | fields:184-198 | ADD:88,1640 | 一致 | 一致 |
| 模型 models | 仅「模型管理」链接 `fields:201-203`（对话框不编辑） | ADD:96,1693-1696 可手填；目录同步 `AS.tsx:2968,1278-1321` | AIGX 可手编并支持回源 | 保留（AIGX 更强） |
| 权重 weight | 对话框无 | ADD:91,1677 | AIGX 多 | 保留（AIGX 特有） |
| 自定义头 extraHeaders | 对话框无 | ADD:95,1698 + raw headers `:1606` | AIGX 多 | 保留 |
| chatPath | 对话框无 | ADD:89,1664 | AIGX 多 | 保留 |
| expiresAt 编辑 | 只读展示（expiry 列） | ADD:96,1710 可编辑 | AIGX 多 | 保留 |
| pluginSecret/pluginPublic/pluginIdentity | （插件字段，A2A 侧由插件管） | `model.rs:609,615,621`（导入已支持 pluginSecret，#45） | 能力在，无 UI | 一致（无需 UI） |

## 4. 操作与批量对照

| 操作 | A2A（file:line） | AIGX（file:line） | 差异 | 建议 |
|---|---|---|---|---|
| 添加账号 | `accounts-page.tsx:466-470`；分组内 `panels.tsx:271-282` | `AS.tsx:3356`；ADD 对话框 | 一致 | 一致 |
| 导入（本地扫描/JSON/批量） | 本地导入 `add-provider-import.tsx` | import JSON `AS.tsx:3328-3346`、import-scan `:3350`、bulk `:3353`、agent2api 导入 | AIGX 更强 | 一致 |
| 导出 | 分组导出 `panels.tsx:151` | 导出 `AS.tsx:3320`、export-files `:3324` | 一致 | 一致 |
| 刷新（临期/令牌） | 刷新 `accounts-page.tsx:458-462`；批量 refresh token `accounts-batch-dialog.tsx:25-34` | refresh-expiring `AS.tsx:3311`；行 refresh `:3062` | 一致 | 一致 |
| 查询全部余额 / 取消 | 全部 `accounts-page.tsx:471-474`；取消 `:475` | 全部 `AS.tsx:3307`；异步 `usage/start|cancel`（后端 `:4418,4444`） | 一致 | 一致 |
| 签到 | 行「已签到」按钮（`accounts-columns.ts` 操作预算注释 `:130`） | claim `AS.tsx:3028-3038` | 一致 | 一致 |
| 设置 | 行 `panels.tsx:1486` | edit `AS.tsx:2979` | 一致 | 一致 |
| 删除 | ⋯菜单 `panels.tsx:1505-1555` | 行 `AS.tsx:3166`；批量 `:3612` | 一致 | 一致 |
| 启停 | Switch `panels.tsx:302-331`；批量 | Switch `AS.tsx:388`；批量 `:3596,3604` | 一致 | 一致 |
| 置顶/首选/promote | ⋯set preferred `panels.tsx:1505-1555` | set-first `AS.tsx:3096`；promote 后端 `:1336` | 一致 | 一致 |
| 并发上限 | ⋯/批量 | 行 `AS.tsx:3136`；批量 `AccountBatchDialog.tsx:304-310` | 一致 | 一致 |
| 批量：代理/用量查询/限制器 | `accounts-batch-dialog.tsx:25-34` | `AccountBatchDialog.tsx:291-338` | 一致 | 一致 |
| 拖拽排序/上下移 | 行拖 `accounts-page.tsx:347-359` | 行/组拖 `AS.tsx:2020,2045`；上下移 `:3111,3119` | 一致 | 一致 |
| 测试连通 / 目录同步 | （隐式） | test `AS.tsx:2959`；catalog sync `:2968` | AIGX 多 | 保留（AIGX 特有） |
| 重登 | needsRelogin 提示 + 设置 | relogin `AS.tsx:3079` | 一致（#37 已补） | 一致 |
| 福利/活动 | 活动面板 `panels.tsx:1466-1471` | welfare `AS.tsx:3155` | 一致 | 一致 |
| 桌面端启动/切号 | `panels.tsx:1480`；`accounts-desktop-launch.tsx:19-46` | 无（网关无桌面客户端） | AIGX 不适用 | 不适用 |
| 每行导出 | ⋯export `panels.tsx:1505-1555` | 仅全量/export-files，无单行 | AIGX 缺单行导出 | 补列操作：单行导出 |
| 单行取消查询 | ⋯cancel query `panels.tsx:1505-1555` | 仅全局取消（后端有 per-id 能力） | AIGX 缺单行取消 | 补列操作：单行取消 |

## 5. 差异计数与实施优先级（供 #50）

| 优先级 | 项 | 差异类型 | 建议动作 | 证据 |
|---|---|---|---|---|
| **P0** | 「限流」列语义分叉 | 改名/合并 | 将 AIGX `限制器`+`限流` 收敛为 A2A「限流」单列（或 `限制器` 改名并补齐 blocked/badge/明细清限） | `panels.tsx:348-394` vs `AS.tsx:2640-2680,2894-2945` |
| **P1** | limits 列渲染字段缺 | 补渲染字段 | 补 upstream 限流原因、余额阻断、令牌阻断、成功徽章、明细/清限 | `panels.tsx:348-394` |
| **P1** | expiry 双凭证行缺失 | 补渲染字段 | 补 Access/Refresh 两行 + 长期有效标签（CodeArts 等） | `panels.tsx:415-455` vs `AS.tsx:2701-2747` |
| **P1** | account 列副信息缺 | 补渲染字段 | 补 email 副行 + source/updatedAt tooltip | `panels.tsx:1309-1368` vs `AS.tsx:2485-2502` |
| **P2** | 到期/用量需分档（对齐移动端） | 新增 | 加 soon/longlived、has/unqueried/failed 分档 | `AccountsPage.tsx:170-181` vs `AS.tsx:3515-3560` |
| **P2** | provider 多选 | 新增 | 单选→多选（对齐移动端） | `AccountsPage.tsx:210-213` |
| **P2** | 新筛选未持久化 | 新增 | relogin/expired/到期/用量并入 `LS_FILTER` | `AS.tsx:432,786-788` |
| **P2** | allowRouteSkip / userAgent 无 UI | 新增 | 设置对话框补两开关（后端字段已备） | `model.rs:646,652`；A2A `ASD:619-644` |
| **P2** | balanceToken / balanceQuery 缺 | 新增 | 补 CatPaw balanceToken 与 per-provider 余额查询凭证 | `ASD:652-677`；`balance-query.ts:45,94-164` |
| **P2** | 单行导出 / 单行取消查询 | 补列操作 | 行 ⋯ 菜单补两项 | `panels.tsx:1505-1555` |
| **P2** | provider/rate/cooldown 多列 | 隐藏/保留 | 保留并标注 AIGX 特有；分组模式可关 provider | `AS.tsx:385,392,393` |
| — | desktop launch | 不适用 | 网关无桌面客户端 | `panels.tsx:1480` |
| — | 自定义 provider 体系 | 不适用 | AIGX 走 registry，无用户自定义 provider | `ASD:696-701` |

## 6. 说明

- 本表「一致」仅指**能力/字段语义等价**，不要求逐字实现一致（AIGX 为独立 Rust/React 实现，A2A AGPL，仅审计未拷贝）。
- AIGX 桌面筛选已是 A2A **桌面**的超集（relogin/expired/到期/用量来自 #36/#37）；对 A2A **移动端**在「分档口径」与「provider 多选」上仍有差。
- 行号基于审计当日（2026-11-22）工作区；`AS.tsx` 行号区间较宽（渲染 switch `:2462-3181`），具体改动以实际代码为准。
- 处理建议均可在前端单独落地，无后端 schema 变更（除 balanceToken/balanceQuery 若采新字段）。
