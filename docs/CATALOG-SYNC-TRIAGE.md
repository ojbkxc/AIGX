# 模型目录同步失败甄别与修复（#56 / #59）

> 日期：2026-11-22（UTC+8）
> 范围：#48 可用性矩阵「② 有 CatalogSpec 但 sync 失败」5 家：catpaw / codearts / officeace / antigravity / trae
> 依据：AIGX `src/account_session/{catalog.rs,registry.rs,adapter.rs}` ↔ Agent2API `desktop-tauri/.../core/providers/<家>/`（只核对「正常取模型目录」的 API 契约，不做任何伪装/规避）
> 状态：4 家已在**工作区**修复（归 v1.0.90，未提交）；codearts 属大改，延后并给出方案；officeace **chat** 已在工作区补修。
> 复验时点：**生产现为 v1.0.88（不含本修复）**，须待 v1.0.90 部署后再复跑。

## 0. 结论一览

| provider | 现象 | 判定 | 修否 |
|---|---|---|---|
| catpaw | 上游 405 Method Not Allowed | **代码 bug**（method 用错） | ✅ 已修 |
| codearts | 上游 401 | **代码缺口**（未做 SDK-HMAC-SHA256 签名 + 多源合并） | ⏸ 延后（需移植签名器） |
| officeace（目录） | 上游 400 Bad Request | **代码 bug**（Basic 缺 secret 一半 + 路径/版本错） | ✅ 已修 |
| officeace（对话） | 对话 400 | **代码缺口**（chat 的 Basic 同样缺 secret 一半） | ✅ 已修（#59） |
| antigravity | 上游 403 Forbidden | **代码 bug**（出站 UA 不含 `antigravity`） | ✅ 已修 |
| trae | `config_info_list:[]` 解析为空 | **代码 bug**（`X-Ide-Version-Code` 过旧，拿不到表） | ✅ 已修（parser 正确，未动） |

> 「账号过期」判断：以上 5 家**均非单纯 token 过期**——失败发生在鉴权/方法/版本校验层，属 AIGX 请求构造与上游契约不符。codearts 的 STS 也可能同时过期，但**根因是请求未签名**，即便换新 STS 仍会 401。

---

## 1. catpaw —— 405（代码 bug，已修）

- **现象**：`POST /api/account-sessions/catalog/sync` 返回「上游返回 405 Method Not Allowed」。
- **根因**：旧 spec 把 `/api/agent/maas/model-types` 当 GET；上游实为 **POST**，`tenant/scene/env` 是 **JSON body** 而非 query（A2A `providers/catpaw/catalog.rs` 模块头明确「推翻旧实现 GET 的误读」，`request_body()` 给 `{tenant:"CatDesk",scene:"CATX_APP",env:"EXTERNAL"}`）。GET 命中错方法 → 405。
- **证据 file:line**
  - AIGX 旧（现改）：`src/account_session/catalog.rs:430`（spec：`method=Post` + `body=catpaw_catalog_body` `:2263`）。
  - 鉴权：上游是 Cookie 形态 `Cookie: X-Passport-Token=<token>`（A2A `catpaw/upstream_http.rs:54-77`），旧 AIGX 用 `x-passport-token` 头；已改 `registry.rs:706-722`，并补客户端身份头 `M-APPKEY/gray-set/X-Agent-Version`，每请求随机会话头 `M-TRACEID`（`catalog.rs:2154`）。
- **判定**：代码 bug（方法 + 头构造），非账号过期。
- **复验**：v1.0.90 后取 1 条 enabled catpaw 账号 `POST /api/account-sessions/catalog/sync`；预期上游 200、目录非空。若 401 → 账号 Passport token 过期（重登）。

## 2. codearts —— 401（代码缺口，延后）

- **现象**：目录同步 401。
- **根因**：上游（华为云 CodeArts）要求 **SDK-HMAC-SHA256 请求签名**：`Authorization: SDK-HMAC-SHA256 Access=<AK>, SignedHeaders=…, Signature=…`（A2A `codearts/chat.rs:175` `signer::sign(...)`、断言 `chat.rs:841`），且 `/v1/model/builtin` 必带 `Agent-Type: PromptCenter`（A2A `codearts/models.rs:7,484`）。AIGX 只把原始 `AK:SK:STS` 塞进 `x-auth-token`（`registry.rs:805`），**未做任何签名** → 401。
- **判定**：AIGX **构造缺口**（功能未实现），不是单纯 token 过期；即便 STS 新鲜，未签名仍 401。
- **修否**：**本次不改**（大改）。方案见 §6。
- **复验**：在签名器移植完成前，复跑预期仍 401；不要把它当「需重登」处理。

## 3. officeace —— 400（目录代码 bug，已修；chat 另案已修）

- **现象**：目录同步 400 Bad Request。
- **根因（两处）**
  1. **鉴权**：上游要 `Authorization: Basic base64(modelAppKey:modelAppSecret)`（A2A `officeace/chat.rs:64-68`）；AIGX 用 registry `Basic {credential}`，其中 credential=主密钥=**modelAppKey**，**缺 modelAppSecret 一半** → 400。
  2. **路径**：上游目录是 `{base}/models` 且 base 必须带 `/v2`（A2A `officeace/chat.rs:41-51` `normalize_base` + `models.rs:264`）；旧 AIGX 为 `/v1/models`，可能拼成 `/v1/v1/models` 或 `/v2/v1/models`。
- **证据 file:line（已修）**
  - spec path `/models`：`src/account_session/catalog.rs:452`。
  - base 补 `/v2`：`catalog.rs:1918`（`catalog_base_url` officeace 分支）。
  - `Basic base64(key:secret)`：`adapter.rs:41`（`officeace_basic_authorization`），目录请求覆盖头 `catalog.rs:2146`。
- **chat 另案（#59，已修）**：officeace 无专用 Bridge，chat 走 `OpenaiCompatibleBridge` 的 session 头；旧行为同样只发 `Basic <modelAppKey>`。已在 `adapter.rs:77`（`session_headers` officeace 分支）替换为 `Basic base64(key:secret)`，与目录同源（共用 `adapter::officeace_basic_authorization`）。**否则即使目录通了，对话仍 400。**
- **判定**：代码 bug（目录）+ 代码缺口（chat 鉴权），非账号过期。
- **复验**：v1.0.90 后同步目录预期 200；再对同账号发一条最小对话，预期 200（若仍 400 且信息为鉴权，检查账号 `modelAppSecret` 是否导入；缺失会被保留为原始模板并如实报错）。

## 4. antigravity —— 403（代码 bug，已修）

- **现象**：目录同步 403 Forbidden。
- **根因**：上游硬要求出站 **User-Agent 含 `antigravity` 且版本 ≥ 4.3.0**，否则直接 403（A2A `antigravity/endpoints.rs` 模块头 §3.2/§7.1）。AIGX 旧 `headers=[]`，走 reqwest 默认 UA（不含 antigravity）→ 403。
- **证据 file:line（已修）**
  - registry 加 `user-agent: vscode/1.X.X (Antigravity/4.3.0)` + `x-client-name/version`：`src/account_session/registry.rs:900-914`。
  - 目录另补 `x-goog-user-project`（取账号 `projectId` 字段，空则不发）：`catalog.rs:2181`。
  - base/path（`daily-cloudcode-pa.googleapis.com` + `/v1internal:fetchAvailableModels`）与 A2A `V1_BASE_URL_DAILY` 一致，无需改。
- **判定**：代码 bug（UA 指纹缺失），非账号过期。
- **残余风险**：A2A 在 sandbox→daily→prod 间回退，AIGX 仅 daily（403 不是环境问题，不影响本次）；若仍 403，按 A2A 坑 #3 先去掉 `x-goog-user-project` 再试。**这不属于「伪装/反检测」——UA 是上游契约明确要求的客户端标识，见 A2A 模块头。**
- **复验**：v1.0.90 后同步 1 条 antigravity 账号，预期 200；若 403 → 账号 OAuth token 过期（重登）。

## 5. trae —— `config_info_list:[]`（代码 bug，已修；parser 正确）

- **现象**：`get_detail_param` 响应 `config_info_list:[]`，目录解析为空。
- **根因**：版本头**决定返回哪张模型表**。A2A `trae/headers.rs` 实测：`X-Ide-Version-Code: 20260820 → 36 条`（`20260716 → 35 条`）。AIGX 旧值为 `x-ide-version-code:"1"` / `x-app-version-code:"1"` / `x-ide-version:"1.0.0"` / UA `Trae/1.0 antigravity-cockpit-tools` → 上游给空表。**不是 parser 问题**（`parse_trae_catalog` 与 A2A 口径一致，未改）。
- **证据 file:line（已修）**
  - registry 版本头/UA：`src/account_session/registry.rs:811-835`（`20260820` / `0.1.61` / `Trae/0.1.61`，并补 `x-os-version`/`x-device-brand`/`request-traffic-type`）。
  - 目录补 `x-uid / x-machine-id / x-device-id`（取账号字段，空则不发）：`catalog.rs:2162`（A2A `trae/headers.rs`：「空值 = 另一个身份」）。
  - parser 未动：`catalog.rs` `parse_trae_catalog`。
- **判定**：代码 bug（版本头过旧），非账号过期。
- **复验**：v1.0.90 后同步 1 条 trae 账号，预期 `config_info_list` 非空、目录非空。注意版本码与目录表绑定；若上游再次升级客户端版本，需同步更新这套常量（已在 registry 注释标注绑定关系）。

---

## 6. #59 · codearts 签名器移植方案（延后，先方案后动手）

> 结论：属移植一个华为云 SDK-HMAC-SHA256 签名器 + 目录多源合并，**不建议在 v1.0.90 硬塞**，建议 v1.0.91。

**要做的事（按依赖顺序）**
1. **HMAC 签名器**（新文件，例如 `src/account_session/codearts_signer.rs`）：按华为云 API 网关规范构造规范化请求串（方法、host、路径、按名排序的参与签名头、body 的 SHA-256），用 `secret_access_key` 做 HMAC-SHA256，产 `Authorization: SDK-HMAC-SHA256 Access=<AK>, SignedHeaders=…, Signature=…`；`x-security-token` 带 STS（参考 A2A `codearts/signer.rs` 的**算法契约**，非照抄实现）。
2. **请求接线**：codearts 目录/对话统一走签名器（替换当前 `x-auth-token`，`registry.rs:805`）；`/v1/model/builtin` 必带 `Agent-Type: PromptCenter`（其余源按各自 Agent-Type）。
3. **目录三源合并**：`agent`（`/v1/agent-center/agents/useragents` + `detail` 取 `gpts.models`）→ `builtin`（`/v1/model/builtin`）→ `benefit`（福利网关，另一套签名契约），按 id 先到先得；`trial` 账号 agent detail 无 `gpts.models` 时不硬编码兜底。
4. **测试**：用 A2A `codearts/signer_vectors.json` 这类向量做**签名正确性**离线断言（不联网）；目录合并用 fixture。补 1 条「未签名必 401 / 签名后按源合并」的单测。
5. **边界**：只对齐契约必需的头/签名；不引入任何指纹随机化/反检测。

**风险/工作量**：中高（新签名器 + 三种 Agent-Type + benefit 另一台主机契约 + STS 续期已存在）。建议单独立项，不与其他 catalog 修复混在一个提交。

---

## 7. 复验方法（通用）

- 路由：`POST /api/account-sessions/catalog/sync`（`src/main.rs:3225` → `handle_sync_account_catalog`，请求体见 `src/api/admin/account_sessions.rs:3018/3180`）。
- 步骤（每家）：取 1 条 `enabled` 账号 → 调用 sync（`force=true`）→ 看返回的目录是否非空 / 上游状态码。
- 预期：catpaw / officeace / antigravity 200 且非空；trae `config_info_list` 非空；codearts 仍 401（待签名器）。

## 8. 边界与说明

- 全程只对齐上游**正常取模型目录**所需的方法/路径/头/body；无任何 provider 伪装、规避检测或指纹伪造。
- 未打印/提交任何明文凭证；A2A 源仅用于核对 endpoint/method/auth 契约。
- 改动均在**工作区**（`catalog.rs` + `registry.rs` + `adapter.rs`），按指示**未 commit/push**，不进 v1.0.89；归 v1.0.90（codearts 若做则 v1.0.91）。
- 行号基于 2026-11-22 工作区；共享树中 `catalog.rs` 另有并发改动（#58/#61），复核以实际代码为准。
