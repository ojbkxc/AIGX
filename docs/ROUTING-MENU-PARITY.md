# 路由菜单 / 网关菜单 · 一比一对齐审计（AIGX ↔ Agent2API）

> 审计日期：2026-11-23（UTC+8）
> 目标：①定位并给出「去掉 AIGX 设置里多余网关菜单」的可执行口径；②把 AIGX「路由菜单」照 Agent2API 一比一对齐的方案。
> 纪律：**只读**，未改动任何代码（本文件是唯一产物，未提交）。AGPL 参考树仅比对 UI 结构与菜单文案，**未拷贝任何源码**。
> 标签：[OK] 语义等价 / [DIFF] 两侧都有但结构或文案不同 / [MISSING] A2A 有 AIGX 无 / [EXTRA] AIGX 有 A2A 无（或反之，注「AIGX 独有」）/ [待确认] 需 lead 拍板。

## 0. 口径与文件简写

- **A2A 根**：`D:\GitHub\Agent2API`
  - `isl` = `desktop-tauri/ui-islands/src/islands/`（桌面岛：路由/设置）
  - `ui` = `desktop-tauri/ui/`（外壳侧栏）
- **AIGX 根**：`D:\GitHub\AIGX`
  - `SIDEBAR` = `frontend/src/components/Sidebar.tsx`（顶级侧栏）
  - `SET` = `frontend/src/pages/Settings.tsx`（系统设置页）
  - `RT` = `frontend/src/pages/Routing.tsx`（路由页）
  - `GW` = `frontend/src/pages/Gateways.tsx`（路由 `/channels` 的「供应商」页，文件名历史遗留为 Gateways）
  - `RTAPI` = `frontend/src/api/endpoints/routing.ts`

---

## 1. 「网关菜单」定位（重点，含张力）

### 1.1 AIGX 现有所有「网关」出现处（逐一取证）

| 位置 | 是否有「网关」菜单/入口 | 证据 |
|---|---|---|
| 顶级侧栏（导航项） | **无**「网关」项 | `SIDEBAR:36-63` navItems 全量；`SIDEBAR:85-131` 三组分组。无 label 为「网关」的条目 |
| 系统设置（左侧分类） | **有，且是唯一菜单级「网关」**：分类 `{ key: 'gateway', label: '网关' }` | `SET:200`；渲染 `SET:1199-1276` |
| 供应商页（`/channels` = Gateways） | **无**「网关」Tab（历史 Tab 已于 2026-11 移除）；仅剩可折叠**只读**信息条「网关运行参数（只读）」，编辑入口指向系统设置→网关 | `GW:26-27,112-116`；`GW:34` `TAB_KEYS = providers/accounts/health/checkin`；`GatewayRuntimePanel.tsx:7`（只读+单一编辑入口） |
| routing.ts 注释里的「设置→网关→…」 | 是对照说明，不是菜单 | `RTAPI:114,130,135,200` |

**结论**：AIGX 中**菜单级「网关」只有一处 = 系统设置 → `网关` 分类**（`SET:200`）。没有侧栏顶级「网关」项，也没有供应商页「网关」Tab（已下线为只读信息条，编辑入口唯一）。

该 `网关` 分类当前挂载的面板（`SET:1199-1276`，共 17 块）：

`GatewayPanel`(监听地址/端口/CORS/反代信任)、`NetworkLayer`(网络层)、`SanitizePanel`(指纹脱敏)、`ClineHeadersPanel`(Cline 伪装头)、`ClientIdPanel`(客户端识别)、`GuardrailPanel`(内容护栏)、`ZcodeCaptchaPanel`(验证码令牌池)、`PromptPanel`(系统提示词)、`PromptRulesPanel`(提示词规则)、`WebToolsPanel`(网页工具调用)、`DebugTrafficPanel`(调试模式)、`StreamCompatPanel`(流兼容)、`ClientCompatPanel`(客户端兼容)、`QueuePanel`(排队)、`AutoContinuePanel`(自动续写)、`ModelMetadataPanel`(models.dev 元数据)、请求体大小上限。

### 1.2 A2A 的「网关」在哪

- **顶级侧栏无「网关」**：`ui/sidebar-layout.js:71-100` MENUS 列表——注意 key=`gateway` 的那一项**显示名是「模型管理」**（`:84`），不是「网关」。
- **系统设置有 `网关` 分类**：`isl/settings-model.ts:507` `{ id: 'gateway', label: t('网关'), icon: 'traffic' }`；内容 `isl/settings-page.tsx:1668-1674` + `GatewayPane`(约 `:680-904`)。
- 重要注释 `settings-model.ts:481`：「**重试**」「**超时**」已从「网关」里拆出来独立成菜单（原先挤在网关一栏）。
- A2A `网关` 分类面板清单（`settings-page.tsx:1668-1674,680-904`）：
  `网络访问`(NetworkAccessPanel)、`客户端识别`(ClientIdentificationPanel)、`排队等待`、`图片压缩`、`验证码令牌池`(插件门控)、`models.dev 模型元数据`、`指纹脱敏`(SanitizePanel)、`网页工具调用`(WebToolsPanel)、`开发者角色兼容`、`第三方模型回答完自动折叠过程`、`Cline 伪装头`(插件门控)、`系统提示词`(PromptPanel)、`调试模式`。
- 另注 `settings-page.tsx:1671` 注释：`图片兼容` 已从设置**搬到「路由 → 特殊请求路由 → 图片兼容」**（与账号路由同层，都在模型解析时改写）。

### 1.3 关键结论（张力，必须 lead 拍板）

**A2A 也有一个 Settings `网关` 分类**（`settings-model.ts:507`），且「伪装头 / 系统提示词 / 指纹脱敏 / 调试模式」在 A2A 恰恰**就放在「设置 → 网关」下**（与 AIGX 现状一致）。
因此「去掉 AIGX 设置里的网关菜单」与「路由照 A2A 一模一样」**并不天然自洽**：A2A 并没有去掉网关分类。故「多余网关菜单」存在三种口径，需 lead 指认（见 §6.2）：

- **口径 A（推荐）**：不是删分类骨架，而是**清理网关分类里的冗余/A2A 没有的 EXTRA 面板**，让 AIGX 网关分类清单与 A2A `网关` 一比一（见 §2）。
- **口径 B（字面删分类）**：删除 Settings `网关` 分类、把面板迁到「路由」菜单下。但 A2A 把伪装头/提示词/脱敏/调试放在 `设置→网关`，这样做会**偏离**「一模一样」。
- **口径 C（去重复运行参数）**：删供应商页残留的「网关运行参数（只读）」信息条。其实现状已是**只读 + 单一编辑入口**（`GW:112-116`、`GatewayRuntimePanel.tsx:7`），并非第二份可编辑表单，A2A 也无对应物——**「多余」证据较弱**。

---

## 2. AIGX 网关分类面板 vs A2A `网关` 分类（逐面板）

| AIGX 面板（`SET:1199-1276`） | A2A 对应 | 判定 | 证据 |
|---|---|---|---|
| GatewayPanel（监听地址/端口/CORS/反代信任） | NetworkAccessPanel(网络访问) + CORS 归「安全」 | [DIFF] | AIGX `GatewayPanel.tsx:9-13`；A2A `settings-page.tsx:1670`（网络访问）、`:1674`（安全 CORS） |
| NetworkLayer（网络层） | NetworkAccessPanel | [OK/DIFF] | — |
| SanitizePanel（指纹脱敏） | SanitizePanel（指纹脱敏） | [OK] | A2A `settings-page.tsx:119`(GatewayPane 内) |
| ClineHeadersPanel（Cline 伪装头） | ClineHeadersPanel（插件门控 `upstream_headers`） | [OK] | A2A `settings-page.tsx:194` |
| ClientIdPanel（客户端识别） | ClientIdentificationPanel | [OK] | A2A `settings-page.tsx:1672` |
| GuardrailPanel（内容护栏 输入/输出黑名单） | **无独立面板**（散在 错误处理「安全边界」/ 提示词「内容拦截」） | [EXTRA] | A2A 无对应 `settings-page.tsx` 面板 |
| ZcodeCaptchaPanel（验证码令牌池） | 验证码令牌池（插件门控 `captcha_pool`） | [OK] | A2A `settings-page.tsx:745-770` |
| PromptPanel（系统提示词） | PromptPanel（系统提示词）+ 匹配规则 | [OK] | A2A `settings-page.tsx:196` |
| PromptRulesPanel（提示词规则，独立一块） | 含在 PromptPanel「匹配规则」内 | [DIFF]（AIGX 拆成两块） | A2A `PromptPanel` 单页含规则 |
| WebToolsPanel（网页工具调用） | WebToolsPanel | [OK] | A2A `settings-page.tsx:120` |
| DebugTrafficPanel（调试模式） | 调试模式 | [OK] | A2A `settings-page.tsx:877-901` |
| StreamCompatPanel（流兼容） | **无独立面板** | [EXTRA] | A2A 无对应 |
| ClientCompatPanel（客户端兼容） | 开发者角色兼容 + 第三方回答完自动折叠（两个开关） | [DIFF] | A2A `settings-page.tsx:122-193` |
| QueuePanel（排队） | 排队等待 | [OK] | A2A `settings-page.tsx:685` |
| AutoContinuePanel（自动续写） | **无独立面板** | [EXTRA] | A2A 无对应 |
| ModelMetadataPanel（models.dev 元数据） | models.dev 模型元数据 | [OK] | A2A `settings-page.tsx:774` |
| 请求体大小上限（请求体上限输入） | **无**（A2A 不在此栏） | [EXTRA] | A2A 无对应 |

> 小计：`[OK]` 6 块、`[DIFF]` 3 块、`[EXTRA]` 4 块（Guardrail / StreamCompat / AutoContinue / 请求体上限）。若要「网关菜单与 A2A 一致」，这 4 块 EXTRA 是「多余」的第一候选——删除或并轨。

---

## 3. A2A 路由菜单结构（源码核实）

**真相源**：`isl/routing-page.tsx`（页面）、`isl/routing-rail.tsx`（左 rail）、`isl/routing-config-panel.tsx`、`routing-group-panel.tsx`、`routing-exec-timeline.tsx`、`ui-shared/routing-errors-panel.tsx`。

### 3.1 顶部模式分段（SegmentedControl）
- `routing-rail`… 其实在 **`routing-page.tsx:228-241`**：`路由模式` = **路由配置**（带未保存计数徽标）/ **请求回放**。
- 右侧 policy 文案 + actions：`请求日志`(仅回放) / `会话粘滞` / `刷新|重新读取`（`routing-page.tsx:242-254`）。

### 3.2 配置视图左 rail（`routing-rail.tsx:38-83`，顺序即界面顺序）
```
路由配置  [计数 N]                       (+ 新增按钮)
── 默认规则 ──
   全局默认            (globe)
   错误处理            (refresh)
── 特殊请求路由 ──
   压缩上下文          (database)
   图片兼容            (eye)
   标题生成            (spark)
── 模型路由 ──   （有已配置模型时出现，每模型一项）
── 自定义路由组 ──（每个组名一项）
[ 新增路由配置 ]（footer，虚线按钮）
```
- 特殊三项定义 `routing-rail.tsx:20-24` `SPECIAL_RAIL`。
- 每项可带「未保存」脏点 `routing-rail-dirty`（`routing-rail.tsx:44,71`）。

### 3.3 配置视图右侧内容区（`routing-page.tsx:255-296`）
- `全局默认 / 模型路由` → `RoutingConfigPanel`（策略 / 粘滞 / 参与方式 / 账号组 / 全局保护）。
- `错误处理` → `RoutingErrorsPanel`，三段子 Tab：**处理规则 / 失败保护 / 安全边界**（`ui-shared/routing-errors-panel.tsx:80-82`）。
- `特殊请求路由` → `CompactSpecialPanel / VisionSpecialPanel / TitleSpecialPanel`；选中时右栏并排 `RoutingSpecialHits`（最近命中）。
- `路由组` → `RoutingGroupPanel`。
- 画布形态：账号路由用点阵画布；特殊/错误用平底画布（`routing-page.tsx:263`）。

### 3.4 回放视图（`routing-page.tsx:277-289`）
- 左：`RoutingRequestList`（过滤 请求模型 / 时间范围 / 仅在途 / 搜索）。
- 右：`RoutingExecTimeline`（执行时间线：选路情况、模型改写、候选顺序、被跳过、最终承载、尝试明细）。

---

## 4. AIGX 路由菜单现状

- **左 rail 扁平五项**（`RT:88-94`）：`配置 / 回放 / 压缩 / 标题 / 视觉`（`Routing.tsx:1-26` 注释说明共 `configured` 五项）。
- **配置视图**（`RT:1-17` 注释、`RT:411-773`）＝多规则总开关 + 基础策略(`RoutingPanel`) + **分层档案**（rail：全局 / 按模型 / 路由组，`RT:427-484`）+ 候选与命中层预演(`/routing/queue`，`RT:590+`) + 能力需求/指定组预演与逐字段来源(`/routing/preview`，`RT:665+`) + `RoutingWorkspace` 模型覆盖对比 + `RouteSticky` 粘滞（配置视图底部）。
- **回放视图**＝`RouteReplayPanel`（按请求 ID 回放 + 执行时间线）+ `RoutingFlow`（请求流向）+ `RoutingTrace`（请求跟踪）。
- **特殊三块**＝`CompactRoutes / TitleRoutes / VisionCompat`，各右侧并排 `SpecialHits`（最近命中）。
- **错误处理**：**不在路由页**；AIGX 把它放在 **系统设置 → 重试 / 超时** 两个独立分类（`SET:201-202`）。
- **模型/路由组**：是三态「分层档案」的 layer（在配置视图内），**不是左 rail 一级项**；无 A2A 式「新增路由配置」创建对话。

---

## 5. 路由菜单逐项对照

| 维度 | A2A | AIGX 现状 | 判定 | 证据 |
|---|---|---|---|---|
| 顶部模式分段（路由配置/请求回放） | 有，SegmentedControl + 脏计数 | 无；配置/回放是扁平 rail 两项 | [DIFF] | A2A `routing-page.tsx:228-241`；AIGX `RT:88-94` |
| 左 rail 分组结构 | 默认规则 / 特殊请求路由 / 模型路由 / 自定义路由组（分组标题 + 项） | 扁平五项，无分组标题 | [DIFF] | A2A `routing-rail.tsx:48-72`；AIGX `RT:88-94,811-830` |
| 全局默认（作为 rail 一级项） | 有 | 合在「配置」内的基础策略 + 分层档案 | [DIFF] | A2A `routing-rail.tsx:51-53` |
| 错误处理（rail 一级项 + 处理规则/失败保护/安全边界） | 有 | 无（散在 Settings 重试/超时） | [MISSING] | A2A `routing-rail.tsx:54-56`、`routing-errors-panel.tsx:80-82`；AIGX `SET:201-202` |
| 压缩上下文 | rail 特殊项 | 「压缩」(`CompactRoutes`) | [DIFF] 命名 | A2A `routing-rail.tsx:20`；AIGX `RT:91` |
| 图片兼容 | rail 特殊项 | 「视觉」(`VisionCompat`) | [DIFF] 命名（视觉↔图片兼容） | A2A `routing-rail.tsx:21`；AIGX `RT:93` |
| 标题生成 | rail 特殊项 | 「标题」(`TitleRoutes`) | [DIFF] 命名 | A2A `routing-rail.tsx:22`；AIGX `RT:92` |
| 模型路由（每模型 rail 项） | 有（独立配置项） | 无独立项；配置内「按模型」层 | [DIFF] | A2A `routing-rail.tsx:60-65`；AIGX `RT:452-460` |
| 自定义路由组（rail 项） | 有 | 无独立项；配置内「路由组」层 | [DIFF] | A2A `routing-rail.tsx:66-72`；AIGX `RT:479-485` |
| 新增路由配置（创建对话） | 有 `RoutingCreateDialog` | 无（用分层档案加层/组） | [MISSING/待确认] | A2A `routing-rail.tsx:74`、`routing-create-dialog.tsx` |
| 请求回放形态 | 左「最近请求」列表 + 右 exec timeline | `RouteReplayPanel`（按请求 ID）+ Flow + Trace | [DIFF] | A2A `routing-page.tsx:277-289`；AIGX `RT:14-15` |
| 特殊路由右侧「最近命中」 | 有 `RoutingSpecialHits` | 有 `SpecialHits` | [OK] | A2A `routing-page.tsx:274-275`；AIGX `RT:844` |
| 会话粘滞 | 顶部按钮弹窗 | `RouteSticky` 在配置视图底部 | [DIFF] 位置 | A2A `routing-page.tsx:245`；AIGX `RT:13` |
| AIGX 独有：预演（候选/能力需求/逐字段来源） | 无（回放侧看结果） | 有 | [EXTRA/AIGX 独有] | AIGX `RT:590,665` |
| AIGX 独有：RoutingWorkspace 覆盖对比 | 无 | 有 | [EXTRA/AIGX 独有] | AIGX `RT:60` |
| AIGX 独有：RoutingFlow / RoutingTrace | 无（exec timeline 内含） | 有 | [EXTRA/AIGX 独有] | AIGX `RT:61-62` |

---

## 6. 对齐方案

### 6.1 路由菜单「一模一样」改造（先出方案，实施待放行）

**目标结构**（照 `routing-page.tsx` + `routing-rail.tsx`）：
- ① 顶部加 **模式分段：路由配置 / 请求回放**（带未保存计数徽标）。
- ② 左侧 rail 改为**分组形态**：
  - `默认规则`：**全局默认** / **错误处理**
  - `特殊请求路由`：**压缩上下文** / **图片兼容** / **标题生成**（改名对齐）
  - `模型路由`：每个已配置模型一项
  - `自定义路由组`：每个组名一项
  - footer：**新增路由配置**
- ③ 右侧内容区映射到 AIGX 既有组件（**不丢功能**）：
  - 全局默认 / 模型路由 → 复用现有「基础策略 + 分层档案（选中该 scope）」。
  - 错误处理 → **新增 rail 面板壳**，承载 AIGX 现有「重试 / 超时」配置（或至少对齐三段：处理规则/失败保护/安全边界）；需决定是否把 Settings 的「重试/超时」迁到此处（见 §7-2）。
  - 特殊三项 → 复用 `CompactRoutes / TitleRoutes / VisionCompat`（改 rail 文案）。
  - 路由组 → 复用 `GroupEditor`（从配置内提为 rail 一级项）。
- ④ 回放视图：保留 AIGX 现有回放；**可选**再加 A2A 形态的「最近请求」左列表（见 §7-4）。
- ⑤ 文案对齐表：`视觉→图片兼容`、`压缩→压缩上下文`、`标题→标题生成`（i18n 双语文案同步）。
- 风险/工作量：**中-大**。核心是「rail 与视图路由重构」+「模型/组从 layer 提升为 rail 一级项」+「新增错误处理 rail 面板」；组件大多可复用，但信息架构会重排，需 e2e 回归（现有 `e2e/*routing*` 用例的 rail 选择器会变）。

### 6.2 网关菜单处理（三选一，请 lead 定）

| 方案 | 动作 | 与 A2A 一致性 | 风险 |
|---|---|---|---|
| **A（推荐）** | 保留 Settings `网关` 分类骨架；按 §2 去掉 A2A 没有的 4 块 EXTRA（Guardrail / StreamCompat / AutoContinue / 请求体上限）或并轨；`PromptRules` 合并进 `PromptPanel`；CORS 归「安全」；`ClientCompat` 拆成 A2A 的「开发者角色兼容 + 第三方折叠」两开关 | ✅ 高（分类清单 1:1） | 小（删/并面板） |
| **B（字面）** | 删除 Settings `网关` 分类，面板迁到「路由」菜单 | ❌ 偏离（A2A 把伪装头/提示词/脱敏/调试放在 设置→网关） | 中-大（IA 大改、功能寻址变化） |
| **C** | 删供应商页残留「网关运行参数（只读）」信息条 | ⚠ 与 A2A 无对应物，但现状已是只读+单一编辑入口，「多余」证据弱 | 极小 |

> 推荐 A：既不破坏 A2A 的「设置→网关」结构，又满足用户「去掉多余网关菜单项」的诉求（删的是 A2A 都没有的 EXTRA 面板）。

---

## 7. 待 lead 确认的问题 → **已拍板（2026-11-23，落定结论如下）**

1. **「多余网关菜单」到底指哪个**（本审计已定位唯一菜单级「网关」= 设置页 `网关` 分类）：
   - A) 分类内冗余 EXTRA 面板（推荐口径 A，§6.2-A）
   - B) 整个分类删除并迁移（口径 B，会偏离 A2A）
   - C) 供应商页只读运行参数条（口径 C，证据弱）
   - **结论：口径 A。** 保留「设置 → 网关」分类骨架，内部对齐 A2A：拍板点名删除 `流兼容（StreamCompatPanel）` 与 `自动续写（AutoContinuePanel）` 两块 EXTRA —— **已落地**（import/渲染全撤，`Settings.tsx` 工作区）。`GuardrailPanel（内容护栏）` 与 `请求体大小上限` 拍板未含 → **保留为 AIGX 特有，记分叉**（§2 判定不变）。供应商页只读运行参数条（口径 C）不动。
2. **错误处理归属**：A2A 把它作为**路由页 rail 一级项**；AIGX 现放 Settings 的「重试/超时」两分类。要不要把这两分类迁入路由页「错误处理」以做到「一模一样」？（否则路由页会缺 A2A 的「错误处理」项，或做成只读入口链到 Settings）
   - **结论：迁入路由。** 路由页 rail 已建「错误处理」一级项并挂 `RoutingErrorsPanel`（`Routing.tsx` 工作区，任务 #10 终态）；Settings 的「重试」「超时」两分类随之**删除**（任务 #11 在途——工作区 `Settings.tsx` 分类表暂仍含 retry/timeout，迁移未收口前不动）。
3. **模型路由 / 自定义路由组**：是否要从「配置内 layer」**提升为 rail 一级项**（A2A 形态）？代价是重排现有配置视图的信息架构。
   - **结论：提升。已落地**（任务 #10 终态）：rail 呈 `模型路由` 分组（每个已配置模型一项）+ `自定义路由组` 分组（每个组 id 一项）+ footer「新增路由配置」输入（新增模型档案名），选中即驱动既有配置编辑器选层——组件复用、信息架构重排为 A2A 形态。
4. **回放视图**：是否要加 A2A 的「最近请求列表 + exec timeline」形态？现 AIGX 是按请求 ID 回放 + Flow + Trace（更像增强版）。若加，是替换还是并存？
   - **结论：换成 A2A 形态、增强版隐藏。已落地**（任务 #10 终态）：回放模式 = 左「最近请求」列表（`api.listRequestLogs` 取最近 50 条，模型+id）+ 右执行时间线（`RouteReplayView`）；无选中时回退既有按 ID 回放面板（`RouteReplayPanel`）。`RoutingFlow` / `RoutingTrace` 隐藏。
5. AIGX 独有的预演 / RoutingWorkspace / RoutingFlow / RoutingTrace：按「一模一样」原则是否**保留**（建议保留并记为 AIGX 增强，不在菜单层面呈现为额外项）。
   - **结论：严格对齐 A2A —— 页面隐藏、代码保留。** `Routing.tsx` 以 `SHOW_ROUTING_ENHANCED = false` 门禁隐藏预演（候选/能力需求/逐字段来源）、覆盖工作台（RoutingWorkspace）、Flow、Trace 四项增强；代码本身保留，待后续单独清理（避免本轮大删引入半死态），不作为菜单项呈现。

---

## 8. 证据清单（file:line）

> 行号为**审计当日（2026-11-23 上午）快照**；路由页/设置页重排后行号已漂移，定位以函数名/结构描述为准。

- **AIGX 网关=设置分类**：`frontend/src/pages/Settings.tsx:195-210`（分类表）、`:1199-1276`（网关渲染）、`:200`（网关项）。
- **AIGX 供应商页无网关 Tab / 只读条**：`frontend/src/pages/Gateways.tsx:26-27,34,112-116`；`frontend/src/components/GatewayRuntimePanel.tsx:2-7`。
- **AIGX 侧栏无网关项**：`frontend/src/components/Sidebar.tsx:36-63,85-131`。
- **AIGX 路由 rail 五项**：`frontend/src/pages/Routing.tsx:86-94,810-864`；配置视图构成 `:1-26,411-773`；回放 `:14-15`。
- **AIGX 错误处理在设置**：`frontend/src/pages/Settings.tsx:201-202`。
- **A2A 侧栏（无网关，key=gateway 显示为「模型管理」）**：`desktop-tauri/ui/sidebar-layout.js:71-100`（路由 `:83`，模型管理/gateway `:84`）。
- **A2A 设置分类 + 网关项**：`desktop-tauri/ui-islands/src/islands/settings-model.ts:481,499-519`（网关 `:507`）。
- **A2A 网关分类面板**：`settings-page.tsx:1668-1674`（网络访问/客户端识别/GatewayPane）+ `:680-904`（GatewayPane 各面板）。
- **A2A 路由页模式分段 + 外壳**：`routing-page.tsx:228-296`。
- **A2A 路由左 rail 分组**：`routing-rail.tsx:20-24,38-83`。
- **A2A 错误处理三段**：`desktop-tauri/ui-shared/routing-errors-panel.tsx:80-82`。

---

## 9. 设置菜单逐项对照 + 删/留/改清单（lead 已拍板方向：A2A 有则照搬、A2A 无则去）

> 用户口径：A2A 设置有哪些分类/菜单项，AIGX 照搬；A2A 设置没有的 AIGX 菜单项去掉；「网关」分类保留、内部按 A2A 对齐。

### 9.1 A2A 侧设置分类（源码实测 `settings-model.ts:499-519`，共 11）
`账户(account)` · `通用(general)` · `显示(display)` · `网关(gateway)` · `提供商(providers)` · `超时(timeout)` · `安全(security)` · `数据(data)` · `反馈与需求(feedback)` · `版本信息(system)` · `更新(about)`。
（「AI 助手」已从设置移到助手页自带的「助手设置」抽屉；`settings-model.ts:502-505` 注释；无独立「重试」分类，`重试` 渲染函数 `settings-state.ts:1646` 有定义但页面 CATEGORIES 未挂载 —— 以 CATEGORIES 为准。）

A2A 各分类 → 面板（`settings-page.tsx`）：account→AccountPane；general→GeneralPane；display→DisplayPane；gateway→NetworkAccessPanel+ClientIdentificationPanel+GatewayPane；providers→ProviderSettingsPane；timeout→TimeoutPane；security→SecurityPane；data→DataPane；feedback→FeedbackPane；system→(system-info-pane)；about→(update-panel)。

### 9.2 AIGX 侧设置分类现状（`Settings.tsx:195-210`，共 14 — **审计时点快照**）

> 工作区当前已变：`通知` 分类已删（迁顶级 `/notify`）、`系统信息` 已改名 `版本信息`、网关内 `流兼容/自动续写` 已撤；其余迁移（#11 在途）落地前本快照仍可作对照基线。
`通用(general)` · `AI 助手(assistant)` · `聊天(chat)` · `报表(report)` · `网关(gateway)` · `重试(retry)` · `超时(timeout)` · `限流与缓存(perf)` · `计费(billing)` · `数据(data)` · `通知(notify)` · `安全(security)` · `系统信息(system)` · `更新(update)`。

### 9.3 分类级对照 + 建议处置 → **拍板落定（2026-11-23）**

| A2A 分类 | AIGX 现状 | 判定 | **拍板处置** |
|---|---|---|---|
| 账户 | **无**（管理员账号/登录管理未在设置分类） | [MISSING] | **只建能落地的**：本版未新建（拍板原则——有真实落地才建，不为凑 A2A 清单硬建） |
| 通用 | `通用`（使用限额/邮箱白名单/主题…） | [OK/DIFF] | 留；内容差异另行评估 |
| 显示 | **无**（主题/语言在侧栏底部） | [MISSING] | **只建能落地的**：本版未新建（主题/语言保持侧栏底部入口） |
| 网关 | `网关` | [DIFF] | **留**；内部对齐：流兼容/自动续写**删除（已落地）**；Guardrail/请求体上限拍板未含 → 保留记分叉（§2） |
| 提供商 | **无 settings 分类**（供应商是顶级页 `/channels`） | [DIFF] | **不建设分叉**：保持 AIGX `供应商` 顶级页（`/channels`）形态，不在设置里补跳转分类 |
| 超时 | `超时` | [OK] | **迁**：与「重试」一并并入 路由→「错误处理」rail 面板（面板已落地，分类删除在途 #11） |
| 安全 | `安全`（安全策略/监控/IP） | [OK] | 留 |
| 数据 | `数据`（数据管理/云同步/日志保留） | [OK] | 留 |
| 反馈与需求 | **无**（工单是顶级页） | [MISSING] | **只建能落地的**：本版未新建（工单保持顶级页 `/tickets`） |
| 版本信息 | `系统信息`（SystemInfoPanel） | [OK/DIFF] label | **改 label「版本信息」——已落地**（`Settings.tsx` 分类表工作区） |
| 更新 | `更新` | [OK] | 留 |

| AIGX 有 · A2A 设置无 | 内容 | **拍板处置** | 状态 |
|---|---|---|---|
| `AI 助手` | AgentSettingsPanel | **迁 `/agent` 页抽屉**（AI 运维页自带「助手设置」抽屉，对齐 A2A 助手页形态） | #11 在途 |
| `聊天` | ChatSettingsPanel + ImageCompatPanel | **迁 `/chat` 页**；`ImageCompatPanel` 并入 路由→「图片兼容」（与 `VisionCompat` 同口径） | #11 在途 |
| `报表` | ReportSettingsPanel | **迁仪表盘 Tab**（A2A 报表=顶级 overview，AIGX 对应为仪表盘页内 Tab） | #11 在途 |
| `重试` | RoutingErrorsPanel + 重试字段 | **迁**：并入 路由→「错误处理」rail 面板；从设置移除 | rail 面板已落地；分类删除 #11 在途 |
| `限流与缓存` | 限流配置/缓存管理/RuntimeCapacityPanel | **迁顶级 `/limit` 页**（新页 `LimitPage.tsx` 已建） | #11 在途（路由挂载待收口） |
| `计费` | 易支付/用户分组/按次计费/价格同步 | **迁 `/products` 计费设置 Tab**（商品页新增 Tab） | #11 在途 |
| `通知` | Notify | **迁顶级 `/notify` 页** | **已落地**（`App.tsx` 路由 + `Sidebar.tsx` 顶级项 + Settings 分类删除） |
| ~~`流兼容`~~ | StreamCompatPanel | **删除**（A2A 无对应、拍板点名） | **已落地**（import/渲染全撤） |
| ~~`自动续写`~~ | AutoContinuePanel | **删除**（A2A 无对应、拍板点名） | **已落地**（import/渲染全撤） |

### 9.4 与路由改造的耦合
- AIGX `Settings→重试` 里已内含 `<RoutingErrorsPanel ...>`（`Settings.tsx:908-947`）—— 该组件可直接复用到 **路由→错误处理** rail 面板；「重试字段」也一并迁入。
- `Settings→聊天` 里含 `ImageCompatPanel`（`Settings.tsx:1281-1290`）—— A2A 把「图片兼容」放在路由特殊项下，故 AIGX 该面板应迁到 **路由→图片兼容**（与现有 `VisionCompat` 合并或并存需定）。

### 9.5 风险与「功能去哪」—— **拍板已全部给出迁移去处（2026-11-23）**

严格「A2A 设置没有就去掉」的原风险（删入口丢功能）已由拍板逐项消除——每个被移除的设置分类都先有迁移去处：

| 被移除入口 | 功能去哪（拍板） |
|---|---|
| `限流与缓存` | **顶级 `/limit` 页**（`LimitPage.tsx` 新页承载限流配置/缓存管理/容量） |
| `计费` | **`/products` 计费设置 Tab**（易支付/用户分组/按次计费/价格同步迁商品页 Tab） |
| `通知` | **顶级 `/notify` 页**（已落地：`Sidebar.tsx` 顶级项 + `App.tsx` 路由） |
| `AI 助手` | **`/agent` 页「助手设置」抽屉**（AgentSettingsPanel 迁 AI 运维页） |
| `聊天` | **`/chat` 页**（ChatSettingsPanel 迁聊天页；ImageCompatPanel 并入路由→图片兼容） |
| `报表` | **仪表盘 Tab**（ReportSettingsPanel 迁仪表盘页内 Tab） |
| `重试 / 超时` | **路由 →「错误处理」rail 面板**（RoutingErrorsPanel + 重试字段整块迁移） |

### 9.6 待你确认（设置页动工前）→ **已拍板（2026-11-23）**

1. `限流与缓存 / 计费 / 通知`：→ **迁**（顶级 /limit、/products 计费设置 Tab、顶级 /notify；通知已落地）。
2. `AI 助手 / 聊天 / 报表`：→ **迁走后删分类**（/agent 抽屉、/chat、仪表盘 Tab；路由→图片兼容收编 ImageCompatPanel）。
3. A2A 独有、AIGX 缺的 `账户 / 显示 / 反馈与需求 / 提供商`：→ **只建能落地的**（本版未新建账户/显示/反馈分类；提供商不建设设置分叉，保持 `/channels` 顶级页）。
4. `系统信息`：→ **改名「版本信息」——已落地**。

> 回执即本条 §9：拍板结论已全部写入（上文加粗）。实施状态：通知迁移/流兼容/自动续写删除/版本信息改名/**路由 rail 全量重排（任务 #10 终态）** 已落地；`/limit`、计费 Tab、agent 抽屉、`/chat`、报表 Tab、retry/timeout 分类删除由任务 #11 收口，终态以复核波（#12）为准。
