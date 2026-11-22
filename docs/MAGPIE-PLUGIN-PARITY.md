# Magpie 目录 × AIGX 脚本宿主兼容性审计

> 审计日期：2026-11-22
> 数据：生产 `GET /api/plugins/scripts/catalog` 快照 `D:\tmp\magpie_catalog.json`（264 条；`manifestAt`≈2026-11-20）
> 上游参考：Agent2API `v3.5.0-beta.7` `core/plugin_host/script/runtime.rs:348-419`（`script.packages` 是 provider 身份的**唯一权威**）
> AIGX 运行面：单 Bun 宿主 + OpenCode v1 auth provider；`MAX_SCRIPT_PACKAGES=64`（`src/plugin_host/script/types.rs:41`）；init 握手 60s（`process.rs` `HANDSHAKE_TIMEOUT`）；单包 import bounded 20s（`host/loader.mjs:64`）。
> 结论口径：**目录声明的 providers 不可信，验收一律以安装+启用后宿主的 `script.packages`/init providers 实际 id 为准。**

---

## 0. 结论（TL;DR）

- 264 条全部 `installable=true`，但 **119/264（45.1%）目录 providers 为空**（全部是 GitHub 第三方条目，占 GitHub 的 55.9%）。这些包能否接管 provider，只能装完 `enable` 后从宿主上报读取。
- 三条安装链路：44 magpie 官方（dist tgz **必带 sha512**）、7 裸 npm（装时现查 `registry.npmjs.org` 的 `dist.integrity`）、213 GitHub（commit 寻址，**无 integrity**；212 条已 pin 40 位 SHA，1 条为分支 ref）。
- provider id 在目录内已有 **12 个被多个条目重复声明**（`agentrouter` 被 5 个条目声称）。AIGX 的 `identity.allocateProvider` 会按 reserved/occupied 冲突把 id 改名为 `<id>-plugin`（`custom-` 前缀再加 `magpie-`，二次冲突追加 sha12），并在 `identities` 记录「声明 id → 实际 id」。**因此任何断言都必须用实际 id，不能用目录 id。**
- 加载面：所有已启用包在**同一个 Bun 进程内并发 import**（`loader.mjs` `Promise.all`），单包 20s 软超时；64 包同时装载的内存/启动耗时是首要风险。
- 批次：按 `MAX_SCRIPT_PACKAGES=64` 硬上限，264 条至少 **5 批**；建议每批 ≤48，采用 `install → enable → 采集 → disable → remove` 的清理式循环，避免 64 包同时驻留单 Bun。

---

## 1. 来源 / 发布者 / selector / providers 完整率

### 1.1 三段式分布

| 维度 | 值 | 数量 |
|---|---|---:|
| `source` | `github` | 213 |
| | `magpie` | 44 |
| | `npm` | 7 |
| `publisher` | `third-party` | 213 |
| | `official` | 44 |
| | `recommended` | 7 |
| `selector` 形态 | `magpie:<scoped-npm-name>` | 44 |
| | `github:<owner>/<repo>#<ref>` | 213 |
| | 裸 npm 名（无前缀） | 7 |
| `integrity` | 有（sha512） | 44（=全部 magpie） |
| | 无 | 220（全部 github+npm） |
| `archiveUrl` | 有（`raw.githubusercontent.com/.../dist/tgz/*.tgz`） | 44 |
| | 空（安装时现解析） | 220 |
| `kind` | `provider` | 264 |

> 说明：`publisher` 与 `source` 一一对应——official↔magpie、recommended↔npm、third-party↔github。前端/审计可只用其一。

### 1.2 selector 示例

| source | selector | archiveUrl | integrity |
|---|---|---|---|
| magpie | `magpie:@magpie-community/opencode-astudio-auth` | `https://raw.githubusercontent.com/magpie-community/plugins/dist/tgz/magpie-community-opencode-astudio-auth-0.2.4.tgz` | `sha512-…` |
| npm | `magpie-deepaicode`（裸包名） | `null` | `null` |
| github | `github:mu-zi-lee/magpie-our-free-model#ad4c4884…`（40 hex） | `null` | `null` |

### 1.3 providers 声明完整率（关键风险）

| 来源 | 条目 | providers 非空 | providers 空 | 空占比 |
|---|---:|---:|---:|---:|
| github | 213 | 94 | **119** | 55.9% |
| magpie | 44 | 44 | 0 | 0% |
| npm | 7 | 7 | 0 | 0% |
| **合计** | **264** | **145** | **119** | **45.1%** |

- providers 条数分布（按条目）：`0`→119、`1`→135、`2`→6、`3`→2、`4`→1、`11`→1。
- 声明 provider 总数 168，去重后 **152 个不同 id**。
- 多 provider 条目 10 个（示例）：`providers-hub`(11)、`qoder`(4)、`xfyun`(3)、`monkeycode`(3)、`codearts`(2)、`autoclaw`(2)、`trae`(2)、`minimax`(2)、`workbuddy`(2，magpie 与 github 各一)。
- `providerVerified=true` 对全部 264 条成立，但其中 119 条 providers 为空——**该字段不代表 providers 已知**，不能用于替代真实身份核对。
- GitHub 条目 `manifestAt`（≈2026-11-20）晚于 `pushedAt`，说明目录刷新时读过 package.json，但仍有 119 条解析不出 providers（多数第三方包把 provider 在入口 JS 里注册，不在 package.json）。**故目录 providers 缺失是常态，不是故障。**

### 1.4 provider id 冲突（目录内）

12 个 id 被 >1 条目声明；`agentrouter` 被 5 个条目声称：

| id | 声称条目数 |
|---|---:|
| agentrouter | 5 |
| gemini-web | 3 |
| codearts | 2 |
| loomy | 2 |
| orcaterm | 2 |
| workbuddy | 2 |
| workbuddy-ai | 2 |
| zcode | 2 |
| codar | 2 |
| teleagent | 2 |
| today | 2 |
| deeprelay | 2 |

AIGX 侧处理（`src/plugin_host/script/host/identity.mjs::allocateProvider`）：
1. 若 `entry.aliases[original]` 已存在且可用 → 复用（重启稳定）。
2. 否则若 id 已被 occupied（含 reserved 原生 provider 集合）→ 改名为 `<original>-plugin`（`custom-` 前缀先加 `magpie-`）。
3. 仍冲突 → 追加 `-<sha256(entry.id)[0..12]>`。
4. 网关 `runtime.rs` 把「声明 id → 实际 id」存入 `identities`，路由按实际 id 登记；`providerRouted` 也按实际 id 计算。

**推论**：同批装多个同名 provider 不会互相踢掉，但实际 id 会被改写；验收须用实际 id。

---

## 2. 安装链路风险分类（三类来源）

安装实现：`src/plugin_host/script/install.rs::fetch_and_unpack` + `catalog/distribution.rs` + `source.rs`。

| 来源 | 下载地址 | 完整性 | 校验/信任链 | 主要风险 |
|---|---|---|---|---|
| magpie（44） | dist tgz：`raw.githubusercontent.com/magpie-community/plugins/dist/tgz/*.tgz`（`archive_url` 必填） | **必须** `integrity`（sha512）；缺失直接拒装 | 本地 `packages::verify_integrity` 验 SRI | dist 索引刷新与 tgz 内容不一致；官方包在 npm 上多数 404，只能靠 dist |
| npm（7） | 装时查 `registry.npmjs.org/<name>` → `dist.tarball` | 装时现取 `dist.integrity` | `resolve_npm_tarball` 强校验 tarball 属 `registry.npmjs.org`，防元数据指向第三方 | npm 传播延迟/包被 deprecate/unpublish；7 个均为裸包名（非 scope） |
| github（213） | `codeload.github.com/<repo>/tar.gz/<commit>` | **无 integrity**（commit 寻址固定内容） | ref→40hex：212 条 selector 已 pin 40hex（**跳过 API，单跳**）；1 条分支 ref 走 `api.github.com/repos/…/commits` 解析（**两跳**） | 分支 ref 条目内容非不可变（`1466109846/ua-pro`）；仓库被删/私有化；tarball 内实际包名需探测 |

公共护栏（`packages.rs`）：archive ≤32 MiB、解包总量 ≤128 MiB、条目 ≤4096、`package.json`/`wasm.json` ≤256 KiB、tar 路径穿越防护。

**Bat 关注点**
- magpie 44 条 integrity 齐全，最可控，作为基线批。
- npm 7 条 integrity 依赖 registry 现查；若 npm 抖动会「解析 tarball 失败」，需重试而非判插件坏。
- github 213 条无 integrity，只靠 SHA；唯一非 pin 条目需单独标注，装前确认其 ref 仍指向预期 commit。

---

## 3. 加载链路风险

加载实现：`src/plugin_host/script/host/loader.mjs`、`entries.mjs`、`dependencies.rs`、`runtime.rs`。

| 风险 | 机制 | 影响 / 判据 |
|---|---|---|
| 单 Bun 承载全部包 | 一个 Bun 进程装载所有 enabled 脚本包；`load()` 对 entries 走 `Promise.all` **并发** import | 包越多、依赖越重，Bun 启动 + 首解析越慢；内存是硬约束。批次务必小 |
| 单包 import 20s bounded | `bounded(..., 20_000, "Provider plugin initialization")` | 单个慢/挂起的包 20s 后置 `error`，不阻塞其它包（并发）；但 import Promise 不可取消，挂死模块仍占资源 |
| init 握手 60s | `process.rs` `HANDSHAKE_TIMEOUT=60s` 包住整个 init | 并发 20s bounded 下通常 <20s 完成；64 包极端情况需观察是否逼近 60s |
| 依赖安装失败 | `bun install --production --ignore-scripts`（300s 超时、`node_modules` ≤512 MiB、隔离临时目录） | 依赖解析失败 → 安装失败，包不可用；重装/换源需人工 |
| provider id 撞 reserved | 原生 provider 与 `HARD_RESERVED=["cloudflare","hub"]`、`custom-*` 视为占用 | 自动改名为 `-plugin`；若改名后仍冲突再加 sha12；别名与清单不一致会整体失败 |
| 别名/清单一致性 | 网关校验 `aliases`/`execution` 与 `providers` 必须一一对应 | 插件自报不一致 → 包级 `error`，`providerRouted=false` |
| 启用即全量 rebuild | `enable` 触发单 Bun 重新装载全部 enabled 包 | 每次启用都会重载所有已启用包，批量时按包逐个 enable 会反复重建；应「先装齐一批 → 一次性启用/等待重建 → 采集」 |

**加载面验收要点**：`host.running=true` 且 `packages[].error` 为空 才可能 `providerRouted=true`；宿主没跑时 `providerRouted` 恒 false（设计如此，不是失败）。

---

## 4. 验收判据（逐包）

数据源：`GET /api/plugins/scripts`（不是 catalog）。字段路径：

```
host.running               必须 true
host.lastError             必须为空/null
host.crashes               基线值在批次前后不变
host.providers[]           实际接管的 provider id（改名后的真身）
packages[].id              包 id
packages[].enabled         true
packages[].error           必须为空（宿主 script.packages.error 回填）
packages[].providers[].id  目录/包声明的 provider（可能为空→以 host.providers 为准）
packages[].providerRouted  必须 true
```

单包 PASS 需同时满足：
1. `POST /api/plugins/scripts/install {selector}` 成功（HTTP 200，无 integrity/解包/依赖错）。
2. `POST /api/plugins/scripts/enable {id, enabled:true}` 成功，且宿主完成 rebuild（轮询到 `host.running=true`）。
3. `packages[id].error` 为空。
4. **实际 id 出现**：`packages[id].providers[].id`（若目录已知）或宿主 `host.providers` 中存在该包声明的 id（或改名后的 `<id>-plugin`）；`identities` 映射「声明→实际」。
5. `packages[id].providerRouted === true`。
6. 采集前后 `host.crashes` 不增长。

FAIL 分类：安装失败（网络/校验/依赖）、加载失败（`error` 非空，import 超时/别名不一致/身份冲突）、未接线（`error` 空但 `providerRouted=false`，通常是宿主没跑或 reserved 让位）。

> 目录 providers 为空的 119 条：PASS 的第 4 步改用 `host.providers` 与 `identities` 判定，不能依赖 `packages[].providers[]`。

---

## 5. 批次计划（`MAX_SCRIPT_PACKAGES=64`）

硬约束：已装脚本包去重数 ≥64 时安装被拒（`script/operations.rs`）。264 条 → **最少 5 批**；建议每批 **≤48**（给内置/已装包和单 Bun 内存留余量）。

循环模板（每批）：

```
for batch in batches:
  for selector in batch:  POST /api/plugins/scripts/install {selector, trusted:true}  # 安装必须显式信任
  for id in batch:        POST /api/plugins/scripts/enable  {id, enabled:true}   # 或一次性 /change
  wait rebuild -> host.running=true
  GET /api/plugins/scripts  -> 采集每包 {error, providerRouted, providers, crashes}
  for id in batch:        POST /api/plugins/scripts/change {id, action:"disable"} (enabled:false)
  for id in batch:        POST /api/plugins/scripts/uninstall {id}
  verify 已装数归零 / 回到基线，再进下一批
```

分批建议：
- 批次 1：44 magpie 官方（integrity 齐全、身份最清晰）——建立基线成功率。
- 批次 2：7 npm recommended（含 providers-hub 11 家、xfyun 3 家）+ 余量补 GitHub 中 providers 已知者。
- 批次 3-6：GitHub 213 条，按 `providers` 是否已知、stars、owner 拆批；每条 pin SHA 便于复现。
- 单独观察项：`agentrouter` 5 个候选、`gemini-web` 3 个——同批/跨批都要记录实际 id（`-plugin`/sha12 改名）与 `providerRouted`。
- 非 pin 条目 `github:1466109846/ua-pro` 单独一批，装前确认 ref 对应 commit 未漂移。

采集产物建议：`{selector, id, source, declared_providers, actual_providers, error, providerRouted, enabled, crashes_before, crashes_after, ts}` 一行一包，便于汇总成功率按来源切分。

---

## 6. 明确不做（红线）

- **不写任何 provider 伪装 / 规避上游检测的实现**：安装、加载、接线全部走 AIGX 既有链路，不为「装上去能跑」而修改宿主的请求头/指纹/风控绕过逻辑。
- **不打包、不分发 AGPL 上游代码**：本审计与后续批量验证只读取目录数据、调用既有 API；不把上游源码或插件包重新分发。
- 不以「目录 providers 为空」为理由改目录或伪造 provider id；身份以安装后宿主 `script.packages`/`identities` 为准。
- 不改 `src/`：本次仅产出本文档。

---

## 附：审计口径与证据锚点

- 目录数据统计脚本只读解析 `D:\tmp\magpie_catalog.json`（`data.entries`，264 条）。
- 安装链：`install.rs::fetch_and_unpack`（三类 source 分支）、`source.rs::resolve_github`（api.github.com→codeload 两跳，40hex 直接信）、`distribution.rs`（magpie dist tgz+integrity 补齐）、`market.rs`（npm registry 元数据）。
- 加载链：`host/loader.mjs`（`Promise.all` 并发 + `bounded 20_000`）、`host/entries.mjs::bounded`、`host/identity.mjs::allocateProvider`、`dependencies.rs`（bun install 300s/512MiB）。
- 运行时权威：`script/runtime.rs:348-419`（上游同口径）、`script/runtime.rs` 的 `compute_routing`/`identities`。
- 视图/验收字段：`api/admin/script_plugins.rs::package_view`/`host_view`（`providerRouted`、`error`、`providers`、`running`、`crashes`）；路由注册见 `main.rs:3333-3383`。
