# AIGX 前端演进路线（参照 open-webui 九大范式）

> 第一阶段提案文档 · 2026-09-07 · 参照项目（只读）：`D:\GitHub\rustAIGX\open-webui-main\open-webui-main`
> 原则：open-webui 是 Svelte，本路线只学交互与布局思想，用 React 18 + TypeScript 重写；UI 文案、图标、品牌元素一律不照搬。
> 决策符号：**引入** / **改造引入** / **暂缓**（需先补后端）/ **不引入**（保持 AIGX 玻璃拟态特色）。

## 一、ChatGPT 式主界面（chat/Chat.svelte）

| 范式 | open-webui | AIGX 现状 | 决策 | 依赖后端项 |
|---|---|---|---|---|
| 侧边栏会话树（文件夹/置顶/搜索/归档） | 完整 | 无用户聊天中心；Playground 是复用 ChatDebugger 的调试器（`pages/Playground.tsx` 仅 23 行薄壳） | **引入**：新建 `/chat` 用户侧聊天工作区 | P1：会话后端持久化（当前可先用 localStorage，接口保持可替换） |
| 居中消息流 | 完整 | 无 | **引入**：居中窄栏 + 大量留白，这是 ChatGPT/DeepSeek 简约范式的核心 | 无（纯前端） |
| 消息气泡/角色头像/操作悬浮条 | 完整 | ChatDebugger 有基础消息渲染 | **引入**：抽离成 `MessageBubble` 组件，悬浮操作条（复制/重试/删除） | 无 |
| DeepSeek 式 reasoning 折叠 | open-webui 无（AIGX 原创超越点） | ChatDebugger 无 | **引入**：reasoning 内容默认收起、可展开 | 无（解析上游 reasoning_content 字段） |

## 二、输入框工程（MessageInput/）

| 范式 | open-webui | AIGX 现状 | 决策 | 依赖后端项 |
|---|---|---|---|---|
| 斜杠命令建议列表 | 有 | 无 | **暂缓**：P2（需要命令体系设计，避免为炫技而加） | 无 |
| @提及模型/知识库 | 有 | 无 | **引入**（P1）：@提及模型切换，复用现有模型映射；@知识库等 RAG 完成后再做 | 模型列表已有（`/v1/models`） |
| 语音输入浮层 | 有 | 无 | **暂缓**：依赖浏览器 Web Speech，中文环境收益低，P2 再评估 | 无 |
| 附件覆盖层 | 有 | ChatDebugger 支持多模态附件 | **引入**：抽离为 `AttachmentOverlay`，支持拖拽 | 无 |
| 排队消息展示 | 有 | 无 | **暂缓**：P2（长任务框架之后才有真实排队语义） | P2：任务通道 |
| 输入变量模板 | 有 | 无 | **暂缓**：受众窄 | 无 |

## 三、消息渲染（chat/Messages/）

| 范式 | open-webui | AIGX 现状 | 决策 | 依赖后端项 |
|---|---|---|---|---|
| Markdown + 代码块（复制/语言标签） | 有 | ChatDebugger 部分支持 | **引入**：补代码块复制按钮与语言标签（已有纯函数可测试化） | 无 |
| Citations 引用溯源 | 有 | 无 | **暂缓**：依赖 RAG/知识库，P2 再议 | P2：RAG |
| Artifacts 侧边实时预览画布 | 有 | 无 | **引入**（P2）：这是 Playground 质的升级方向——代码/HTML/图表产物在侧边画布实时渲染 | 无（纯前端） |
| 结构化问答回复卡片（AskUserCard） | 有 | 无 | **暂缓**：Agent 化能力，P2+ 再议 | 无 |

## 四、会话治理

| 范式 | open-webui | AIGX 现状 | 决策 | 依赖后端项 |
|---|---|---|---|---|
| 文件夹/标签 | 有 | 无 | **引入**（P1）：localStorage 版本先行，后端持久化时迁移 | P1：会话持久化 |
| 分享链接弹窗 | 有 | 无 | **暂缓**：分享语义涉及后端（只读快照），P2 | P2 |
| Overview Flow 会话流程图 | 有 | 无 | **暂缓**：炫技优先度低 | 无 |
| 归档与恢复 | 有 | 无 | **引入**（P1）：简单软删除语义 | 无 |

## 五、设置信息架构（chat/Settings/）

| 范式 | open-webui | AIGX 现状 | 决策 | 依赖后端项 |
|---|---|---|---|---|
| 分区式设置 | General/Interface/Personalization/Audio/Notifications/Account/Privacy/DataControls/Shortcuts/Connections/Tools/Usage 12 分区 | `Settings.tsx` 672 行单页 | **引入**：拆成分区式 Settings V2（左栏导航 + 右栏内容），是信息架构升级而非功能新增 | 无 |
| 分区范围 | — | — | AIGX 保留 5 个分区即可：通用/界面/通知/账户/用量；不做 open-webui 的 Audio/Privacy（AIGX 无这些能力） | 无 |

## 六、Playground 三模式（playground/）

| 范式 | open-webui | AIGX 现状 | 决策 | 依赖后端项 |
|---|---|---|---|---|
| Chat 模式 | 有 | ChatDebugger 已支持 | **改造引入**：加参数调节（temperature/top_p/max_tokens/惩罚系数） | 无 |
| Completions 模式 | 有（全参数 + 双栏 JSON） | 无 | **引入**（P1）：请求/响应双栏 JSON、流式开关 | `/v1/completions` 已有 |
| Images 模式 | 有 | 无 | **引入**（P1）：提示词 → 生成 → 结果画廊 | `/v1/images/generations` 已有 |

## 七、workspace 知识体系

| 范式 | open-webui | AIGX 现状 | 决策 | 依赖后端项 |
|---|---|---|---|---|
| Models 模型预设 | 有 | 无 | **引入**（P1）：系统提示词 + 参数 + 绑定模型的预设集合 | 无 |
| Knowledge RAG 知识库 | 有 | 无 | **暂缓**：需要向量存储与 chunking，是大工程；P2 立项并单独设计（不动 bridge 边界） | P2：向量存储选型（Rust 生态） |
| Prompts 提示词库 | 有 | 无 | **引入**（P1）：轻量 CRUD + 插入语法 | 无 |
| Tools / Skills | 有 | 无 | **暂缓**：Agent 化，P2+ | 无 |

## 八、主题系统

| 范式 | open-webui | AIGX 现状 | 决策 | 依赖后端项 |
|---|---|---|---|---|
| theme store（system/light/dark） | 有（CSS 自定义属性驱动） | 有 `data-theme` 双态（light/dark）+ CSS 变量（`App.css`），但无 system 三态、无 store 抽象、主题色不可选 | **引入**：三态 theme store + 跟随系统 + 主题色变量（open-webui 是「器」的主要来源） | 无 |
| 30+ 主题色 | 有 | 无 | **引入**（P1）：预置 6–8 个主题色即可，不追 30+ | 无 |
| mobile 响应式断点 store | 有 | 部分页面有移动端适配（Dashboard 已修 400px 溢出） | **引入**：统一断点 store，替换各页散落的媒体查询 | 无 |
| OnBoarding 首次引导 | 有 | 无 | **暂缓**：P2（需等核心功能稳定后才有引导价值） | 无 |

## 九、组件手感（Drawer/DropdownMenu/ConfirmDialog/Toast/Skeleton/Tooltip/EmojiPicker/CodeEditor）

| 范式 | open-webui | AIGX 现状 | 决策 | 依赖后端项 |
|---|---|---|---|---|
| 组件库覆盖 | 全部 | `ui/` 已有 Button/Card/DataTable/Select/Switch/Tabs/Tooltip/Skeleton/EmptyState 等 18 个 + 玻璃三件套 | **保持覆盖** | 无 |
| 手感细节 | 五态齐全（default/hover/active/focus/disabled）、动效 120–200ms ease-out | 部分组件缺 focus-visible 焦点环、disabled 态；动效 250–300ms 偏慢 | **引入**：按五态标准逐个对齐（见《UI 审美迁移方案》验收表） | 无 |
| 键盘可达 | Tab/Esc/Enter 全覆盖 | 部分 | **引入**：Dropdown/Dialog/Drawer 补键盘导航与 aria | 无 |
| 触控目标 ≥44px | 是 | Switch 36×20 等偏小 | **引入**：交互控件按 ≥44px 触控目标修正（可视觉上保持紧凑） | 无 |
| Drawer 抽屉 | 有 | 有 MobileDrawer（移动端专用） | **引入**：桌面端 Drawer（设置面板/详情面板用） | 无 |
| ConfirmDialog | 有 | 有 ConfirmDialog 组件 | **保持** | 无 |
| EmojiPicker | 有 | 无 | **暂缓** | 无 |
| CodeEditor | 有 | 无 | **暂缓**：P2（Artifacts 时一并引入轻量方案，不引 Monaco） | 无 |

## 附：玻璃拟态特色的去留（本章重点决策）

open-webui 是干净平面风，但 AIGX 的玻璃拟态（`glass/` 三件套 + `App.css` 变量体系）是既有品牌资产。**决策：保留但收敛**——

- **品牌层保留毛玻璃**：侧边栏、顶栏、模态框、登录页
- **内容区转干净平面**：消息流、表单、数据表、设置页（open-webui 式，保证可读性与性能）
- **落地方式**：CSS 变量新增 `--surface-flat` 语义色板，新页面默认平面，旧页面渐进迁移（见《UI 审美迁移方案》优先级表）
