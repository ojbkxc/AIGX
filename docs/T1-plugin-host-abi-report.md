# T1 交付报告 —— AIGX 插件宿主 ABI 全量对齐

> 负责人：plugin-abi｜任务 #2｜日期：2026-11-16
> 门禁：**L1 绿 / L2 绿 / L3 clippy（本人文件）绿**；`cargo test --lib plugin_host::` → 365 passed / 0 failed。
> 未 commit（按用户口径，push 需当轮点头）。

---

## 一、门禁证据

| 门禁 | 命令 | 结果 |
|---|---|---|
| L1 | `cargo check --lib` | 绿（`Finished dev profile`） |
| L2 | `cargo check --lib --tests` | 绿（`Finished dev profile`） |
| L3 | `cargo clippy --lib --tests`，过滤 `plugin_host/script\|provider_settings\|script_plugins` | 0 命中（本人文件干净） |
| 单测 | `cargo test --lib plugin_host::` | **365 passed / 0 failed** |
| 格式 | `rustfmt --edition 2021 <本人每个文件>` | 已格式化 |

---

## 二、文件清单

### 新增
- `src/plugin_host/script/http.rs` —— `script.http.open/read/close`（A2A 形状，复用 `Transport` 会话，`SessionMap`）。4 测试。
- `src/plugin_host/script/websocket.rs` —— `script.ws.open/read/send/abort`（tokio-tungstenite 直连，自定义 `Cancel`）。6 测试。
- `src/plugin_host/script/usage.rs` —— `script.usage` → 拼 `RequestLog` 入 `LogStore`。2 测试。
- `src/plugin_host/script/icons/{mod.rs,formats.rs,cache.rs,download.rs}` —— 图标缓存（`{sha256}.{ext}` + `index.json`，容量上限，SSRF 防护）。
- `src/plugin_host/provider_settings/{mod.rs,schema.rs}` —— 提供商设置（A2A Schema 移植 + CAS 存储）。11 测试。
- `src/plugin_host/script/host/*.mjs` —— **29 个宿主 mjs**（27 新 + 覆盖原 5），每个文件头加 AGPL 来源注释。
- `src/api/admin/plugin_provider_settings.rs` —— 由 plugin-api 提供（其文件，我未改）。

### 修改（本人边界内）
- `src/plugin_host/script/types.rs` —— `ScriptInitPayload` 改 A2A 平铺形状；`ScriptPackage` 补 `provider_options/disabled_providers/provider_aliases/settings_schema/icon/provider_icons/error`；`ScriptProviderInfo` 补 `#[serde(flatten)] extra`。
- `src/plugin_host/script/host_rs.rs` —— HOST_FILES 扩到 29；init 帧平铺；`build_init_payload` 新签名；**包内 `disabled_providers` 在 init 载荷剔除**；对表测试改可达性遍历。
- `src/plugin_host/script/runtime.rs` —— `host_env` 注入 + `set_host_env/host_env`；`reserved_providers` 插件优先 + **包内 disabled 不解除 reserved**；`script_takeover_blocked` 去 `is_native_provider`；`register_provider_settings`；`call_provider` 公开方法；`compute_routing`/`claimed` 过滤 disabled。
- `src/plugin_host/script/rpc.rs` —— **重写为真实线面**（见 §五 缺口 1）。
- `src/plugin_host/script/protocol.rs` —— 加字符串 id 表（`open_named/push_named/close_named`）服务 `script.stream.*`。
- `src/plugin_host/script/transport.rs` —— `proxy_wire` + `resolve_proxy`（`script.proxy.resolve`）。
- `src/plugin_host/script/callbacks.rs` —— 全量重写分发（http/ws/stream/proxy/signature/usage + 兼容旧 transport/protocol）。
- `src/plugin_host/script/operations.rs` —— `update_settings` / `set_provider_enabled` / `check_updates`。
- `src/plugin_host/registry.rs` —— 插件优先 `is_plugin_owned` + `HARD_RESERVED` / `is_hard_reserved`。
- `src/plugin_host/manager.rs` —— 加 `process_for_pack`（Wasm 沙箱按包 id 取进程；**非我主动改**，需核实归属）。
- `src/plugin_host/mod.rs` —— 加 `pub mod provider_settings;`（**一行，需 integrator 保留**）。
- `src/plugin_host/script/mod.rs` —— 加 `pub mod rpc;`（**缺口 1 修复**）。
- `src/api/admin/script_plugins.rs` —— `ScriptInstallBody` 兼容双形状；新增 `handle_script_change/settings/prompts/check/catalog_refresh` + 3 个 body 结构。

---

## 三、公开接口（已告知 plugin-api 的签字面）

### `plugin_host::provider_settings`
```rust
pub struct ProviderSettingsError { pub status_code: u16, pub message: String } // Display+Error
pub fn snapshot() -> serde_json::Value;                  // 同步；{settings:[{provider,label,package,schema,values,effective,invalid,revision}]}
pub async fn configure(provider, revision, values) -> Result<Value, ProviderSettingsError>; // revision CAS，409
pub fn available(provider) -> bool;
pub fn user_owned(provider, field) -> bool;
pub fn storage_provider(provider, field) -> String;
pub fn install(store: Arc<FileStore>);                   // main.rs 调（已接线）
pub fn register_host_schemas(entries: Vec<(String, RegisteredSchema)>);
pub fn clear_host_schemas();
pub struct RegisteredSchema { pub schema, pub package, pub label, pub generation }
```

### `plugin_host::script::icons`（plugin-api 的 `plugin_icons.rs` 依赖）
```rust
pub fn read(file: &str) -> Option<(Vec<u8>, &'static str)>; // 异步侧 spawn_blocking
pub fn wake(force: bool);
pub fn install(root: PathBuf);
pub fn spawn(script_root, store);       // runtime.start() 调
pub fn installed(package) -> Option<String>;   // /plugins/script-icons/{file}
pub fn provider(package, id) -> Option<String>;
pub fn declaration(value: &str) -> Option<String>;
pub fn source(value: &str) -> Option<String>;  // cache 重导出
```

### `ScriptHostManager` 新增
```rust
pub fn set_host_env(&self, env: HostEnv);      // main.rs 构造后注入
pub fn host_env(&self) -> HostEnv;
pub async fn call_provider(&self, provider, method, params, timeout) -> Result<Value, String>;
```

---

## 四、T9 共享 patch 清单（**必须做，我不改这些文件**）

### `src/main.rs`
1. **`script_host.set_host_env(host_env.clone())`**（功能半死型）：
   - 现状：L566 构造 `host_env` → L578 被 move 进 `PluginHostManager::new`；L589 `ScriptHostManager::new` 之后**没有任何地方给脚本宿主注入 host_env**。
   - 不注入的后果：`host_env()` 返回 `HostEnv::default()` → init 载荷的 `env` 全空（`data_dir=""`、`target=""`）→ Bun 宿主按空目录跑，`data_dir` 下的 work 目录/缓存全落错地方，或直接 init 失败。
   - 改法：L578 `host_env,` → `host_env.clone(),`；L596 `)` 之后加一行 `script_host.set_host_env(host_env);`（或 `host_env.clone()` 视后续是否还用）。
2. **`crate::plugin_host::script::usage::install(log_store.clone());`**（功能半死型）：
   - 不装的后果：脚本 `script.usage` 回调返回「用量日志存储未就绪」，用量不上报（静默）。
   - 位置：L490 `let log_store = ...` 之后、或紧邻 L283 的 `provider_settings::install` 附近（log_store 在 L490 才构造，放 L490 之后）。
3. **路由 5 条**（否则新 handler 全 404）：
   ```rust
   .route("/api/plugins/scripts/change", post(api::admin::script_plugins::handle_script_change))
   .route("/api/plugins/scripts/settings", post(api::admin::script_plugins::handle_script_settings))
   .route("/api/plugins/scripts/prompts", post(api::admin::script_plugins::handle_script_prompts))
   .route("/api/plugins/scripts/check", get(api::admin::script_plugins::handle_script_check))
   .route("/api/plugins/scripts/catalog/refresh", post(api::admin::script_plugins::handle_script_catalog_refresh))
   ```
   现有 scripts 路由块在 `src/main.rs` L2858–2886，追加到 `handle_script_rebuild` 之后即可。

### 已由别人做完（我核过，无需再做）
- `provider_settings::install(store.clone())` —— main.rs L283 **已在**。
- `model::capability_overrides::install(store.clone())` —— main.rs L287 **已在**（model-mgmt 报的项）。
- `/api/models/mappings*` 路由 —— model-mgmt 负责，非我文件。

### `src/plugin_host/mod.rs`（integrator 重写时保留）
- `pub mod provider_settings;` —— 我加的，plugin-api 的 `plugin_provider_settings.rs` 依赖。

---

## 五、本轮新修的两个真缺口（由 lead 提问暴露）

### 缺口 1：`script/rpc.rs` 是对表契约但从未编译进 lib
- 现象：`script/mod.rs` 里**没有** `pub mod rpc;` → 该文件是孤儿，`dispatch_methods_match_js_table` 测试从未跑过。
- 暴露的真相：原方法表缺 `provider.info`（JS `dispatch()` 有 25 个 case，表只有 24）；回调面常量还是旧的 `script.transport.*`/`script.protocol.chunk/finish`，而真实 JS 用 `script.http.*`/`script.ws.*`/`script.stream.*`/`script.proxy.resolve`/`script.signature`/`script.usage`。
- 修复：`pub mod rpc;` 接上；方法表按真实 JS 重写为 25 个；回调面常量换成真实线面（旧 transport/protocol 保留为兼容常量）。测试改**双向**（Rust→JS 逐 case 查 + JS→Rust 逐 case 反查 + 数量相等），并加 `callback_method_names_match_host_mjs`（每个回调常量必须被宿主 JS 以 `rpc("<name>", …)` 调用）。

### 缺口 2：包内 `disabled_providers` 是死字段
- 现象：`operations::set_provider_enabled` 会写 `ScriptPackage.disabled_providers`，但**没有任何读取点**——写了不生效。A2A 的 `active_ids` 会把它从加载集合剔除。
- 修复（四处）：`host_rs::build_init_payload`（init 载荷 providers 剔除 disabled）、`runtime::compute_routing`（兜底路由不登记 disabled）、`claimed`（不参与 claimed）、`reserved_providers`（disabled 者不解除 reserved，让位原生）。加回归测试 `routing_skips_provider_disabled_inside_package`。

---

## 六、未完成 / 已知差异（如实记录）

1. **`script.ws.*` 的 proxy 参数被忽略（走直连）**：AIGX reqwest 0.13 无 `upgrade()`，改用 `tokio_tungstenite::connect_async_tls_with_config` 直连；proxy 不支持。已写进 `websocket.rs` 文件头注释。需要代理的 WS 包会失败。
2. **`script.signature` 用 AIGX 现有 antigravity 签名缓存**（`bridge::session::antigravity_signature_cache`），不是 A2A 的独立实现；语义等价，数据源不同。
3. **`script.packages` / `script.settings` 的 gateway→host RPC 封装未做**：只做了等价物（图标刷新读 `storage::load_packages`；schemas 由 `register_provider_settings` 从 init 应答注册）。若将来需要主动向宿主查 `script.packages`，需补一层 host RPC 封装。
4. **`login.interaction` / `login.submit` 不在脚本能力面**（A2A 同口径，见 §七 Q1）。
5. **仅为自己将来预留、当前无调用方的公开函数**（T9 删 dead_code allow 时勿误判）：
   - `provider_settings::available` / `user_owned` / `storage_provider` —— 给 `settings.set` 拒绝覆盖用户声明字段用，当前无外部调用方。
   - `icons::installed` / `provider` / `declaration` / `source` —— 给管理面展示图标 URL 用，当前 `plugin_icons.rs` 只调 `read`；其余为预留。
   - `ScriptProviderInfo::default_models()` —— 宿主上报默认模型清单的读取辅助。
   - `transport::proxy_wire` —— 仅 `transport.rs` 内部用（非预留）。

---

## 七、三条语义确认（回应 lead）

### Q1 `login.interaction` / `login.submit`：**包级钩子，不进 CAPABILITY_METHODS**
- A2A `core/plugin_host/rpc.rs:823` 只在 wasm 宿主转发（`options.wasm.is_some()`）；脚本宿主 `dispatch()` 无这两个 case，会落 `default: throw unsupported()`。
- AIGX 照抄分层：脚本包自己实现登录交互，宿主不转发。管理面 `plugin_login_interaction` 对脚本家按「无交互」处理。
- ⚠️ **转 plugin-api**：`plugin_login_call` 把错误拼成 `"插件 login.interaction 失败：{message}"`，而 JS `unsupported()` 文案是 `"Plugin does not provide this capability"`（不含 unsupported）——降级匹配 `error.contains("unsupported")` **匹配不上**，会变 400。建议按 `RpcError.code == "unsupported"` 判，或把 `"does not provide"` 加进匹配。

### Q2 `resolve.rs` 插件崩溃回落：**回落到「还存在的原生渠道」；只剩插件渠道时确实 502**
- 原生 62 家在数据面是**渠道**（`ChannelType::Account` + 账号池），不是硬编码 bridge。插件崩 → 插件渠道分支不 push → 原生 Account 渠道仍被选中（前提：运维建过且未停用）。
- 最坏情况（只有插件渠道）：502 no_bridge，非插件优先引入的回归，是「本来只有一条路」。

### Q3 `plugin_disabled` gates：**手动整家关停，与插件崩溃正交，不干扰**
- 唯一写入口 `POST /api/plugins/provider`；持久化 KV `plugins:disabled_providers`。崩溃只让 `process_for_provider`/`route_for_provider` 返 None，不碰 gates。
- ⚠️ `all_provider_kinds()` 只列原生 registry，脚本独占 provider（kiro/astudio）不在白名单 → `POST /api/plugins/provider` 对它们 400。脚本家 provider 级停用走 `handle_script_change(action=provider)` → 包内 `disabled_providers`（见缺口 2 修复）。

---

## 八、归属核实（team 并发纪律，2026-11-16 lead 已核正）

- 我改的文件：`src/plugin_host/**`（除 `wasm/**`）+ `src/api/admin/script_plugins.rs`。
- **非我改动**：`src/plugin_host/manager.rs` 的 `process_for_pack` —— **integrator 在 T0 阶段加的**（非 plugin-wasm；lead 已核 git 现场并把该方法转告为 plugin-wasm 的调用面）。
- **非我文件**：`src/api/admin/account_sessions.rs` 的 `plugin_login_interaction`/`plugin_login_call` 是 **T2 plugin-api 写的**（不是 T4 account-pool；T4 只动了 `provider_is_plugin_provider` 与 `merge_provider_rows`）。§七 Q1 的转修对象是 plugin-api。
- `src/plugin_host/mod.rs` 的 `pub mod provider_settings;` 我加；`pub(crate) mod wasm;` 非我。
