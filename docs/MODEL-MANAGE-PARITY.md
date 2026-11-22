# 模型管理字段级一对一审计（AIGX ↔ Agent2API）

> 审计日期：2026-11-22（UTC+8）
> 对象：AIGX「模型管理」页 vs Agent2API 桌面端「模型管理」岛
> 范围：表格列、行内/批量操作、弹窗逐字段、筛选/排序/分页、数据来源与空态、后端 API 字段
> 只读审计，未改动任何代码；本文件是本次唯一改动。证据行号基于审计当日两侧工作区，后续改动可能漂移。

## 0. 结论摘要

| 判定 | 项数 | 说明 |
|---|---:|---|
| 完全对齐 / 等价 | 15 | 勾选、模型测试、映射 chip、启停、能力三态弹窗（6 键）、列设置、状态筛选、持久化等 |
| **差异（需处理）** | **20** | P0=0 / P1=8 / P2=12，见下方“差异清单” |
| AIGX 独有（A2A 无对应） | 9 | 定价子系统、价格筛选、别名前缀批量、元信息覆盖、模型测试协议/节点之外的来源多选等 |

**首要结论（P1）**：AIGX 的“定价”列与 A2A 的“倍率”列**不是同一量纲**——A2A 展示上游返回的 `credits`（只读文本 + 排序键），AIGX 展示运营设置的定价（token/count/free + ratio），**AIGX 数据模型里根本没有 `credits`**；加之 A2A 的“上下文/输出”列和“来源（清单三态）”列在 AIGX 被并入/替换，且批量“删除”在 A2A 删除模型登记、在 AIGX 删除的是定价——这些是本轮最需要决策的字段级缺口。

## 1. 证据口径与文件简写

- **A2A 根**：`D:\GitHub\Agent2API-main\desktop-tauri`
  - `ISL/` = `ui-islands/src/islands/`
  - `SRV/` = `src-tauri/server/src/server/`
- **AIGX 根**：`D:\GitHub\AIGX`
  - `MM` = `frontend/src/pages/ModelManage.tsx`
  - `MCD` = `frontend/src/components/ModelCapabilityDialog.tsx`
  - `MTD` = `frontend/src/components/ModelTestDialog.tsx`
  - `MB` = `src/api/admin/model_manage.rs`
  - `MModel` = `src/model/manage.rs`
  - `MAIN` = `src/main.rs`
  - `TYPES` = `frontend/src/types/index.ts`

> 约定：A2A 后端字段是 **camelCase**（`providerLabel`/`capSources`/`reasoningLevels`/`mapping.provider`…）；AIGX 后端结构体无 `rename_all`，输出 **snake_case**（`provider_label`/`cap_sources`/`model_ratio`/`alias_entries`…）。下表只按语义对齐，命名风格差异单列，不逐行重复。

---

## 2. 表格列

A2A 列定义：`ISL/models-panel-state.ts:208-218`（check / model / rate / source / budget / caps / alias / act，共 8 列）
AIGX 列定义：`MM:80-135`（`COL_ORDER` `MM:83`，`MM_COL_DEFS` `MM:125`；check / model / price / source / caps / alias / act，共 7 列）

| 功能/字段 | A2A 证据 | AIGX 证据 | 差异 | 处理建议 | 优先级 |
|---|---|---|---|---|---|
| 选择列 check | `ISL/models-panel-state.ts:208`；渲染 `ISL/models-page.tsx:728-734` | `MM:83,102`；行勾选 `MM:1417-1427`、表头全选 `MM:1733-1739` | 无（列名/顺序一致，居中） | — | — |
| 模型列 | `ISL/models-page.tsx:736-747`：`id`（点击复制）+ `name` 副行（`name!==id` 时） | `MM:1443-1447`：`id` + 副行 `provider_label · 上下文` | A2A 副行是**模型名**；AIGX 副行是**来源家名+上下文**，且 `ManageModel` 无 `name` 字段（`MModel:196-243`） | AIGX 增补 `name` 字段并在副行/悬浮展示；是否保留 provider 副行由信息架构定 | P2 |
| **倍率列 rate ↔ 定价列 price** | `ISL/models-panel-state.ts:210`（“倍率”）；取值/格式化 `ISL/models-page.tsx:166-184,749-753`：取 `model.credits`（`x0.7 credits` 文本），只读展示 + `rate-asc/desc` 排序键 | `MM:104`（“定价”）；渲染 `MM:1449-1482`：`price`（token/次/免费）+ `model_ratio`（Nx）；点击开定价弹窗 | **量纲不同**：A2A `credits`=上游倍率（只读、排序），AIGX 换成运营定价（可编辑）。**AIGX 数据模型无 `credits`**（`MModel:196-243`；A2A `SRV/core/providers/catalog/view.rs:207-231` 有 `credits`） | 二选一：①AIGX 后端 catalog 透传 `credits` 并在“定价”列并存“上游倍率”；②确认 AIGX 以定价替代倍率属有意分叉，记录为设计差异 | **P1** |
| **来源列 source** | `ISL/models-page.tsx:597-611`：按后端 `source` 分 `manual`/`remote`/`builtin` 三态色点（清单**来源**） | `MM:1483-1507`：渲染 `sources[]`（`account`/`channel` 承载方 tag + `models.dev` 标），语义是**承载方** | **语义不同**：A2A=“清单从哪来”，AIGX=“谁承载”。AIGX 模型行不展示 `manual/remote/builtin` 清单来源（该信息只在 `ProviderGroup.source`，`MModel:270-275`） | 确认 AIGX 需要展示的是承载方（保留），还是同时补“清单来源”三态点；若补，需 `ManageModel` 增加 model 级 `source` | **P1** |
| **上下文/输出列 budget** | `ISL/models-panel-state.ts:215`；渲染 `ISL/models-page.tsx:633-654`：`maxInputTokens / maxOutputTokens` 两数并列，标 models.dev 补齐（虚线）与“已覆盖上游值”标记 | 无独立列；上下文只以**一个数**进 caps 列（`MM:1541-1547` 仅 `row.context_length`） | A2A 分显**输入+输出 token**，AIGX 只显示输入上下文、**无输出 token 展示**；且列被合并进 caps | 增加“上下文/输出”列（或 caps 列内补输出 token + models.dev/已覆盖标记）；`capability_flags` 已含两键（`MModel:217-218`），只需前端展示 | **P1** |
| 能力列 caps（徽标） | `ISL/models-page.tsx:674-700`：4 键徽标（工具/图片/视频/思考）+ 覆盖标记；键集 `ISL/model-capability.ts:40-51` | `MM:142-148`（`CAP_DOTS` 5 个：工具/图像/视频/音频/思考）；渲染 `MM:1508-1562`，含三态圆点 + models.dev 内侧描边 + 上下文 + 两个编辑入口 | AIGX 多 1 个 **audio** 圆点（11 行/`MM:146`），A2A 无 audio 键；两者三态判定语义一致 | 若要对齐：确认 audio 是否为 AIGX 扩展；`capability_flags` 六键是否含 audio 需核对 `MModel:211-218` 与后端 KEYS | P2 |
| 能力来源三档（upstream/modelsdev/user）展示 | `ISL/models-page.tsx:674-700` 用 `capSources`（`view.rs:214-219`），cells 区单列来源三档 | 后端已发 `cap_sources`（`MModel:211-214`），但 caps 列只读 `row.capabilities`（`MM:1518`），**未消费 `cap_sources`** | 数据已具备、前端未用：AIGX 看不到“上游实测/社区补齐/用户改”可信度 | caps 列/悬浮按 `cap_sources` 分档上色（与 A2A 同口径） | **P1** |
| 模型映射列 alias | `ISL/models-page.tsx:519-590`：chip + 开关 + 思考等级标 + 删除 × + “＋ 映射” | `MM:1595-1650`：chip + `Switch` + `→target` 常显 + 思考标 + × + “映射” | 等价（AIGX 额外常显 target 名）。写接口不同见 §6 | — | — |
| 操作列 act | `ISL/models-page.tsx:763-780`：测试 + 移除（仅自定义/手动） | `MM:1654-1700`：测试 + 定价 + 删定价 + 删自定义 | AIGX 多“定价/删定价”（对应其定价子系统） | 非差距（AIGX 独有） | — |
| 列设置（显隐/拖序/对齐/宽度） | `ISL/models-panel-state.ts:30-44`（ColSettingsHandle）；`ISL/models-page.tsx:986-1026` | `MM:80-135`（`COL_DEFAULT_W`/`COL_DEFAULT_ALIGN`/`LOCKED_COLS`/`applyTableColumns` `MM:1120`） | 等价（列设置能力一致） | — | — |
| 独立“状态/启用”列 | 无（禁用行压淡 + 映射开关） | 无（`MM:81` 注释“对齐 Agent2API：无启用列”，压淡 + 默认绑定开关） | 等价 | — | — |

---

## 3. 行内与批量操作

| 功能/字段 | A2A 证据 | AIGX 证据 | 差异 | 处理建议 | 优先级 |
|---|---|---|---|---|---|
| 行启停（默认绑定开关） | `SRV/api/model_manage.rs:62-131`（`set_state`，`provider`+`id`+`enabled`） | `MM` 默认绑定开关；`frontend/src/api/endpoints/users_models.ts:65-66`→`POST /api/models/state`；后端 `MB:154-184` | 等价 | — | — |
| 别名 chip 开关 | `ISL/models-panel-state.ts` 写绑定；`ISL/models-page.tsx:550-574` | `MM:1604-1610`→`setModelAliasState`→`POST /api/models/alias/state`（`MB:578-606`） | 等价（A2A 走 mapping 三元组，AIGX 单独 alias/state） | — | — |
| 行内测试 | `ISL/model-test-dialog.tsx`；act `ISL/models-page.tsx:763-780` | `MM:1654-1670`→`ModelTestDialog`；`POST /api/models/test`（`MAIN:2091`） | 等价（字段差异见 §4 测试弹窗） | — | — |
| 行内移除自定义模型 | `ISL/models-page.tsx:702-723`（`source=manual`/自定义才可删） | `MM:1689-1700`→`removeCustomModel`→`POST /api/models/custom/remove`（`MB:1035-`） | 等价 | — | — |
| 行内定价/删定价 | A2A **无** | `MM:1671-1688`→`POST /api/models/price`、`/price/remove`（`MAIN:2092-2099`） | AIGX 独有（A2A 无定价子系统） | 记录为设计差异 | — |
| 批量：启用 / 禁用 / 设思考等级 | `ISL/models-batch-dialog.tsx:54-57`（enable/disable/reasoning/delete） | `MM:2988-3055`（enable/disable/set-reasoning/delete-price） | 前 3 项等价 | — | — |
| **批量：删除** | `ISL/models-batch-dialog.tsx:54-57,96-117,156-161`：删除**模型登记**（跳过非手动登记，二次确认） | `MM:3009`（`delete-price`）、`MM:3055`：批量“删除定价”，**不删除模型** | **动作不同**：A2A 删模型登记，AIGX 同位置是删定价；AIGX 无“批量删除模型” | 若需对齐：AIGX 增加“批量删除自定义模型登记”（复用 `/custom/remove`，仅对自定义/手动行）；否则在 UI 明确“删除定价”文案避免误解 | **P1** |
| 批量：前缀映射（prefix+sep+合并） | A2A **无** | `MM:2753-`（前缀弹窗）→`POST /api/models/alias/prefix`（`MB:749-`）；契约 `users_models.ts:97-108` | AIGX 独有 | 记录为增强 | — |
| 跨分页全选 | `ISL/models-page.tsx:422`（全选当前筛选，跨分页） | `MM` 表头全选（勾选集合跨页） | 等价 | — | — |

---

## 4. 对话框逐字段

### 4.1 映射弹窗（model mapping）

A2A：`ISL/models-page.tsx:1106-1301`（`MappingDialog`，`provider` 由 context 固定；`target` 只能从该家清单下拉，见 `:1101`）
AIGX：`MM:2576-2690`（`setAliasTarget` 弹窗）

| 字段 | A2A 证据 | AIGX 证据 | 差异 | 处理建议 | 优先级 |
|---|---|---|---|---|---|
| 对外映射名 alias | `:1235-1238`（校验 `:1171`；允许 `- _ . / :` `:1301`） | `MM:2591`（`对外映射名…*`） | 等价 | — | — |
| 上游模型 target | context 下拉（`provider` 固定，`:1101`） | `MM:2602`（`转发到上游模型` combobox，可跨家选） | AIGX 可选范围更宽（非固定家） | 确认是否需收敛到“当前家”以对齐；否则记录差异 | P2 |
| 思考等级 | 下拉 + 自定义（`:1278-1287`，`CUSTOM_LEVEL`） | `MM:2612`（映射级思考等级 + custom） | 等价 | — | — |

### 4.2 能力弹窗（capability tri-state）

A2A：`ISL/model-capability.ts:23-51`（6 键：`maxInputTokens/maxOutputTokens/supportsToolCall/supportsImages/supportsVideo/supportsReasoning`），三态 `inherit/on/off`（`:222-247`）
AIGX：`MCD:24-29`（4 布尔键）+ token 两键；`MCD:82-110`（稀疏覆盖提交，`null`=清除）

| 字段 | A2A 证据 | AIGX 证据 | 差异 | 处理建议 | 优先级 |
|---|---|---|---|---|---|
| 4 布尔能力位三态 | `ISL/model-capability.ts:40-51,222-247` | `MCD:24-29,32-38`（继承/是/否） | 等价（键与三态一致） | — | — |
| maxInputTokens / maxOutputTokens | `ISL/model-capability.ts:45-47`；范围 1..1e8（`MCD` 同类校验 `MCD:84-95`） | `MCD:45-47,82-110`（1–100000000，空=继承） | 等价 | — | — |
| 编辑入口 | caps 列点击即开能力弹窗（`ISL/models-page.tsx:641-642`） | **两个入口**：dot 区开“元信息覆盖”表单（`MM:1515`→`MM:2346-2462`），Sliders 小按钮开能力三态弹窗（`MM:1552-1560`→`MCD` `MM:3071-...`） | AIGX 能力编辑被拆成两个近义弹窗（目录级声明 vs provider 级三态，见 `MM:1549-1551` 注释） | 明确两入口职责或在 UI 文案区分；避免同一“能力”两处编辑 | P2 |

### 4.3 元信息覆盖弹窗（AIGX 独有）

AIGX：`MM:2346-2462`（`metaForm`：模型 ID / 归属方 `owned_by` / 上下文窗口 / 能力“继承|显式”+5 开关 / 其他标签 `extraTags` / 思考等级候选 `reasoningLevels`）；覆盖列表 `MM:2264-2338`
A2A：**无对应逐模型覆盖弹窗**；其 `/api/models/metadata`（`SRV/http.rs:469-473`）是 models.dev 缓存状态，不是逐模型覆盖。

| 差异 | 处理建议 | 优先级 |
|---|---|---|
| AIGX 提供逐模型 `owned_by`/上下文/额外能力标签/思考候选覆盖；A2A 无 | AIGX 独有能力，保留；确认与 A2A 的“能力弹窗”边界，避免与 §4.2 重复 | P2 |

### 4.4 自定义/添加模型弹窗

A2A：`ISL/models-page.tsx:1407-1414`（`上游模型 ID`，家由 context 定；登记后可请求）
AIGX：`MM:2694-2748`（“新建自定义模型”：模型 ID / 提供商（默认 custom）/ **归属方（采集但不提交）**）+ `MM:2925-2980`（“添加自定义模型”：Provider + 模型 ID）

| 字段/差异 | A2A 证据 | AIGX 证据 | 差异 | 处理建议 | 优先级 |
|---|---|---|---|---|---|
| 模型 ID | `:1407` | `MM:2705-2714`（新建）/ `MM:2967`（添加） | 等价 | — | — |
| 归属方 owned_by | 无 | `MM:2725-2734`（输入）；`saveCustom` 不传（`MM:872-873` 注释“远程后端暂不支持”） | AIGX 死字段（采集不提交） | 移除输入或接线后端 `owned_by`；不要留静默无效字段 | P2 |
| 入口数量 | 1 个“添加模型” | 2 个近义弹窗（新建/添加自定义模型） | AIGX 重复入口 | 合并为一个（provider+id+可归属方），避免歧义 | P2 |

### 4.5 获取/刷新模型弹窗

A2A：`ISL/models-fetch-modal.tsx`：per-provider 行 + **“模型来源”账号下拉**（`:126-128,221,248-272`，落盘 `{provider:accountId}`）+ 结果 `已刷新/失败/跳过` 三态（`:208-218`）
AIGX：`MM:2826-2921`：per-渠道/供应商行 + “全部拉取/重试” + 结果 + “清理失效映射 N 条”；后端走 `fetchChannelModels`（`frontend/src/api/endpoints/tickets.ts:197`→`/api/channels/fetch_models`）

| 差异 | A2A 证据 | AIGX 证据 | 处理建议 | 优先级 |
|---|---|---|---|---|
| 按 provider 选“用哪个账号拉目录” | `ISL/models-fetch-modal.tsx:126-128,221,248-272` | 无账号下选（账号池按账号内部逐号拉，见 `MM:2836` 文案） | 若需对齐：AIGX 拉取弹窗增加“账号”列/下拉并传 `account_id`；否则记录差异（AIGX 自动逐号） | P2 |
| 刷新触发 | `POST /api/models/refresh`（`SRV/http.rs:454`） | `POST /api/channels/fetch_models`（逐渠道） | 语义不同（全局 vs 逐渠道）；确认 AIGX 是否需全局一键刷新端点 | P2 |

### 4.6 批量弹窗（字段）

A2A：`ISL/models-batch-dialog.tsx:54-57`（动作单选 enable/disable/reasoning/delete；reasoning 分支 `:79-117`，自定义等级必填 `:111-112`；删除二次确认 `:96-103`）
AIGX：`MM:2988-3055`（动作 enable/disable/set-reasoning/delete-price；reasoning 留空=清除 `MM:3030-3040`）

| 差异 | A2A 证据 | AIGX 证据 | 处理建议 | 优先级 |
|---|---|---|---|---|
| 第 4 个动作语义 | 删除模型登记 | 删除定价 | 见 §3 批量删除行；按决策补“删除模型登记”动作 | **P1** |
| 思考等级“清除”语义 | 有 CUSTOM_LEVEL / 清除 | 留空=清除已有等级（`MM:3030`） | 等价 | — |

### 4.7 模型测试弹窗（逐字段）

A2A：`ISL/model-test-dialog.tsx:9-11,71-99`（对话**协议** `chat_completions/responses/anthropic`；**测试节点** `proxy` `direct/pool:<id>`；账号**多选**；提示词/系统提示词/思考/流式）
AIGX：`MTD:66-78,128-152,221-294`（系统/用户提示词 + 思考等级 + 流式 + **测试来源多选**（账号/渠道）；payload `provider,model,account_id?,channel_id?,prompt,system_prompt,reasoning_effort,stream`）

| 字段 | A2A 证据 | AIGX 证据 | 差异 | 处理建议 | 优先级 |
|---|---|---|---|---|---|
| 系统/用户提示词 | `:9-11` | `MTD:221-245` | 等价 | — | — |
| 思考等级 | `:71-99` | `MTD:246-260` | 等价 | — | — |
| 流式 | `:76` | `MTD:262-265` | 等价 | — | — |
| **对话协议 protocol** | `:9-11,77-78`（chat_completions/responses/anthropic） | 无 | AIGX 测试无法选协议分支 | 增加 `protocol` 选择并透传到 `/api/models/test`（后端需支持） | **P1** |
| **测试节点 proxy** | `:79-80`（跟随账号/direct/pool:<id>） | 无 | AIGX 测试无法临时指定出网节点 | 增加节点选择（或等后端支持后接） | **P1** |
| 测试账号选择 | 账号多选（每账号一条请求） | 来源多选（账号/渠道，每来源一条） | AIGX 多“渠道”维度 | 等价增强（AIGX 更宽） | — |

---

## 5. 筛选 / 搜索 / 排序 / 分页

| 功能/字段 | A2A 证据 | AIGX 证据 | 差异 | 处理建议 | 优先级 |
|---|---|---|---|---|---|
| 状态筛选 | `ISL/models-panel-state.ts:174,176-192`（all/enabled/disabled/mapped） | `MM:397,1199-1203`（all/on/off/mapped） | 等价（命名 on/off） | — | — |
| 价格筛选 | 无 | `MM:398,1204-1205`（priced/unpriced） | AIGX 独有 | 记录 | — |
| 来源筛选 | 无（来源在列内） | `MM:399,1206-1207`（channel/account） | AIGX 独有 | 记录 | — |
| 排序档位 | `ISL/models-panel-state.ts:193-198`：default / rate-asc / rate-desc（按 `credits`） | `MM:400,1216-1218`：default / ratio-asc / ratio-desc（按 `model_ratio`） | 排序键不同（credits vs ratio），与 §2 倍率/定价同源 | 随倍率/定价决策统一 | P2 |
| 搜索字段 | `ISL/models-page.tsx:953`（模型 ID / 名称 / 映射名） | `MM:1209-1211`（`id` + `owned_by` + `provider_label` + `aliases` + `sources[].name`） | AIGX 多 owned_by/来源名、缺“模型名”（无 `name` 字段） | 随 §2 模型列补 `name` 后加入搜索 | P2 |
| 分页档位 | `ISL/table-shell.tsx:50,53`：`[20,50,100,200,'all']`，默认 **50** | `MM:53-54,411`：`[20,25,50,100]`，默认 **25**（无 200、无 all） | AIGX 缺 200/all，多 25；默认值不同 | 若对齐：补 200 与“全部”（需评估大目录性能），默认值统一 | P2 |
| 左栏 provider/自定义家分层 | `ISL/models-page.tsx:455-497`（内置全部 + 各家 + 自定义组） | `MM` 左栏 rail（provider/渠道分组 + 自定义组，分组折叠 `GROUP_LIMIT=8` `MM:56-57,2182`） | 等价（AIGX 增分组折叠） | — | — |
| 筛选/列设置持久化 | A2A `localStorage`（`ISL/models-panel-state.ts:107-108,238-276`） | `MM:60,406-411`（`aigx-mm-filters`、列宽/页大小） | 等价 | — | — |

---

## 6. 数据来源 / 空态 / 后端 API 字段

### 6.1 主视图字段

A2A `GET /api/models/manage` → `{models, mappings, reasoningLevels, modelsDev}`（`SRV/http.rs:456`；`SRV/core/providers/catalog/view.rs:249-257`）
- `models[]`（`view.rs:207-231`）：`id, name, credits, isDefault, provider, providerLabel, source, enabled, aliases, capabilities, capOverrides, capSources, reasoningLevels, reasoningDefaultLevel, refreshedAt`
- `mappings[]`（`view.rs:174-247`）：`alias, target, provider, enabled, reasoning, isDefault, dangling, carried`

AIGX `GET /api/models/manage` → `ManageView`（`MB:93-153`；结构 `MModel:278-298`）
- `models[]`（`MModel:196-243`）：`provider, provider_label, id, enabled, default_enabled, sources[], owned_by, context_length, capabilities, cap_sources, capability_flags, tool_calling_mode, cap_overrides, price, model_ratio, aliases, alias_entries[]`
- `mappings`（HashMap，只含全局）、`mapping_entries[]`、`dangling[]`、`capability_store_ready`、`unpriced`、`providers[]`

| 字段/差异 | A2A 证据 | AIGX 证据 | 差异 | 处理建议 | 优先级 |
|---|---|---|---|---|---|
| **`mappings` 形状** | 数组（每项含 `dangling/carried/provider`）`view.rs:174-247` | `mappings` 是 **HashMap<String,String>**（仅全局），逐条在 `mapping_entries[]`，悬空在 `dangling[]`（`MModel:283-289`） | **形状不同**：A2A 单数组带标记，AIGX 三份（map + entries + dangling） | 前端如按 A2A 单数组消费会漏 provider 级/悬空条目；建议统一为单数组（保留旧 `mappings` map 兼容） | **P1** |
| **悬空映射字段名** | `mappings[].dangling`（`view.rs:246`） | 后端字段 `dangling`（`MModel:289`）；前端类型/消费却是 `dangling_mappings`（`TYPES:789`，`MM:1377`） | **字段名不一致**：前端读 `dangling_mappings`，后端发 `dangling` → 前端只能回退自行推断（`MM:1385-1388`），可能读不到后端事实 | 统一字段名（后端补 `dangling_mappings` 或前端改读 `dangling`），并覆盖 `mapping_entries` 类型 | **P1** |
| per-model `credits` | `view.rs:208` | 无（`MModel:196-243`） | AIGX 缺字段（同 §2 倍率） | 随 §2 决策补数据 | **P1** |
| per-model `name` | `view.rs:208` | 无 | AIGX 缺字段 | 随 §2 模型列补 | P2 |
| per-model `reasoningLevels/reasoningDefaultLevel` | `view.rs:225-226` | 无（AIGX 用全局候选 + meta override `MM:294-308`） | AIGX 无逐模型思考档位候选 | 若需逐模型候选，后端 view 透传；否则记录 | P2 |
| per-model `capSources` | `view.rs:219` | 有 `cap_sources`（`MModel:211-214`）但前端未用（§2） | 数据具备、未展示 | 前端消费 | **P1** |
| per-model `isDefault` / `refreshedAt` | `view.rs:209,230` | `model_ratio`/`price`、`ProviderGroup.refreshed_at`（`MModel:273-275`） | 语义不同（AIGX 默认绑定在 `default_enabled`） | 记录差异，非缺口 | P2 |
| 定价字段 `price`（token/count/free/ratio） | **无** | `MModel:223-232`；定价引擎 `src/pricing/mod.rs` | AIGX 独有定价子系统 | 记录为增强 | — |
| tool_calling_mode | A2A 在模型条目另有出口（`/v1/models`） | `MModel:219-225`（管理视图已带） | AIGX 管理页可见；A2A 管理表不显式列 | 记录 | P2 |

### 6.2 端点对照

| 能力 | A2A 路由（`SRV/http.rs`） | AIGX 路由（`MAIN`） | 差异 |
|---|---|---|---|
| 主视图 | `GET /api/models/manage`（`:456`） | `GET /api/models/manage`（`:2088`） | 一致 |
| 启停 | `POST /api/models/state`（`:457`） | `POST /api/models/state`（`:2089`） | 一致 |
| 映射增/删 | `POST /api/models/mappings`、`/mappings/remove`（`:458-459`） | 同左（`:2119-2127`）+ 额外 `alias`/`alias/state`/`alias/remove`/`alias/reasoning`/`alias/prefix`（`:2100-2117`） | AIGX 拆分更多写端点 |
| 自定义模型 | `POST /api/models/custom`、`/custom/remove`（`:461-462`） | 同左（`:2132-2139`） | AIGX handler 名称不同（`add_custom_model`） |
| 能力位 | `POST /api/models/capabilities`（`:465`） | 同左（`:2128-2131`） | AIGX 同时支持新三态稀疏表与旧标签形态（`MB:839-960`） |
| 元数据 | `GET/PUT /api/models/metadata` + `/metadata/refresh`（`:469-473`，models.dev 缓存） | `GET /api/models/meta`、`PUT/DELETE /api/models/meta/:model`（`:2141-2149`，逐模型覆盖） | **语义不同**：A2A=models.dev 状态；AIGX=逐模型覆盖 |
| 刷新目录 | `POST /api/models/refresh`（`:454`） | `POST /api/channels/fetch_models`（前端 `tickets.ts:197`，逐渠道） | 全局 vs 逐渠道 |
| 测试 | `POST /api/models/test`（`:478`） | `POST /api/models/test`（`:2091`） | 一致（字段见 §4.7） |
| 定价 | 无 | `/api/models/price`、`/price/remove`（`:2092-2099`） | AIGX 独有 |
| 映射思考等级 | 随 mapping body（`SRV/api/model_manage.rs` 三态 reasoning） | 独立 `POST /api/models/alias/reasoning`（`:2109-2112`） | AIGX 批量化端点 |

### 6.3 空态

| 空态 | A2A 证据 | AIGX 证据 | 差异 |
|---|---|---|---|
| 加载中 | `ISL/models-page.tsx` | `MM` “加载模型目录” | 等价 |
| 无结果 / 搜索空 | `ISL/models-page.tsx:820-823`（自定义家/内置家分别文案） | `MM:2153`（筛选/搜索变体） | 等价 |
| 未定价 / 免费 | A2A 无定价（N/A） | `MM:1466-1479`（“免费” vs “未定价”区分，`isFreePrice` `MM:187-192`） | AIGX 独有 |
| 未挂载映射区块 | A2A 以 `mappings[].dangling` 表达，无独立区块 | `MM:2067-2077`“未挂载的模型映射”独立区块（`MM:1370-1400` 取数） | AIGX 有独立区块；取数依赖 §6.1 字段名统一 |
| 非管理员可见性 | 桌面单用户（无过滤） | `MB:102-138` 非管理员只返回“已启用+分组允许”且清空来源/映射 | 架构差异（AIGX 多租户） |

---

## 7. 差异清单（按优先级）

### P1（8 项，字段/行为级，建议随 #51 实施）

1. **倍率 `credits` 缺失**：AIGX 以“定价”替代，数据模型无 `credits`（§2、§6.1）。
2. **上下文/输出列**：A2A 分显输入+输出 token 及 models.dev/已覆盖标记，AIGX 仅显示输入上下文（§2）。
3. **来源列语义**：A2A=清单来源三态（manual/remote/builtin），AIGX=承载方；AIGX 模型行缺清单来源（§2）。
4. **`capSources` 已发未用**：AIGX 后端 `cap_sources` 具备，前端 caps 列未消费，可信度不可见（§2、§6.1）。
5. **批量删除语义**：A2A 删模型登记，AIGX 同位置删定价；AIGX 无批量删模型（§3、§4.6）。
6. **测试弹窗缺 `protocol`**：无法选 chat_completions/responses/anthropic（§4.7）。
7. **测试弹窗缺 `proxy`（测试节点）**：无法临时指定出网节点（§4.7）。
8. **`dangling` 字段名/`mappings` 形状不一致**：后端 `dangling` vs 前端 `dangling_mappings`；AIGX 三份 vs A2A 单数组（§6.1）。

### P2（12 项，展示/一致性/冗余，可排后）

1. 模型列副行：A2A 显示 `name`，AIGX 显示 provider+context，且无 `name` 字段（§2、§6.1）。
2. caps 列多出 audio 圆点（A2A 4 键，AIGX 5 圆点）（§2）。
3. per-model `reasoningLevels/reasoningDefaultLevel` AIGX 未消费（§6.1）。
4. 映射弹窗 target 可选范围：A2A 固定家下拉，AIGX 跨家 combobox（§4.1）。
5. 两个能力编辑入口（元信息覆盖 vs 能力三态弹窗）职责重叠（§4.2）。
6. 自定义模型“归属方 owned_by”采集但不提交（死字段）（§4.4）。
7. “新建自定义模型”与“添加自定义模型”两个近似入口（§4.4）。
8. 获取模型弹窗缺按 provider 选账号（模型来源）（§4.5）。
9. 刷新触发：全局 `/models/refresh` vs 逐渠道 `fetch_models`（§4.5、§6.2）。
10. 排序键 credits vs model_ratio（§5）。
11. 搜索字段缺 `name`、多 `owned_by`/来源名（§5）。
12. 分页档位 `[20,50,100,200,all]`/默认50 vs `[20,25,50,100]`/默认25（§5）。

### AIGX 独有（9 项，非差距，记录为设计分叉）

定价子系统（列/弹窗/删定价/价格筛选）、别名前缀批量、映射级批量思考等级端点、元信息覆盖（owned_by/上下文/额外标签）、覆盖列表弹窗、`tool_calling_mode`、非管理员过滤、models.dev 补齐标记、`未挂载映射`独立区块。

---

## 8. 说明与证据边界

- 本审计仅比对**两侧代码可见字段**；A2A 为桌面单用户客户端，AIGX 为多租户网关，部分差异（如非管理员过滤、渠道/账号承载方）源于架构而非缺陷，已在表中标注“架构差异/独有”。
- A2A 无定价子系统，故 AIGX 的定价相关列/弹窗/筛选/端点一律记为“AIGX 独有”，不计入差距计数。
- “完全对齐/等价”指字段语义与交互一致，不排除命名风格（camelCase vs snake_case）与文案差异。
- 行号基于 2026-11-22 工作区；若后续改动漂移，以实际代码为准。