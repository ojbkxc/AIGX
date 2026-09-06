# AIGX 业务全景文档

> 本文档是 AIGX 的**业务真相源（single source of truth）**，用于对齐前后端代码与维护者认知。
> 最后更新：2026-09-05 · 对应代码基线 `main@dc810e1`

---

## 1. 项目是什么

AIGX 是一个 **AI 中转网关**（OpenAI/Anthropic 兼容），聚合上游渠道（Cloudflare Workers AI、OpenAI 兼容、Anthropic、Gemini、智谱 Z.AI），向下游暴露统一 API，并叠加**认证、限流、分组权限、定价计费、多渠道调度、支付充值、日志审计、安全监控**等完整网关能力。

- **后端**：Rust 单 crate（axum 0.7），`src/` 约 3.6 万行
- **前端**：React 18 + Vite 5（管理后台），`frontend/src/` 23 个页面
- **存储**：默认 FileStore（rusqlite bundled SQLite KV），可选 SeaORM（MySQL/PostgreSQL）
- **部署形态**：Linux Docker 为主（系统监控采集在非 Linux 下自动降级）

---

## 2. 模块全景（后端 src/）

| 模块 | 职责 |
|------|------|
| `main.rs` | 入口 + 全部路由注册（115+ 条） |
| `config.rs` | AppConfig（server/admin/usage/epay/stripe/oauth/notify 等） |
| `api/` | `openai.rs`（数据面 + AppState）、`anthropic.rs`（Claude 原生协议）、`auth.rs`（ApiKey）、`admin.rs`（99 个管理 handler） |
| `channel/` | 渠道存储 + **调度增强**：`circuit_breaker`、`affinity`、`health_manager`、`empty_response`、`balancer`、`aimd`、`rate_budget`、`scheduler`、`prober` |
| `bridge/` | 上游协议适配：`cf`、`openai`、`anthropic`、`gemini`、`zai` + `tool_repair` |
| `pricing/` | 定价目录 + `price_sync`（多源同步）+ `exchange_rate`（多币种）+ `video_billing` |
| `payment/` | 易支付 Epay + Stripe + `order_store` |
| `ratelimit/` | 多维度 RPM/TPM 限流 |
| `user/` `user_group/` | 用户、分组 |
| `log/` | 请求日志 + 审计日志 + **安全事件**（SecurityEventStore） |
| `notify/` | Telegram/SMTP(含 STARTTLS)/Slack/Webhook + `alert`（告警规则）+ `alert_patrol`（巡检） |
| `ip/` | 全局 IP 白名单/黑名单（CIDR 支持） |
| `redemption/` | 兑换码 |
| `monitor.rs` | 系统监控采集（CPU/内存/负载/进程） |
| `metrics.rs` | Prometheus 文本指标 |
| `guardrail/` `semantic/` `proxy/` `graphql/` `oauth/` | 辅助能力 |

---

## 3. 数据模型（核心）

### 3.1 User（`src/user/mod.rs`）
`id, email(唯一), username, password(argon2), role, quota, used_quota, status, group, created_at`

### 3.2 ApiKey（`src/api/auth.rs`）
`id, key, name, is_active, created_at, last_used_at, user_id(None=管理员级), group, allowed_models, expires_at, quota_limit, used_quota, ip_limit, status`

### 3.3 Channel（`src/channel/mod.rs`）
`id, name, channel_type(Cloudflare/OpenaiCompatible/Anthropic/Gemini/Zai), base_url, api_key(enc:base64), priority, weight, status, models, account_id, discovered_models, ...`
> 渠道调度优先级：`priority` 降序 → 同优先级 `weight` 加权随机 → 断路器/健康/亲和再叠加。

### 3.4 ModelPrice / RatioConfig（`src/pricing/mod.rs`）
- ModelPrice：`model_name, input_price, output_price, cache_price, price_type(token|count), ...`
- RatioConfig：`model_ratio: HashMap<model, f64>` + `group_ratio: HashMap<group, f64>`

### 3.5 UserGroup（`src/user_group/mod.rs`）
`name(唯一), ratio, allowed_models, description`

### 3.6 TopUpOrder（`src/payment/mod.rs`）
`trade_no, user_id, amount, money, quota(下单锁定量), payment_method, status(pending/paid/expired), create_time, paid_time`

### 3.7 Redemption（`src/redemption/mod.rs`）
`id, code, name, quota, status(1未用/2已用/3禁用), used_by, used_at, created_at, expires_at`

### 3.8 SecurityEvent（`src/log/mod.rs`）
`id, event_type(auth_failure/rate_limit/ip_blocked/abuse/intrusion), severity(info/warning/critical), ip, user_id, request_id, detail, created_at`

---

## 4. API 契约（前端 ↔ 后端 ↔ 字段）

> 分组说明：`A`=管理员鉴权，`U`=用户鉴权，`公开`=无需鉴权。前端方法见 `frontend/src/api.js`。

### 4.1 认证
| 方法+路径 | 前端方法 | 说明 |
|-----------|---------|------|
| POST `/api/auth/login` | `login` | 邮箱/密码登录，返回 `{token,email,username,role,expires_at}` |
| POST `/api/auth/register` | `register` | 公开注册 |
| POST `/api/auth/logout` | `logout` | 登出 |
| GET `/api/auth/google` / `github` | Login 页跳转 | OAuth 授权 |
| POST `/api/auth/forgot-password` | `forgotPassword` | 生成重置 token |
| POST `/api/auth/reset-password` | `resetPassword` | 验证 token 后重置 |
| GET `/api/users/check?username=` | `checkUsername` | 用户名/邮箱可用性 |

### 4.2 数据面（下游调用，Bearer API Key）
| 路径 | 说明 |
|------|------|
| POST `/v1/chat/completions` | OpenAI 对话（流式/非流式 + 响应缓存） |
| POST `/v1/responses` | OpenAI Responses API 透传 |
| POST `/v1/completions` | 文本补全 |
| POST `/v1/embeddings` | 向量嵌入 |
| POST `/v1/images/generations` | 图片生成 |
| POST `/v1/audio/transcriptions` / `translations` / `speech` | 语音 |
| GET `/v1/models` | 模型列表 |
| POST `/v1/messages` | Anthropic 原生协议 |

### 4.3 管理面（核心，含字段要点）

**用户/密钥/渠道**
| 路径 | 前端方法 | 响应字段 |
|------|---------|---------|
| GET/POST `/api/users` | `listUsers`/`createUser` | User 全字段 |
| PUT/DELETE `/api/users/:id` | `updateUser`/`deleteUser` | |
| GET `/api/users/me` | `getMe` | 当前用户 |
| GET/POST `/api/keys` | `listKeys`/`addKey` | ApiKey 全字段 |
| DELETE `/api/keys/:id` | `deleteKey` | |
| POST `/api/tokens/:id/rotate` | `rotateToken` | 轮换返回新 key |
| POST `/api/tokens/:id/reset_used` | `resetTokenUsed` | 清零用量 |
| GET/POST `/api/channels` | `listChannels`/`addChannel` | Channel 全字段 |
| PUT/PATCH/DELETE `/api/channels/:id` | `updateChannel`/`patchChannel`/`deleteChannel` | |
| POST `/api/channels/:id/test` | `testChannel` | 连通性 `{latency_ms}` |
| POST `/api/channels/:id/reset-circuit` | `resetChannelCircuit` | 断路器重置 |
| POST `/api/channels/fetch_models` | `fetchChannelModels` | 拉上游模型列表 |
| POST `/api/channels/chat_test` | `testChannelChat` | 对话调试（SSE） |

**定价/分组/兑换码**
| 路径 | 前端方法 |
|------|---------|
| GET/POST `/api/prices` | `listPrices`/`upsertPrice` |
| DELETE `/api/prices/:model` | `deletePrice` |
| GET/PUT `/api/ratios` | `getRatios`/`updateRatios` |
| GET/POST `/api/groups` | `listGroups`/`upsertGroup` |
| DELETE `/api/groups/:name` | `deleteGroup` |
| GET `/api/redemptions` | `listRedemptions` |
| POST `/api/redemptions/batch` | `batchRedemptions` |
| POST `/api/redemptions/redeem` | `redeem` |
| DELETE `/api/redemptions/:id` | `deleteRedemption` |

**财务/限流/日志**
| 路径 | 前端方法 |
|------|---------|
| GET/PUT `/api/epay/config` | `getEpayConfig`/`updateEpayConfig` |
| GET `/api/orders` `/api/orders/me` | `listOrders`/`myOrders` |
| POST `/api/topup` | `topup` |
| GET/PUT `/api/limits` `/api/ratelimit/config` | `getLimits`/`updateLimits`/`getRateLimitConfig`/`updateRateLimitConfig` |
| GET `/api/logs/requests` `/api/logs/audits` | `listRequestLogs`/`listAuditLogs` |
| GET `/api/logs/requests/export` | 导出 CSV/JSON |

**仪表盘/监控/通知/安全**
| 路径 | 前端方法 | 响应字段 |
|------|---------|---------|
| GET `/api/usage/summary` `/api/usage/trend` `/api/usage/models` | `getUsageSummary`/`getTrend` | |
| GET `/api/tokens/today` | `getTodayTokens` | |
| GET `/api/dashboard/*` | `getConsumptionTrend`/`getModelDistribution`/`getUserRanking`/`getChannelHealth`/`getRealtime` | channel_health 含 `circuit_breaker: open/halfopen/closed` |
| GET `/api/monitor/system` | `getSystemMonitor` | `{cpu,memory,load,process}` |
| GET `/api/monitor/security` | `getSecurityOverview` | `{total_events,critical_events,recent_24h,score,sparkline}` |
| GET `/api/monitor/security/events` | `getSecurityEvents` | `{events,total,page,page_size}`，事件含 `event_type/severity/ip/detail` |
| GET/PUT `/api/notify/config` | `getNotifyConfig`/`updateNotifyConfig` | 含 telegram/smtp/slack/webhook |
| POST `/api/notify/test-*` | `testTelegram`/`testEmail`/`testSlack`/`testWebhook` | |
| GET/PUT `/api/alerts/rules` | `getAlertRules`/`updateAlertRules` | 8 种告警类型 |
| GET `/api/alerts/active` | `getActiveAlerts` | |
| GET `/api/alerts/history` | `getAlertHistory` | `{total,items}` |
| POST `/api/alerts/test` | `testAlert` | `{triggered,message}` |
| GET `/api/cache/stats` / POST `/api/cache/clear` | `getCacheStats`/`clearCache` | |

**IP / 价格同步 / 汇率**（⚠️ 2026-09-05 补全，此前前端调了后端 404）
| 路径 | 前端方法 | 说明 |
|------|---------|------|
| GET/PUT `/api/ip/filter` | `getIpFilter`/`updateIpFilter` | `{enabled,whitelist,blacklist}` |
| POST/DELETE `/api/ip/whitelist[/:pattern]` | `addWhitelist`/`removeWhitelist` | |
| POST/DELETE `/api/ip/blacklist[/:pattern]` | `addBlacklist`/`removeBlacklist` | |
| GET/PUT `/api/pricing/sync-config` | `getPriceSyncConfig`/`updatePriceSyncConfig` | `{enabled,sync_url,interval_secs,last_sync}` |
| POST `/api/pricing/sync` | `triggerPriceSync` | `{models_synced,errors,source}` |
| GET/PUT `/api/pricing/exchange-rates` | `getExchangeRates`/`updateExchangeRates` | 扁平 `{CNY:7.2,...}`，USD 恒 1.0 |

---

## 5. 核心业务流程

### 5.1 数据面请求（chat completions）
```
下游请求 → extract_api_key → 全局 IP 过滤(ip::check_ip)
→ verify_api_key_full（ApiKey 校验: 状态/过期/模型白名单/额度/IP）
→ 限流检查（RPM/TPM）→ check_group_model_permission（分组模型权限）
→ ensure_model_priced → 亲和 session 提取(body.user)
→ resolve_bridges_with_affinity（priority/weight + 断路器过滤 + 亲和置顶）
→ failover 循环（record_channel_success/failure 记入断路器+健康+亲和）
→ SSE 输出 → 计费 finalize（usage 累计 + charge_usage + 请求日志）
→ 断连兜底（StreamUsageGuard drop 时计费）
```

### 5.2 计费公式（`pricing/mod.rs`）
```
cost = (input_tokens*input_price + output_tokens*output_price)/1000
       * model_ratio(model) * group_ratio(group)
```
- 工具调用附加费：`calculate_tool_surcharge`
- 缓存命中：**不重复扣费**，但写 `channel_id="cache"` 日志留痕（费用 0）

### 5.3 渠道调度增强
- **断路器**：三态 Closed/Open/HalfOpen，阈值 5、冷却 30s；AuthFailed/PaymentRequired 强制 30min 冷却；429 记 retry_after
- **亲和**：(session,model) 双 TTL 粘性路由
- **健康追踪**：per-channel/model 错误率 + 延迟 EMA(α=0.2)
- **探活**：`prober.rs` 每 300s 发 1-token 探测（Open 渠道跳过）

### 5.4 告警巡检
```
alert_patrol(60s) → 断路器 open/延迟 EMA/内存 → AlertRuleEvaluator.evaluate
→ 静默期判定 → 分发 Telegram/Email/Slack/Webhook → 历史落盘(500条)
```

---

## 6. 维护者指南

### 6.1 编译与验证（⚠️ 重要）
- **本地 `cargo check` 无法完整跑通**：Windows 下 `link.exe` 被 Git 自带 `/usr/bin/link.exe` 抢占，且未装 MSVC Build Tools。
- **源码级验证**：`cargo check --offline 2>&1 | grep -E "error\[E"` 为空即源码 OK（link 阶段失败可忽略）。
- **权威验证 = 远程 CI**：所有编译/clippy/fmt/test 结论以 GitHub Actions 为准。

### 6.2 提交流程
```
编辑 → cargo fmt → cargo check --offline(源码级) → 前端 npm run build
→ git push https://ojbkxc:<token>@github.com/ojbkxc/AIGX.git main
→ 查询 CI: GET /repos/ojbkxc/AIGX/actions/runs?per_page=1
```

### 6.3 已知坑
1. **rustfmt 断行**取决于上下文宽度，修 fmt 必须按 CI 日志 `+` 行逐字符对齐。
2. **MutexGuard 跨 await**：`std::sync::Mutex` 的 guard 非 Send，跨 await 会导致 future 非 Send（tokio::spawn / axum Handler 报错）。跨 await 持锁必须用 `tokio::sync::Mutex`。
3. **`mod ip` 未在 main.rs 声明**曾导致 `unresolved import crate::ip`——新增模块要同时加 lib.rs 和 main.rs 两处声明。
4. **断路器状态契约**：`get_state()` 返回机器可读 `open/halfopen/closed`，前端 lowercase 后比较；人类可读用 `get_status_human()`。

---

## 7. UI 优化方案清单（待办，按优先级）

### P0 — 功能断裂已修复（本次）
- [x] IP 管理页后端 6 个端点补齐 + 数据面拦截
- [x] 价格同步/汇率后端 5 个端点补齐
- [x] 安全事件路径 `/api/security/events` → `/api/monitor/security/events`

### P1 — 建议优化
- [x] **Dashboard 断路器状态**：后端已返回机器可读 `circuit_breaker`，前端 `Dashboard.jsx` 的 `breakerState` 逻辑可简化为直接读该字段（当前兼容多字段但逻辑冗余）
- [x] **Settings 价格同步**：`enabled/sync_url/interval_secs` 与后端 `PriceSyncConfig` 字段对齐后，保存后应立即 `loadPriceSyncConfig()` 刷新 `last_sync` 展示
- [x] **汇率配置**：前端 `Object.keys(exchangeRates)` 需处理 USD 基准（后端返回含 `USD:1.0`），建议前端隐藏/禁用 USD 输入
- [x] **安全事件筛选**：`timeRange` 与 `eventType` 变化后，前端 `loadEvents` 的 useEffect 依赖未包含这两个字段（需显式触发），交互略不跟手

### P2 — 长期增强
- [x] 全局搜索（Ctrl+K 命令面板：页面导航 + 渠道/用户/令牌跨实体检索，角色感知）
- [x] 日志时间线可视化（请求日志按小时聚合成功/失败柱状图，表格/时间线视图切换）
- [x] Playground 增加系统提示词模板、参数预设
- [x] 移动端响应式适配（9 个核心页面 390px 视口无横向溢出，抽屉导航）

---

## 8. 数据面端点与 handler 位置速查

| 端点 | 位置 |
|------|------|
| chat/completions | `src/api/openai.rs:884` |
| responses | `src/api/openai.rs:1540` |
| completions/embeddings/images/audio | `src/api/openai.rs` 各处 |
| anthropic messages | `src/api/anthropic.rs:214` |
| 全部管理 handler | `src/api/admin.rs`（99 个） |
| 路由注册 | `src/main.rs:439 build_router` |
