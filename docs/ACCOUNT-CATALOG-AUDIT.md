# 账号池 30 家 provider 模型目录实现审计（#40）

> 审计日期：2026-11-22
> 审计对象：AIGX `src/account_session/catalog.rs`（spec_for 32 个分支 / 9 张兜底表 / 6 个专用解析器）
> 覆盖范围：`registry.rs::AGENT2API_PROVIDERS` 全 29 家（含 intl 变体）+ AIGX 自有 `deepseek-web` = **30 家**
> 只读审计，未改动任何代码；本文件是 #40 唯一改动。

## 0. 结论摘要

| 判定 | 家数 | 说明 |
|---|---:|---|
| 正确 | 9 | 静态兜底家（按设计无远程）8 + 小浣熊 parser（结构正确，线上 404 属端点问题）1 |
| 疑似字段错 | 2 | cline-free / cline-pass（`contextWindow`/`reasoning` 未识别，见 §2） |
| 无兜底（层级/端点响应形态风险） | 12 | 有远程端点、`fallback` 为空、无生产账号可验证 |
| 无法判定（上游插件，需真实账号实测） | 7 | 有专用解析器但上游契约在闭源插件 |
| 端点风险 | 1 | 小浣熊线上 404（与 parser 无关） |
| **合计** | **30**（+1 AIGX `cline` 别名并入 cline-free/pass 行） | |

| 优先级 | 家数 | 含义 |
|---|---:|---|
| P0 | 1 | 唯一在产且目录当前为空（小浣熊端点 404），需真实账号排障 |
| P1 | 14 | 无兜底且响应形态未验证（一家出错=整家空目录），或账号状态阻断 |
| P2 | 15 | 有兜底 / 静态兜底 / 未接目录（按设计），时效抽查或等插件端点 |

## 1. 方法与证据边界（务必先读）

**关键事实：A2A 参考树里，各家模型目录的「响应解析」实现已整体搬进未发布的 `agent2api-providers` 插件，主仓只剩身份与旧缓存归一。** 直接证据：

- `core/providers/accio/mod.rs:4`：「转发…、**模型目录**、额度、OAuth 登录、续期**全在插件里**」
- `core/providers/autoclaw/mod.rs:10`：「模型路由 + 远程目录 `plugins/autoclaw/models.rs + catalog.rs`」
- `core/providers/cline/models.rs:3-6`：「凭证格式、设备授权、续期、**目录拉取**…全在 `agent2api-providers` 里（不随本仓库发布）」
- `core/providers/loomy/mod.rs:4`、`modelscope/mod.rs:4`、`monkeycode/mod.rs:4`、`qoder/endpoints.rs:3-6`、`workbuddy/mod.rs:5`、`zcode/mod.rs:5-9` 同款自述
- `core/providers/catalog.rs:82-83`：目录元数据走 `plugin_host::model_catalog_meta`，即运行时由插件供给

因此，除非参考树留有**旧缓存归一**或**身份字段**证据，否则「AIGX 读的字段 vs 上游真实字段」**无法在参考树内证实**，本表一律标「无法判定（上游闭源插件）」，并给出「需真实账号实测」的动作，不臆造字段。

**唯一可静态比对的字段级证据**是 `core/providers/catalog_legacy.rs`（插件化前旧缓存条目的归一规则，反映当时上游真实字段）：

- 小浣熊 `:58-84`：`id ← name`、`name ← displayName`、`contextWindow→maxInputTokens`、`maxTokens→maxOutputTokens`、`supportImage→supportsImages`、`supportThinking→supportsReasoning`
- Cline `:85-119`：模型对象含 `id` / `pool` / `contextWindow` / `reasoning` / `supportsToolCall`
- chatglm `:121-125`：`id ← selected_model`、`name ← display_name`、`reasoningEfforts ← reasoning_efforts`

### 1.1 三类问题（对齐 lead #41 线上事实）

| 类别 | 定义 | 本次归属 |
|---|---|---|
| ① 远程端点 404/不可达 | 端点过期、路径/版本变更、权限或鉴权头不符 | 小浣熊（线上 `上游返回 404 Not Found`，余额接口正常）；其余待实测 |
| ② JSON 结构/字段错 | parser 读错层级或字段名（小浣熊历史缺陷属此类） | 小浣熊 parser **已修正**（结构对齐）；cline 疑似字段错；无兜底家为潜在 ② |
| ③ 账号状态 | `disabled` / `error_count` 高，非目录缺陷 | workbuddy（`configured_status=disabled`、`error_count=18`，models 非空） |

### 1.2 「可静态判定」vs「需真实账号实测」

- **可静态判定**：`spec_for` 分支（path/method/parser/fallback/sign）、parser 代码结构、参考树旧缓存归一字段、单测 fixture。
- **需真实账号实测**：上游远程响应的真实 JSON 形态与 HTTP 状态——因为解析实现已搬入闭源插件，参考树无契约。生产仅 `workbuddy`、`xiaohuanxiong` 两个账号（lead #41）；其余 28 家无账号，只能标「需实测」。

## 2. 逐 provider 差距表

> 列口径：AIGX spec 引 `catalog.rs` 行号；A2A 引参考树 `file:line`，闭源插件标「插件不可得」。`parser` 列 `None` 表示走通用 `parse_response`（openai_shape 见路径说明）。

| provider | AIGX spec（path / method / parser / fallback） | A2A 行为（文件:行） | 判定 | 类别 | 可静态判定? | 修复动作 | 优先级 |
|---|---|---|---|---|---|---|---|
| workbuddy | `path=""` / GET(无) / None / WORKBUDDY_FALLBACK(23) `catalog.rs:642-650` | `workbuddy/mod.rs:1-16`、`region.rs:5`（`/v3/config` 目录在插件）；参考树种子见 `core/models/mod.rs::builtin_models` | 正确（静态兜底，按设计无远程） | ③ 账号状态 | 是（静态）；兜底时效需抽查 | 目录无需改；处理账号 `disabled`/`error_count=18`（重登/排错） | P1（账号状态） |
| workbuddy-intl | 同上（两地共用一份种子）`catalog.rs:642-650` | `workbuddy/region.rs:5,96-105`（插件） | 正确（静态兜底） | — | 是 | 兜底时效抽查；无生产账号 | P2 |
| xiaohuanxiong（A2A: raccoon） | `/model_catalog` / GET / **Raccoon** / RACCOON_FALLBACK(5) `catalog.rs:554-562`；parser `:2345-2444`；测试 `:3172-3218` | `catalog_legacy.rs:58-84`（`id←name`、`contextWindow/maxTokens/supportImage/supportThinking`）；解析实现本体在插件 | parser **正确**（`data.categories[].models[].name`、params 两级、缺省 reasoning=true 均对齐）；线上空目录是端点 404 | ① 端点 404 | parser 是；端点否（需实测 base_url/路径/头） | **不要改 parser**；用真实账号核对默认 base_url 与 `/model_catalog` 是否仍存在（可能版本/路径/鉴权变更），抓真实状态与响应 | **P0** |
| catpaw | `/api/agent/maas/model-types` / GET / None（openai_shape=false）/ 空 `catalog.rs:423-431` | 插件不可得 | 无法判定（插件）+ **无兜底**（通用 parser 只认 `models`/`data`/顶层数组；多一层即空） | ② 潜在 | 否（需抓响应） | 生产账号抓真实 JSON；若非顶层数组→加专用 parser + 兜底表 | P1 |
| autoclaw | `path=""` / — / None / AUTOCLAW_FALLBACK `catalog.rs:485-493` | `autoclaw/mod.rs:10,27`、`region.rs:4`（插件静态表） | 正确（静态兜底，上游无可拉接口） | — | 是 | 兜底时效抽查 | P2 |
| autoclaw-intl | 同上 `catalog.rs:485-493` | `autoclaw/region.rs:4`（插件） | 正确（静态兜底） | — | 是 | 兜底时效抽查 | P2 |
| qoder | `/algo/api/v2/model/list?Encode=1` / GET / Cosy→`parse_qoder_chat` / QODER_CN_FALLBACK `catalog.rs:453-461`；parser `:2477-2521`；测试 `:2929-2963` | `qoder/endpoints.rs:3-6`（COSY 签名/目录/转发全在插件） | 无法判定（插件）；实现自洽且有兜底 | ② 潜在 | 否（需实测 COSY 签名目录） | 真实账号实测 `chat[]`/`key`/`display_name`；有兜底，风险低 | P2 |
| qoder-intl | 同上（INTL 兜底）`catalog.rs:462-470` | `qoder/endpoints.rs:3-6`（插件） | 无法判定（插件）；有兜底 | ② 潜在 | 否 | 同上 | P2 |
| cline-free（AIGX 别名 `cline` 同 spec） | `/ai/cline/recommended-models` / GET / None（openai_shape=false）/ CLINE_FALLBACK `catalog.rs:567-575`；上下文 `:70-86`；能力 `:93-156` | `cline/models.rs:39-45,121-166`（池前缀/短名）+ **`catalog_legacy.rs:85-119`（旧缓存字段 `id`/`pool`/`contextWindow`/`reasoning`/`supportsToolCall`）** | **疑似字段错**：`pick_context_length` 不认 `contextWindow`（驼峰无下划线）、`pick_capabilities` 不认裸 `reasoning` → 上下文/推理位丢失；`id` 识别正确 | ② 潜在 | 部分（字段来自参考树旧缓存证据；远程现由插件，需实测确认仍用旧键） | 在 `pick_context_length` 增 `"contextWindow"`、`pick_capabilities` 增 `("reasoning","reasoning")`；或在 Cline 分支加专用 parser | P2（有兜底） |
| cline-pass | 同上 `catalog.rs:567-575` | 同上 | 同上（疑似字段错） | ② 潜在 | 部分 | 同上 | P2 |
| accio | `/api/llm/config` / **POST** / Accio / 空 `catalog.rs:542-550`；parser `:2226-2282`；测试 `:3016-3033` | `accio/mod.rs:4`、`endpoints.rs:4`（Gemini 信封+目录全在插件） | 无法判定（插件）+ **无兜底** | ② 潜在 | 否（需实测 POST 响应） | 实测 `{data:[{modelList:[{modelCode,modelDisplayName,visible}]}]}`；补兜底表 | P1 |
| accio-cn | 同上 `catalog.rs:542-550` | `accio/endpoints.rs:4`（插件） | 无法判定（插件）+ 无兜底 | ② 潜在 | 否 | 同上 | P1 |
| zcode | `path=""` / — / None / ZCODE_FALLBACK `catalog.rs:507-515` | `zcode/mod.rs:1-13`（模型静态表与适配器全在插件） | 正确（静态兜底，按设计） | — | 是 | 兜底时效抽查（协议侧见 ACCOUNT-POOL-PARITY.md §1 L：后续） | P2 |
| zcode-intl | 同上 `catalog.rs:507-515` | `zcode/mod.rs:1-13`（插件） | 正确（静态兜底） | — | 是 | 兜底时效抽查 | P2 |
| codearts | `/v1/model/builtin` / GET / None（openai_shape=false）/ 空 `catalog.rs:432-440` | 插件不可得 | 无法判定（插件）+ **无兜底** | ② 潜在 | 否 | 抓真实响应；补 parser/兜底 | P1 |
| trae | `/api/ide/v1/get_detail_param` / **POST**(`trae_catalog_body`) / Trae / 空 `catalog.rs:520-528`；parser `:2122-2183`；测试 `:2965-2997` | 插件不可得（AIGX 注释称移植自插件 `providers/trae/models.rs`） | 无法判定（插件）+ **无兜底**（POST 且 `X-Ide-Version-Code` 决定返回哪张表） | ② 潜在 | 否（需真机/正确版本头） | 带正确 IDE 版本头实测 `config_info_list`；建议补最小兜底表 | P1 |
| loomy | `/api/v1/models` / GET / None（openai_shape=true）/ 内联兜底 2 条(spark-x、spark-x2.5) `catalog.rs:379-404` | `loomy/mod.rs:4`（插件） | 无法判定（插件）；有内联兜底 | ② 潜在 | 否 | 实测 OpenAI 形态；抽查 2 条兜底是否覆盖主力模型 | P2 |
| kuku | `/wenchain/genflowpro/model/list` / GET / **Kuku**(sign=Kuku) / 空 `catalog.rs:473-481`；parser `:2284-2333`；测试 `:3251-3272` | 插件不可得 | 无法判定（插件）+ **无兜底** | ② 潜在 | 否（需实测 STOKEN 三件套 + 响应） | 实测签名查询与 `status.code==0` 门控；补兜底表 | P1 |
| monkeycode | `/api/v1/users/models` / GET / None（openai_shape=false）/ 空 `catalog.rs:405-413` | `monkeycode/mod.rs:4`、`region.rs:4`（插件） | 无法判定（插件）+ **无兜底** | ② 潜在 | 否 | 抓真实响应；若非顶层数组→专用 parser + 兜底 | P1 |
| monkeycode-intl | 同上 `catalog.rs:405-413` | `monkeycode/region.rs:4`（插件） | 无法判定（插件）+ 无兜底 | ② 潜在 | 否 | 同上 | P1 |
| commandcode | `/provider/v1/models` / GET / None（openai_shape=true）/ 空 `catalog.rs:414-422` | 插件不可得 | 无法判定（插件）+ **无兜底**（若确为 OpenAI `{data:[{id}]}` 则风险低） | ② 潜在 | 否 | 实测；补兜底 | P1 |
| antigravity | `/v1internal:fetchAvailableModels` / **POST**(`{}`) / AntigravityModelsDict / 空 `catalog.rs:531-539`；parser `:2185-2224`；测试 `:2999-3014` | 插件不可得 | 无法判定（插件）+ **无兜底**（响应 `models` 字典非数组，专用 parser 已处理） | ② 潜在 | 否 | 实测 POST `{models:{id:{...}}}`；补兜底 | P1 |
| officeace | `/v1/models` / GET / None（openai_shape=true）/ 空 `catalog.rs:441-449` | 插件不可得 | 无法判定（插件）+ **无兜底** | ② 潜在 | 否 | 实测 OpenAI 形态；补兜底 | P1 |
| orcaterm | `path=""` / — / None / ORCATERM_FALLBACK `catalog.rs:496-504` | 插件不可得（AIGX 注释称移植自插件 `providers/orcaterm/models.rs MODELS`） | 正确（静态兜底，上游无可拉接口） | — | 是 | 兜底时效抽查 | P2 |
| chatglm | `path=""` / — / None / **空** → 不支持目录发现 `catalog.rs:591-599` | **`catalog_legacy.rs:121-125`（`id←selected_model`、`name←display_name`、`reasoningEfforts←reasoning_efforts`）**；端点本身在插件 | **无兜底**（按设计：端点未定）；未来接线若用通用 parser 会读不到 `selected_model`/`display_name` | ② 潜在（未来接线） | 是（path/fallback 空可静态确认） | 待插件端点确定后补 path + **专用 parser 读 `selected_model`/`display_name`** + 兜底表 | P2 |
| modelscope | `/api/v1/models` / GET / None（openai_shape=true）/ 空 `catalog.rs:578-586` | `modelscope/mod.rs:4`、`region.rs:4`（插件） | 无法判定（插件）+ **无兜底**（OpenAI 兼容形态，风险相对低） | ② 潜在 | 否 | 实测两站独立响应；补兜底 | P1 |
| modelscope-intl | 同上 `catalog.rs:578-586` | `modelscope/region.rs:4`（插件） | 无法判定（插件）+ 无兜底 | ② 潜在 | 否 | 同上 | P1 |
| atomcode | `path=""` / — / None / **空** → 不支持目录发现 `catalog.rs:602-610` | 插件不可得 | 无兜底（按设计：端点未定） | — | 是 | 待插件端点确定后补 path + 兜底 | P2 |
| lobsterai | `path=""` / — / None / **空** → 不支持目录发现 `catalog.rs:613-621` | 插件不可得 | 无兜底（按设计：端点未定） | — | 是 | 待插件端点确定后补 path + 兜底 | P2 |
| deepseek-web | `path=""` / — / None / DEEPSEEK_WEB_FALLBACK `catalog.rs:627-635` | AIGX 注释称对齐 ds2api `config/models.go` 静态表（网页版无 `/models`） | 正确（静态兜底，按设计） | — | 是 | 兜底时效抽查 | P2 |

## 3. 统计

### 3.1 按判定

| 判定 | provider |
|---|---|
| 正确（静态兜底，按设计） | workbuddy、workbuddy-intl、autoclaw、autoclaw-intl、zcode、zcode-intl、orcaterm、deepseek-web |
| 正确（parser，线上 404 另计） | xiaohuanxiong |
| 疑似字段错 | cline-free、cline-pass |
| 无法判定（插件）+ 无兜底 | catpaw、accio、accio-cn、codearts、trae、kuku、monkeycode、monkeycode-intl、commandcode、antigravity、officeace、modelscope、modelscope-intl |
| 无法判定（插件），有兜底 | qoder、qoder-intl |
| 无法判定（插件），有内联兜底 | loomy |
| 无兜底（按设计，端点未定） | chatglm、atomcode、lobsterai |

> 说明：cline-free 与 cline-pass 共用同一 spec，AIGX 另有 `"cline"` 别名并入两者，不单独计数。

### 3.2 按优先级

| 优先级 | 家数 | provider |
|---|---:|---|
| **P0** | 1 | xiaohuanxiong（端点 404，在产空目录） |
| **P1** | 14 | catpaw、accio、accio-cn、codearts、trae、kuku、monkeycode、monkeycode-intl、commandcode、antigravity、officeace、modelscope、modelscope-intl、workbuddy（账号状态） |
| **P2** | 15 | workbuddy-intl、autoclaw、autoclaw-intl、qoder、qoder-intl、cline-free、cline-pass、zcode、zcode-intl、loomy、orcaterm、chatglm、atomcode、lobsterai、deepseek-web |

## 4. 修复排序（建议执行顺序）

1. **P0 · 小浣熊端点排障（唯一在产空目录）** — 用真实账号核对默认 `base_url`（`https://xiaohuanxiong.com/api/web/llm/v2`）与 `/model_catalog` 是否仍存在、鉴权头/版本是否变更；线上当前 `404 Not Found`、余额正常 → 属①端点问题。**parser 已正确，不要动 `parse_raccoon_catalog`。**
2. **P1 · 无兜底 + 形态未验证批量实测** — 按风险分两组抓真实响应，确认通用/专用 parser 是否命中，未命中则补专用 parser + 兜底表：
   - (a) 通用 parser 潜在层级错（多半需专用 parser）：`catpaw`、`codearts`、`monkeycode`/`-intl`、`commandcode`、`officeace`、`modelscope`/`-intl`
   - (b) 特殊 method/sign：`trae`(POST+版本头)、`antigravity`(POST 字典)、`kuku`(STOKEN 签名)、`accio`/`accio-cn`(POST)
3. **P1 · workbuddy 账号状态** — 目录非空（23 条静态兜底），问题是 `disabled`/`error_count=18`，走账号运维（重登/排错），与目录代码无关。
4. **P2 · 字段级修正** — Cline 家族补 `contextWindow`（`pick_context_length`）与裸 `reasoning`（`pick_capabilities`）识别；chatglm 若日后接线，parser 必须读 `selected_model`/`display_name`。
5. **P2 · 兜底时效抽查** — 静态兜底与内联兜底家（autoclaw×2、orcaterm、zcode×2、deepseek-web、workbuddy-intl、loomy、qoder×2、cline×2）对照上游当前清单核对是否过期。
6. **P2 · 未接目录** — chatglm、atomcode、lobsterai 待插件端点确定后补 path+兜底（当前 UI 显示「不支持目录发现」，非缺陷）。

## 5. 说明

- 「正确」仅指 **AIGX 实现与其自述的真实形态一致**（parser 结构、静态兜底按设计）；不宣称上游当前仍返回该形态——参考树无契约（见 §1）。
- 本审计无法在参考树内证实 22 家远程 provider 的字段级正确性，因为解析实现整体在未发布的 `agent2api-providers` 插件；「疑似字段错」只对**参考树留有旧缓存证据**的 cline 家族下结论，其余标「无法判定 / 需实测」，不臆造。
- 小浣熊线上 `404` 与历史上「parser 读错结构导致空目录」是**两类不同问题**：历史结构缺陷已由 #34 修复并纳入单测（`catalog.rs:3172-3218`）；当前空目录由①端点 404 造成。
- 证据行号基于审计当日（2026-11-22）工作区；后续改动可能漂移，复核以实际代码为准。
