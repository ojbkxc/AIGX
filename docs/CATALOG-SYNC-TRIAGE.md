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

---

## 9. #65 codearts 签名器落地方案 + 离线向量（v1.0.91）

> 结论先行：**AIGX 已有一份可用的 SDK-HMAC-SHA256 签名器** `src/bridge/session/codearts.rs::CodeArtsBridge::sign`（chat + 签到在用）。#65 **不新造**，而是「提取为可复用的签名函数 + 修两处与上游契约不一致的编码 + 目录侧接上 + 三源合并」。**代码待 lead 说「v1.0.90 已推送」后落**，本节省只做设计 + 离线向量。

### 9.1 现有实现 vs 上游契约（A2A `codearts/signer.rs`）

| 点 | A2A 契约（需对齐） | 现有 AIGX (`bridge/session/codearts.rs`) | 结论 |
|---|---|---|---|
| 未保留字符集 | JS `encodeURIComponent`：`A-Za-z0-9-_.!~*'()`（**含 `! * ' ( )`**） | `:557-569` 只留 `A-Za-z0-9-._~`（缺 `!*'()`） | **须修**（值/查询含这些字符即签名不一致 401） |
| canonicalQuery 排序 | 按 `(encodedKey, rawValueBytes)` | `:588-615` 按 `(encodedKey, encodedValue)` 字符串 | 修（同键多值时才有差；对齐 A2A） |
| canonicalURI | 先 `path_unescape` 再逐段重编码 + 强制尾 `/` | `:572-585` 用 `Url::path()` 直接编码 | 对纯 ASCII 路径等价；带 `%` 转义时 A2A 先解码。**目录端点（`/v1/model/builtin` 等）无转义，现状可用**；实现时按 A2A 先解码更稳 |
| Host 头 | 按上游开关：snap-access **不带**、福利网关**带** | `:80-88` 恒带 Host | 可工作（SignedHeaders 自洽）；实现目录时按源区分 `include_host` |
| X-Domain-Id | 非空才带（benefit 网关 domainless 不带） | 不带 | 目录实现补（snap-access 带；benefit 不带） |
| 空体摘要 | 非 PUT/PATCH/POST 恒 `sha256("")` | pass body（GET 传空即等价） | 等价 |
| 算法/头形状 | `SDK-HMAC-SHA256 Access=<AK>, SignedHeaders=…, Signature=…` | 一致 | 复用 |

### 9.2 落地步骤（待落代码）

1. **提取签名函数**：把 `CodeArtsBridge::sign` 的算法体抽成 `pub(crate) fn sign_v4(method, url, headers, body, ak, sk) -> Result<Authorization, String>`（放 `bridge/session/codearts.rs` 或新 `bridge/session/codearts_signer.rs`），`CodeArtsBridge::sign` 改调它；`catalog.rs` 复用。**修正 §9.1 的 `!*'()` 字符集 + 查询值排序**。
2. **凭证映射**（import 后）：`AK = session.credential.secret_plain()`（导入时 `access_key_id` 落 secret，见 `import.rs` 的 `A2A_SECRET_KEYS` 含 `access_key_id`）、`SK = field_plain("secret_access_key")`、`STS = field_plain("security_token")`、`Domain = field_plain("domain_id")`。AK/SK 缺一 → 不签、报「凭据不完整」。
3. **目录三源**（`catalog.rs` 新增 `CatalogSign::CodeArts` 变体，按源发请求并合并，先到先得）：
   - `agent`：`GET {base}/v1/agent-center/agents/useragents?offset=..&limit=100&is_primary_agent=true&supported_client=VSCODE&min_compatible_plugin_version=26.9.101` → 取 `agent_id` 列表 → 逐个 `GET .../detail?agent_id=<urlencoded>`（`Agent-Type: AgentCenter`）→ `gpts.models`；trial 账号 detail 200 但无 `gpts.models` → **不硬编码兜底**。
   - `builtin`：`GET {base}/v1/model/builtin`（`Agent-Type: PromptCenter`）。
   - `benefit`：先 `GET {base}/v1/benefit-gateway-config`（`PromptCenter`）判总开关；开着再 `GET {benefit_base}/api/v1/gateway/config`（**include_host=true、无 X-Domain-Id、无 Agent-Type**）。
   - 默认头（全部参与签名）：`Content-Type: application/json`、`Accept: application/json`、`X-Language: en-us`、`plugin-name: snap_vscode`、`plugin-version: 26.9.101`、`client_version: Vscode_26.9.101`、`is_confidential: false`（+ 各源 `Agent-Type`），签名器注入 `X-Sdk-Date`。
   - `base = https://snap-access.cn-north-4.myhuaweicloud.com`；benefit base = `https://opengw.developer.huaweicloud.com`。
4. **registry**：codearts 的 `headers: &[("x-auth-token","{credential}"),…]`（`registry.rs:805`）不再用于目录——目录走签名；chat 走 `CodeArtsBridge`。保留 registry 头只影响 generic adapter（codearts 是 bridge 型，chat 不经它）。
5. **单测**：用 §9.3 向量做**离线**断言（不联网）——比对 `canonical_request / string_to_sign / signed_headers / signature / Authorization` 与错误用例「凭据不完整」；三源合并用 fixture（复用 A2A `catalog_fixtures`）。

### 9.3 离线签名向量（固定输入 → 期望输出）

> 由独立脚本按 A2A `signer.rs` 的 Go 语义现算（`D:\tmp\codearts_vectors_gen.py`，不入仓）。凭据为**测试占位**（非真实）。落地时把本 JSON 存为 `src/bridge/session/codearts_signer_vectors.json` 并在单测里 `include_str!` 逐字段比对。

```json
[
  {
    "name": "builtin_get",
    "method": "GET",
    "url": "https://snap-access.cn-north-4.myhuaweicloud.com/v1/model/builtin",
    "headers": {"Content-Type":"application/json","Accept":"application/json","X-Language":"en-us","plugin-name":"snap_vscode","plugin-version":"26.9.101","client_version":"Vscode_26.9.101","is_confidential":"false","Agent-Type":"PromptCenter","X-Sdk-Date":"20260101T000000Z"},
    "body_b64": "", "include_host": false,
    "access_key": "HUAWEI_AK_TEST_0001", "secret_key": "sk-test-secret-0123456789abcdef",
    "security_token": "sts-test-token-xyz", "domain_id": "domain-test-0001",
    "canonical_request": "GET\n/v1/model/builtin/\n\naccept:application/json\nagent-type:PromptCenter\nclient_version:Vscode_26.9.101\ncontent-type:application/json\nis_confidential:false\nplugin-name:snap_vscode\nplugin-version:26.9.101\nx-domain-id:domain-test-0001\nx-language:en-us\nx-sdk-date:20260101T000000Z\nx-security-token:sts-test-token-xyz\n\naccept;agent-type;client_version;content-type;is_confidential;plugin-name;plugin-version;x-domain-id;x-language;x-sdk-date;x-security-token\ne3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
    "string_to_sign": "SDK-HMAC-SHA256\n20260101T000000Z\n30091f3e5f922b4d95ed6b18995bc007cebc430dbee83b0801763bfbb6e87d09",
    "signed_headers": "accept;agent-type;client_version;content-type;is_confidential;plugin-name;plugin-version;x-domain-id;x-language;x-sdk-date;x-security-token",
    "signature": "adab4b38bc40c488c61ea3a334d114970e1c224f2644b3996adeac1e141389b3",
    "authorization": "SDK-HMAC-SHA256 Access=HUAWEI_AK_TEST_0001, SignedHeaders=accept;agent-type;client_version;content-type;is_confidential;plugin-name;plugin-version;x-domain-id;x-language;x-sdk-date;x-security-token, Signature=adab4b38bc40c488c61ea3a334d114970e1c224f2644b3996adeac1e141389b3"
  },
  {
    "name": "chat_post",
    "method": "POST",
    "url": "https://snap-access.cn-north-4.myhuaweicloud.com/api/v2/chat/completions",
    "headers": {"Content-Type":"application/json","Accept":"text/event-stream","client_version":"Vscode_26.9.101","Agent-Type":"ChatAgent","X-Language":"en-us","is_confidential":"false","plugin-name":"snap_vscode","plugin-version":"26.9.101","model-id":"glm-5.3-flash","model-name":"glm-5.3-flash","x-model-id":"glm-5.3-flash","X-Sdk-Date":"20260101T000000Z"},
    "body_b64": "eyJtb2RlbCI6ImdsbS01LjMtZmxhc2giLCJzdHJlYW0iOmZhbHNlLCJtZXNzYWdlcyI6W3sicm9sZSI6InVzZXIiLCJjb250ZW50IjoiaGkifV19",
    "include_host": false,
    "access_key": "HUAWEI_AK_TEST_0001", "secret_key": "sk-test-secret-0123456789abcdef",
    "security_token": "sts-test-token-xyz", "domain_id": "domain-test-0001",
    "canonical_request": "POST\n/api/v2/chat/completions/\n\naccept:text/event-stream\nagent-type:ChatAgent\nclient_version:Vscode_26.9.101\ncontent-type:application/json\nis_confidential:false\nmodel-id:glm-5.3-flash\nmodel-name:glm-5.3-flash\nplugin-name:snap_vscode\nplugin-version:26.9.101\nx-domain-id:domain-test-0001\nx-language:en-us\nx-model-id:glm-5.3-flash\nx-sdk-date:20260101T000000Z\nx-security-token:sts-test-token-xyz\n\naccept;agent-type;client_version;content-type;is_confidential;model-id;model-name;plugin-name;plugin-version;x-domain-id;x-language;x-model-id;x-sdk-date;x-security-token\n1874667df95f2fe031c6714ed37018091e9cd4cefd57fe297e3fc4ff92619d18",
    "string_to_sign": "SDK-HMAC-SHA256\n20260101T000000Z\n4eca681dd746b986cd42c969b552d5e263c58c7a870026e27f887fb52a7670ed",
    "signed_headers": "accept;agent-type;client_version;content-type;is_confidential;model-id;model-name;plugin-name;plugin-version;x-domain-id;x-language;x-model-id;x-sdk-date;x-security-token",
    "signature": "127a304454f5875e66732e236b91c6fb2b3c659b689d0acf6d17136d7125fd0b",
    "authorization": "SDK-HMAC-SHA256 Access=HUAWEI_AK_TEST_0001, SignedHeaders=accept;agent-type;client_version;content-type;is_confidential;model-id;model-name;plugin-name;plugin-version;x-domain-id;x-language;x-model-id;x-sdk-date;x-security-token, Signature=127a304454f5875e66732e236b91c6fb2b3c659b689d0acf6d17136d7125fd0b"
  },
  {
    "name": "agent_detail_query_get",
    "method": "GET",
    "url": "https://snap-access.cn-north-4.myhuaweicloud.com/v1/agent-center/agents/detail?agent_id=agent-123&b=2&a=1",
    "headers": {"Content-Type":"application/json","Accept":"application/json","X-Language":"en-us","plugin-name":"snap_vscode","plugin-version":"26.9.101","client_version":"Vscode_26.9.101","is_confidential":"false","Agent-Type":"AgentCenter","X-Sdk-Date":"20260101T000000Z"},
    "body_b64": "", "include_host": false,
    "access_key": "HUAWEI_AK_TEST_0001", "secret_key": "sk-test-secret-0123456789abcdef",
    "security_token": "sts-test-token-xyz", "domain_id": "domain-test-0001",
    "canonical_request": "GET\n/v1/agent-center/agents/detail/\na=1&agent_id=agent-123&b=2\naccept:application/json\nagent-type:AgentCenter\nclient_version:Vscode_26.9.101\ncontent-type:application/json\nis_confidential:false\nplugin-name:snap_vscode\nplugin-version:26.9.101\nx-domain-id:domain-test-0001\nx-language:en-us\nx-sdk-date:20260101T000000Z\nx-security-token:sts-test-token-xyz\n\naccept;agent-type;client_version;content-type;is_confidential;plugin-name;plugin-version;x-domain-id;x-language;x-sdk-date;x-security-token\ne3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
    "string_to_sign": "SDK-HMAC-SHA256\n20260101T000000Z\n9c501ed95523c102ee723d6a094179c0acaf2ff2465d48e61e810e740279fef8",
    "signed_headers": "accept;agent-type;client_version;content-type;is_confidential;plugin-name;plugin-version;x-domain-id;x-language;x-sdk-date;x-security-token",
    "signature": "0e3fadcaf52722be624302dde335559bffdfa2d331716e34b104d246187c826b",
    "authorization": "SDK-HMAC-SHA256 Access=HUAWEI_AK_TEST_0001, SignedHeaders=accept;agent-type;client_version;content-type;is_confidential;plugin-name;plugin-version;x-domain-id;x-language;x-sdk-date;x-security-token, Signature=0e3fadcaf52722be624302dde335559bffdfa2d331716e34b104d246187c826b"
  },
  {
    "name": "benefit_gateway_host_get",
    "method": "GET",
    "url": "https://opengw.developer.huaweicloud.com/api/v1/gateway/config",
    "headers": {"Content-Type":"application/json","Accept":"application/json","X-Language":"en-us","plugin-name":"snap_vscode","plugin-version":"26.9.101","client_version":"Vscode_26.9.101","is_confidential":"false","X-Sdk-Date":"20260101T000000Z"},
    "body_b64": "", "include_host": true,
    "access_key": "HUAWEI_AK_TEST_0001", "secret_key": "sk-test-secret-0123456789abcdef",
    "security_token": "sts-test-token-xyz", "domain_id": "",
    "canonical_request": "GET\n/api/v1/gateway/config/\n\naccept:application/json\nclient_version:Vscode_26.9.101\ncontent-type:application/json\nhost:opengw.developer.huaweicloud.com\nis_confidential:false\nplugin-name:snap_vscode\nplugin-version:26.9.101\nx-language:en-us\nx-sdk-date:20260101T000000Z\nx-security-token:sts-test-token-xyz\n\naccept;client_version;content-type;host;is_confidential;plugin-name;plugin-version;x-language;x-sdk-date;x-security-token\ne3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
    "string_to_sign": "SDK-HMAC-SHA256\n20260101T000000Z\n830573424c039a3789fce6675aedd74fed102ab2bbed82a1cdf4d18a75e0ee31",
    "signed_headers": "accept;client_version;content-type;host;is_confidential;plugin-name;plugin-version;x-language;x-sdk-date;x-security-token",
    "signature": "726b06aa2770ae2595cd8286493dacee25167c2e3dcd7ba1ba94d10986fb1c19",
    "authorization": "SDK-HMAC-SHA256 Access=HUAWEI_AK_TEST_0001, SignedHeaders=accept;client_version;content-type;host;is_confidential;plugin-name;plugin-version;x-language;x-sdk-date;x-security-token, Signature=726b06aa2770ae2595cd8286493dacee25167c2e3dcd7ba1ba94d10986fb1c19"
  },
  {
    "name": "query_encoding_contract",
    "method": "GET",
    "url": "https://snap-access.cn-north-4.myhuaweicloud.com/v1/x?q=a!b*c'd(e)+f%20g&z=%2F&a=1",
    "headers": {"Content-Type":"application/json","Accept":"application/json","X-Language":"en-us","plugin-name":"snap_vscode","plugin-version":"26.9.101","client_version":"Vscode_26.9.101","is_confidential":"false","Agent-Type":"PromptCenter","X-Sdk-Date":"20260101T000000Z"},
    "body_b64": "", "include_host": false,
    "access_key": "HUAWEI_AK_TEST_0001", "secret_key": "sk-test-secret-0123456789abcdef",
    "security_token": "sts-test-token-xyz", "domain_id": "domain-test-0001",
    "canonical_request": "GET\n/v1/x/\na=1&q=a!b*c'd(e)%20f%20g&z=%2F\naccept:application/json\nagent-type:PromptCenter\nclient_version:Vscode_26.9.101\ncontent-type:application/json\nis_confidential:false\nplugin-name:snap_vscode\nplugin-version:26.9.101\nx-domain-id:domain-test-0001\nx-language:en-us\nx-sdk-date:20260101T000000Z\nx-security-token:sts-test-token-xyz\n\naccept;agent-type;client_version;content-type;is_confidential;plugin-name;plugin-version;x-domain-id;x-language;x-sdk-date;x-security-token\ne3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
    "string_to_sign": "SDK-HMAC-SHA256\n20260101T000000Z\n46547c1c4ec0ba33448649bc5fa0c26be1cb48ccd46cf59bac03dc9263995acc",
    "signed_headers": "accept;agent-type;client_version;content-type;is_confidential;plugin-name;plugin-version;x-domain-id;x-language;x-sdk-date;x-security-token",
    "signature": "d4774abbad92a5fc36cea5c1abfd85b79444b6bafbc3de01ed1a84982cbfdb5f",
    "authorization": "SDK-HMAC-SHA256 Access=HUAWEI_AK_TEST_0001, SignedHeaders=accept;agent-type;client_version;content-type;is_confidential;plugin-name;plugin-version;x-domain-id;x-language;x-sdk-date;x-security-token, Signature=d4774abbad92a5fc36cea5c1abfd85b79444b6bafbc3de01ed1a84982cbfdb5f"
  },
  {
    "name": "missing_sk",
    "method": "GET",
    "url": "https://x.example/v1/model/builtin",
    "expected_error_kind": "incomplete_credential",
    "access_key": "only-ak", "secret_key": "", "security_token": "", "domain_id": ""
  }
]
```

> 关键锚点：`query_encoding_contract` 的 canonical query 必须是
> `a=1&q=a!b*c'd(e)%20f%20g&z=%2F`——即 `! * ' ( )` **不转义**、空格→`%20`、`/`→`%2F`。
> 现有 `encode_component`（只留 `-._~`）会把它算错，这条向量专门钉这个差异。

### 9.4 生产复验（v1.0.91 部署后）

- 需要：至少 1 条 enabled codearts 账号（AK/SK/STS 新鲜）。
- 步骤：`POST /api/account-sessions/catalog/sync`（`force=true`），预期 2xx 且目录含 `agent`/`builtin`（及福利开启时的 `benefit`）来源的模型；再发一条最小对话（`POST /v1/chat/completions`，model 取目录里的名），预期 200。
- 若 401：先确认 STS 是否过期（24h），再核对 `SignedHeaders` 是否与上游收到的头一致（尤其 `!*'()` 编码与 Host/domain 头取舍）。

