# Agent2API v3.5.0-beta.7 → v3.5.0 wasm 宿主变更清单

- 上游仓库：`D:\GitHub\Agent2API`
- 基线：`v3.5.0-beta.7` = `d20e8501`；目标：`v3.5.0` = `cc5d8b9b`
- 上游代码根：`desktop-tauri/src-tauri/server/src/server/core/plugin_host/`
- 下游对照：`D:\GitHub\AIGX\src\plugin_host\`
- 审计方式：纯源码阅读 + `git diff`，未运行 cargo/rustc/clippy/fmt，未改动 AIGX 代码。
- 范围：wasm 子树 38 个变更文件 + plugin_host 外围 18 个变更文件 = **56**（全量，未抽样）
- `git diff --stat` 合计：`56 files changed, 1587 insertions(+), 389 deletions(-)`

---

## §0 统计

| 维度 | 数值 |
|---|---|
| 变更文件总数 | 56 |
| ├ wasm 子树 | 38（含 1 删除 `legacy_credentials.rs`、1 新增 `desktop_launch/proxy.rs`） |
| └ plugin_host 外围 | 18 |
| 新增能力 | 17 |
| 行为修正 | 34 |
| 重构无行为变化 | 5 |
| 纯注释 | 0 |

AIGX 对齐情况：

| 状态 | 数量 | 说明 |
|---|---|---|
| 已对齐 | 12 | 语义在 AIGX 已存在，无需动作 |
| 部分 | 1 | AIGX 有相近实现但缺本次关键语义（desktop_credentials/doubao 的 `iid`） |
| 未对齐 | 15 | AIGX 缺失或语义未跟上，需跟进 |
| 不适用 | 28 | AIGX 明确未做的能力域（checkin_call / doubao_turn / catpaw_turn / workbuddy_http / desktop_launch / conversation 等） |

优先级分布：**P0 = 4**（#51 #52 #53 #54，插件安装/更新闭环），**P1 = 7**（#15 #25 #38 #40 #43 #45 #46），**P2 = 5**（#1 #27 #42 #44 #50），其余 `N/A`。

> 备注：AIGX 的 `wasm/` 采用与上游不同的模块切分（`access.rs` / `egress.rs` / `native.rs` / `probe.rs` / `verification.rs` 等自有模块），因此"已对齐"按**语义**判定而非文件同构。

---

## §1 逐文件表

### 1.1 wasm 子树（38）

| # | 文件 | 性质 | 关键变化 | AIGX 状态 | P |
|---|---|---|---|---|---|
| 1 | `wasm/catalog.rs` | 行为修正 | `merge()` 新增 `toolEmulation` 浅合并（L82-86）：目标缺省/为 null 且源非 null 时补入，其余字段合并策略不变 | 未对齐（AIGX `wasm/catalog.rs` 无 `toolEmulation` 分支） | P2 |
| 2 | `wasm/context.rs` | 重构无变化 | 删除字段 `supplied_legacy_credentials`（L94）与 `Context::new` 对应初始化（L130），随 legacy 能力下线 | 已对齐（beta.8 已删同名字段） | N/A |
| 3 | `wasm/conversation.rs` | 重构无变化 | `mod ordinary_http` → `pub(super) mod ordinary_http`（L6），为跨模块调用开放可见性 | 不适用（AIGX 无 conversation 模块） | N/A |
| 4 | `wasm/conversation/ordinary_http.rs` | 新增能力 | 新增 `finish_http_submission()`（L18-32）：按 HTTP 状态判定"未提交可重放"还是"永久锁定重放"；新增 `confirm_stream_started()`（L34-38）；新增 `is_safe_http_rejection()`（L161-163）白名单 `400/401/402/403/404/409/413/422/429/501/503/507` | 不适用 | N/A |
| 5 | `wasm/conversation/source_ref.rs` | 新增能力 | `SourceRef` 新增 `confirm_unsubmitted_rejection()` / `lock_permanent_replay()`（L192-201），把重放闸门下沉到 source 粒度 | 不适用 | N/A |
| 6 | `wasm/conversation/workbuddy_http.rs` | 行为修正 | 先取 `preparation` 句柄（L151），请求完成后按 `is_safe_http_rejection` 分流（L155-162），再 `?` 上抛，避免错误路径漏放/漏锁 | 不适用 | N/A |
| 7 | `wasm/diagnostics.rs` | 行为修正 | 兜底统计名 "Wasm 插件计算用量" → "Wasm 插件沙盒资源统计"（L84）；删除 `CONVERSATION` 方法强制 `info` 级别的特判（L87 删除） | 不适用（AIGX 无 diagnostics.rs） | N/A |
| 8 | `wasm/doubao_turn.rs` | 行为修正 | 改用不可变借用取 client（L67-68）；`/chat/completion` 走 `finish_http_submission`，其余路径 `confirm_stream_started`（L69-75）；reader 注册前再确认一次并容忍并发清空（L77-80） | 不适用 | N/A |
| 9 | `wasm/host.rs` | 新增能力 | 握手装载时解析 `error_rules`（L91），返回元组扩为 4 元（L114/L117），并写入 `self.error_rules`（L132） | 已对齐（AIGX `wasm/host.rs` 已有 `error_rules` 字段与装载） | N/A |
| 10 | `wasm/manifest.rs` | 行为修正 | `CAPABILITIES` 删除 `credentials.legacy-import.v1`（L23）；依赖表删对应项（L118）；删除"旧环境凭据导入仅允许固定提供商"校验（L179-182） | 已对齐（AIGX `wasm/manifest.rs` L103/L657/L662 明确记录 beta.8 移除并断言） | N/A |
| 11 | `wasm/mod.rs` | 重构无变化 | 删除 `mod legacy_credentials;`（L37） | 已对齐 | N/A |
| 12 | `wasm/runtime.rs` | 行为修正 | 删除 `ACCOUNT_NORMALIZE` 上的 legacy 分支与 `legacy_credentials::prepare/selected` 调用（L372-383），仅保留 autoclaw 系 `supplied_tokens` 路径（L375-379） | 已对齐（AIGX `wasm/runtime.rs` 同口径） | N/A |
| 13 | `wasm/services.rs` | 行为修正 | 删除 `"credentials.legacy.take"` 方法臂（L113）；`http.open_account`/`open_body_with_policy` 改为先取 `opened` 再 `finish_http_submission` 后 `?`（L432-442）；`2xx-3xx` 状态处 `confirm_stream_started`（L486-488） | 已对齐（AIGX `wasm/services.rs` L62-63 注释保留名字但已不派发） | N/A |
| 14 | `wasm/legacy_credentials.rs` | 行为修正（删除） | 整文件删除（-76 行）：`selected` / `prepare` / `take` 三个旧环境变量读取入口全部下线，封闭 `credentials.legacy-import.v1` 通道 | 已对齐 | N/A |
| 15 | `wasm/tasks.rs` | 新增能力 | `snapshot()` 拆为 `build_snapshot(checkin_only, records)`（L105-143），`provider_label` 每 provider 惰性求值一次（L115-121）；新增 `checkin_snapshot_with(records)`（L154-158）；导出 `travel_state_with`（L7-12） | 未对齐（AIGX `wasm/tasks.rs` 无 `checkin_snapshot_with` / `travel_state_with` / 惰性 label） | P1 |
| 16 | `wasm/catpaw_turn/owner.rs` | 行为修正 | `network::short(Route::Round)` 结果分流（L144-157）：成功即 `lock_permanent_replay()`；失败按 `catpaw_turn_upstream_rejected` 判定"未提交可重试"还是"永久锁定"，避免重复烧 round | 不适用（AIGX 无 catpaw_turn） | N/A |
| 17 | `wasm/checkin_call/doubao_daily.rs` | 新增能力 | 新增 `SubmitRejection { code: Option<i64>, message: String }`（L16-21）；`DailyError` 增 `SubmitRejected` 变体（L47） | 不适用（AIGX 未做 checkin_call，明确不做） | N/A |
| 18 | `wasm/checkin_call/doubao_daily/claim.rs` | 行为修正 | `DailyRun` 增 `rollback_pending` / `rejection`（L17-18）；`submit_checkin` 返回 `SubmitOutcome` 后按 `Accepted/Rejected/ResponseRejected` 三路分流（L50-66）；回查仍为未签且有拒绝信息时 → `NotEntered(SubmitRejected)` + `rollback_pending = snapshot.pending_reserved()`（L93-102），撤回本轮暂存 Pending | 不适用 | N/A |
| 19 | `wasm/checkin_call/doubao_daily/credentials.rs` | 行为修正 | `CONTROL_VERSION` `2.34.12`→`2.36.11`；`META_KEYS`/cookie 映射/query 全线新增 `iid`（L17/L47/L50/L93/L117/L130）；`runtime` `local_web`→`web`、`runtime_version` `3.42.2`→`3.45.3`（L127）；`fp` 取值优先 `deviceId` 再 `fingerprint`（L134-136 顺序反转） | 不适用 | N/A |
| 20 | `wasm/checkin_call/doubao_daily/network.rs` | 新增能力 | 新增 `SubmitOutcome::{Accepted,Rejected}`（L12-16）、`rejection_code()`（L18-33）、`rejection_message()`（L35-60，含 160 字符/控制字符/`://`/凭据泄露过滤）；`submit_checkin` 返回类型改为 `SubmitOutcome`；状态判定从"非 2xx 即错"改为"重定向即错 + 4xx（除 408）判拒绝 + 5xx 判 `QueryUnknown`"（L99-137）；非 2xx 时容忍非 JSON 体（L112-117） | 不适用 | N/A |
| 21 | `wasm/checkin_call/doubao_daily/parse.rs` | 行为修正 | 缺 `data` / `ug_campaign_info` / `campaign_group_code` 由 `ResponseInvalid` 改判 `NoActivity`（L15-24）；删除 `BTreeSet` 去重与 `recognized` 标志（L3-4/L35-36/L42-43/L53）；同 `(scene,item,checked)` 三元组重复改为 `continue` 跳过而非直接判 `AmbiguousDaily`（L74-79）；`campaign_group(s)` 增加 null 过滤（L83-84） | 不适用 | N/A |
| 22 | `wasm/checkin_call/persistence.rs` | 行为修正 | `Collector`/`Settlement` 增 `rollback_pending` / `rejection`（L65-66/L77-78）；新增 `None if rollback_pending => bound.clear_pending(result)` 分支（L244）；消息链新增 `rolled_back` 抑制 pending 与拒绝文案（L296-315）；`daily_failure_message` 合并 `ResponseRejected | SubmitRejected`（L341） | 不适用 | N/A |
| 23 | `wasm/checkin_call/persistence/daily.rs` | 行为修正 | 采集时透传 `run.rollback_pending` / `run.rejection`（L92-93） | 不适用 | N/A |
| 24 | `wasm/checkin_call/result_snapshot.rs` | 新增能力 | 新增 `clear_pending()`（L98-105）与 `finalize_unknown(result, clear)`（L107-117），按 `clear` 走 `snapshot.clear_pending` 或 `snapshot.save_unknown` | 不适用 | N/A |
| 25 | `wasm/desktop_credentials/doubao.rs` | 行为修正 | `device_id` 单点提取改为 `[("device_id","deviceId"),("install_id","iid")]` 双键循环（L205-221），新增 `iid` 落盘；字符串/整数两种形态都保留 | **部分**（AIGX `wasm/desktop_credentials/doubao.rs` 仅取 `aha.device.device_id`，L232） | P1 |
| 26 | `wasm/desktop_credentials/refresh/cline.rs` | 行为修正 | `replacement()` 增 `fallback_refresh` 参数（L18）；refresh 缺失时回落到既有 token（L29-37）；`tokens()` 增 `allow_empty` 并把空串跳过而非报错（L69-90）；`expiration()` 容忍空串、秒级时间戳自动 ×1000（L122/L129） | 不适用（AIGX `desktop_credentials` 未含 `cline-free`） | N/A |
| 27 | `wasm/desktop_credentials/refresh/mod.rs` | 行为修正 | 续期分支的 provider 白名单 `autoclaw/autoclaw-intl` 扩为 `+ cline-free`（L104） | 未对齐 | P2 |
| 28 | `wasm/desktop_credentials/refresh/protocols.rs` | 行为修正 | `401/403` 直接返回 `refresh_token_expired`（"请在官方客户端重新登录"，L84-86）；`cline::replacement` 调用传入旧 refresh 作回落（L90） | 不适用 | N/A |
| 29 | `wasm/desktop_launch.rs` | 新增能力 | 新增 `pub(crate) mod proxy;`（L6） | 不适用（AIGX 未做 desktop_launch） | N/A |
| 30 | `wasm/desktop_launch/declaration.rs` | 新增能力 | `Client` 增 `#[serde(default)] proxy_launch: Option<ProxyLaunch>`（L28-29）；新增 `ProxyLaunch { kind }` 与 `ProxyLaunchKind::ChromiumEnv`（`deny_unknown_fields`，L33-42） | 不适用 | N/A |
| 31 | `wasm/desktop_launch/operations.rs` | 新增能力 | 新增 `PrepareSelection { provider, account, client, use_proxy: Option<bool> }`（L29-38）；`Operation` 增 `proxy: LaunchProxyState`（L63）；`prepare()` 改吃 `PrepareSelection`、校验代理可行性并产出 `proxy_state`（L152-173）；`check_proxy()` 二次核对（L85-95）；热切换在有代理时禁用（L167/L227）；`confirm()` 落地前再 `check_proxy()`（L236） | 不适用 | N/A |
| 32 | `wasm/desktop_launch/operations/recovery.rs` | 行为修正 | 两条重试路径各加 `runner.check_proxy()?`（L50/L69）；`windows::start` 传 `proxy.resolved`（L71）；成功文案按是否带代理区分（L80-83） | 不适用 | N/A |
| 33 | `wasm/desktop_launch/operations/worker.rs` | 行为修正 | 执行/提交前各 `check_proxy()`（L26/L86）；`not_applied`/`rolled_back` 阶段清理 `state.backup`（L130-132）；`windows::start` 传 `proxy.resolved`（L150）；带代理文案（L157） | 不适用 | N/A |
| 34 | `wasm/desktop_launch/proxy.rs` | 新增能力 | **整文件新增（200 行）**：`LaunchProxy{url,label,address}` / `LaunchProxyState`（`disabled()`, `to_json()`）/ `evaluate_raw()`（轮询代理、链式代理、带认证代理、SOCKS5、非法端口、非法 host 全部判"不可用"并给中文原因）/ `prepare_state(supported, raw, use_proxy)`（默认 `enabled=true`）/ `reverify()`（配置变化即要求重新确认）/ `safe_proxy_host()` 字符白名单 | 不适用 | N/A |
| 35 | `wasm/desktop_launch/recovery.rs` | 行为修正 | `Operation` 初始化补 `proxy: LaunchProxyState::disabled()`（L58） | 不适用 | N/A |
| 36 | `wasm/desktop_launch/windows.rs` | 行为修正 | 非 Windows 桩函数 `start()` 签名同步加 `Option<&LaunchProxy>`（L26） | 不适用 | N/A |
| 37 | `wasm/desktop_launch/windows/platform.rs` | 行为修正 | 进程身份核对改为 `Result<Option<..>>`（`open_query()` 把 `ERROR_INVALID_PARAMETER` 视作"进程已退出"而非错误，L331-344）；`process_exited()` 用 `WaitForSingleObject(…,0)`（L331）；`inspect_handle()`/`terminate()` 对已退出进程幂等返回 Ok（L346-392）；`close()` 增 `spawned_from_known()` 族系校验 + 按"有活跃父进程/非主程序"排序关闭（L413-460）；`start()` 增代理注入：清 8 个代理环境变量后写 `HTTP(S)/ALL_PROXY`+`NO_PROXY` 与 `--proxy-server=`（L469-489） | 不适用 | N/A |
| 38 | `wasm/tasks/travel.rs` | 行为修正 | `state()` 增 `has_capability(PROVIDER,"tasks.v1")` 门禁（L11）；新增 `state_with(&states)` 接受外部快照，避免重复 `read_all()`（L15-20） | 未对齐（AIGX `wasm/tasks.rs` 无 `state_with`） | P1 |

### 1.2 plugin_host 外围（18）

| # | 文件 | 性质 | 关键变化 | AIGX 状态 | P |
|---|---|---|---|---|---|
| 39 | `error_rules.rs` | 新增能力 | **整文件新增（203 行）**：从握手 JSON 解析 `error_classifications` / `error_rules`；上限 `MAX_RULES_PER_PROVIDER=32`、`MAX_CLASSIFICATIONS_PER_PROVIDER=64`；状态码裁到 `400..600`、`take(32/50)`；`canonical_id` 归一为 `plugin:{provider}:{local}`（≤128）；`fallback_seconds` clamp `1..86400`、`cap` 不低于 fallback；非 Auth 的 `RefreshThenSwitch` 降级为 `SwitchAccount`；`for_provider()` / `all_provider_rules()` 供上层读取 | 已对齐（AIGX `plugin_host/error_rules.rs` 同名函数齐全） | N/A |
| 40 | `install.rs` | 行为修正 | 新增 `InstalledPack::single_provider_id()` / `normalize_single_provider()`（L298-317）：单提供商包以包级 `enabled` 为唯一真相，"包启用+唯一提供商停用"读入即折为包停用并恢复子标记；`PackRegistry::load` 对每个 pack 归一（L349-355）；`set_pack_enabled`/`set_provider_enabled` 写回同口径（L608-612/L635-641） | 未对齐（AIGX `install.rs` 无 `single_provider_id` / `normalize_single_provider`） | P1 |
| 41 | `lifecycle.rs` | 行为修正 | 握手装载同时解析 `error_rules` 并写入 `Host.error_rules`（L213-228） | 已对齐（AIGX 在 `wasm/host.rs` 完成同等装载） | N/A |
| 42 | `mod.rs` | 新增能力 | `provider_label()` 改走 `pack_view::provider_label()`（L109-113，= 注册表缓存/清单，不扫图标）；`admin_panels()` / `checkin_features()` 拆出 `collect_*` 并新增 `checkin_provider_metadata()`（L238-272）一次性锁取 `admin_panels`/`checkin_features`/`checkin_by_provider`/`campaign_providers`；`Host` 增 `error_rules` 字段（L347/L433） | 未对齐（AIGX `mod.rs` 无 `checkin_provider_metadata` / `collect_admin_panels`） | P2 |
| 43 | `operations/management.rs` | 行为修正 | `PackChange::Enabled` 与 provider 变更均接入单提供商归一化写回（L49-56/L65-70） | 未对齐 | P1 |
| 44 | `pack_view.rs` | 新增能力 | 新增 `provider_label(id)`（L196-227）：先查 `availability::is_suspended` 短路，再走 `PackRegistry::load()` + `manifest_providers`，缺清单时才读盘；避免逐次扫描图标与宿主状态 | 未对齐（AIGX 无 `pack_view.rs`，label 逻辑在 `mod.rs`） | P2 |
| 45 | `packages.rs` | 行为修正 | `PreparedPack` 构造后即 `normalize_single_provider()`（L143-155） | 未对齐 | P1 |
| 46 | `rpc.rs` | 新增能力 | `RpcAdapter` 增 `web_search: Option<WebSearchCapability>`（L175/L212）；加载时经 `ADMIN_ACTION{action:"web-search.capabilities"}` 探测并缓存（L271-284）；实现 `ProviderAdapter::web_search_capability()`（L629-634）；会话错误码透传：`failure.code` 为空时从 `code`/`providerCode`/`error.code` 补取（≤256 字符，L2231-2240） | 未对齐（AIGX `rpc.rs` 无 `web_search_capability`） | P1 |
| 47 | `script/catalog.rs` | 重构无变化 | 删除未被引用的 `github_commit()` 包装（L15-18），调用方改用 `github_commit_for()` | 已对齐 | N/A |
| 48 | `script/host/login-policy.mjs` | 行为修正 | 新增 `localPage` 计算（loopback + 显式端口 + 无凭据，L312-313），并把 `host:port` 追加进 `allowedHosts`（L340-341/L351） | 已对齐（AIGX 以自有 `loopbackTargets` 机制覆盖同一需求，L247/L276） | N/A |
| 49 | `script/mod.rs` | 重构无变化 | 导出 `refresh_model_catalog_if_active`（L39） | 已对齐 | N/A |
| 50 | `model_catalog.rs` | 行为修正 | 硬编码 `models.dev/api.json` 改为官方 `CDN_MODELS_CATALOG_URL` 主 + `API_MODELS_CATALOG_URL` 备（L9-13）；新增 `refresh_if_active(force)`：仅宿主就绪或已有本地缓存才刷新（L30-38）；新增 `models_dev_enabled()` 总开关（L40-42）；`force` 时忽略 retry 退避（L49）；下载加 `Accept: application/json` 与 60s 超时（L69-84） | 未对齐（AIGX `model_catalog.rs` 无 `CDN_MODELS_CATALOG_URL` / `refresh_if_active`） | P2 |
| 51 | `script/operations.rs` | 新增能力 | 新增 `allow_catalog_fallback()`（L59-74）：仅"无预期 id 且来源是 GitHub 且该仓库此前未安装"才允许回退目录缓存；安装结果改为带 `installed_latest` 的元组并交给 `updates::record_installed`（L108-151）；`enable`/`provider` 变更接入单提供商归一化（L174-190）；卸载成功后 `updates::remove_entry`（L202-218）；`finished()` 不再无条件 `updates::wake(true)`（L50-53 删除）；补 3 条单元测试（L325-365） | 未对齐 | **P0** |
| 52 | `script/source.rs` | 行为修正 | `download()` 签名加 `allow_catalog_fallback` 并返回 `(Package, bool)`（L83-99）；新增 `parse_github_commit_sha()` / 公开的 `fetch_github_commit()`（L101-130）；新增 `github_commit_fallback()`（L130-141）与 `resolve_github_commit(..., allow)`（L143-157）：**更新/重装绝不回退到旧目录缓存 commit**，否则报错；补 2 条测试（L178-205） | 未对齐（AIGX `script/source.rs` 无 `allow_catalog_fallback` / `fetch_github_commit`） | **P0** |
| 53 | `script/storage.rs` | 行为修正 | `Package` 增 `single_provider_id()` / `normalize_single_provider()`（L54-70）；`read()` 对每个 package 归一（L84-90） | 未对齐 | **P0** |
| 54 | `script/updates.rs` | 新增能力 | 抽出 `save_cache()` / `installed_latest_label()` / `PackageFingerprint` / `package_fingerprint()`（L81-113）；新增 `record_installed()`（安装/更新后立即把该条写为最新，含 8s 超时兜底，L128-148）与 `remove_entry()`（卸载后清缓存，L150-156）；抽出 `check_entry()`（L172-203）；`refresh_inner()` 记录初始 fingerprint，回写前比对磁盘注册表，**指纹变化则改用磁盘缓存而非旧快照覆盖**（L230-233/L252-273）；`github_entry()` 改用 `source::fetch_github_commit`（L309-315）；删除 `JSON_LIMIT`；补 1 条 tokio 测试（L371+） | 未对齐（AIGX 无 `script/updates.rs`） | **P0** |
| 55 | `stream/workbuddy.rs` | 行为修正 | 未记录冷却的通知按 `decision.deprioritizes()` 分流文案（"降级排序；响应已开始未重放" vs "不记录冷却；响应未重放"，L97-104） | 不适用 | N/A |
| 56 | `strict_errors.rs` | 行为修正 | 业务码透传不再受 `numeric_known_code` 门禁约束，上游码一律透传（L202-203）；`QuotaLimited` 的 `429` 判定不再限定 `copilot-subscription`，改为排除"上下文超长"（L309-310） | 不适用（AIGX 无 `strict_errors.rs`） | N/A |
---

## §2 需跟进清单

> 落点均为 `D:\GitHub\AIGX\src\plugin_host\` 下相对路径；行号为上游 v3.5.0 行号。

### P0（4 条，插件安装/更新闭环的静默故障）

**1. `script/source.rs` — 更新/重装回退到旧目录缓存 commit（#52）**

- 上游语义：`allow_catalog_fallback` 仅对"无预期 id + GitHub 来源 + 该仓库此前未安装"为真；命中则回退 `catalog_github_commit_for()` 并标记 `resolved_live=false`；更新与重装路径一律不允许回退，直接抛原始网络错误（L130-141/L143-157）。
- 失败模式：AIGX 现有的 `script/source.rs` 若无此门禁，网络抖动时会用目录缓存的旧 commit 静默重装同一版本，插件永远显示"可更新"但点更新无效。
- 落点建议：在 `script/source.rs` 的 GitHub 解析链上引入同签名 `allow_catalog_fallback: bool`，由调用方传入；`catalog_github_commit_for` 的调用点收敛到唯一的 `github_commit_fallback()`；同时把 `parse_github_commit_sha()` 抽成纯函数便于单测（上游有 2 条）。

**2. `script/storage.rs` — 单提供商包"包启用 + 唯一提供商停用"状态不自洽（#53）**

- 上游语义：`Package::single_provider_id()` 判定 `providers` 恰为 1 个；`normalize_single_provider()` 在 `read()` 时把 `disabled_providers` 命中唯一 provider 的情况折为 `enabled=false` 并从列表移除该项（L54-70/L84-90）。
- 失败模式：不做归一，用户关掉唯一子开关后包仍算启用，下次打开包开关会被隐藏子开关卡住，表现为"开关打开但插件没跑"。
- 落点建议：AIGX `script/storage.rs` 增加两个同名方法并在 `read()` 反序列化后遍历归一；`Package` 结构新增字段时保持 `serde(default)` 兼容旧登记文件。

**3. `script/updates.rs` — 更新状态缓存无安装/卸载即时同步，且旧快照覆盖新结果（#54）**

- 上游语义：① `record_installed()` 在安装/更新成功后立即把该条写成 `available=false`（`installed_latest=true` 时直接用 commit 前 12 位，否则 8s 超时兜底），② `remove_entry()` 卸载后清条目，③ `refresh_inner()` 记 `initial_fingerprints`，回写前比对磁盘注册表，指纹变化则改用磁盘缓存而非旧快照覆盖（L128-156/L230-273）。
- 失败模式：AIGX 缺 `script/updates.rs` 的这层，会出现"刚更新完界面仍提示可更新"以及"检查期间安装/卸载被旧快照覆盖回去"两类回归。
- 落点建议：在 AIGX 脚本插件更新模块补 `record_installed` / `remove_entry` 两个入口 + `PackageFingerprint` 比对；`save_cache()` 统一出口，避免多处直接 `atomic_write`。

**4. `script/operations.rs` — 安装结果未回写更新状态、卸载未清缓存（#51）**

- 上游语义：`allow_catalog_fallback()` 决定是否允许目录回退（L59-74）；`install()` 返回 `(Package, installed_latest)` 并调 `updates::record_installed`（L108-151）；`change()` 在 `remove` 成功后调 `updates::remove_entry`（L202-218）；`finished()` 不再无条件触发全量 `updates::wake(true)`（L50-53 删除）。
- 失败模式：只有 #54 的缓存能力、没有 #51 的接线，#54 永远不被调用，等于没修。
- 落点建议：#54 落地后，在 AIGX `script/operations.rs` 的安装/卸载成功分支接入两个入口，并删掉 `finished()` 里的无条件 `wake(true)`（改由状态表精确更新）。

### P1（7 条）

**5. `install.rs` + `operations/management.rs` + `packages.rs` — 单提供商包归一化（#40 #43 #45）**

- 上游在四处同口径落地：`InstalledPack::single_provider_id/normalize_single_provider`（install.rs L298-317）、`PackRegistry::load` 遍历归一（L349-355）、`set_pack_enabled`/`set_provider_enabled` 写回（L608-641）、`operations/management.rs` 的 `PackChange` 分支（L49-70）、`packages.rs` 构造后归一（L143-155）。
- AIGX 落点：`install.rs`（包注册表）+ `operations.rs`（等价于上游 `operations/management.rs`）两处必须同改，否则读入归一了、写回又写脏。
- 理由：只改读侧会造成"改完开关下次启动被折回"，开关行为反复。

**6. `rpc.rs` — 提供商 web-search 能力未探测（#46）**

- 上游语义：握手后发 `ADMIN_ACTION{action:"web-search.capabilities"}` 拿 `WebSearchCapability` 并实现 `ProviderAdapter::web_search_capability()`（L271-284/L629-634）。
- AIGX 落点：`rpc.rs` 的 adapter 元数据初始化 + `providers/adapter.rs` 的 trait 实现。
- 理由：这是新增能力缺失（不是回归），但直接影响联网搜索类插件在 AIGX 的可用性探测。

**7. `wasm/tasks.rs` — 任务快照重复读盘与 provider label 重复求值（#15 #38）**

- 上游语义：`build_snapshot(checkin_only, records)` 接受外部 `records` 快照，`provider_label` 每 provider 惰性求值一次；`checkin_snapshot_with(records)` 与 `travel_state_with(&states)` 供上层一次性构建多份视图（tasks.rs L105-158、tasks/travel.rs L11-20）；`travel::state()` 增 `has_capability("tasks.v1")` 门禁。
- AIGX 落点：`wasm/tasks.rs`（AIGX 把 travel 合并在单文件里，需自建等价 `state_with`）。
- 理由：签到中心单次构建会多次 `read_all()`，属可观测的性能与一致性缺陷；能力门禁缺失会让未申报 `tasks.v1` 的插件读到旅行任务。

**8. `wasm/desktop_credentials/doubao.rs` — 缺 `iid`（install_id）提取（#25）**

- 上游语义：`device_id` 单点提取改为 `[("device_id","deviceId"),("install_id","iid")]` 双键循环，字符串/整数两形态都落盘（L205-221）。
- AIGX 落点：AIGX `wasm/desktop_credentials/doubao.rs` L232 目前只读 `aha.device.device_id`。
- 理由：`iid` 是豆包新版本签到的必需参数，缺失会导致 AIGX 侧豆包凭据在新版客户端上不被接受（AIGX 已有 17 家 desktop_credentials，此项是其中一家的字段补齐，非新域）。

### P2（5 条）

**9. `wasm/catalog.rs` — `toolEmulation` 未参与清单合并（#1）**：上游 `merge()` 增加"目标缺省或 null 时由源补齐"的浅合并（L82-86）。AIGX 落点 `wasm/catalog.rs` 的 `merge()`。理由：仅在多来源清单合并时丢失字段，影响面小。

**10. `wasm/desktop_credentials` 续期 provider 白名单 — 缺 `cline-free`（#27）**：上游把续期分支白名单从 `autoclaw/autoclaw-intl` 扩到含 `cline-free`（L104），并配套 `refresh/cline.rs` 的 fallback refresh + 401/403 → `refresh_token_expired`（#26 #28）。AIGX 落点：若后续接入 cline-free 需一并补 `desktop_credentials` 的 provider 分支。理由：AIGX 当前未接 cline-free，纯前瞻项。

**11. `mod.rs` — 缺 `checkin_provider_metadata()` 一次性快照（#42）**：上游把 `admin_panels`/`checkin_features` 拆成 `collect_*` 并新增一次性锁取的 `CheckinProviderMetadata`（L238-272）。AIGX 落点 `mod.rs`。理由：纯性能/锁竞争优化，无行为差异。

**12. `pack_view.rs` — `provider_label` 未走注册表缓存（#44）**：上游新增 `pack_view::provider_label()`，先短路 `availability::is_suspended` 再查注册表缓存/清单，避免逐次扫描图标（L196-227）。AIGX 无 `pack_view.rs`，label 逻辑在 `mod.rs`。理由：性能项，标签结果一致。

**13. `model_catalog.rs` — 模型目录仍硬编码 models.dev（#50）**：上游改官方 CDN 主 + API 备、加 `models_dev_enabled()` 开关、`refresh_if_active()` 只在宿主就绪或已有缓存时刷新、`force` 时忽略退避、下载加 `Accept` 与 60s 超时（L9-84）。AIGX 落点 `model_catalog.rs`。理由：可用性与可配置性改进，非阻断。

---

## §3 不适用清单与理由

以下 28 项在 AIGX 侧无对应能力域，本次审计**不建议跟进**；若未来启用对应域，应按 §1 行号回查。

| 领域 | 涉及文件（#号） | 理由 |
|---|---|---|
| `credentials.legacy-import.v1` 全链下线 | #2 context.rs、#10 manifest.rs、#11 mod.rs、#12 runtime.rs、#13 services.rs、#14 legacy_credentials.rs（删除） | AIGX 已在 beta.8 同步删除：`wasm/manifest.rs` L103/L657/L662 记录移除并断言 `!CAPABILITIES.contains("credentials.legacy-import.v1")`，`wasm/services.rs` L62-63 保留名字但不再派发。**已对齐，无需动作。** |
| conversation 重放闸门 | #3 conversation.rs、#4 ordinary_http.rs、#5 source_ref.rs、#6 workbuddy_http.rs | AIGX 未实现 `conversation` 模块（无 preparation/replay 状态机），不在已声明能力域内。 |
| `doubao_turn` / `catpaw_turn` | #8 doubao_turn.rs、#16 catpaw_turn/owner.rs | AIGX 明确未做 `http.doubao-turn.v1` / `http.catpaw-turn.v1` 的服务端实现（`wasm/services.rs` 无对应方法臂）。能力字符串虽在 `CAPABILITIES` 中列出，但无消费方。 |
| `checkin_call` 豆包签到 | #17-#24（doubao_daily 及其 claim/credentials/network/parse、persistence、result_snapshot） | AIGX 明确未做 `checkin_call`。本次上游改动（`SubmitOutcome` 拒绝分类、`iid`、Pending 回滚）全部绑定该域内部状态机。 |
| `desktop_launch` 本机启动 | #29-#37（desktop_launch.rs、proxy.rs、declaration、operations、recovery、windows、platform） | AIGX 明确未做 `desktop_launch`，`desktop.launch.v1` 仅在清单中声明，无执行侧。新增的 `proxy.rs` 与 Windows 进程族系核对均无落点。 |
| `stream/workbuddy.rs` | #55 | AIGX 无 workbuddy 流式重放路径。 |
| `strict_errors.rs` | #56 | AIGX 的严格错误处理走自有 `wasm/errors.rs` + `plugin_host/error_rules.rs`，无同名模块；本次两处改动（业务码无门禁透传、`QuotaLimited` 判定放宽）需在 AIGX 对应实现处单独评估，不随本清单跟进。 |
| `diagnostics.rs` | #7 | AIGX 无 wasm diagnostics 模块。 |
| 已对齐的等价实现 | #9 host.rs、#39 error_rules.rs、#41 lifecycle.rs、#47 script/catalog.rs、#48 login-policy.mjs、#49 script/mod.rs | 语义已在 AIGX 存在（`error_rules.rs` 同名函数齐全、`wasm/host.rs` 已持 `error_rules` 字段、`login-policy.mjs` 以自有 `loopbackTargets` 覆盖同一需求）。**无需动作。** |

---

## 附：审计口径说明

1. 行号取自 `git -C D:\GitHub\Agent2API diff v3.5.0-beta.7..v3.5.0` 的 hunk 头（新文件侧行号）。
2. "性质"判定标准：新增文件/新增 capability 名/新增方法臂 → 新增能力；已有路径的分支、参数、上限、文案、错误分类变化 → 行为修正；仅可见性、提取函数、签名包一层而同语义 → 重构无变化。
3. "已对齐"按**语义**判定，不要求文件同构：AIGX `wasm/` 的模块切分（`access.rs` / `egress.rs` / `native.rs` / `probe.rs` / `verification.rs`）与上游不同。
4. 本次审计未运行 cargo/rustc/clippy/fmt，所有结论均来自源码与 diff 阅读，未做编译期验证。