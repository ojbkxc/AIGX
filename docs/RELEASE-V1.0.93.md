# v1.0.93 发布说明（草稿）

> 日期：2026-11-23（UTC+8）· 状态：**工作区快照，未提交/未打包**
> 本文件按用户提出的 **9 个问题**逐条给出「问题 → 改动 → 验证方式」。
> 口径说明：改动文件以当前工作区为准；其中「设置菜单大块迁移」由任务 #11 收口中，终态以全量复核（#12）结果为准。
> **验证一栏为「生产部署后回填」的待办清单**（本地已过 cargo fmt/clippy/test 与前端 tsc/lint/i18n，真机验证待部署 104 后补）。

---

## ① 账号池筛选条太挤 → 主行 4 控件 + 「更多筛选」折叠

**问题**：账号池顶部筛选控件过多（状态/限额/冷却/仅需重登/仅已过期/到期分档/用量分档/到期区间/用量区间…），一行铺满、密度过高。

**改动**：
- `frontend/src/pages/AccountSessions.tsx`：主筛选行只留 **4 个控件**——提供商、搜索、状态分段、限额状态分段；其余细分（冷却中 / 仅需重登 / 仅已过期 / 到期分档 / 用量分档 / 到期与用量区间）收进可折叠的 **「更多筛选」`<details>` 抽屉**（默认收起，桌面降密度；对齐 A2A 窄屏收 ResponsiveSheet 思路）。
- 「清除筛选」只重置筛选维度（分组/排序/余额刷新间隔/列设置属偏好，不动）。
- `frontend/src/pages/AccountSessions.css`、`frontend/src/i18n/{zh,en}.json`（新增「更多筛选 / 按冷却过滤 / 更多状态筛选 / 仅显示处于冷却中的账号」等键）。

**验证**：
1. 打开账号池：主筛选行仅 提供商 / 搜索 / 状态 / 限额 4 控件。
2. 展开「更多筛选」：冷却、仅需重登、仅已过期、到期分档、用量分档、区间上下限均可用且生效（结果与折叠前一致）。
3. 「清除筛选」后：筛选维度清空，分组/排序/列设置保持不变。

---

## ② 账号列太宽 → 列瘦身 + 悬停气泡（v1.0.92 已交付）

**问题**：账号池列过多/过宽，横向拥挤。

**改动**：**v1.0.92 已交付**（账号池 1:1 对齐波，任务 #3/#5）。列宽收敛，超长值（账号名/邮箱/ID 等）收进 `title` 悬停气泡；本版**无新增改动**，仅延续。

**验证**：
1. 账号池表格列宽收窄、无需横向滚动即可看全主要列。
2. 悬停账号名/邮箱等单元格：气泡显示完整值。

---

## ③ kiro 插件安装被拒（origins 上限 32 太小）→ 放宽到 64

**问题**：kiro 插件清单声明的 HTTPS origin 数量超过 32，安装时报「Wasm 网络地址过多」被拒。

**改动**：
- `src/plugin_host/wasm/manifest.rs`：origin 数量上限 **32 → 64**（`spec.origins.len() > 64` 才拒绝）。逐条校验不放松：仍必须精确 HTTPS origin、无通配符/路径/查询/凭据。

**验证**：
1. 安装 kiro 插件包（origins > 32 且 ≤ 64）：安装成功，网络访问正常。
2. origins > 64：仍被拒并报错。
3. 非法 origin（`http://`、通配符、带路径/查询）：仍被拒。

> 注意：`manifest.rs` 顶部文档注释（`:22`「网络来源：≤32 条」）仍是旧文案，属并发波文件域，建议随 wasm 波收口时一并更正（纯注释，非功能）。

---

## ④ 日志/审计看不到客户端 → 客户端识别对齐 A2A + 自身请求标 AIGX

**问题**：日志页看不到请求来自哪个客户端；AIGX 自身的内部请求（Playground/模型测试/渠道调试/AI 助手）客户端字段为空/unknown，排障时无法区分「外部客户端」与「站内自身请求」。

**改动**：
- `src/api/client_id.rs`：新增 `SELF_CLIENT = "AIGX"` + `self_identity()`；内置目录补 10 条（AIGX 自条目 + A2A 有而 AIGX 缺的 9 家：DeepSeek Harness / Magpie / Hermes / Hindsight / WorkBuddy / CodeBuddy / Crush / Factory Droid / Goose）；外部声明不得冒用 `AIGX`、自定义规则不得占用保留名 `AIGX`/`unknown`。
- 7 处自身请求标 AIGX：`src/agent/llm.rs`、`src/api/admin/playground.rs`(×2)、`src/api/admin/legacy.rs`(×2)、`src/api/admin/model_test.rs`（结构体字面量 `client_name=SELF_CLIENT`）、`src/api/admin/assistant.rs`（`attach(self_identity())`）。
- `src/log/mod.rs`：`list_with_filter_ext` 新增 **client 过滤**（大小写不敏感匹配 `client_name`/`client_version`）；**keyword** 搜索追加客户端两字段；CSV 导出补 `client_name,client_version` 两列（16→18 列）。
- `src/api/admin/logs.rs`：`RequestLogQuery` 新增 `client` 参数并接入筛选。
- `frontend/src/pages/Logs.tsx`：筛选行新增「按客户端过滤」输入框（Enter 即查询）。
- 对齐审计文档：`docs/CLIENT-ID-PARITY.md`。

**验证**：
1. 日志页客户端列：外部请求按 UA/声明显示规范名+版本；Playground/模型测试/渠道调试/AI 助手记录的客户端为 **AIGX**。
2. 日志页客户端筛选：输入 `AIGX` / `claude` 等 → 结果仅匹配对应客户端（含版本号匹配）；关键词框输入客户端名也能搜到。
3. CSV 导出：表头末尾含 `client_name,client_version`，行值与列表一致。
4. 新目录识别：构造含 `deepseek-harness` / `magpie-router` / `goose` 等 UA 的请求 → 客户端列显示对应规范名+版本。
5. 安全：外部请求带 `X-Client-Name: AIGX` → 不被识别为 AIGX（回落 UA/unknown）；管理面自定义规则把目标名设为 `AIGX`/`unknown` → 保存被拒。

---

## ⑤ 工单提交人只显示 UUID → 显示邮箱

**问题**：管理员「全部工单」列表的提交人列显示用户 UUID，难以辨认。

**改动**：
- `src/api/admin/tickets.rs`：新增 `ticket_json_with_email()`——按 `user_id` 从 user_store 解析邮箱注入 `email` 字段（仅管理员列表/详情视图）；用户已删时不下发该键，前端回退 UUID。
- 前端 `frontend/src/pages/Tickets.tsx` 既有渲染 `tk.email || tk.user_id`（本次链路因此闭合，无前端改动）。

**验证**：
1. 管理员 → 工单 → 全部工单：提交人列显示邮箱；搜索框输入邮箱可命中。
2. 对应用户被删除后：该列回退显示原 user_id（不报错）。

---

## ⑥ 「自动维护」整页空壳 → 7 个 handler 补 `{success,data}` 信封

**问题**：自动维护页任务概览显示「没有该任务的状态」、执行记录永远「暂无」、明细弹窗报「维护记录不存在或已清理」、维护设置恒禁用/保存恒报失败——整页看起来「没落地」。

**根因**：`/api/maintenance*` 4 个 handler + `/api/scheduled-tasks*` 3 个 handler 成功响应是**扁平 JSON**；前端（A2A 移植）一律读 `res.data`/`res.success` → 恒 undefined。后端执行逻辑本身为真执行（审计 `docs/MAINTENANCE-LANDING-AUDIT.md`）。

**改动**（方案 A：后端补信封，前端零改）：
- `src/api/admin/maintenance.rs`：overview / record_detail / run_task / retry_record —— 成功分支统一包 `{success:true,data:<payload>}`。
- `src/api/admin/scheduled_tasks.rs`：list / configure / run —— 同上。
- `src/api/admin/openapi.rs`：维护接口描述从「2 类任务」更正为 4 类（credentialMaintenance / modelRefresh / updateCheck / pluginCatalog）。
- `frontend/src/components/MaintenancePanel.tsx`：404 fallback 文案由「接口尚未就绪」改写为「无法读取维护记录（接口返回 404）」+ 升级重启提示（线上不触发，去掉误导）。

**验证**（一次修好三处）：
1. 自动维护页：4 个任务概览条正常显示（上次运行时间/状态），执行记录列表有数据，点「上次明细」正常弹窗，「立即执行」有成功 toast。
2. 维护设置弹窗可打开、保存成功（不再弹「部分设置可能已保存」）。
3. 各页工具栏「页面自动刷新」控件恢复渲染（`PageAutoRefreshControl` 同根因）。
4. 更新页「检查计划」区块恢复显示（`UpdatePanel` 同根因）。

---

## ⑦ 路由菜单 & 设置菜单对齐 A2A（全量重排 + 设置迁移）

**问题**：路由页/设置页的菜单结构、分组、文案与 A2A 不一致；设置里挂着 A2A 没有的网关面板与分类（含用户点名「去掉多余的网关菜单」）。

**改动（路由页，任务 #10 终态）**：
- `frontend/src/pages/Routing.tsx` + `frontend/src/pages/RoutingShell.css`：
  - 顶部模式分段 **路由配置 / 请求回放**（配置带未保存脏点）。
  - 左 rail 改 A2A 分组形态：**默认规则**（全局默认 / 错误处理）、**特殊请求路由**（压缩上下文 / 图片兼容 / 标题生成——文案对齐）、**模型路由**（每模型一项）、**自定义路由组**（每组一项）、footer「新增路由配置」。
  - 错误处理成为 rail 一级项，承载 `RoutingErrorsPanel`（处理规则/失败保护/安全边界）。
  - 回放模式改「左最近请求列表 + 右执行时间线（`RouteReplayView`）」，按 ID 回放保留为空态兜底。
  - AIGX 独有增强（预演 / 覆盖工作台 / Flow / Trace）按拍板**隐藏**（`SHOW_ROUTING_ENHANCED=false`，代码保留待后续清理）。
- 设置页 `frontend/src/pages/Settings.tsx`：删除 **流兼容（StreamCompatPanel）**、**自动续写（AutoContinuePanel）** 两块 A2A 无对应面板；删除 **通知** 分类（迁顶级页）；**系统信息 → 版本信息**改名。
- 顶级通知页：`frontend/src/App.tsx`（`/notify` 路由）、`frontend/src/components/Sidebar.tsx`（顶级「通知」项）、`frontend/src/pages/Notify.tsx`。
- 拍板落定与逐项状态见 `docs/ROUTING-MENU-PARITY.md` §7/§9。

**迁移完整口径（拍板）**：限流 → 顶级 `/limit`；计费 → `/products` 计费设置 Tab；通知 → 顶级 `/notify`（已落地）；AI 助手 → `/agent` 抽屉；聊天 → `/chat`；报表 → 仪表盘 Tab；重试/超时 → 路由「错误处理」；流兼容+自动续写删除（已落地）；系统信息→版本信息（已落地）；提供商不建设分叉（保持 `/channels` 顶级页）；账户/显示/反馈只建能落地的。
> 状态：`/limit`（`LimitPage.tsx` 已建）、计费 Tab、agent 抽屉、`/chat`、报表 Tab、retry/timeout 分类删除由任务 #11 收口；本页终态以 #12 复核为准。

**验证**：
1. 路由页：分段切换正常；rail 四组齐全（有模型/组时才出现对应分组）；错误处理面板三段可切换；回放左列表点选后右时间线加载。
2. 设置页：无「流兼容」「自动续写」「通知」分类/面板；「版本信息」分类显示系统信息面板；网关分类内无流兼容/自动续写两块。
3. 侧栏「通知」→ 独立 `/notify` 页（管理员可访问，非管理员不可见）。
4. #11 收口后补验：`/limit` 页、`/products` 计费 Tab、`/agent` 助手设置抽屉、`/chat` 设置、仪表盘报表 Tab、设置页无 retry/timeout 分类。

---

## ⑧ 账号池 provider 图标不全 → 补品牌 png + 首字母兜底

**问题**：账号池/供应商卡片部分 provider 无图标（只显示纯文字或空位），识别度低。

**改动**：
- `frontend/src/lib/providerIcons.ts`：`KIND_ICONS` 新增 9 个品牌映射（atomcode / chatglm / lobsterai / modelscope(+intl) / doubaowork / tabbit / astudio / quwork）；新增 `providerMonogram(kind)` 首字母圆标兜底——**任何 provider 都有图标**（有品牌 png 用图，没有则首字母）。
- `frontend/public/providers/` 新增 9 张品牌 png（astudio/atomcode/beichen/chatglm/doubaowork/lobsterai/modelscope/quwork/tabbit），来源对齐 A2A `ui/assets/providers/`。
- `frontend/src/pages/AccountSessions.tsx`（调用方）：`providerIcon()` 为 null 时渲染 `providerMonogram()`。

**验证**：
1. 账号池按家分组/供应商页：registry 62 个 provider kind 全部有图标；有品牌图的显示品牌 png。
2. 无品牌图的 kind 显示首字母圆标（大写首字符；空 kind 显示 `?`）。

---

## ⑨ 签到不覆盖停用账号 → 只排除「凭证过期」

**问题**：账号池里「停用（disabled）」的账号即使凭证有效也不参与签到（快照显示「账号已停用」排除），用户要求停用账号也能签到领积分/福利。

**改动**：
- `src/checkin/scheduler.rs`：`collect_targets` 去掉 `is_enabled()` 条件——参与条件仅剩「凭证未过期 + provider 有签到链路」。
- `src/checkin/center.rs`：`account_available` 改为只看凭证是否过期（停用不再判不可用）；`out_of_scope_reason` 移除「账号已停用」分支（保留：实现未进主仓 / 没有签到链路 / 凭证已过期）。快照与「立即全部签到」同源，口径一致。

**验证**：
1. 一个 **停用但凭证有效** 的账号：出现在签到中心账号列表，`available` 为真，参与「立即全部签到」/定时签到并获得奖励。
2. **凭证过期**账号：仍显示不可用、被排除（`凭证已过期`）。
3. 无签到链路的 provider / 插件实现未进主仓：仍按原口径排除。

---

## 附：门禁与证据文档

- 本版全部改动未提交，留工作区；docs 证据：`docs/CLIENT-ID-PARITY.md`（④）、`docs/MAINTENANCE-LANDING-AUDIT.md`（⑥）、`docs/ROUTING-MENU-PARITY.md`（⑦）。
- 本地门禁（实施波自跑口径，终态以 #12 全量复核为准）：`cargo fmt --check`（本波文件干净；残留仅在并发的 `plugin_host/wasm/*`、`scheduled_tasks.rs`）、`cargo clippy --bin aigx`（0 warning）、`cargo test --bin aigx`（客户端识别波：2497 passed / 1 failed，唯一失败为并发 wasm 波的 `contract_advertises_abi_and_capabilities`，非本版改动）、前端 `tsc`（仅 `Routing.tsx` 并发波重复标识符、非本版）、`eslint`、`i18n:check`（0 缺失）。

**验证状态：⏳ 待生产部署（104）后回填真机结果。**
