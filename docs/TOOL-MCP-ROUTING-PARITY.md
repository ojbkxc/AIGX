# 工具服务（MCP / Skill）与路由对齐审计 — AIGX ↔ Agent2API

> 审计日期：2026-11-21
> AIGX：`D:\GitHub\AIGX`，`HEAD d80ca193`，`Cargo.toml` 版本 **1.0.76**（工作树存在其他成员的未提交改动，行号以本次只读快照为准；为抗漂移，每条同时给出符号名）。
> 上游：`D:\GitHub\Agent2API\desktop-tauri\src-tauri\server\src\server`（只读）。任务口径为 v3.5.0-beta.3，但其 `Cargo.toml` 实际声明 **3.5.0-beta.1**，本文按代码现状审计。
> 方法：只读 `Read`/`rg` 两棵源码树 + 既有文档；**未编译、未跑测试**。所有结论以 `file:line` 为证；"上游无"均指在 `core/`、`api/`、`config/` 全量搜索无对应符号/端点。
> 图例：**[已对齐]** 能力与契约等价；**[有差异]** 两边都有但语义/粒度/契约不同（含 AIGX 超出上游）；**[半死]** 代码在、接线不完整，数据面达不到声明效果；**[缺]** 上游有、AIGX 无对应实现。

---

## 0. 结论摘要

| 范围 | 项数 | 已对齐 | 有差异 | 半死 | 缺 |
|---|---:|---:|---:|---:|---:|
| 工具服务（MCP / Skill / REST / 配置 / 认证） | 22 | 15 | 6 | 1 | 0 |
| 路由（策略 / profile / 粘性 / 熔断 / AIMD / 特殊路由 / 可观测） | 33 | 12 | 18 | 2 | 1 |
| **合计** | **55** | **27** | **24** | **3** | **1** |

最重要的 4 条（详见第 4 节）：

1. **P0 — profile 的 provider 作用域按渠道类型比较**：`plan_route` 用 `ch.channel_type.as_str()` 匹配 `scope.providers`（`src/channel/routing_policy.rs:907`、`:916`），账号型渠道的类型恒为 `"account"`（`src/channel/mod.rs:103`）。一旦启用档案且 `provider_scope=Listed`，所有账号型渠道会在渠道层被整条裁掉，账号级白名单还没机会生效——存在"配置后全量路由为空"的风险。
2. **P1 — 路由组没有"虚拟模型"入口**：数据面 `resolve_profile(..., None)`（`src/api/openai/resolve.rs:320`）只靠 profile 的 `default_group` 才能命中组（`src/channel/routing_policy.rs:689-697`）；上游把组直接登记成 `/v1/models` 里的模型并按请求模型选组（`core/routing_groups.rs:184-196`、`api/pipeline.rs:255-263`）。
3. **P1 — 粘性粒度远粗于上游**：AIGX 的会话标识是 `body.user` 或 `api_key.user_id`（`src/api/openai/chat.rs:175-179`），且只粘渠道；上游按请求头/`prompt_cache_key`/内容哈希派生会话键并粘账号（`core/upstream/route_state.rs:347-367`、`provider_loop.rs:615-622`）。
4. **P1 — 图片 MCP 提交缺两类上游护栏**：AIGX `/mcp/image-generation` 直接进通用 `mcp_post`（`src/api/tool_runtime.rs:456-463`），没有上游的 Origin 同源校验、16 MiB 上限和"拒绝批量数组"（上游 `api/tool_runtime.rs:304-323`）。

---

## 1. 工具服务（MCP / Skill / REST / 配置 / 认证）

### 1.1 MCP 三类工具

| # | 项 | 状态 | AIGX 证据（file:line） | 上游证据（file:line） | 差异/动作 |
|---|---|---|---|---|---|
| T1 | web-search MCP `tools/list` 契约（`web_search` / `web_fetch`，参数与上限 1–10） | [已对齐] | `src/tool_services/mcp.rs:76`（`web_search_spec`） | `core/tool_services/mcp.rs:50`（同名函数） | 工具名、title、description、`inputSchema` 逐字段一致。 |
| T2 | web-search MCP `initialize` / `server/discover` / notification / `tools/call` 分发 | [已对齐] | `src/tool_services/mcp.rs:313`（`dispatch_one`）、`:356`（initialize）、`:368`（server/discover）、`:390`（tools/list）、`:402`（tools/call）；协议版本常量 `:31-32` | `core/tool_services/mcp.rs:248`（dispatch_one）、`:280`（initialize）、`:289`（server/discover）、`:299`（tools/list）、`:305`（tools/call）；`:18-19` | AIGX 额外接受批量数组与 `id` 非法校验，语义与上游一致。 |
| T3 | vision MCP（`image_analyze`，1–10 张，prompt ≤16000） | [已对齐] | `src/tool_services/mcp.rs:127`（`vision_spec`）；`src/tool_services/vision.rs:334`（`run_vision`）、`:32-33`（上限） | `core/tool_services/mcp.rs:101`（`vision_spec`）；`core/tool_services/vision.rs:194`（`run_vision`）、`:26-27` | 契约一致；AIGX 视觉走内部网关 Key，与上游同型。 |
| T4 | image-generation MCP 工具集（`generate_images` / 可选 `edit_images` / `get_image_result`） | [已对齐] | `src/tool_services/image_mcp.rs:17`（`spec`）、`:26-74`（工具定义） | `core/tool_services/image_mcp.rs:4`（`spec`）、`:10-54` | 工具名、`inputSchema`、`additionalProperties:false`、参考图上限一致。 |
| T5 | 图片任务生命周期：`request_id` 幂等、去重 `request_hash`、`running/succeeded/failed/uncertain/expired`、保留期、`get_image_result` 指引 | [已对齐] | `src/tool_services/image_runtime.rs:271`（`submit`）、`:849`（`job_result`）、`:899`（`mcp_result`）；`src/tool_services/image_jobs.rs:114-130`（状态）、`:274-292`（超时/过期） | `api/tool_images.rs:49-75`（参数/校验）、`:217`（`run_job`）；`api/tool_image_results.rs:9-46`（结果形状） | 状态机与结果字段对齐；AIGX 结果里同样带 `expires_at` / `deduplicated_until` / `guidance`。 |
| T6 | 图片 MCP 提交护栏：Origin 同源、请求体 16 MiB、拒绝批量数组 | [有差异] | `src/api/tool_runtime.rs:456-463`（`mcp_image_generation` 直接 `mcp_post`）；Origin 只在下载路由校验 `:474` | `api/tool_runtime.rs:304-323`（`validate_origin` + `to_bytes(16 MiB)` + 数组拒绝） | AIGX 参考图取 URL 有 SSRF 校验（`image_runtime.rs:493`），但 MCP 入口本身缺 Origin/批量/显式体积护栏。建议补到 `mcp_image_generation`（安全/契约）。 |
| T7 | 图片生成计费/用量遥测 | [有差异] | `src/tool_services/image_runtime.rs:16-24`（模块头明说无 `RequestTelemetry`、不写日志、不扣费）；`:744`（`run_job` 直接调 bridge） | `api/tool_images.rs:218`（`RequestTelemetry`）、`:228-240`（telemetry 回填 credit/账号/模型） | AIGX 用管理员账号池直接转发，无 Key 级用量；若属产品选择需在文档/前端明示，否则补遥测。 |
| T8 | image-generation 管理面端点（config GET/PUT/DELETE、jobs、delete、files、bridge 脚本） | [已对齐] | `src/api/admin/tool_image_admin.rs:153-166`（router）；`src/api/admin/tool_image_admin.rs:51-166` | `http.rs:460-464`（图片管理路由）；`api/tool_image_admin.rs:22-43` | 路径与语义一致；AIGX 的 `GET config` 额外回 `generation_error/editing_error`。 |
| T9 | 生成图文件下载鉴权（MCP 与管理员两条） | [已对齐] | MCP：`src/api/tool_runtime.rs:469-496`（Origin + Key + owner + provider/model 白名单）；管理：`src/api/admin/tool_image_admin.rs:124-134`（admin） | MCP：`api/tool_images.rs:286-299`（`download`）；管理：`api/tool_image_admin.rs:39-43` | AIGX owner 用 Key id（`image_runtime.rs:61`），上游用 Key 哈希（`api/tool_images.rs:18-23`）；隔离效果等价。 |
| T10 | 图片生成本地 stdio 桥接脚本（`.cjs` 下发 + Node≥18 探测 + 目录约束） | [已对齐] | `src/tool_services/image_bridge.rs:1-35`（与上游逐字脚本）、`:37-60`；路由 `src/api/admin/tool_image_admin.rs:163-166` | `core/tool_services/image_bridge.rs:1-60`；路由 `http.rs:472` | 环境变量名保留 `AGENT2API_IMAGE_*`（有意兼容已接入客户端）。 |

### 1.2 Skill 打包与安装

| # | 项 | 状态 | AIGX 证据（file:line） | 上游证据（file:line） | 差异/动作 |
|---|---|---|---|---|---|
| T11 | Skill 包内容：`SKILL.md`、`skill.json`、Node/Python 两变体、版本 1.2.0、`.agent2api.json` | [已对齐] | `src/tool_services/skill_package.rs:17-18`（名称/版本）、`:44-71`（变体）；`src/tool_services/install.rs:7-15`（落点） | `core/tool_services/skill_package.rs:11`、`:27-70`；`core/tool_services/install.rs:1-15` | 包名/配置文件名保留上游，AIGX 品牌化留待客户端迁移波。 |
| T12 | Skill/MCP 安装与卸载管理端点 | [已对齐] | `src/api/admin/tools.rs:773-804`（router）；`:568`（install-skill）、`:612`（install-mcp）、`:697/:725`（uninstall） | `http.rs:465-472`；`api/tools.rs:402/433/482/498` | 端点集合一致。 |
| T13 | 安装目标客户端覆盖 | [已对齐]（2026-11-21 补齐） | `src/tool_services/config_files.rs`：10 个目标 `claude-code / codex / gemini-cli / opencode / zcode / workbuddy / cursor-cli / kimi-code / dsh / claude-desktop`；多 Skill 根目录（codex/dsh/kimi）、Claude Code MCP 落点改为 `~/.claude.json`、ZCode `mcp.servers`、Gemini `httpUrl`、各客户端超时字段 | `core/agents/registry.rs:47-152` + `resources/paths.rs`：9 个可安装目标（opencode 为 JSONC，AIGX 暂只提供 Skill 安装，MCP 写入标不支持） | 覆盖上游全部可安装客户端；`claude-desktop` 为 AIGX 保留。 |
| T14 | MCP 安装的"预览 + 确认令牌"写入协议 | [有差异] | `src/tool_services/install.rs:17-24`（模块头明确：上游走预览令牌，AIGX 直接改写 `mcpServers.<name>`） | `core/tool_services/install.rs:12-18`（声明确认令牌）；实现见 `core/agents/preview.rs` | AIGX 省掉确认令牌，直接原子写；风险是误改客户端配置前不可预览。 |

### 1.3 REST 端点

| # | 项 | 状态 | AIGX 证据（file:line） | 上游证据（file:line） | 差异/动作 |
|---|---|---|---|---|---|
| T15 | `/api/web-search`（元数据免鉴权；search/fetch 需 Key） | [已对齐] | `src/api/tool_runtime.rs:281-381`（handlers）、`:510-512`（路由） | `api/tool_runtime.rs:169-245`（handlers）；`http.rs:1011-1013`（路由） | 错误码、`queries/urls` 字段、全失败 502 语义一致。 |
| T16 | 搜索提供商集合（tavily/querit/firecrawl/ollama + workbuddy/workbuddy-intl/kiro） | [已对齐] | `src/tool_services/mod.rs:107-117`；provider 行 `src/tool_services/mod.rs:530-552` | `core/tool_services/mod.rs:91-93`、`:409-424` | 集合一致；账号型来源走插件宿主（AIGX 在 `main.rs:943-947` 注入桥）。 |

### 1.4 配置 schema 与持久化

| # | 项 | 状态 | AIGX 证据（file:line） | 上游证据（file:line） | 差异/动作 |
|---|---|---|---|---|---|
| T17 | 搜索提供商多密钥（有序 `keys`、每密钥限额/冷却、轮换） | [有差异] | `src/tool_services/provider_keys.rs:28`（上限 100）、`:45-58`（`ProviderKeyConfig`）、`:114`（`prepare_updates`）、`:225`（`usable`）；`ProviderConfig.keys` `src/tool_services/mod.rs:279-290` | 上游该树仅单密钥 `ProviderConfig{encrypted_key,key_hint,...}`（`core/tool_services/mod.rs:176-190`），`provider_keys` 文件不存在 | AIGX 已到 beta.2 形态，**超出**当前上游树；无动作，注意上游后续合并时的字段兼容。 |
| T18 | `ToolConfig` 主体形状与持久化键 | [有差异] | `src/tool_services/mod.rs:314-325`（`enabled/timeouts/providers/selection/vision/image_generation`），键 `toolServices` `:86`，存 `FileStore` `:221-255` | `core/tool_services/mod.rs:209-223`（同名字段），存网关配置 `toolServices`（`:261-266`） | 除 `keys` 扩展外形状一致；持久化介质不同（FileStore KV vs 网关 kv 表），对外 API 不受影响。 |
| T19 | 视觉/生图子配置形状 | [已对齐] | `VisionConfig` `src/tool_services/vision.rs:43-58`；`ImageConfig` `src/tool_services/image_config.rs:140-183` | `VisionConfig` `core/tool_services/vision.rs:34-49`；`ImageConfig` `core/tool_services/image_config.rs:18-38` | 字段逐项一致；生图任务存储 AIGX 用 FileStore（`image_jobs.rs:191-207`），上游用 SQLite `Db`（`api/tool_images.rs:16-20`）。 |

### 1.5 认证

| # | 项 | 状态 | AIGX 证据（file:line） | 上游证据（file:line） | 差异/动作 |
|---|---|---|---|---|---|
| T20 | 工具数据面鉴权（Bearer/x-api-key 网关 Key，跳过模型白名单，保留启用/过期/额度/IP/余额） | [已对齐] | `src/api/tool_runtime.rs:100-119`（`authenticate` 调 `validate_listing`）、`:155-162`（provider 白名单归一） | `api/tool_runtime.rs:57-59`（`model_access::authenticate(...,"tool")`）；`api/model_access.rs:31-66` | AIGX 额外查用户余额；两者都不在鉴权阶段按模型白名单裁剪。 |

### 1.6 其他工具面

| # | 项 | 状态 | AIGX 证据（file:line） | 上游证据（file:line） | 差异/动作 |
|---|---|---|---|---|---|
| T21 | 终端用户工具策略（`/api/user-tools/policy|catalog`） | [有差异] | `src/agent/user_tool_policy.rs`；路由 `src/main.rs:2495-2504`；响应构造 `src/api/admin/assistant.rs:189` | 上游该树无 `userTool/user-tool` 符号（`core/`、`api/` 全量搜索） | AIGX 独有能力（#34），不是对齐缺口；确认上游无同类需求即可。 |
| T22 | "网页工具调用模拟"开关 `/api/web-tool-emulation` | [半死] | 配置/读写 `src/config.rs:956-999`、`src/api/admin/settings.rs:1633-1656`、路由 `src/main.rs:2357-2359`；**唯一读取点**是 `/skill/web-search` 元数据 `src/api/tool_runtime.rs:184` | 上游该树无该端点/开关 | 开关不会改变任何转发行为（仅被元数据回显）。要么在工具调用路径消费它，要么在前端标注"仅声明能力"。 |

> 注：工具服务数据面的路由注册在 `src/main.rs:3523-3524`（`tool_runtime::router()`）与 `src/main.rs:3526-3527`（`admin::tools/tool_image_admin`）；MCP 路径为 `src/api/tool_runtime.rs:507-519`。

---

## 2. 路由

### 2.1 策略

| # | 项 | 状态 | AIGX 证据（file:line） | 上游证据（file:line） | 差异/动作 |
|---|---|---|---|---|---|
| R1 | 策略枚举与语义 | [有差异] | `RouteStrategy{Weighted,RoundRobin,Ranked}` `src/channel/routing_policy.rs:37-43`；数据面分派 `src/channel/mod.rs:2141-2184` | `RouteStrategy{Order,Rotate,Provider}` `config/types.rs:1140-1168`；选路 `core/routing.rs:550-565`、`core/upstream/rotate.rs:114-120` | 两套分类不同：上游"顺序/轮转/家内轮转"是**账号级队列**；AIGX"加权随机/轮询/多因子"是**渠道级**。AIGX 头注释把 A2A 三档映射成自己的三档（`routing_policy.rs:10-11`），但语义并非一一对应。 |
| R2 | weighted（加权随机 + 同优先级分组） | [有差异] | `src/channel/mod.rs:2180-2182`（Weighted → `select_for_model_filtered`）；同优先级加权随机 `src/channel/mod.rs:1075-1110` | 上游无"weighted 随机"策略；`Order` 是队首优先（`config/types.rs:1136-1137`、`core/routing.rs:550`） | AIGX 独有策略；上游 `order` 语义不同，不能当作对齐项。 |
| R3 | round_robin（首优先级组内确定性轮询） | [有差异] | `src/channel/mod.rs:2152-2179`（稳定排序 + `balancer.next`）；`src/channel/balancer.rs:1-80` | `Rotate`（整条账号线轮转）`core/routing.rs:551-563`；`Provider`（家内轮询）`:564` | 都"轮转"，但 AIGX 轮"渠道且仅首优先级组"，上游轮"账号线/家内账号"。粒度不同，不可互换。 |
| R4 | ranked（health × cost × rpm 多因子） | [有差异] | `src/channel/mod.rs:1740-1800`（`select_for_model_ranked_filtered`）；因子/权重 `src/channel/scheduler.rs:54`、`:496`；管理面 `src/api/admin/routing.rs:42/155-168` | 上游无多因子排序（`core/`、`config/` 搜索 `rank_candidates/cost_weight/rpm_weight` 无命中） | AIGX 独有且已接线；上游无此概念，属"超出"而非缺口。 |
| R5 | 策略可热更与持久化 | [已对齐] | `RoutingPolicyStore` `src/channel/routing_policy.rs:585-615`；启动应用 `src/main.rs:408-434`；保存 `src/api/admin/routing.rs:95-235` | `RoutingSettings` 内存快照 `config/types.rs:1211-1232`；`PUT /api/routing` `api/routing_api.rs:42-166` | 都是"保存即下一个请求生效"，无需重启。 |

### 2.2 分层 profile 与路由组

| # | 项 | 状态 | AIGX 证据（file:line） | 上游证据（file:line） | 差异/动作 |
|---|---|---|---|---|---|
| R6 | 三层档案容器（global / 模型级 / 组）与逐字段覆盖 | [有差异] | `RoutingProfiles{global,models,groups}` `src/channel/routing_policy.rs:408-416`；`RoutingProfile` 三态字段（含 `needs/sort_rules/default_group`）`:287-300`；解析 `:646-724` | `RoutingProfiles{global,models,groups}` `config/routing_profiles.rs:52-59`；`RoutingProfile`（`provider_scope/providers/disabled_providers/account_order/excluded_accounts/strategy/sticky`）`:22-36`；`settings_for` `:73-80` | 两边都有三层，但 AIGX 逐字段三态 + 能力需求/排序规则；上游 `account_order` 是 `BTreeMap<provider, Vec<account>>`（`:29`），AIGX 是扁平 `Vec<String>`（`routing_policy.rs:225`）。API 形状不兼容，前端需按各自契约。 |
| R7 | 路由组 CRUD | [已对齐] | `src/api/admin/routing.rs:532`（PUT profiles）、`:578`（GET group）、`:600`（PUT group）、`:642`（DELETE）；`upsert_group` `routing_policy.rs:481-486` | `api/routing_groups.rs:15`（GET groups）、`:25`（PUT group）；`config/routing_groups.rs:16-34` | 能力等价；AIGX 是 `/api/routing/profiles` + `/profiles/group/:id`，上游是 `/api/routing/groups` + `/api/routing/profile`，路径不同。 |
| R8 | 组作为对外可调用的"虚拟模型"入口 | [半死] | 数据面只传 `group_id=None`（`src/api/openai/resolve.rs:320`），组仅能由 profile 的 `default_group` 命中（`routing_policy.rs:689-697`）；`/v1/models` 聚合不含组（`src/api/openai/models.rs:49-60` 无组逻辑） | 组以 `group.model` 登记进 `/v1/models`（`core/routing_groups.rs:184-196`、`core/providers/catalog/view.rs:99`），请求模型命中即改写并锁定组（`api/pipeline.rs:255-263`、`core/upstream/route_context.rs:51-56`） | 组管理/预演可用，但客户端无法"把组当模型调用"。需数据面从模型名或请求头/Key 绑定解析 group_id，并登记模型清单。 |
| R9 | 档案 provider 作用域过滤 | [有差异] | `plan_route` 用 `ch.channel_type.as_str()` 比较 `scope.providers`（`src/channel/routing_policy.rs:907-925`）；账号型渠道类型恒为 `"account"`（`src/channel/mod.rs:103`） | 上游按账号所属 provider 过滤（`core/routing.rs:589/627`、`core/routing_groups.rs:44-47`） | **P0 风险**：`provider_scope=Listed` 时账号型/插件型渠道会被整条裁掉（`account`/`plugin` 不等于任何真实 provider），账号级白名单未及生效。建议 `plan_route` 对 Account/Plugin 渠道跳过渠道级 provider 判定（账号级已有 `account_scope`：`resolve.rs:352-410`）。 |
| R10 | 账号级规则：`account_order`、`excluded_accounts`、排序规则 | [已对齐] | `account_scope` 构造 `src/api/openai/resolve.rs:352-355`；注入 `AccountBridge.with_account_rules` `:410`；实现 `src/account_session/store.rs:264-268`、`:1453`、`:1599` | 上游 `plan_route` 账号顺序/排除 `core/routing.rs:640-785`；档案字段 `config/routing_profiles.rs:29-31` | AIGX 在账号取号阶段执行，语义一致；AIGX 额外支持 `sort_rules`（余额/加入时间/到期/配额重置）。 |
| R11 | 多规则总开关 | [有差异] | `RoutingPolicy.enabled` 默认 `false`（`src/channel/routing_policy.rs:571`）；关闭时 `resolve_profile` 直接返回基座 `:663-665` | 上游无独立总开关；档案存在即参与 `settings_for`（`config/routing_profiles.rs:73-80`） | AIGX 默认关，升级后已有档案不会自动生效；`GET/PUT /api/routing` 已暴露 `enabled`（`src/api/admin/routing.rs:44/176-177`）。 |

### 2.3 粘性

| # | 项 | 状态 | AIGX 证据（file:line） | 上游证据（file:line） | 差异/动作 |
|---|---|---|---|---|---|
| R12 | 四档粘性（auto/session/turn/off）与 TTL | [已对齐] | `StickyMode` `src/channel/routing_policy.rs:69-80`；TTL `:84-90`（auto 5m/30m、session 6h/6h、turn/off 不粘）；全局应用 `src/main.rs:424-431` | `StickyMode` `config/types.rs:1172-1206`（auto 5m、session 6h、turn/off 不粘） | 档位与 TTL 一致；AIGX 多一个 auto 硬过期 30m（额外保护，不冲突）。 |
| R13 | 会话标识来源（决定"粘多细"） | [有差异] | `session_id = body.user` 或 `api_key.user_id`（`src/api/openai/chat.rs:175-179`）；亲和键 `(session_id, model)`（`src/api/openai/resolve.rs:224-240`、`channel/mod.rs:1870-1876`） | `conversation_source` 支持 `ah-thread-id`/`x-opencode-session`/`x-session-id`/`x-session-affinity`/`prompt_cache_key`/Claude Code/Codex/内容哈希（`core/upstream/route_state.rs:347-367`）；粘账号 `sticky_bind`（`provider_loop.rs:747-749`） | AIGX 不解析这些会话头/字段，同一用户的不同会话会共享粘性、同一会话在无 `user` 时回落到用户级；且粘"渠道"不是"账号"。建议至少接入 `prompt_cache_key`、`x-session-id`、`ah-thread-id`、`x-opencode-session`。 |
| R14 | 粘滞管理面（查看/解除） | [有差异] | `GET /api/routing/sticky` `src/api/admin/routing.rs:393-463`；`POST .../clear` `:470-505`；路由 `src/main.rs:2437-2444` | `get_sticky`/`clear_sticky` `api/routing_workspace.rs:224-265`；路由 `http.rs:325-326`；条目是账号级（`core/upstream/route_sticky.rs`） | AIGX 返回 `channel_id` + `session/model` 编解码 id；上游返回账号级粘滞。功能等价、字段不同。 |
| R15 | 粘性写入/读取由策略门控 | [已对齐] | 读取门控 `src/api/openai/resolve.rs:222-240`（`sticky_enabled()`）；写入门控 `src/api/openai/chat.rs:754-761`；`StickyMode::sticky_enabled_from` `routing_policy.rs:98-101` | 上游 `ttl_ms()` 为 `None` 时不绑定（`core/upstream/rotate.rs:227`、`provider_loop.rs:622`） | 行为一致：turn/off 不读写粘性。 |

### 2.4 熔断

| # | 项 | 状态 | AIGX 证据（file:line） | 上游证据（file:line） | 差异/动作 |
|---|---|---|---|---|---|
| R16 | 熔断参数（开关/阈值/冷却/半开成功阈值）与管理面 | [已对齐] | `BreakerPolicy` `src/channel/routing_policy.rs:130-169`；应用 `src/api/admin/routing.rs:197-215`、启动 `src/main.rs:416-423`；状态机 `src/channel/circuit_breaker.rs:219-471`（`success_threshold` `:229-268`、`:423-447`） | `RoutingSettings` 四参 `config/types.rs:1211-1232`；`breaker_success` `core/upstream/route_state.rs:491-503`；调用 `provider_loop.rs:805-812`、`:1395-1400` | 四参齐全、半开连续成功语义一致。**默认冷却不同**：AIGX 30s（`routing_policy.rs:152-154`），上游 60s（`config/types.rs:1231`）——建议对齐或在前端标注。 |
| R17 | 熔断粒度 | [有差异] | 渠道级 `CircuitBreaker`（`src/channel/circuit_breaker.rs:219`；候选过滤 `channel/mod.rs:1066-1069`） | 账号级 `open` 集合（`core/upstream/route_state.rs:76-100`、`core/routing.rs:512`；`provider_loop.rs:1842-1855`） | AIGX 断"渠道"，上游断"账号"。一个账号型渠道下有多个账号时，AIGX 无法只熔断坏账号。属架构差异，需确认是否接受。 |

### 2.5 AIMD

| # | 项 | 状态 | AIGX 证据（file:line） | 上游证据（file:line） | 差异/动作 |
|---|---|---|---|---|---|
| R18 | AIMD 自适应限流（429 降额、成功回升、冷却过滤、监控快照） | [有差异] | 状态机 `src/channel/aimd.rs:1-280`；喂入成功/429 `src/channel/mod.rs:1872`、`:1920`；候选过滤 `:1069`、`:2092-2098`；看板 `src/api/admin/dashboard.rs:274` | 上游该树无 AIMD（`core/`、`api/`、`config/` 搜索 `aimd` 无命中） | AIGX 独有且**已完整接线**（此前 `aimd.rs` 的 `#![allow(dead_code)]` 注释并不代表未接线，实际三处调用点在上）。无动作。 |

### 2.6 特殊路由（compact / title）

| # | 项 | 状态 | AIGX 证据（file:line） | 上游证据（file:line） | 差异/动作 |
|---|---|---|---|---|---|
| R19 | 上下文压缩路由（ZCode 模板识别、按来源模型→压缩模型、revision CAS、`cap_output`） | [已对齐] | `src/route_special/compact.rs:1-95`（判据/配置/上限 200）；`src/main.rs:554`（安装）；路由 `src/main.rs:2564-2567`；管理面 `src/api/admin/route_special.rs:98-176` | `core/compact_routing/mod.rs:1-78`；`core/compact_routing/parameters.rs`；`api/compact_routes.rs:1-88`；路由 `http.rs:596` | 判据模板逐字一致；AIGX 持久化改 FileStore（`compact.rs:22`），`cap_output` 取 models.dev（`route_special/mod.rs:30-35`），上游取 provider 目录。 |
| R20 | 标题生成路由（Codex 头 / ZCode 提示词 / Claude Code 前缀，按客户端→模型，fallback） | [已对齐] | `src/route_special/title.rs:1-118`（判据/规则/fallback）；管理面 `src/api/admin/route_special.rs:178-263`；路由 `src/main.rs:2568-2571` | `core/title_routing/mod.rs:1-209`；`api/title_routes.rs:1-92`；路由 `http.rs:597` | 三家客户端判据、`fallback{original,error}` 一致；AIGX 无历史旧形状，故不做折叠（有意）。 |
| R21 | 特殊路由客户端识别来源 | [有差异] | 用 `api::client_id::ClientIdentity`（UA/`X-Client-Name`/自定义规则）映射客户端 id（`src/route_special/mod.rs:19-27`、`:70+`） | 用 `pipeline::request_source.behavior_client`（`api/chat.rs:134`、`api/protocol.rs:150/370`） | AIGX 无行为客户端识别，依赖 UA/显式声明；ZCode 已注册内置 UA（`mod.rs:24-27`），但非常规 UA 的客户端可能漏识别。 |
| R22 | 特殊路由数据面挂载点 | [已对齐] | chat `src/api/openai/chat.rs:203`；responses `src/api/openai/responses.rs:151`；anthropic `src/api/anthropic.rs:509` | chat `api/chat.rs:134-139`；responses `api/protocol.rs:150-155`；anthropic `api/protocol.rs:370-375` | 三入口都接，识别发生在协议转换前。 |

### 2.7 视觉 / 图片兼容

| # | 项 | 状态 | AIGX 证据（file:line） | 上游证据（file:line） | 差异/动作 |
|---|---|---|---|---|---|
| R23 | 视觉兼容规则集（`/api/vision-compatibility`：规则/assist/reroute/unknown/mode/overrides、revision） | [有差异] | 管理面 `src/api/admin/vision_compat.rs:1-60`；裁决 `src/image_compat/vision.rs:558`（`decide`）；路由 `src/main.rs:2557-2560` | `api/vision_compat.rs:1-160`；配置 `config/vision_compat.rs:207-248`；路由 `http.rs:795` | AIGX 模块头自述三处差距：revision 非强制、`legacyExpanded` 未展开、`prefer_current_route` 结构性不支持（保存 `true` 直接拒绝）——见 `src/api/admin/vision_compat.rs:24-38`。 |
| R24 | 会话图片压缩（base64 大图缩放/重编码/缓存） | [有差异] | `ImageCompatConfig{enabled,max_edge,quality,threshold_kb}` `src/image_compat/mod.rs:96-180`；压缩 `:269-500`；端点 `GET/PUT /api/image-compat/settings` `src/main.rs:2551-2555`、`src/api/admin/image_compat_api.rs:24-65` | `ImageCompressSettings` `config/types.rs:1035-1062`；`GET/PUT /api/image-compress` `api/image_compress.rs:1-50`、`http.rs:799-802` | 功能等价，但**端点与字段名不同**：上游 `/api/image-compress` + `imageCompressEnabled/MaxEdge/Quality/ThresholdKb`；AIGX `/api/image-compat/settings` + `enabled/maxEdge/quality/thresholdKb`，且与视觉兼容共用一个端点。AIGX 前端已按本仓契约实现（`frontend/src/api/endpoints/tickets.ts:371-376`）。 |

### 2.8 错误策略

| # | 项 | 状态 | AIGX 证据（file:line） | 上游证据（file:line） | 差异/动作 |
|---|---|---|---|---|---|
| R25 | 统一错误处理策略（9 类错误规则、动作、冷却、提供商例外、revision）与数据面接线 | [有差异] | `src/error_policy.rs:1-80`（`ErrorKind/ErrorAction`）；路由 `src/main.rs:2344-2348`；管理面 `src/api/admin/settings.rs:1446-1521`；数据面 `src/bridge/account.rs:284`（`self_retry_allowed`）、`:407`（`should_switch`） | 当前上游树**无**该模块/端点（全仓搜索 `routingErrorPolicy/ErrorRule/SwitchAccount` 无命中） | AIGX 对齐的是 beta.2 契约面，当前 beta.3 树没有；属 AIGX 超出/保留能力。需在升级上游时确认是否被上游正式引入，否则保持现状。 |

### 2.9 预览 / 工作台 / 追踪

| # | 项 | 状态 | AIGX 证据（file:line） | 上游证据（file:line） | 差异/动作 |
|---|---|---|---|---|---|
| R26 | 候选队列 `/api/routing/queue` | [有差异] | `src/api/admin/routing.rs:274-322`；路由 `src/main.rs:2452` | `api/routing_api.rs:168`；路由 `http.rs:320`；`model` 可省（`routing_api.rs:19-20`） | AIGX 强制 `model`，缺失直接 400（`routing.rs:280-290`）；且队列是渠道级（账号级跳过原因需看 workspace）。要完全对齐需允许省略 model 的全局队列。 |
| R27 | 判定预演 `/api/routing/preview`（与队列共用判定序列，可指定 group） | [已对齐] | `src/api/admin/routing.rs:689-750`；路由 `src/main.rs:2466-2469` | `api/routing_groups.rs:58`（`preview`）；路由 `http.rs:323` | 都复用生产选路序列；AIGX 额外支持 `group_id` 预演。 |
| R28 | 工作台 `/api/routing/workspace` | [有差异] | `src/api/admin/routing_workspace.rs:240-...`；路由 `src/main.rs:2471-2474` | `api/routing_workspace.rs:265`；路由 `http.rs:321` | AIGX 模块头列出数据源差距（`routing_workspace.rs:26-38`）：无 `client_model`、无账号维度、无会话维度、`stickyHit` 依赖 route_replay、在途归因缺 provider/channel。响应形状为 AIGX 自定。 |
| R29 | 请求流向 `/api/routing/flow` | [半死] | `src/api/admin/routing_workspace.rs:695-...`；路由 `src/main.rs:2475-2478` | `api/routing_flow.rs:1-35`；路由 `http.rs:328` | AIGX 自述在途归因"`set_channel` 数据面未接线 → provider/channelId 恒空"（`routing_workspace.rs:31-33`）；实证 `InflightRegistry::set_channel` 定义于 `src/log/inflight.rs:215` 但全仓无调用者。running 维度只能按 model/user/key 看。 |
| R30 | 请求追踪 `/api/routing/trace` 与单条详情 | [已对齐] | `src/api/admin/routing_workspace.rs:1524`（列表）、`:1614`（详情）；路由 `src/main.rs:2480-2484` | `api/routing_trace.rs:12`（列表）、`:33`（详情）；路由 `http.rs:329-330` | 数据源不同：AIGX 扫 `RequestLogStore`（`routing_workspace.rs:1548-1568`），上游扫 `request_stats`（`routing_trace.rs:23`）；过滤/分页/详情语义等价。 |
| R31 | 回放列表 `/api/routing/replays` 与单条 `/api/routing/replay/:id` | [已对齐] | `src/api/admin/routing_workspace.rs:1661`；单条 `src/api/admin/logs.rs`（路由 `src/main.rs:2506`） | `api/routing_workspace.rs`（get_replays）；路由 `http.rs:327` | AIGX 单条回放走既有 `GET /api/routing/replay/{request_id}`，与上游同名能力。 |

### 2.10 其他路由开关

| # | 项 | 状态 | AIGX 证据（file:line） | 上游证据（file:line） | 差异/动作 |
|---|---|---|---|---|---|
| R32 | `failover_backoff`（失败退避）与 `sink`（垫底池）开关 | [缺] | 明确不做：`src/channel/routing_policy.rs:14`（"AIGX 已有冷却/断路器/健康归档承担，不做开关"）；`RoutingPolicy` 无对应字段 `:555-575` | `RoutingSettings.failover_backoff/sink` `config/types.rs:1214-1215`、`:1227-1228`；`sink` 参与重试 `provider_loop.rs:797`、`:1687`；`sink` 表 `route_state.rs:92/451-458` | AIGX 无"垫底重试已失败账号"能力；上游 `sink=false` 时不会把最后失败的账号再当备胎。若需完全对齐，需要新增字段并接入账号重试链；否则列入明确不做。 |
| R33 | 断路器默认冷却 | [有差异] | 默认 30s `src/channel/routing_policy.rs:152-154`（`default_breaker_cooldown`） | 默认 60s `config/types.rs:1231`（`breaker_timeout_seconds: 60`） | 仅默认值不同，可一键对齐；不影响显式配置。 |

> 路由注册总表见 `src/main.rs:2344-2508`；数据面入口 `src/api/openai/resolve.rs:318-410`（策略 → 候选 → 裁剪 → 账号规则）。

---

## 3. 差距计数（与第 0 节一致）

| 状态 | 工具服务（T1–T22） | 路由（R1–R33） | 合计 |
|---|---:|---:|---:|
| [已对齐] | 14（T1–T5、T8–T12、T15、T16、T19、T20） | 12（R5、R7、R10、R12、R15、R16、R19、R20、R22、R27、R30、R31） | 26 |
| [有差异] | 7（T6、T7、T13、T14、T17、T18、T21） | 18（R1–R4、R6、R9、R11、R13、R14、R17、R18、R21、R23–R26、R28、R33） | 25 |
| [半死] | 1（T22） | 2（R8、R29） | 3 |
| [缺] | 0 | 1（R32） | 1 |
| **合计** | **22** | **33** | **55** |

> 正文每行标签是本表的唯一依据；本表仅用于快速浏览。T22 的"网页工具调用模拟"是工具面单独一条，计入工具服务项数；R32 的 `failover_backoff/sink` 计入路由项数。

---

## 4. 优先级

### P0（先修，防功能失效）

| # | 项 | 证据 | 建议 |
|---|---|---|---|
| P0-1 | profile `provider_scope=Listed` 会把 Account/Plugin 渠道整条裁掉 | `src/channel/routing_policy.rs:907-925`；`src/channel/mod.rs:103` | 在 `plan_route` 对 `ChannelType::Account|Plugin` 跳过渠道级 provider 比较，交由账号级 `account_scope`（`resolve.rs:352-410`）过滤；补一条账号渠道 + Listed 单测。 |

### P1（功能/安全对齐）

| # | 项 | 证据 | 责任方 |
|---|---|---|---|
| P1-1 | 路由组没有"虚拟模型"入口，客户端无法按组调用 | `src/api/openai/resolve.rs:320`；`src/channel/routing_policy.rs:689-697`；上游 `core/routing_groups.rs:184-196`、`api/pipeline.rs:255-263` | routing-backend |
| P1-2 | 粘性会话标识只看 `body.user`/用户 id，且粘渠道不粘账号 | `src/api/openai/chat.rs:175-179`；`resolve.rs:224-240`；上游 `route_state.rs:347-367` | routing-backend |
| P1-3 | 图片 MCP 提交缺 Origin/16 MiB/批量数组护栏 | `src/api/tool_runtime.rs:456-463`；上游 `api/tool_runtime.rs:304-323` | tool-backend |
| P1-4 | 图片生成无计费/用量遥测 | `src/tool_services/image_runtime.rs:16-24`；上游 `api/tool_images.rs:218` | tool-backend（先决策是否产品化保留） |
| P1-5 | Skill/MCP 安装目标只有 4 家客户端 | `src/tool_services/config_files.rs:230-243`；上游 `core/agents/registry.rs:47-152`（13 家） | tool-backend（前端按 `install-targets` 自动渲染，无需改） |
| P1-6 | MCP 安装缺预览+确认令牌 | `src/tool_services/install.rs:17-24`；上游 `core/tool_services/install.rs:12-18` | tool-backend（与安全/产品确认） |

### P2（语义/契约/默认值）

| # | 项 | 证据 |
|---|---|---|
| P2-1 | 策略分类与上游 order/rotate/provider 不同名不同义（需在文档/前端明确映射） | `routing_policy.rs:10-11`、`:37-43`；`config/types.rs:1140-1168` |
| P2-2 | 熔断默认冷却 30s vs 上游 60s | `routing_policy.rs:152-154`；`config/types.rs:1231` |
| P2-3 | `failover_backoff` / `sink` 未实现 | `routing_policy.rs:14`；`config/types.rs:1214-1215`、`provider_loop.rs:797` |
| P2-4 | `/api/routing/queue` 强制 model；上游可省略做全局队列 | `src/api/admin/routing.rs:280-290`；`api/routing_api.rs:19-20` |
| P2-5 | flow 在途归因缺 provider/channel（`set_channel` 无调用者） | `src/api/admin/routing_workspace.rs:31-33`；`src/log/inflight.rs:215` |
| P2-6 | workspace 缺账号/会话/client_model 维度 | `src/api/admin/routing_workspace.rs:26-38` |
| P2-7 | 视觉兼容 revision 非强制、`legacyExpanded` 未做、`prefer_current_route` 不支持 | `src/api/admin/vision_compat.rs:24-38` |
| P2-8 | 图片压缩端点/字段名与上游不同 | `src/image_compat/mod.rs:96-180`、`src/api/admin/image_compat_api.rs:24-65`；上游 `api/image_compress.rs:1-50` |
| P2-9 | web-tool-emulation 开关只被元数据回显 | `src/config.rs:993`；`src/api/tool_runtime.rs:184` |
| P2-10 | 多规则档案默认关（升级后档案不生效） | `routing_policy.rs:571` |
| P2-11 | 组/档案 API 路径与上游不同（`/profiles` vs `/groups`、`/profile`） | `src/api/admin/routing.rs:532-660`；`http.rs:322-324` |
| P2-12 | 特殊路由客户端识别依赖 UA/显式头，非上游 behavior_client | `src/route_special/mod.rs:19-27`；`api/chat.rs:134` |

---

## 5. 明确不做（本轮审计建议保留的边界）

1. **provider 伪装 / 指纹伪造 / 上游签名伪造**：AIGX 渠道模型明确声明"网关不做上游伪装头/签名"（`src/channel/mod.rs:75-77`），伪装类能力应由插件边界承担；不把上游的伪装常量搬进网关。
2. **AGPL 产物打包/分发**：Agent2API 为 AGPL-3.0，AIGX 为 Source Available License；仅按已有做法移植后端逻辑并保留文件头归属（如 `src/tool_services/mod.rs:1`），不打包上游前端岛屿/桌面壳/闭源插件二进制。
3. **上游 SQLite 统计库/请求统计表**：AIGX 以 `RequestLogStore` + FileStore 聚合（`routing_workspace.rs:3-14`），不引入第二套 SQL schema。
4. **`prefer_current_route`（"当前路由范围内的视觉模型"）**：AIGX 视觉裁决在选路前，无"当前路由范围"概念；继续拒绝保存 `true`（`src/api/admin/vision_compat.rs:34-38`），不造无效开关。
5. **`failover_backoff` / `sink` 独立开关**：若团队接受"冷却/断路器/健康归档已覆盖"的现状，把 R32 列为不做而非补缺；否则转 P2 实现。

---

## 6. 给成员的行动简报（与本次 SendMessage 同步）

### tool-backend
- `mcp_image_generation` 补 Origin 同源、拒绝批量数组、显式 16 MiB 限制（`src/api/tool_runtime.rs:456-463`）。
- 决定图片生成是否补计费/用量遥测（`image_runtime.rs:16-24`）；若保留"管理员账号池不扣费"，在工具页文案与文档写明。
- `web_tool_emulation` 开关要么接入转发路径，要么降级为纯展示（`config.rs:993`、`tool_runtime.rs:184`）。
- 安装目标扩充到上游 13 家（`config_files.rs:230-243`；上游 `core/agents/registry.rs:47-152`）；MCP 安装的预览/确认令牌与产品确认（`install.rs:17-24`）。

### tool-frontend
- `install-targets` 按后端响应渲染，后端扩到 13 家时无需改结构；确认当前没有硬编码 4 家。
- 工具页明确图片生成默认关闭（`tool_services/mod.rs:335` 默认 `service != IMAGE_GENERATION`）与 `image-generation` 的 MCP-only 协议（`mod.rs:147`）。
- 若 tool-backend 决定 `web_tool_emulation` 仅展示，前端该开关需标注"能力声明"。
- 图片任务/配置面板已对齐 `/api/tools/image-generation/*`（`frontend/src/api/endpoints/tools_image.ts:57-75`），无需改路径。

### routing-backend
- **P0**：`plan_route` 对 Account/Plugin 渠道跳过渠道级 provider 过滤（`routing_policy.rs:907-925`）。
- 组作为虚拟模型：数据面解析 group_id/模型名 + `/v1/models` 登记（`resolve.rs:320`；上游 `routing_groups.rs:184-196`、`pipeline.rs:255-263`）。
- 粘性会话键接入 `prompt_cache_key`/`x-session-id`/`ah-thread-id`/`x-opencode-session`（`chat.rs:175-179`；上游 `route_state.rs:347-367`）。
- `/api/routing/queue` 允许省略 model；flow 在途 `set_channel` 接线或明确降级（`routing.rs:280-290`、`inflight.rs:215`）。
- 熔断默认冷却 30→60；`failover_backoff/sink` 决策（`routing_policy.rs:152-154`、`:14`）。

### routing-frontend
- 策略下拉继续用 `weighted/round_robin/ranked`（`components/RoutingPanel.tsx:28-38`），不要引入上游 `order/rotate/provider`；若产品要求展示上游术语，做映射文案。
- 组编辑器需在 P1-1 落地后支持"组作为模型"的提示/入口；当前只支持 profile `default_group`（`ProfileForm.tsx:398-415`）。
- 视觉兼容页需展示三处已知差距（revision 非强制/legacy 不展开/prefer_current_route 不支持），避免用户误以为配置生效。
- 图片压缩面板使用 `/api/image-compat/settings`（`tickets.ts:371-376`），不要改成上游 `/api/image-compress`。
- 工作台/流向页对"账号/会话维度为空"展示降级说明（`routing_workspace.rs:26-38`）。

---

## 附录 A：本次审计用到的关键上游路径

- 数据面：`api/tool_runtime.rs`、`api/tool_images.rs`、`api/tool_image_results.rs`、`api/tool_image_admin.rs`、`api/tools.rs`
- 工具核心：`core/tool_services/{mod,mcp,image_mcp,image_config,image_jobs,skill_package,install,vision}.rs`
- 安装/客户端：`core/agents/registry.rs:47-152`、`core/agents/resources`、`core/agents/preview.rs`
- 路由配置：`config/types.rs:1111-1235`、`config/routing_profiles.rs`、`config/routing_groups.rs`、`config/compact_routes.rs`、`config/title_routes.rs`、`config/vision_compat.rs`
- 路由运行：`core/routing.rs`、`core/upstream/{provider_loop,rotate,route_state,route_sticky}.rs`、`api/routing_api.rs`、`api/routing_workspace.rs`、`api/routing_flow.rs`、`api/routing_trace.rs`
- 兼容/特殊：`core/compact_routing`、`core/title_routing`、`api/vision_compat.rs`、`api/image_compress.rs`

## 附录 B：本次审计用到的关键 AIGX 路径

- 数据面：`src/api/tool_runtime.rs`、`src/api/admin/tools.rs`、`src/api/admin/tool_image_admin.rs`
- 工具核心：`src/tool_services/{mod,mcp,image_mcp,image_runtime,image_jobs,image_config,skill_package,install,vision,provider_keys}.rs`
- 路由策略：`src/channel/{routing_policy,scheduler,balancer,affinity,aimd,circuit_breaker}.rs`、`src/channel/mod.rs:1740-2184`
- 特殊路由：`src/route_special/{mod,compact,title}.rs`、`src/api/admin/route_special.rs`
- 视觉/图片兼容：`src/image_compat/{mod,vision}.rs`、`src/api/admin/{image_compat_api,vision_compat}.rs`
- 工作台/追踪：`src/api/admin/{routing,routing_workspace}.rs`、`src/log/inflight.rs`
