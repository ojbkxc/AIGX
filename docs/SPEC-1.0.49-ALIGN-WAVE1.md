# SPEC-1.0.49 — AIGX ↔ Agent2API 3.4.2 对齐（波次 1）

> 基准：`D:/GitHub/Agent2API-upstream-main`（**3.4.2**，最新）。旧副本 `Agent2API`/`Agent2API-main` 是 3.0.8，不作为基准。
> 来源：2026-11-12 四路只读审计（账号池 / 签到 / 数据面 / 管理面）+ 主会话交叉核实。
> 目标版本：**v1.0.49**。本文件是并行开发的**唯一事实源**；成员偏离必须写 §9。

---

## 0. 并行纪律（违反即返工）

1. **`src/main.rs` 只有 lead 改**（路由注册）。成员写好自己的 handler（`pub` 可见），在本文件中登记「需要注册的路由行」，由 lead 统一集成。
2. **`frontend/src/i18n/zh.json` / `en.json` 只有 lead 改**（集成时跑 `node scripts/i18n-fill-missing.mjs`）。成员在代码里正常写 `t('中文')`，不要手改词表。
3. `frontend/src/api/index.ts` 与 `frontend/src/types/index.ts` 允许 2 名前端成员各自**外科式 Edit**：
   - 编辑前**必须**立即重读该文件（禁止复用几分钟前的 Read 结果）；
   - 只增不改，锚点用本流独有字符串；
   - **禁止** `Write` 整文件；改完 `grep` 自己的键 + 对方流的关键键各确认一次。
4. **禁止本地 `cargo build/test/clippy`**（用户硬约束）。只跑 `cargo fmt --all -- --check`。Rust 编译/测试交 CI。
   前端可本地跑 `npx tsc --noEmit`、`npx eslint <file>`、`node scripts/i18n-fill-missing.mjs --check`。
5. 每个成员交付时报告：①改了哪些文件（含行号区间）②新增/修改的测试名 ③`cargo fmt` 结果 ④未做/偏离项。
6. 禁止改别人拥有的文件。需要跨文件改动时在报告里提出，由 lead 或对应 owner 执行。
7. 任何「不该出现 X / 扫描零命中」的负向测试必须配**正向对照**（见 memory pitfalls #10）。
8. `include_str!` 自指断言必须用 `concat!` 拆写 + 锚点，并做**变异检查**（pitfalls #17）。

---

## 1. 文件所有权矩阵

| 流 | 成员 | 独占文件 |
|---|---|---|
| A 入站 Anthropic 保真 | `proto-anthropic` | `src/api/anthropic.rs` |
| B `/v1/responses` 入站转换 | `proto-responses` | `src/api/openai/responses.rs`、新建 `src/api/openai/responses_convert.rs` |
| C 签到后端 | `checkin-backend` | `src/checkin/**`、`src/api/admin/checkin_center.rs` |
| D 签到前端 | `checkin-frontend` | `frontend/src/pages/CheckinCenter.tsx`、`frontend/src/pages/CheckinCenter.css` |
| E 凭证刷新核对 | `credential-verify` | `src/account_session/refresh.rs`、`src/account_session/maintenance.rs` |
| F 加密备份 UI | `backup-ui` | `frontend/src/components/DataManagement.tsx` |
| G 脱敏规则可配置 | `sanitize-config` | `src/sanitize/mod.rs`、`src/api/admin/settings.rs`（仅 sanitize 段） |
| H 账号排序/置顶 | `account-order` | `src/account_session/store.rs`（仅排序段）、`src/account_session/rotate.rs`、`src/api/admin/account_sessions.rs`（仅排序段） |

> `src/api/admin/settings.rs` 与 `src/api/admin/account_sessions.rs` 是大文件：只许在**本流语义段**内 Edit，不得顺手重构。

---

## 2. 流 A：入站 Anthropic → Chat 保真

**现状缺陷**：`src/api/anthropic.rs:128-228` 只认 `text` / `tool_use` / `tool_result`（且 content 必须是字符串）。
→ 带图请求经 `/v1/messages` **静默丢图**；thinking 段丢失；`cache_control` 丢失；`tool_result` 数组形态丢失。

**冻结契约**（对内 `ChatMessage`，不改结构体——避免全仓字面量断裂）：

1. `image` 块 → `content_blocks` 里的 OpenAI 形态：
   - `source.type == "base64"` → `{"type":"image_url","image_url":{"url":"data:<media_type>;base64,<data>"}}`
   - `source.type == "url"` → `{"type":"image_url","image_url":{"url":"<url>"}}`
   - 其它 source 形态 → 丢弃并 `tracing::debug!` 记类型（不编造）。
2. 一条 user 消息的 content 数组只要含 image 块，就设 `content_blocks = Some(数组)`；数组内 `text` 块保留为 `{"type":"text","text":...}`，
   其它可透传块原样保留；`content` 同时置为拼接后的纯文本（无文本时为 `None`）。
   **不含** image 的消息维持现状（`content_blocks: None`），避免给上游换形状。
3. `thinking` / `redacted_thinking` 块 → 文本累加进 `ChatMessage.reasoning`（`redacted_thinking` 只取 `data` 存在与否，不塞密文，记 debug）。
4. `tool_result.content` 为数组时：文本部分 join 成 tool 消息 content；其中 image 块 → **在本条 assistant 的全部 tool 消息之后**补一条
   `role=user` 的 `content_blocks` 消息（A2A 同序，见 `core/protocol/anthropic.rs:287-321`）。
   `tool_result.content` 为字符串时维持现状。
5. `cache_control`：对 `text` 块，若该块带 `cache_control`，则该块以块形态进 `content_blocks`（即使无图）——保证断点透传。
6. **已知不修（写进注释与 §9）**：`tool_result.is_error` 在 OpenAI 形出口没有对应表达，本轮不引入 `ChatMessage` 新字段（全仓字面量成本过高）；`document` / `search_result` 块丢弃并记 debug。

**测试**（同文件 `mod tests`）：
- `anthropic_image_block_becomes_image_url`（base64 + url 两形态）
- `anthropic_text_only_message_keeps_bare_content`（正向对照：无图仍是 bare-string，不误加 blocks）
- `anthropic_thinking_accumulates_into_reasoning`
- `anthropic_tool_result_array_images_appended_after_tool_messages`（断言顺序：tool 消息在前、图片 user 消息在后）
- `anthropic_cache_control_forces_block_form`

---

## 3. 流 B：`/v1/responses` 入站转换

**现状缺陷**：`src/api/openai/responses.rs` 只做透传；`bridge/mod.rs:849-864` 默认返回 `Err("does not support the Responses API")`
→ **session 型渠道（qoder/trae/cmdcode…）用 Responses 直接失败**。

**冻结契约**：`POST /v1/responses` 的处理顺序
1. 鉴权/限流/分组/定价/failover 骨架**保持现有流程不动**。
2. 对每个候选 bridge：
   - 若 `responses_passthrough` 返回 `Ok(..)` → 沿用现有透传路径（零行为变化）。
   - 若返回 `Err(BridgeError::Config(_))` → **降级为入站转换**：`responses_convert::request_to_chat(&body)` → `bridge.chat()` / `bridge.chat_stream()` → `responses_convert::chat_to_response(..)` / `chat_chunk_to_sse(..)`。
   - 其它 `BridgeError`（Transport/Upstream*）→ 维持现有 failover 语义，不降级。
3. 新建 `src/api/openai/responses_convert.rs`，**纯函数、无 IO**，签名：
   ```rust
   pub(crate) struct ChatFromResponses { pub chat: ChatFormat, pub stream: bool }
   pub(crate) fn request_to_chat(body: &Value) -> Result<ChatFromResponses, String>;
   pub(crate) fn chat_to_response(resp: &ChatResponse, model: &str) -> Value;
   pub(crate) fn chat_chunk_to_response_events(chunk: &ChatStreamChunk, seq: &mut u64) -> Vec<Value>;
   pub(crate) fn done_response_object(model: &str, usage: Option<&UsageStats>) -> Value;
   ```
4. 请求映射：
   - `input` 为字符串 → 一条 `user` 消息；为数组时按 `type` 处理 `message`（role + content）、`function_call`、`function_call_output`；
     `input_text` / `output_text` / `input_image` 块进 `content_blocks`（`input_image` 的 `image_url` 原样）。
   - `instructions`（字符串）→ 首条 `system` 消息。
   - `max_output_tokens` → `max_tokens`；`reasoning.effort` → `reasoning_effort`；`top_p`/`temperature`/`tool_choice`/`parallel_tool_calls` 透传。
   - `tools`：`type=="function"` → 既有 `Tool` 结构；其它类型丢弃并 debug（不编造）。
5. 响应映射（非流式）：
   ```json
   {"id":"resp-<uuid>","object":"response","created_at":<ts>,"model":"<model>","status":"completed",
    "output":[{"type":"message","id":"msg-…","role":"assistant","status":"completed",
               "content":[{"type":"output_text","text":"…","annotations":[]}]},
              {"type":"function_call","id":"fc-…","call_id":"<tool id>","name":"…","arguments":"<json串>"}],
    "usage":{"input_tokens":n,"output_tokens":n,"total_tokens":n}}
   ```
   `resp.reasoning` 存在时附 `{"type":"reasoning","id":"rs-…","summary":[{"type":"summary_text","text":…}]}` 于 output **首位**。
6. 响应映射（流式）：至少产出 `response.created` →（`response.output_item.added`）→ `response.output_text.delta`* → `response.completed`，
   每条形如 `event: <type>\ndata: <json>\n\n`；`[DONE]` 结尾与现有 SSE 约定一致。
7. 计费：降级路径的 usage 从 `UsageStats` 取，口径与 chat 路径一致（复用现有 helper，不自造公式）。

**测试**（`responses_convert.rs` 的 `mod tests`，纯函数）：
- `request_maps_input_string_and_instructions`
- `request_maps_input_array_items_and_image_block`
- `request_maps_function_tools_and_effort`
- `response_wraps_text_and_function_calls`（含 reasoning 在首位）
- `stream_events_are_well_formed`（created…delta…completed 顺序 + `data:` 可解析）
- 反向对照：`request_to_chat` 对缺 model 的 body 返回 Err（不得静默兜底）

---

## 4. 流 C：签到后端

**C1 结果分类**：`CheckinOutcome` 增 `Captcha`（`serde` 名 `"captcha"`，`as_str()` 同）；`is_ok()` 对它返回 false。
`claim.rs` 里「需要人机验证」的失败分支改判 `Captcha`（否则仍 `Failed`）。

**C2 记录结构**：`CheckinRecord` 增
```rust
#[serde(default)] pub kind: String,      // manual | schedule | makeup | retry
#[serde(default)] pub error: String,     // 失败原文（成功时为空）
```
`kind` 与既有 `source` **同值双写**（`record()` 一次写入两者），保证旧读端不破。
**存量零迁移**：`#[serde(default)]`。

**C3 查询筛选**：`GET /api/checkin-center/records` 新增 query：
- `kind`（`manual|schedule|makeup|retry`，空/缺省 = 不过滤）
- `start` / `end`（unix 秒，闭区间；缺省 = 不限）
筛选在 `recent_records` 内或之后由 handler 做均可；分页与 `total` 语义不变。

**C4 排程模型**：`CheckinSchedule` 增（全部 `#[serde(default)]`，存量零迁移）
```rust
pub time_end: String,          // "HH:MM"；空 = 无窗口（仅按 time）
pub randomize: bool,           // true = 每个账号在 [time, time_end] 内取确定性随机时刻
pub retry_limit: u32,          // 0 = 关闭重试；缺省 3（与现 RETRY_LIMIT 一致）
pub retry_gap_minutes: u32,    // 缺省 5；<=0 时回落全局 gap_ms
pub makeup_time: String,       // "HH:MM"；空 = 不跑补签轮
```
- 随机时刻：`hash(account_id) % 窗口分钟数` → 分钟偏移，**确定性**（同账号同日同刻，重启不漂）。
- 重试：按条目 `retry_limit` / `retry_gap_minutes`；`retry_limit=0` → 不排重试。
- 补签轮：`makeup_time` 到达时，挑「当天该条目下 `!outcome.is_ok()` 且未成功过」的账号再跑一次，`source/kind="makeup"`。
- 内置专属时刻（qoder 10:01 / zcode 19:01）优先级不变：**provider_times > 内置 > 条目 time**。

**C5 契约下发给前端**：`GET /api/checkin-center` 的 data 增
```json
"unsupported_providers": ["chatglm","atomcode","lobsterai"]
```
（这三家实现随上游搬进闭源插件仓、主仓如实失败；前端据此禁用入口。来源 `src/checkin/claim.rs:1594/1723/1735` 的桩。）

**测试**：captcha outcome 往返；kind 双写；start/end 过滤边界；随机时刻确定性（同输入同输出、不同账号可不同）；
retry_limit=0 不排重试；makeup 只挑非 ok 账号；`unsupported_providers` 恰好 3 家且都能在 `CHECKIN_PROVIDERS` 里找到（正向对照）。

---

## 5. 流 D：签到前端

按 §4 冻结契约实现（**不猜后端字段**）：
- 台账 tab：新增「类型」(kind) 列、「失败原因」(error) 列；筛选区加 `kind` 下拉与起止日期（转 unix 秒）；`outcome` 下拉增 `captcha`（文案「需人机验证」）。
- 定时签到 tab：条目编辑器增 `time_end`、`randomize`、`retry_limit`、`retry_gap_minutes`、`makeup_time` 五个控件，带说明 tooltip。
- provider 选择器：`unsupported_providers` 中的项置灰 + tooltip「实现随上游进入插件仓，主仓未接入」。
- 所有新文案走 `t('中文')`（不手改 i18n 词表）。
- `frontend/src/api/index.ts` 的 `getCheckinRecords` 参数类型补 `kind/start/end`；`types/index.ts` 的 `CheckinRecord`/`CheckinSchedule` 补字段（`?` 可选，兼容旧后端）。

---

## 6. 流 E：凭证刷新「假成功」核对

对齐 A2A issue #407（`core/credential_maintenance.rs:99-120`）。

**契约**：`refresh::refresh_one` 在调用上游前抓 `expires_at`（以及凭证 `updated_at`，若模型有），刷新成功落盘后**重新读取账号**比对：
- 有推进（`expires_at` 变大或 `updated_at` 变大）→ 维持 `Refreshed`。
- 未推进 → 返回 `RefreshOutcome::Failed("上游未推进凭证（expires_at/updated_at 未变化），账号可能已失效")`，**不改写**凭据。
- 上游显式返回新 token 但 `expires_at` 为空（部分家不给过期）→ 以「token 明文是否变化」为次级判据。
- 无法判定（字段本就为空且 token 相同）→ 不倒扣，保持 `Refreshed` 并 `tracing::warn!`（保守方向：不误报失败）。

**测试**（用假上游 sink，或纯函数抽取）：
- `refresh_detects_unchanged_expiry_as_failure`
- `refresh_accepts_advanced_expiry`
- `refresh_accepts_changed_token_with_empty_expiry`（次级判据）
- 反向对照：正常推进不会被误判失败。

---

## 7. 流 F：加密备份 UI（修生产 404 死链）

**已实证 bug**：前端 `GET /api/backup/export` → 生产 **404**（路由已因安全整改移除）。页面无加密导出入口。

**契约**：
1. 删除 `api.exportBackup` 与明文导出按钮路径；`DataManagement.tsx` 改为两输入框（口令 / 确认口令，`char count >= 8`）+「导出加密备份」按钮。
2. 流程：`POST /api/backup/export-encrypted {password, include_logs}` → 取 `data.file` → `GET /api/backup/download?file=<file>` → 触发下载（`a.download` 用返回的 `file` 名）。
   口令不写日志、不进 localStorage、成功后清空输入。
3. `include_logs` 复选框保留。
4. 导入：保留既有 `/api/backup/import`；另加「导入加密备份」入口接 `POST /api/backup/import-encrypted`（multipart：`file` + `password`）。
5. 所有新文案 `t('中文')`。
6. `frontend/src/api/index.ts`：删 `exportBackup`，增 `exportEncryptedBackup`、`downloadBackup(file)`、`importEncryptedBackup(file, password)`（blob 响应需按现有 `request` 能力处理，必要时用原生 fetch + 认证头，参考文件内已有下载实现）。

**验收**：`npx tsc --noEmit` 通过；`grep -rn "backup/export?" frontend/src` 零命中（正向对照：`backup/export-encrypted` 有命中）。

---

## 8. 流 G：脱敏规则可配置

对齐 A2A `core/sanitize/rules.rs`（≤200 条，支持字面/regex、作用域）。

**契约**：
1. 配置项（沿用项目既有配置注入方式，读 `src/config.rs` 现状后接入，不新造持久层）：
   `sanitize_rules: Option<Vec<SanitizeRule>>`，语义：
   - `None`（缺省）= 用内置硬编码规则（现行为，零变化）
   - `Some([])` = **关闭**全部脱敏
   - `Some([...])` = 只用用户规则（内置不再生效）
2. `SanitizeRule { pattern: String, replacement: String, is_regex: bool, roles: Vec<String> /*空=全部*/, providers: Vec<String> /*空=全部*/ }`
   上限 200 条；超限 → 配置校验报错（不静默截断）。
3. `sanitize::sanitize_messages` 增加 `provider: &str` 入参（调用点由 lead 在集成时接）；默认行为不变。
4. 现有 `/api/sanitize` 端点（管「额外脱敏头名单」）**不改语义**，只增字段 `rules` 用于读写上述配置；`null`/`[]` 语义按第 1 条。
5. 正则编译失败 → 该条规则跳过并 `tracing::warn!`，不影响其它规则。

**测试**：内置缺省不变（正向对照）；`[]` 清空后原本命中的文本不再改写；regex 规则生效；provider 作用域隔离；超 200 条报错；坏正则被跳过但好规则仍生效。

---

## 9. 流 H：账号排序 / 置顶

对齐 A2A `core/account_order.rs` + `/api/accounts/sorting|reorder`（**只做网关需要的部分**）。

**契约**：
1. `AccountSession` 增 `#[serde(default)] pub sort_order: i64`（存量 0）。
2. 选路顺序：`priority` 升序（现有语义）→ **`sort_order` 升序**（0 视为中位，负数置顶）→ 现有 `weight` 轮转。只影响同 `priority` 组内顺序。
3. 新端点（handler 写在 `account_sessions.rs` 排序段）：
   - `POST /api/account-sessions/{id}/promote` → 该账号 `sort_order = 组内最小 - 1`；返回最新排序。
   - `PUT /api/account-sessions/order` body `{"order": ["id1","id2",...]}` → 按数组顺序写 `sort_order = index+1`；未知 id 忽略并计入 `skipped`。
4. 列表 `GET /api/account-sessions` 返回按「channel_id, priority, sort_order」稳定排序（前端不再重排）。
5. **不动**现有多账号轮转/冷却/限流语义。

**测试**：promote 后组内名次第一；PUT order 幂等；未知 id 被跳过；跨 priority 不串组（正向对照）。

---

## 10. 交付与集成（lead）

1. lead 统一：`src/main.rs` 路由、i18n 词表、跨流类型/API 客户端核验、版本号 6 处、`static/` 重建。
2. **版本号规则（踩过的坑）**：`Cargo.lock` 只改 `name = "aigx"` 那一段；改完 `git diff -- Cargo.lock | wc -l` 必须只有 1 行变动。
3. CI 12/12 全绿 → tag `v1.0.49` → Multi-platform 5 平台 success → Release 15 资产 → 部署 `104.223.65.202:22` → 五探针 + 公网三探针 + 管理员实测。
4. 生产验收重点：
   - 加密备份导出跑通（不再 404）；
   - 带图 `/v1/messages` 不再丢图（用生产 key 实测，无图则记录）；
   - 签到记录出现 `kind`/`error`/`captcha`，筛选生效；
   - 账号池排序/置顶生效且不影响原有转发。

---

## 11. 波次 2（本轮之后立刻排）

degrade 状态机、compact 路由、路由 trace/flow、client-identification + sidebar-layout + system-info、
zcode version/claim-verification、growth travel/school、**自定义供应商实体**、**工具服务运行时（web-search/vision）**。

**明确不做**（桌面客户端工具链，与自托管网关定位不符）：云同步、本机客户端配置托管（`api/agents.rs` 语义）、进程隔离插件宿主、AI 助手图片生成/编辑、每-Agent 资源绑定、HTTPDNS/网络回退、图片上行压缩、cc-switch 导入源、qoder-umid。

---

## 12. 实施偏离记录

（成员在此追加；格式：`流X | 项 | 原计划 | 实际 | 原因`）