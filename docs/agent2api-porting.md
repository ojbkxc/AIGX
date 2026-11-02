# Agent2API → AIGX 移植对照手册

> 本文档记录从 [aimod-cc/Agent2API](https://github.com/aimod-cc/Agent2API)（本地副本 `C:\GitHub\Agent2API-main`）移植到 AIGX 的功能与代码映射，以及后续跟随上游更新的手工同步方法。
> 上游是独立仓库（Tauri/Rust 混合架构，无 Rust 可直接 git 合并），同步只能按本表逐文件手工移植。

## 已移植功能

### 1. 统一模型管理（Agent2API models-page）

| AIGX | Agent2API 蓝本 |
|---|---|
| `src/model/manage.rs`（ModelRules 启停规则 + build_view 聚合） | `core/model_rules/mod.rs` + `core/providers/catalog/view.rs` |
| `src/api/admin/model_manage.rs`（`/api/models/manage` + state/price/alias 写接口） | `server/src/server/api/model_manage.rs` |
| `frontend/src/pages/ModelManage.tsx` | `ui-islands/src/islands/models-page.tsx` |
| 前端 API/类型：`frontend/src/api/index.ts`（listManageModels 等）、`frontend/src/types/index.ts`（ManageView） | `models-panel-state.ts` 等 |

差异：AIGX 聚合粒度为 `(provider, model_id)`，数据源是渠道模型 + 账号池模型（Agent2API 是各家 provider manifest）；定价走 AIGX 自己的 `pricing::ModelPrice`（Agent2API 的 rate 只是上游目录展示，不计费）。

### 2. 签到中心（Agent2API checkin-page）

| AIGX | Agent2API 蓝本 |
|---|---|
| `src/checkin/store.rs`（台账按日分桶 KV + 计划 + 设置） | `checkin_records` 表（schema v11）+ kv `autoCheckin.schedules` |
| `src/checkin/claim.rs`（各 provider 签到执行体，白名单制） | `core/billing/checkin.rs` |
| `src/checkin/scheduler.rs`（30s 轮询、UTC+8、当天去重、启动补签） | `core/auto_checkin.rs` |
| `src/api/admin/checkin_center.rs`（`/api/checkin-center/*`） | `api/checkin_{center,records,schedules}.rs` |
| `frontend/src/pages/CheckinCenter.tsx`（center/schedule/records 三 tab） | `ui-islands/src/islands/checkin-page.tsx` |

**claim 白名单**（`src/checkin/claim.rs`，12 家全覆盖）：`loomy / kuku / qoder / qoder-intl / xiaohuanxiong / autoclaw / autoclaw-intl / workbuddy / workbuddy-intl / trae / codearts / officeace`。
未接入家（zcode / catpaw / accio 等）：上游没有每日签到通道。**扩展方法**：在 `claim.rs` 加分支 + `CHECKIN_PROVIDERS` 白名单加名，判据翻译成统一 `ClaimResult{success, msg, outcome}`。注意：officeace 签到需要临时 AK/SK（`refresh_secret` JSON 或 `ak:sk:sts`），web 登录流程暂未留存，缺凭据时报错指引重登。

注意区分：`/api/checkin`（用户签到送配额，`src/user/checkin.rs`）与 `/api/checkin-center`（管理员替上游账号签到）是两套并存系统。

### 3. 报表并入仪表盘（Agent2API report-page）

| AIGX | Agent2API 蓝本 |
|---|---|
| `src/api/admin/dashboard.rs` 尾部 `handle_report_{heatmap,top_sources,token_trend,overview}` | `api/stats_api.rs` + `request_stats/report.rs` |
| `frontend/src/pages/Dashboard.tsx`（Heatmap/TokenTrendChart 组件 + Top 渠道/模型卡 + 概览卡） | `report-page.tsx` + `report-charts.ts` |

差异：AIGX 日志无 provider/账号维度，Top 提供商/Top 账号统一为「Top 渠道」（`channel_name`）；权限走 `verify_user` + `is_admin_user` 收敛（普通用户只见自己的 `user_id` 数据）。

### 4. 定价倍率表格化

`frontend/src/pages/Pricing.tsx`：主视图表格（模型名/倍率/增删行）+ JSON 高级模式折叠项。后端零改动（`pricing::RatioConfig` 的 HashMap 结构天然适配），费用公式 `基础费用 × model_ratio × group_ratio`（`src/pricing/mod.rs` `calculate_cost`）不变。

### 5. 安全策略面（Agent2API 设置 → 安全）

| AIGX | Agent2API 蓝本 |
|---|---|
| `src/config.rs`（`SecurityPolicyConfig` + `[security]` 段 + 进程级原子快照） | 上游 `config/types.rs` 的安全项 |
| `src/api/admin/settings.rs`（`GET/PUT /api/security/policy`） | 上游 `api/security*.rs` |
| `frontend/src/components/SecurityPolicyPanel.tsx`（设置 → 安全顶部） | `ui-islands/.../settings-page.tsx` 的安全 tab |

把原先硬编码在 `src/api/admin/auth.rs`（阈值 5 / 限流 10）与 `main.rs`（锁定 TTL 300s）的三个门槛抽成可配项：连续失败锁定开关 / 失败阈值 / 锁定时长 / 每分钟登录上限。默认值与原硬编码一致，行为不变。阈值/开关走原子快照保存即生效；锁定时长是失败计数缓存 TTL，改后需重启（与网关监听地址同口径）。ALTCHA 机器人校验见下一节（同一面板内的开关）。

### 6. 调试模式 · 上游原始报文（Agent2API 设置 → 网关 → 调试模式）

| AIGX | Agent2API 蓝本 |
|---|---|
| `src/debug_traffic.rs`（进程内环形缓冲 + 凭据头硬脱敏 + `traced()` 构造器包装） | `core/debug_traffic.rs`（独立 `debug_traffic` 表） |
| `src/api/admin/debug.rs`（`GET/DELETE /api/debug/traffic`） | `api::debug_api` |
| 开关 `usage.debug_traffic`：`/api/limits` GET/PUT + main 启动快照 | config `debugMode` |
| `frontend/src/components/DebugTrafficPanel.tsx`（设置 → 运维 → 网关） | `ui/requests-panel.js` 调试详情 |
| 桥接层抓取点：`src/bridge/{openai,anthropic,gemini,zai}.rs` 的 13 个 `.send()` 前 | 各家适配器 `build_chat_request` |

差异与取舍：

- **只抓请求侧**（URL / 脱敏后的请求头 / 最终请求体），落**进程内环形缓冲**（200 条、单条 64KB）而非数据表：请求侧体积小，省掉一次 schema 迁移；代价是重启即清空（调试模式本就是临时排障）。
- **响应侧**（状态码 / 响应头 / 流式原始 SSE 文本）尚未移植——需要再动 13 个发送点在 `.send()` 之后接入。非流式响应体与错误体已由 `log_body` 的 `LogDebugSnapshot.response_body` 覆盖一部分。
- 脱敏为**硬要求**：`authorization` / `cookie` / `x-api-key` / 各家自定义 token 头一律 `[redacted]`（名单见 `src/debug_traffic.rs` 的 `SENSITIVE_HEADERS`）。
- 关闭开关时桥接层零开销（`traced()` 先判原子快照，不做 `try_clone`/`build`）；关闭时顺手清空缓冲。

`src/account_session/registry.rs` 的 `AGENT2API_PROVIDERS` 已对齐上游 24 家全名单（workbuddy/-intl、raccoon、catpaw、autoclaw/-intl、qoder/-intl、cline-free/pass、accio/-cn、zcode/-intl、codearts、trae、loomy、kuku、monkeycode/-intl、commandcode、antigravity、officeace、orcaterm）。

### 7. 机器人校验 · ALTCHA 工作量证明（Agent2API 设置 → 安全 → 机器人校验）

| AIGX | Agent2API 蓝本 |
|---|---|
| `src/altcha.rs`（SHA-256 PoW：签发题面、HMAC 验签、内存台账、10 分钟过期、用后即焚） | `server/src/server/altcha.rs` |
| `GET /api/auth/captcha`（公开端点；开关关闭回 400） | 上游同款领题路由 |
| `src/api/admin/auth.rs` 三处 gate：`handle_login` / `handle_register` / `handle_login_with_code`，均放在限流/失败计数**之前** | 上游登录 / 注册前置校验 |
| 开关 `security.captcha_enabled`（默认**开**）：`/api/security/policy` GET/PUT + main 启动快照 | config `security.captchaEnabled` |
| `frontend/src/lib/altcha.ts`（求解器，含纯 JS SHA-256 兜底） | `ui/panel-captcha.js` |
| `frontend/src/hooks/useAltcha.ts` + `components/AltchaWidget.tsx`（登录 / 注册页共用） | 上游 widget |

设计要点：

- **真人无感**：页面挂载即领题并在后台枚举 `sha256(salt + n)`，答案算完前提交按钮只会提示「正在完成机器人校验」。
- **不依赖安全上下文**：官方 altcha widget 在 `http://<内网IP>` 下会因 `crypto.subtle` 缺失直接失败；本实现检测不到原生 Web Crypto 时改用内置纯 JS SHA-256（数学完全一致，服务端不区分），局域网 http 部署可用。
- **用后即焚**：`verify` 成功即从台账删除，同一 payload 不能换第二个密码；因此前端每次提交后（无论成败）都重新领题。
- **防伪造**：台账只认本服务器签发过的 challenge，`signature = HMAC-SHA256(secret, challenge)`，secret 首次生成后落 KV（`auth.altcha_secret`），从不外发。
- **成本可控**：`maxnumber` 随机 5–10 万，浏览器百毫秒级完成；脚本每试一次都要重解一道题。

## 后续手工同步上游的方法

1. 拉上游最新代码：`git -C C:\GitHub\Agent2API-main pull`（或重新下载 zip）。
2. 关注三个目录的变更：
   - `server/src/core/providers/<name>/` — provider 协议/端点变更 → 对照 `src/account_session/registry.rs` 与 `src/bridge/`
   - `server/src/core/billing/checkin.rs` — 签到接口变更/新增家 → 移植到 `src/checkin/claim.rs`
   - `server/src/core/providers/mod.rs` 的 `PROVIDERS` — 新增 provider → 同步 `AGENT2API_PROVIDERS` + registry 条目
3. 登录态型 provider 需同步确认 `src/account_login/` 有登录实现，否则账号加不进来。
4. 移植后本地 `cd frontend && npm run build` 验证前端，Rust 侧直接 push 走 CI 编译验证（本机不编译）。

## 自动化数据层同步（工具脚本）

手工核对容易漏，数据层（provider id / 登录态条目 / 表单覆盖）的漂移检测已工具化：

```bash
# 检测漂移（有差异 exit 1，适合发版前 / 定期巡检）
python tools/sync_agent2api_providers.py --upstream /path/to/Agent2API --check

# 回写 registry.rs 的 AGENT2API_PROVIDERS 基准表（保守合并：保序、只追加缺项、不删多余项）
python tools/sync_agent2api_providers.py --upstream /path/to/Agent2API --write

# 生成 docs/upstream-providers-diff.md 漂移报告
python tools/sync_agent2api_providers.py --upstream /path/to/Agent2API --report
```

脚本比对四个口径：上游 `PROVIDERS` 注册表 ↔ `AGENT2API_PROVIDERS` 基准表、`SESSION_PROVIDERS` 登录态覆盖、`addAccountConfigs.ts` 表单覆盖（含 `Object.assign` 补的拆家项）。id 归一别名在脚本顶部 `ALIASES`（如 raccoon → xiaohuanxiong）。只比对标识符与常量，不拷贝上游实现代码（上游 LICENSE 禁商用/二次分发）。

能力层（协议/签名/风控）仍按上文逐文件手工移植 —— 上游 `providers/` 深度耦合其 `AccountStore`/`GatewayError`，无法整目录拷贝。

## 能力层移植路线图（账号池缺口）

按优先级（均为可选项，按需排期）：

| 优先 | 能力 | 状态 | 移植来源 | 落点 |
|---|---|---|---|---|
| 1 | 账号级 token 自动刷新 | **已接入**（qoder/autoclaw/trae/codearts/officeace） | 上游各家 `providers/*/refresh.rs` + `providers/refresh_flight.rs` | `src/account_session/refresh.rs` + `store.update_tokens_cas` + cron `account-session-refresh`；登录期 OAuth 上下文落盘见 `LoginFlow::extra_fields`/`refresh_token`；**单飞**见 `src/account_session/flight.rs`（一次性 refresh_token 必须并发收口）；上游 401 拒证 → `store.mark_credential_rejected` 排进续期队列（`src/bridge/account.rs` 的 `note_failure`） |
| 2 | 账号级余额/用量查询 | **已接入**（qoder/autoclaw/小浣熊/zcode） | 上游各家 `providers/*/balance.rs` | `src/account_session/balance.rs` + `GET /api/account-sessions/:id/balance`；账号池列表「余额」列待 UI |
| 3 | 账号级模型清单刷新 | **已接入**（catalog 目录发现） | 上游各家 `providers/*/models.rs` | `src/account_session/catalog.rs` + `POST /api/account-sessions/catalog/sync` |
| 4 | 签到覆盖面扩展 | **12/12 已接入** | 上游各家 `providers/*/checkin.rs` | `src/checkin/claim.rs`；凭据侧：officeace 签到依赖的临时 AK/SK 由登录期落盘 + `refresh_officeace` 自动续期维持，不再 2 小时后失效 |

新增登录式 provider 的完整清单：`AGENT2API_PROVIDERS` + `PROVIDERS` + `SESSION_PROVIDERS` + `addAccountConfigs.ts` 表单 + `account_login` 登录实现 + （可选）`checkin/claim.rs`。
