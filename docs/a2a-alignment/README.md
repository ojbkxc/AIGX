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
| 工具服务 | 部分对齐 | 管理面完成；**账号型搜索运行时 / 图片生成运行时 / 视觉模型目录校验未移植**（§六.2） |
| 维护任务明细 | 完成 | 管理面 + 记录分页 |
| Wasm 沙箱 | 部分对齐 | 21 文件 + 79 测试全绿（含 8 个真跑 wasmtime 的行为探针）；运行期接线已做（`manager`/`PluginBackend` 可走 wasm 包、`main.rs` 三行任务接线）；**未验证的是「真实第三方 .wasm 发布包」的安装→启用→数据面转发全链路**（仓库无此类包），`native.install_source` 延后（§六.3/§六.10） |
| 前端六页 | 完成 | tsc/lint/i18n **4057** 键/vitest 20 files/149 tests 全绿（须 `cd frontend` 跑） |

## 六、未完成项（如实清单）

1. **`wasm::catalog` 整块未接线**：删除 `src/plugin_host/wasm/mod.rs` 的
   `#![allow(dead_code)]` 后该块仍报 dead_code（预期：本期不接线的公开面）。
   不得以「N 个符号」这类旧数字当判据，以删 allow 后的真实输出为准（见 T9 报告）。
2. **T7 未移植三项**：① 账号型搜索运行时（`core/tool_services/account_search.rs` 的执行体
   只接了配置面）；② 图片生成运行时（`image_jobs/image_mcp/image_bridge` 未移植）；③ 视觉
   分析未接「模型目录校验」（上游会校验所选模型在目录中支持视觉）。
3. **Wasm `native.install_source` 延后**：不注入时 `native.snapshot` 为空表、
   `native.call` 返回 `native_helper_not_authorized`（报错不 panic），native helper 快照
   链路未做。
4. **`script.ws.*` proxy 参数被忽略**：A2A 的 ws 桥走宿主 Bun 侧代理配置；AIGX 用
   `tokio-tungstenite` 直连，`proxy` 参数解析但不生效（多数场景无感，代理出口场景有差）。
5. **`script.signature` 复用 antigravity 缓存**：语义对齐，但缓存键与刷新策略沿用 AIGX
   既有实现，未按上游逐字段比对。
6. **`script.packages` / `script.settings` 无 host→gateway RPC 封装**：目前这两类调用由宿主
   mjs 直接读本地文件/环境变量完成，没有走上游的 RPC 回环（行为等价，形态不同）。
7. **原生插件包缺 provider 级开关**：`PackMeta` 无 provider 级字段，只支持整包启停；
   前端「三层开关」的 provider 粒度对原生包呈只读。
8. **`getPluginProgress`**（`frontend/src/api/index.ts`）零调用点 —— HEAD 存量函数，非本波
   新增，保留未删。
9. **`account_sessions.rs` 若将来挪进 `router()`**：必须同步加进 `tools/extract_routes.py` 的
   `ROUTE_FILES`（当前 handler 逐个在 main.rs 注册，不在白名单扫描面；`check_router_coverage()`
   会在挪动后自动抓白名单缺口）。
10. **未验证「真实第三方 `.wasm` 包」的安装→启用→数据面转发全链路**：**单测层已真跑 wasmtime**
    ——`wasm/probe.rs` 有 **8 个行为探针**（`cargo test --lib plugin_host::wasm` → 79 passed），
    用手写合法 wasm 二进制（`fixture.rs` 生成：`\0asm` 头 + 真实段/导出函数）走
    「装配 → 加载 → `instantiate_async` → 调用 → 关停」全链，**零 mock**（注入的 `WasmAccountAccess`
    等只是宿主侧回调桩，wasm 执行是真的）。**未验证的是仓库没有的「真实发布包」端到端**
    （AIGX 不打包分发 AGPL 二进制，也无此类包可装）。隔离证据：`Store::new` 每调用重建
    （`runtime.rs:263`）、`instantiate_async`（`:314`）、`trap_on_grow_failure(true)`（`:257`）、
    `ENGINE: OnceLock`（`:46`）。
11. **存量「handler 有、路由无」（非本轮引入，只记录不修）**：
    - `cache.rs::handle_cache_info`（`GET /api/cache/info`）—— 函数体自称「未来实现」，返回
      `"Cache information endpoint (to be implemented)"` 的占位文案。
    - `security.rs::handle_reset_security`（`POST /api/security/reset`）—— 函数体自称「未来实现」，
      直接返回 `data: null`。
    - `dashboard.rs::handle_channel_stats`（预期 `GET /api/dashboard/channel-stats`）—— 实现完整
      （逐渠道统计行 + 冷却渠道数），但无路由注册；与已注册的 `/api/dashboard/channel_health`
      数据源不同（后者是健康巡检记录，前者是渠道配置统计），不构成重复端点。
    - 三条都属 HEAD 存量，本波次未触碰，留待后续波次决定接线或删除。
12. **前端死封装（调用了不存在的后端端点，全部静默降级）**：

    | 前端函数（`api/index.ts`） | 指向 | 后端现状 | 引入 | 表现 |
    |---|---|---|---|---|
    | `listKeys` | `GET /api/keys` | legacy 已删（1.0.x 收敛到 `/api/tokens`） | 存量 | 零调用点 |
    | `getModelMappings` | `GET/PUT /api/settings` | 旧封装 | 存量 | 零调用点（新映射弹窗走 `/api/models/mappings`） |
    | `batchAddAliasPrefix` | `POST /api/models/alias/prefix` | 无此路由（注释自称「待实现」） | 存量 | 报错兜底 |
    | `updateCheckinTravel` / `runCheckinTravelNow` | `PATCH/POST /api/checkin-center/travel[/run]` | 无此路由（A2A `growth_scheduler` 未移植） | 存量 | 404 → 降级（注释自称 be-fix #16-B 契约） |
    | `startPluginInstall` | `POST /api/plugins/install-from-release` | 无此路由（A2A `install_from_release` 的 operations 机制未移植） | **本波 T8** | 404 → 回落 url 直装 |
    | `startPluginUpdate` | `POST /api/plugins/update` | 该 path 只注册了 GET（`handle_check_plugin_updates`） | **本波 T8** | 405 → 回落 url 直装 |

    「后台任务式插件安装/更新」在 AIGX 是空壳（`GET /api/plugins/progress` 恒返回
    `{operations: []}`），实际全走 url 直装回落，不报错给用户但「任务已启动」toast 永不出现。
    **处置：两个死封装已删**（T8 跟进），`handleInstall`/`handleUpdate` 改纯 url 直装，
    无匹配平台时报「该插件不提供当前平台（{{target}}）的安装包」；`PluginStartOperation` 类型
    与两条 i18n key 一并清除。`GET /api/plugins/progress` 保留（A2A 契约形状）。

## 六·五、本轮新补的「handler 有、路由无」（已修）

- **T5 模型映射三条**：`model_manage.rs` 的 `handle_list_mappings` / `handle_upsert_mapping` /
  `handle_remove_mapping` 定义了但 **main.rs 无路由、openapi.rs 无登记**，门禁两侧同时失明
  （真实侧不扫、文档侧不报）。已注册 `GET|POST /api/models/mappings` 与
  `POST /api/models/mappings/remove`，并补 openapi 文档。路由闸门数字随之 399→**401 真实 /
  395→397 文档**（GET+POST 同 path 记一条 path，故 3 handler = +2 path）。
- 配套说明：前端 `getModelMappings`（`api/index.ts`）指向旧 `${API_BASE}/settings`，是**零调用点**
  的存量死封装；新映射弹窗走 `POST /api/models/mappings` 族。二者并存，未删旧封装。

## 六·六、T12（Wasm 数据面）四项无法验证（如实记录）

plugin-wasm 报告以下四点在本环境无法端到端验证，**不改变 §三 的 wasm「unverified」状态**：

1. **`resolve.rs` 的 wasm 路由分支无 `AppState` 构造器**：`resolve.rs:439` 加了三级路由，
   但单测里没有能构造完整 `AppState`（含 wasm 宿主）的夹具 → 分支逻辑只有编译级保证。
2. **真实 SSE / `tasks.list` 数组无法夹具模拟**：需要真实 wasm 包返回流式响应，手写字节夹具
   只能覆盖非流式。
3. **生产 `tasks::run` 走 `active_accounts()` 未直测**：`tasks::run` 的账号选择依赖运行时
   账号池状态，单测用的是注入的 mock，未验证真实 `active_accounts()` 路径。
4. **30s 循环到点判定未直测**：`spawn()` 的 tick 到点逻辑靠时钟推进，单测用 `spawn_with`
   注入令牌但不真正等 30s，到点边界未覆盖。

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

## 八·五、T9 门禁实测结果（2026-11-17 07:00，停手后）

所有命令重定向取码（`cmd > log 2>&1; echo EXIT=$?`），不用管道。

| 步 | 命令 | 结果 |
|---|---|---|
| 1 | `cargo fmt --all -- --check` | **EXIT=0 / 0 diff**（先跑过一次 `cargo fmt --all`，格式化了 45 个成员新增文件；`main.rs`/`lib.rs`/`openapi.rs` 三文件 mtime 未变，证明 fmt 未动我的文件） |
| 2 | `cargo check --lib --tests` | **EXIT=0 / 0 error / 0 warning** |
| 3 | `cargo clippy --workspace --all-targets -- -D warnings` | **EXIT=0 / 0 error**（覆盖 `aigx-net` 成员与 bins） |
| 4a | `cargo test --lib`（第一次） | 2010 passed / **1 failed** / 0 ignored |
| 4b | `cargo test --lib`（第二次） | 2010 passed / **1 failed** / 0 ignored |
| 4c | 失败集比对 | **完全一致**（同一用例 `log::tests::tz_migrate_zero_to_eight_rewrites_old_keys`） |
| 5 | 分区 allow 确认 + 反向自证 | `wasm/mod.rs` 的**模块级** `#![allow(dead_code)]` **已无**；catalog/native/tasks 三处子模块各带理由 allow + 6 处逐项 allow（见下）。**反向自证**：临时删 tasks 的 allow → `cargo clippy --lib -D warnings` **精确报出 8 条，全部在 `wasm/tasks.rs`，零其它文件混入** → 已恢复（md5 与备份一致）。恢复后 `--lib` 与 `--workspace --all-targets` 两口径均 **EXIT=0** |

**附加门禁（同样实测）**

| 项 | 命令 | 结果 |
|---|---|---|
| 路由闸门 | `python tools/extract_routes.py .` | **EXIT=0**；真实 401 / 文档 397 / 豁免 5；[A]–[G] 全 0；`router_coverage_gap` 空 |
| 前端 tsc | `npx tsc --noEmit` | **EXIT=0** |
| 前端 lint | `npm run lint` | **EXIT=0**（max-warnings 0） |
| 前端 i18n | `npm run i18n:check` | **EXIT=0**；zh=en=**4057**；缺失 0；集合一致 |
| 前端 i18n todo | `npm run i18n:check-todo` | **EXIT=0**；en.json 无 `[TODO en]` |
| 前端单测 | `npx vitest run` | **EXIT=0**；20 files / 149 tests passed |
| static 重建 | `npm run build` | **EXIT=0**；`index.html` md5 `78bcfbd2…` → **`6482fdd7d9a59502959de78d491f45b9`**（变了=重建成功；assets 99→103） |

### 唯一失败测试的定性（`log::tests::tz_migrate_zero_to_eight_rewrites_old_keys`）

- **`src/log/` 与 HEAD 字节一致**（`git status` 零改动）——**存量测试，非本波引入**。
- **根因是测试没考虑开发者时区**：`date_key_at(t, 0)` 用 `report_tz_of(0)` → `chrono::Local`
  系统偏移；`date_key_at(t, 8)` 用 +8。**UTC+8 开发机上两者相等** → 迁移是 no-op → 断言
  `old_b == new_a`（「20:00Z 在 +8 归次日」的错位前提）不成立 → 崩（`left "2026-11-15"
  right "2026-11-14"`）。
- 测试注释声称「只有在 UTC+9 及以东的机器上跑才会退化」——**这句是错的，UTC+8 也退化**。
  **中国时区开发机上此测试必红。** CI runner=UTC 时应绿。
- Windows 上 `TZ=UTC cargo test` 无效（`chrono::Local` 不读 `TZ`），已实测仍失败。
- **处置：如实记录、不改代码**（lead 裁决）。这不是「环境特殊」而是「测试假设了开发者时区」。

### dead_code 分区 allow 的实测数字（三区反向自证）

`cargo clippy --lib` 视角（lib target 看不到 bin 的 `main.rs` 调用点）下，**逐区删 allow 反向自证**
（每区删 allow → 跑 `cargo clippy --lib -D warnings` → 记录 → 恢复 → `touch`）：

| 区 | 删 allow 后报出 | 位置 | 说明 |
|---|---|---|---|
| `tasks` | **8 条** | 全部 `wasm/tasks.rs`，零其它文件混入 | `StoreSettings`/`install_store_settings`/`install_settings`/`scheduler_token`/`scheduler_started`/`spawn`/`shutdown` + `new/key`。9 处接线只在 bin 侧生效（lib target 结构上看不到 `main.rs` 调用者）+ `checkin_time`/`spawn_with` |
| `catalog` | **15 条** | 全部 `wasm/catalog.rs`，零其它文件混入 | 本期不接（计划内唯一残留） |
| `native` | **1 条** | 全部 `wasm/native.rs` | `install_source`，deferred |

另有 **6 处逐项 allow**（`WasmOptions::dir`、`http::MultipartPart::is_text`、`host::hosts`、
`mod::loaded_providers`、`tasks::checkin_time` 待签到中心接 wasm 签到窗口、`tasks::spawn_with`
仅测试用）+ **1 处 `#[cfg(test)]` 专用**（`fixture.rs::module_with_len_dispatch`，夹具辅助）。
三区反向自证证明**分区 allow 没有掩盖其它文件的接线遗漏**。恢复后三口径（`--lib` /
`--lib --tests` / `--workspace --all-targets`）clippy 均 **EXIT=0**。

> **allow 总账**：`grep -rn '#\[allow(dead_code)\]' src/plugin_host/wasm/*.rs` = **10 行** =
> **3 分区**（catalog/native/tasks，`mod.rs:45/58/67`）+ **6 逐项**（`host.rs:46/515`、
> `http.rs:26`、`mod.rs:333`、`tasks.rs:287/608`）+ **1 test-only**（`fixture.rs:62`）。

> 全仓**模块级** `#![allow(dead_code)]` 实测共 **3 处**（`src/main.rs:6`、`src/api/openai/mod.rs:11`、
> `src/image_compat/mod.rs:29`），均为存量、与本波无关；`src/plugin_host/wasm/mod.rs` 的模块级
> allow 已在本波删除（改为上述分区形态）。

> **坑**：`cp` 恢复文件后 mtime 落在同一秒时，cargo 可能复用旧编译指纹给出**假红**（报错与
> 探针阶段逐字相同）。判别法：把报错位置集合与探针 log 做 diff，逐字相同=指纹复用。
> 恢复后 `touch` 广播新 mtime 即可。本表数字均在 `touch` 后复测。

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
