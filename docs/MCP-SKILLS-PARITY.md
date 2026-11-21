# MCP & Skills 前端对齐规格（AIGX ↔ Agent2API v3.5.0-beta.6）

> 审计日期：2026-11-21
> AIGX：`D:\GitHub\AIGX`，分支 `main`。本文为**只读调研产物**，不改代码。
> 上游真相源：`D:\GitHub\Agent2API`，标签 `v3.5.0-beta.6`（`git show v3.5.0-beta.6:<path>`），
> 前端路径前缀 `desktop-tauri/ui-islands/src/islands/`。
> 配套：`docs/TOOL-MCP-ROUTING-PARITY.md`（后端契约审计，不在本文重复）。
> 红线：不写 provider 伪装 / 规避检测；不动 `frontend/src/components/plugins/*` 与刚提交的深色 CSS。
> 图例：**[复刻]** 结构/文案照上游；**[复用]** 用 AIGX 现有实现承载；**[改名]** 仅改文案；**[差异]** 有意保留的 AIGX 能力或平台差异。

---

## 0. 目标任务与结论

把 AIGX 侧边栏「工具服务」整页改为 **MCP & Skills**，页面结构对齐 beta.6：

1. **列表页**：卡片列表 → `Card` 内一张**表格**（服务名称 / 接入协议 / 状态 / 能力数量 / 版本 / 服务入口 / 操作 / 本机接入）。
2. **详情页**：点「详情」进入，包含服务头卡、MCP 接入信息卡、`服务配置 / 服务能力` 两个 Tab。
3. **配置 Tab**：按服务 id 分派到现有 `ToolProviderPanel`（web-search）、`ToolVisionPanel`（vision）、`ToolImageConfigPanel`（image-generation）。
4. **安装/卸载弹窗**：沿用现有 `ToolInstallDialog`，按上游补齐文案、状态标签与成功行为。
5. **i18n**：菜单、H1、Card 标题及所有「工具服务」用户可见文案改为「MCP & Skills」；补齐详情页/能力页/弹窗新 key（zh + en）。
6. 现有 AIGX 独有能力（用户工具策略卡、beta.2 多密钥）**保留**，不因对齐而回退。

范围内文件（均只读）：上游 `tools-page.tsx`、`tools-detail.tsx`、`tools-install.tsx`、`tools-data.ts`、`tools-config.tsx`、`tools-image-config.tsx`；AIGX `pages/ToolsPage.tsx`、`components/tools/*`、`api/endpoints/misc.ts`、`types/a2a.ts`、`components/Sidebar.tsx`、`i18n/{zh,en}.json`。

---

## 1. 改名清单（「工具服务」→「MCP & Skills」）

### 1.1 必须改（用户可见）

| # | file:line | 现状 | 改为 |
|---|---|---|---|
| R1 | `frontend/src/components/Sidebar.tsx:61` | `labelKey: '工具服务'` | `labelKey: 'MCP & Skills'`（`MCP & Skills` key 已存在） |
| R2 | `frontend/src/pages/ToolsPage.tsx:214` | `<h1>{t('工具服务')}</h1>` | `<h1>{t('MCP & Skills')}</h1>` |
| R3 | `frontend/src/pages/ToolsPage.tsx:207` | `t('读取工具服务')` | `t('读取 MCP & Skills')` |
| R4 | `frontend/src/pages/ToolsPage.tsx:239` | `t('工具服务后端尚未就绪')` | `t('MCP & Skills 后端尚未就绪')` |
| R5 | `frontend/src/pages/ToolsPage.tsx:240` | `t('工具服务管理面（/api/tools/*）…')` | `t('MCP & Skills 管理面（/api/tools/*）…')` |
| R6 | `frontend/src/pages/ToolsPage.tsx:244` | `<Card title={t('MCP & Skills')}>` | **已是目标值，不改**（确认保留） |
| R7 | `frontend/src/components/routing/special/VisionCompat.tsx:93,94` | `t('工具服务的视觉模型')` | `t('MCP & Skills 的视觉模型')` |
| R8 | `frontend/src/components/routing/special/VisionCompat.tsx:431` | `t('沿用工具服务的视觉模型')` | `t('沿用 MCP & Skills 的视觉模型')` |
| R9 | `frontend/src/components/routing/special/VisionCompat.tsx:441` | `t('沿用工具服务 = 使用工具配置里已有的视觉模型。')` | `t('沿用 MCP & Skills = 使用该页配置里已有的视觉模型。')` |
| R10 | `frontend/src/components/routing/special/VisionCompat.tsx:559` | `t('沿用工具服务的视觉模型：视觉目标由「工具服务」的视觉配置决定…')` | `t('沿用 MCP & Skills 的视觉模型：视觉目标由「MCP & Skills」的视觉配置决定，本规则只负责把请求路由到识图环节。')` |
| R11 | `frontend/src/components/routing/special/VisionCompat.tsx:568` | `t('工具服务当前未配置共享视觉模型；启用「沿用」前请先在工具服务里配置。')` | `t('MCP & Skills 当前未配置共享视觉模型；启用「沿用」前请先在该页配置。')` |

> R7–R11 是其他页面对本页的交叉引用；若只改菜单/H1 会留下「工具服务」断链，必须一起改。

### 1.2 仅注释（不阻塞，建议顺手改）

`App.tsx:56,262`、`Sidebar.tsx:60,116`、`pages/ToolsPage.css:1`、`components/tools/*.tsx` 文件头注释、`api/endpoints/misc.ts:181,187`、`types/a2a.ts:4,147`。这些不进界面，可保持「工具服务」或改成「MCP & Skills」，不影响验收。

### 1.3 保留不动的上游文案

- 「没有匹配的工具服务」：上游 beta.6 原文如此，**保留**（与菜单改名不冲突）。
- `serviceDesc('web-search')`：AIGX 现为「…已有账号…」，上游为「…已有 WorkBuddy、Kiro 账号…」。**AIGX 保留自己的文案**，不要引入 AIGX 未支持的 WorkBuddy/Kiro 品牌承诺（能力边界诚实原则）。
- `protocolDesc` / 安装提示中的产品名：上游写「由 Agent2API 面板安装」，AIGX 必须写「由 AIGX 面板安装」；MCP 示例里的 `<Agent2API API Key>` 改为 `<AIGX API Key>` 或 `<网关 Key>`。

### 1.4 旧 key 清理

改名后 `工具服务`、`读取工具服务`、`工具服务后端尚未就绪`、`工具服务管理面…`、`工具服务的视觉模型` 等旧 key 在 `zh.json` 不再被引用，可删除；**不要删除**「没有匹配的工具服务」。

---

## 2. 页面架构：列表 ↔ 详情

上游 `tools-page.tsx:82` 用**组件内状态**切换详情，不换路由：

```tsx
const [detail, setDetail] = useState<ToolService|null>(null)
if (detail) return <ToolDetail service={detail} onBack={...} onChanged={...} />
```

AIGX 照此实现，路由仍为 `/tools`（`App.tsx:263` 不动）：

- 新增状态 `detailKey: string | null`（存 `service.key`，不存对象，避免 reload 后快照陈旧）。
- 派生 `const detail = services.find(s => s.key === detailKey) ?? null`；若 `detailKey` 非空但服务消失，effect 里清空 `detailKey`。
- 「详情」→ `setDetailKey(service.key)`；「返回列表」→ `setDetailKey(null)` 并 `void load()`。
- 列表 reload 后详情自动拿到新对象（enabled/timeout/capability_count 同步）。

建议新增/调整文件：

| 文件 | 动作 |
|---|---|
| `frontend/src/components/tools/ToolDetail.tsx` | **新增**，详情页（§4） |
| `frontend/src/components/tools/tools-meta.ts` | **新增**，共享 `serviceName/serviceDesc/protocolDesc/protocolLabel/protocolTone/capabilityGroups`（从 `ToolsPage.tsx:40-77` 迁出） |
| `frontend/src/pages/ToolsPage.tsx` | 改为列表表格 + 详情切换；移除内嵌配置卡（§3） |
| `frontend/src/pages/ToolsPage.css` | 追加表格/详情样式；保留 `.tp-spin` 等通用类 |
| `frontend/src/components/tools/ToolInstallDialog.tsx` | 文案/交互微调（§5） |
| `frontend/src/i18n/{zh,en}.json` | 新增/改名 key（§6） |

> `tools-meta.ts` 里所有文案必须写成**字面量 `t('…')`**（函数参数收 `t`），别用「中文常量表 + t(变量)」——i18n 扫描器只登记字面量调用（与 `ToolsPage.tsx:34-39` 现有注释同一约束）。

---

## 3. 列表页差异

### 3.1 上游结构（`tools-page.tsx:84-162`）

```
Card
  CardHeader: CardTitle「MCP & Skills」+ 副标题「将搜索、视觉等能力…」 + (桌面)「客户端环境」按钮
  CardContent:
    工具栏: SegmentedControl(全部 · Skill · MCP)        + 右对齐「刷新」
    error Alert
    loading 骨架 / Empty / Table
      Table 列: 服务名称 | 接入协议 | 状态 | 能力数量 | 版本 | 服务入口 | 操作 | (桌面) 本机接入
    install / uninstall Dialog
```

数据侧：`fetchServices()` 后 **过滤 `protocol === 'api'`**（`tools-page.tsx:41`）；AIGX 已在 `ToolsPage.tsx:108` 做同样过滤，复用。

### 3.2 AIGX 现状与动作

| 现状（file:line） | 动作 |
|---|---|
| 卡片列表 `.tp-service-list` + 每服务 `.tp-service`（`ToolsPage.tsx:248-379`） | **删除**，替换为表格 |
| 服务名/协议/版本/启用 Badge（`:259-264`） | 拆进表格列 |
| 内联超时输入（`:287-309`） | **移到详情页头卡**（§4.2） |
| `运行时地址` `<details>`（`:312-332`） | **移到详情页 MCP 接入信息卡**（§4.3） |
| 安装/卸载/已接入数（`:334-368`） | 移到表格「本机接入」列 |
| 图片生成配置内嵌（`:370-374`） | 移到详情配置 Tab |
| 用户工具策略卡（`:382-393`） | **保留在列表页**（AIGX 独有，上游无） |
| `ToolProviderPanel` / `ToolVisionPanel`（`:395-396`） | **移出列表页**，进各自详情配置 Tab |
| `loadTargets`（`:126-140`） | 保留，用于「本机接入」列 |
| `loadInstallInfo`（`:142-158`） | **删除**，改由详情页自取（§4.3） |

### 3.3 逐列规格（**[复刻]**）

| 列 | 内容 | AIGX 实现要点 |
|---|---|---|
| 服务名称 | `<strong>{serviceName}</strong>` + 下行 `{serviceDesc}`，描述 `max-width:420px`、11.5px muted | `serviceName/serviceDesc` 从 `tools-meta.ts` 取 |
| 接入协议 | `Badge`（skill→info / mcp→success / api 不展示）+ 下行 `PROTOCOL_DESC`（max-width 220px、11px muted） | `protocolTone` 复用现有映射 |
| 状态 | `Badge` success「已启用」/neutral「已停用」+ `Switch`；Switch 期间该行禁用 | `ariaLabel={t('启用 / 停用此服务')}` |
| 能力数量 | `service.capability_count` | 纯文本 |
| 版本 | `Badge tone="neutral">v{service.version}` | — |
| 服务入口 | `<code className="...nowrap">{service.endpoint}</code>` | 长串 `word-break` 或横向滚动 |
| 操作 | `Button size="sm" variant="outline"`「详情」 | `setDetailKey(service.key)` |
| 本机接入 | 「安装/更新」+「卸载」+ 已接入数 | 上游仅桌面壳显示；**AIGX 始终显示**（AIGX 是服务端管理面板，「本机」= 服务器主机），见 §7 |
| 行数 | `visible = tab==='all' ? services : filter(protocol===tab)` | 复用 `ToolsPage.tsx:201-204` |

「安装/更新」按钮：`disabled={!service.enabled || busy}`，`installed > 0 ? t('更新') : t('安装')`；「卸载」`disabled={installed===0}`；已接入数 `<span title={t('已接入 {{count}} 个目标', { count: installed })}>`。

### 3.4 工具栏与页头

- **页头**：保留 AIGX `page-header`（全站一致），`h1` 改「MCP & Skills」，副标题沿用「将搜索、视觉等能力通过 MCP 或 Skills 接入客户端。」；**刷新按钮从页头移入 Card 工具栏右侧**（对齐上游 `tools-page.tsx:98`）。
- **工具栏**：`Tabs`（全部 / Skill / MCP）+ 右对齐 `Button variant="outline"`「刷新」，加载时图标换 `Loader2` + 旋转。
- **Card 标题**：保留 `<Card title={t('MCP & Skills')}>`（`ToolsPage.tsx:244`），不回退。
- **错误**：`<div className="error-message">` 放在 Card 内工具栏之后（上游 Alert 位置）。
- **空态**：`EmptyState icon="🧰" message={t('没有匹配的工具服务')}`。
- **未就绪**：保留 `notReady` 404 降级（`ToolsPage.tsx:236-241`），文案按 R4/R5 改名。

### 3.5 表格用 AIGX 组件重写

AIGX **没有** `Table/TableHeader/TableRow` 原语；`components/ui/TableShell.tsx` 只是分页 hook + 页脚。按现有管理页惯例（`Redemptions.tsx:325`、`Logs.tsx:799`）：

```tsx
<div className="table-wrapper">
  <table className="dense-table">
    <thead>…</thead>
    <tbody>…</tbody>
  </table>
</div>
```

- 不引入新依赖，不用上游 Tailwind 的 `overflow-x-auto`；`.table-wrapper` 已含横向滚动与移动端卡片化。
- 「本机接入」列在 AIGX 恒显示，因此不需要 `colSpan` 分支；若未来按桌面壳隐藏，再补。

---

## 4. 详情页完整结构（`ToolDetail.tsx`）

上游 `tools-detail.tsx:97-222`，逐块如下（**[复刻]**）。所有块用 AIGX `Card`/`Button`/`Badge`/`Switch`/`Input`/`Tabs` 重写。

### 4.1 返回列表

```tsx
<Button variant="outline" size="sm" onClick={onBack}>← {t('返回列表')}</Button>
```
容器 `display:flex; flex-direction:column; gap:16px`（本地 class，如 `.mcp-detail`）。**详情页不再渲染页头 h1**，标题由服务头卡承担。

### 4.2 服务头卡（`tools-detail.tsx:156-189`）

- **标题**：`serviceName(service.id)`（如「网络搜索」）。
- **标题行内 badges**：协议 `Badge` + 启用 `Badge variant success/neutral` + `能力数量: {count}`（小字 muted）+ `v{version}`。
  - AIGX `Card` 的 `title` 是 `ReactNode`，可传 `<span className="mcp-detail-title">{name}<Badge…/>…</span>`。
- **右侧操作**：`<span>{t('启用 / 停用此服务')}</span>` + `Switch checked={enabled} disabled={toggling} onChange={…}`（通过 `Card actions` 传入）。
- **正文**：
  1. 服务描述（`serviceDesc`）
  2. 协议描述（`protocolDesc`）
  3. **工具执行超时**：`Input type="number" min={5} max={600}` + 「秒」+ `Button variant="outline"`「保存」；`disabled` 条件：`loading || savingTimeout || !timeoutDraft || timeoutDraft === savedTimeout`。保存 `PATCH /api/tools/servers/{id} {protocol, timeout_seconds}`，toast「工具执行超时已保存」，并 `onChanged()`。
     - **注意范围变化**：AIGX 列表旧实现允许 `1–3600`（`ToolsPage.tsx:182`），上游为 **5–600**；本页按 5–600 做前端校验。若后端也校验，需在整合期确认（只读审计未验证）。
  4. 超时说明：image-generation 用「这是后台生成任务的时间上限。单次 MCP 调用最多等待 20 秒…」；其余用「服务端强制单次工具执行的超时上限…」。
  5. `!enabled` → 警告条「该协议已停用，已接入的客户端会收到服务停用错误。」
  6. `error` → 破坏性提示条。

状态：`enabled` 本地态跟随 `service.enabled`（effect 同步）；`timeoutDraft/savedTimeout` 跟随 `service.timeout_seconds`。toggle/save 成功后调用 `onChanged()`（父级 `load()`），失败回滚并展示错误。

### 4.3 MCP 接入信息卡（`tools-detail.tsx:59-95,191-193`）

- **仅当 `service.protocol === 'mcp' && info`** 渲染。
- 数据来源：`api.getToolInstallInfo(service.id)`（`misc.ts` 已封装）；详情页自己 loading/fetch，不依赖父级 map。
- 内容：
  1. **服务地址**：`<code>{info.mcp.url}</code>` + `Button variant="outline" size="sm"`「复制」；说明「在支持远程 MCP 的客户端中填写该地址，并使用网关 Key 作为 Bearer 令牌。」
  2. **暴露的工具**：`info.mcp.tools.map(name => <Badge key={name}>{name}</Badge>)`。
  3. **客户端配置示例**：
     ```json
     {"mcpServers":{"<server_name>":{"type":"http","url":"<info.mcp.url>","headers":{"Authorization":"Bearer <AIGX API Key>"}}}}
     ```
     用 `<pre>` 展示，右上「复制」；说明「把下面这段合并到客户端配置的 mcpServers 里，并把 Key 换成你自己的。」
- **Skill 协议**（`protocol === 'skill'`）：不渲染 MCP 卡，渲染提示「该协议通过 AIGX 面板安装到本机客户端目录。」（上游为 Agent2API，AIGX 改品牌）。
- `info` 加载中：转圈「正在加载工具服务配置…」（改名为「正在加载 MCP & Skills 配置…」）；加载失败：头卡 error 展示，MCP 卡不渲染。
- 复制失败：toast「复制失败，请手动选择后复制」（key 已存在）。

### 4.4 配置/能力 Tabs（`tools-detail.tsx:195-220`）

```tsx
<Tabs items={[{key:'config',label:t('服务配置')},{key:'capabilities',label:t('服务能力')}]}
      active={tab} onChange={setTab} ariaLabel={t('服务配置')} />
```

上游是 `SegmentedControl`；AIGX 无该组件，**用 `Tabs`**（当前列表页协议过滤已是 `Tabs`，视觉一致、可键盘操作）。

### 4.5 配置 Tab 分派（`tools-detail.tsx:201-203`）

```tsx
tab === 'config'
  ? service.id === 'image-generation' ? <ToolImageConfigPanel serviceEnabled={enabled} onSharedError={setError} />
  : service.id === 'vision'          ? <ToolVisionPanel onSharedError={setError} />
  :                                    <ToolProviderPanel />
  : <能力卡>
```

**组件映射**（AIGX 现有 → 上游）：

| 上游 | AIGX 现有 | 映射方式 |
|---|---|---|
| `ImageGenerationConfigPanel`（`tools-image-config.tsx:21`，包 `ui-shared/ImageGenerationPanel`） | `ToolImageConfigPanel` + `ToolImageJobsPanel` | **原样复用**。入参 `serviceEnabled={service.enabled}`、`onSharedError={setError}`；内部已含配置 + 最近任务。 |
| `VisionConfigPanel`（`tools-config.tsx:70`） | `ToolVisionPanel` | **原样复用**。入参 `onSharedError={setError}`。 |
| `WebSearchConfig`（`tools-config.tsx:46`） | `ToolProviderPanel` | **原样复用**；仅把 Card 标题「搜索提供商」改为「上游提供商配置」，与上游 `tools-config.tsx:148` 一致。 |

分派键是 **`service.id`**（`web-search`/`vision`/`image-generation`），与协议无关；同一 id 的 skill 与 mcp 两行进入详情后看到同一份配置（正确，配置本就按服务共享）。

#### 4.5.1 三个面板的差异（保留 AIGX 实现，记录不阻塞项）

- `ToolProviderPanel` 比 beta.6 多：beta.2 多密钥清单/排序/单密钥 search/fetch 检查（`ToolProviderPanel.tsx:101-124,262-606`）。**保留，不得回退**；Card 标题改名即可。上游的 4 列提供商卡片布局与 AIGX 行式布局不同，属展示差异，P2 可选对齐。
- `ToolVisionPanel`：AIGX 思考等级固定 `['',low,medium,high]`（`:21`），上游从 `/api/models/manage` 的 `reasoningLevels` 读取（`tools-config.tsx:75,99`）；AIGX 无「已失效 Key」禁用项。功能可用，P2 可选补齐。
- `ToolImageConfigPanel`：AIGX 已含任务列表（`ToolImageJobsPanel`），与上游面板的 history 等价；字段/校验一致。

### 4.6 服务能力 Tab（`tools-detail.tsx:204-220` + `CAPABILITY_DESCRIPTIONS:15-50`）

Card 标题「服务能力」，右侧「共 {{count}} 项能力」。正文 `grid`，桌面 2 列、移动 1 列；每项：

```
[能力标题]
[能力说明]
  参数名 (code) | 类型 | 必填/可选 | 参数说明
```

数据用 `capabilityGroups(t, service.id)`（放 `tools-meta.ts`，返回 `Array<{title,desc,params:{name,type,desc,required}[]}>`）：

- **web-search**
  1. 批量网页搜索：批量搜索 1～10 条查询，并发调用后台选中的提供商后合并重复来源。参数 `queries:string[]`（必填）要搜索的查询，每条并发执行（1～10 条）。
  2. 批量网页抓取：批量抓取 1～10 个 URL，使用后台选中的单个抓取提供商。参数 `urls:string[]`（必填）要抓取并提取正文的网页 URL（1～10 个）。
- **image-generation**
  1. 生成图片：按描述使用已配置的模型生图，默认一张，会消耗账号额度。参数 `prompt:string`（必填）、`request_id:string`（必填，本次提交的唯一编号，断线重试使用原编号）。
  2. 编辑图片：按参考图修改，仅在配置了可用编辑模型时提供。参数 `images:string[]`（必填，参考图链接或已生成图片 ID；本地桥接还可传文件路径）。**过滤条件**：`info?.mcp.tools.includes('edit_images')` 才展示。
  3. 查询生成结果：查询原任务并取回图片，不再次生图或扣费。参数 `job_id / request_id:string`（必填，至少提供一项）。
- **vision**
  1. 图片分析：把 1～10 张图片与提示词交给后台配置的视觉模型，返回模型回复文本。参数 `images:string[]`（必填）、`prompt:string`（必填）。

「必填」为新 key；「可选」已存在。

---

## 5. 安装/卸载弹窗差异（`tools-install.tsx` ↔ `ToolInstallDialog.tsx`）

上游拆成 `ToolInstallDialog` + `ToolUninstallDialog`；AIGX 用同一个组件 + `uninstall` 布尔。**建议保留单组件**（减少改动面），按上游补齐文案与行为即可。

### 5.1 安装弹窗逐项差异

| 项 | 上游 beta.6 | AIGX 现状（file:line） | 规格动作 |
|---|---|---|---|
| 标题 | 安装：`安装 {name}` / MCP：`{name} 接入客户端` | `安装 {{name}}`（`:224-226`） | 按协议分标题，MCP 用「{{name}} 接入客户端」 |
| 说明 | 安装/写入两段说明 | 一句「选择要安装…目录」 | 按上游两段文案（§6） |
| 目标列表 | 复选 + 状态「未检测到 / 已检测到 / 已安装」+ 路径 + note | 复选 + Badge「已探测 / 已安装 / 不支持」（`:264-288`） | 状态文案改为上游三态；`supported===false` 仍禁用 |
| 全选 | 「全选 / 取消全选」+ 已选计数 | 已有（`:249-260`） | 复用；计数文案用 `{{count}}/{{total}}` |
| DSH 提示 | 有 | 有（`:292-296`） | 复用 |
| Skill 运行时 | `SegmentedControl` Node.js / Python + 说明 | `Select` 变体（`:298-310`） | 保留 `Select`（AIGX 无 SegmentedControl；变体来自后端，比固定两项更稳），文案对齐「Node.js 无需额外依赖…」 |
| MCP 接入方式 | `SegmentedControl` 本地桥接 / 远程 HTTP + 说明 | `Select`（`:312-329`） | 保留 Select；默认 bridge（vision/image）不变 |
| 图片输出目录 | 输入框 + **选择目录**（桌面 `pickFolder`） | 纯 `Input`（`:333-340`） | 保留纯输入；AIGX 无桌面桥接，无法弹目录选择（§7） |
| 参考图目录 | Textarea + 说明 + 权限 Alert | Textarea + placeholder（`:341-351`） | 补说明与 Alert |
| Key 选择 | `Select` + 说明「Key 明文只写入…」 | `Select`（`:353-365`） | 补说明 |
| 超时提示 | image 120 秒 / 其他按服务超时 | 无 | 新增 Alert |
| MCP 配置示例 | **不在安装弹窗**（在详情页） | 安装在弹窗内展示（`:367-380`） | **移除**，统一放详情 MCP 卡 |
| 结果 | 成功 Alert + 失败拼 error；**全部成功自动关闭** | 回执列表，不自动关闭（`:382-393`） | 保留回执；全部成功时 `onClose()`；失败不关 |
| 底部按钮 | 取消 / 安装\|写入配置 | 关闭 / 安装\|卸载（`:398-405`） | 安装态文案改「写入配置」；卸载态保持「卸载」

### 5.2 卸载弹窗差异

- 标题：上游「移除 {name} 配置」；AIGX「卸载 {{name}}」→ 改为「移除 {{name}} 配置」。
- 只列 `installed` 目标（AIGX 已隐含，但当前展示全部目标）；按上游过滤。
- 空态：「当前没有已安装/已配置的目标」。
- 主按钮：`variant="danger"`「卸载」；成功后 toast「已处理 {{count}} 个目标」。

### 5.3 后端能力已知差异（前端照实渲染，不假装）

- 安装目标：AIGX 后端 4 家（`claude-code / codex / claude-desktop / dsh`），上游 13 家。前端按 `install-targets` 响应渲染即可，**不要写死客户端清单**。
- 无「预览 + 确认令牌」写入协议（上游有），前端无需改；不要在 UI 宣称可预览。
- 图片桥接脚本环境变量仍为 `AGENT2API_IMAGE_*`（后端有意兼容），前端不展示变量名，无影响。

---

## 6. i18n key 清单（zh + en）

AIGX 语言包：`zh.json` 是 key 的源语言（key 即中文原文），`en.json` 提供英文；仅 zh/en 上架（`i18n/index.ts:8-19`），其余四包 fallback 中文。**插值统一用 `{{name}}`（i18next），不要照搬上游的 `{name}`。**

### 6.1 改名（旧 key → 新 key / 英文）

| 旧 key | 新 key | en |
|---|---|---|
| 工具服务 | MCP & Skills | MCP & Skills（已存在） |
| 读取工具服务 | 读取 MCP & Skills | Loading MCP & Skills |
| 工具服务后端尚未就绪 | MCP & Skills 后端尚未就绪 | MCP & Skills backend not ready yet |
| 工具服务管理面（/api/tools/*）由后端 tools 模块提供，接口上线后本页自动可用。 | MCP & Skills 管理面（/api/tools/*）由后端 tools 模块提供，接口上线后本页自动可用。 | The MCP & Skills admin API (/api/tools/*) is provided by the backend tools module; this page becomes available once it ships. |
| 工具服务的视觉模型 | MCP & Skills 的视觉模型 | MCP & Skills vision model |
| 沿用工具服务的视觉模型 | 沿用 MCP & Skills 的视觉模型 | Use the MCP & Skills vision model |
| 沿用工具服务 = 使用工具配置里已有的视觉模型。 | 沿用 MCP & Skills = 使用该页配置里已有的视觉模型。 | Inherit from MCP & Skills = use the vision model already configured on that page. |
| 沿用工具服务的视觉模型：视觉目标由「工具服务」的视觉配置决定，本规则只负责把请求路由到识图环节。 | 沿用 MCP & Skills 的视觉模型：视觉目标由「MCP & Skills」的视觉配置决定，本规则只负责把请求路由到识图环节。 | Using the MCP & Skills vision model: the vision target comes from the MCP & Skills configuration; this rule only routes requests into the image-reading step. |
| 工具服务当前未配置共享视觉模型；启用「沿用」前请先在工具服务里配置。 | MCP & Skills 当前未配置共享视觉模型；启用「沿用」前请先在该页配置。 | MCP & Skills currently has no shared vision model configured; configure one on that page before enabling "inherit". |

### 6.2 新增：列表/详情

| key（zh） | en |
|---|---|
| 服务名称 | Service name |
| 本机接入 | Local install |
| 正在加载 MCP & Skills… | Loading MCP & Skills… |
| 返回列表 | Back to list |
| MCP 接入信息 | MCP connection info |
| 服务地址 | Service URL |
| 暴露的工具 | Exposed tools |
| 客户端配置示例 | Client configuration example |
| 在支持远程 MCP 的客户端中填写该地址，并使用网关 Key 作为 Bearer 令牌。 | Enter this URL in a client that supports remote MCP, and use a gateway key as the Bearer token. |
| 服务配置 | Service config |
| 服务能力 | Service capabilities |
| 必填 | Required |
| 共 {{count}} 项能力 | {{count}} capabilities in total |
| 工具执行超时 | Tool execution timeout |
| 该协议已停用，已接入的客户端会收到服务停用错误。 | This protocol is disabled; connected clients will receive a service-disabled error. |
| 正在加载 MCP & Skills 配置… | Loading MCP & Skills configuration… |
| 工具执行超时已保存 | Tool execution timeout saved |
| 这是后台生成任务的时间上限。单次 MCP 调用最多等待 20 秒，未完成时返回任务编号；查询不会重新生图。 | This is the time limit for background generation jobs. A single MCP call waits at most 20 seconds and returns a job ID if unfinished; querying never regenerates images. |
| 服务端强制单次工具执行的超时上限（同一服务的 Skill / MCP 共用）。安装 MCP 时会按此值写入客户端配置；修改后需在客户端重新「更新」配置并重进会话才会生效。 | Server-enforced timeout for one tool execution (shared by the same service's Skill and MCP). It is written into the client config when installing MCP; after changing it, re-run Update in the client and reopen the session. |
| 该协议通过 AIGX 面板安装到本机客户端目录。 | This protocol is installed into local client directories through the AIGX panel. |

能力页新增：

| key（zh） | en |
|---|---|
| 批量网页搜索 | Batch web search |
| 批量搜索 1～10 条查询，并发调用后台选中的提供商后合并重复来源。 | Search 1–10 queries concurrently with the selected providers and merge duplicate sources. |
| 要搜索的查询，每条并发执行（1～10 条） | Queries to search, each executed concurrently (1–10) |
| 批量网页抓取 | Batch web fetch |
| 批量抓取 1～10 个 URL，使用后台选中的单个抓取提供商。 | Fetch 1–10 URLs with the single fetch provider selected in the backend. |
| 要抓取并提取正文的网页 URL（1～10 个） | Web page URLs to fetch and extract text from (1–10) |
| 生成图片 | Generate images |
| 按描述使用已配置的模型生图，默认一张，会消耗账号额度。 | Generate images with the configured model from a description; one by default, consuming account quota. |
| 完整的图片要求 | Full image requirements |
| 本次提交的唯一编号，断线重试使用原编号 | Unique ID for this submission; reuse it when retrying after a disconnect |
| 编辑图片 | Edit images |
| 按参考图修改，仅在配置了可用编辑模型时提供。 | Edit from reference images; available only when a usable edit model is configured. |
| 参考图链接或已生成图片 ID；本地桥接还可传文件路径 | Reference image URLs or generated image IDs; the local bridge also accepts file paths |
| 查询生成结果 | Query generation result |
| 查询原任务并取回图片，不再次生图或扣费。 | Query the original job and retrieve images without regenerating or charging again. |
| 任务编号或原提交编号，至少提供一项 | Job ID or original request ID; provide at least one |
| 图片分析 | Image analysis |
| 把 1～10 张图片与提示词交给后台配置的视觉模型，返回模型回复文本。 | Send 1–10 images and a prompt to the configured vision model and return its text reply. |
| 要分析的图片（1～10 张），每张为 http(s) URL 或图片 data URI | Images to analyze (1–10), each an http(s) URL or image data URI |
| 提示词，描述要让模型在图片中查看的内容 | Prompt describing what the model should look for in the images |

### 6.3 新增：安装/卸载弹窗

| key（zh） | en |
|---|---|
| 安装目标 | Install targets |
| 已选择 {{count}} / {{total}} 个 | {{count}} / {{total}} selected |
| 未检测到 | Not detected |
| 已检测到 | Detected |
| 请至少选择一个目标 | Select at least one target |
| 请先选择 API Key | Choose an API key first |
| 没有可用的网关 Key | No gateway key available |
| Key 明文只会写入所选客户端的本机配置文件。 | The plaintext key is written only into the selected clients' local config files. |
| 当前没有可用的安装目标 | No install targets available |
| 调用用的 API Key | API key for calls |
| 安装 {{name}} | Install {{name}} |
| {{name}} 接入客户端 | Connect {{name}} to clients |
| 写入配置 | Write config |
| 写入失败 | Write failed |
| 卸载失败 | Uninstall failed |
| 移除 {{name}} 配置 | Remove {{name}} config |
| 已处理 {{count}} 个目标 | Processed {{count}} targets |
| 已完成 {{count}} 个目标 | Completed {{count}} targets |
| 已安装到 {{count}} 个客户端 | Installed to {{count}} clients |
| 已写入 {{count}} 个客户端配置 | Wrote config for {{count}} clients |
| 正在加载安装目标… | Loading install targets… |
| 当前没有已安装/已配置的目标 | No installed or configured targets |
| 选择安装目标、运行时与调用用的 API Key。Skill 包与密钥只写入所选客户端的本机目录。 | Choose install targets, runtime, and the API key for calls. The skill package and key are written only into the selected clients' local directories. |
| 选择要写入 MCP 配置的客户端。配置只动本服务那一项，客户端原有的其它 MCP 服务保持不变。 | Choose the clients to write MCP config into. Only this service's entry changes; other MCP servers stay untouched. |
| Node.js 无需额外依赖，推荐使用；Python 需要本机有 python。 | Node.js needs no extra dependencies and is recommended; Python requires a local python. |
| 本地桥接需要 Node.js 18 或更新版本，自动读取授权目录的参考图并保存生成结果；HTTP 接入只返回下载地址。 | The local bridge needs Node.js 18+ and reads reference images from authorized directories and saves results; HTTP access only returns download URLs. |
| 本地桥接会在本机运行一个 node 脚本，支持直接传本地图片路径，推荐在桌面端使用。 | The local bridge runs a node script locally and accepts local image paths; recommended on desktop. |
| 远程 HTTP 只接受图片 URL 或 data URI，不需要本机安装 node。 | Remote HTTP accepts only image URLs or data URIs and needs no local node. |
| 选择已有目录，图片只会写入此目录及其子目录 | Choose an existing directory; images are written only there and its subdirectories |
| 选择目录 | Choose directory |
| 可选，每行一个已有绝对目录。输出目录默认允许读取；其他位置的文件不会读取或上传。 | Optional; one existing absolute directory per line. The output directory is readable by default; files elsewhere are not read or uploaded. |
| 只保存新文件，不覆盖已有不同内容。目录权限写入客户端配置，更新配置时需要重新确认。 | Only new files are saved; existing different files are not overwritten. Directory permissions are written into client config and need re-confirmation on update. |
| 客户端调用超时设为 120 秒；后台生成超时由服务配置控制，未完成时通过任务编号查询。 | Client calls time out at 120 seconds; background generation timeout is controlled by service config; query unfinished jobs by ID. |
| 将按服务的工具执行超时（{{seconds}} 秒）同步写入客户端配置。 | The service tool timeout ({{seconds}}s) will be written into the client config. |
| 选择要从中移除的客户端，未勾选的客户端保持不变。 | Choose clients to remove from; unchecked clients stay unchanged. |

> 已有可复用 key（勿重复新增）：`MCP & Skills`、`接入协议`、`状态`、`能力数量`、`版本`、`服务入口`、`操作`、`详情`、`已启用`、`已停用`、`全部`、`刷新`、`没有匹配的工具服务`、`复制`、`已复制`、`复制失败，请手动选择后复制`、`把下面这段合并到客户端配置的 mcpServers 里，并把 Key 换成你自己的。`、`可选`、`秒`、`保存`、`启用 / 停用此服务`、`网络搜索`、`视觉分析`、`图片生成`、`安装`、`更新`、`卸载`、`服务状态已更新`、`取消全选`、`全选`、`图片输出目录`、`网关 Key`、`取消`、`关闭`、`运行时变体`、`推荐`、`接入方式`、`本地桥接脚本`、`远程 HTTP（需客户端支持 HTTP MCP）`、`参考图允许读取的目录（可选，每行一个）`、`网关 Key（写入客户端配置，用于调用本工具）`、`客户端配置示例（mcpServers）`、`回执`、`成功`、`失败`、`已配置`、`未配置`、`获取 API Key`、`编辑`、`密钥已清除`、`请选择提供商`、`上游提供商`、`上游模型`、`思考等级`、`不指定`、`内部调用 API Key`、`请选择提供商与上游模型`、`请选择内部调用 API Key`、`提供商选择`、`搜索引擎（可多选）`、`网页抓取`、`搜索提供商`、`用户工具策略`、`前往配置`、`工具`、`网关基址`、`运行时地址（MCP 端点 / 桥接脚本）`、`桥接脚本`、`已接入 {{n}} 个客户端目录`、`尚未接入本机客户端`、`超时（秒）`、`请选择图片输出目录`。

---

## 7. 用 AIGX 自有 UI 组件重写（禁止照搬 `@ui`）

上游 `@ui` 与 AIGX `components/ui` 不是同一套。**不得**整段拷贝上游 TSX/Tailwind class，按下表替换：

| 上游 `@ui` | AIGX 替换 |
|---|---|
| `Card / CardHeader / CardTitle / CardContent` | `Card`（`title`、`actions`、`bodyClassName`）；标题内的 badges 直接放进 `title` 的 ReactNode |
| `Badge variant=brand/info/success/outline` | `Badge tone=info/success/neutral`（无 brand，用 info；outline→neutral） |
| `Button variant=ghost/destructive` | `Button variant="outline"` / `"secondary"` / `"danger"`；只有 `primary/outline/secondary/danger` |
| `Switch` | `Switch checked onChange ariaLabel disabled`（**不是** `onCheckedChange`） |
| `SegmentedControl` | 视图切换用 `Tabs`（全部/Skill/MCP、服务配置/服务能力）；表单枚举用 `Select` |
| `Table / TableHeader / TableRow / TableCell` | 原生 `<table className="dense-table">` + `<div className="table-wrapper">`（无 Table 原语） |
| `Dialog / DialogContent / DialogHeader / DialogFooter` | 复用 `ToolInstallDialog` 的 `tid-overlay` / `tid-panel` 自定义弹层；详情页无 Dialog |
| `Alert variant=destructive/warning/success` | 无 Alert 组件：破坏性用 `.error-message`，警告/成功用局部 class（如 `.mcp-alert-warn` / `.mcp-alert-ok`），样式写进 `ToolsPage.css` / 工具局部 CSS |
| `Field / FieldLabel / FieldDescription` | 原生 `<label className="…"><span className="…">…</span></label>`（现有面板写法）或 `Input/Select` 的 `label`/`hint` props |
| `Input / Textarea / Select` | 同名 AIGX 组件（`Select` 是原生 select，不是 Radix） |
| `MultiSelect / SearchableSelect` | `MultiSelect` 已存在；可搜索下拉用 `Combobox`，或退化为 `Select`（P2） |
| `Empty` | `EmptyState icon message [action]` |
| `Spinner` | `Loader2`（lucide-react）+ 现有 `*-spin` 动画 class |
| `Checkbox` | 原生 `<input type="checkbox">`（`ToolInstallDialog` 已用） |
| `Tooltip` | `Tooltip label`（现有） |
| `toast` | `useToast()` → `addToast(message, 'success' \| 'error')` |

样式约束：
- **不碰** `components/plugins/*`、刚提交的深色主题全局 CSS；新增样式只用 `ToolsPage.css` 或 `components/tools/*.css` 的局部 class（前缀 `mcp-` / 沿用 `tp-`）。
- 上游 Tailwind 工具类（`flex flex-col gap-3`、`text-subtle`）不要照抄；AIGX 虽装了 Tailwind，但现有页面以语义 class 为主，混用会造成主题切换时颜色不一致。

---

## 8. 后端契约复用（无需改后端）

`frontend/src/api/endpoints/misc.ts` 已封装全部所需接口，直接复用：

| 接口 | 方法 | 用途 |
|---|---|---|
| `api.listToolServices()` | GET `/api/tools/servers` | 列表 |
| `api.patchToolService(id,{protocol,enabled?,timeout_seconds?})` | PATCH `/api/tools/servers/{id}` | 列表/详情开关、详情超时 |
| `api.getToolInstallInfo(id)` | GET `/api/tools/install-info` | 详情 MCP 卡 |
| `api.getToolInstallTargets(id, protocol)` | GET `/api/tools/install-targets` | 列表已接入数、弹窗目标 |
| `api.listToolApiKeys()` | GET `/api/tools/api-keys` | 弹窗 Key |
| `api.installToolSkill/installToolMcp/uninstallToolSkill/uninstallToolMcp` | POST | 弹窗提交 |
| `api.listToolProviders/getToolProviderSelection/saveToolProviderSelection/saveToolProvider/clearToolProviderKey/testToolProvider` | GET/PUT/DELETE/POST | web-search 面板 |
| `api.getToolVisionConfig/saveToolVisionConfig/clearToolVisionConfig/testToolVision` | GET/PUT/DELETE/POST | vision 面板 |
| `imageToolApi.*` | `/api/tools/image-generation/*` | image 面板 |

类型均在 `frontend/src/types/a2a.ts:147+`（`ToolServiceItem`、`ToolInstallInfo`、`ToolInstallTarget`、`ToolApiKeyOption` 等），无需新增。

---

## 9. 验收清单（实现完成后逐条手工验）

1. 侧边栏菜单、页面 H1、Card 标题、全局搜索结果显示「MCP & Skills」；`grep -rn "工具服务" frontend/src` 只剩注释/旧 key 或已清空。
2. 列表为表格，8 列齐全；`Skill`/`MCP`/`全部` 过滤正确；状态开关即时生效并 toast；刷新可用。
3. 点「详情」进入详情；「返回列表」回到列表且数据刷新；刷新页面仍停留 `/tools`（详情是内存态，刷新回列表可接受）。
4. 详情头卡：协议/启用/能力数/版本 badges；超时 5–600 可保存；停用时出现警告条。
5. MCP 协议显示接入信息卡（地址/工具 badges/配置示例/复制）；Skill 协议显示安装提示；示例中不出现 Agent2API 品牌。
6. `服务配置` Tab：web-search 显示「上游提供商配置」并可保存/测试/多密钥；vision 显示「视觉模型配置」；image-generation 显示图片生成配置 + 任务列表。
7. `服务能力` Tab：web-search 2 项、image-generation 2–3 项（编辑图片随 `edit_images` 出现）、vision 1 项；参数名/类型/必填/说明齐全。
8. 安装弹窗：目标三态文案、全选、运行时、接入方式、输出目录、Key、超时提示；全部成功自动关闭，部分失败保留回执。
9. 卸载弹窗：只列已安装目标；标题「移除 … 配置」；危险按钮。
10. `npx tsc --noEmit` / 前端构建通过；zh/en 新增 key 无缺失（缺 key 时英文界面会落回中文，需逐条核对）。
11. 未修改 `components/plugins/*` 与深色主题全局 CSS；未写 provider 伪装/规避检测逻辑。

---

## 10. 明确保留的差异 / 不做

| 项 | 处理 |
|---|---|
| AIGX「用户工具策略」卡 | 保留在列表页（AIGX 独有，上游无） |
| beta.2 多密钥管理 | 保留（上游 beta.6 只有单密钥） |
| 桌面壳 `isDesktopShell()` / 「客户端环境」按钮 | AIGX 无桌面壳概念，不实现；「本机接入」列恒显示 |
| 桌面目录选择 `pickFolder` | AIGX 无桌面桥接，图片输出目录用纯输入框 |
| 安装目标 4 家 vs 13 家 | 后端差异，前端按响应渲染；不在本次前端任务内扩表 |
| MCP 安装「预览 + 确认令牌」 | 后端无此协议，前端不宣称 |
| `WebSearchConfig` 的 4 列卡片 / SearchableSelect | P2 视觉对齐项，不阻塞主结构；先保证功能与文案 |
| 超时上限 1–3600 → 5–600 | 前端按上游收紧；若后端仍接受更大值，以本次规格为准并在整合期确认 |
