# 账号池插件能力对齐：AIGX × Agent2API v3.5.0-beta.7（WASM 宿主面）

> 审计日期：2026-11-23
> 上游：`D:\GitHub\Agent2API` tag `v3.5.0-beta.7`（`HEAD` 与 tag 同一提交 `d20e8501`）
> AIGX：`D:\GitHub\AIGX`（本次改动前 `HEAD`=847911ba / v1.0.91 工作区）
> 范围：**账号池相关的 wasm 插件宿主能力**（`core/plugin_host/wasm/**`），不含前端与数据面协议转换
> 度量口径：`wasm/*.rs` 模块数与行数、`wasm/services.rs` 的能力分派臂、`wasm/manifest.rs` 的能力白名单

## 0. 结论

| 指标 | 上游 beta.7 | AIGX 改动前 | AIGX 现在 |
|---|---:|---:|---:|
| `plugin_host/wasm` 文件数 | 218 | 26 | 39 |
| `plugin_host/wasm` 行数 | 39 149 | 9 164 | 12 402 |
| `services.rs` 能力分派臂 | 38 | 22 | 25 |
| 已服务能力（`SERVED_CAPABILITIES`） | 见上游表 | 27 项 | 32 项（+`desktop.credentials.v1`、+`runtime.platform.v1`、+`runtime.architecture.v1`、+`request.context.v1`、+`credential.kiro-external-refresh.v1`、+`resources.fetch-public.v1`，−`checkin.rolling.v1` 虚报；`verification.claim.v1` 复核后保留——它闸的租约 AIGX 真服务，三个动作由 `servedActions` 单独标 false） |

**本轮落地（账号池主线）**：`desktop.credentials.read` —— 本机客户端登录态读取，
**17 家 provider** 的固定来源读取器（含 `autoclaw`/`autoclaw-intl`/`doubaowork` 的
DPAPI·safeStorage 解密链）+ 能力/声明/归属三道门 + 登录导入作用域；另补 `runtime.platform` /
`runtime.architecture` 两个通用能力。这是「从桌面客户端导入账号」在插件形态下唯一的宿主入口；
此前插件声明 `desktop.credentials.v1` 能装上，但一调用就 `NO_METHOD`，本机导入整条链是死的。

**剩余缺口**（全部列出，不装的项也写明理由）：见 §3。

## 1. 方法级差距（`services.rs` 分派臂）

| 方法 | 上游 | AIGX 改动前 | 本轮 | 账号池相关性 | 落点 |
|---|---|---|---|---|---|
| `desktop.credentials.read` | ✅ | ❌ `NO_METHOD` | ✅ | **高**：桌面登录态导入 | `wasm/desktop_credentials/**`（新增） |
| `credentials.legacy.take` | ✅ | ❌ | ❌ R2 | 高：老环境凭据迁移导入 | 需 `supplied_legacy_credentials` 供给链 |
| `login.desktop.credentials.take` | ✅ | ❌ | ❌ R2 | 高：登录流内取本机凭据（chatglm） | 需 chatglm 桌面库读取（`native/chatglm/**`） |
| `credential.kiro.external.refresh` | ✅ | ❌ | ✅ | 高：Kiro 外部 IdP 续期 | `external_refresh.rs`：复注入 egress 换证 + 账号级 CAS 占位防一次性 refresh_token 重烧 + 换证前后凭据指纹比对 |
| `managed_task.read` | ✅ | ❌ | ❌ R2 | 中：受控任务读取回执 | `managed_task/**`（AIGX 已有 begin/end/status/abort） |
| `catpaw.turn.read` | ✅ | ❌ | ❌ R3 | 中：CatPaw 固定对话 | `catpaw_turn/**`（24 文件） |
| `doubao.image.upload` | ✅ | ❌ | ❌ R3 | 中：豆包图片上传 | `doubao_upload.rs` |
| `resource.fetch_public` | ✅ | ❌ | ✅ | 中：插件侧公网图片下载 | `resource_fetch.rs`：注入 egress + 不跟跳转 + 响应头与魔数双重校验 + 只产本次调用封存资源 |
| `request.context` | ✅ | ❌ | ✅ | 中：请求上下文（客户端原本是否要流式） | `services.rs` 臂 + `Context.client_stream`；数据面 `bridge/plugin.rs` 传 `req.stream`，非对话调用如实 `forbidden` |
| `runtime.platform` / `linux`/`macos`/`windows` 取值 | ✅ | ❌ | ✅ | 低但通用：插件按平台分支 | `services.rs` 分派臂，回 `win`/`mac`/`linux`/`other` |
| `runtime.architecture` | ✅ | ❌ | ✅ | 低但通用：CPU 架构 | `services.rs` 分派臂，回 `std::env::consts::ARCH` |
| `desktop.launch.context` / `desktop.launch.read` | ✅ | ❌ | ❌ 不做 | 无：桌面客户端启动/恢复 | 见 §3「不适用」 |
| `crypto.monkeycode.cap.solve` | ✅ | ❌ | ❌ R3 | 低：MonkeyCode 计算挑战 | `monkeycode_cap.rs`（109 行，需 checkin 观测上下文） |

## 2. 本轮实现（`desktop.credentials.read`）

### 2.1 文件

| 文件 | 行数 | 内容 |
|---|---:|---|
| `src/plugin_host/wasm/desktop_credentials.rs` | 424 | 入口、能力/声明/归属闸门、导入作用域、固定路径与只读工具（symlink/reparse 拒收、大小上限、只读 SQLite）、`catpaw` 明文读取、provider 分发 |
| `desktop_credentials/snapshots.rs` | 375 | workbuddy / workbuddy-intl（`CodeBuddyExtension` `.info` 降序取最新）、orcaterm、cline、quwork（SQLite → `.credentials.yaml` 回落） |
| `desktop_credentials/kiro.rs` | 277 | kiro-cli `data.sqlite3`（social/odic/external-idp 三种登录态）与 kiro-ide `kiro-auth-token.json` |
| `desktop_credentials/raccoon.rs` | 118 | 小浣熊/office-desktop 多布局探测 + 7 组字段别名 |
| `desktop_credentials/kuku.rs` | 140 | 百度 Cookies SQLite 固定查询 + 域名/值校验（不调系统解密） |
| `desktop_credentials/dumate.rs` | 284 | 千帆桌面 `auth.json` + `.cookie-key`（32B）AES-256-GCM 解封 + 企业域拒绝 |
| `desktop_credentials/astudio.rs` | 224 | `ASTUDIO_HOME`/注册表 `AStudioStorage`/`~/.acode` 三来源 + `config.toml` 回填 uid |
| `desktop_credentials/codar.rs` | 83 | `~/.codar/config.json` + speedai JWT 取用户名 |
| `desktop_credentials/commandcode.rs` | 45 | `~/.commandcode/auth.json` 的 `user_` 前缀 apiKey |
| `desktop_credentials/loomy.rs` | 728 | Chromium Local Storage（严格 LevelDB：CRC32C/压缩块/日志批次/墓碑）或 `auth-session.json` |
| `desktop_credentials/autoclaw.rs` + `autoclaw/crypto.rs` | 167 + 185 | safeStorage：`auth.json` / `openclaw.json` 双来源，DPAPI 解 key → AES-256-GCM 解密文，JWT `exp` 映射到期 |
| `desktop_credentials/doubao.rs` | 672 | 豆包工作版：`Local State` 主密钥（DPAPI）→ Cookie 库快照（`-wal/-shm/-journal`）→ `sessionid` 判账号，Windows-only，多账号报 `desktop_credentials_ambiguous` |

### 2.2 闸门与作用域

1. 能力：`desktop.credentials.v1`（本轮已加入 `SERVED_CAPABILITIES`，此前只在可声明全集里）。
2. 声明：`wasm.json.helpers` 必须含该 provider 的固定 helper id（`helper()` 表 17 项）。
3. 归属：有账号 → 账号必须属于当前 provider（`Context::account()`）；
   无账号 → 必须处于 `desktop_credentials::import_scope` 作用域内。
4. 作用域只在**一次插件登录 RPC** 内有效：`src/api/admin/account_sessions.rs::plugin_login_call`
   用 `import_scope(provider, true, …)` 包住 `login.start/poll/callback/submit/cancel`。
   离开即失效，不是常开开关。

### 2.3 与上游的差异（如实声明）

| 项 | 上游 | AIGX | 影响 |
|---|---|---|---|
| 账号记录判据 | `record.desktop == true` | 导入作用域 + provider 归属 | 语义等价；AIGX 插件账号无 `desktop` 字段 |
| `autoclaw` / `autoclaw-intl` | safeStorage（DPAPI + AES-GCM） | ✅ 已移植：`extern "system"` 直连 `crypt32!CryptUnprotectData`，`aes-gcm 0.10` 解密文 | 一致；非 Windows 如实 `desktop_credentials_unavailable` |
| `doubaowork` | Cookie 库 + DPAPI | ✅ 已移植（Windows-only）：`Local State` 主密钥 → Cookie 快照；多账号报 `desktop_credentials_ambiguous` | 一致 |
| `cookie_file`（kuku 加密单元） | `resources.cookie-file.v1` 单独能力 | 未接（kuku 只取明文 `value`，密文行专用报错） | 已在上游口径内：不调系统解密 |
| `refresh/*` | 读 → 续期 → 回写（CAS） | **决策不移植**（见 §3-2：原生续期已覆盖，双写会烧 token） | 若要回写客户端文件，应在原生续期成功后追加受控回写 |
| 依赖 | `zeroize` / `bytes` / `windows` crate | 无 zeroize（生命周期内短持有）、AStudio 注册表 / DPAPI 全走 `extern "system"` FFI | 无行为差异 |

### 2.4 能力表一致性（本轮新发现）

按「已服务表 = 真能服务」逐项静态核对 `SERVED_CAPABILITIES`：

| 发现 | 处理 |
|---|---|
| `checkin.rolling.v1` 在已服务表里，但 wasm 宿主**没有任何 checkin 执行入口**（AIGX 签到在原生 `src/checkin/*`；`checkin.claim` 只出现在原生/脚本插件 RPC 方法表 `plugin_host/rpc.rs`） | **从已服务表移除**，`CAPABILITIES` 保留前向兼容；注释写明理由 |
| `verification.claim.v1` 在已服务表里，但它闸的三个动作（`captcha.push`、`claim.verification.poll|complete`）只在**验证执行上下文**内可达，AIGX 只落地了租约（`verification.lease.*`），没有验证执行器 | **从已服务表移除**，声明放行保留 |
| 未服务但被插件调用时只回 `NO_METHOD`「未支持的主程序能力」，分不清「名字写错」还是「这家没做」 | 新增 `UNSERVED_METHODS` 表（21 条）：命中即回 `unsupported` + 指名能力，例：`网关未实现该方法（能力 login.managed.v1 未服务）` |
| `errors.v1`、`settings.schema.v1` 无 `permission()` 调用 | 合法：前者是错误形状、后者在 `init` 握手里返回 schema，都不是方法闸门 |

## 3. 剩余缺口与分组

### R2（账号池导入/续期闭环，优先）

1. ~~`autoclaw` / `autoclaw-intl` / `doubaowork` 的 DPAPI/safeStorage 解密~~ ✅ 本轮完成。
2. `desktop_credentials/refresh/**`（`desktop.refresh.{kiro,autoclaw,autoclaw-intl,cline-free,raccoon}.v1`）
   —— **决策：不移植，避免双写**。AIGX 的凭据续期在原生 `account_session/refresh.rs`
   （`supports_refresh` 已覆盖 autoclaw/autoclaw-intl/cline/cline-free/cline-pass，单次 CAS 落盘、
   失败绝不改写凭据），插件侧 refresh helper 会把**同一份凭据**再换一次并把结果写回客户端文件，
   两条续期链同时存在必然互相烧 token（上游 CodeArts 一次性 refresh_token 的教训）。
   若要补「续期后回写客户端文件」，应在原生续期成功后追加一次受控回写，而不是再开一条插件链。
3. `credentials.legacy.take` + `legacy_secret`：需在账号新增/登录入口登记 `supplied_legacy_credentials`
   （AIGX 目前没有「按用户选择的环境变量导入」入口，单独做等于半死功能，需连 UI 一起）。
4. `login.desktop.credentials.take`（chatglm 桌面库）+ `native/chatglm/**`。
5. ~~`credential.kiro.external.refresh`~~ ✅ 本轮完成（用注入 egress 替代上游 `FrozenHttpClient`）。
6. ~~`resource.fetch_public`~~ ✅ 本轮完成（注入 egress + 不跟跳转 + 双重编码校验）。

### R3（provider 专属流）

`checkin_call/**`（豆包每日签到 + 结果快照）、`doubao_rewards`、`doubao_upload`、`doubao_turn`、
`catpaw_turn/**`、`conversation/**`（含 workbuddy_http）、`compilation`、`progress`、`query_queue`、
`monkeycode_cap`、`deepseek_pow`（AIGX 已有原生 `crypto.deepseek.pow.solve`，无需插件侧实现）。

### 不适用（产品形态差异，不移植）

- `desktop.launch/**`：AIGX 是服务端网关，无桌面客户端可启动/恢复。
- `aws_oidc`、`qoder_umid`、`client_device`、`quwork_device`：桌面端设备指纹/UMID 采集，网关形态无消费方。
- `inbound_images` / `inline_images` / `image_compat` 的宿主侧：AIGX 图片链在 `bridge`/`image_compat`，
  不经 wasm 宿主（按数据面现状判定）。

## 4. 漂移检查工具（本轮新增）

`tools/account-pool-plugin-parity.mjs` —— 每次上游发版可重跑，不必再人工数文件：

```bash
node tools/account-pool-plugin-parity.mjs                    # 人类可读漂移报告
node tools/account-pool-plugin-parity.mjs --json             # 机器可读
node tools/account-pool-plugin-parity.mjs --check            # 只对「基线之外的新漂移」退出 1
node tools/account-pool-plugin-parity.mjs --write-baseline   # 人工确认后更新基线
```

它静态解析两侧的 `wasm/services.rs` 分派臂、`manifest.rs` 的 `CAPABILITIES` /
`SERVED_CAPABILITIES`、`wasm/**` 模块集合，并检查「已服务但代码里完全没出现的能力」
（`checkin.rolling.v1` 这类虚报就是这条抓出来的）。存量缺口写在
`tools/account-pool-plugin-parity.baseline.json`，`--check` 不因老缺口报红。

本轮实测输出（上游 beta.7 vs AIGX，方法名同时统计 `match` 臂 / `pub const` 方法表 /
`method ==` 与 `matches!` 分派）：wasm 文件 218→39、行 42 162→14 604、方法名
**111 → 59**、可声明能力 55→55（齐）、**上游有而 AIGX 无的方法 58 个**、AIGX 独有 6 个
（`account.normalize`、`account.view`、`native.helpers`、`managed_task.begin/end/abort`）、
「已服务但代码里完全没出现」0。

58 个缺口按归属分组（据 `--json` 输出）：

| 组 | 方法 | 处置 |
|---|---|---|
| 登录协议（12） | `login.context`、`login.input.take`、`login.step.publish`、`login.loopback.open/close`、`login.loopback.kiro.advance`、`login.kiro.exchange`、`login.trae.context.prepare`、`login.trae.exchange`、`login.autoclaw.authorization`、`login.desktop.credentials.take`、`login.context` 家族 | **不补实现，改标注**（见 §2.4 更正）：这些名字在 `contract().managedLogin.methods` 里只是 ABI 描述，同处 `served` 字段已如实为 `false`；宿主侧现在还会回 `unsupported`+指名能力，插件不会误以为是拼写错误。真要做托管登录得先有 `login.managed.v1` 执行器（R3） |
| managed_task（4） | `managed_task.create/observe/read/close` | **不移植**：上游 `managed_task` 模块头注自述「服务实现未接准入 ABI，真实审批入口与任务 ABI 仍关闭」，追名字等于给关闭的 ABI 造壳；AIGX 的 `begin/end/abort/status` 是自有闸门，保留 |
| CodeArts（6） | `codearts.login.*`、`codearts.session.*` | R3（上游在 e4d8e3f7 把 CodeArts 续期迁回插件） |
| 对话/传输（8） | `catpaw.turn.*`（5）、`doubao.turn.receipt/finish`、`doubao.image.upload` | R3 provider 专属流 |
| 协议桥（4） | `protocol.request/open/push/close` | R3（AIGX 协议转换在 `bridge`，不经宿主） |
| 校验/任务/用量（9） | `claim.status`、`claim.verification.*`、`checkin.claim`、`claim.doubao.tasks`、`tasks.list`、`usage.query`、`models.list`、`models.refresh` | 逐项核对：AIGX 多为**原生**路径实现（不宿主化），需一案一议 |
| 其它（15） | `admin.action`、`captcha.push`、`credential.kiro.external.refresh`、`credentials.legacy.take`、`crypto.monkeycode.cap.solve`、`desktop.launch.*`、`request.context`、`resource.cookie-file.take`、`resource.fetch_public`、`runtime.client_fingerprint`、`token.*` | R2/R3 或不适用（见 §3） |

## 5. 验收

| 项 | 结果 | 口径 |
|---|---|---|
| `cargo check`（改动中段，含 12 个读取器 + 闸门 + 分派） | ✅ `Finished dev profile`，零 error | 本机增量检查（此后按全局约定停止本机编译；后续 `runtime.*` 两臂与 autoclaw/doubao 三文件未本机编译） |
| `cargo fmt` | ✅ 已应用、`--check` 清洁 | 本机 |
| `cargo clippy --all-targets -- -D warnings` | ⏳ 待远端/CI | 本机仅跑到「5 条 lint 全在 `loomy.rs`」，已逐条修掉（doc list 缩进、`is_multiple_of`、两处 `chunks_exact→as_chunks`、`collapsible_if`），修复后**未再本机编译** |
| `SERVED_CAPABILITIES` | ✅ 27→32：新增 `desktop.credentials.v1`/`runtime.platform.v1`/`runtime.architecture.v1`/`request.context.v1`/`credential.kiro-external-refresh.v1`/`resources.fetch-public.v1`；移除虚报 `checkin.rolling.v1`；`verification.claim.v1` 保留（租约真服务，动作由 `servedActions` 标 false） | 代码审阅 + 静态扫描（`servedWithoutGuard` = 0） |
| 手工核对：插件调用面 | `desktop.credentials.read` 走 `permission("desktop.credentials.v1")` + `helpers` 声明 + 归属/作用域 | 代码审阅 |
| 单元测试（新增，`desktop_credentials.rs` 内 `mod tests`） | 8 个：helper 表覆盖 17 家与命名段、未知 provider 的 `desktop_credentials_unavailable`、`project` 投影只留白名单键、`content` 信封形状、`object` 上限/非对象/空文件、unix symlink 拒收、`catpaw_field` 头注入字符、导入作用域一次性 | 待 CI（`cargo test --offline plugin_host::wasm`） |

远端验证命令（CI 同款）：

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --offline plugin_host::wasm
# feature 矩阵（本次新增了 rusqlite 依赖面，三条腿都要跑）
cargo check --no-default-features
cargo check --no-default-features --features sea-orm,postgres
cargo check --no-default-features --features sea-orm,mysql
```

### 5.1 静态编译风险审查（本机禁编译，改用只读审查 + 已修）

用只读审查（不跑 cargo）对新代码做了一轮逐条核对，发现并修掉：

| 级别 | 问题 | 修复 |
|---|---|---|
| P0 | `external_refresh.rs`、`resource_fetch.rs` 用了 `base64::STANDARD.encode/decode` 但没引入 `Engine` trait（base64 0.22 里 encode/decode 是 trait 方法） | 两文件各加 `use base64::Engine as _;` |
| P0 | `services.rs` 测试里 `CAPABILITIES.iter().any(\|item\| item == capability)` 是 `&&str` 比 `&str` | 改 `*item == capability` |
| P0 | `desktop_credentials` 依赖 `rusqlite`，而它是 `sqlite-kv` feature 下的可选依赖；CI 跑 `--no-default-features` 三条腿必红 | 模块加 `#[cfg(feature = "sqlite-kv")]`；`services.rs` 未启用时回 `unsupported`（"本机构建未启用 sqlite-kv"）；`account_sessions.rs` 的 `import_scope` 包装同样按 feature 分支 |
| P1 | `astudio.rs` 非 Windows 上 `roots` 不再被 push → `unused_mut` | `#[cfg_attr(not(windows), allow(unused_mut))]` |
| P1 | `loomy.rs` 的 `sha2` import 只在 Windows 分支用 → 非 Windows `unused_imports` | `#[cfg(windows)] use sha2::{Digest, Sha256};` |
| P1 | `loomy.rs` 的 `candidates` 非 Windows 不再被 push → `unused_mut` | 同上 attr |
| P2 | `app_data()` / `loomy::app_root()` 是「函数体只有 `#[cfg] return`」形态，`needless_return` 可能报 | 改成各平台一个 cfg 表达式块（尾表达式） |

审查同时确认无问题的点（免重复排查）：模块与文件布局、`pub(super)` 可见范围、`WasmEgress`/`WasmAccountAccess`/`Resources` 调用签名、`aead 0.5` 的 `Aead::decrypt` 与 `Payload`、`#\[cfg\]` 平台分支完整性、`tempfile`/`tokio::test` 可用性、`client_stream` 数据面类型一致。

### 5.2 11 个读取器的第二轮审查（3 427 行）

同一方式复审 `desktop_credentials/*`，**P0 阻断项 0**（60 处父模块调用签名全部匹配、`super::` 层级全部正确、测试外 0 处 `unwrap/expect/panic`）。修掉：

| 级别 | 问题 | 修复 |
|---|---|---|
| P1 | `raccoon.rs` 的 `roots` 非 Windows 不再被 push → `unused_mut` | 同样加 `#[cfg_attr(not(windows), allow(unused_mut))]`（与 astudio/loomy 一致，之前只漏了这一个） |
| P2 | `raccoon.rs:109` 用 `aliases[0]` 硬取下标，配置成空组即 `panic`（release `panic=abort`） | 改 `aliases.first()`，空组跳过 |
| P2 | `doubao.rs` 打开客户端库副本用的是 `Connection::open`（读写） | 改 `open_with_flags(READ_ONLY｜NO_MUTEX)`，与父模块 `database()` 同口径 |

保留并已声明的偏差（不修，理由在文件头）：

- `loomy.rs` / `codar.rs` 用 `serde_json` 解析替代上游 `parse_object`：不拒重复键、不卡深度 32；只投影 3 个标量/白名单键，放大面为零。
- `doubao.rs` 的 `snapshot` 是「字节复制主库 + `-wal/-shm/-journal`」而非上游 `rusqlite::backup` 一致快照：客户端正在写时可能拿到撕裂副本，随后按「不可读」失败、不猜值。
- 非 Windows 的错误码统一成 `desktop_credentials_unavailable`（上游是 `desktop_platform_unsupported`），插件若按 `code` 分支需注意。


## 附录 A：最初的开工顺序（已完成，留作追溯）

```text
1) cargo test --offline plugin_host::wasm       # 先有基线
2) R2-1 autoclaw/doubao DPAPI：新增 windows-sys 绑定 → 两模块 read() → read() 分发打开
3) R2-3 legacy_credentials：Context 增 supplied_legacy_credentials → 登录/新增入口登记 → take 分派
4) R2-6 通用小项：runtime.platform / request.context / resource.fetch_public（三处独立，可并行）
5) 每步 cargo fmt && cargo clippy --all-targets -- -D warnings
```

## 6. 现状结论（本轮收口）

已闭环、无需再动：本机导入 17 家（含 DPAPI/safeStorage 两家）、`runtime.platform`/`runtime.architecture`、
`request.context`、`credential.kiro.external.refresh`、`resource.fetch_public`、能力表三条一致性修复、
未服务方法诊断分层、漂移检查器 + 基线、两轮只读编译风险审查（宿主侧 4 模块 + 11 读取器）。

剩余项都卡在代码之外：

| 项 | 卡点 |
|---|---|
| CI 验证 | 需要 push 授权（本机禁编译） |
| 插件侧 `desktop.refresh.*` | 决策不移植：原生 `account_session/refresh.rs` 已覆盖，双写会烧轮换 token（§3-2） |
| `credentials.legacy.take` | 缺「按用户选择的环境变量导入」UI 入口 |
| `login.desktop.credentials.take`（chatglm） | 依赖 `login.managed.v1` 执行上下文，宿主侧无该上下文时取值无处交付 |
| R3 专属流（`checkin_call`/`doubao_*`/`catpaw_turn`/`codearts`/`protocol`/`admin.action`/`captcha.push`/`token.*`） | 先确认 AIGX 是否有消费方；没有就记不适 |

## 7. 交付清单与提交口径

```text
M src/plugin_host/wasm/mod.rs          # 模块声明（desktop_credentials 按 sqlite-kv 门控）
M src/plugin_host/wasm/services.rs     # 7 条新分派臂 + UNSERVED_METHODS 诊断表 + 4 个测试
M src/plugin_host/wasm/manifest.rs     # 已服务能力 27→32，移除 1 项虚报（checkin.rolling.v1），注释写明理由
M src/plugin_host/wasm/context.rs      # Context.client_stream
M src/bridge/plugin.rs                 # 对话请求参数补 client_stream
M src/api/admin/account_sessions.rs    # 插件登录 RPC 包 import_scope（按 feature 分支）
A src/plugin_host/wasm/desktop_credentials.rs + desktop_credentials/（12 个读取器）
A src/plugin_host/wasm/external_refresh.rs
A src/plugin_host/wasm/resource_fetch.rs
A tools/account-pool-plugin-parity.mjs + .baseline.json
A docs/ACCOUNT-POOL-PLUGIN-PARITY.md
```

提交注意：工作区同时有**另一个会话**的未提交改动（`src/api/admin/tickets.rs`、`docs/CLIENT-ID-PARITY.md` 等），
必须用 pathspec 只提交上面这些路径，绝不 `git add -A`。

## 8. 回归单测清单（15 个）

| 模块 | 用例 |
|---|---|
| `desktop_credentials.rs`（8） | helper 表 17 家与命名段、未知 provider 码、`project` 白名单、`content` 信封、`object` 上限/非对象/空文件、unix symlink 拒收、`catpaw_field` 注入字符、导入作用域一次性 |
| `services.rs`（4） | 未服务方法指名能力、未知方法仍 `NO_METHOD`、`UNSERVED_METHODS` 不得指向已服务能力、表内无重复 |
| `external_refresh.rs`（4） | 端点 9 种非法形态、表单 RFC 3986 编码、stamp 随端点/密钥变化、旧版字符串 secret 解码 |
| `resource_fetch.rs`（3） | 头类型白名单、魔数优先于响应头、分片编码不超 append 上限 |
