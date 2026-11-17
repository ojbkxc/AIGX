# AIGX × Agent2API v3.5.0-beta.1 对齐移植说明

> **上游**：[aimod-cc/Agent2API](https://github.com/aimod-cc/Agent2API) `v3.5.0-beta.1`
> （只读检出 `D:\tmp\a2a-beta`；server 根 = `desktop-tauri/src-tauri/server/src/server/`）
>
> **版权与许可**：Agent2API 以 **AGPL-3.0** 发布，Copyright (c) 2026 aimod-cc。
> 本仓库中带 `// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)`
> 文件头的每个文件都是其衍生作品，遵循 AGPL-3.0。分发/部署本仓库时须一并满足 AGPL-3.0
> 的源码可得性要求。上游代码版权方与本项目相同（aimod-cc），移植已获授权。
>
> 对齐波次：2026-11-16 ~ 2026-11-17。上游没有可直接 `git merge` 的 Rust 结构对应关系
> （目录布局与抽象层不同），跟随上游更新只能按本文件 §二 的清单**逐文件手工同步**。

---

## 一、移植原则

1. **按可观察行为移植**：wire 形状（字段名/大小写）、状态语义、错误码、持久化口径与
   上游一致；内部实现允许按 AIGX 的既有抽象改写（例如 `script.ws.*` 用
   `tokio-tungstenite` 直连而不是 Bun 侧代理）。
2. **保留兼容面再改内部实现**：与 AIGX 既有实现冲突时，先保留同名方法/字段的签名与
   语义，再换内部数据源。样板：`ModelMapper` 换内部条目表时保留全部 13 个同名兼容
   方法，8 个调用方文件零改动。
3. **不引入新 DB schema**：所有新增持久化一律走既有 `FileStore` KV（SQLite kv 表），
   部署无需迁移。
4. **插件优先归属**：provider 归属由「内置目录 → 原生实现 → 插件声明」三层裁决；
   双实现 provider（workbuddy / atomcode）数据面走插件、登录桥同认插件归属，
   `is_native_provider` 已退化为纯测试辅助（见 §四.3）。

## 二、移植清单（AIGX 文件 → A2A 源）

标注 `[新]` 为本波次新增文件，`[改]` 为改动既有文件。所有 `[新]` 的 Rust/mjs 文件均带
AGPL 来源头；`[改]` 文件在改动块内以注释注明对应上游模块。

### 2.1 脚本插件宿主 ABI（原 T1）

| AIGX | A2A 源 | 说明 |
|---|---|---|
| `src/plugin_host/script/host/*.mjs`（29 个）[新/改] | `core/plugin_host/script/host/*.mjs`（同名 29 个） | 宿主 mjs 全量对齐（27 新增 + 覆盖原有 5 个） |
| `src/plugin_host/script/http.rs` [新] | `core/plugin_host/callbacks.rs`（`script.http.*` 分发）+ `host/fetch.mjs` | HTTP 会话桥（open/read/close） |
| `src/plugin_host/script/websocket.rs` [新] | `core/plugin_host/callbacks.rs`（`script.ws.*`）+ `host/websocket.mjs` | WebSocket 桥（open/read/send/abort），tokio-tungstenite 直连 |
| `src/plugin_host/script/usage.rs` [新] | `core/plugin_host/script/usage.rs` | `script.usage` → `RequestLog` 入 `LogStore` |
| `src/plugin_host/script/icons/{mod,formats,cache,download}.rs` [新] | `core/plugin_host/script/icons/{mod,formats,cache,download}.rs` | 图标缓存（内容摘要文件名、容量上限、SSRF 防护） |
| `src/plugin_host/provider_settings/{mod,schema}.rs` [新] | `core/plugin_host/provider_settings/{mod,schema}.rs` | 提供商设置 Schema + CAS 存储 |
| `src/plugin_host/script/{callbacks,rpc,protocol,transport,operations,runtime,types,host_rs}.rs` [改] | `core/plugin_host/{callbacks,rpc,protocol,transport,operations,runtime}.rs` + `script/` | ABI 线面重写、init 帧平铺、proxy/signature 回调、包内 disabled 不解除 reserved |
| `src/api/admin/plugin_provider_settings.rs` [新] | `api/plugin_provider_settings.rs` | `GET/PUT /api/plugins/provider-settings` |

### 2.2 插件管理面 API（原 T2）

| AIGX | A2A 源 |
|---|---|
| `src/api/admin/plugin_icons.rs` [新] | `api/plugin_icons.rs` |
| `src/api/admin/plugin_theme.rs` [新] | `api/plugin_theme.rs` |
| `src/api/admin/plugin_ui.rs` [新] | `api/plugin_ui.rs` |
| `src/api/admin/plugin_login.rs` [新] | `api/plugin_login.rs` |
| `src/api/admin/plugin_tasks.rs` [新] | `api/plugin_tasks.rs` |
| `src/api/admin/plugin_helpers.rs` [新] | `api/plugin_helpers.rs` |
| `src/api/admin/plugins.rs` [改] | `api/plugin*.rs` + `core/plugin_host/pack_view.rs` | 包视图字段扩展、原生/脚本/内置三层开关 |
| `src/api/admin/script_plugins.rs` [改] | `api/plugin_*.rs` 脚本侧 + `core/plugin_host/script/{settings,updates,operations}.rs` | catalog / rebuild / check 等脚本管理面 |

### 2.3 Wasm 插件沙箱（原 T3）

| AIGX | A2A 源 | 说明 |
|---|---|---|
| `src/plugin_host/wasm/`（21 个文件）[新] | `core/plugin_host/wasm/`（16 个文件） | `catalog/context/errors/host/http/limits/login/manifest/native/public_view/resources/runtime/services/state/tasks` 一一对应 |
| `src/plugin_host/wasm/egress.rs` [新] | `core/egress.rs` | 出站网络策略 |
| `src/plugin_host/wasm/{access,account_access,methods,wire}.rs` [新] | —（AIGX 专用胶水） | 账号访问 trait/实现、方法表、wire 类型 |
| `Cargo.toml`/`Cargo.lock` [改] | — | `wasmtime = 36.0.19`（lock 实际 36.0.20，与上游 lock 一致；`default-features=false, features=[async,cranelift,runtime,std]`） |

### 2.4 账号池与插件账号（原 T4）

| AIGX | A2A 源 |
|---|---|
| `src/account_session/auth_file.rs` [新] | `api/accounts_export.rs`（`import_account_file` 行为）+ `api/account_import_setup.rs` |
| `src/account_session/plugin.rs` [新] | `core/account_store/{plugin_accounts,plugin_contract,plugin_identity,plugin_public,plugin_state,hidden_accounts,usage_settings}.rs` |
| `src/account_session/sorting.rs` [新] | `core/account_order.rs` + `api/accounts_sort.rs` |
| `src/account_session/usage_jobs.rs` [新] | `core/usage_query/{jobs,run,config}.rs` |
| `src/account_session/{model,store,mod}.rs` [改] | `core/account_store/{store,store_crud,store_batch,store_view,snapshot}.rs` |
| `src/api/admin/account_sessions.rs` [改] | `api/{accounts,accounts_batch,accounts_export,accounts_sort,accounts_usage}.rs` | 11 个插件账号请求字段 + 8 个补注册 handler + 影子路径 `/api/accounts/*` |
| `src/api/admin/account_login.rs`、`desktop_import.rs`、`channels.rs`、`zcode_captcha.rs` [改] | `api/{accounts,account_bootstrap,import_sources}.rs`、`api/zcode_captcha.rs` |

### 2.5 模型管理（原 T5）

| AIGX | A2A 源 |
|---|---|
| `src/model/capability_overrides.rs` [新] | `core/capability.rs` + `core/model_rules`（`CapabilityOverride` 部分） |
| `src/api/admin/model_manage.rs` [改] | `api/model_manage.rs` |
| `src/api/admin/model_metadata.rs` [新] | `api/model_meta.rs` + `api/models.rs::refresh_models` |
| `src/api/openai/models.rs` [改] | `api/chat.rs::list_models` + `anthropic_models_view` + `core/models/shape.rs` |
| `src/model/{manage,mod,models_dev}.rs` [改] | `core/model_rules`、`core/model_meta`、`core/models` |

### 2.6 签到中心（原 T6）

| AIGX | A2A 源 |
|---|---|
| `src/checkin/center.rs` [新] | `core/auto_checkin`（`state/run_now/run_schedule_now`）+ `api/checkin_center.rs` |
| `src/checkin/schedules.rs` [新] | `core/checkin_schedules/` + `api/checkin_schedules.rs` |
| `src/api/admin/checkin_records.rs` [新] | `api/checkin_records.rs` + `core/checkin_records.rs` |
| `src/api/admin/checkin_schedules.rs` [新] | `api/checkin_schedules.rs` + `core/checkin_schedules.rs` |
| `src/api/admin/checkin_center.rs` [改] | `api/checkin_center.rs` + `api/campaign_rewards.rs` |
| `src/checkin/{store,scheduler,claim,mod,runtime_tests}.rs` [改] | `core/{checkin,auto_checkin,checkin_history}.rs` |

### 2.7 工具服务与维护明细（原 T7）

| AIGX | A2A 源 |
|---|---|
| `src/tool_services/`（11 个文件）[新] | `core/tool_services/{mod,account_search,bridge,image_config,install,mcp,normalize,providers,search,skill_package,vision}.rs` |
| `src/api/admin/{tools,tool_image_admin}.rs` [新] | `api/{tools,tool_image_admin}.rs` |
| `src/api/tool_runtime.rs` [新] | `api/tool_runtime.rs` + `api/{tool_images,tool_image_results}.rs` 运行时侧 |
| `src/maintenance/{mod,storage,types}.rs` [新] | `core/maintenance/{mod,storage,types}.rs` |
| `src/api/admin/maintenance.rs` [新] | `api/maintenance.rs` + `request_stats/maintenance.rs` |
| `src/scheduled_tasks/{mod,task_state}.rs` [改] | `api/scheduled_tasks.rs`（维护明细扩展） |

### 2.8 前端（原 T8）

| AIGX | A2A 源 |
|---|---|
| `frontend/src/pages/{Plugins,ToolsPage,AccountSessions,ModelManage,CheckinCenter,ScheduledTasks}.tsx` [改/新] | `ui-islands/src/islands/{plugins-page,tools-page,accounts-page,models-page,checkin-page}.tsx` |
| `frontend/src/components/{AccountSortDialog,CheckinCampaignCard,CheckinVerificationNotice,MaintenancePanel,ModelCapabilityDialog}.tsx` [新] | 上游同名功能（见各文件头注释） |
| `frontend/src/components/plugins/{ProviderSettingsDialog,ScriptPluginsPanel}.tsx` [新/改] | `settings-page.tsx` 插件设置 dialog |
| `frontend/src/api/index.ts`、`types/{index,a2a}.ts`、`App.tsx`、`Sidebar.tsx`、i18n 六语 [改] | 上游对应类型/端点 |

### 2.9 共享接线与门禁（integrator）

| AIGX | 说明 |
|---|---|
| `src/main.rs` [改] | 全部子 router merge、`plugin_host::provider_settings::install`、`model::capability_overrides::install`、`script::usage::install`、`script_host.set_host_env`、account_sessions 8 个漏注册 handler、5 条脚本路由 |
| `src/lib.rs` [改] | 与 main.rs 双声明新模块（`tool_services`、`maintenance`） |
| `src/state.rs`（经 lib.rs 声明） | `AppState` 增 `tool_services` / `maintenance` 字段 |
| `src/api/admin/openapi.rs` [改] | 本波新增路由的文档清单（`real_admin_routes()` + `paths_*`）与 EXEMPT 条目 |
| `tools/extract_routes.py` [改] | `ROUTE_FILES` 白名单补齐 6 个 router 文件 + 新增 `check_router_coverage()` 常驻闸门（白名单差集非空即 FAIL） |
| `docs/api-documentation.md` [改] | 新增「工具服务与维护任务」「模型管理」「签到中心」「账号池」「插件管理面子页」「签到活动福利」等章节 |

## 三、行为验证（真实服务探针）

本波次的「完成」不以编译绿为准，关键接线都做了真实请求探针（临时数据目录 + 首次启动改密 +
ALTCHA 解题，结束后清理进程与目录）：

| 验证点 | 修复前 | 修复后 |
|---|---|---|
| `GET /api/plugins/provider-settings` | 503「设置存储尚未就绪」（`install` 零调用 → `STORE.get()` 恒 None） | **读侧 503→200 `{"settings":[]}` 已实证**；写侧因当前环境无「声明 settings schema 的插件包」，`PATCH` 走不到 configure（404「插件没有声明可编辑的提供商设置」），端到端未验证，由 `cargo test --lib plugin_host::provider_settings` **11 passed** 覆盖 configure/snapshot/未知字段拒绝语义 |
| `POST /api/models/capabilities` | 500「服务启动接线缺失」 | **200**，重启后 sqlite kv 表 `model_capability_overrides` 有值（落真 FileStore） |
| `capabilityStoreReady` | false | **true**（证明实时读，不是常量） |
| 8 个 account_sessions handler | 404（未注册进 main.rs） | 已注册（含 `/api/accounts/*` 影子路径） |
| 5 条脚本路由（change/settings/prompts/check/catalog-refresh） | 未注册 | 已注册并进 OpenAPI |

**unverified**：`script.ws.*` / `script.signature` 未做真实插件包行为探针（见 §六）。
**wasm 的状态是「单测层已真跑 wasmtime，真实发布包端到端未验证」**：`wasm/probe.rs` 已有 8 个
行为探针用手写合法 wasm 二进制走「装配→加载→实例化→调用→关停」全链（零 mock，79 测试全过）；
仍缺的是「真实第三方 `.wasm` 发布包」的安装→启用→数据面转发全链路（仓库无此类包）。

## 四、本轮修复的「功能半死」清单（正向记录）

以下问题**全部构建门禁都过**，只有行为探针/静态对表能抓；修复方式已沉淀：

1. **`install(store)` 单例零调用**：`plugin_host::provider_settings::install`、
   `model::capability_overrides::install` 均定义了但无人调用 → 写接口 500/503、读接口静默降级。
   已在 `main.rs` 接线。通用检测式：`grep -rn "pub fn install(store: Arc<FileStore>)"` 后逐个
   查调用点。
2. **handler 写了但没注册**：`account_sessions.rs` 35 个 handler vs main.rs 27 个注册，
   8 个静默 404。已补注册。通用检测式：模块内 `pub async fn handle_*` 集合 vs main.rs 引用
   集合做差集。
3. **`is_native_provider` 残留短路**：`account_sessions.rs` 两处「插件优先」前的残留判断
   （`provider_is_plugin_provider` 首行、`push_plugin` 闭包）已随 T4 修复移除；全仓现在只剩
   函数定义 + 单元测试断言 + 说明注释，函数已退化为纯测试辅助（后续可考虑私有化或
   `#[cfg(test)]`）。
4. **字段名大小写错配**：后端发 `streakDays`、前端读 `snake_case` → 连登天数永远显示「—」
   且不报错。已统一为 snake_case（`checkin_center.rs`）。活动对象字段保留 camelCase
   （对齐上游 `checkin-campaign-types.ts`）——**两种风格并存是刻意设计**，见
   `docs/api-documentation.md` 的「注（有意的混用，勿统一）」。
5. **静态闸门失明**：`tools/extract_routes.py` 的 `ROUTE_FILES` 是白名单，6 个新 router 文件
   （T7 25 条 + T6 10 条共 35 条路由）不在扫描面 → 闸门对它们完全失明而显示绿。已补白名单，
   并新增 `check_router_coverage()`：`grep "pub fn router"` 得到的文件集合与白名单差集非空即
   FAIL，防再犯。
6. **flaky 测试**：wasm `spawn_with_is_stoppable` 断言进程级 `scheduler_token()` 未被取消，
   而另一用例 `shutdown_is_idempotent` 会 cancel 它 → 1/3 概率红。已改为各自自洽断言；
   修复后连跑 4 次全绿（wasm 命名空间现 79 测试全过）。

## 五、三分法判定（完成 / 部分对齐 / 未验证）

判据：**「完成」= 真实包能装、能在账号页选到、数据面能转发**（行为可达）；「部分对齐」=
编译绿、单测绿，但注入面是空壳或只有单侧；「未验证」= 无行为证据。

| 模块 | 判定 | 依据 |
|---|---|---|
| 脚本插件宿主 ABI（29 mjs + 回调面） | 完成 | 既有脚本插件链路（v1.0.53 起）在其上运行；24 方法 wire 对表测试钉住 |
| 插件管理面 API（7 个模块） | 完成 | 路由已注册 + OpenAPI 对表 + 前端调用点全接通 |
| 模型管理（映射/能力三态/`/v1/models`） | 完成 | 真实探针 200 + 重启后 KV 有值；`/v1/models` 双形态输出 |
| 账号池与插件账号 | 完成（插件账号登录桥部分靠单测） | CRUD/导入/排序/usage jobs 路由通；登录交互走 T2 的 plugin_login |
| 签到中心 | 完成（活动福利仅 ZCode 两地有链路） | 台账/计划/调度/补签/活动 API 全注册；`streak_days` 修复 |
| 工具服务 | 完成（附偏离） | 管理面 + 账号型搜索运行时 + 图片生成运行时 + 视觉模型目录校验均已移植（§六.2）；偏离见该条 |
| 维护任务明细 | 完成 | 管理面 + 记录分页 |
| Wasm 沙箱 | 部分对齐 | 21 文件 + 79 测试全绿（含 8 个真跑 wasmtime 的行为探针）；运行期接线已做（`manager`/`PluginBackend` 可走 wasm 包、`main.rs` 三行任务接线）；自建真 wasm 包的 install→enable→数据面转发已由 e2e 走通（§六.10）；`native.install_source` 已接线（§六.3）。**仍不可验证的是真实第三方 atomcode wasm 包**（仓库无此类包） |
| 前端六页 | 完成 | tsc/lint/i18n **4057** 键/vitest 20 files/149 tests 全绿（须 `cd frontend` 跑） |

## 六、未完成项（v1.0.64 遗留收尾波处置结果）

> 本节原为 v1.0.63 结束时的 12 条未完成项。v1.0.64 收尾波逐条处置：**已修**给修复方式，
> **如实记录**给本环境不可验证的原因。

1. **`wasm::catalog` 整块未接线 → 已修**：provider 级模型目录落地 `plugin_host/model_catalog.rs`
   （KV `plugin_catalog:{provider}`），`manager.install/enable` 注入 `WasmOptions.catalog` 并在
   enable 后 rebuild；`wasm/catalog.rs::forget` → `model_catalog::remove`；wasm `settings.get`
   拦截 `modelCatalog` → 快照；`provider_settings/schema.rs` 拒绝声明保留键；`api/openai/models.rs`
   与 `model/manage.rs` 的 Plugin 分支改读目录（声明优先、目录回落）。`wasm/mod.rs` 的 catalog
   `#[allow(dead_code)]` 已删除；`cargo clippy --lib -D warnings` 绿 = catalog 无残留死码。
   残留偏离：目录生产者**已接**（`account_sessions.rs::refresh_plugin_account_catalog` 走
   `models.refresh` → `wasm::catalog::save_refresh` 校验后 CAS，管理员「同步模型」触发）；
   未移植的是上游「签到 claim 成功后自动刷新目录」这条路径，因此**不会自动刷新，需人工触发**。
   插件 provider 缺省只显渠道声明（空目录按 `remote:true` 存，不编造）。
2. **T7 未移植三项 → 已修**：① 账号型搜索运行时（`AccountSearchHost` +
   `PluginHostSearchBridge`，三级路由与 `resolve.rs` 同序：原生进程 → 脚本宿主 → wasm；
   `main.rs` 已注入）；② 图片生成运行时（`image_jobs` / `image_runtime` / `image_mcp` /
   `image_bridge(.cjs)`，FileStore 任务层 + MCP 三工具 + 文件下载鉴权）；③ 视觉模型目录校验
   （`resolve_vision_target`：未知 / 未启用 / 图片生成模型 / `supportsImages≠true` 拒绝）。
   偏离：账号搜索依赖插件新增的 `hooks.admin.action` 通用透传（上游 v3.5.0-beta.1 的
   `provider.mjs` 只分发 `auth.*`，无 web-search 契约；插件未实现 = `configuration`，不伪造）；
   图片运行时无 `KeyScope::restrict_image_target` / `routing_snapshot`，`account_id` 只做存在性
   与 provider 匹配校验；`install.rs` 的图片本地桥安装（Node 探测 / 客户端 env）仍未移植，
   本波只提供脚本下载端点 `GET /api/tools/mcp-bridge/image-generation.cjs`。
3. **Wasm `native.install_source` 延后 → 已修**：`RegistryHelperSource`（`packs` / `grants` /
   `call_pack`）实现 `WasmHelperSource`，`main.rs` 一行注入。反向自证：删 native 分区 allow →
   `clippy --lib` 精确报 7 条、全部在 `native.rs`。残留偏离：`call_pack` 仅覆盖原生插件包
   （脚本包在 `PackRegistry` 无元数据、无每包进程，上游可调脚本宿主）。
4. **`script.ws.*` proxy 被忽略 → 已修**：http 手写 CONNECT 隧道（Basic）、socks5/socks5h
   手写握手（socks5 本地解析 / socks5h 交代理解析）、每会话独立出口、本机环回绕过代理。
   **仍不支持**：https 代理（TLS-to-proxy，需 CA roots 直接依赖）——显式报错，不静默直连。
5. **`script.signature` 未逐字段对齐 → 已修**：两档键 `c|session|call_id` / `s|session|name`
   （2048 两代、无 TTL）；`script.stream.open` 对转换前原始字节写两档缓存（跨 chunk 拼行 +
   pending 签名）；写侧 `decode_event` 调 `remember_*` 并带出 `ToolCallDelta.signature`；
   `fill_cached` 在请求构造点回填（哨兵可覆盖、客户端签名优先）。
6. **`script.packages` / `script.settings` 无 host→gateway RPC 封装 → 已修**：rebuild 后拉
   `script.packages` 回填 `settings_schema` / `icon` / `providerIcons` / `aliases` / `error`
   （合并全量 KV，防抹未启用包）；`configure_settings` 走 `script.settings` 取 schema 并落盘
   （失败反向回滚）。残留：`models.refresh` / checkin 类 RPC 仍未移植（见第 1 条）。
7. **原生插件包缺 provider 级开关 → 已修**：外置原生包的 provider 停用写入
   `PackMeta.disabled_providers`（调用返回 200），内置包行为不变；`/api/plugins/provider`
   路由未动。
8. **`getPluginProgress` / `listKeys` / `getModelMappings` → 已删**：三个零调用点前端封装
   在 v1.0.64 一并删除（`git diff` 可见三条 `-` 行；`frontend/src` 现零命中）。
9. **`account_sessions.rs` 挪 router 的白名单约束 → 守卫在位**：仍未挪进 `router()`；
   `tools/extract_routes.py::check_router_coverage()` 会在挪动后自动抓白名单缺口。
10. **真实第三方 `.wasm` 包端到端 → 已补证（自建真 wasm 包）**：`plugin-wasm` 用自建真 wasm 包
    完成「本地 zip → 管理面 install → enable（真 wasmtime 编译 + init）→ 建 plugin 渠道 →
    真实 `/v1/chat/completions`」：非流式聚合 `hello`、流式 SSE 含 `hel` / `lo`、未声明模型非 200
    （`tests/wasm_data_plane_e2e.rs`，Windows 打印 SKIP、CI linux 执行）；`probe_6/7/8` 分别覆盖
    流式下发、`tasks::run` 真实账号选择、30s tick 第二轮。**仍不可验证**：仓库无真实第三方
    atomcode wasm 包（无账号不可对话，不做假验证）。
11. **存量「handler 有、路由无」→ 已处置**：
    - `cache.rs::handle_cache_info`、`security.rs::handle_reset_security`：两处「未来实现」占位
      handler 已删除（全仓 grep 零引用）。
    - `dashboard.rs::handle_channel_stats`：已注册 `GET /api/dashboard/channel-stats` + openapi
      登记；与 `/api/dashboard/channel_health` 数据源不同（配置统计 vs 健康巡检），不重复。
12. **前端死封装 → 已处置**：

    | 前端函数 | 指向 | v1.0.64 状态 |
    |---|---|---|
    | `batchAddAliasPrefix` | `POST /api/models/alias/prefix` | **已修**：后端 handler + 路由 + openapi 已补；整批先校验后写（merge=false 冲突 409、非法 400、同 target 幂等） |
    | `updateCheckinTravel` / `runCheckinTravelNow` | `PATCH/POST /api/checkin-center/travel[/run]` | **已修**：travel 排程移植（30s tick、按计划时刻去重、无插件稳定跳过不落标记） |
    | `listKeys` | `GET /api/keys` | **已删**：legacy 存量、零调用点，v1.0.64 删除封装 |
    | `getModelMappings` | `GET/PUT /api/settings` | **已删**：存量零调用点，v1.0.64 删除封装（新弹窗走 `/api/models/mappings`） |
    | `startPluginInstall` / `startPluginUpdate` | install-from-release / update | v1.0.63 已删（改 url 直装） |

## 六·五、本轮新补的「handler 有、路由无」（已修）

- **T5 模型映射三条**：`model_manage.rs` 的 `handle_list_mappings` / `handle_upsert_mapping` /
  `handle_remove_mapping` 定义了但 **main.rs 无路由、openapi.rs 无登记**，门禁两侧同时失明
  （真实侧不扫、文档侧不报）。已注册 `GET|POST /api/models/mappings` 与
  `POST /api/models/mappings/remove`，并补 openapi 文档。路由闸门数字随之 399→**401 真实 /
  395→397 文档**（GET+POST 同 path 记一条 path，故 3 handler = +2 path）。
- 配套说明：前端 `getModelMappings`（`api/index.ts`）原指向旧 `${API_BASE}/settings`，是
  **零调用点**的存量死封装；v1.0.64 已删除，新映射弹窗走 `POST /api/models/mappings` 族。

## 六·六、T12（Wasm 数据面）四项无法验证 → v1.0.64 已补证

v1.0.63 结束时的四点已在 v1.0.64 用「自建真 wasm 包 + 真实二进制 e2e」补证（详见 §六.10）：

1. **`resolve.rs` wasm 路由分支**：`tests/wasm_data_plane_e2e.rs` 经真实 `/v1/chat/completions`
   走通 wasm 包分支（`registry().is_plugin_owned` 闸门在位）；单测夹具构造完整 `AppState` 的空白
   由 e2e 实际替代。
2. **真实 SSE / 流式**：`fixture::streaming_wasm_module` 真导入 `agent2api.call`，经宿主导入面
   下发 `stream.head` + 分段 `stream.chunk`；`probe_6` 断言 head + 2 chunk，e2e 断言 SSE 含
   `hel` / `lo`。
3. **生产 `tasks::run` 走 `active_accounts()`**：`probe_7` 用真实 `AccountSessionStore::upsert`
   种子账号，断言选中真实账号（异家账号反向对照）。
4. **30s 循环到点判定**：`probe_8` 用 `start_paused` + `advance(30s)` 验证第二轮真实执行
   （真实墙钟等过 claim 的 `min_gap=1000ms`，`schedule_now` 排下一拍）。

## 六·七、本轮发现并修复的缺口总表（7 类）

| # | 缺口 | 形态 | 修复 |
|---|---|---|---|
| 1 | `plugin_login_interaction` 按错误文案猜语义 | 半死（降级匹配不上） | 改按 `RpcError.code == "unsupported"` 判（T2） |
| 2 | `GET /api/channels` 未打 `hidden` | 数据泄漏（隐藏账号可见） | 补 `hidden` 过滤（T4） |
| 3 | provider 开关三层错位 + `package_view` 无 `enabled` | 半死（开关不生效） | 三层开关 + `package_view` 补 `enabled`（T2/T13） |
| 4 | **Wasm 整块接线为零**（最大规模半死） | 公开面零引用 + trait 无实现者 | `manager::enable_wasm` 自建全部注入面；`main.rs` 三行任务接线（T3/T12/我） |
| 5 | **T5 三条映射路由双侧失明** | handler 有、路由无、门禁看不见 | 注册 `GET\|POST /api/models/mappings` + `.../remove` + openapi 登记（我） |
| 6 | **两个 T8 死封装** | 前端调不存在的后端端点 | 删 wrapper + 改 url 直装 + 清 i18n（T8 跟进） |
| 7 | 3 条非 wasm clippy（check 绿但 `-D warnings` 红） | 静态门禁漏网 | `plugins.rs`/`plugin_login.rs`/`schedules.rs` 三条 lint 修掉（owner） |

## 七、部署影响面

- **DB schema**：无迁移。新增状态全部走 `FileStore` KV（SQLite kv 表）。
- **`plugins_dir` 目录结构**：无变化（脚本包仍是 hash 目录名契约；wasm 包复用同一目录布局）。
- **二进制体积**：**未测量**（本波禁跑 release 构建）。预判 +15~30MB（wasmtime + cranelift）。
- **运行时资源**：wasm 实例仅在启用 wasm 包时创建；未启用时零开销。
- **配置**：无新增必填项（插件宿主 v1.0.55 起恒开，无可关开关）。

## 八、门禁与验证方式

```bash
# 后端静态四层
cargo fmt --all -- --check
cargo check --lib --tests
cargo clippy --workspace --all-targets -- -D warnings
cargo test --lib            # 连跑两次比对 passed/failed/ignored 三数（存在进程级单例）

# 路由闸门（真实路由 ↔ OpenAPI ↔ 豁免 ↔ router 文件白名单）
python tools/extract_routes.py

# 前端（必须在 frontend/ 下跑；仓根会捞 e2e Playwright spec）
cd frontend && npx tsc --noEmit && npm run lint && npm run i18n:check && npx vitest run
```

> 并发编辑同一工作树时「编译绿」有四个时间维陷阱（L0 解析 / L1 test-only / L1.5 dead_code /
> L-1 中间态），宣布树静止须同时满足 task 全 completed、`git status` 哈希与行数稳定、
> 两次 check 间隔 ≥90s 且 error/warning 数一致、`src/` 最新 mtime 未变。

## 八·五、门禁实测结果（v1.0.64 遗留收尾波，2026-11-17）

树静止口径：t1–t7 + t11 全 completed、integrator 停手后按序跑；命令一律
`cmd > log 2>&1; echo EXIT=$?`，不用管道吞码。

| 步 | 命令 | 结果 |
|---|---|---|
| 1 | `cargo fmt --all -- --check` | **EXIT=0 / 0 diff** |
| 2 | `cargo check --lib --tests` | **EXIT=0 / 0 error / 0 warning** |
| 3 | `cargo clippy --workspace --all-targets -- -D warnings` | **EXIT=0 / 0 error** |
| 4a | `cargo test --lib`（integrator 第一次） | **2106 passed / 1 failed / 0 ignored** |
| 4b | `cargo test --lib`（integrator 第二次） | **2106 passed / 1 failed / 0 ignored** |
| 4c | 失败集比对 | integrator 两次一致（唯一 `log::tests::tz_migrate_zero_to_eight_rewrites_old_keys`）；**verifier 复跑第二次多出 1 条 flaky**，见下 |
| 5 | 分区 allow 反向自证 | 见下 |
| 6 | `python tools/extract_routes.py .` | **EXIT=0**；真实 409 / 文档 405 / 豁免 5；[A]–[G] 全 0 |

**verifier 独立复跑（t10）**：run1 = 2106 passed / 1 failed；run2 = 2105 passed / **2 failed**
（多出 `channel::balance_refresh::tests::sweep_survives_channel_deleted_mid_round`，
`balance_refresh.rs:398` 的 50ms 时序前提）。该文件本波未改动，判为**环境 flaky、不阻塞交付**；
但「两次失败集完全一致」这一条**不成立**，如实记录（需 Linux CI 复跑确认）。

**前端（cwd=frontend）**

| 项 | 命令 | 结果 |
|---|---|---|
| tsc | `npx tsc --noEmit` | **EXIT=0** |
| lint | `npm run lint` | **EXIT=0**（max-warnings 0） |
| i18n | `npm run i18n:check` | **EXIT=0**；zh=en=**4057**；缺失 0；集合一致 |
| i18n todo | `npm run i18n:check-todo` | **EXIT=0**；en.json 无 `[TODO en]` |
| 单测 | `npx vitest run` | **EXIT=0**；20 files / 149 tests passed |
| static 重建 | `npm run build` | **EXIT=0**；`index.html` md5 `6482fdd7…` → **`b9e060dbe3571953baf14d15254c088b`**；`static/version.json`=1.0.64 |

### 唯一失败测试（沿用既有裁决）

`log::tests::tz_migrate_zero_to_eight_rewrites_old_keys`：**`src/log/` 与 HEAD 字节一致**
（`git diff HEAD -- src/log` 为空），UTC+8 开发机 `chrono::Local` 使断言前提失效，CI runner=UTC
应绿；属于存量测试假设问题，不改。

### dead_code 分区 allow 反向自证（v1.0.64）

- 本波**新增 0 处** `#[allow(dead_code)]`；净删除 3 处（`wasm/mod.rs` 的 catalog 分区、
  `account_search.rs` 两处）。
- `native` 分区（删 allow → `cargo clippy --lib -D warnings`）：**7 条**，全部
  `wasm/native.rs`，零其它文件混入。`tasks` 分区：**8 条**，全部 `wasm/tasks.rs`
  （`StoreSettings` / `install_*` / `scheduler_*` / `spawn` / `shutdown` 等），零其它文件。
  恢复后三口径 clippy **EXIT=0**。
- `catalog` 分区 allow 已删除且 clippy 绿 = 目录接线后无残留死码。
- 新增 3 处非 dead_code 定向 allow：`double_must_use` ×2（`travel.rs` / `account_search.rs`
  的 `async_trait` trait，repo 内既有先例 `account_login/mod.rs`）、`result_large_err` ×1
  （`script/websocket.rs::Socket::send` 透传 tungstenite 错误）。

## 八·六、改动文件清单（分组）

> 统计口径：`git status --porcelain`（本波全程未 commit）。数字为 2026-11-17 07:00 快照。

| 分组 | 数量 | 说明 |
|---|---|---|
| **共享文件（integrator 改）** | 7 | `src/main.rs`（子 router merge + 5 项 T1 接线 + wasm 三行 + 8 个 account handler + T5 映射路由）、`src/lib.rs`、`Cargo.toml`/`Cargo.lock`（wasmtime）、`tools/extract_routes.py`（ROUTE_FILES + check_router_coverage）、`src/api/admin/openapi.rs`（本波路由文档）、`docs/api-documentation.md` |
| **`docs/a2a-alignment/`** | 1 | 本文件（新增目录） |
| **`static/`** | 155 | 前端构建产物重建（79 新增 chunk + 76 修改/删除，hash 文件名导致）；`index.html` md5 已变 |
| **`src/` 新增（`??`）** | 52 | 成员模块新文件（wasm 21 / tool_services 11 / api/admin 13 / account_session 4 / 其余） |
| **`src/` 修改（`M`）** | 51 | 成员模块改动（script host / checkin / model / bridge / plugin_host 等） |
| **`frontend/src/`** | 38 | 六页 + 组件 + i18n 六语 + types |

**我（integrator）直接改的文件**：`src/main.rs`、`src/lib.rs`、`Cargo.toml`、`Cargo.lock`、
`tools/extract_routes.py`、`src/api/admin/openapi.rs`、`docs/api-documentation.md`、
`docs/a2a-alignment/README.md`。其余全为成员产出（我只做集成验证与门禁）。
**版本号（VERSION / Cargo.toml version / package.json version）全程未动**。

## 九、给 verifier（T10）的验收点

### 9.1 Wasm 沙箱隔离证据（代码位置，供逐条复核）

| 断言 | 位置 | 读法 |
|---|---|---|
| `Module` 跨调用复用（编译一次） | `src/plugin_host/wasm/runtime.rs:41` 字段 `module: Module`；`:100` `Module::new` | 结构体持有 `Module`，`invoke` 不重编译 |
| 每次调用新建 `Store`（实例状态不跨调用泄漏） | `runtime.rs:263` `let mut store = Store::new(...)` | 在 `invoke` 内部，逐调用 |
| 每次调用新建实例 | `runtime.rs:314` `self.linker.instantiate_async(&mut *store, &self.module)` | 同上 |
| 内存/表增长失败即 trap（非静默截断） | `runtime.rs:257` `.trap_on_grow_failure(true)` | `StoreLimitsBuilder` 链上 |
| 内存/表容量上限 | `runtime.rs:250-256` `memory_size` / `tables` / `table_elements` | 见 `limits.rs` 口径 |
| 燃料（fuel）耗尽中断 | `runtime.rs:60-62` `Config.consume_fuel(true)`；`limits.exhausted` 判定 | `runtime.rs:291` 把「耗尽」映射成 `plugin_resource_limit` |
| 引擎进程级单例 | `runtime.rs:47` `static ENGINE: OnceLock` | 只初始化一次 |
| ABI 版本校验 | `runtime.rs:318` 比 `a2a_abi_version()` 与 `manifest::ABI` | 版本不符即拒绝 |
| 取消/超时 | `runtime.rs:299-306` `tokio::select!`（shared.cancel / cancel / timeout） | 三个分支 |
| provider 钉死 | `runtime.rs:196-200` 非 `init` 调用要求 `params.provider == manifest.provider` | 防越权调用他家 |

### 9.2 行为探针（真实服务，非编译绿）

- 模型能力覆盖：`POST /api/models/capabilities` 200 + 重启后 `sqlite kv` 表 `model_capability_overrides` 有值。
- 提供商设置：`GET /api/plugins/provider-settings` **读侧 503→200 `{"settings":[]}` 已实证**；
  写侧（PATCH configure）因当前环境无声明 settings schema 的插件包而未验证，由
  `cargo test --lib plugin_host::provider_settings` **11 passed** 覆盖 configure 语义。
- 账号池 8 个补注册 handler：`/api/accounts/*` 影子路径可达（非 404）。

### 9.3 门禁复跑命令

见 §八。**注意 `cargo test --lib` 必须连跑两次**（进程级单例顺序依赖），比对
`test result:` 的 passed/failed/ignored 三数与失败用例名排序列表。

### 9.4 `AccountSessionRequest` 11 个新字段的 serde 三态核查（已完成）

11 个 A2A 插件账号字段**全部带 `#[serde(default)]`**，且**全部有真实消费点**（不只是解析）：

| 字段 | serde | 消费点 |
|---|---|---|
| `auth_file_content` / `auth_file_b64` / `auth_file_name` | `default` + camelCase alias | `account_sessions.rs:1189-1191` → `auth_file::import_*` |
| `plugin_secret` / `plugin_public` / `plugin_identity` | `default` + alias | `account_session/plugin.rs`（21 / 12 / 4 处） |
| `hidden` | `default`（同名，无 alias） | 管理面列表过滤（10 处） |
| `allow_route_skip` / `user_agent` / `usage_query` / `rate_limits` | `default` + alias | 选路 / 出站 / 余额查询 / 限流（7 / 5 / 6 / 9 处） |

「三态」语义（缺省 = 保持原值 / 显式值 = 设置）在 handler 里逐字段实现，不是一律覆盖。
