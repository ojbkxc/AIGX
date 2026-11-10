# 1.0.44 规格：账号池全面对齐 Agent2API + 表格密度统一 + 定价列内联

> 唯一事实源。所有成员按本文件开发，偏离必须在此文件「偏离记录」小节登记。
> 上游参考：`D:\GitHub\Agent2API\desktop-tauri\ui-islands\src\islands\`（v3.0.8）。

## §0 已完成（lead，勿重复）

**后端（Rust，已本地 `cargo check --tests` + 6 单测通过，fmt 干净）**
- `AccountSessionPatch` 新增 `proxy: Option<String>`（`src/api/admin/account_sessions.rs:203-228`）：
  三态 = 缺省保持 / 空串清空 / 非空设置。赋值前调 `proxy_pool::resolve_proxy(Some(trimmed), "")`
  校验，解析失败返回 400（引用不存在的池条目在写入口就拒绝）。
- `session_json` 新增两个字段（同文件 `:640-670` 附近）：
  - `"proxy_state": { "mode": "direct"|"url"|"pool"|"rotate", "ok": bool, "error": string|null }`
  - `"limiter_state": { "account_skip": bool, "skip_models": string[], "disable": bool }`
- `mask_proxy` 修正：`pool:` / `rotate` 引用形态**原样透出**（URL 形态才打码）。

**前端（lead）**
- `pages/Channels.tsx` 失效文案修正（「账号池型供应商请到「账号池」Tab 添加账号时自动创建，这里只建 API Key 型」），zh/en i18n 同步。

---

## §1 账号池列表：视觉对齐供应商页

目标：行高 / 字号 / 表头 sticky / 单元格内边距与 `pages/Channels.css:25-64`（`.channels-table`）一致。

- `.as-table` 由「继承全局 14px / 12px16px padding」改为**显式**：
  - `font-size: 12.5px`
  - `th`: `font-size: 10.5px; padding: 7px 10px; position: sticky; top: 0; z-index: 2;`
    `text-transform: uppercase; letter-spacing: .05em; color: var(--text-muted); white-space: nowrap;`
  - `td`: `padding: 7px 10px;`
- 表头 sticky 需要滚动容器：给 `.as-scroll` 加 `max-height: calc(100vh - 250px); overflow: auto;`
  （与 `ModelManage.css:289-293` `.mm-table-wrap` 同值）。**注意**：账号页表格上方有工具栏 +
  筛选条，`250px` 是模型管理页实测值，如实际遮挡则调整为 `calc(100vh - 300px)` 并在偏离记录登记。
- **修复已有 bug**：列设置里的「对齐」从未生效（`grep "ta-" AccountSessions.tsx` = 0 命中）。
  `th` 与 `td` 都要加 `ta-${colAlign[key]}`，对齐值从 `appliedCols.find(c => c.key === key)?.align` 取。
- 保留现有全部列与交互，不改列集合（新增列见 §2.9 / §2.10）。

## §2 补齐 Agent2API 缺口（14 项）

### §2.1 分页页脚
- 复用公共件：`components/ui/TableShell.tsx` 的 `usePageSize` + `TablePaginationFooter`
  （参考 `pages/ModelManage.tsx:45` 的 import 与用法；`TablePaginationFooter` 接口见 `TableShell.tsx:227-301`）。
- localStorage 键：`aigx-tbl-pagesize.accounts`（与既有 `aigx-tbl-pagesize.<page>` 命名一致）。
- 分页作用于**筛选+排序后的结果**（`visibleSorted`），分组模式下按「组内行数累计」翻页：
  每组最多在页内出现其完整的行（组带随该组第一行出现；跨页的组重复组带）。**若实现成本过高，
  可降级为「分组模式关闭分页」并在偏离记录登记**（上游也只对平铺列表分页）。
- 默认每页条数档位与模型管理页一致（见 `TableShell.tsx:61-95` 的档位表）。

### §2.2 折叠全部 / 展开全部
- 工具栏加两个小按钮（`ChevronsDownUp` / `ChevronsUpDown` 图标，lucide-react）。
- 仅在 `grouped === true` 时渲染。
- 「折叠全部」把所有 provider 写入 `collapsedGroups`；「展开全部」清空。两者都落 `LS_COLLAPSED`。
- 对齐上游 `accounts-page.tsx:437-442`。

### §2.3 清除筛选
- 一个按钮，重置：`providerFilter=''`、`search=''`、`statusSeg='all'`、`limitSeg='all'`（新增，见 §2.4）。
- **不**重置：分组开关、排序、自动余额间隔、列设置（那些是偏好不是筛选）。
- 按钮仅在有任一筛选生效时可用（disabled 否则）。
- 对齐上游 `accounts-page.tsx:482-486`。

### §2.4 限额筛选（全部 / 正常 / 已限流）+ 双级计数
- 数据源：**后端新字段 `limiter_state`**（§0），**禁止**前端按 `limiters` 自己重算阈值。
- 判定（对齐 Agent2API `accounts-domain.ts:1002-1010`）：
  ```
  isLimitedNow(s) =
      有未到期冷却（复用现有 isCooling(s)）
    || s.limiter_state?.account_skip === true
    || (s.limiter_state?.disable === true && s.status !== 'enabled')
  ```
- 「正常」= `status === 'enabled'` 且 `!isLimitedNow`。
- 「已限流」= `status === 'enabled'` 且 `isLimitedNow`（**禁用账号既不算正常也不算已限流**，
  对齐 `accounts-domain.ts:1040-1050`）。
- 计数按 Agent2API 的**双级 scope** 算（`accounts-domain.ts:1085-1107`）：
  - 状态分段三档的计数，scope = 应用「provider + limit」筛选后的集合（不含自身状态维度）；
  - 限额分段三档的计数，scope = 应用「provider + 状态」筛选后的集合（不含自身限额维度）。
  - 「全部」档计数 = 该 scope 的总数。
- 分段 UI 复用 `.seg-filter` / `.seg-filter-btn`（已有）。

### §2.5 筛选偏好持久化
- 落盘：`provider`、`statusSeg`、`limitSeg`（**不落** `search`，对齐上游）。
- 键：`aigx-as-filter`，值为 JSON `{provider, status, limit}`。
- 启动读取时做**失效自愈**（对齐 `accounts-store.ts:273-279`）：若存的 provider 在当前
  `sessions` 里已不存在，静默重置为 `''`。

### §2.6 分组带「第 N 顺位」+ 拖组排序
- 组带上显示 `第 {{n}} 顺位`（组间按该组最高 priority 降序，与现有分组顺序一致）。
- 组带可拖拽（`draggable`），拖到另一组带上 → 整组重排。
- 落盘方式**沿用现有行拖拽同一套**：把所有组内账号按新组序重新赋 `priority`
  （队首最大，递减 10，与 `AccountSessions.tsx:1588-1620` 的 `handleDropRow` 完全一致），
  逐条 `PATCH { priority }`，用 `Promise.allSettled` 并统计成功数。
- 组带拖拽期间**禁用**行拖拽（`draggable={!grouped || groupDragActive}` 之类的判定要写清楚）。
- 对齐上游 `accounts-page.tsx:276-294` + `accounts-panels.tsx:166-168`。

### §2.7 组内拖拽把手
- 现有「优先级」列有 `#N` 编号（`:1734-1749`）但**无把手**。加 `⠿` 把手图标
  （`GripVertical`，lucide-react）作为 `draggable` 的视觉锚点，`title` 提示「拖动调整组内顺序」。
- 现有整行 `draggable={!grouped}` 的语义**保持不变**（分组模式仍禁行拖，改由组带拖动承担）。

### §2.8 账号详情气泡
- 账号列的名称悬停（`title` 或自绘气泡）显示：邮箱/标识、备注、更新于、来源（`provider`）、
  桌面登录态（`if (s.provider === 'desktop' || s.relogin_due_at != null) 显示「桌面端实时登录态」`）。
- 现有 `s.api_key` / `s.proxy` / `s.last_error` 三行 `form-hint` 保留（它们是"列内可见"的信息，
  不复述到气泡里）。
- 对齐 `accounts-panels.tsx:1225-1283`。

### §2.9 余额明细弹层 + 跳过徽章 + Token 限额第二行
- **明细弹层**：点击余额格 → 弹层列出 `s.last_balance.wallets[]` 每一桶
  `{displayName, balance, total?, used?}`，以及 `windows[]` 每窗口（`label`、`remaining_percent`、
  `remaining`、`resets_at`）。空态「上游未返回明细」。
- **跳过徽章**：
  - `余额不足·已跳过` —— `s.limiter_state.account_skip === true` 且
    `s.last_balance.available != null` 且存在 balance 类 skip 规则命中（用 `limiter_state` 判定，
    **不要**自己按 `limiters` 算阈值；若要细分到「是余额还是 Token 触发的」，用
    `s.limiter_state.skip_models.length === 0 && account_skip` 近似为整号命中，并在偏离记录登记）。
  - `Token 限额·已跳过` —— `s.limiter_state.skip_models.length > 0`。
- **Token 限额第二行**：余额格下方的 `form-hint` 追加一行「Token 限额 N 个模型已跳过」，
  `title` 列出模型名（`skip_models.join(', ')`）。
- 对齐 `accounts-panels.tsx:987-1105,1120-1188`。

### §2.10 状态列健康标签
- 在现有状态徽章旁追加标签（有则该标签，无则不占位）：
  - `未配置凭证` —— 凭证为空（`s.api_key` 为空串或 `'—'`；实现时请读 `AccountSessionItem` 的
    实际脱敏形态确认，见 `types/index.ts:1308-1353`）。
  - `代理异常` —— `s.proxy_state?.ok === false`，`title` 显示 `proxy_state.error`。
  - `Token 限额已禁用` —— `s.status !== 'enabled'` 且 `s.limiter_state?.disable === true`。
- 对齐 `accounts-panels.tsx:204-233`。
- **前提**：`AccountSessionItem` 类型（`types/index.ts:1308`）需补三个字段（见 §4）。

### §2.11 行内代理下拉「选中即存」
- 新增列 `proxy`（`AsCol` 加 `'proxy'`，`AS_COLS` 加 `{ key:'proxy', label:'代理', w:132 }`）。
- 单元格：`<select>`，选项 = `直连` + 「网络代理」页的全部条目（`api.proxyPoolList()`，
  `api/index.ts:797`）+ `轮询全部启用`（`rotate`）+ 当前值（若不在选项里则补一条「自定义：<原值>」）。
- 选中 → `api.patchAccountSession(s.id, { proxy: v })`（v 为空串 = 直连）。成功后局部更新
  `sessions` 里该条的 `proxy` 与 `proxy_state`（用后端返回的 `data` 回填，不要自己猜）。
- 失败 → `addToast(..., 'error')` + 回退选中值。
- 下拉旁小圆点：`proxy_state.ok ? 绿 : 红`，红时 `title=proxy_state.error`。
- 对齐 `accounts-panels.tsx:1304-1377`。

### §2.12 行内「领套餐」
- 操作列加按钮，**仅**当 `checkin 支持该 provider` 时出现。
- 判定来源：**前端需要一份 provider 白名单**。后端权威在
  `src/checkin/claim.rs:81 supports_checkin`（`CHECKIN_PROVIDERS` 常量，同文件更上方）。
  请把该常量列表复制为前端常量（`lib/checkinProviders.ts`），并在注释里写明「与后端
  `src/checkin/claim.rs` 的 `CHECKIN_PROVIDERS` 同步维护」。
- 点击 → `POST /api/checkin-center/run` body `{ account_ids: [s.id] }`，成功后 toast
  显示返回的 `results[0]` 消息，然后 `load()` 刷新。
- 对齐 `accounts-panels.tsx:1400-1410`。

### §2.13 行内「设为首选」+ 并发上限快捷
- 现操作列已有「刷新 token」「删除」两个独立按钮，新增：
  - **设为首选**：`PATCH { priority: <全池最大值 + 10> }`（与 `handleDropRow` 的队首口径一致）。
  - **并发上限快捷**：一个小的数字输入/步进（`PATCH { max_concurrency }`），或收进「⋯」菜单。
- **建议**（不强制）：把「刷新 / 设为首选 / 并发 / 删除」收进一个 `⋯` 聚合菜单，
  对齐上游 `accounts-panels.tsx:1440-1484`，以减少操作列宽度。若要收进聚合菜单，
  请确保「删除」仍需 `ConfirmDialog` 二次确认（现有行为）。
- 对齐 `accounts-panels.tsx:1440-1484`。

### §2.14 provider 下拉带每家账号数
- 下拉选项文案改为 `{label} ({n})`，`n` = 该 provider 在当前 `sessions` 里的账号数。
- 不影响筛选值（`value` 仍是 `kind`）。
- 对齐 `accounts-page.tsx:162-163`。

---

## §3 其余表格页紧凑化 + 侧栏收窄

### §3.1 侧边栏
- `App.css:172`：`--sidebar-width: 240px` → **`160px`**。
- 给 `.nav-item span`（`App.css:1416-1429` 附近）补 `overflow: hidden; text-overflow: ellipsis;`
  （`.nav-item` 本身已有 `white-space: nowrap`；`title` 属性已在 `Sidebar.tsx:215` 提供完整文案）。
- **不要**动 `Agent.css:17-19` 的 `.agent-drawer:240px` 与 `App.css:908` 的 `.toast min-width:240px`
  （与全局侧栏无关）。
- 折叠态 `--sidebar-width-collapsed: 48px` 保持不变。

### §3.2 表格页紧凑化
统一到供应商页规范（12.5px 字号 / th 10.5px / padding 7px 10px / th sticky）。

**必须改（当前用全局 14px 表格）**：
- `pages/Logs.tsx`（无独立 CSS，`table-wrapper` 用的全局 `table` 样式）
- `pages/Redemptions.tsx`（无独立 CSS）
- `pages/Wallet.tsx`（无独立 CSS）
- `pages/NetworkLayer.tsx`（有 CSS 但未用 12.5px）

**做法**（推荐，避免给每个页面复制一份 CSS）：在 `styles/density.css` 已有的 `.dense-table`
基础上补齐 sticky 表头与 hover，然后给上述四个页面的 `<table>` 加 `className="dense-table"`
（若已有 className 则追加）。`density.css` 已被 `main.tsx:6-7` 全局引入。
- 在 `.dense-table` 补：`th { position: sticky; top: 0; z-index: 2; background: var(--table-header-bg); }`
  与 `td { border-bottom: 1px solid var(--border-color); }`（与全局表一致的下划线，避免"无框感"）。
- `.dense-table` 的表头字号/内边距已由 token 驱动（`--font-size-xs` / `--pad-1` / `--pad-2`），
  若 token 值与 12.5px/7px10px 不等，请**直接改 `App.css` 的 token 定义**（它们是全站密度基线）
  并在偏离记录登记影响面；否则保持 token 不动、在 `.dense-table` 里写死数值。
- 滚动容器：给这些页的 `.table-wrapper` 加 `max-height`（同 §1 的 `calc(100vh - 250px)`）或
  在 `<div className="table-wrapper table-wrapper-scroll">` 用新类；选一种并保持一致。

**不要改**：已经用了 12.5px 的页面（Channels / ModelManage / CheckinCenter / Diagnostics /
Groups / IpManagement / Keys / Routing / Security / Tickets / Users）——它们已经是目标形态，
只需抽查确认没有回退。

---

## §4 类型定义（`types/index.ts`）

`AccountSessionItem`（`:1308-1360`）新增：
```ts
/** 出口代理当前可用性（后端 proxy_state_of；列表行内下拉的状态点） */
proxy_state?: {
  mode: 'direct' | 'url' | 'pool' | 'rotate';
  ok: boolean;
  error?: string | null;
};
/** 限制器当前拦截快照（后端 limiter_state_of；「限额」筛选与状态徽章的唯一判据） */
limiter_state?: {
  account_skip: boolean;
  skip_models: string[];
  disable: boolean;
};
```

---

## §5 定价列内联弹窗（`pages/ModelManage.tsx`）

- 定价 `td`（`:1427-1450`）改为可点击：
  - `role="button"`、`tabIndex={0}`、`onClick={() => openPrice(row)}`、
    `onKeyDown`（Enter / Space → `openPrice(row)`，并 `e.preventDefault()`）。
  - `title` 改为「点击编辑定价」+ 原有单位说明。
  - 未定价（`.mm-price-empty`）同样可点（空表单预填）。
- CSS（`ModelManage.css` `.mm-c-price` 附近）加：
  ```css
  .mm-c-price { cursor: pointer; }
  .mm-c-price:hover { background: var(--hover-bg); }
  .mm-c-price:focus-visible { outline: 2px solid var(--accent-color); outline-offset: -2px; }
  ```
- **保留**操作列的「定价」按钮（`:1626-1633`）不动。
- e2e：点定价列 → 弹窗标题「编辑定价」出现（见 §7）。

---

## §6 i18n

- 所有新增 UI 文案必须走 `t()`，并在 `i18n/zh.json` + `i18n/en.json` **同时**补 key。
- 收尾跑：`npm run i18n:check`（必须「缺失 0 / 集合完全一致」）与
  `node scripts/i18n-interpolation-check.mjs`（必须 PASS）。
- 新增术语沿用 1.0.43 定稿：**供应商**（不是渠道）、**供应商调试**（不是渠道调试）。

## §7 e2e（`frontend/e2e/`）

新增 `account-pool.spec.ts`（或扩展现有 spec），至少覆盖：
1. 账号池表格表头 sticky + 12.5px 字号（断言 computed style）。
2. 列设置里把某列改为「右对齐」→ 该列 `th`/`td` 真的出现 `ta-right`（修复 §1 的对齐 bug）。
3. 限额分段：点「已限流」→ 只剩满足 `limiter_state` 的行（可用 route mock 造数据）。
4. 折叠全部 / 展开全部按钮在分组模式下可见且生效。
5. 行内代理下拉改变时发出 `PATCH`（route mock 断言请求体含 `proxy`）。
6. 定价列点击 → 「编辑定价」弹窗出现（§5）。
7. 侧边栏宽度 = 160px（断言 computed `width`）。

## §8 验收门禁

- `npm run typecheck` 0 错
- `npm run lint` 0 警告
- `npm run test`（vitest）全绿
- `npm run i18n:check` + `i18n-interpolation-check` 通过
- `E2E_PORT=4174 npx playwright test --trace=off` 全绿（含既有 340 例）
- `cd frontend && npm run build`，重建后 `git status static/` 零漂移（CI 有门禁）
- Rust 侧：`cargo fmt --all -- --check` 通过（本地已验），编译交 CI

## §9 偏离记录

（成员实施时的任何偏离写在这里，格式：`- [成员] 偏离内容 —— 原因 —— 影响面`）

- [table-fe] 密度 token 直接改 `App.css` 定义（`--font-size-sm` 12→12.5px / `--font-size-xs` 11→10.5px / `--pad-1` 4→7px / `--pad-2` 8→10px），未在 `.dense-table` 写死数值 —— 按 SPEC §3.2「token 值与 12.5px/7px10px 不等则直接改 token」；经全量 grep 这 4 个 token **仅**被 `styles/density.css` 消费（`.dense-table` / `.surface-titlebar` / `.surface-meta`），后者全站零采用 —— 影响面：仅本节的 `.dense-table`（Logs/Redemptions/Wallet/NetworkLayer 4 页）；全站其它页面零影响。
- [table-fe] `.nav-item span` 在 SPEC 要求的 `overflow:hidden / text-overflow:ellipsis` 之外**额外加了 `min-width:0`** —— flex 子项默认 `min-width:auto` 会阻止收缩，不加则长菜单名的 ellipsis 不生效 —— 影响面：仅侧栏菜单名截断行为，符合 §3.1 意图。
- [table-fe] 滚动容器用**新类 `.table-wrapper-scroll`**，并以 `@media (min-width:768px)` 限定仅桌面生效 —— SPEC §3.2 允许「用新类」；移动端 ≤767px 走 `.table-wrapper` 的卡片化布局，若 `max-height` 无条件生效会把卡片套进定高滚动区 —— 影响面：仅 4 页的桌面视图；移动端不变。NetworkLayer 原用 `overflow-x-auto`（非 `.table-wrapper`），在该 div 上追加 `table-wrapper-scroll`。
- [e2e-fe] §7-5 行内代理列定位改用「表头文案 → 列索引」（`thead th` 含「代理」求序）而非 `td.as-c-proxy` —— §2.11 只钉了列 key/label，未钉单元格 class；按 `as-c-<col>` 约定本应渲染 `as-c-proxy`，但为避免实现命名漂移让用例假红，改为运行时按表头文案定位 —— 影响面：仅本 spec 定位方式；若「代理」列被默认隐藏或表头文案变更需同步。
- [e2e-fe] §7-4 折叠/展开全部按钮以可访问名（`getByRole('button', { name })`）定位，SPEC 只给了图标（ChevronsDownUp/ChevronsUpDown）未给文案 —— 需要实现给按钮补 `title`/`aria-label` 或文案，否则用例红。
- [e2e-fe] e2e 目录不在 TypeScript 覆盖范围（`tsconfig.json` 的 `include` 仅 `src/**/*`），项目 `npm run typecheck`/§8 的 typecheck 门禁对 e2e **无效** —— 本次自验改用 `npx tsc --noEmit --strict --skipLibCheck --module esnext --moduleResolution bundler --target ES2020 --jsx react-jsx --esModuleInterop e2e/account-pool.spec.ts`（退出 0）＋ `npx eslint e2e/account-pool.spec.ts`（退出 0）—— 影响面：e2e 类型错误不会在门禁暴露，建议后续把 e2e 纳入独立 tsconfig。- [acct-fe] §2.1 分页档位采用 TableShell 的 `CLIENT_PAGE_SIZES`（20/50/100/200/全部），默认「全部」 —— SPEC 同段指向 `TableShell.tsx:61-95` 的公共件档位表，且账号池原本就是一屏全量，默认「全部」避免升级后行突然消失 —— 影响面：仅账号池的每页档位/默认值，不影响其它页。
- [acct-fe] §2.9「余额不足·已跳过」细分采用 `limiter_state.skip_models.length === 0 && account_skip` 近似为整号命中（余额类） —— SPEC 允许并点名的降级方案（前端不按 limiters 重算阈值） —— 影响面：若账号同时命中余额规则与 Token 规则且 Token 规则未产生模型清单，可能显示为「余额不足·已跳过」。
- [acct-fe] §2.8 账号气泡未含「邮箱/标识、备注」两行 —— `AccountSessionItem` 无独立 email/remark 字段（本页数据只有 name/provider/updated_at/relogin_due_at） —— 影响面：气泡信息量少于上游，仅展示「账号名 / 更新于 / 来源 / 桌面端实时登录态」。
- [acct-fe] §2.11 代理池读取用实际 API 名 `api.listProxyPool()` —— SPEC 写作 `api.proxyPoolList()`，`api/index.ts:796-797` 的实现名是 listProxyPool —— 影响面：无。
- [acct-fe] `types/index.ts` 的 `AccountSessionItem` 额外补 `relogin_due_at?: number | null`（§4 之外） —— §2.8「桌面端实时登录态」判据需要，后端 `session_json` 已下发同名字段 —— 影响面：纯类型声明。
- [acct-fe] §2.6 组带拖拽按「每组全量账号（含被筛选隐藏）」重排 priority，而非仅可见行 —— 筛选态下只重排可见行会与隐藏账号的 priority 撞车、破坏全局调度序 —— 影响面：隐藏账号的 priority 也随组序整体重排（组内相对顺序保持不变）。
- [acct-fe] §2.13 操作列把「刷新 Token / 设为首选 / 并发上限 / 删除」收进 ⋯ 聚合菜单 —— SPEC 建议（不强制）以压缩操作列宽度；「删除」仍走既有 ConfirmDialog 二次确认 —— 影响面：既有按钮位置变化，功能不变。
