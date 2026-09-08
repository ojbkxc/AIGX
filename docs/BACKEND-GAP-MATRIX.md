# AIGX 后端差距矩阵（参照 new-api 八大能力域）

> 第一阶段提案文档 · 2026-09-07 · 参照项目（只读）：`D:\GitHub\rustAIGX\new-api-main\new-api-main`
> 决策符号：**采纳** = 直接进入路线图；**改造采纳** = 按 AIGX 架构重设计后进入路线图；**放弃** = 明确不做并说明理由。
> 本矩阵只做「业务能力」对标，不涉及 UI 审美（见《前端演进路线》与《UI 审美迁移方案》）。

## 一、渠道适配器体系

| 项 | new-api | AIGX 现状 | 决策 | 工作量粗估 |
|---|---|---|---|---|
| 适配器数量 | 50+（阿里/百度/腾讯/火山/智谱/讯飞/Vertex/AWS/Cohere/Mistral/OpenRouter/Perplexity/Replicate/DeepSeek/Moonshot/MiniMax/SiliconFlow/Jina/Ollama/Dify/Coze/Cloudflare/Codex…） | 5 个 bridge：`cf`、`openai`、`anthropic`、`gemini`、`zai`（`src/bridge/`） | 改造采纳（分级策略，见下） | 见下 |
| OpenAI 兼容超集 | 30+ 家上游实质是 OpenAI 兼容协议变体 | `bridge/openai.rs` 已覆盖通用 OpenAI 兼容协议 | 采纳：**默认归并到 openai bridge**，仅按上游差异做「配置式补丁」而非新建适配器 | 每家 0.5–1 人日（主要是端点/鉴权头差异） |
| 非 OpenAI 原生协议 | 阿里、腾讯、讯飞、MiniMax、Cohere、Mistral、Replicate、Dify、Coze 等自研协议 | 缺 | 改造采纳：**P1 只做高价值 3 家**（阿里百炼、腾讯混元、MiniMax），P2 视需求再扩 | 每家 3–5 人日 |
| 渠道即插件（JS plugin / relaykit） | new-api 用 Go 插件或 relaykit 扩展 | AIGX 用 Rust trait `Bridge`（`src/bridge/mod.rs`）——自带 5 个方法面：chat/chat_stream/embed/complete/generate_image/responses_passthrough | 改造采纳：保留 Rust trait 边界，不引入 JS 插件（见「百年工程红线」） | — |
| Cloudflare Workers AI | new-api 有 cloudflare 适配器 | AIGX `bridge/cf.rs` 已实现，含账号池/GraphQL 用量 | 采纳：**保持**（这是 AIGX 对 new-api 的独有优势） | — |
| Claude 原生协议 | claude 适配器 | `bridge/anthropic.rs` + `/v1/messages` 路由 | 采纳：**保持** | — |

**结论**：AIGX 的 bridge trait 边界已足够优雅，不建议照搬 new-api 的「每家一个适配器目录」的膨胀模式。
新增适配器的正确姿势 = **OpenAI 兼容归并 + 原生协议独立 bridge**，并在 `docs/` 记下「为什么这家不能归并」。

## 二、多 API 形态

| 项 | new-api | AIGX 现状 | 决策 | 工作量粗估 |
|---|---|---|---|---|
| OpenAI Responses API | 有（`relay/responses_handler.go`） | 有：`/v1/responses` 路由 + `responses_passthrough`（透传，不经过 ChatFormat 转换） | 采纳：**保持现状**（透传已够用；需要计费/转换时再升级为转换模式） | — |
| Realtime WebSocket | 有（`relay/websocket.go`） | 缺 | 改造采纳：**P2**（aigx-net 已有 WebSocket 实验层可复用；Realtime 是长连接，必须评估对单机网关的 goroutine/task 占用） | 5–8 人日 |
| Rerank | 有（`rerank_handler.go`） | 缺 | 采纳：**P1**（rerank 是 RAG 客户刚需，实现简单：新增 bridge 方法 + 路由，参考 new-api 的 `channel/ali/rerank.go` 但用 ChatFormat 无关的独立 DTO） | 2–3 人日 |
| Embedding | 有 | 有：`/v1/embeddings` + `Bridge::embed` | 采纳：**保持** | — |
| Audio（TTS/STT） | 有（`audio_handler.go`） | 有：`/v1/audio/speech` + `/v1/audio/transcriptions` + `/v1/audio/translations`（`main.rs` 已挂全部三个路由） | 采纳：**保持**，P2 考虑补流式 TTS | — |
| Image 生成 | 有（`image_handler.go`） | 有：`/v1/images/generations` | 采纳：**保持** | — |
| 视频生成与轮询（task_polling） | 有（`task_polling.go`/`task_artifact_store.go`） | 缺路由；`pricing/video_billing.rs` 已有 Seedance/Veo 按时长计费模型，但未接数据面 | 改造采纳：**P2**——先补「长时任务框架」，视频任务是其第一个使用者 | 8–12 人日 |

## 三、任务系统（Midjourney / Suno / 视频任务）

| 项 | new-api | AIGX 现状 | 决策 | 工作量粗估 |
|---|---|---|---|---|
| Midjourney-Proxy | 有 | 缺 | **放弃**（本轮）。理由：Midjourney 依赖第三方中转，属灰色地带且需要大量运营维护；若未来真需，以「通用任务通道」承载而非专用适配器 | — |
| Suno 音乐 | 有 | 缺 | **放弃**（本轮）。理由同上，受众窄 | — |
| 通用长时任务框架（提交/轮询/取消/产物存取/计费） | 有（task_billing + task_artifact_store） | 缺 | 采纳：**P2 建设「任务通道」抽象**——`src/task/`，产物走 FileStore 子命名空间，计费复用 `pricing` | 10–15 人日 |
| 任务计费（task_billing） | 有 | 缺（但 `video_billing.rs` 已有雏形） | 改造采纳：**P2** 并入任务通道 | 含在任务框架内 |

## 四、计费深度

| 项 | new-api | AIGX 现状 | 决策 | 工作量粗估 |
|---|---|---|---|---|
| 配额预扣 + 结算（quota_reserve/tiered_settle） | 有 | 缺（`usage` 模块是「事后累加」，无预扣） | 改造采纳：**P1**——引入「预留/结算」两段式，对齐 long-running 流式请求的正确扣费语义 | 4–6 人日 |
| 缓存命中差异化计费 | 有 | 有 `cache_price` 字段（`pricing/mod.rs` + 管理 API）但数据面缓存命中当前记 0 费用 | 采纳：**P1**——把 `cache_price` 真正接到命中计费上（当前命中免费但已留痕，只需替换费用计算） | 1–2 人日 |
| 模型定价配置中心（model_pricing_config） | 有 | 有 `PricingStore` + `/api/prices` CRUD + `price_sync.rs` | 采纳：**保持** | — |
| 倍率体系（ratio_setting） | 有 | 有 `RatioConfig`（模型倍率 + 分组倍率）+ `/api/ratios` | 采纳：**保持** | — |
| 订阅制（多种支付并行） | 有（Stripe/Creem/Epay/Waffo） | 有 Stripe + epay（`payment/`），无「订阅」语义 | 改造采纳：**P2**——订阅是「周期配额包」而非新支付渠道，先补周期任务再谈 | 6–10 人日 |
| 违规罚金（violation_fee） | 有 | 缺 | **放弃**。理由：AIGX 是 API 网关而非内容平台，罚金是运营政策而非网关能力；需要时可经配额接口人工操作 | — |

## 五、认证安全

| 项 | new-api | AIGX 现状 | 决策 | 工作量粗估 |
|---|---|---|---|---|
| Passkey/WebAuthn | 有 | 缺 | 改造采纳：**P2**（Rust 生态无成熟 crate，按百年红线需自研，成本高、收益中等，排后） | 8–10 人日 |
| 2FA/TOTP | 有（`service/twofa.go`） | 缺 | 采纳：**P1**——TOTP 算法可用自有 100 行实现（HMAC-SHA1 + RFC 6238），不需要引 crate | 2–3 人日 |
| OIDC / 多 OAuth | 有（OIDC/Telegram/微信/Discord/LinuxDO） | 有 GitHub/Google OAuth（`src/oauth/`），无 OIDC | 改造采纳：**P2**——OIDC 是标准协议，一个通用实现覆盖多家 IdP，优先级高于逐一接厂商 SDK | 3–4 人日 |
| 登录二次验证 | 有 | 有基础版（per-IP 登录限流 + 连续失败锁定 5 分钟，`api/admin/auth.rs`），无邮件/短信二次验证码 | 采纳：**P1** 补 email 验证码（复用现有 SMTP 通知能力） | 2 人日 |
| 会话数上限与撤销 | 有 | 缺（Session 是无状态 HMAC token，无服务端会话列表） | 改造采纳：**P1**——引入会话注册表（jti + 用户维度计数），保持 HMAC 无状态校验但加「撤销检查」 | 2–3 人日 |
| casbin RBAC | 有 | 有 4 角色 RBAC（admin/manager/user/auditor，见 `AGENTS.md` 权限矩阵） | 采纳：**保持**，P2 考虑把权限矩阵下沉为可配置 | — |

## 六、模型治理

| 项 | new-api | AIGX 现状 | 决策 | 工作量粗估 |
|---|---|---|---|---|
| 模型元信息同步（model_sync） | 有 | 有 `/v1/models` 聚合（启用渠道 models），`owned_by` 硬编码为 `aigx`，缺真实供应商归属与上下文长度/能力元信息 | 采纳：**P1**——`/v1/models` 增补真实供应商归属（owned_by）与上下文长度/能力字段 | 2 人日 |
| 缺失模型检测（missing_models） | 有 | 缺 | 采纳：**P1**（渠道连通性测试可顺带返回「请求了渠道不支持的模型」统计） | 1–2 人日 |
| 模型归属（model_owned_by） | 有 | 缺 | 采纳：**P1** 随元信息同步一起做 | 含在上项 |
| 模型排行（rankings） | 有 | 缺 | **放弃**。理由：AIGX 已有 usage/models 统计（`/api/usage/models`），排行是展示层功能，前端可自行排序 | — |
| prefill 分组 | 有 | 有 user_group 模块（`src/user_group/`） | 采纳：**保持** | — |

## 七、运营数据

| 项 | new-api | AIGX 现状 | 决策 | 工作量粗估 |
|---|---|---|---|---|
| 使用数据看板（usedata） | 有 | 有 `/api/usage/trend` + `/api/usage/models` + `/api/usage/summary` | 采纳：**保持** | — |
| 性能指标（perf_metrics） | 有 | 有 `src/metrics.rs`（Prometheus 风格 `/metrics`）+ 延迟记录 | 采纳：**保持** | — |
| ClickHouse 日志 | 可选 | FileStore SQLite KV | **放弃**。理由：单机网关 + SQLite 是 AIGX 的零依赖卖点，ClickHouse 与「单二进制交付」冲突 | — |
| 定时系统任务框架（system_task） | 有 | 缺（只有手动触发） | 采纳：**P1**——轻量 cron 调度器（自有实现，不引大 crate），首期用于缓存清理/每日用量快照 | 2–3 人日 |

## 八、渠道运维

| 项 | new-api | AIGX 现状 | 决策 | 工作量粗估 |
|---|---|---|---|---|
| 渠道连通性测试 | 有 | 有 `/api/channels/:id/test` + 对话调试（ChatDebugger） | 采纳：**保持** | — |
| 渠道上下游自动更新 | 有 | 有 `save_discovered_models`（从渠道发现模型自动入库） | 采纳：**保持**，P2 考虑定时刷新 | — |
| 亲和性模板（affinity） | 有 | 有 `channel/affinity.rs` | 采纳：**保持** | — |
| 约束表达式（channel_constraint） | 有 | 有部分（模型白名单/分组），无通用表达式 | 改造采纳：**P2**——约束表达式容易失控；AIGX 方向是「窄而强」的内置约束而非图灵完备表达式 | 2–3 人日 |
| 自动分组（auto_group） | 有 | 缺 | 采纳：**P1**——按模型/供应商自动归类渠道，减少手工分组 | 2 人日 |

## 附：差距总览（P0 已具备 / 缺口排序）

**AIGX 已超越 new-api 的领域**（决策表中无需跟进的）：

- 调度深度：断路器三态 + AIMD 自适应 + 亲和路由 + 响应质量评分（`src/channel/`），new-api 无对应深度
- Cloudflare Workers AI 账号池与 GraphQL 用量查询
- 响应缓存 + token 估算 + 成本预估
- GraphQL 管理查询（`src/graphql/`）
- 单二进制交付（new-api 是 Go 服务 + 嵌入式前端，AIGX 更彻底）

**建议的 P1 后端缺口（按价值/成本排序）**：

1. 缓存命中差异化计费接入（1–2 人日，直接省钱可视化）
2. Rerank 端点（2–3 人日，RAG 客户刚需）
3. 配额预留/结算（4–6 人日，计费正确性）
4. 2FA/TOTP + email 二次验证（2–3 人日，安全水位）
5. 会话注册表与撤销（2–3 人日，安全水位）
6. 定时系统任务（2–3 人日，运营自动化基座）
7. 模型元信息同步 + 缺失模型检测（2–3 人日，模型治理）
8. 自动分组（2 人日，渠道运维）

**P2 后端缺口**：Realtime WebSocket、长时任务框架（视频生成）、OIDC、Passkey、订阅制、阿里/腾讯/MiniMax 原生适配器。

---

> 本矩阵的每一项决策理由都遵循「百年工程红线」：放弃项同样有价值（防止功能肥大），采纳项均标注了 Rust 生态实现路径与自有接口边界。

