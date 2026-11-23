# 客户端识别 / 日志审计对齐审计（AIGX ↔ Agent2API）+ AIGX 自身请求标识

> 审计日期：2026-11-23（UTC+8）
> 目标：1) AIGX `src/api/client_id.rs`（目录+识别优先级）对齐 A2A `core/client`；2) 定位 AIGX 自身请求（聊天/运维/自测/Playground/Agent）写空 `client_name` 的点，统一应标 **"AIGX"**；3) 日志页/审计页前端是否补齐客户端维度；4) `record_audit` 是否需带 client；5) 识别优先级/归一/版本提取缺口。
> 纪律：只读审计，未改任何代码；本文件是唯一产物，未提交。AGPL 参考树仅比对行为/字段，未拷贝源码。

## 0. 文件简写与标签

- **A2A**：`core/client/` = `desktop-tauri/src-tauri/server/src/server/core/client/{client.rs,catalog.rs,matcher.rs,evidence.rs}`；`keepalive.rs` = `core/billing/keepalive.rs`
- **AIGX**：`CID` = `src/api/client_id.rs`；`LOG` = `src/log/mod.rs`；`LQ` = `src/api/admin/logs.rs`；`ADMINCID` = `src/api/admin/client_id.rs`；`FE` = `frontend/src/pages/Logs.tsx`；`CIP` = `frontend/src/components/ClientIdPanel.tsx`
- 标签：`[OK]` 等价；`[DIFF]` 都有但口径不同；`[MISSING]` 一方缺失。

## 0.1 结论计数

| 维度 | OK | DIFF | MISSING | 备注 |
|---|---:|---:|---:|---|
| 1. 目录 + 识别口径 | 3 | 6 | 1（缺 9 家） | AIGX 目录缺 A2A 的 9 家 AI 客户端；识别机制 6 处口径差 |
| 2. AIGX 自身请求标识 | — | — | **7 处** | 全部写空→unknown，应标 "AIGX" |
| 3. 日志页前端 | 5 | 1 | 2 | 列/详情/JSON 导出/设置面板/看板 OK；缺客户端筛选 + CSV 列；关键词不匹配 client |
| 4. 审计 record_audit | 0 | 0 | 2（可选） | AuditLog 无 client 维度；A2A 无同构审计，标记可选 |
| 5. 优先级/归一/版本 | 2 | 4 | 0 | 声明白名单、超 UA 请求头、歧义/候选、证据持久化 |

> **用户问题「日志与审计里查不到客户端」的直接根因**：①（主）AIGX 自身请求写空 client_name → 归 `unknown`；②目录缺 A2A 的 9 家常见客户端 → 真客户端落 `unknown`；③日志页无客户端筛选、关键词不匹配 client、CSV 导出无 client 列 → 「查」不到；④审计日志根本没有 client 维度。

---

## 1. 客户端目录 + 识别规则（A2A `core/client` vs AIGX `BUILTIN_CLIENTS`）

A2A 目录：`catalog.rs:22-43`（20 家，顺序即界面序）。AIGX 目录：`CID:96-279`（21 家，顺序即匹配优先级）。

### 1.1 A2A 有、AIGX 缺的客户端（9 家）→ **[MISSING]**

| 客户端 | A2A id | A2A UA 产品令牌 | 后果 | 证据 |
|---|---|---|---|---|
| DeepSeek Harness | dsh | `deepseek-harness` | 命中不了 → unknown | A2A `catalog.rs:28`；AIGX 无 |
| Magpie | magpie | `magpie`,`magpie-router`,`magpie-vision`,`magpie-search`,`magpie-image` | 同上 | `catalog.rs:29` |
| Hermes Agent | hermes | `hermes`,`hermesagent`,`hermes-agent` | 同上 | `catalog.rs:30` |
| Hindsight | hindsight | `hindsight` | 同上 | `catalog.rs:31` |
| WorkBuddy | workbuddy | `workbuddy`,`workbuddy-ai` | 同上 | `catalog.rs:35` |
| CodeBuddy | codebuddy | `codebuddy`,`codebuddy-code` | 同上 | `catalog.rs:36` |
| Crush | crush | `crush` | 同上 | `catalog.rs:39` |
| Factory Droid | factory-droid | `factory-cli` | 同上 | `catalog.rs:41` |
| Goose | goose | `goose` | 同上 | `catalog.rs:42` |

> 建议：把上述 9 家补进 `CID:96` 的 `BUILTIN_CLIENTS`（id/name/aliases/products 按 A2A 口径），顺序无需严格对齐（AIGX 目录是有序匹配，产品令牌更具体的应在前）。

### 1.2 AIGX 有、A2A 无的客户端（10 家）→ **[DIFF] 口径**

AIGX 额外识别：`Continue / OpenAI SDK / Anthropic SDK / LangChain / Aider / Roo Code / Windsurf / Gemini CLI / Chatbox / LobeChat`（`CID:148-278`）。

- A2A **刻意不把通用 SDK/传输层**当客户端：`matcher.rs:37-45` 的 `unknown_product()` 把 `openai`/`anthropic`/`asyncopenai`/`requests`/`axios`/`go-http-client` 等列为 generic，识别不出来时**不**当客户端身份。
- AIGX 把 `OpenAI SDK`/`Anthropic SDK`/`LangChain` 当客户端 → 会把大量「普通 SDK 调用」标成具名客户端，统计噪声与 A2A 口径相反。
- **建议**：保留 Continue/Aider/Roo/Windsurf/Gemini CLI/Chatbox/LobeChat（这些是真实 IDE/CLI 产品），但对 OpenAI/Anthropic SDK/LangChain 要么降级为 generic（不识别）、要么单列「SDK」档避免与「产品客户端」混淆。至少标注口径差异，供运营决定。

### 1.3 A2A 目录附带的两个字段（AIGX 缺）→ **[DIFF]**

- A2A `BuiltinClient` 带 `value`（该客户端的规范值）与 `ua`（一份代表性 User-Agent，供**账号级自定义 UA 预置**，`catalog.rs:5-18`）。`ua` 用于账号「请求标识」下拉预置。
- AIGX 账号级 UA 是自由文本（`AS:4732-4738` 输入框，无预置下拉）。
- **建议（低优先）**：若账号页 UA 想要 A2A 那种「一键填入真实 UA」预置，可在 `BUILTIN` 增加 `ua` 预置字段并在账号设置消费。本轮「查不到客户端」不依赖此项。

### 1.4 识别优先级 / 归一 / 版本（逐项）

| 维度 | A2A | AIGX 现状 | 判定 | 证据 |
|---|---|---|---|---|
| 优先级链 | 声明 → 自定义规则 → 内置 UA → **已核实身份头** → other/unknown/missing | 声明 → 自定义规则 → 内置 UA → unknown | **[DIFF]**（AIGX 缺超-UA 身份头与 other/missing 分档） | A2A `matcher.rs:59-132`；AIGX `CID:627-645` |
| 声明头名 | `x-agent2api-client`（仅此一个） | `x-client-name` + 别名 `x-aigx-client`/`x-agent2api-client` | **[DIFF]** | A2A `catalog.rs:45`；AIGX `CID:46-50` |
| 声明是否受开关控制 | `settings.accept_declarations` 关闭则不接受声明 | 恒接受声明（无开关） | **[DIFF]** | A2A `matcher.rs:72-80`；AIGX `CID:628-637` |
| 声明名归一 | 仅当命中内置 id/name/value/products 或已注册自定义客户端才接受；否则 `unregistered` 回落 UA | 命中内置别名→规范名；否则只要「安全字符串」就**原样接受**（≤64、无控制、非 `<>"'\\`） | **[DIFF]**（AIGX 接受任意声明名，无注册校验） | A2A `catalog.rs:134-144`+`matcher.rs:75`；AIGX `CID:648-704` |
| 超-UA 身份头 | 识别 `x-title=Cline`、`x-client-type=cline-sdk`、`x-cerebras-3rd-party-integration=cline`（`matcher.rs:106-115`） | 不扫描这些头 | **[MISSING]** | A2A `catalog.rs:46-48`；AIGX 无 |
| 自定义规则形态 | target 只能是 `user-agent` 或 `IDENTITY_HEADERS`；match `product/prefix/contains/exact`；指向已存在 client_id | target 任意头（仅敏感头黑名单）；`prefix/contains/regex`；输出任意 client_name | **[DIFF]**（AIGX 更自由：正则可读任意非敏感头；A2A 限制只能读 UA/身份头） | A2A `catalog.rs:170-187`；AIGX `CID:377-517` |
| 歧义处理 | 多个内置产品令牌同时命中不同家 → `ambiguous` → 归 `other` | 首个命中即返回（顺序优先），无歧义档 | **[DIFF]** | A2A `matcher.rs:90-119`；AIGX `CID:707-720` |
| 未识别值 | 有头但认不出 → `other` 且提取 `candidate`(UA 首个产品名)；无任何头 → 空串（missing）；坏头 → other | 一律 `unknown`，不提取候选 | **[DIFF]**（AIGX 无 candidate，排障看不到「像什么客户端」） | A2A `matcher.rs:31-47,125-131`；AIGX `CID:719` |
| 产品令牌边界 | 两侧须空白/`/`/`(`/结尾；且**括号深度=0**（不在 UA 注释内匹配） | 两侧须非 `[A-Za-z0-9-_.]`（token 边界），无括号深度判断 | **[DIFF]**（边界算法不同，AIGX 未排除括号内子串） | A2A `matcher.rs:4-17`；AIGX `CID:745-770` |
| 版本提取 | `/ver`、` Product ver`、前缀末尾 `/`；带 `truncated` 标记防截断误判 | 同三形态；但无 truncated 标记（UA 被 1024 截断时可能误取尾串） | **[DIFF]（minor）** | A2A `matcher.rs:4-16`；AIGX `CID:778-818` |
| 证据持久化 | `ClientEvidence`：method / rule_id / matched_header / matched_pattern / client_id / client_name / candidate / declaration_status（`evidence.rs:11-31`） | `ClientIdentity` 只有 name/version/source；`attach()` 只写 name+version，**source 不落日志** | **[DIFF]**（日志无法回答「怎么认出来的」） | A2A `evidence.rs`；AIGX `CID:283-310` |

> 自名保留：A2A `catalog.rs:165` 把 `other/unknown/assistant/model_test/assistant_images/assistant_title/checkin_keepalive` 保留为**系统自用客户端名**，自定义客户端不得占用。AIGX 无保留名单 → 见 §2 建议。

---

## 2. AIGX 自身请求标识：写空 → 应统一标 "AIGX"（7 处）

A2A 对自身请求是**显式打标签**的：`keepalive.rs:243` 写 `RequestSource{client:"checkin_keepalive"}`；`catalog.rs:165` 的保留名说明 `model_test/assistant/assistant_images/assistant_title/checkin_keepalive` 是 A2A 自身子系统名。**AIGX 对应点全部写空 → 落 `unknown`，日志页「客户端」列显示为「未记录」**（`FE:1127-1129`）。

| # | 入口 | 现状 | 证据 | 建议 |
|---|---|---|---|---|
| 1 | AI 运维 Agent 调 LLM | `client_name: String::new()` | `src/agent/llm.rs:514` | 标 `AIGX` |
| 2 | Playground（成功） | `client_name: String::new()` | `src/api/admin/playground.rs:98` | 标 `AIGX` |
| 3 | Playground（失败） | `client_name: String::new()` | `src/api/admin/playground.rs:175` | 标 `AIGX` |
| 4 | 渠道调试 chat_test（成功） | `client_name: String::new()`（key_id=`chat_test`） | `src/api/admin/legacy.rs:4384` | 标 `AIGX` |
| 5 | 渠道调试 chat_test（失败） | `client_name: String::new()` | `src/api/admin/legacy.rs:4445` | 标 `AIGX` |
| 6 | 模型测试 model_test | `client_name: String::new()`（key_id=`model_test`） | `src/api/admin/model_test.rs:790` | 标 `AIGX` |
| 7 | 运维助手 Assistant | `RequestLog::new()` 后**未 attach**（client 恒空） | `src/api/admin/assistant.rs:226`（无 attach） | 标 `AIGX` |

**建议实现（最小改动、集中口径）**：
- 在 `client_id.rs` 增常量 `pub const SELF_CLIENT: &str = "AIGX";` 与便捷函数 `pub fn self_identity() -> ClientIdentity { ClientIdentity { name: SELF_CLIENT.into(), version: String::new(), source: "internal" } }`（或直接返回 `&'static ClientIdentity`）。
- 上述 7 处要么设 `client_name: SELF_CLIENT.to_string()`、要么在构造后 `crate::api::client_id::attach(&mut log, &self_identity());`。
- 把 `AIGX`（及可选子类如 `AIGX Playground`/`AIGX Agent`，但用户要求统一「AIGX」，建议 name=`AIGX`）加入 §1.4 的保留名单，禁止自定义规则/客户端占用。
- 如需区分来源，可把子系统写进 `key_id`（已分别有 `playground:*`/`chat_test`/`model_test`），`client_name` 统一 `AIGX` 即可，不破坏按子系统排查。
- 数据面（chat/anthropic/audio/…）`RequestLog::new()` + `attach` 的覆盖已核查为**一一对应**（见 §附录），无需改动。

---

## 3. 日志页 / 审计页前端客户端维度

AIGX 已有：`FE:171,1117-1131` 客户端列（client_name + client_version，缺省显示「未记录」）；`FE:80-82` 类型；`LogDetailDialogs.tsx:38` 详情弹窗显示「客户端 名 版本」；`CIP` 系统设置→客户端识别规则面板（内置目录 + 自定义规则 CRUD + 预览）；`TopClients.tsx` 看板「客户端」维度（unknown 显示「未知客户端」）。**以上 [OK]**。

| 维度 | A2A | AIGX 现状 | 判定 | 证据 | 建议 |
|---|---|---|---|---|---|
| 客户端列 | 无桌面日志表（桌面为本地代理，非网关日志页） | 有客户端列（可列设置显隐） | **[OK]** | `FE:171,1117-1131` | — |
| 详情展示 client | 有证据维度 | 详情弹窗「客户端 名 版本」 | **[OK]** | `LogDetailDialogs.tsx:38` | — |
| **客户端筛选** | — | 筛选只有 user/model/channel/status/keyword/时间；**无 client 筛选** | **[MISSING]** | `FE:150-193,853-938`；后端 `LQ:83-105` 无 client 字段；`LOG:1469-1517` 过滤无 client | 后端 `RequestLogQuery` 加 `client`（或 `client_name`）字段 + store 过滤按 `client_name` 精确/包含匹配；前端筛选行加客户端下拉（选项来自 `top_clients` 或 distinct）。 |
| 关键词是否匹配 client | — | keyword 只匹配 model/channel_name/error_msg/user_id | **[DIFF]** | `LOG:1464,1501-1515` | 把 `client_name`/`client_version` 并入 keyword 匹配（最小改动，立刻「搜得到」）。 |
| CSV 导出含 client | — | CSV 表头**无** client_name/client_version | **[MISSING]** | `LOG:1651-1695`（表头 + 15 列） | CSV 追加 `client_name,client_version` 两列。 |
| JSON 导出含 client | — | `serde_json::to_value(l)` 全字段序列化，含 client | **[OK]** | `LOG:1549-1600` | — |
| 看板客户端榜 | A2A report 有 client 视角 | `TopClients` + `by_client` 聚合 | **[OK]** | `TopClients.tsx`；`ADMINCID:1-21`（`top_clients` + by_client/clients 维） | — |
| 识别规则管理 | A2A「设置→客户端识别」 | `ClientIdPanel`（内置目录展示 + 自定义规则 CRUD + 预览） | **[OK]** | `Settings.tsx:1214-1215`；`CIP`；`ADMINCID:112-161` | — |

> 说明：`FE:478` 的 `needsClientFilter` 变量名有误导——它实际决定「服务端导出不支持 status/keyword 时是否需客户端侧二次过滤」，**不是**「客户端筛选」。真正缺的是按客户端维度的筛选/搜索（见上表两条 MISSING/DIFF）。

---

## 4. 审计日志（record_audit）是否需要 client 维度

| 维度 | 现状 | 判定 | 证据 | 建议 |
|---|---|---|---|---|
| AuditLog 字段 | id/admin_id/action/target/before/after/**ip**/created_at；**无 client** | **[MISSING]（可选）** | `LOG:741-767`；`common.rs:286-297`（`record_audit` 无 ip/client 入参；另有 `record_audit_with_ip`） | 审计记录的是**管理员管理操作**（浏览器发起的 API），与「数据面客户端识别」不是同一维度；A2A 无同构的管理审计，故本项**非 1:1 强制**。若为溯源，可选给 AuditLog 加 `client_name`/`user_agent` 或至少复用 `record_audit_with_ip`。建议本轮**低优先/可选**。 |
| 审计前端列 | 审计 Tab 列：管理员/时间/操作/目标/变更；无 client | **[MISSING]（可选）** | `FE:184-193`（`logsAuditColDefs`） | 若后端补 client 字段再补列；否则不动。 |

> 结论：**「审计里查不到客户端」这条**，A2A 侧并无对应的「审计×客户端」维度可对齐（A2A 是桌面单机、审计即本地变更记录）。建议把它理解为：①审计页可加 `ip`（已有后端入口）溯源；②若确需，再给 AuditLog 加 client。核心用户诉求应落在**请求日志**的 client 维度（§2+§3），而非管理员审计。

---

## 5. 实施清单（本次审计的改动点，供派实施 → v1.0.93）

### P0（直接解决「查不到客户端」）
1. **自身请求标识（7 处）**：`client_id.rs` 加 `SELF_CLIENT="AIGX"` + `self_identity()`；改 `agent/llm.rs:514`、`playground.rs:98/175`、`legacy.rs:4384/4445`、`model_test.rs:790` 为 `AIGX`；`assistant.rs:226` 构造后 `attach(&self_identity())`。
2. **补目录（9 家）**：`CID:96` 补 dsh/magpie/hermes/hindsight/workbuddy/codebuddy/crush/factory-droid/goose（按 A2A products）。
3. **关键词搜得到 client**：`LOG:1499-1516` keyword 增加 `client_name`/`client_version` 匹配。

### P1（「查」的完整度）
4. **客户端筛选**：后端 `LQ:83` 加 `client` 字段 + `LOG` 过滤按 client_name；前端筛选行加客户端下拉。
5. **CSV 导出补列**：`LOG:1651-1695` 追加 `client_name,client_version`。
6. **保留自名**：`AIGX`（及系统自用名）加入自定义客户端保留校验，禁止规则/自定义客户端占用（对齐 A2A `catalog.rs:165`）。

### P2（口径对齐 / 可诊断性）
7. **声明名归一**：对齐 A2A「声明须命中已注册客户端」或至少加 `X-Client-Name` 白名单/开关（当前接受任意安全串）。
8. **超-UA 身份头**：可选识别 `x-title`/`x-client-type`/`x-cerebras-3rd-party-integration`（Cline）。
9. **候选/证据持久化**：`RequestLog` 可加 `client_source`（declared/custom_rule/builtin_ua/internal/unknown）或 `client_candidate`，把 `attach` 里丢弃的 `source` 落库，排障时能看到「像哪个客户端」。
10. **歧义/边界**：可选引入 ambiguous→other 与括号深度边界（A2A 口径），否则记录差异。
11. **SDK 口径**：`OpenAI/Anthropic SDK/LangChain` 是否降级为 generic（对齐 A2A 不把通用 SDK 当客户端），供运营拍板。
12. **（可选）审计**：AuditLog 加 client/UA 或复用 `record_audit_with_ip`；前端审计 Tab 补列。

---

## 附录：数据面 attach 覆盖核查（无需改动）

数据面各模块 `RequestLog::new()` 数与 `client_id::attach()` 数一一对应，无「建了日志却没 attach」的漏点：
anthropic.rs 3/3、openai/audio.rs 6/6、openai/billing.rs 3/3、openai/chat.rs 4/4、openai/completions.rs 2/2、openai/embeddings.rs 2/2、openai/images.rs 4/4、openai/rerank.rs 2/2、openai/responses.rs 3/3。`identify(&headers)` 在各 handler 入口取头，`attach` 在各 `RequestLog::new()` 分支（成功/失败/流式）写入，覆盖完整。

`RequestLog::new()` 的其余调用点：`routing_workspace.rs:1744/2103`、`dashboard.rs:1654`、`diagnostics.rs:390`、`logs.rs:1437` 均为**测试夹具**（`fn bkt_log`/`fn log_with`/`#[cfg(test)]`），非生产自身请求，不需处理。`log/mod.rs:710` 是 `RequestLog::new()` 本体默认值。

## 说明
- 行号基于 2026-11-23 工作区；后续改动可能漂移。
- A2A 为桌面单用户客户端，其「客户端识别」用于**冒充上游 UA / 报表**；AIGX 用于**请求日志归属**。两者目标相近但存储面不同（A2A 存 `ClientEvidence` 多维证据，AIGX 只存 name+version），是本轮多数 DIFF 的根源。
- 本审计仅比对两侧代码可见字段/行为，未运行；标「可选」项不阻塞 v1.0.93。
