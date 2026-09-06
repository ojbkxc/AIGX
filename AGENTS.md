# AGENTS.md
> AIGX项目开发行为规范 - 确保代码可追溯、可接手、高质量

## 0. 项目速览（新会话必读）

**AIGX** 是 OpenAI/Anthropic 兼容的 AI 中转网关（对标 new-api）：
Rust (axum 0.7) 后端 + React 18 + TypeScript + Vite 前端，SQLite KV 存储，单二进制交付（前端构建产物内嵌仓库 `static/`）。

- **仓库**：https://github.com/ojbkxc/AIGX（main 分支；GitHub 凭据见服务器本地 git remote 或用户私有记录）
- **生产**：http://104.223.65.202:9527（美国服务器，systemd 服务 `aigx`，二进制 `/opt/aigx/aigx`，数据 `~/.aigx/`；SSH 凭据见用户私有记录，勿写入任何入库文件）
- **当前管理员**：`admin / 123456`（username 或 email `admin@aigx.local` 均可登录）

### 硬性约束（违反会导致返工，全部踩过的坑）

1. **本地不编译 Rust**——本机无完整验证工具链，所有 Rust 编译验证走 GitHub CI（push 后用 GitHub API 查 actions runs）。本地只做：`cd frontend && node_modules/.bin/tsc --noEmit && npm run build`。
2. **axum 0.7 路由参数是 `:id` 不是 `{id}`**——`{id}` 是 0.8 语法，在 0.7 下被当字面量导致所有带参路由 405。
3. **`--locked` 编译**——改 `Cargo.toml` 版本必须同步 `Cargo.lock`，否则 CI 全红。
4. **rustfmt 强制**——CI 跑 `cargo fmt --check`；长链式调用（`error_response("长中文", StatusCode::X)` 单行超宽）会被要求拆行，按 CI fmt diff 手工应用。
5. **并行会话共存**——常有另一个 AI 会话同时编辑本仓库。开工前 `git status` 检查工作区；**只 add/commit 自己改的文件**（不 `git add -A`），发现他人未提交改动时避让。
6. **Rust 测试用独立目录**——`temp_dir + pid + AtomicU64 序号`，并行测试共用 SQLite 会 `database is locked`。
7. **React `onClick={() => fn(args)}`**——`onClick={fn(args)}` 渲染期立即执行，TS2322。
8. **部署二进制后检查 config**——新二进制首启可能把 `~/.aigx/config.toml` 重置回 `127.0.0.1:8080`，需 sed 回 `0.0.0.0:9527` 再重启。
9. **`scripts/` 目录被 gitignore**（含服务器凭据的部署助手）——可入库的工具脚本放 `tools/`。

### 架构关键点

- 数据面 `/v1/*` 与无前缀**双挂**（`/v1/chat/completions` 和 `/chat/completions` 都通，兼容两种 base_url 填法）
- 管理面 `/api/*`；**前端管理后台永远不调 `/v1/*`**（数据面要 sk-xxx 密钥，管理 token 会 401 触发全局误踢登录）
- 模型映射是**可选别名**（new-api 语义）：渠道 `models` 是主数据源，未命中映射时模型名原样透传；`/v1/models` = 启用渠道 models 聚合
- 令牌权限：`verify_user` + 本人过滤；普通用户可见自己 key 明文，管理员看脱敏
- E2E：`cd frontend && node ../tests/e2e/run-e2e.mjs`（Playwright Chromium 直连生产，12 项断言含真实上游对话）
- 同步脚本：`tools/newapi_sync.py`（new-api MySQL → AIGX 渠道/分组/定价，凭据走环境变量）

### 参考项目（本机路径）

- `C:\GitHub\rustapi\new-api-main\new-api-main` — 功能对标（用户中心/充值/权限模型）
- `C:\GitHub\rustapi\open-webui-main\open-webui-main` — UI 审美基准（中性灰阶+蓝色强调）
- `C:\GitHub\v2board` — 用户账户/找回密码逻辑

## 1. 思考优先编码（Think Before Coding）

**Don't assume. Don't hide confusion. Surface tradeoffs.**

- 状态：假设显式；如果不确定，询问
- 多重解释：呈现它们；不要沉默地选择
- 更简单的方法存在时说出来；必要时推回
- 不明确时停止，指出混淆所在，并询问

**Scope a task from the requested words only.**

查找用户引述的话（按内容，而不是标题），不是从正文中隐含扩展。AIGX项目建议从"原始需求"开始验证范围扩展。

### 编码前三件事（必须确认）

1. **引述请求**：用户真正要求什么？
2. **我将构建什么**：具体是什么功能？
3. **我将放弃什么**：明确范围削减

**不要沉默地方法削减**。每次进行了方法削减，明确说明放弃了什么。

## 2. 简化优先（Simplicity First）

**最少的代码解决问题。没有推测。**

- 没有超出请求范围的功能
- 单用途代码没有抽象
- 如果需要200行且可以是50行，重写
- 如果资深工程师说它过度复杂，简化它

## 3. 手术式修改（Surgical Changes）

**只改动必须改动的。只清理你的烂摊子。**

- 不要"改进"附近的代码、注释或格式；不要重构未损坏的内容；匹配现有风格，即使你不同
- 删除你更改孤立的导入/变量/函数——但不要删除预先存在的死代码；提及它
- 测试：每一行更改直接追溯到用户的请求

## 4. 目标驱动执行（Goal-Driven Execution）

**定义成功标准。循环直到验证。**

- 将任务转化为可验证目标："添加验证" → 编写无效输入测试，然后通过它们
- 多步骤任务：简要说明计划：`step → verify: check` 行
- **强标准**让你能独立循环；**弱标准**（"让它工作"）需要持续澄清

## 5. 测试纪律（Testing Discipline）

**E2E测试是最高优先级信号。覆盖真实用户旅程。从不静默失败。**

- E2E覆盖有限时，在概率上优先于单元/集成测试
- 面向用户真实路径设计用例，不要跳过步骤
- **对于任何前端UI，用Playwright编写E2E**，向真正的后端API发送请求：没有模拟网络、fixture服务器或拦截的响应
- 不要在E2E中使用模拟数据；运行真实数据和服务。如果模拟似乎不可避免，停止并请求人工确认先
- 永远不要跳过、禁用或`.only`测试变绿；相反调查底层bug
- **在信任检查之前，命名它会让它失效——如果不会失效，它就不是检查。**

## 6. 研究纪律（Research Discipline）

**仅基于官方文档和源代码验证。不要猜测或用假设填补空白。**

- 细节仅通过**官方文档**和**源代码**确认；不要推测或填补空白与假设
- 如果文档和源代码回答不了，说明并询问——不要发明答案
- 引用具体的文档URL、文件路径或commit/version作为任何第三方行为的依据

## 7. 参考实现优先（Reference Implementations Before Building）

**在实现任何功能之前，研究成熟的玩家是如何做的——不要偏离生态系统。**

在编写第一行新功能之前，阅读：
- **主流AI网关实现**——研究至少三个成熟的、主流的AI网关，并研究每个网关如何解决此问题。对转换不确定时，阅读其上游源码了解相同提供者+端点，并进行比较
- **上游提供者文档**：每个端点的权威规范
- **上游SDK源码**：当文档模糊时（使用子字段、流式事件顺序、错误信封）的实际合约

规则：
- 对于任何新端点、请求转换或响应归一化，比较至少三个主流网关如何处理它，引用一个上游规范源，并将此比较总结为设计注释/PR描述
- 如果你的设计与这些网关的解决方案不同，命名偏离并证明其合理性（"他们做X但我们需要Y，因为Z"——不是"我不知道他们如何处理它"）

## 8. 独立审计优先合并（Independent Audit Before Merge）

**每次推送到PR必须先由独立审计代理审查。合并被阻止，直到所有HIGH/MEDIUM发现已解决或明确证明合理。**

每次推送到PR后，用新的`general-purpose`代理开始新的内容，没有共享上下文。简要地冷配置 PR URL 和 PR 声称阻止的合约。将每个角度作为阻碍：

- **正确性**：它做描述声称的吗？真实回归会失败断言吗？
- **可靠性**：race条件、错误处理、重试/超时、慢CI上的传播时序
- **安全**：auth/authz、边界处输入验证、注入、头转发（及什么故意不转发）
- **敏感信息泄漏**：日志/错误中的密钥、内部分类或上游提供者详细信息在用户可见字段中、测试中的令牌/PII
- **破坏性更改**：API形状、磁盘上格式、线路协议、默认转移；如果是破坏性的，它是否被门控/版本化
- **E2E覆盖**：用户可见的合约，不仅是单元快乐路径；模拟足够紧，以便回归不能偷偷通过

用**具体建议代码**而不是含糊的"考虑"输出**HIGH/MEDIUM/LOW**每个发现。**合并门**：每个HIGH和MEDIUM要么在代码中解决，要么在PR中明确证明合理（例如"功能缺口，已 filed as #N，同意不阻塞"）；静默合并是不够的。对于显示网关/产品行为缺口的出现发现，申请单独的问题并链接它们。自审查遗漏作者的盲点——独立代理会捕捉到它们。

## PR批量规则

**默认每个会话一个PR。**此仓库由代理端到端开发——不需要人工审阅者需要小审阅单元，而 CodeRabbit 按PR计费和限制配额

**为每个会话只保留一个打开的PR**，将后续工作和相关工作作为额外提交推送到它（包括规则和文档骑手）而不是打开另一个。只在修复必须在其他地方独立合并的情况下拆分，或当用户要求单独交付时拆分

## 行为变更在PR描述中陈述

**发布说明在发布时从PR内容汇编，读作为PR正文而不是提交主题——所以不在文件中列在任何地方的行为变更从未到达用户。**在描述中明确说明：发生了什么，升级后现有配置或调用方体验到什么变化，以及是否需要编辑任何内容。

这涵盖了验证变得更强（现有资源不再保存）、过滤变得更宽（配置值静默停止生效）、默认转移以及线路或schema重塑。**不要打开单独的跟踪问题**；PR正文是记录。

## Handler Families保持同步 — 修复整个类

**面向客户端的端点处理程序按共享调度、认证、路由、遥测和围栏逻辑的handler families出现**；一个family中一个点的bug或功能通常适用于其他所有点，以及未修复的兄弟而产生的**静默**（什么都不会错误，行为只是安静地降级）。

- 当你修改按请求机制时（运行时指标、限制、auth检查、usage发射、头线程），在整个handler家族中重叠编写相同模式风格的grep并**wire every sibling path在相同的PR中**——流式和非流式分支——或明确声明哪个兄弟延迟，为什么，并立即申请随访问题
- 提到随访没有问题**就不是问题**：它在某个PR描述中并总没有人再回来了
- **Metrics中没有任何调用者的emit函数是不可见的。**它的方法是`pub`，所以死代码分析从未触发；单元测试直接调用它并传递；唯一症状是抓取中永远不出现的系列——无法与"还无流量"区分。度量family在真实流量驱动时发货——在`GET /metrics`上驾驶E2E测试之后——而不是当`Metrics`可以发射时

## 用法在客户端边界转换，不在遥测一个

**`UsageStats`携带两个*不共用*的缓存表示，相反算术，跨协议handler必须转换一个给客户端同时将UsageEvent保持在upstream的自己的形状。**

```rust
// 正确的转换
impl UsageRenderer for MetricsRenderer {
    fn render(&amp;self, stats: &amp;UsageStats) -&gt; &amp;'static str {
        match stats.direction {
            ReqDirection::ToClient => snippet_display(stats),
            ReqDirection::ToUpstream => stats.subset
        }
    }
}
```

## 大商5025倍的下沉dB去现（LargestSocketDowngrade）

- "Downgrade to Savings"消息总是以开放性的"省钱路线"表示，而不是忽视其他限制如SLA或可靠性

## AIGX权限管理（RBAC）

### 角色定义
```css
1. admin（最高权限）
2. manager
3. user
4. auditor（审计员）
```

### 权限矩阵
```typescript
const permissions = {
  admin: ['system:admin', 'user:*', 'channel:*', 'key:*', 'billing:*', 'logs:*'],
  manager: ['user:read', 'user:write', 'channel:read', 'channel:write', 'key:read', 'key:rotate', 'billing:read'],
  user: ['key:read', 'billing:read'],
  auditor: ['logs:read', 'logs:export']
}
```

### 认证流程
1. 用户登录 → 验证凭证
2. 获取JWT token
3. 使用token访问API
4. 后端验证token和权限

### 安全最佳实践
- HTTPS mandatory
- OAuth2/JWT for API auth
- 请求签名验证
- 敏感信息脱敏
- 操作审计日志