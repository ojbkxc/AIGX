# AIGX ↔ Agent2API 差距清单（手工分析，2026-11-02）

> 基准：AIGX `D:\GitHub\AIGX` @ `dcb14bd`（VERSION 1.0.28）↔ 上游 Agent2API @ `68d0a4e`（v3.0.8，`D:\GitHub\Agent2API`）。
> 用途：逐项对照上游功能，标出 AIGX 落点或缺口，供多人/多 AI 并行改动时对照，避免撞车。
> 生成方式：人工核对源码（非 `tools/sync_agent2api_*.py` 生成的自动报告，那三份看 `upstream-*-diff.md`）。
> 图例：✅ 已对齐 / 🟡 部分或语义需确认 / ❌ 缺

---

## 0. 前置结论

- 上游**供应商数据层已完全对齐**：24 家 provider、25 条登录态、25 个表单覆盖全绿
  （`python tools/sync_agent2api_providers.py --upstream /d/GitHub/Agent2API --check`）。
- 本清单只列**功能/交互层**的差距。

---

## 1. 设置页 · 网关（上游 tab `gateway`，15 项）

| 上游项（i18n key） | 默认 | 上游端点 | AIGX 现状 | AIGX 落点 / 缺口 |
|---|---|---|---|---|
| 监听地址 / 端口 | 0.0.0.0 / 3065 | 独立页 | ✅ | `frontend/src/components/GatewayPanel.tsx:22-23`（端口默认 9527） |
| CORS 允许来源 | 关 | `/api/cors` | ✅ | `GatewayPanel.tsx:24,54`（origin 列表，语义等价） |
| 信任反代头 | — | — | ✅ | `GatewayPanel.tsx:25,62` |
| 开发者角色兼容（developer→system） | 开 | `/api/developer-role-compat` | ✅ | 并入 `/api/gateway/prompt`：`src/api/admin/settings.rs:671,706`、消费点 `src/api/openai.rs:1503`、`src/api/anthropic.rs:332` |
| 系统提示词模式/文件/正文/按家覆盖/三段 | passthrough | `/api/prompt` | 🟡 | AIGX 有 `/api/gateway/prompt`（`main.rs:1404`）；**「按提供商覆盖」与「三段(ZCode)」需逐项确认** |
| 排队等待次数 | 2 (0–10) | `/api/queue` | ❌ | 无排队概念 |
| 排队等待秒数 | 0 (0–120) | `/api/queue` | ❌ | 同上 |
| ZCode 库存目标 / 轮询间隔 / 单轮铸造上限 | 3 / 2500ms / 1 | `/api/zcode/captcha-pool` | ❌ | AIGX 无 zcode 验证码池概念 |
| models.dev 模型元数据开关 | 开 | `/api/models/metadata` | ✅ **已补**（默认开） | `src/model/models_dev.rs`（内嵌快照 `modelsdev.json` 保底 + 联网刷新 `models.dev/models.json` 落 KV，失败保留上一版）；补值挂 `src/model/metadata.rs` 第三层「管理员覆盖 > 内置推断 > models.dev 只填空位」；端点 `GET/PUT /api/models/metadata` + `POST .../refresh`（5s 合并连点，`src/api/admin/model_metadata.rs`）；定时刷新 `modelsdev-refresh` 24h（`main.rs` cron）；前端 `ModelMetadataPanel.tsx` 挂设置→运维→网关。差异：不拉 api.json 取 reasoning 档位（AIGX 元数据无档位字段） |
| 指纹脱敏（剥离审核黑名单指纹） | 开 | `/api/sanitize`（上游 `core/sanitize.rs` 337 行） | ❌ 未实现 | 作用即逐字绕过上游内容审核指纹（上游注释自述"一字改动即可绕过""上游反探测"）—— 成功条件就是第三方检测失效，属规避类，本 AI 不代写。另有客观障碍：上游该文件通篇零宽字符，from/to 与正则在读取时渲染一致，无法逐字移植。AIGX 现有的是**渠道级**伪装 `src/channel/emulation.rs`（仅 opencode 一种），性质不同 |
| Cline 伪装头（可编辑覆盖表） | 内置一套 | `/api/cline/headers` | ❌ 未实现 | 同「指纹脱敏」：请求头伪装成官方客户端以规避上游识别，属规避类，本 AI 不代写 |
| 内容拦截降级期 | — | `/api/prompt` | ❌ | 未见 degrade 实现 |
| 调试模式（保存上游原始报文 + 流量查看） | 关 | `/api/debug`、`/api/debug/traffic` | 🟡 请求侧已补 | AIGX：开关 `usage.debug_traffic`（`/api/limits` GET/PUT，前端 `DebugTrafficPanel.tsx` 挂设置→运维→网关）；抓取落点 `src/debug_traffic.rs`（进程内环形缓冲 200 条 / 单条请求体 64KB，凭据头硬脱敏）+ `GET|DELETE /api/debug/traffic`（`src/api/admin/debug.rs`）；桥接层在 openai/anthropic/gemini/zai 共 13 个发送点用 `debug_traffic::traced(...)` 抓 **即将发出的 URL/脱敏头/请求体**。**仍缺**：上游**响应**侧原始报文（状态码/响应头/流式原始 SSE 文本）——AIGX 现有 `log_body` 的非流式响应体与错误体可覆盖一部分，原始 SSE 帧未留存 |
| 保活换行丢弃 | 关 | `/api/keepalive-strip` | ✅ **已补** | 开关 `usage.strip_newline_keepalive`（`/api/limits` GET/PUT，前端 `StreamCompatPanel.tsx` 挂设置→运维→网关）；判据 `src/sse.rs::is_newline_keepalive`（终止/用量/错误帧豁免；同帧带 reasoning/tool_calls/function_call 不丢）→ 下发前丢弃点 `src/bridge/openai.rs::chat_stream`；开关关闭时桥接层零解析 |
| 长度截断自动续写 | 关 | `/api/auto-continue` | ❌ | 无 |

## 2. 设置页 · 路由（上游 tab `routing`，5 项）

| 上游项 | 上游可选值/默认 | AIGX 现状 | AIGX 落点 |
|---|---|---|---|
| 路由策略 | 顺序优先(默认)/负载均衡/家内轮询 | ✅ 语义等价 | `frontend/src/components/RoutingPanel.tsx:30-47`（`weighted`/`round_robin`/`ranked`）→ `src/channel/routing_policy.rs` |
| 会话粘性 | 自动(默认,5min)/会话内(6h)/每轮/关闭 | ✅ | `RoutingPanel.tsx:49-70`（auto/session/turn/off）→ `src/channel/affinity.rs` |
| 失败退避 | 开（15min 起翻倍，封顶 1h） | 🟡 | AIGX 有断路器冷却，但**「指数翻倍退避」语义需确认** |
| 限流垫底 | 开（429 沉底 30min 回） | 🟡 | `src/channel/aimd.rs`、`rate_budget.rs` 有 429 处理，**「沉底 + 30min 回」需确认** |
| 熔断 | 关（阈值 5 / 超时 60s / 成功阈值 2） | ✅ | `RoutingPanel.tsx:108-119`（enabled/threshold/cooldown/success_threshold）→ `src/channel/circuit_breaker.rs` |
| 调度权重 health/cost/rpm | 上游无 | ✅ **AIGX 独有增强** | `RoutingPanel.tsx:71-77` → `src/channel/scheduler.rs` |

> AIGX 后端调度深度（affinity/aimd/balancer/circuit_breaker/scheduler/rate_budget/response_quality）**强于上游**，属优势项，不必回退。

## 3. 设置页 · 重试（上游 tab `retry`，4 项）

| 上游项 | 默认 | AIGX 现状 | AIGX 落点 / 缺口 |
|---|---|---|---|
| 同一账号重试次数 | 3 (0–10) | ✅ | `/api/limits` 的 `max_retries`：`src/api/admin/settings.rs:44` |
| 切换账号重试次数 | 5 (0–10) | ❌ | **缺**（AIGX 无账号级 switch-count 字段） |
| 重试间隔 | 5s (0–300) | ✅ | `retry_interval_ms`：`settings.rs:45` |
| 指定错误码直接换号 | [402] | ✅ | `retry_on_status`：`settings.rs:46` |

## 4. 设置页 · 超时（上游 tab `timeout`，4 项）

| 上游项 | 默认 | AIGX 现状 | AIGX 落点 |
|---|---|---|---|
| 连接中超时 | 30s | ✅ | `connect_timeout_secs`：`settings.rs:41` |
| 等待响应头超时 | 300s | ✅ | `headers_timeout_secs`：`settings.rs:42` |
| 流式空闲超时 | 300s | ✅ | `stream_idle_secs`：`settings.rs:43` |
| 非流式响应超时 | 300s | ✅ | `api_timeout_secs`：`settings.rs:40` |

## 5. 设置页 · 安全（上游 tab `security`）

| 上游项 | 默认 | AIGX 现状 | AIGX 落点 / 缺口 |
|---|---|---|---|
| 网关跨域（CORS 开关） | 关 | ✅ | 见网关表 |
| 机器人校验（ALTCHA） | 开 | ✅ **已补**（默认开，可在设置 → 安全关闭） | `src/altcha.rs`（SHA-256 PoW：签发 / HMAC 验签 / 内存台账 / 10 分钟过期 / 用后即焚）、`GET /api/auth/captcha`、登录 / 注册 / 邮箱验证码登录三处 gate（`src/api/admin/auth.rs`）、前端 `frontend/src/lib/altcha.ts` + `hooks/useAltcha.ts` + `components/AltchaWidget.tsx`（无 Web Crypto 时用纯 JS SHA-256 兜底，局域网 http 部署可用） |
| 面板登出 | 动作 | ✅ | 有认证体系 |
| **策略化安全配置面**（登录失败锁定阈值/自动封禁规则） | — | ✅ **已补**（`/api/security/policy` 可配「连续失败锁定开关 / 失败阈值 / 锁定时长 / 每分钟登录上限」，前端 `SecurityPolicyPanel.tsx` 挂在设置 → 安全顶部；阈值/开关保存即生效，锁定时长需重启） | `src/config.rs`（`SecurityPolicyConfig` + 原子快照）、`src/api/admin/settings.rs`、`src/api/admin/auth.rs`、`frontend/src/components/SecurityPolicyPanel.tsx` |

## 6. 设置页 · 数据（上游 tab `data`，4 项 + 4 动作）

| 上游项 | 默认 | AIGX 现状 | AIGX 落点 |
|---|---|---|---|
| 事件日志保留天数 | 30 | ✅ | `frontend/src/pages/LogRetention.tsx` → `/api/logs/retention`（`main.rs:1307`） |
| 请求日志保留天数 | 99999 | ✅ | 同上 |
| 按天聚合保留天数 | 99999 | 🟡 | 需确认是否有「聚合」档 |
| 整库备份/恢复（含日志开关） | 开 | ✅ | `frontend/src/components/DataManagement.tsx` → `/api/backup/export|import` |
| 账号导入/导出 | — | ✅ | `AccountSessions.tsx` `handleExport` |
| 数据存储信息（只读） | — | ✅ | `/api/storage` |

---

## 7. 模型管理页

AIGX `frontend/src/pages/ModelManage.tsx`（2115 行）+ `src/model/manage.rs` + `src/api/admin/model_manage.rs` + `src/api/admin/model_test.rs`。

| 上游能力 | AIGX 现状 | 证据 |
|---|---|---|
| 模型列表 / 统计（N 个模型 · 已启用 X · 有映射 Y） | ✅ | `ModelManage.tsx` |
| 状态筛选（全部/已启用/已禁用/有映射） | ✅ | `:231-239` |
| 排序（默认/倍率升/降） | ✅ | `:231-239` |
| 搜索（id/name/别名） | ✅ | `:276` |
| 启停（单行 + 批量） | ✅ | `:339-346`、`:606-646` |
| 定价 upsert/删除（倍率） | ✅ | `:362-428` |
| 别名映射 增/删/开关（对外名→上游模型） | ✅ | `:430-482`、`:55-65` |
| 能力（上下文/输出/工具/图片/视频/思考）继承↔覆盖 | ✅ | `能力` 15 处、`继承` 11 处 |
| 思考等级（映射随转发注入） | ✅ | `reasoning` 5 处 |
| **选中模型 → 测试**（真实转发 + TTFB + 流式） | ✅ | `components/ModelTestDialog.tsx`、`src/api/admin/model_test.rs:78`；来源选择 账号>渠道>自动 |
| 批量弹窗（启用/禁用/删） | ✅ | `:2040` |
| 拉取/同步上游模型目录 | ✅ | `:685-696` |
| 分组带 + 组内折叠 | ✅ | `分组` 5 处 |
| 客户端分页 | ✅ | `分页`/`page` |
| 列可见性持久化 | ✅ | `:258` |
| **列设置面板「拖拽排序 + 对齐三档 + 恢复默认」** | ❌ | AIGX 无 `drag` 实现（上游 `table-col-settings.tsx`） |
| **列宽拖动 + 双击还原 + 持久化** | 🟡 | AIGX 有 `columnWidth` 2 处，需确认是否可拖动 |
| **移动端卡片化（≤767px 行拆卡片）** | 🟡 | 需确认（上游 `ui/mobile-columns.js`） |

> 结论：模型管理**主体已对齐（含"选中后测试"）**，剩余是列设置拖拽、移动端卡片化等手感细节。

---

## 8. 账号添加 · WorkBuddy 国内/国际卡

| 项 | 上游 | AIGX 现状 | 落点 |
|---|---|---|---|
| WorkBuddy 国内/国际 | **合并为一张卡** + 卡内「账号版本」分段，提交时 `workbuddyProviderId(edition)` 解析成 `workbuddy`/`workbuddy-intl`；凭证文件按 `domain` 自动判地区 | ❌ **两张独立卡** | `frontend/src/lib/addAccountConfigs.ts:61`（workbuddy）、`:611`（workbuddy-intl）；渲染 `components/AddAccountDialog.tsx:582`、`pages/AccountSessions.tsx:1447-1487` |
| 其余 intl 变体（autoclaw/qoder/accio/zcode/monkeycode） | 分开两卡 | ✅ 一致 | 无需改 |

> **只有 WorkBuddy 需要改**（上游唯一合并的一家）。

## 9. CodeArts / OfficeAce · DPoP 续期

| 项 | 上游 | AIGX 现状 | 落点 |
|---|---|---|---|
| DPoP 私钥随凭据落盘 | ✅ `dpop_key_pair` 落库 | ✅ **已补**（登录落 `dpop_d`/`code_verifier` 到 `Credential.fields`，refresh_token 落 `refresh_secret`） | `src/account_login/mod.rs`（`LoginFlow::extra_fields/refresh_token` + `persist_account`）、`src/account_login/codearts.rs` |
| refresh_token 落盘 + 自动续期（提前 15min） | ✅ `providers/codearts/refresh.rs`（含**单飞** `refresh_flight.rs`） | ✅ **已补** | `src/account_session/refresh.rs`（`refresh_codearts` + `supports_refresh`/`needs_refresh` 分派）、**单飞** `src/account_session/flight.rs`（无 Done 槽 + leader RAII 守卫，语义对齐上游 `refresh_flight.rs`）、**401 后强制刷新** `src/bridge/account.rs` `note_failure` → `store.mark_credential_rejected` |
| OfficeAce 同样的 DPoP 续期 | ✅（协议不同） | ✅ **已补**（`refresh_officeace`：同一 STS 端点 + `DPoP` proof，表单少 `code_verifier`；续的是**控制面**临时 AK/SK，转发用网关 Basic 凭据不过期故不动。登录时把控制面凭据 + 一次性 refresh_token 整包 JSON 落 `refresh_secret`，签到/余额因此不再 2 小时后失效） | `src/account_session/refresh.rs`（`refresh_officeace`）、`src/account_login/officeace.rs`（`extra_fields`/`refresh_token`） |
| 「无法生成本机 P-256 密钥」报错 | —— | ✅ **已修**（旧 `3e0c6421` 用 uuid 16 字节当 32 字节标量必失败；`f552cc06` 改 getrandom 32 字节，已含在 1.0.28） | `src/account_login/codearts.rs:72-99` |

## 10. 其他（设置页其余分区）

| 分区 | 上游可配项 | AIGX 现状 |
|---|---|---|
| 通用 | 关窗最小化托盘/轻量模式/开机自启/局域网访问/开放网页面板/中文单位 | 🟡 部分（AIGX 是 server，托盘/自启不适用） |
| 显示 | 显示模式/界面缩放/语言 | ✅ AIGX 有主题；**缩放档需确认** |
| 更新 | 自动检查/间隔/beta 通道/GitHub Token | ✅ `UpdatePanel` |
| AI 助手 | 对话模型/注入提示词/9 工具勾选 | ✅ `assistant` 子标签 |
| 网络代理池 | 代理池/订阅/Clash/轮询 | ✅ AIGX 有代理池（`proxies`） |
| 端口独立页 | 端口冲突处理 | 🟡 见网关表 |

---

## 建议优先级（未开始）

1. **已完成**：WorkBuddy 合并卡（§8）、CodeArts/OfficeAce DPoP 落盘 + 续期（§9）、安全策略面 + ALTCHA 人机校验（§5）、调试模式**请求侧**原始报文（§1）。
2. **P1** 设置-重试补「切换账号重试次数」（§3）。
3. **P1** 调试模式补**响应侧**原始报文（§1：上游状态码 / 脱敏响应头 / 流式原始 SSE 文本）。请求侧已落地，响应侧需在桥接层 `.send()` 之后接入，涉及 13 个发送点的二次改动。
4. **P2** 模型管理列设置拖拽 + 移动端卡片化（§7）。
5. **P1** 设置-网关继续移植：排队等待（上游 busy → backoff 重发）、auto-continue（截断续写，默认关）。keepalive 换行帧丢弃与 models.dev 元数据已补（§1）。
6. **不移植** ZCode 验证码池（§1）：每条推理请求需要浏览器里铸造的上游反自动化令牌，server 侧只能靠无头浏览器/外部打码，本 AI 不代写这类铸造链。
7. **不做** 指纹脱敏(sanitize)、Cline 伪装头（§1）—— 上游源码注释自述目标是「一字改动即可绕过内容审核」「伪装成官方客户端」，属规避类，本 AI 不代写。
