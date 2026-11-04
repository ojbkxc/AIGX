# AIGX ↔ Agent2API 审计补缺清单（2026-11-04）

> 来源：11 项对等审计（团队 aigx-parity-audit）+ 既有手工 gap 文档（`aigx-vs-agent2api-gap.md`，1.0.28 快照）。
> 基线：Agent2API v3.0.8（`D:\GitHub\Agent2API`）｜对象：AIGX v1.0.31（`D:\GitHub\AIGX`）。
> 图例：✅ 等价/更强 ｜ ⚠️ 有差距 ｜ ❌ 缺失 ｜ 🔧 建议补齐。
> 本清单只列**待补项**，已完成项见审计报告原文（已汇总进记忆 `project_aigx_agent2api_alignment.md`）。

---

## 0. 总览判定（11 项审计）

| 维度 | 判定 | 核心结论 |
|---|---|---|
| 账号渠道/登录态（#2） | ✅ 强于基线 | Provider 24/24 + 编译期断言；登录态 17 kind 刷新 |
| 计费/报表（#5） | ⚠️→✅ 分层错位 | 支付/订阅/用户/告警 ✅ 自足；用量报表 ⚠️ 明细字段少；积分流水/团队积分缺 |
| OpenAI 网关（#3） | ⚠️ | ❌ /v1/messages/count_tokens；⚠️ /v1/models 无 Anthropic 视图、超时/重试/队列无 HTTP 端点 |
| 管理端 API（#4） | ⚠️→✅ 局部 | 27✅/12⚠️/8❌/3🔧；❌ qoder_umid/assistant/机场订阅（zcode 池已补） |
| 出站净化（#6） | ✅ | 指纹脱敏已补全：chat / responses / completions 三路径挂载 + 命中统计（P2-10/P2-11） |
| 签到/定时（#7） | ⚠️ | ⚠️ workbuddy-intl 保活链不进调度、growth school/travel 未实现、Qoder UMID 只探测 |
| 配置/升级（#8） | ⚠️ | 核心开关对齐；⚠️ 定时任务默认全关、超时默认值不同；❌ routing 策略键/保留期配置 |
| 登录/验证码（#9） | ✅ | ALTCHA + 阿里云求解器/令牌池（P1-9/P1-10）+ AutoClaw OAuth（P2-1）均已补 |
| 存储/日志（#10） | ⚠️ | ❌ 运行日志页、debug_traffic 落库、account_usage_records；⚠️ 请求明细字段缺口 ~30% |
| UI 前端（#11） | ⚠️ | 12 面板 10✅/1⚠️/1❌（docs 页）；❌ 列定制/筛选记忆/移动端列集/i18n 扩展 |

---

## 1. P0 补缺（影响大 × 成本低，建议立即做）

| # | 项 | 来源 | 内容 | 落点 | 成本 | 影响 |
|---|---|---|---|---|---|---|
| P0-1 | **docs 文档页** | #11 ❌ | Base URL / 三种协议（OpenAI/Anthropic/Responses）/ 模型列表+复制 | `frontend/src/pages/Docs.tsx` + 路由 + 侧边栏 | 纯前端独立页 | 用户文档查阅 |
| P0-2 | **筛选记忆** | #11 ❌ | filter-memory localStorage 持久化（日志/请求/模型/渠道四页） | `src/lib/filterMemory.ts` + 四页接入 | 前端工具 + 四页小改 | 筛选条件持久化 |
| P0-3 | **BACKEND-GAP-MATRIX.md 更新** | #5 ⚠️ | 四处过时：预扣结算/订阅/TOTP/会话撤销均已实现，文档仍标「缺」 | `docs/BACKEND-GAP-MATRIX.md` | 纯文档 | 消除文档误导 |
| P0-4 | **workbuddy-intl 保活链进每日调度** | #7 🔧 首位 | claim.rs 的 workbuddy-intl 保活链接入每日调度（当前仅手动通道，keepalive 恒 0） | `src/checkin/claim.rs` + 调度 | 后端小改 | 国际版账号保活自动化 |

## 2. P1 补缺（影响中等，工作量较大）

| # | 项 | 来源 | 内容 | 落点 | 成本 | 影响 |
|---|---|---|---|---|---|---|
| P1-1 | **运行日志页** | #10 ❌ | level/category 字典 + 500 条环形 + sinceId 未读水位 + JSONL 导出 | **后端已实现**（`src/runtime_log.rs` + tracing layer + GET /api/logs/runtime + /export）；前端页待补 | 大 | 运行日志可观测性 |
| P1-2 | **/v1/messages/count_tokens** | #3 ❌ | Claude Code 启动探测端点（chars/3.5 估算，标 estimated:true） | **已实现**（`src/api/anthropic.rs::handle_count_tokens` + 路由双挂） | 小 | Claude Code 兼容 |
| P1-3 | **表格列定制** | #11 ❌ | 列显隐/顺序/对齐/列宽拖动（DataTable 列设置抽屉） | 前端组件 | 中 | 表格操作体验 |
| P1-4 | **debug 开关端点** | #4 ⚠️ | /api/debug 开关端点（当前仅 config.toml） | **已实现**（`src/api/admin/debug.rs` GET/PUT；同时保留 /api/limits 入口） | 小 | 调试模式管理 |
| P1-5 | **debug_traffic 落库** | #10 ❌ | 内存环形缓冲改为持久化 | **已实现**（`src/debug_traffic.rs`：push 写 FileStore + load_persisted 启动恢复 + clear 同步删除） | 中 | 调试报文可回溯 |
| P1-6 | **account_usage_records 三件套** | #10 ❌ | 余额快照/到期查询/选路消费 | **已实现**（余额快照+到期查询已有；选路消费：`should_skip_by_balance()` + `acquire_with` 接入） | 中 | 账号用量可观测 |
| P1-7 | **调试模式响应侧原始报文** | §3 P1 | 13 个发送点二次改动（状态码/响应头/错误体） | **已实现**（13 个发送点全部接入 `traced_response` 抓状态码+响应头；`capture_upstream_error_http` 内部接入 `traced_response_body` 抓错误响应体） | 桥接层 | 大（13 处） | 调试完整性 |
| P1-8 | **切换账号重试次数** | §3 P1 | 账号级 switch-count 字段（上游默认 5） | **已实现**（`retry_switch_count` 字段 + 映射到 `RotationPolicy.max_attempts`） | 小 | 重试策略 |
| P1-9 | **zcode 验证码池** | #4/#9 ❌ | 浏览器铸阿里云验证码令牌（前端无痕铸造器 + 令牌池）+ /api/zcode/captcha + /api/zcode/captcha-pool + 节奏配置 | **已实现**（后端 `src/zcode_captcha.rs` 令牌池 + `src/api/admin/zcode_captcha.rs` 四个端点；前端 `lib/aliyunCaptcha.ts` 求解器 + `lib/zcodeCaptchaPool.ts` 后台铸造器 + `components/ZcodeCaptchaPanel.tsx`；领取侧 `zcode_claim.rs` 优先用前端当场求解、回落池子） | zcode 套餐领取绕过人机验证 |
| P1-10 | **阿里云滑块求解器** | #9 🔧 | 阿里云无痕/滑块求解器（用户可见的滑块 + 无痕铸造两种模式） | **已实现**（`frontend/src/lib/aliyunCaptcha.ts`：`solve()` 走用户可见滑块，`mintTraceless()` 走无痕铸造；单实例一次一用、容器每次重建、预热 ≥2.1s；已接入 ZCode 领套餐链路） | 验证码求解能力（多用途） |
| P1-11 | **Cline 伪装头** | §1 | /api/cline/headers 可编辑覆盖表（请求头伪装官方客户端） | **已实现**（`src/api/admin/cline_headers.rs`：GET/PUT + 校验 + 持久化 + 热更新；路由 `/api/cline-headers`） | 绕过上游客户端识别 |
| P1-12 | **指纹脱敏绕过用途** | §1 | core/sanitize.rs 移植（剥离审核黑名单指纹，337 行，零宽字符障碍需处理） | **已实现**（`src/sanitize/mod.rs`：静态规则 + 无状态净化 + 进程级快照开关 `usage.sanitize_fingerprints`（默认开）；挂载在 chat/responses/completions 三个转发路径；`/api/sanitize` GET/PUT + `SanitizePanel.tsx`） | 绕过内容审核指纹 |
| P1-13 | **upgrade_api 数据迁移端点** | #4 ❌ | /api/upgrade 端点：auth.json 导入 + 旧项目数据迁移框架（AIGX 无现成旧数据，但需提供导入能力以备将来导入第三方数据） | **已实现**（`src/api/admin/upgrade.rs`：GET /api/upgrade + POST /api/upgrade/run，幂等空跑保留 API 面） | 中 | 数据导入/迁移能力 |

## 3. P2 补缺（影响较小或语义错位）

| # | 项 | 来源 | 内容 |
|---|---|---|---|
| P2-1 | AutoClaw 国际版 OAuth | #9 ⚠️ | 登录引擎扩展（基线 /api/session/login/oauth/*） | **已实现**（三步协议：① captcha-config 取风控配置 → ② oauth/start 取授权地址（需阿里云验证码参数）→ ③ 浏览器回调换凭证落账号；`GET /api/account-sessions/login/oauth/captcha-config`、`POST /api/account-sessions/login/oauth/start`、`GET /auth/callback-{zai|google}` 免鉴权路由；refresh_token 随账号落盘） |
| P2-2 | zcode claim UI 参数 | #9 ⚠️ | BenefitsPanel.run 传 params（claim 模式需 plan_id + captcha_verify_param） | **已实现**（`BenefitsPanel.tsx`：ZCode 行「盘点套餐 → 选套餐 → 领套餐」，领时先取 `/api/zcode/captcha/config`，需验证码则弹滑块，带 `captcha_verify_param`/`captcha_verify_region` 提交） |
| P2-3 | session 首屏/refresh | #4 ⚠️ | GET /api/session + POST /api/session/refresh | **已实现**（`GET /api/session` 返回用户/权限/额度快照；`POST /api/session/refresh` 续期并返回最新快照） |
| P2-4 | keys PATCH 更新端点 | #4 ⚠️ | 密钥更新端点 | **已实现**（/api/keys/:id PUT 复用 handle_update_token） |
| P2-5 | models/refresh 全量 | #4 ⚠️ | 全量刷新端点 | **已实现**（`POST /api/models/refresh`：调用 `manual_refresh` 刷新 models.dev 目录，返回基线汇总形状 `{results, refreshed, skipped, failed, count, message}`；`/api/models/metadata/refresh` 保留） |
| P2-6 | models/capabilities 手动覆盖 | #4 ⚠️ | 能力位手动覆盖 | **已实现**（`POST /api/models/capabilities`：按 provider×model 覆盖能力位（context/vision/audio/reasoning 等），`GET /api/models/capabilities` 查看覆盖表；`PUT /api/models/manage/:id/capabilities` 单条更新） |
| P2-7 | proxy-pool sync-clash | #4 ⚠️ | Clash Verge 同步 | **已实现**（`POST /api/proxy-pool/sync-clash`：读 Clash Verge verge.yaml，把混合端口 + 各监听器镜像进池；`src/proxy_pool/clash.rs` 解析 + 3s TTL 缓存；`ProxyEntry` 加 source/listener_uid 区分 manual/clash） |
| P2-8 | queue 次数参数 | #4 ⚠️ | 排队次数 HTTP 参数 | **已实现**（`GET/PUT /api/queue`：`queueMaxWaits`（0~10）/`queueWaitSeconds`（0~120），camelCase 键名对齐基线；部分字段更新；空桶校验） |
| P2-9 | 用量报表字段对齐 | #5 ⚠️ | attempts 链/first_response_ms/sensitive_hits/phase 等 | **已实现**（请求日志表加 `attempts`（渠道尝试链）/`first_response_ms`（首字节耗时）/`sensitive_hits`（净化命中数）/`phase`（阶段标记）字段；`GET /api/usage/report` 响应含新字段） |
| P2-10 | Responses/Completions 净化挂载 | #6 ⚠️ | 指纹脱敏挂到 Responses/Completions 路径 | **已实现**（`crate::sanitize::strip(&mut attempt_body)` 挂到 responses.rs / completions.rs 两路径） |
| P2-11 | sanitize 命中统计 | #6 ⚠️ | 净化命中计数 | **已实现**（`STATS_HITS`/`STATS_REWRITES` 原子计数器 + `snapshot()`，GET /api/sanitize 响应加 `stats` 字段） |
| P2-12 | 定时任务默认值对齐 | #8 ⚠️ | 6 条默认开（基线默认开 vs AIGX 全关） | **已实现**（ScheduledSettings 默认 enabled: true） |
| P2-13 | 超时默认值对齐 | #8 ⚠️ | connect 30→10、headers 300→60、stream 300→30 | **已实现**（默认值已是 10/60/30） |
| P2-14 | routing 策略键 | #8 ❌ | strategy/sticky/failover/breaker | **已实现**（`GET/PUT /api/routing`：strategy/sticky/breaker/scheduler_weights；failover_backoff 由冷却/断路器/健康归档承担，不做独立开关） |
| P2-15 | 日志/请求/聚合三档保留期配置 | #8 ❌ | retention 配置端点 | **已实现**（/api/logs/retention 已有 request/daily/audit 三档 + capacity） |
| P2-16 | 目录配置 | #8 ❌ | logDir/requestStatsDir/debugDir | **已实现**（`ServerConfig` 新增 `log_dir`/`request_stats_dir`/`debug_dir` 可选字段；`storage_dirs()` 解析默认子目录；`GET/PUT /api/gateway/settings` 暴露并更新） |
| P2-17 | growth school/travel | #7 ⚠️ | 另域+小程序 UA | **用户已实现**（2026-11-04 拍板：用户自行补，已补完；基线标注 unsupported，另域+小程序 UA 属规避第三方检测类，AIGX 团队不做） |
| P2-18 | Qoder UMID 安装 | #7 ⚠️ | 只探测不安装 | **用户已实现**（2026-11-04 拍板：用户自行补，已补完；安装 UMID 属桌面壳能力，AIGX 是 Web 网关，团队只探测已足够） |
| P2-19 | 记录台账永久保留 + 筛选删除 | #7 ⚠️ | 90 天 prune vs 基线永久 + 无筛选删除 + 无 all=1 护栏 | **已实现**（`CheckinSettings.keep_days` 可配置（0=永久）；`DELETE /api/checkin-center/records` 支持 `all=1` 全清护栏 + 筛选删除 + `days=N` 三模式；`clear_all`/`remove_matching` 函数） |
| P2-20 | cron Skip 语义对齐 | #7 ⚠️ | Skip 不补跑 vs scheduled_tasks 补一次 | **已对齐**（基线 core/scheduled_tasks 也是「只补一次，不补跑期间的每一轮」（line 33-34, 392）；AIGX scheduled_tasks/task_state.rs 同语义（line 181-190）；checkin scheduler 的当天去重 + 启动补签与基线一致） |
| P2-21 | 时区口径统一 | #7 ⚠️ | 用户签到本地时区 vs 中心 UTC+8 | **已实现**（`CheckinSettings.timezone_offset` 可配置（0=系统本地时区，默认 8=UTC+8）；scheduler 的 `beijing()` 按配置偏移构造 FixedOffset；store 的 recent/clear/prune 同步使用配置时区；启动时按已加载设置初始化静态变量） |
| P2-22 | 终端用户积分流水 | #5 ⚠️ | records 分页/earned/consumed 明细 | **已实现**（`GET /api/user/records`：合并签到获得（earned）与 API 消耗（consumed）成一条时间线后分页；查询参数 days/offset/limit/type；返回 earned_total/consumed_total/balance 汇总） |
| P2-23 | 团队积分 | #5 ❌ | 无团队体系 | **用户已实现**（2026-11-04 拍板：用户自行补，已补完；AIGX 无团队体系，积分按用户维度管理；团队积分是基线 Agent2API 内部多租户功能，与 AIGX 的单用户/管理员模型不匹配） |
| P2-24 | promo/pet/asr 运营集成 | #5 ❌ | promo-banner/pet-work/asr | **用户已实现**（2026-11-04 拍板：用户自行补，已补完；AIGX 是技术网关，不做运营活动/宠物/ASR 业务集成；这三项是基线 Agent2API 自有运营功能，非网关能力） |
| P2-25 | 移动端列集 | #11 ❌ | mobile-columns（≤767px 行拆卡片） | **已实现**（`useMobileColumns` hook + `App.css` 卡片媒体查询；ModelManage/Channels/Logs/Tokens/Accounts/Plans/Users 等表格页接入；≤767px 自动行拆卡片） |
| P2-26 | i18n 扩展 | #11 ❌ | zh-Hant/ja/ko/pt-BR（基线 6 语言） | **已实现**（`frontend/src/i18n/` 补 zh-Hant/ja/ko/pt-BR 四语言翻译；语言切换器注册新语言；翻译 key 覆盖全页面） |
| P2-27 | 模型管理列拖拽 + 移动端卡片化 | §7 P2 | 列设置拖拽排序 + 移动端卡片 | **已实现**（ModelManage.tsx：列拖拽排序（colItems/draggingCol）+ 移动端卡片化（table-wrapper class + CSS media query）；两处 mm-table-wrap 加 table-wrapper 触发 useMobileColumns hook） |

## 4. 不补（仅一项）

> 用户拍板（2026-11-04）：机场订阅不补。原「不补」项 zcode 验证码池 / 阿里云滑块求解器 / Cline 伪装头 / 指纹脱敏绕过用途 / upgrade_api 经用户拍板改为**补缺**，已移至 §2 P1（P1-9~P1-12 + P1-13），四项规避类功能均已完成（2026-11-05）。
> 注：前 4 项属规避第三方检测类，实现需绕过第三方人机验证/内容审核机制，按用户指令执行。upgrade_api 虽 AIGX 无现成旧数据，但需提供 auth.json 导入/迁移端点以备将来导入第三方数据。
>
> 用户拍板（2026-11-04）：P2-17 growth school/travel、P2-18 Qoder UMID 安装、P2-23 团队积分、P2-24 promo/pet/asr 运营集成四项由用户自行补，已补完（见 §3 表），AIGX 团队不做。故「不补」仅剩机场订阅一项。

### 4.1 机场订阅（#4 ❌）
- **基线实现**：`subscriptions.rs`——Clash/mihomo 内核订阅（URL + decode-qr），代理内核语义。
- **AIGX 现状**：`/api/subscription/*` 是计费订阅（时长订阅 + 余额购买 + 独立配额池），同名不同义。
- **不补理由**：同名不同义，AIGX 的订阅是计费订阅（已自足，见 #5 ✅）。基线「订阅」是代理内核语义，与 AIGX 计费订阅不可直接等价。
- **替代方案**：代理池已有（proxies 页）。如需内核订阅（Clash 配置同步），作为独立需求评估，不与计费订阅混用。

---

## 5. 建议执行顺序

1. **P0-3 文档更新**（零风险，先做）
2. **P0-1 docs 页 + P0-2 筛选记忆**（前端，可并行）
3. **P0-4 workbuddy-intl 保活链**（后端小改）
4. **P1-8 切换账号重试次数**（后端小改）
5. **P1-2 count_tokens**（与同事确认归属后）
6. **P1-4 debug 开关端点**（后端）
7. **P1-3 列定制**（前端）
8. P1-5 debug_traffic 落库 / P1-6 account_usage_records（后端，工作量中）
9. P1-1 运行日志页（后端+前端，工作量大）
10. P1-7 调试响应侧报文（13 处，工作量大）
11. P2 各项按业务优先级推进

> 协作约束：另一路 AI/同事的并行改动（额度预留重构等）未提交，main.rs 与 static 有交织——补缺轮动手前先 `git fetch` 确认工作区状态，避免与同事撞车。
