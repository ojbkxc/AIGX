# 信息架构整合规格 v1（供应商域）

日期：2026-11-09　状态：待团队评审后开发

## 0. 用户拍板的四条决策

1. **供应商页内部**：统一列表 + 账号池明细。上游渠道与账号池合并成一张「供应商」表；账号池型供应商行内显示账号数并可下钻到账号池明细。
2. **并入供应商页的菜单**：**仅签到中心**（网络代理、插件管理保持独立菜单）。
3. **其它归并**：用户工具权限 → 用户管理；定时任务 → 系统设置；移除系统设置「账户」Tab；模型管理「元信息覆盖」并入模型目录。
4. **术语范围**：供应商域内统一（导航、供应商页、模型管理、仪表盘、日志筛选等指向上游供应商的「渠道」文案改为「供应商」）；**通知渠道、channel_id 等代码字段与后端契约不动**。

---

## 1. 术语与命名约定（全队必须一致）

| 概念 | 中文（UI） | 说明 |
|---|---|---|
| 上游 API 供应商（本规格的主角） | **供应商** | 替代现行「渠道」 |
| 账号池容器 | **账号池** | 保留，是供应商的一种凭证形态 |
| 通知出口（TG/SMTP/Slack/Webhook） | **通知渠道** | **不改**，与供应商无关 |
| 代码/DB 字段 | `channel*`、`provider` | **不改**（后端契约、API 路径、kv 键前缀） |

术语替换判定规则（供 reviewer 校验）：
- 出现在「上游/供应商」语境 → 改。
- 出现在「通知/告警/推送」语境 → 不改。
- 出现在代码注释里描述历史决策 → 可保留（不强制）。
- i18n key 本身是中文，改文案等于改 key：**同时改 zh.json 和 en.json 的 key 与 value**，并用 `npm run i18n:check` 验证。

---

## 2. 供应商页（原「渠道与账号池」）

### 2.1 导航与路由
- 侧栏菜单文案：`渠道与账号池` → `供应商`。path 保持 `/channels`（避免大量书签/e2e 失效）。
- 页面主标题：`供应商`。页脚副标题改为说明「API Key 型供应商 + 账号池型供应商」。
- 旧路由保持：
  - `/accounts` → `/channels`
  - `/account-sessions` → `/channels?tab=accounts`
  - `/mappings` → `/channels`
  - `/diagnostics` → `/channels?tab=health`

### 2.2 Tab 结构调整
现状 4 Tab：`upstream` / `accounts` / `health` / `gateway`。

调整为 3 Tab：
| key | 文案 | 内容 |
|---|---|---|
| `providers`（原 `upstream`） | 供应商 | 统一列表（见 2.3） |
| `accounts` | 账号池 | AccountSessions（保持现状） |
| `health` | 健康 | Diagnostics（保持现状） |
| ~~`gateway`~~ | — | **移除**。GatewayRuntimePanel 降级为供应商页顶部的一个可折叠只读信息条，或直接在「系统设置 → 网关」查看（见 2.4） |

兼容性：
- URL `?tab=upstream` 仍可用（映射到 `providers`），避免旧链接 404。
- URL `?tab=gateway` → 重定向到 `?tab=providers` 并在顶部展示网关信息条。
- Tab 按钮顺序：供应商 / 账号池 / 健康。

### 2.3 统一供应商列表
数据源：`api.listChannels()` 全量（不再过滤 `channel_type !== 'account'`）+ `api.accountSessionStats()` 获取账号数。

列（在现有 `CH_COL_ORDER` 基础上调整）：
| key | 标签 | 说明 |
|---|---|---|
| id | ID | 不变 |
| name | 名称 | 不变 |
| kind | 形态 | **新增**：API Key / 账号池 |
| accounts | 账号数 | **新增**：仅账号池型显示 `enabled/total`（来自 stats），API Key 型显示 `—` |
| status | 状态 | 不变 |
| models | 模型 | 不变 |
| priority | 优先级 | 不变（账号池型可隐藏该列） |
| weight | 权重 | 不变 |
| balance | 余额 | 不变 |
| response | 响应 | 不变 |
| testtime | 测试时间 | 不变 |
| actions | 操作 | 见下 |

行操作差异：
- API Key 型：编辑 / 测试 / 删除 / 查看映射（现状）。
- 账号池型：**管理账号**（跳 `/channels?tab=accounts` 并带上供应商过滤参数 `?provider=<channel_id>`） / 查询余额 / 删除。不再提供「编辑 Key」（账号池的凭证在账号页管理）。

过滤与搜索：
- 顶部筛选增加「形态」下拉：全部 / API Key / 账号池。
- 搜索框保持（后端 search 已覆盖 name/type/base_url/models）。
- **不要**在前端再做一次 `channel_type !== 'account'` 过滤。

账号池 Tab 的下钻联动：
- `AccountSessions` 接收 `initialChannelFilter?: string`（从 URL `?provider=` 读）。
- 现行 `channelFilter` 是 `useState('')` 且无 UI 切换（AccountSessions.tsx:424）。本次**只接入 URL 初值 + 保留现有内部逻辑**，不新增筛选 UI（避免膨胀）。
- 从供应商行点「管理账号」→ `navigate('/channels?tab=accounts&provider=<id>')`。AccountSessions 组件读 URL 初始化 `channelFilter`。

### 2.4 网关 Tab 的处置
现状 `GatewayRuntimePanel` 是只读展示 + 指引去系统设置改。

**决策：移除独立 Tab**，把 `GatewayRuntimePanel` 渲染在供应商 Tab 的顶部，包在可折叠 `<details>` 里（默认收起，标题「网关运行参数（只读）」）。这样：
- 用户在配供应商时仍能看到网关监听口径。
- 少一个 Tab，减少割裂。
- 编辑入口仍唯一（系统设置 → 网关），不产生第二份表单。

---

## 3. 签到中心并入供应商

- 侧栏移除「签到中心」菜单项（`/checkin`）。
- 路由 `/checkin` → 重定向 `/channels?tab=checkin`。
- 供应商页增加第 4 个 Tab：`checkin` / 文案「签到」。
- `CheckinCenter` 增加 `embedded?: boolean`（与 Channels/AccountSessions 同款），`embedded=true` 时隐藏自身 `<h1>签到中心</h1>` 标题区（保留页内 4 个子 Tab：签到中心/定时签到/签到记录/福利通道）。
- 理由：签到是账号池账号的每日额度领取，与供应商强耦合；页内已有 4 个子 Tab，嵌入后是「Tab 套 Tab」，必须保留二级 Tab 而不是拍平。

---

## 4. 其它归并

### 4.1 用户工具权限 → 用户管理
- 侧栏移除「用户工具权限」（`/user-tools`）。
- 路由 `/user-tools` → 重定向 `/users?tab=tools`。
- `Users.tsx` 增加 Tab 结构（当前无 Tab）：`用户列表` / `工具权限`。
  - 第一个 Tab 是现有用户表格。
  - 第二个 Tab 渲染 `<UserTools embedded />`（需要新增 `embedded` prop，去掉自带的 `Card title` 重复标题）。
- Tab 状态走 URL `?tab=`，与 Keys/ModelManage 同构。

### 4.2 定时任务 → 系统设置
- 侧栏移除「定时任务」（`/scheduled-tasks`）。
- 路由 `/scheduled-tasks` → 重定向 `/settings?tab=scheduled`。
- `Settings` 增加分类 `scheduled` / 文案「定时任务」，渲染 `<ScheduledTasks embedded />`（需要新增 `embedded` prop，去掉自带 `<h1>`）。
- `SETTINGS_CATEGORIES` 追加该项，放在「数据」之后、「通知」之前（语义相邻）。

### 4.3 移除系统设置「账户」Tab
- `SETTINGS_CATEGORIES` 移除 `account`。
- `SettingsTab` 类型移除 `'account'`，对应渲染块删除。
- 「账户安全（修改密码）」与个人中心 `/profile` 重复，删除后引导：在设置页顶部或移除处不留残链。`/settings?tab=account` → 重定向 `/profile`。
- 注意：`Profile.tsx` 已有完整改密 + `must_change_password` 清除逻辑（Profile.tsx:175），功能不丢。
- 设置页「反馈与需求」卡片在 `account` 块里，需迁移到「通用」Tab 或保留在「账户」被删后放「通用」底部。

### 4.4 元信息覆盖并入模型目录
- `ModelManage` 移除 `meta` Tab（`MainTab` 从 `catalog|meta|ratio` → `catalog|ratio`）。
- 目录行的「能力」列点击已打开 `openMetaForRow` 弹窗（ModelManage.tsx:721），已覆盖归属方/上下文/能力/思考等级。
- 移除 `meta` Tab 后，需确认：
  - `openMetaCreate`（新增覆盖）入口迁到目录工具栏（现在在 meta Tab 头部）。可在「+ 添加模型」旁边加「+ 新增元信息覆盖」，或在自定义模型创建流程里带入。
  - `overrides` 列表页（列出所有覆盖项、可删）**不丢**：改为在目录工具栏加一个「覆盖管理」入口，打开一个弹窗列出全部覆盖项（复用现有表格渲染）。
- `?tab=meta` → 重定向 `?tab=catalog` 并自动打开覆盖管理弹窗（或直接重定向，不自动开弹窗；推荐后者，简单）。

---

## 5. 模型管理：映射列显示具体模型名（独立需求）

现状（截图已验证）：
- identity chip：`GLM-5.2 [原样透传] ×`
- 真实映射 chip：`alias-free [开关] ×`（目标模型只在 tooltip：`alias-free → unpriced-model`）

用户要求：**不要只写「原样透传」，要写出具体模型名称**。

设计（保持信息密度，不撑爆列宽）：
- identity chip：显示 `GLM-5.2 → GLM-5.2`，右侧小标签从「原样透传」改为「原样」或保留「原样透传」。用户能看到具体模型名与方向。
  - 更优：`GLM-5.2 → 同名（原样）`。
- 真实映射 chip：显示 `alias-free → unpriced-model`（对外名 → 目标名），开关与 × 保留。目标名用弱化色/小字号，避免主次不分。
- chip 的 `title` 仍保留完整链路 + 思考等级。
- CSS：`.mm-alias` 的 `max-width` 由药丸自适应，内部 `mm-alias-text` 已有 ellipsis；新增 `.mm-alias-arrow` / `.mm-alias-target` 样式（弱化色，9~10px，等宽字体）。
- 关闭态（`is-off`）：保持整条压淡 + 划线；箭头与目标名同步压淡。
- e2e `models.spec.ts` 的 identity 断言需同步：
  - 现在断言 `identity` 含 `unpriced-model` 与标签 `原样透传` → 改为断言含 `unpriced-model → unpriced-model`（或新结构）。
  - 真实映射 chip 断言新增「含目标名」。
- 注意：identity chip 的 `×` 仍只是「隐藏这条提示」（localStorage `aigx-mm-hide-identity`），不改任何映射。

---

## 6. 术语替换清单（供应商域）

### 6.1 必改（UI 文案）
| 位置 | 现值 | 新值 |
|---|---|---|
| Sidebar.tsx:44 | 渠道与账号池 | 供应商 |
| Gateways.tsx:59 | h1 渠道与账号池 | 供应商 |
| Gateways.tsx:71-74 | Tab 上游渠道 | Tab 供应商 |
| Channels.tsx:1357 | h1 渠道管理 | 供应商列表（embedded 时隐藏，但单独渲染仍要有） |
| Channels.tsx:1358 | 管理上游 AI 渠道… | 管理上游 AI 供应商… |
| AccountSessions.tsx:2074 | h1 账号池管理 | 账号池（保持，账号池不是供应商） |
| ModelManage.tsx:1722 | 渠道与账号池的模型统一在此管理 | 供应商与账号池的模型统一在此管理 |
| Dashboard.tsx:294 | 管理渠道 | 管理供应商 |
| Dashboard.tsx:273 | Top 渠道（{{range}}） | Top 供应商（{{range}}） |
| Dashboard.tsx:283 | 渠道健康状态 | 供应商健康状态 |
| Diagnostics.tsx:143 | h1 系统诊断 | 系统诊断（保留；它是诊断面板名，不是供应商） |
| Diagnostics.tsx:144 | 渠道健康、用量概况… | 供应商健康、用量概况… |
| Logs.tsx:132/692/743 | 渠道 / 渠道 ID | 供应商 / 供应商 ID（筛选标签） |
| Channels.tsx 内所有「渠道」按钮/空态/提示 | 渠道 | 供应商（逐条判断上游语境） |
| AccountSessions.tsx 内「账号型渠道」 | 账号型渠道 | 账号池（如果指容器本身） |
| Agent.tsx:279/285/288/291 | 渠道 | 供应商（「该模型的渠道/账号」→「该模型的供应商/账号」） |
| GlobalSearch.tsx:98 | 实体 sub 兜底「渠道」 | 供应商 |
| Keys.tsx:139/180/503 | 渠道页（注释） | 可不改（注释） |
| KeysDocsPanel.tsx:218 | 渠道管理或账号池 | 供应商或账号池 |

### 6.2 不改（易误伤）
- `i18n` 里含「通知渠道」的 key（如 zh.json:661）。
- `Logs.tsx:92` 的 `chat_test: '渠道调试'` → 这是请求类型标签，指「渠道调试」这个功能，可改「供应商调试」但需确认后端是否也返回该字符串（若后端返回中文，前端 key 必须匹配）。**先查后端**，不一致就不改。
- 所有 `.css` / `.ts` 注释里的「渠道」。
- 所有 API 路径、类型字段、localStorage 键、kv 前缀。

### 6.3 i18n 双文件同步
- zh.json 与 en.json 的 **key** 与 **value** 都要改（key 是中文文案本身）。
- 新增 key（如「供应商」）要同时进两份。
- 跑 `npm run i18n:check` 与插值校验。

---

## 7. 验收口径

### 7.1 单测/静态
- `npx tsc --noEmit` 0 错。
- `npx eslint` 零告警。
- `npx vitest run` 全绿（当前 124）。
- `npm run i18n:check` 0 缺失 + 插值校验 PASS。

### 7.2 e2e（新增/修改）
- `smoke.spec.ts:147` 的导航点击 `渠道与账号池` → 改 `供应商`。
- 新增 `e2e/ia-consolidation.spec.ts`：
  - 侧栏不含「渠道与账号池 / 签到中心 / 用户工具权限 / 定时任务」。
  - 侧栏含「供应商」「用户管理」「系统设置」「模型管理」。
  - `/account-sessions` → `/channels?tab=accounts`。
  - `/checkin` → `/channels?tab=checkin`。
  - `/user-tools` → `/users?tab=tools`。
  - `/scheduled-tasks` → `/settings?tab=scheduled`。
  - `/settings?tab=account` → `/profile`。
  - `/model-manage?tab=meta` → `/model-manage?tab=catalog`。
  - 供应商 Tab 只有 3 个（供应商/账号池/健康）+ 第 4 个签到（共 4 个），无「网关」Tab。
  - 供应商表含「形态」列；账号池型行有「账号数」与「管理账号」按钮。
- `models.spec.ts` identity/mapping 断言按 §5 更新。
- 全量 playwright 必须 0 failed（已知 3~6 条 flaky 单独复跑通过）。

### 7.3 视觉验收
- 供应商页 desktop 亮/暗 + 390px 移动端截图。
- 映射列 chip 在 1600px 与 390px 下不溢出、不折断。
- 签到 Tab 嵌入后二级 Tab 不重叠。

### 7.4 生产
- CI 全绿 → tag → 部署 → 五探针 200 → 公网密钥 200（ttt 用 deepseek，ttt2 用 glm-5.3-flash）。

---

## 8. 实施顺序与归属（团队分工）

| 序号 | 任务 | 负责人 | 依赖 |
|---|---|---|---|
| 1 | 供应商页骨架：3+1 Tab、菜单改名、网关信息条、旧路由兼容 | `ia-core` | 无 |
| 2 | 统一供应商列表：形态列、账号数列、行操作、URL 下钻 | `ia-core` | 1 |
| 3 | 签到中心嵌入（embedded + 路由重定向） | `merge-page` | 1 |
| 4 | 用户工具→用户管理 Tab；定时任务→设置分类 | `merge-page` | 无 |
| 5 | 移除设置账户 Tab；元信息覆盖并入目录 | `merge-page` | 无 |
| 6 | 映射列显示具体模型名 + e2e 同步 | `map-chip` | 无 |
| 7 | 术语统一替换（zh/en + 源码） | `term-sweep` | 1,2 |
| 8 | e2e 新增/修改 + 全量回归 | `verify` | 1-7 |
| 9 | 发版 + 部署 + 验收 | lead | 8 |

并行策略：
- `ia-core`（1→2）、`merge-page`（3→4→5）、`map-chip`（6）三条线可并行。
- `term-sweep` 等 1、2 落地后再动，避免与 `ia-core` 同文件冲突（Sidebar/Gateways 是热点文件）。
- `verify` 最后统一跑。

---

## 9. 风险与约束

1. **文件冲突**：`Sidebar.tsx`、`Gateways.tsx`、`Channels.tsx` 被 `ia-core` 与 `term-sweep` 同时改 → term-sweep 必须等 ia-core 完成并 rebase；或由 ia-core 一次性完成自己文件内的术语替换。
2. **i18n key 是中文**：改文案即改 key，两份 json 必须同步，否则英文界面回退中文。
3. **Tab 嵌套**：签到中心自带 4 个子 Tab，嵌入后 URL 只有父 `?tab=checkin`，子 Tab 用 localStorage 记忆（现状即如此），不做 URL 二级参数（避免复杂化）。
4. **e2e Server 生命周期**：全量 e2e 建议单独端口（如 E2E_PORT=4174），避免端口 4173 中途死亡导致大批 CONNECTION_REFUSED 假失败（2026-11-09 实际发生过）。
5. **不改后端**：本规格全部为前端信息架构调整；`channel_type`、`provider`、API 路径、kv 键一律不动。若发现必须改后端（如 stats 接口缺失），先停下来报告。
6. **不做的事**：不动通知渠道文案；不动 `channel_id` 等字段名；不合并网络代理/插件管理（用户已明确不并入）。

---

## 10. 待确认（开发中遇到再问，不阻塞开工）

- 「渠道调试」（chat_test）标签的中文来源在前端还是后端 → 若前端固定中文，可改；若后端返回，保持。
- `Diagnostics` 的 h1「系统诊断」是否也改「供应商健康」→ 建议保留「系统诊断」（面板名），仅改描述里的「渠道」。
---

# 追加规格 v2（2026-11-09 晚，用户新需求）

## 11. 金额统一两位小数

### 11.1 适用范围（判定规则）
- **货币金额**：费用 / 成本 / 利润 / 充值金额 / 应付金额 / 套餐售价 / 缓存节省 → 统一 `两位小数`（千分位 + 固定两位）。
- **模型单价（$ / 1k tokens）**：属于**费率**不是金额，保持现有自适应精度（<0.01 四位、<1 三位、其余两位）。强制两位会把 $0.001 显示成 $0.00，属于信息丢失，**不改**。
- **配额（quota）**：`fmtQuota` 的万/亿缩写口径是配额单位不是货币，**不改**。

### 11.2 落点
| 文件 | 现状 | 改法 |
|---|---|---|
| `lib/format.ts` `fmtCost` | `¥${fmtInt(Math.round(n))}` | 固定两位 + 千分位；负数格式 `-¥1.00` |
| `pages/Logs.tsx` 费用列 | `¥{l.cost}` | `fmtCost(l.cost, {{lang}})` |
| `components/LogDetailDialogs.tsx` | 费用/渠道成本/利润 `¥${...}` | 同上（利润为负时 `-¥`） |
| `pages/Wallet.tsx` 充值档位 | `¥{v}` | `¥{Number(v).toFixed(2)}` |
| `pages/Wallet.tsx` 应付 | `¥{payMoney(...)}` | `.toFixed(2)` |
| `pages/Plans.tsx` / 订单表 | 已是两位 | 不动，仅确认 |
| `pages/ModelManage.tsx` `fmtPrice` | 自适应 | **不改**（见 11.1） |

### 11.3 全出口一致性
`fmtCost` 是仪表盘唯一货币出口：StatCards / TopSources / UserRanking / TrendPanels / Heatmap tooltip / TrendBars 轴标签自动跟随，不逐处改。

## 12. 仪表盘重新规划

### 12.1 快捷操作
- **移到最上方**：紧贴 page-header 之下、实时卡之上，渲染为一条紧凑工具栏（不占整卡高度）。管理员与普通用户视图都移。
- 管理员按钮文案「管理渠道」→「管理供应商」，链接仍 `/channels`。

### 12.2 布局与去重（管理员）
固定顺序：
1. header（标题 + 时间范围 + 导出 + 刷新）
2. **快捷操作条**
3. 实时四卡（RPS / 活跃用户 / 活跃供应商 / 平均延迟）
4. 今日用量 / Token 统计 / 本月限额 三卡
5. **区间概览六卡**（总请求 / 成功请求 / 总 Token / 总成本 / 活跃天数 / 连续天数）
   - 删除三张重复卡：`Top 模型`（ModelPanels 已有）、`缓存节省估算`（CachePanels 已有）、`渠道`（健康面板已有）
6. **供应商健康**（只显示启用；重做，见 §13）
7. 消费趋势 + Token 趋势
8. 缓存效率（CachePanels）
9. 模型分布 + Top 模型（ModelPanels）
10. Top 供应商 + 用户消费排行
11. 活跃热力图

普通用户视图同样把快捷操作移到最上方，其余顺序不变。

### 12.3 文案
「Top 渠道（…）」→「Top 供应商（…）」；「渠道健康状态」→「供应商健康状态」；「活跃渠道」→「活跃供应商」；「暂无渠道数据」等同步。

## 13. 供应商健康面板重做

### 13.1 数据过滤（核心）
- `ChannelHealthRow` 增加 `status?: string`（后端 `handle_channel_health` 已返回 `status`，前端类型未接）。
- **只显示 `status === 'enabled'` 的供应商**；停用的不参与统计、不渲染。
- 若存在停用供应商，在标题区右侧显示一行小字「已隐藏 {{n}} 个已停用供应商」。

### 13.2 布局（更直观）
放弃大卡片网格，改为**紧凑表格**（`.table-wrapper` 横向滚动兜底）：
| 供应商 | 状态 | 成功率 | 错误率 | 平均延迟 | 请求数 | 最近错误 |
- 状态徽章：健康/警告/异常/熔断/半开（沿用现有 CAT_STYLE 配色）。
- 成功率、错误率带颜色（异常红/警告黄/正常主色）。
- 默认按严重度排序（熔断 → 异常 → 半开 → 警告 → 健康），同级按错误率降序；表头可点击排序（错误率/请求量/延迟/成功率）。
- 顶部保留汇总 chips（健康/警告/异常/熔断）并**可点击筛选**；保留搜索框。
- 空态：无启用供应商 → 「暂无启用中的供应商」；筛选无结果 → 「无匹配供应商」。
- 数据新鲜度文案保留。

### 13.3 移动端
表格横向可滚动，不折行、不溢出；汇总 chips 可换行。

## 14. 验收追加
- `fmtCost(1234.5) === '¥1,234.50'`；`fmtCost(-1.5) === '-¥1.50'`；`fmtCost(null) === '—'`（补单测）。
- 日志页费用列、详情弹窗费用/渠道成本/利润均两位小数。
- 钱包档位/应付两位小数。
- 仪表盘顺序符合 §12.2；三张重复卡消失。
- 健康面板只出现 `status=enabled` 的供应商；停用数量提示正确。
- e2e：仪表盘/日志相关既有断言更新（文案与结构变化）。
