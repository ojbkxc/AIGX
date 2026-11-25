# Trae 通道：客户端指纹与请求形状探针矩阵

> 探针日期：2026-11-25（UTC+8）
> 目标：查清 `{"code":"11-128","msg":"request illegal","displayTips":"这次调用没通过渠道校验，可能不是官方客户端发出的"}` 到底缺哪个指纹；顺带把「哪些头必需、哪些多余」一次性钉死。
> 方法：真实凭据直打 `https://trae-api-cn.mchost.guru/api/agent/v3/llm_utils_chat`，**一次只改一个变量**。
> 结论：**指纹头早就齐了，11-128 不是这条链路产生的。** 真正的 4001 来自 `config_name` 被全局模型映射改写成内部 ID（见 §3.8、§3.10）。
> 纪律：本文不含任何 token / deviceId / machineId / uid 真值，一律 `<token>` 占位。

---

## 0. 结论速览

| # | 问题 | 结论 |
|---|---|---|
| 1 | `6eefa01c-1036-4c7e-9ca5-d891f63bfcd8` 是哪个头的值 | **`x-app-id`**，恒定值，32 条成功请求全一样 |
| 2 | 缺它会怎样 | 缺 → `400 4001 … expr_path=app_id, missing required parameter`；值换错 → `200 2001 … record not found`。**必需** |
| 3 | `x-app-version-code` 从哪来 | 客户端版本号，**不是必需**（删掉仍 200 出真回复）；同值 `x-ide-version-code` 才是必需项 |
| 4 | `devicePublicKey` / `devicePrivateKey` 怎么用 | **对话链路完全不用**。删/换 `x-device-id`、`x-machine-id`、`x-uid` 全部 200 出真回复 |
| 5 | 有没有请求体签名 | **没有**。body 里无 signature 字段，头里无 HMAC；`request_id`/`session_id` 只是追踪用 |
| 6 | 11-128 从哪来 | **不是 Trae 侧**。trae.wasm 里 0 次出现；60+ 组探针一次都没复现（§4） |
| 7 | 9 个账号全失败的真正原因 | `kv.model_mappings` 把 `deepseek-v4.1-flash` 改写成 `sn-deepseek-v4-1-flash-1017` 发给上游 → 4001（§3.8） |

---

## 1. 取证来源（为什么这份表可信）

### 1.1 主证据：A2A 的 `debug_traffic` 表

`/opt/llm-proxy/Agent2API/data/agent2api.db` 的 `debug_traffic` 表里存着 **32 条 trae 成功请求（`status=200`）的完整 `request_headers` JSON**。这是官方客户端实际发出的东西 —— 比反汇编 wasm 硬，因为它是运行时事实而不是常量池。

取最新一条，**20 个头**（token / device / machine 由 A2A 自己 redact 成 `[redacted]`，其余值可见）：

```
accept              = text/event-stream
content-type        = application/json
request-traffic-type= prod
user-agent          = Trae/0.1.69
x-app-id            = 6eefa01c-1036-4c7e-9ca5-d891f63bfcd8
x-app-version       = 0.1.69
x-app-version-code  = 20260917
x-device-type       = linux
x-ide-version       = 0.1.69
x-ide-version-code  = 20260917
x-ide-version-type  = stable
x-plugin-channel    = icube-ai
x-uid               = <账号 uid，未 redact>
x-request-id        = <uuid>
x-trae-request-id   = <同 x-request-id>
authorization       = [redacted]
x-cloudide-token    = [redacted]
x-ide-token         = [redacted]
x-device-id         = [redacted]
x-machine-id        = [redacted]
```

对应的请求体（形状，字符串截断）：

```json
{
  "config_name": "deepseek-v4.1-flash",
  "function": "chat_v3",
  "max_tokens": 2000,
  "messages": [{"content": [{"text": "...", "type": "text"}], "role": "user"}],
  "model": "deepseek-v4.1-flash",
  "model_name": "deepseek-v4.1-flash__max",
  "request_id": "<uuid>",
  "session_id": "[redacted]",
  "stream": true,
  "temperature": 0.2,
  "user_message_context": {"model_info": {"prompt_max_tokens": 998000}}
}
```

**关键否定证据**：成功请求里**没有** `x-user-region`、`accept-language`、`package-type`、`x-lgw-req-sdk-type`、`x-market-client-id`、`sec-fetch-*`、`x-tt-trace-id`、`x-os-version`、`x-device-brand`。这些是 wasm 常量池里的**别名残留**，官方客户端并不发。

### 1.2 wasm 字符串的真实形态（为什么不能直接读邻接）

`trae.wasm` 0.1.1（A2A 与 AIGX 两份内容一致，md5 同为 865361 字节）的数据段是**首尾相接的常量池**，`strings` 会把它们糊成一长条：

```
715904  */*user-agentVSCode 1.107.1 (TRAE SOLO CN)VSCode 1.107.1 (TRAE CN)x-user-regionCN
        accept-languagepackage-typestable_cnx-lgw-req-sdk-type3x-market-client-idVSCode 1.107.1
        x-device-typex-app-versionapp-versionx-request-idx-tt-trace-idsec-fetch-destempty…
716264  x-cloudide-tokenx-ide-token6eefa01c-1036-4c7e-9ca5-d891f63bfcd8x-ide-version
        x-ide-version-code20260917x-app-version-codex-ide-version-typestablex-trae-request-id
        x-plugin-channelicube-airequest-traffic-typeprodx-uidx-machine-id
```

按偏移和邻接**只能推断「这些名字都在池子里」**，不能推断「哪个名字配哪个值、哪个真的被发出去」——`x-app-version` 后面紧跟 `app-version`（别名），`x-app-version-code` 后面紧跟 `x-ide-version-type`（值被前一个键抢走），都是这个糊法的产物。**判定必须靠运行时头，不能靠 wasm 邻接。**

### 1.3 与 AIGX 当前实现的差异

`src/account_session/registry.rs` 的 trae 头表（已含 `x-app-id`）：

```
x-app-id         = 6eefa01c-1036-4c7e-9ca5-d891f63bfcd8
x-app-version    = default          ← 与官方 0.1.69 不同，实测无影响
x-app-version-code = 20260820
x-ide-version    = 0.1.61           ← 与官方 0.1.69 不同，实测无影响
x-ide-version-code = 20260820
x-ide-version-type = stable
x-device-type    = windows          ← 官方是 linux，实测无影响
x-os-version     = Windows 11 Pro   ← 官方不发
x-device-brand   = 83DG             ← 官方不发
request-traffic-type = prod
user-agent       = Trae/0.1.61      ← 官方 0.1.69，实测无影响
```

`src/bridge/session/trae.rs` 的 `DEFAULT_HEADERS` 又叠了一层（`accept-language` / `x-user-region` / `package-type` / `x-lgw-req-sdk-type` / `x-market-client-id` / `sec-fetch-*`），每请求再生成 `x-request-id` / `x-trae-request-id` / `x-tt-trace-id`。**这 33 个头实测全部不影响成败**（§3.3–§3.7）。

---

## 2. 必需 / 非必需一览

| 头 | 必需？ | 缺失时的响应 |
|---|---|---|
| `authorization`（+ `x-cloudide-token` + `x-ide-token` 至少一处在） | **必需** | 三个全去掉 → `401 {"code":1001,"message":"…not able to authenticate you"}` |
| `x-app-id` | **必需** | `400 4001 bad request: binding: expr_path=app_id, cause=missing required parameter` |
| `x-ide-version-code` | **必需** | `400 4001 bad request: binding: expr_path=app_version_code, cause=missing required parameter` |
| `x-app-version-code` | 非必需 | 删掉仍 200 出真回复 |
| `x-app-version` / `x-ide-version` / `x-ide-version-type` | 非必需 | 删掉或换值均 200 |
| `x-uid` | 非必需 | 删掉 / 换值均 200 出真回复 |
| `x-device-id` | 非必需 | 删掉 / 换值均 200 出真回复 |
| `x-machine-id` | 非必需 | 删掉 / 换值均 200 出真回复 |
| `x-device-type` | 非必需 | `ide` / `linux` / `windows` 均 200 |
| `x-plugin-channel` / `request-traffic-type` | 非必需 | 删掉仍 200 |
| `x-request-id` / `x-trae-request-id` | 非必需 | 删掉仍 200（但**保留**：真实客户端每请求都换，复用一个值会被当成重放） |
| `x-tt-trace-id` / `x-user-region` / `accept-language` / `package-type` / `x-lgw-req-sdk-type` / `x-market-client-id` / `sec-fetch-*` / `x-os-version` / `x-device-brand` | **多余**（官方不发） | 实测无影响，见 §3.3 |
| `user-agent` | 非必需（但 `curl` 形态会拿空流） | 见 §3.3 |

> **「多余」≠「有害」**：这些头实测不影响 200，删掉要重新验证一遍。留着无害、删了要重新验证，收益不对等，**故本次不动代码，仅在此标注**。

---

## 3. 逐组探针结果

统一判据：`content=YES` = 流里出现 `"response"` 字段（真实回复）；否则记流内 `event:error` 的 code。

### 3.1 `x-app-id`（必需项，也是那串固定 UUID 的归属）

| 改动 | 结果 |
|---|---|
| 原值 `6eefa01c-1036-4c7e-9ca5-d891f63bfcd8` | 200，出真回复 |
| **删掉** | **400** `{"code":4001,"message":"bad request: binding: expr_path=app_id, cause=missing required parameter"}` |
| 换 `00000000-0000-0000-0000-000000000000` | 200 流内 `2001 failed to do chat: failed to get app config: record not found` |
| 换随机 UUID | 同上 `2001` |
| 换 `trae` | 同上 `2001` |
| 换 `1` | 同上 `2001` |

**读法**：缺 → 参数校验层直接 400；值错 → 过了校验但查不到 app 配置（2001）。所以这串 UUID 是**应用身份**，不是设备身份，也不是 clientId。

### 3.2 `x-ide-version-code` / `x-app-version-code`（版本码）

| 改动 | 结果 |
|---|---|
| `x-ide-version-code` = `20260820` | 200，真回复 |
| `x-ide-version-code` = `20260917` | 200，真回复 |
| `x-ide-version-code` = `20261101` | 200，真回复 |
| `x-ide-version-code` = `20260101` | 200 流内 `4001 param is invalid` |
| `x-ide-version-code` = `0` / `1` / `20250101` | 200 流内 `4001` |
| **删掉 `x-ide-version-code`** | **400** `expr_path=app_version_code, cause=missing required parameter` |
| `x-app-version-code` = `20260820` / `20260917` / **删掉** | 200，真回复 |

**读法**：上游要的是「一个够新的版本码」，太旧（20260101 及以前）被拒；`x-app-version-code` 不是校验项。

### 3.3 `user-agent`（换什么都没事，除了 curl 形态）

| 值 | 结果 |
|---|---|
| `Trae/0.1.69` / `Trae/0.1.61` | 200，真回复 |
| `VSCode 1.107.1 (TRAE SOLO CN)` | 200，真回复 |
| `VSCode 1.107.1 (TRAE CN)` | 200，真回复 |
| `Trae/1.0 antigravity-cockpit-tools` | 200，真回复 |
| `Mozilla/5.0` | 200，真回复 |
| `curl/8.5.0` | 200 但**空流**（无 content，非报错） |
| 删掉 UA | 200 但**空流** |

**读法**：UA 不是鉴权门槛；只有「明显非客户端」的 UA（curl / 缺省）会拿到空流。**这条直接推翻了「按 variant 分两套 UA 才能过」的假设**（见 §6）。

### 3.4 三个 token 头 / 认证形态

| 改动 | 结果 |
|---|---|
| `authorization: Cloud-IDE-JWT <token>`（官方形态） | 200，真回复 |
| `authorization: Bearer <token>` | 200，真回复 |
| `authorization: <token>`（裸 token） | 200，真回复 |
| `authorization: Cloud-IDE-JWT`（无 token 值） | 200，真回复（另两个头带着 token） |
| **只删 `authorization`** | 200，真回复（`x-cloudide-token` / `x-ide-token` 仍生效） |
| **三个 token 头全删** | **401** `{"code":1001,"message":"…not able to authenticate you…"}` |
| 三个头全填 `xxx` | 401 同 1001 |
| token 截断到前 40 字符 | 200，真回复 |

**读法**：三个头是**或**关系，有一处带真 token 就过；全缺/全错才 401。**401 与 11-128 是不同层**。

### 3.5 `base_url`（404 的真正来源）

| 站 | 结果 |
|---|---|
| `https://trae-api-cn.mchost.guru` | 200，真回复 |
| `https://api.trae.cn` | **404** `<html><center><h1>404 Not Found</h1></center><hr><center>TLB</center>` |
| `https://api.trae.com.cn` | **404** 同上 TLB |

**读法**：`api.trae.cn` / `api.trae.com.cn` 是网页/控制台站，由字节 TLB 直接回 404，与路径无关。AIGX 的 `adapter::canonical_trae_agent_base` 已把这两个站归一到 agent 站。

### 3.6 身份头去留（`x-uid` / `x-device-id` / `x-machine-id`）

| 改动 | 结果 |
|---|---|
| 全带（官方形态） | 200，真回复 |
| **删 `x-uid`** | 200，真回复 |
| **删 `x-device-id`** | 200，真回复 |
| **删 `x-machine-id`** | 200，真回复 |
| `x-uid` 换成 `9999999999999999` | 200，真回复 |
| `x-device-id` 换成 `0000000000000000` | 200，真回复 |
| `x-machine-id` 换成随机 UUID | 200，真回复 |

**读法**：**这三个不是门槛，换值也不校验。** 因此 `devicePublicKey` / `devicePrivateKey`（凭证里存了但我们从没用）**不参与对话签名** —— 它们只服务于 OAuth 换码（`ExchangeToken` 的 `DeviceInfo.DevicePublicKey`），对话链路完全不需要。

### 3.7 `variant` / `x-device-type`

| 改动 | 结果 |
|---|---|
| `x-device-type` = `ide` | 200，真回复 |
| `x-device-type` = `linux` | 200，真回复 |
| `x-device-type` = `windows` | 200，真回复 |
| 删 `x-device-type` | 200，真回复 |
| 删 `x-plugin-channel` / `request-traffic-type` | 200，真回复 |

### 3.8 `config_name`（决定性 —— 线上 9 个账号全失败的真正原因）

| `config_name` | 结果 |
|---|---|
| `deepseek-v4.1-flash` | **200，真回复 `"你好！有什么"`** |
| `sn-deepseek-v4-1-flash-1017` | **200 流内 `4001 We're sorry, the param is invalid`** ← 线上今天发的是这个 |
| `deepseek-v4-1-flash-1017` | 200 流内 `4001` |
| `deepseek-v4-1-flash` | 200 流内 `4001` |
| `DeepSeek-V4.1-Flash`（大小写不同） | 200 流内 `4001` |
| `DeepSeek-V4-Pro` | **200，真回复** |
| `deepseek-v4-pro-0813` | 200 流内 `4001` |
| 空字符串 | 200，真回复（上游按 usage 兜底） |
| `glm-5.3-flash` | **200，真回复** |
| `sn-glm-5-3-flash` | 200 流内 `4001` |
| `GLM-5.3-Flash` | 200 流内 `4001` |

**根因链**：`kv.model_mappings` 有两条 `provider: null`（对所有供应商生效）的全局映射：

```
deepseek-v4.1-flash  ->  sn-deepseek-v4-1-flash-1017
glm-5.3-flash        ->  sn-glm-5-3-flash
```

`trae.rs::build_body` 原来把 `config_name` 直接取 `req.model`，于是把**内部 ID** 发给了 Trae。Trae 拿它查自己的 `get_detail_param` 清单，查不到就流内 `4001`；failover 再把 `4001` 当「账号坏了」逐个打进冷却 —— 好账号被一条映射表配置打死。

**修正后验证**（归一函数产出的名字，用同一 token 再打一遍）：

| 输入 | `resolve_config_name` 产出 | 上游结果 |
|---|---|---|
| `sn-deepseek-v4-1-flash-1017` | `deepseek-v4.1-flash` | **200 `"你好！有什么"`** |
| `sn-glm-5-3-flash` | `glm-5.3-flash` | **200** |

### 3.9 `model` 字段（不参与查表）

`config_name` 固定为真名 `deepseek-v4.1-flash`，只改 `model`：

| `model` | 结果 |
|---|---|
| `deepseek-v4.1-flash` | 200，真回复 |
| `sn-deepseek-v4-1-flash-1017` | 200，真回复 |
| `DeepSeek-V4.1-Flash` | 200，真回复 |
| 空 | 200，真回复 |

**读法**：`model` 不参与上游 config 查表，所以修复只归一 `config_name`，`model` 保持调用方原样 —— 日志与计费口径不变。

### 3.10 `function` 四个候选

| `function` | 结果 |
|---|---|
| `chat_v3` | 200，真回复 |
| `solo_agent` | 200，真回复 |
| `solo_agent_lite` | 200，真回复 |
| `solo_work_lite` | 200，真回复 |
| `solo_work_remote` | 200，真回复 |
| `solo_coder` | 200 流内 `4001`（该 function 的模型表不含 `deepseek-v4.1-flash`） |

**例外（值得记）**：`function=solo_work_lite` + `model_name=deepseek-v4.1-flash__max` 曾得到 `4023 {"message":"We're sorry, the model is unknown."}` —— `__max` 后缀与该 function 的目录不一致时会被拒。`solo_work_lite` + `model_name` 缺省 / `__dev` 则正常。

### 3.11 body 形状

| 改动 | 结果 |
|---|---|
| 官方完整形态（含 `request_id` / `session_id` / `user_message_context`） | 200，真回复 |
| 去掉 `request_id` / `session_id` / `user_message_context` | 200，真回复 |
| `max_tokens` = 2000 / 1000000 | 均 200 |
| `model_name` = `__max` / `__dev` / 缺省 | 均 200（配合 `chat_v3`） |

**读法**：body 白名单比想象宽；`request_id` / `session_id` 只是追踪，不是签名。

### 3.12 畸形请求（对照，用于区分错误层）

| 改动 | 结果 |
|---|---|
| 空 body | 200 流内 `2001 [LLMUtilsChat.resolveByUsage] function is empty, cannot resolve model by usage=` |
| 非 JSON（`hello world`） | **400** `{"code":4001,"message":"bad request: invalid character 'h' looking for beginning of value"}` |
| JSON 数组（`[]`） | **400** `{"code":4001,"message":"bad request: json: cannot unmarshal array into Go value of type ideagent.LLMUtilsChatRequest"}` |
| 有 `function` 无 `messages` | 200 流内 `4001 param is invalid` |

### 3.13 路径与方法

| 请求 | 结果 |
|---|---|
| `POST /api/agent/v3/llm_utils_chat` | 200 |
| `GET /api/agent/v3/llm_utils_chat` | 404 `Not Found` |
| `POST /api/agent/v3/chat` | 404 `Not Found` |
| `POST /v1/chat/completions`（打 agent 站） | 404 TLB |

**读法**：`/api/agent/v3/*` 只有 `llm_utils_chat` 一条（wasm 里也只有这一个串，偏移 705904）。

---

## 4. 11-128 的排除（为什么它不是指纹问题）

### 4.1 静态证据

| 位置 | `11-128` 出现次数 |
|---|---|
| `trae.wasm` 0.1.1（A2A 那份） | **0** |
| `trae.wasm` 0.1.1（AIGX 那份） | **0** |
| `workbuddy.wasm` 0.1.0 | 1（是 workbuddy 自己的错误码表） |
| `/opt/aigx/aigx` 二进制 | 2 处，**都在 Claude Code 模板字符串区**（紧邻 `github.com/anthropics/`、`You are Claude Code…`），是巧合子串，不是 Trae 判据 |

### 4.2 运行时证据

| 证据 | 内容 |
|---|---|
| PG 里 9 个 trae 账号的 `last_error` | 只有 `404 TLB` 与 `502 [4001] the param is invalid`，**从来没有 11-128** |
| `journalctl -u aigx` 全部 trae 失败行 | 同上，无 11-128 |
| `request_logs` 表 | 空（该表当前无写入），也无 11-128 |
| 60+ 组探针（UA / 头 / body / 路径 / 方法 / 认证） | **一次都没拿到 11-128** |

### 4.3 判定

`11-128` 更像是上游对**某个旧版客户端形态**的拦截，在 AIGX 当前这套头下已不再触发。它与 `401 code 1001`（认证）、`4001`（参数/模型）、`404 TLB`（打错站）、`2001`（app 配置/function 缺失）**分属不同层**，不应混为一谈。

---

## 5. 90 个上游真名清单：来源与重取

### 5.1 来源

`POST https://trae-api-cn.mchost.guru/api/ide/v1/get_detail_param`，body `{"function": "<fn>"}`，读 `config_info_list[].config_name`：

| `function` | 条数 |
|---|---|
| `solo_work_lite` | 54 |
| `chat_v3` | 65 |
| `solo_agent` | 75 |
| **三者并集去重** | **90** |

代码里 `trae.rs::TRAE_CONFIG_NAMES` 用的就是这 90 个的并集（避免只认一条 function 时把别的 function 的模型挡掉）。

### 5.2 什么时候该重取

- 上游**加新模型**（新模型名不在表里 → `resolve_config_name` 会 warn 并原样发出 → 4001）；
- `resolve_config_name` 打出 `trae: 模型 '…' 不在上游 config_name 清单里` 这条 warn 时；
- 换 `function` 时（不同 function 的清单不同，见上表）。

### 5.3 重取方法（骨架，`<token>` / `<uid>` 等用真实值代入，勿入库）

```python
import json, uuid, urllib.request

URL = "https://trae-api-cn.mchost.guru/api/ide/v1/get_detail_param"
H = {
    "authorization": "Cloud-IDE-JWT <token>",
    "x-cloudide-token": "<token>",
    "x-ide-token": "<token>",
    "x-app-id": "6eefa01c-1036-4c7e-9ca5-d891f63bfcd8",   # 必需
    "x-ide-version-code": "20260917",                     # 必需，且要够新
    "x-ide-version": "0.1.69",
    "x-app-version": "0.1.69",
    "x-device-type": "linux",
    "x-uid": "<uid>", "x-device-id": "<deviceId>", "x-machine-id": "<machineId>",
    "user-agent": "Trae/0.1.69",
    "content-type": "application/json",
    "accept": "application/json",
}
names = set()
for fn in ("solo_work_lite", "chat_v3", "solo_agent"):
    req = urllib.request.Request(
        URL, data=json.dumps({"function": fn}).encode(), headers=H, method="POST"
    )
    d = json.loads(urllib.request.urlopen(req, timeout=30).read())
    lst = d.get("data", {}).get("config_info_list") or d.get("config_info_list") or []
    names |= {c["config_name"] for c in lst if c.get("config_name")}
print(len(names))            # 期望 90（上游加模型后会变多）
print(sorted(names))
```

拿到新清单后，替换 `trae.rs::TRAE_CONFIG_NAMES` 并同步本文 §5.1 的条数。

---

## 6. 已知「基于错误假设写的代码」（评审时可简化，本次不动）

### 6.1 `variant` → 两套 UA 的分流没有实际作用

`trae.rs` 里按凭证 `variant` 选 `VSCode 1.107.1 (TRAE SOLO CN)` 或 `VSCode 1.107.1 (TRAE CN)`。但 §3.3 实测：**UA 换成任何客户端形态都是 200 出真回复**，只有 `curl/8.5.0` 或删掉 UA 才拿空流。

结论：这套分流**不产生正确性收益**，只是「贴近真实客户端形态」的写法。将来若简化，把 UA 固定成 `Trae/0.1.69`（官方值）即可，`variant` 只留给 OAuth 换码用（`client_id` 与 `PlatformCode` 那里是真需要区分的）。

### 6.2 那批「多余的头」留着无害

见 §2 末尾。删掉要重新验证一遍，收益不对等 —— **本次不动代码**。

---

## 7. 附：本次修复对应的回归点

| 改动 | 位置 | 判据 |
|---|---|---|
| `config_name` 三级归一（真名原样 / `sn-…` 反解 / 认不出原样发出 + warn） | `src/bridge/session/trae.rs::resolve_config_name` | 6 条单测：内部 ID 反解、真名零涟漪、`model` 字段不变、大小写兜底、认不出不替换、空模型默认值在表内 |
| 90 个真名清单 | `src/bridge/session/trae.rs::TRAE_CONFIG_NAMES` | 见 §5 |
| 凭证缺 `variant` 时显式给 `solo` | `src/account_session/adapter.rs` | 免得 adapter 与 trae.rs 两处缺省口径打架 |
| 身份头注释纠正到实测口径 | `src/account_session/adapter.rs` | 原文写「不发这些头会被判非法客户端」，实测是「只有 `x-app-id` 与 `x-ide-version-code` 是硬门槛」 |
