# AIGX UI 审美迁移方案

> 第一阶段提案文档 · 2026-09-07
> 审美基调 = ChatGPT / DeepSeek 的简约范式（「气」）+ open-webui 的色彩与控件体系（「器」）。
> 验收直觉：拿不准时自问「ChatGPT 会这么做吗？open-webui 的控件会长这样吗？」两个都是否定，就该重做。

## 一、迁移总原则

1. **玻璃拟态收敛**：保留用于品牌层（侧边栏、顶栏、模态框、登录页）；内容区转向干净平面（open-webui 式）。
2. **新页面默认平面**：新建页面一律用平面色板；旧页面按优先级渐进迁移。
3. **CSS 变量先行**：`App.css` 新增 `--surface-flat` 语义色板（平面背景/边框/文本三档灰阶 + 焦点环变量），旧页面不动也能与新页面共存。
4. **五态控件标准**：default/hover/active/focus-visible/disabled 五态齐全；动效统一 120–200ms ease-out；触控目标 ≥44px；焦点环可见。
5. **三档字阶**：标题/正文/辅助文字三档字号 + 灰阶，克制使用色彩。

## 二、页面改造优先级（21 个路由页 + 组件库，共 34 项）

### P0 · 本轮必改（用户侧核心体验，共 7 项）

| # | 对象 | 现状 | 改造动作 |
|---|---|---|---|
| 1 | `/chat`（新建） | 不存在 | 新建三段式聊天工作区：会话侧栏 + 居中消息流 + 底部输入框。纯平面风。 |
| 2 | Playground.tsx | 23 行薄壳复用 ChatDebugger | 升级为三模式（Chat/Completions/Images）+ 参数调节 + 双栏 JSON。内容区平面化。 |
| 3 | Login.tsx | 玻璃拟态 | 保留玻璃品牌层（登录页属品牌层），微调：控件五态 + 主题切换入口。 |
| 4 | Register.tsx | 玻璃拟态 | 同上。 |
| 5 | ChatDebugger.tsx | 调试器形态 | 抽离消息渲染为 MessageBubble/ReasoningFold（DeepSeek 式折叠），供 /chat 与 Playground 共用。 |
| 6 | Sidebar.tsx | 玻璃拟态 | 保留玻璃（品牌层）；加统一断点与 focus-visible。 |
| 7 | 组件库 `ui/` + `glass/`（21 组件） | 18 ui + 3 glass，手感参差 | 五态补齐、焦点环、动效 120–200ms、aria、≥44px 触控目标。新建 Drawer/Dropdown 桌面版。 |

### P1 · 高优先级（高频管理面，共 15 项）

| # | 对象 | 现状 | 改造动作 |
|---|---|---|---|
| 8 | Settings.tsx（672 行） | 单页混杂 | 拆为五分区 Settings V2（通用/界面/通知/账户/用量），左栏导航 + 右栏内容。 |
| 9 | Dashboard.tsx（763 行） | recharts 图表 + 玻璃卡片 | 内容区平面化 + 新增「缓存节省」与「渠道实时状态」卡片。 |
| 10 | Keys.tsx | 数据表 | 表格平面化 + 触控目标修正 + 空状态。 |
| 11 | Channels.tsx | 渠道管理 | 渠道行增加健康档案展开（C1），测试按钮五态。 |
| 12 | Pricing.tsx | 定价管理 | 成本模拟器（B2）面板 + 表格平面化。 |
| 13 | Users.tsx | 用户管理 | 表格平面化。 |
| 14 | Groups.tsx | 分组 | 平面化。 |
| 15 | Mappings.tsx | 模型映射 | 平面化。 |
| 16 | Logs.tsx | 日志 | 请求日志增加「调度决策回放」展开（C2）。 |
| 17 | Notify.tsx | 通知配置 | 平面化 + 表单五态。 |
| 18 | Security.tsx | 安全事件 | 新增 TOTP/会话撤销区块（P1 后端完成后）。 |
| 19 | Orders.tsx | 订单 | 平面化。 |
| 20 | Redemptions.tsx | 兑换码 | 平面化。 |
| 21 | Wallet.tsx / Profile.tsx | 用户侧 | 新增用户侧「用量」视图（D2）。 |
| 22 | NetworkLayer.tsx | 网络层 | 升级为「调度实时地图」（A2）：延迟分布/熔断状态/AIMD/failover 次数。 |

### P2 · 低优先级（共 12 项）

| # | 对象 | 现状 | 改造动作 |
|---|---|---|---|
| 23 | Epay.tsx | 支付回跳页 | 保持（品牌层），五态补齐。 |
| 24 | IpManagement.tsx | IP 过滤 | 平面化。 |
| 25 | 新建 Artifacts 画布 | 无 | 侧边实时预览画布 + 轻量代码编辑器。 |
| 26 | 新建 GraphQL 面板 | 无 | 只读 GraphQL 查询面板（D1）。 |
| 27 | 新建 Models/Prompts workspace | 无 | 模型预设 + 提示词库。 |
| 28 | GlobalSearch | Ctrl+K 面板 | 五态/aria 对齐。 |
| 29 | SystemMonitorPanel | 监控面板 | 平面化。 |
| 30 | Toast | 通知 | 动效统一 160ms ease-out。 |
| 31 | ConfirmDialog | 确认框 | aria-modal + Esc 关闭。 |
| 32 | MobileDrawer | 移动抽屉 | 与桌面 Drawer 合并为同一组件双断点。 |
| 33 | EmptyState / Loading / Skeleton | 辅助组件 | 统一占位节奏。 |
| 34 | 全局字体与色板 | 系统字体栈已用 | 引入 `--surface-flat` 色板与主题色变量，统一三档字阶 token。 |

## 三、色彩与控件对齐表（open-webui 参照，原创落地）

| 维度 | open-webui | AIGX 落地 |
|---|---|---|
| 主色 | 中性灰阶 + 蓝色强调 | 保留现有 accent 蓝，新增主题色变量（预置 6–8 色，system 三态） |
| 背景 | 平面分层（bg/base/surface） | `--surface-flat-bg` / `--surface-flat-card` / `--surface-flat-hover` 三档 |
| 边框 | 1px 低对比 | `--surface-flat-border`，hover 才提亮 |
| 焦点环 | 可见 focus ring | `--focus-ring`（2px，accent 色，focus-visible 专用） |
| 动效 | 120–200ms ease-out | 全局 token `--motion-fast: 140ms` / `--motion-base: 180ms`，统一替换 250–300ms 过渡 |
| 圆角 | 大圆角卡 | 保持 AIGX 现有 `rounded-card/control` token，不照搬 |
| 触控 | ≥44px | 交互控件统一 `min-h-11` 语义，视觉可保持紧凑 |

## 四、改造顺序与验收

- 每轮 UI 改造只动一个「视觉域」（如「表格平面化」批次涵盖 Keys/Users/Groups/Mappings 四页），爆炸半径最小，避免并行合并冲突。
- 验收：每个批次改完后执行 `npm run typecheck` + `npm run build` + 手动走查五态（default/hover/active/focus/disabled）+ Tab 键盘路径 + 移动端 400px 断点不溢出。
- 违反者返工标准：发现新的页面用了全局毛玻璃做内容区、动效超过 200ms、按钮无 focus-visible 态、触控目标 <44px，均视为不合格。
