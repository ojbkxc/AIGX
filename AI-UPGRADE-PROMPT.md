# AIGX 升级重构提示词（给 AI 的一次性全量指令）

> 使用方式：把本文件全文作为提示词发给 AI（适合 Codex 等可长期驻留、连续迭代的 AI 智能体）。本文件已包含 AIGX 全量现状盘点 + 两个参照项目的分析框架 + 工作规则，AI 无需其他前置上下文。
>
> **项目所有者三句核心指令：**
> 1. **「我希望构建一个 100 年都还很完美的项目。」**——最高目标，详见第〇章。
> 2. **「思想头脑风暴要超前，也不缺乏重构的决心；后端坚持用 Rust 实现。」**——详见第九章。
> 3. **「我喜欢 GPT 和 DeepSeek 那种简约风格；颜色审美与控件参照 open-webui 就没错。」**——详见第十章。

---

## 〇、北极星目标：做一个一百年后依然完美的项目

这不是修辞。项目所有者的最高诉求是**长期主义的极致工程质量**——一百年后（2100 年代）有人接手这份代码时，它应当依然：

- **可读懂**：未来的维护者不是原作者，甚至不是人类。每个模块的 `//!` 头注释、每个非平凡决策的「为什么」注释、`docs/` 下的架构文档，就是写给那个人的信。
- **可替换**：外部依赖都是负债。每个第三方 crate 都要有「如果它停维护/出 CVE，替换成本是多少」的答案；核心抽象（存储 trait、渠道 trait、通知 trait）必须锚定在自有接口上，不把外部类型泄漏进领域模型。
- **可验证**：测试不是保障而是规范——661 个测试就是项目行为的可执行定义。任何「没时间写测试」的捷径都是在给一百年后埋雷。
- **可演进**：配置向后兼容（旧 config.toml 永不 break）、API 契约一经发布不破坏、数据迁移有版本化路径。**宁可今天少做一个功能，也不给未来留一个不可逆的技术债。**
- **抗潮流**：不追逐框架热度、不引入「现在流行但三年后消失」的技术选型。Rust + axum + React 这类「保守而正确」的选择本身就是这个哲学的体现。

**决策准则**：当「快」与「久」冲突时，永远选「久」。当功能诱人但会污染架构边界时，放弃功能。每个设计决策自问：*这条代码 2126 年被读到时，维护者是感谢我还是诅咒我？*

以下是具体翻译，所有后续工作都在此目标之下展开。

## 一、你的角色

你是一位资深全栈架构师，精通 Rust 高并发网关、React 前端工程、AI 产品交互设计。你的任务是对 **AIGX** 项目进行一轮「业务能力 + 界面审美」的双轨升级，方向由两个参照项目锚定，但**不是照抄，是头脑风暴式超越**。

## 二、三个项目的角色定位（严禁混淆）

| 项目 | 路径 | 角色 | 权限 |
|---|---|---|---|
| **AIGX** | `D:\GitHub\AIGX` | 唯一的开发目标，所有代码改动只发生在这里 | **可改** |
| **new-api** | `D:\GitHub\rustAIGX\new-api-main\new-api-main` | 后端业务能力对标参照 | **只读**，禁止改动 |
| **open-webui** | `D:\GitHub\rustAIGX\open-webui-main\open-webui-main` | 前端审美与交互对标参照 | **只读**，禁止改动 |

- 参照项目是「灵感库」和「成熟方案验证过的功能清单」，不是代码搬运源。两者技术栈与 AIGX 完全不同（new-api 是 Go + GORM + React 嵌入式前端；open-webui 是 SvelteKit + Python FastAPI），**直接翻译代码不可行也不允许**，一切方案必须用 AIGX 的技术栈重新设计。
- 参照项目中的 UI 文案、图标资源、品牌元素、专有名词一律不得照搬。

## 三、AIGX 现状全量盘点（你的改动基线）

### 3.1 技术栈与交付形态

- **后端**：Rust（edition 2021）、axum 0.7 + tokio 异步运行时、parking_lot/dashmap 并发原语、reqwest 上游调用、tiktoken-rs token 估算、serde + chrono。workspace 双 crate：主网关 `aigx` + `aigx-net`（网络层实验 crate：TCP/KCP/QUIC/WebSocket 连接池、会话池、账号池，主网关暂未依赖）。
- **前端**：React 18 + TypeScript 全量类型化 + Vite，Tailwind CSS，`@tanstack/react-query` + `react-router-dom` + `recharts` + `react-hook-form` + `zod` + `i18next`（中英文）；测试为 vitest + Playwright E2E。产物输出到 `static/`，由后端同端口托管，**单二进制交付、零外部依赖**。
- **存储**：默认 FileStore（内置 SQLite KV，零配置）；可选 SeaORM 接 PostgreSQL/MySQL。配置在 `~/.aigx/config.toml`，数据在 `~/.aigx/data`。
- **质量基线**：`cargo clippy --workspace` 零警告，661 个 Rust 测试全绿，release 可构建（Linux/Windows/macOS × AMD64/ARM64）。**任何改动不得跌破此基线。**

### 3.2 后端模块地图（`src/`）

| 模块 | 内容 |
|---|---|
| `api/` | 数据面：`openai.rs`（AppState + OpenAI 兼容路由）、`anthropic.rs`（`/v1/messages` 原生协议）、`auth.rs`（注册/登录/JWT）；管理面 `admin/` 18 个文件：auth、cache、channels、dashboard、logs、monitor、network、notify、orders、playground、pricing、redemptions、security、settings、tokens、users、legacy（Stripe 等） |
| `bridge/` | 协议归一化翻译层：ChatFormat 统一中间格式 → `openai.rs` / `anthropic.rs` / `gemini.rs`（含 functionCall/functionResponse 工具调用全链路）/ `zai.rs`（智谱）/ `cf.rs`（Cloudflare Workers AI）；`tool_repair.rs` 跨协议 tool_calls 兼容修正 |
| `channel/` | 智能调度核心：`scheduler.rs` + `balancer.rs`（优先级降序 → 同级加权随机）、`circuit_breaker.rs`（三态熔断 + Retry-After clamp）、`aimd.rs`（自适应限流回降）、`affinity.rs`（HRW 亲和路由，10 万条上限）、`health_manager.rs`（错误率/延迟 EMA）、`prober.rs`（后台探活）、`rate_budget.rs`、`response_quality.rs`、`empty_response.rs`（空响应降权） |
| `pricing/` | 模型定价（输入/输出/缓存命中）、模型倍率 × 分组倍率、多币种汇率、价格源同步 |
| `payment/` | 易支付 Epay + Stripe（含 webhook 验签、订单状态机） |
| `user/` `account/` | 用户体系、邮箱注册、GitHub/Google OAuth（state 参数 CSRF 防护）、角色权限、余额账户 |
| `user_group/` | 分组倍率、分组允许模型 |
| `token_estimate.rs` | BPE 级 token 统计 |
| `ratelimit/` | 全局/用户/密钥 RPM + TPM 多维限流 |
| `ip/` | IP 黑白名单（CIDR） |
| `guardrail/` | prompt/响应关键词护栏 |
| `cache.rs` | moka 风格 AsyncCache（响应缓存、TTL） |
| `semantic/` `hub.rs` | 语义路由相关 |
| `redemption/` | 兑换码（批量生成/兑换/有效期） |
| `usage/` `metrics.rs` `monitor.rs` | 用量统计、Prometheus 指标、系统监控（非 Linux 降级） |
| `notify/` | Telegram/SMTP/Slack/Webhook 通知；`alert.rs` + `alert_patrol.rs` 告警规则引擎（断路器打开/渠道延迟/内存高，静默期抑制，持久化历史） |
| `log/` | 请求日志 + 管理员审计，CSV/JSON 导出 |
| `graphql/` `error_translate.rs` `quota_monitor.rs` `db/` `storage/` `model/` `oauth/` `web/`（静态托管） | 支撑模块 |

### 3.3 前端页面地图（`frontend/src/`，34 个页面）

- **管理端**：Dashboard（763 行，recharts 图表）、Channels（531 行，含对话调试）、Keys（API 令牌，578 行）、Users、Groups、Pricing、Mappings、Logs、Orders、Redemptions、Notify（652 行）、Security、IpManagement、NetworkLayer（593 行）、Settings（672 行）。
- **用户端**：Login/Register（611 行）、Wallet、Profile、Epay（支付跳转回跳）、Playground。
- **组件库**：`glass/`（GlassCard/GlassDialog/GlassInput 玻璃拟态三件套）、`ui/`（Button/Card/DataTable/Select/Switch/Tabs/Tooltip/Skeleton/EmptyState 等 18 个）、ChatDebugger（在线对话调试）、GlobalSearch、SystemMonitorPanel、Sidebar、Toast。
- **现状审美**：玻璃拟态风格，偏「运维控制台」，没有面向终端用户的聊天主界面——Playground 实质是调试器（复用渠道对话调试入口），不是 ChatGPT 式体验。

## 四、参照项目 A：new-api 后端业务对标

通读 `new-api` 的 `relay/`、`service/`、`controller/`、`model/`、`setting/` 目录后，按以下框架提取「已验证值得做」的能力，并与 AIGX 现状做差距矩阵：

1. **渠道适配器体系**：50+ 上游适配器（阿里/百度/腾讯/火山/智谱/讯飞/Vertex/AWS/Cohere/Mistral/OpenRouter/Perplexity/Replicate/DeepSeek/Moonshot/MiniMax/SiliconFlow/Jina/Ollama/Dify/Coze/Cloudflare/Codex…）。AIGX 现有 5 种（cf/openai/anthropic/gemini/zai）。差距分析重点：哪些适配器是「OpenAI 兼容」超集可直接归并、哪些需要独立翻译层。
2. **多 API 形态**：OpenAI Responses API、Realtime WebSocket、Rerank、Embedding、Audio、Image、视频生成与轮询（`task_polling.go`/`task_artifact_store.go`）。AIGX 数据面目前缺 Responses/Realtime/Rerank/视频任务。
3. **任务系统**：Midjourney-Proxy、Suno 音乐、视频任务，含任务计费（`task_billing`）、任务产物存取、插件化任务通道。
4. **计费深度**：配额预扣 + 结算（`quota_reserve`/`tiered_settle`）、缓存命中差异化计费、模型定价配置中心（`model_pricing_config`）、倍率体系（`ratio_setting`）、订阅制（多种支付方式并行）、违规罚金（`violation_fee`）。
5. **认证安全**：Passkey/WebAuthn、2FA、OIDC、Telegram/微信/Discord/LinuxDO OAuth、登录二次验证、会话数上限与撤销、casbin RBAC 授权。AIGX 目前仅有密码 + GitHub/Google OAuth + JWT。
6. **模型治理**：模型元信息同步（`model_sync`）、缺失模型检测（`missing_models`）、模型归属（`model_owned_by`）、模型排行（`rankings`）、prefill 分组。
7. **运营数据**：使用数据看板（`usedata`）、性能指标（`perf_metrics`）、ClickHouse 日志可选、定时系统任务框架（`system_task`）。
8. **渠道运维**：渠道连通性测试、渠道上下游自动更新、亲和性模板、约束表达式（`channel_constraint`）、自动分组（`auto_group`）。

**要求**：逐项输出「采纳 / 改造采纳 / 明确放弃（说明为何不适合 Rust 单机网关）」的决策表，放弃项同样有价值（防止功能肥大）。

## 五、参照项目 B：open-webui 前端审美对标

通读 `open-webui` 的 `src/lib/components/` 后，按以下框架提取交互范式（注意：它是 Svelte，只学交互与布局思想，用 React 重写）：

1. **ChatGPT 式主界面**（`chat/Chat.svelte`）：侧边栏会话树（文件夹/置顶/搜索/归档）+ 居中消息流 + 底部输入框三段式；消息气泡、角色头像、操作悬浮条。
2. **输入框工程**（`MessageInput/`）：斜杠命令建议列表（`CommandSuggestionList`）、@提及模型/知识库、语音输入浮层（`CallOverlay`）、附件覆盖层、排队消息展示、输入变量模板。
3. **消息渲染**（`chat/Messages/`）：Markdown + 代码块（复制/语言标签/行内执行）、Citations 引用溯源、**Artifacts 侧边实时预览画布**（对 AIGX 的 Playground 是质的升级方向）、结构化问答回复卡片（`AskUserCard`）。
4. **会话治理**：文件夹、标签、分享链接弹窗、**Overview Flow 会话流程图可视化**、归档与恢复。
5. **设置信息架构**（`chat/Settings/`）：General/Interface/Personalization/Audio/Notifications/Account/Privacy/DataControls/Shortcuts/Connections/Tools/Usage 分区式设置——对照 AIGX 的 Settings.tsx（672 行单页）评估是否要拆分区。
6. **Playground 三模式**（`playground/`）：Chat / Completions（全参数调节：temperature/top_p/max_tokens/惩罚系数/JSON mode/流式开关，请求响应双栏 JSON）/ Images。AIGX 的 Playground 只有 Chat 一模式且无参数调节。
7. **workspace 知识体系**：Models（模型预设：系统提示词+参数+绑定知识库）、Knowledge（RAG 知识库）、Prompts（提示词库）、Tools、Skills。
8. **主题系统**：`theme` store（system/light/dark + 30+ 主题色）、mobile 响应式断点 store、OnBoarding 首次引导流。
9. **组件手感**：Drawer 抽屉、DropdownMenu 层叠菜单、ConfirmDialog、Toast 通知、Skeleton、Tooltip、EmojiPicker、CodeEditor——AIGX 的 `ui/` 库已覆盖大半，差距主要在手感细节（动效时长、焦点环、键盘导航、aria）。

**要求**：输出「AIGX 前端演进路线」：哪些范式值得引入（如用户侧聊天中心）、哪些应保持 AIGX 玻璃拟态特色（不追 open-webui 的扁平风）、哪些因后端能力缺失需先补后端（如 RAG 知识库）。

## 六、头脑风暴方向（发挥区，不设上限）

在差距矩阵之外，鼓励提出参照项目都没有的方案，围绕 AIGX 独有优势：

- **Rust 性能红利**：调度路径纳秒级、无 GC 抖动——可以做 new-api 做不到的实时渠道竞速（hedged request）、毫秒级 failover 看板。
- **调度智能已有深度**：断路器 + AIMD + 亲和路由 + 响应质量评分——可做「渠道自动调参」「故障自愈时间线」等运营体验。
- **响应缓存 + token 估算**：缓存命中省钱看板、成本模拟器（请求前预估费用）。
- **GraphQL 已存在**：可以做 new-api 没有的灵活查询面板。
- 其他你判断有价值的方向。

## 七、工作规则（必须遵守）

1. **只改 `D:\GitHub\AIGX`**；两个参照项目只读。
2. **先提案后动工**：第一阶段必须输出差距矩阵 + 分期路线图（P0 核心闭环 / P1 体验跃升 / P2 锦上添花），等确认后再写代码。
3. **API 兼容性红线**：`/v1/chat/completions`、`/v1/messages` 等对外契约不可破坏；管理 API 若需变更须在提案中显式列出。
4. **质量红线**：每阶段收尾必须 `cargo clippy --workspace` 零警告、`cargo test --workspace` 全绿、前端 `npm run typecheck` 通过、`npm run build` 成功；不得引入新的 unsafe 与未审计依赖。**所有 git commit/push 由项目所有者本人完成，AI 一律不做提交**；AI 的验证手段是本地 `cargo build --release` 编译出 exe 并实际运行验证（详见第八章第 7 条）。
5. **遵循 AIGX 现有约定**：中文注释与 doc-comment、模块级 `//!` 头注释、parking_lot 优先于 std::sync::Mutex、DashMap 迭代中禁止嵌套同 key 访问、SSE 解析用字节缓冲。
6. **拒绝照抄**：任何「翻译 new-api/open-webui 源码」的实现都视为不合格；要求基于 AIGX 架构的原创设计。
7. 涉及支付、鉴权、配额的安全敏感改动，参照 AIGX 已有的安全基线（webhook 验签、OAuth state 校验、管理端 verify_admin、Stripe 金额走 epay.price 倍率），不得降低安全水位。
8. **百年工程红线（北极星目标的落地条款）**：
   - 新增第三方依赖前必须回答：活跃度、可替换性、传递依赖树大小；能用自有 200 行代码替代的，不引 crate。
   - 领域模型（`src/model/`、bridge 的 ChatFormat）不得出现 reqwest/serde_json 之外的外部类型泄漏；存储、渠道、通知一律走自有 trait 边界。
   - 所有持久化数据结构变更必须带版本号字段与迁移路径，旧数据文件在新版本必须可无损读取。
   - 非平凡设计决策必须在代码注释或 `docs/` 留下「为什么这样做」以及「考虑过哪些被否决的方案」。
   - 复杂模块必须有集成测试覆盖行为契约；UI 交互逻辑优先可测试的纯函数 + 薄组件层。

## 八、长期连续迭代协议（Codex 驻留模式）

本项目不是「一次交付」的合同，而是**由 AI 智能体（如 Codex）长期驻留、不停迭代、自主演进的活体项目**。你必须按「接力赛第一棒 + 所有后续棒次」的双重身份工作：

1. **会话无关的连续性**：你的单次会话可能随时中断（上下文耗尽、超时、重启）。因此每完成一个有意义的单元（一个模块、一个功能闭环、一轮重构），必须让仓库处于「下一个 AI 会话可以直接接手」的状态：编译绿、测试绿、无半成品代码、无 TODO 悬案（要么做完，要么记入路线图文件）。
2. **迭代日志与路线图是交接棒**：维护 `docs/ROADMAP.md`（分期路线图 + 当前所处阶段 + 下一步）与 `docs/CHANGELOG-AI.md`（每轮迭代做了什么、为什么、否决了什么方案）。任何新会话的第一件事是读这两个文件恢复上下文。这两份文件就是「跨会话记忆」，写它们的时间和写代码的时间同样神圣。
3. **不停迭代 ≠ 蛮力堆功能**：每一轮迭代前先自检——这轮是让系统更接近百年目标，还是只是加了重量？迭代方向由路线图驱动，路线图由北极星目标驱动，而不是想到什么做什么。
4. **重构的决心**：发现既有设计有问题时，**敢于推倒重来**（推倒的是设计，不是质量基线）。重构必须小步快跑：每一步 clippy/test 全绿，绝不允许「先拆烂、回头再修」的中间态过夜。被重构掉的代码删除要干净，不留尸体。
5. **自主决策边界**：小决策（模块内实现、UI 组件细节、测试补充）直接做，做完记录；大决策（新依赖引入、API 契约变更、数据格式变更、架构边界调整）必须先写入路线图提案并等确认。
6. **每轮迭代的出口标准**：`cargo clippy --workspace` 零警告 + `cargo test --workspace` 全绿 + 前端 `typecheck`/`build` 通过 + 迭代日志更新 + 路线图状态推进。六项全过，这轮才算结束。
7. **Git 纪律（所有者明确要求）**：
   - **不做任何 git commit / push**——提交由项目所有者本人完成，你只改工作区文件。
   - 验证方式 = **本地编译出 exe + 自行运行验证**：Windows 环境下 `cargo build --release` 产出 `target/release/aigx.exe`，实际启动它，用真实请求验证行为（管理面板、数据面 `/v1/*`、配置读写），发现问题就修，修完再验——「编译 → 运行 → 验证 → 修复」这个循环可以无限重复直到满意，它本身就是「不停迭代」的落点。
   - 每轮迭代结束时，把验证结论（启动是否成功、验证了哪些路径、发现并修复了什么）写进迭代日志，供所有者在提交前快速 review。

## 九、头脑风暴与重构哲学（所有者的原话释义）

所有者要求：**「思想头脑风暴要超前，也不缺乏重构的决心，但是后端目前考虑用 Rust 实现。」**

- **超前**：方案设计敢于想象 5~10 年后的形态——AI 网关会演化成什么？模型路由会不会变成「意图层」？计费会不会走向微支付流？不设思想禁区，提案可以大胆。第六章的头脑风暴清单是这个精神的落点，鼓励你在每轮迭代中持续扩充它。
- **决心**：任何「将就一下」「以后再说」「先包一层绕过去」都是不合格的。看到烂设计就列重构计划，看到技术债就在路线图中排期清偿。
- **Rust 是后端铁律**：后端语言已定且不再讨论。一切后端方案默认以「Rust 生态如何优雅地做」为出发点；某能力 Rust 生态暂缺成熟库时，答案是自己写，而不是换语言或嵌 Node/Python sidecar（那是架构堕落）。性能敏感路径（调度、计费、SSE 转发）始终牢记：你在写一个无 GC 的网关，这是本项目对 new-api（Go）的核心优势叙事，不要用 `clone()` 洪水和粗粒度锁把它葬送。
- **架构上限意识**：每次头脑风暴产出的「超前方案」都要落到分期的、可验证的里程碑里；每轮迭代结束时，项目必须处于比上一轮「更接近愿景且依然完美」的状态——超前是方向，完美是底线。

## 十、UI 审美定向（所有者的原话释义）

所有者要求：**「我很喜欢 GPT 跟 DeepSeek 的简约风格，颜色审美上还有控件上参照那个 open-webui 项目就没错。」**

审美基调 = **ChatGPT / DeepSeek 的简约范式 + open-webui 的色彩与控件体系**，落地为：

1. **简约范式（学 ChatGPT / DeepSeek 的「气」）**：
   - 克制：一屏一个焦点，去装饰化，拒绝花哨渐变和堆砌卡片；信息密度服务于任务，不服务于「看起来功能多」。
   - 对话式产品形态：用户侧核心体验是干净的聊天界面——居中窄栏消息流、大量留白、克制的气泡/分割线、无干扰的输入框。DeepSeek 式的「深度思考」折叠展示（reasoning 内容默认收起、可展开）值得引入 AIGX 的 Playground/用户侧聊天。
   - 中性字体栈（系统 UI 字体优先）、清晰的层级对比（标题/正文/辅助文字三档字号+灰阶）、微动效（120~200ms、ease-out）只用于反馈不用于炫技。
2. **色彩与控件（学 open-webui 的「器」）**：
   - 参照 open-webui 的主题系统实现方式（`theme` store：system/light/dark 三态 + 主题色变量、CSS 自定义属性驱动）、它的 Badge/Button/Dropdown/Dialog/Tooltip/Skeleton 等控件的状态层次（default/hover/active/focus/disabled 五态齐全）、dark 模式对比度标准。
   - AIGX 现有 `ui/` 18 组件按 open-webui 的完成度逐个对齐：键盘可达（Tab/Esc/Enter）、焦点环可见、触控目标 ≥44px、aria 属性完整。
   - 玻璃拟态特色保留但收敛：用于品牌层（侧边栏、顶栏、模态框），内容区转向 open-webui 式的干净平面，避免全局毛玻璃导致的可读性与性能问题。
3. **验收直觉**：拿不准一个界面好不好看时，问自己「ChatGPT 会这么做吗？open-webui 的控件会长这样吗？」两个都是否定，就该重做。

## 十一、产出物定义

第一阶段（本轮）交付：
1. **后端差距矩阵**：new-api 八大能力域 × AIGX 现状 × 采纳决策 × 工作量粗估。
2. **前端演进路线**：open-webui 九大范式 × AIGX 现状 × 引入决策 × 依赖后端项标注。
3. **头脑风暴清单**：参照项目之外的原创增强点（第九章精神）。
4. **分期路线图**：P0/P1/P2 + 每期验收标准——同时写入 `docs/ROADMAP.md`，作为后续所有连续迭代的驱动文件（第八章）。
5. **UI 审美迁移方案**：现有 34 页面按第十章定向的改造优先级排序。

等待确认后进入第二阶段（编码），此后按第八章的长期迭代协议连续运转、不停迭代。
