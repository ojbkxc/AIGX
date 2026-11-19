# AIGX × Agent2API v3.5.0-beta.2 差距矩阵（含 221 在途改动覆盖审计）

> **任务**：T1 —— 审计 AIGX 与 Agent2API `v3.5.0-beta.2` 的差距，并先审计 221 个在途未提交
> 改动，标出已被覆盖、**不要重复做**的项。
> **审计人**：auditor（只读审计，未改动任何源码）
> **审计日期**：2026-11-18
> **结论速览**：221 项在途改动中，**75 项是真实源码/前端改动，146 项是构建产物**；这 75 项
> 已经把 beta.2 的 **模型管理（M1–M5）、错误策略、运行容量、提示词规则、插件宿主 WASM 契约、
> 插件批量运维** 六大块**实质性覆盖**。**真正需要继续做的缺口集中在三处**：订阅类账号
> （Claude/ChatGPT/Codex）、面板实时事件 SSE、以及数据面/端点侧的接线收口。
>
> ⚠️ **审计期间发生 `git reset`（2026-11-19 11:31:39），221 项中的 56 个「已修改」文件被重置回
> HEAD**，19 个新增文件与前端新面板保留。**本矩阵对 221 基线的判定依然成立，但“接线类”结论
> 已随重置失效**，详见 §二.3 与 §十。

---

## 一、审计基线

| 项 | 取值 | 证据 |
|---|---|---|
| 参考上游 | `Agent2API` tag **v3.5.0-beta.2**（commit `0affd3af`） | `git show v3.5.0-beta.2:package.json` → `"version": "3.5.0-beta.2"` |
| 参考源码根 | `D:\GitHub\Agent2API`（另有只读检出 `D:\tmp\a2a-beta` @ `0affd3af`） | `git worktree list` |
| 上游 server 根 | `desktop-tauri/src-tauri/server/src/server/` | 目录结构 |
| 目标仓库 | `D:\GitHub\AIGX`，`main` @ `def61a2b`（v1.0.66） | `git log --oneline -3` |
| 目标源码根 | `AIGX/src/`、`AIGX/frontend/src/` | 目录结构 |
| 在途改动 | `git status --porcelain` = **221** 条 | 见 §二 |

### 方法

1. 先还原 **beta.1 → beta.2** 的上游真实增量：`git diff v3.5.0-beta.1 v3.5.0-beta.2`
   （**1086 文件 / +113 655 / −13 110**，其中 server Rust 新增文件清单已逐条列出）。
2. 再对 AIGX 的 **221 项在途改动**逐项归类，判定它对应 beta.2 的哪一条 / 哪个模块。
3. 判定口径分四档：

| 档位 | 含义 | 给 T2/T3/T4 的指令 |
|---|---|---|
| ✅ **已覆盖** | 契约 + 接线 + 前端俱全，含数据面消费 | **不要重复做** |
| 🟡 **部分覆盖** | 契约或开关已备，缺数据面 / 端点 / 聚合 | **只补接线，勿重写模块** |
| ❌ **未覆盖** | 无对应实现 | 需要实现 |
| ⛔ **不适用** | AIGX 架构上无对应面（如桌面端壳） | 明确理由后跳过 |

---

## 二、221 个在途改动的构成（先看清再动手）

### 2.1 构成总览

| 分类 | 数量 | 说明 |
|---|---|---|
| `static/assets/**`（构建产物） | 145 | Vite 重新哈希的 chunk/图标，`??` 新增 + `D` 删除成对出现 |
| `static/index.html` | 1 | 构建入口 |
| **非构建产物（真实改动）** | **75** | 前端 16 + Rust/Cargo 59 |

### 2.2 75 项真实改动的分布

**75 项真实改动**（19 个新增 `??` + 56 个修改 ` M`）：

- **前端 16**：`api/endpoints/{misc,plugins}.ts`、4 个新面板（`PromptRulesPanel` /
  `RoutingErrorsPanel` / `RuntimeCapacityPanel` / `WebToolsPanel`）、`ToolProviderPanel.{tsx,css}`、
  `pages/{Plugins,Plugins.css,Settings,Wallet}.tsx`、`i18n/{en,zh}.json`、`types/{a2a,index}.ts`
- **Rust/Cargo 59**：`src/runtime_capacity.rs`、`src/error_policy.rs`、`src/prompt.rs`、
  `src/model_key_source.rs`、`src/credential_version.rs`、`src/desktop_launch.rs`、
  `src/plugin_ledger.rs`、`src/accounts_list.rs`、`src/plugin_host/operations.rs`、
  `src/api/admin/accounts_query.rs`、`src/plugin_host/wasm/{account_origin,managed_task,verification}.rs`、
  `src/tool_services/provider_{keys,errors,request}.rs`（以上为新增）+ `main.rs`、`config.rs`、
  `bridge/account.rs`、`api/{anthropic,auth,tool_runtime}.rs`、`api/openai/{chat,models,resolve,responses}.rs`、
  `api/admin/{mod,settings,plugins,openapi,legacy,model_test,tools}.rs`、`model/{manage,mod}.rs`、
  `channel/mod.rs`、`user_group/mod.rs`、`plugin_host/{mod,install,manager}.rs`、
  `plugin_host/wasm/{mod,host,login,manifest,resources,services,state}.rs`、`tool_services/*`、
  `account_session/model.rs`、`update/mod.rs`、`image_compat/mod.rs`、`agent/registry/tests.rs`、
  `Cargo.toml`、`Cargo.lock`

> **给团队的直接结论**：`static/assets` 的 146 项是构建产物，**不要手工处理**；**不要对已判定
> ✅ 的模块重新开工**，那会造成与在途改动的冲突。

### 2.3 ⚠️ 审计期间工作区被 `git reset`（重大变更，务必先读）

审计进行中，工作区发生了 **`git reset`（mixed，`moving to HEAD`）**，时间与证据：

| 证据 | 值 |
|---|---|
| reflog | `2026-11-19 11:31:39 +0800: reset: moving to HEAD`（`HEAD@{0}`） |
| 受影响文件 mtime | `src/main.rs`、`src/api/admin/settings.rs` 的 mtime 均为 `11:31:39`（与重置同刻） |
| 重置前 | `git status` = **221**（145 构建产物 + 56 修改 + 19 新增 + 1 index.html） |
| 重置后（本次快照） | `git status` = **102**（**6 修改 + 96 未跟踪**，无暂存） |
| 有无 stash | `git stash list` **为空**，重置未留恢复口 |

**后果**：

1. **19 个新增文件（`??`）与 4 个前端新面板** 在重置中**存活**——模块契约层仍在。
2. **56 个「已修改」文件被回退到 HEAD**，即**接线层被整体抹掉**：
   - `main.rs` 里的 `mod` 声明（`runtime_capacity` / `error_policy` / `model_key_source` /
     `accounts_list` / `plugin_ledger` / `credential_version` / `desktop_launch`）、路由
     （`/api/routing/errors`、`/api/plugins/capacity`、`/api/prompt/rules`、`/api/web-tool-emulation`、
     `/api/model-key-source`）**全部消失**；`main.rs` 现在只剩既有的 `mod prompt;`
   - `api/admin/settings.rs` 里 20 余个 beta.2 handler（`handle_get_error_policy` 等）**全部消失**
   - 数据面消费点消失：`api/anthropic.rs:73`、`api/openai/resolve.rs:123` 的 `model_key_source::check_current`，
     `bridge/account.rs` 的 `error_policy::self_retry_allowed/should_switch`，`api/admin/plugins.rs` 的
     beta.2 字段投影，`prompt.rs` 的规则引擎改动（`git diff src/prompt.rs` 现为空）
3. **修复方式已重启**：重置后 `src/plugin_host/{mod,registry}.rs`、`src/plugin_host/wasm/wire.rs`、
   `src/model/metadata.rs`、`src/update/mod.rs`、`Cargo.toml` 在 **11:32+ 重新出现修改**，
   说明团队正在**重做接线**。

**对本矩阵的读法**：

- §三 的 ✅ 判定是**针对 221 基线**作出的，**模块实现层仍成立**（新增文件还在）。
- 但 §三 中所有**「接线/消费」类证据**（路由、handler、数据面调用）**需按 §十 重新核验**，
  因为承载它们的 `main.rs` / `settings.rs` / `bridge/account.rs` 等已回退。
- **T2/T3/T4 不要重写已存在的新模块**，只需**重新接线**；接线点清单见 §十。

---

## 三、✅ 已覆盖（不要重复做）

以下每条均给出在途改动中的落点与数据面消费证据，T2/T3/T4 可直接跳过。

### 插件宿主（T2）

| 编号 | beta.2 条目 | 在途落点 | 接线/消费证据 |
|---|---|---|---|
| **P1** | #2 提供商插件全面转向独立 WASM 包 | `src/plugin_host/wasm/*`（**16 412 行**，含 `host/runtime/services/state/tasks/probe/manifest/login/verification/account_origin/managed_task/wire/...`） | `wasm::contract_public()` / `declared_extensions()` 由 `api/admin/plugins.rs` 包视图输出（`"wasm"` / `"wasmDeclarations"`） |
| **P2** | #8 插件批量安装/更新/启停/卸载 + 逐项进度 | `src/plugin_host/operations.rs`（774 行：`start_install` / `start_lifecycle` / `batch` / `snapshot` / `mutation_guard`） | 路由 `/api/plugins/install`、`/enable`、`/uninstall`、`/update`、`/progress`（`main.rs:3126-3155`）；前端 `pages/Plugins.tsx`（+248 行） |
| **P3** | 插件包视图 beta.2 字段 | `api/admin/plugins.rs` | `wasm` / `wasmDeclarations` / `desktopLaunch`（如实空表）/ `providerDocs` / `adminPanels` / `accountFeatures` / `operations` 均已投影 |
| **P4** | #10 插件登录 / 人机验证 / 本机凭据 / 服务器访问权限**能力契约** | `wasm/login.rs`(379)、`wasm/verification.rs`(333)、`wasm/account_origin.rs`(403)、`wasm/managed_task.rs`(315) | 契约经 `contract_public()` 逐项带 `served` 标识下发；前端据 `served/declared` 标黄 |
| **P5** | #2/#8 插件更新检查版本比较 | `src/update/mod.rs` | 新增 `pub fn compare_versions(a,b) -> Option<i32>`，供插件目录版本比较 |

### 账号池（T3）

| 编号 | beta.2 条目 | 在途落点 | 接线/消费证据 |
|---|---|---|---|
| **A1** | #1 账号列表按需分页 | `src/accounts_list.rs`(342)、`src/api/admin/accounts_query.rs`(224) | 路由消费：`handle_accounts_list_query` / `handle_accounts_summary` / `handle_accounts_ids`；`AccountListQuery` 支持关键词/provider 多选/状态过滤/折叠/时区 |
| **A3** | 凭证替换 | `src/api/admin/accounts_query.rs` | `handle_replace_account_credentials`（对齐上游 `accounts_credentials.rs`） |

### 模型管理（T4）

| 编号 | beta.2 条目 | 在途落点 | 接线/消费证据 |
|---|---|---|---|
| **M1** | 模型 Key 权威代 + 撤销闸 | `src/model_key_source.rs`(487) | **数据面已消费**：`api/anthropic.rs:73` 与 `api/openai/resolve.rs:123` 调用 `check_current(&key)`；`main.rs:344` 启动时 `publish_keys`；`api/admin/settings.rs:1941-1975` 提供 `GET/POST /api/model-key-source[/refresh]` |
| **M2** | #4 系统提示词规则（按 provider/客户端/模型匹配，替换/追加/透传 + 预览） | `src/prompt.rs`(1102)、`api/admin/settings.rs:1356-1487`、`frontend/src/components/PromptRulesPanel.tsx` | 路由 `GET/PUT /api/prompt/rules`、`POST /api/prompt/rules/preview`（`main.rs:2319-2325`）；三层覆盖（模型 > 提供商 > 全局） |
| **M4** | #5 统一错误处理设置（重发/换号/冷却/失败保护） | `src/error_policy.rs`(878)、`frontend/src/components/RoutingErrorsPanel.tsx` | **数据面已消费**：`src/bridge/account.rs:284` `error_policy::self_retry_allowed()`、`:407` `error_policy::should_switch()`；路由 `GET/PUT /api/routing/errors` |
| **M5** | #6 运行容量管理（自动/手动、并发+内存上限、排队） | `src/runtime_capacity.rs`(777)、`frontend/src/components/RuntimeCapacityPanel.tsx` | **数据面已接线**：`main.rs:3448` 在 `track_inflight` 中间件对 `/v1/*` 取 `Class::Request` 票据，票据经 `InflightBody` **绑定到响应体生命周期**（Drop 释放，优于上游仅中间件计数）；配置热更新 `runtime_capacity::changed()`；路由 `GET/PATCH /api/plugins/capacity` |
| **M6** | 模型匹配/启停/映射**大小写不敏感** | `src/model/manage.rs`、`src/model/mod.rs`、`src/channel/mod.rs`、`src/user_group/mod.rs`、`src/api/auth.rs` | 各入口统一 `eq_ignore_ascii_case`，与 A2A `RuleEntry::matches` 同口径；含 `disabled_rule_match_is_case_insensitive` 测试 |
| **M7** | 搜索服务多密钥排序 + 额度/限流/恢复状态 | `src/tool_services/provider_keys.rs`(455)、`provider_errors.rs`(385)、`provider_request.rs`(174) | 上游 `encryptedKey → keys[]` 迁移、按 `all/search/fetch` 作用域记录额度/恢复时间、失败换下一个密钥重试，均已实现 |

### 共享 / 其他

| 编号 | beta.2 条目 | 在途落点 | 证据 |
|---|---|---|---|
| **S1** | 报表/账号页**数据没变不再重渲染**类优化 | `frontend/src/pages/Wallet.tsx`(+11)、`ToolProviderPanel.tsx`(+335) | 前端按需刷新 |
| **S2** | 请求响应模型回显对外名 | `api/openai/chat.rs`、`api/openai/responses.rs` | `rewrite_response_model` 同口径，不泄漏上游清单大小写 |

---

## 四、🟡 部分覆盖（只补接线，勿重写模块）

| 编号 | 归属 | 现状（在途已做到哪一步） | 缺什么 |
|---|---|---|---|
| **P5b** | 插件宿主 / #10 | `wasm/login/verification/account_origin/managed_task` 的**能力契约**已完备并下发 | 缺聚合端点 **`GET/PATCH /api/plugins/permissions`**（上游 `api/plugin_permissions.rs` 合并 `account_origin::snapshot` + `managed_task::permissions::snapshot`）。前端统一展示与「敏感能力独立确认、插件变化后重新核对」依赖它 |
| **P6** | 插件宿主 / #9 | `src/desktop_launch.rs`(149) 已移植 `LaunchOrigin` 语义（快照 `check/update/accept` + CAS） | **实质未接线**：`main.rs:67` 仅 `mod desktop_launch;`，全仓**无第二处引用**；宿主侧 `core/plugin_host/wasm/desktop_launch/**`（28 文件）未实现，7 条 `/api/plugins/desktop-launch/*` 端点全部缺失 |
| **P8** | 插件宿主 | `src/account_login/chatglm.rs` 已有该家登录 | 缺 `/api/plugins/desktop-import/chatglm/{prepare,confirm,cancel,status}` 四端点（上游 `plugin_desktop_import.rs`） |
| **M3** | 模型管理 / #7 | 开关 + 提供商清单 + 对外宣称已就位：`/api/web-tool-emulation`、`WebToolsPanel.tsx`、`tool_runtime.rs:184` 下发 `emulation` | **数据面模拟管线未实现**：上游 `core/upstream/web_tools/{mod,parser,request,stream}.rs` **1 047 行**——`tagged_json_v1` 的模型声明校验、请求改写、从流里解析 `<agent2api_tool_calls>`、工具历史与结果注入。当前关闭开关后上游协议契约不成立 |
| **A2** | 账号池 / #1 | 账号列表查询/分页/摘要已通（`A1`） | 缺列表内的**余额聚合**：上游 `core/accounts_list/{balance.rs,balance_cache.rs}`。`accounts_list.rs` 目前不产出余额（AIGX 有独立的 `account_session/balance.rs`，需接到列表行上） |
| **A4** | 账号池 / #3 | `src/credential_version.rs`(165) 提供**存储无关**的契约层 | 上游是 `core/account_store/credential_version/**` **40+ 文件**的完整状态机（cline_free_enrollment / codearts_slot / doubao_state 及 SQL 投影层）。AIGX 未移植具体各家续期编排，需确认是「契约已备待接」还是「需补具体实现」 |
| **A5** | 账号池 / #2 | 已有 per-account 余额巡检 `account_session/usage_query.rs`(250) + 异步任务 `usage_jobs.rs`(381)，语义为 beta.1 口径（失败不清快照、写回前重读、并发 3） | 缺上游 beta.2 的**统一排队 + 失败分类 + 恢复入口**：`core/usage_query/{queue,network,diagnostics,query,recovery}.rs`——按提供商排队、区分等待超时/查询超时/缺授权/凭据失效、`requires_action` 恢复门控 |
| **A7** | 账号池 / #7 | `api/admin/checkin_center.rs` 已有签到中心聚合快照 | 缺 `pluginTasks` 段（上游 `wasm::tasks::checkin_snapshot()`）与 `?view=summary` 轻量视图。**AIGX `wasm` 模块无 `checkin_snapshot()`**，需补函数再接端点 |
| **S3** | 共享 / #1 | `update/mod.rs` 已给插件目录比较用版本函数 | 「软件与插件改由官网检查更新和下载、安装包全量校验通过后才开放更新」的整体链路未核验，属增量待确认 |
| **S4** | 共享 / #5 | 未在在途改动中定位到明确的流式结束状态处理 | beta.2「流式响应、工具续接和取消保留真实结束状态」需在 `api/openai/chat.rs` / `api/anthropic.rs` 逐点核对，本审计未取证，**建议 T5 实测确认** |

---

## 五、❌ 未覆盖（需要实现）

### 插件宿主（T2）

| 编号 | beta.2 条目 | 上游规模 | 缺口说明 |
|---|---|---|---|
| **P7** | 插件包**来源校验 / 归档 / 生命周期 shutdown** | `packages/provenance/**`（**1 535 行**）+ `lifecycle/shutdown.rs`（229）+ `support.rs` | AIGX `plugin_host/` 无 `packages/provenance`、无 `lifecycle/` 目录。上游对安装包做 zip 布局校验、兼容 zip、来源注册表、启动期校验与插件进程**优雅关停**。AIGX 现有 `process.rs` 未覆盖该语义 → 需实现 |
| **P9** | 插件 **enrollment** 流程 | `api/plugin_codearts_enrollment.rs`、`api/plugin_cline_free_enrollment.rs`（各 4 端点）+ `wasm/codearts_enrollment/**`、`wasm/cline_free_enrollment/**` | AIGX 全仓 **grep `enrollment` = 0 命中**。上游为 CodeArts / Cline 免费接入提供 `prepare → confirm → cancel → status` 四段式。AIGX 有 `account_login/{codearts,cline}.rs`，但**没有 enrollment 状态机与端点** |
| **P10** | WorkBuddy 插件流 | `core/plugin_host/stream/workbuddy.rs`、`stream_events.rs`、`errors/workbuddy_stream.rs` | AIGX 无 `plugin_host/stream/` 目录，未覆盖 |
| **P11** | 插件安装来源 / 遗留凭据 | `wasm/install_source.rs`、`wasm/legacy_credentials.rs`、`wasm/legacy_secret.rs`、`wasm/client_device.rs`、`wasm/quwork_device.rs` | AIGX `wasm/` 无对应模块，需评估是否属 beta.2 必须 |

### 账号池（T3）

| 编号 | beta.2 条目 | 上游规模 | 缺口说明 |
|---|---|---|---|
| **A8** | **Claude / ChatGPT / Codex 订阅接入**（beta.2 #3） | `core/providers/{claude_subscription,chatgpt_subscription,codex_subscription}/**`（**合计约 70+ 文件**），并注册为 `ProviderKind::{ClaudeSubscription,ChatgptSubscription,CodexSubscription}`（`providers/mod.rs:260-262`、`adapter.rs:1720-1722`、`catalog.rs:74-76`） | **本审计发现的最大缺口**。AIGX 全仓 grep `claude_subscription/chatgpt_subscription/codex_subscription` = **0 命中**；`/api/subscriptions*` 在上游是 `retired_proxy_feature`，订阅能力挂在 provider 层。AIGX 仅在 `account_session/import.rs` 里解析桌面客户端配置（`SUPPORTED_TYPES = ["claude","codex","claude-desktop"]`），**无订阅 provider 的适配器、凭据模型、鉴权与模型清单**。注意上游自述「相关链路尚未完成真实账号验收」，对齐时可先按契约落地 |
| **A9** | 桌面端登录 / SMS 端点路径 | `/api/session/login/sms/{send,verify}`、`/api/session/login/loomy/sms/*`、`/api/session/login/tabbit/sms/*`、`/api/session/login/oauth/{captcha-config,start}` | AIGX 用的是**另一套路径**：`/api/account-sessions/login/sms/send|verify`（`main.rs:2898-2902`）。功能上 SMS 收发已有，但 loomy / tabbit 两家与 oauth captcha-config/start 端点**未见**，需逐项核对 provider 覆盖 |

### 共享 / 跨块

| 编号 | beta.2 条目 | 缺口说明 |
|---|---|---|
| **S5** | 面板**实时事件推送** | 上游 `api/panel_events.rs` + `core/panel_events.rs`（232 行）：`/api/panel/events` SSE，带 `Revisions` 版本号、**150 ms 合并窗口**、15 s 心跳、面板/模型 Key 双鉴权。AIGX **grep `panel/events` = 0**，账号页与报表页目前依赖轮询。这是 beta.2 #1「账号列表实时状态更新」的**数据面支撑**，建议 T5 一并收口 |
| **S6** | 广告/赞助位 | 上游 `core/advertisements/**` + `/api/advertisements`（`no-store`） | AIGX 无对应模块。**建议判 ⛔ 或 ❌**：AIGX 无官网/赞助位场景，若 T5 决定不做需在此留结论 |

---

## 六、⛔ 不适用（AIGX 架构上无对应面）

| 条目 | 上游 | AIGX 理由 |
|---|---|---|
| Linux 桌面端硬件加速开关与兼容模式重启 | beta.2「其他更新 #6」 | AIGX 是 Web/服务端形态，无桌面壳，无对应面 |
| 手机端六语文案同步 | beta.2「其他更新 #8」 | AIGX 前端为单页应用，i18n 已在 `frontend/src/i18n/{en,zh}.json` 覆盖，无移动壳 |
| 容器内升级/回滚、官网说明文档 | beta.2「其他更新 #9」 | 文档/发行件，不属源码对齐 |

---

## 七、风险与依赖（交给 team-lead / T5）

1. **`#![allow(dead_code)]` 信号**：多数在途新增模块头部都带「本模块的公开 API 覆盖 beta.2 的完整契约面，
   部分符号为后续接线/线表预留」。这表示**契约层已完成、接线层尚未全部完成**——T2/T3/T4 若看到模块
   已存在，**不要重写**，应只补消费点与端点。
2. **`desktop_launch.rs` 是“契约已备、接线为零”**：在 221 基线中 `main.rs` 仅 `mod desktop_launch;`
   而无第二处引用；重置后连该声明也一并消失。T2 接手时**不要重写**，需补声明 + 宿主侧
   `desktop_launch/**` 与 7 条端点。
3. **构建产物不要手工处理**：`static/assets/**` 是 Vite 重哈希结果（基线 145 项、重置后约 73 项未跟踪），
   提交前由构建流程生成即可；手工触碰会造成无意义 diff。
4. **工作区无提交保护**：本次审计中一次 `git reset` 就抹掉了 56 个已修改文件且无 stash 留底。
   **建议 team-lead 让各成员在完成一个可编译、可自测的接线单元后立即提交**，或至少保留 stash，
   避免跨成员并行时相互覆盖。
4. **依赖顺序**：`S5 面板 SSE` 是 `A1`（账号列表实时状态）与 `A7`（签到中心）的**数据面前置**；
   建议在 T5（共享接线）优先做，否则 T3 的前端实时性无法真正达成。
5. **A8 订阅接入的验收口径**：上游自述「相关链路尚未完成真实账号验收」，AIGX 落地时**不要以
   「上游已验收」为前提**，需自行建验收用例。

---

## 八、证据命令附录（可复核）

> ⚠️ 以下命令中，标注 **基线** 的结果对应 221 审计时刻；标注 **现状** 的对应 `git reset` 之后。

```bash
# 上游基线与增量规模
git -C D:/GitHub/Agent2API show v3.5.0-beta.2:package.json
git -C D:/GitHub/Agent2API diff --stat v3.5.0-beta.1 v3.5.0-beta.2   # 1086 files

# AIGX 在途改动构成（基线 = 221，审计时刻）
cd D:/GitHub/AIGX
git status --porcelain | wc -l                                          # 221（基线）
git status --porcelain | grep -c 'static/assets'                       # 145（基线）
git status --porcelain | grep -vE 'static/' | wc -l                    # 75（基线）

# 重置发生时刻与后果（现状）
git reflog -3 --date=iso                                               # HEAD@{0}: reset: moving to HEAD
git status --porcelain | wc -l                                          # 102（现状：6 修改 + 96 未跟踪）
git stash list                                                          # 空，无恢复口
git diff --stat src/api/admin/settings.rs src/main.rs src/prompt.rs      # 现状应为空（接线已回退）

# 上游新增路由（beta.2 真实端点增量）
git -C D:/GitHub/Agent2API diff v3.5.0-beta.1 v3.5.0-beta.2 \
  -- desktop-tauri/src-tauri/server/src/server/http.rs | grep -E '^\+' | grep -oE '"/api/[^"]*"' | sort -u

# 数据面接线核对
cd D:/GitHub/AIGX
grep -n "model_key_source::check_current" src/api/anthropic.rs src/api/openai/resolve.rs
grep -n "error_policy::self_retry_allowed\|error_policy::should_switch" src/bridge/account.rs
grep -n "runtime_capacity::try_acquire" src/main.rs
grep -n "desktop_launch" src/main.rs                                      # 仅 mod 声明，无调用
grep -rn "enrollment" src --include=*.rs                                 # 0 命中
grep -rin --include=*.rs "claude_subscription\|chatgpt_subscription\|codex_subscription" src   # 0 命中
grep -rn "panel/events\|advertisements" src --include=*.rs               # 0 命中
```

---

## 九、给 T2 / T3 / T4 的一句话交接

- **T2（插件宿主）**：`P1–P5` ✅ 别重做；要做的是 `P7` 包 provenance/关停、`P9` enrollment、
  `P6` desktop-launch 接线、`P5b` permissions 聚合端点，以及评估 `P10/P11`。
- **T3（账号池）**：`A1/A3` ✅ 别重做；要做的是 `A8` 订阅接入（最大项）、`A5` 余额查询统一排队，
  并补 `A2` 列表余额聚合、`A4` 凭据续期具体实现、`A7` 签到中心 pluginTasks。
- **T4（模型管理）**：`M1/M2/M4/M5/M6/M7` ✅ 别重做；**唯一实质缺口是 `M3` 的网页工具数据面模拟管线**
  （1 047 行），其余仅需在 T5 做端到端验证。
- **T5（共享接线）**：`S5` 面板 SSE 与 `S3/S4/S6` 的核对/结论，是三块前端实时性与更新链路能否达成的关键。

---

## 十、⚠️ 重置后需重新接线的点位清单（2026-11-19 11:31 起生效）

模块实现**仍在**（见 §二.3），但承载接线的 56 个文件已回退。以下点位需按序补回，**不要重写模块本体**。

### 10.1 模块声明与路由（`src/main.rs`）

| 需补回 | 说明 |
|---|---|
| `mod runtime_capacity;` `mod error_policy;` `mod model_key_source;` `mod accounts_list;` `mod plugin_ledger;` `mod credential_version;` `mod desktop_launch;` | 7 个新模块声明，现仅剩既有 `mod prompt;` |
| `/api/routing/errors`（GET/PUT） | 对齐 beta.2 #5，handler 在 `api/admin/settings.rs` |
| `/api/plugins/capacity`（GET/PATCH） | 对齐 beta.2 #6，handler 在 `api/admin/settings.rs` |
| `/api/web-tool-emulation`（GET/PUT） | 对齐 beta.2 #7 |
| `/api/prompt/rules` + `/api/prompt/rules/preview`（GET/PUT/POST） | 对齐 beta.2 #4 |
| `/api/plugin-task-grants`（GET/PUT） | 插件受控任务授权 |
| `/api/model-key-source` + `/api/model-key-source/refresh`（GET/POST） | Key 权威代 |
| `src/api/admin/mod.rs` 的 `pub use settings::{...}` 批量再导出 | 20 余个 handler 现未导出 |
| 启动期 `install_snapshot`/`install_settings`/`set_web_tool_emulation`/`publish_keys` 调用 | 数据面开关的初始化 |
| `track_inflight` 中间件的 `runtime_capacity::try_acquire` 分支 | 容量准入，含 `InflightBody` 票据绑定 |

### 10.2 数据面消费点

| 位置 | 需补回 |
|---|---|
| `src/api/anthropic.rs` / `src/api/openai/resolve.rs` | `model_key_source::check_current(&key)` 撤销闸 |
| `src/bridge/account.rs` | `error_policy::self_retry_allowed` / `should_switch`（自重试与换号判定） |
| `src/api/admin/plugins.rs` | `wasm` / `wasmDeclarations` / `desktopLaunch` / `providerDocs` 字段投影 + `plugin_operations` |
| `src/prompt.rs` | beta.2 提示词规则引擎改动（`git diff` 现为空，需重新移植） |
| `src/config.rs` | `web_tools` / `runtime_capacity` / `error_policy` / `plugin_task_grants` 配置段 |
| `src/api/openai/chat.rs` / `responses.rs` | 响应模型名回显对外名（`rewrite_response_model` 口径） |
| `src/model/manage.rs` / `model/mod.rs` / `channel/mod.rs` / `user_group/mod.rs` / `api/auth.rs` | 大小写不敏感匹配 |
| `src/tool_services/*` | 多密钥搜索（provider_keys/provider_errors/provider_request 的消费） |
| `frontend/src/api/endpoints/{misc,plugins}.ts`、`pages/{Plugins,Settings,Wallet}.tsx`、`ToolProviderPanel.tsx`、`types/*`、`i18n/*` | 前端接线（4 个新面板文件仍在，但 API 客户端与页面挂载已回退） |

### 10.3 当前仍在自动推进的修改（重置后新增，无需重复做）

重置后已重新出现、处于在途状态的 6 个修改文件，**视为正在推进中，勿重复动手**：

`Cargo.toml`、`src/model/metadata.rs`、`src/plugin_host/mod.rs`、`src/plugin_host/registry.rs`、
`src/plugin_host/wasm/wire.rs`、`src/update/mod.rs`

> 建议 T2/T3/T4 在开工前各自 `git status --porcelain` 复测一次，确认本清单点位当前是否已被同僚补回，
> **避免在同一文件上与在途改动打架**（本次重置即暴露了工作区无提交保护的风险）。
