# Magpie 脚本插件全量线上验证报告

> 日期：2026-11-22（生产时区 UTC+8）
> 环境：生产 `104.223.65.202`，AIGX **v1.0.85**（commit `429d15a2`；全量批次跑在 v1.0.83，v1.0.84 重测了 manifest 上限项，v1.0.85 只加展示兜底/工具/文档）
> 目录快照：`GET /api/plugins/scripts/catalog` → **264 条**（2026-11-20 manifest）
> 判据与批次计划：见 [`MAGPIE-PLUGIN-PARITY.md`](./MAGPIE-PLUGIN-PARITY.md)
>
> **v1.0.84 更新**：`package.json`/`wasm.json` 上限已从 256 KiB 对齐到 **1 MiB**；
> 原失败项 `github:tianpeng-dev/opencode-doubao-work-auth` 已在 1.0.84 上**重测通过**
> （安装成功、`doubao-work` 接管、`providerRouted=true`），通过数更新为 **254/264**。

## 1. 结论

| 来源 | 条目 | 线上端到端通过 | 失败 |
|---|---:|---:|---:|
| magpie official | 44 | **44** | 0 |
| npm recommended | 7 | **7** | 0 |
| GitHub third-party | 213 | **203** | 10 |
| **合计** | **264** | **254（96.2%）** | **10（3.8%）** |

- **官方与推荐包 100% 通过**：安装 → 启用 → 宿主装载 → provider 实际接管（`providerRouted=true`）全部成立。
- GitHub 第三方 10 条仍失败**全部有明确归因**：1 条仓库本身不可用，其余 9 条是插件自身的打包/配置/代码问题（另有 1 条 AIGX 侧上限问题已在 v1.0.84 修复并重测通过，详见 §4）。
- 验证过程宿主 `crashes=0`、`lastError=null`；每批结束均清理，生产最终回到验证前的 3 个已装包且全部 `providerRouted=true`。

## 2. 验证方法

单包 PASS 需同时满足：

1. `POST /api/plugins/scripts/install {selector, trusted:true}` 成功；
2. `POST /api/plugins/scripts/change {id, action:enable, enabled:true}` 成功且宿主完成 rebuild；
3. `packages[].error` 为空（宿主 `script.packages.error` 回填）；
4. `packages[].providerRouted === true`（实际 provider id 已进路由表）；
5. 采集前后 `host.crashes` 不增长、`host.lastError` 为空。

批次：`install → enable → 采集 → uninstall`，每批 6–8 个（`MAX_SCRIPT_PACKAGES=64`，上游同口径）。工具 `D:\tmp\aigx_batch_verify.py`（不入仓），结果 JSONL/CSV 在 `D:\tmp\v_{magpie,npm,github}.jsonl`。

## 3. 修复的两个线上阻断（v1.0.82 → v1.0.83）

### 3.1 路由恒空：宿主 init 是裸数组、没有 identities

- **现象**：`host.providers=[]`、所有包 `providerRouted=false`；但日志 `[Script] 宿主 init 完成，providers: ["xfyun-coding",…, "astudio-plugin"]` 明明有。
- **根因**：`host/main.mjs` 的 init 应答是 `[...providers.values()].map(metadata)`（裸数组），不含 `identities`；旧 `compute_routing` 只认 init identities + 包声明。声明 id 撞 reserved 被改名（`astudio`→`astudio-plugin`）、以及目录声明为空（npm/GitHub 装的家）时，实际 id 无人登记。
- **修复**：`script.packages` 作为 provider 身份权威（与上游 beta.7 同口径），加载成功时以实际 id 回填 `package.providers` 并登记「声明 id→实际 id」别名。
- **线上证据（1.0.83）**：

```
host.providers = ["agnes","astudio-plugin","xfyun-coding","xfyun-maas","xfyun-token"]
opencode-xfyun-maas-auth           routed=true  providers=[xfyun-coding, xfyun-maas, xfyun-token]
@magpie-community/...astudio-auth  routed=true  providers=[astudio-plugin]
@magpie-community/...agnes-auth    routed=true  providers=[agnes]
```

### 3.2 GitHub 来源全部装不上：`pax_global_header`

- **现象**：`github:` 安装报「包内存在多个根目录：pax_global_header 与 <repo>-<sha>」。
- **根因**：GitHub codeload tarball（`git archive` 产物）首块是 PAX 全局扩展头（tar type `g`）；`tar 0.4.46` 只吞 GNU longname/longlink 与 pax local，不吞 pax global。
- **修复**：解包器跳过 `is_pax_global_extensions / is_gnu_longname / is_gnu_longlink`；新增单测 `unpacks_github_codeload_tarball_with_pax_global_header`。
- **线上证据**：`github:miaocell/magpie-doubao-work-auth#9e85…` 从「解包失败」变为安装成功、`doubao` 接管。

## 4. 失败清单（10/264 仍失败，另有 1 条已在 v1.0.84 修复）

| # | selector | 分类 | 归因 |
|---|---|---|---|
| 0 | `github:tianpeng-dev/opencode-doubao-work-auth#7b34…` | 解包 | **已在 v1.0.84 修复**：`package.json` 857 KB 被旧 256 KB 上限拒绝；1.0.84 起上限 1 MiB（对齐上游），生产重测安装成功、`doubao-work` 接管 |
| 1 | `github:Guuuu233/claude-tui-boost#944d…` | 解包 | 压缩包 48.5 MB > 32 MB 上限（上游同为 32 MB）；包内为超大资产，按契约拒装 |
| 2 | `github:1466109846/ua-pro` | 解析 | 该仓库默认分支解析 `api.github.com/repos/…/commits` 未返回 sha（仓库/分支不可用） |
| 3 | `github:ikashana/magpie-comate-auth#635d…` | 依赖 | `bun install` 拉 `@baidu/comate-browser-use@2.0.4` 在 npmmirror 404（上游依赖缺失） |
| 4 | `github:mu-zi-lee/magpie-claudeweb2api#9fed…` | 加载 | 插件自报「No packaged backend for linux/x64」——仓库未提供 Linux 构建产物 |
| 5 | `github:korioinc/magpie-tokscale-import#fbbc…` | 加载 | 插件要求 Magpie 桌面端 `usage.jsonl`，运行环境不适用 |
| 6 | `github:jingyi0605/magpie-newapi#167d…` | 加载 | 「Duplicated provider in plugin package」——包内重复导出同一 provider（插件缺陷） |
| 7 | `github:qq136692547-cmyk/magpie-google-aistudio#1c36…` | 加载 | 「No valid API keys provided」——加载即要求有效 Key，未配置无法装载 |
| 8 | `github:double2tea/magpie-image-models#daa5…` | 加载 | 「Invalid plugin provider id」——导出的 provider id 不合法（插件缺陷） |
| 9 | `github:HYKSC-X/magpie-relay-identity#fb2c…` | 加载 | 必须配置 `options.baseURL` 才能装载（配置类插件） |
| 10 | `github:wanglz111/magpie-workbuddy-search#90f3…` | 加载 | 入口源码相对路径引用 `@magpie-community/opencode-workbuddy-auth`，但未声明依赖；同时安装兄弟插件仍不可解析（插件打包缺陷） |

> 另：`magpie-codex-guard` 首轮 load_error（后台进程启动竞争），重测通过，已计入通过。
>
> 配置类补充验证：`magpie-relay-identity` 在设置 `options.baseURL` 后安装→启用→`providerRouted=true`（error 为空），证明该类失败是「缺配置」而非宿主缺陷；`magpie-google-aistudio` 同属需要 API Key 的配置类，未提供凭据故未计入通过。

## 5. 产品边界与限制（非缺陷，需运维知悉）

- `MAX_SCRIPT_PACKAGES=64`（上游同口径）：**264 条不能同时驻留**。本报告为逐条验证 + 每批清理；生产最终保留验证前的 3 个包。
- 单 Bun 宿主承载全部已启用包，init 握手 60 s、单包 import bounded 20 s；不要一次启用过多重包。
- 安装护栏：压缩包 ≤32 MiB、解包 ≤128 MiB、条目 ≤4096、`package.json`/`wasm.json` ≤1 MiB（v1.0.84 已对齐上游；更早为 256 KiB）。
- GitHub 来源无 integrity，只靠 40 位 commit SHA 固定内容；目录中仅 1 条为分支 ref。
- 目录 `providers=[]` 的 119 条是常态（第三方入口在 JS 里注册），验收以宿主实际 id 为准。

## 6. 最终生产状态（验证后）

```
host: running=true crashes=0 lastError=null packages=3
providers: [agnes, astudio-plugin, xfyun-coding, xfyun-maas, xfyun-token]
opencode-xfyun-maas-auth           enabled routed=true
@magpie-community/...astudio-auth  enabled routed=true
@magpie-community/...agnes-auth    enabled routed=true
```

## 7. 红线

- 未写任何 provider 伪装 / 规避上游检测的实现；所有安装、加载、路由均走 AIGX 既有链路。
- 未打包、未分发任何 AGPL 插件二进制；验证只在生产运行期从目录/仓库拉取。
- 验证过程使用 `trusted:true` 显式安装第三方代码，所有临时包均已卸载。

## 8. 目录刷新复测（2026-11-22 晚，v1.0.85）

`POST /api/plugins/scripts/refresh` 后重新拉取 `GET /api/plugins/scripts/catalog`，目录由
264 条变为 **233 条**（official 44 / npm recommended 7 / GitHub 182；`status` 显示
`ignored=200`、GitHub/market/stars 均非 stale）：

| 变化 | 内容 |
|---|---|
| 新增 6 条 GitHub 条目 | 全部按同判据复测 **6/6 通过**（install → enable → routed → uninstall） |
| 移除 37 条 | 上游目录不再列出的仓库；其中含两条历史失败项 `tokscale-import`、`relay-identity` |
| 剩余失败项 | 8 条，commit SHA 与上轮完全一致，未变化，无需重测 |

新增 6 条结果（`D:\tmp\v_new6.jsonl`）：

```
magpie-plugin-msmoli        routed=true providers=[msmoli]
magpie-amazon-quick         routed=true providers=[amazon-quick]
magpie-lobsterai-auth       routed=true providers=[lobsterai-plugin]        # 原生 lobsterai 占用→改名
agent2api-plugin-astudio    routed=true providers=[astudio-plugin-f9ff8166fbfa]  # 二次冲突→追加 sha12
magpie-provider-eaichat     routed=true providers=[eaichat]
magpie-xiaoyunque           routed=true providers=[xiaoyunque]
```

其中 `lobsterai-plugin` 与 `astudio-plugin-f9ff8166fbfa` 验证了宿主身份改名的两条路径
（reserved 让位 `-plugin`、改名后仍冲突追加 sha12）。复测后生产仍回到验证前的 3 个包，
`host.crashes=0`、`lastError=null`、全部 `providerRouted=true`。
