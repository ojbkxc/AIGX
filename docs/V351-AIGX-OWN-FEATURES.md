# AIGX 独有功能台账（v3.5.1 对齐期间）

> ⚠️ **部分失实（2026-11-26 复核）**：§0.1 把 `PROVIDER_DESC` 标为「被删待恢复」，
> 实际已恢复（`frontend/src/pages/checkin-shared.ts`）。核对现状见
> [`V351-GAP-AUDIT-2026-11-26.md`](./V351-GAP-AUDIT-2026-11-26.md)。

> 口径来源：用户 2026-11-24 明确「**优先严格 1:1 对齐 A2A 的列表等功能，最后再评审在哪些地方加自己的**」
> 与「**你还要对齐 AIGX 自己的功能哦，别忘记了**」。
>
> 本文档的作用：对齐期间各成员按 A2A 改造时会碰到 AIGX 独有的实现。**不能默默删掉**，也不能因为
> 「A2A 没有」就判定该删。每一项都要在此登记状态与处置，收口时逐条结清。
>
> 状态取值：`保留`（未受影响）/ `改形`（还在，形态变了）/ `被删待恢复`（确实丢了）/ `待评审`（要不要留由人定）

---

## 0. 已确认被误删（必须恢复）

| # | 项 | 原位置 | 现状 | 处置 |
|---|---|---|---|---|
| 0.1 | `PROVIDER_DESC` 签到链路说明表（12+ 家静态文案：Loomy「每日领取会员权益」、Kuku「每日签到领积分」、qoder-intl「风控组件 UMID 见福利通道」、workbuddy-intl「保活链见福利通道」、xiaohuanxiong「桌面端/移动端各领一次」…） | `pages/CheckinCenter.tsx:102`，渲染于左栏 provider 行 `group.description \|\| PROVIDER_DESC[group.id]`（`:1437`） | **整表消失**：全仓 grep `PROVIDER_DESC` / `讯飞 Loomy` / `保活链见福利通道` / `风控组件 UMID` 均 0 命中；签到重构为 A2A 的四分区 + `checkin-provider-rail.tsx`（该 rail 只有 `name`，无 description 槽位） | **恢复**。A2A 的 rail 没有说明位，但这是 AIGX 独有信息（讲清每家签到链路、UMID/保活链入口在哪）。落点：把表迁到 `checkin-shared.ts`（或新 `checkin-provider-desc.ts`），在 rail 行加 `title` 悬停展示 + 行列表头/分区脚注择一处显示。**不塞回 A2A 左栏布局**，避免破坏已对齐结构 |

## 1. 路由（align-routing 上报）

| # | 项 | 状态 | 说明 / 处置 |
|---|---|---|---|
| 1.1 | 分层档案三态体系（全局 / 模型级 / 路由组逐字段继承） | 保留 | A2A 是账号画布模型，没有这一层。这是 AIGX 的路由核心，不动 |
| 1.2 | `SHOW_ROUTING_ENHANCED=false` 隐藏的候选/命中层预演、能力需求预演、模型覆盖工作台（RoutingWorkspace） | 保留（隐藏） | 代码保留、入口隐藏。待评审：是否恢复入口 |
| 1.3 | 请求流向（RoutingFlow）/ 请求跟踪（RoutingTrace）两面板 | 保留 | A2A 无对应面板 |
| 1.4 | `SpecialHits` 的 15s 静默刷新与「查看全部」 | 保留 | — |
| 1.5 | 页头「会话粘滞」用滚动定位到配置视图底部（A2A 是弹窗） | 改形 | AIGX 无独立弹窗体系。待评审：是否改成弹窗形态 |
| 1.6 | 路由组折叠用「聚焦组 + 其余收成一行可展开」（A2A 是列表始终展开 + 搜索） | 改形 | 搜索/折叠全部/展开全部已按 A2A 补上，折叠形态保留 AIGX 式 |
| 1.7 | 「仅在途」筛选落成「仅失败」 | 改形 | AIGX 日志无在途字段，走服务端 `status=error`。**待评审：改名还是等后端补字段** |
| 1.8 | 错误处理「安全边界」分类 | 待评审 | AIGX 后端 `ErrorSettings` 无对应字段，未落空壳分类。要做得先补后端 |

## 2. 签到中心（align-checkin 上报）

| # | 项 | 状态 | 说明 / 处置 |
|---|---|---|---|
| 2.1 | 「福利通道」第 8 个 Tab（`BenefitsPanel`）：一次性/限时动作手动入口 + 保活模型链编辑 + UMID 提示条 + ZCode 套餐领取 + 验证码提示卡 | 保留 | A2A 无此 Tab。是 AIGX 的手动兜底入口 |
| 2.2 | 签到设置卡（账号间隔 `gap_ms` / 签到时区 `timezone_offset` / 全局重试间隔 `retry_gap_ms` / 台账保留 `keep_days`） | 保留 | A2A 无这四项全局设置 |
| 2.3 | 猫猫旅行定时排程（开关 + 时刻 + 立即巡检） | 保留 | A2A 的 travel 由插件面板驱动 |
| 2.4 | 活动福利卡 `CheckinCampaignCard` | 保留 | A2A 无 |
| 2.5 | 逐账号福利预览卡 `CheckinExtrasPreview` + 轮次时间线 `CheckinHistoryTimeline` | 保留 | A2A 无 |
| 2.6 | 签到后自动链 `AUTO_CHAIN`（codearts/loomy/xiaohuanxiong/workbuddy-intl，带进度条、失败即停） | 保留 | A2A 无 |
| 2.7 | 记录的「重试」类型筛档 + 起止日期 | 保留 | A2A 无这两个控件 |
| 2.8 | 文案：`OUTCOME_LABEL` 的「待重试/需人机验证」→ 改为 A2A 的「未领取/需要人工验证」 | 改形 | 按对齐口径统一用词，符合「先对齐」 |
| 2.9 | `PROVIDER_DESC` 说明表 | **被删待恢复** | 见 §0.1 |

## 3. 模型管理 / 密钥 / 日志（align-observability 上报）

| # | 项 | 状态 | 说明 / 处置 |
|---|---|---|---|
| 3.1 | `act` 三颗按钮（测试 / 计价 / 删除），A2A 是两颗 | 保留 | act 宽度已按 A2A 落 8.5% |
| 3.2 | `caps` 五枚圆点（多「音频」）+「按提供商编辑能力位」入口 | 保留 | A2A 四枚 + 能力弹窗 |
| 3.3 | `model` 列第二行 `provider_label · 上下文`（A2A 是 `name`） | 保留 | AIGX `ManagedModel` 无 `name` 字段 |
| 3.4 | 计价格第二行「定价倍率 ×N」（A2A 是计价来源 + Peak/时段倍率） | 改形 | `model_ratio` 从「倍率列」挪到计价格第二行（倍率列改读 A2A 的 credits） |
| 3.5 | 筛选行多三档：排序分段、来源、供应商、定价下拉 | 保留 | A2A 无 |
| 3.6 | `cap_sources` 可信度上色 + 逐项出处 tooltip | 保留 | A2A 用虚线底 + 覆盖小点 |
| 3.7 | 元信息覆盖弹窗的「可见性」三态（不覆盖 / 可见 / 不展示） | 保留 | A2A 由适配器算，无覆盖入口 |
| 3.8 | 网关 Key 列集 11 列超集（A2A 5 列） | 保留 | 砍列是功能删减 |
| 3.9 | 网关 Key 列名「密钥」（A2A 用不可译的 `Key`） | 保留 | 与 `KeysDocsPanel` 等处术语一致 |
| 3.10 | 日志默认时间档 `all`（A2A 默认 `today`） | 待评审 | 改今天会把历史日志静默藏起来 |
| 3.11 | 日志时间控件仍是 datetime-local（A2A 是日期字段） | 保留 | AIGX 后端契约是 `start/end` unix 秒 |
| 3.12 | 仪表盘 6 档（默认 30）/ A2A 7 档（默认 7） | 待评审 | `RangeSwitcher.tsx` 未在写入白名单；A2A 的「全部」无后端等价物 |
| 3.13 | 报表的来源统计面板（clients / apiKeys）未加 | 待评审 | AIGX 统计端点无这两维，加面板等于造假 |

## 4. 插件 / 工具 / 设置 / 代理池（align-plugins 上报）

| # | 项 | 状态 | 说明 / 处置 |
|---|---|---|---|
| 4.1 | 插件批量管理（`batchPlugins` + 进度轮询）、`FileInstallDialog`、`ProviderSettingsDialog`（settings_schema）、缺失插件面板、诊断弹窗、`builtin` 过滤 | 保留 | A2A 批量走 `plugins-batch-runner` 抽象，语义同级 |
| 4.2 | 单家包不因包停用而禁用子开关 | 保留 | 已折叠单家包，实际只剩多提供商包可预配置 |
| 4.3 | 工具页 `notReady` 404 降级、`toolPayload` 双形态解包、行内多密钥编辑器 | 保留 | A2A 用对话框式 `ProviderDialog` |
| 4.4 | 设置页「请求体大小上限、邮箱白名单、品牌自定义、语言、汇率与计价」 | 保留 | A2A 的计价是 localStorage 显示偏好 |
| 4.5 | 代理池卡片形态、`pool:<id>` 引用复制、连通性测试设置、`test-account` | 保留 | 表格化（A2A 7 列）未做，属版面重写，**待评审是否排期** |

## 5. 未受影响的 AIGX 页面（A2A 无对应功能）

`Epay`（易支付）、`Plans`、`Products`、`Redemptions`、`Wallet`、`Tickets`（工单）、`Users`、`Groups`、`IpManagement`、`NetworkLayer`、`Security`、`Notify`、`Profile`、`Prompts`、`PurgeConsole`（破甲控制台）、`LogRetention`、`Diagnostics`、`Maintenance`、`Channels`、`Gateways`、`LimitPage`、`Register`、`Login`。

> 这些是 AIGX 的商业/运维面，A2A 是自用网关没有这些模块。**对齐期间一律不动。**

---

## 6. 收口检查清单

- [ ] §0.1 `PROVIDER_DESC` 恢复并接到新结构
- [ ] §1.7 日志「仅在途」筛选的命名/后端字段二选一
- [ ] §1.8 错误处理「安全边界」是否补后端
- [ ] §2.8 签到文案改形已确认（对齐口径，非丢失）
- [ ] §3.10 / §3.12 / §3.13 三个「待评审」逐项拍板
- [ ] §4.5 代理池是否排期表格化
- [ ] 全部 `待评审` 项结清后，本台账归档
