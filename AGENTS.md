# AGENTS.md — AIGX

> 给 AI 编码代理的仓库指南。规则分三级:**Never(禁止)/ Ask first(先问)/ 默认自主**。
> 本仓库由代理端到端开发,规则按"无人值守可执行"标准书写;每条硬规则都对应真实踩过的坑。

## 0. 项目速览(新会话必读)

**AIGX**(AI Gateway Extended)是 OpenAI/Anthropic 兼容的 AI 中转网关(对标 new-api):
Rust(axum 0.7 + Tokio)后端 + React 18 + TypeScript + Vite 前端,默认 SQLite KV 存储
(可切 PostgreSQL/MySQL via SeaORM feature),单二进制交付(前端构建产物内嵌 `static/`)。

- **仓库**:https://github.com/ojbkxc/AIGX(main 分支,直接 push,不走 PR)
- **生产**:http://104.223.65.202:9527(systemd 服务 `aigx`,二进制 `/opt/aigx/aigx`,
  数据 `~/.aigx/`)。SSH 凭据见用户私有记录,**勿写入任何入库文件**。
- **测试账号**:`admin / 123456`(username 或 `admin@aigx.local` 均可)
- **工作区**:Cargo workspace = 主 crate `aigx` + `aigx-net`(网络层)
- **后端主要模块**(`src/`):`api`(openai/anthropic/admin)、`channel`(调度/熔断/AIMD/健康)、
  `auth`(JWT/TOTP)、`user`、`payment`、`pricing`、`usage`、`quota_monitor`、`db`(SeaORM
  entity/migration)、`graphql`、`sse`、`oauth`、`guardrail`、`semantic` 等
- **前端**:`frontend/`(React+TS);构建产物落在 `static/` 并提交入库
- **E2E**:`tests/e2e/run-e2e.mjs`(Playwright 直连生产,12 项断言含真实上游对话)
- **同步工具**:`tools/newapi_sync.py`(new-api MySQL → AIGX 渠道/分组/定价)

### 硬约束(违反会导致返工,全部踩过的坑)

1. **本地不编译 Rust**——本机无完整验证工具链,所有 Rust 编译验证走 GitHub CI(push 后查
   Actions API)。本地只做前端验证:`cd frontend && node_modules/.bin/tsc --noEmit && npm run build`。
2. **axum 0.7 路由参数是 `:id` 不是 `{id}`**——`{id}` 是 0.8 语法,在 0.7 下被当字面量,
   导致所有带参路由 405。
3. **`--locked` 编译**——改 `Cargo.toml` 依赖必须同步 `Cargo.lock`(本地可用
   `cargo update -p <pkg>` 生成,但最终验证在 CI),否则 CI 全红。
4. **rustfmt/clippy 强制**——CI 跑 `cargo fmt --all -- --check` 和
   `cargo clippy --all-targets -- -D warnings`;单行超宽的长链式调用按 fmt 意见拆行。
   提交前自查不了 Rust,就保持改动小、风格贴近邻行代码,让 CI 首跑即绿。
5. **并行会话共存**——常有另一个 AI 会话同时编辑本仓库。开工前 `git status` 检查工作区;
   **只 add/commit 自己改的文件**(不 `git add -A`),发现他人未提交改动时避让。
6. **Rust 测试用独立临时目录**——`temp_dir + pid + AtomicU64 序号`;并行测试共用一个
   SQLite 文件会 `database is locked`。
7. **React `onClick={() => fn(args)}`**——`onClick={fn(args)}` 在渲染期立即执行,TS2322。
8. **部署新二进制后检查 config**——新二进制首启可能把 `~/.aigx/config.toml` 重置回
   `127.0.0.1:8080`,需 sed 回 `0.0.0.0:9527` 再重启。
9. **`scripts/` 目录被 gitignore**(部署助手,含服务器凭据)——可入库的工具脚本放 `tools/`。
10. **前端管理后台永远不调 `/v1/*`**——数据面要 sk-xxx 密钥,管理 token 会 401 并触发
    全局误踢登录;管理面一律走 `/api/*`。

## 1. Never(硬性禁止)

1. 禁止本地 `cargo build` / `cargo check` / `cargo test`(见硬约束 1)。
2. 禁止把服务器 IP 凭据、数据库密码、JWT secret 写进任何入库文件;部署脚本凭据走环境变量。
3. 禁止为绕过 CI 而修改 workflow(`.github/workflows/`)——除非任务就是修 CI 且用户明确要求。
4. 禁止直接改 `static/`(前端构建产物,由 `frontend` build 生成)。
5. 禁止删除/绕过测试使 CI 变绿;不许 `.skip`/`.only`/删断言。
6. 禁止动 SeaORM migration 历史文件(`src/db/migration/` 已有迁移不可改,只能新增)。
7. 禁止在 E2E 中引入 mock/fixture 拦截——AIGX 的 E2E 价值就在于真实上游;做不到就停下来问。
8. 禁止提交 `scripts/` 下的任何文件。

## 2. 架构边界

- **双挂数据面**:数据面 `/v1/*` 与无前缀双挂(`/v1/chat/completions` 和
  `/chat/completions` 都通)。改路由/中间件时两边都要顾到。
- **模型映射是可选别名**(new-api 语义):渠道 `models` 是主数据源,未命中映射时模型名
  原样透传;`/v1/models` = 启用渠道 models 聚合。`Some([])` 与 `None` 的 allowed_models
  同义为"不限"。
- **usage 转换发生在客户端边界**:`UsageStats` 携带两个不共用的缓存表示,跨协议 handler
  转换时,给客户端的与给上游的(UsageEvent 原样)分开。
- **RBAC**:角色 admin > manager > user > auditor;权限矩阵见 CLAUDE.md;令牌接口一律
  `verify_user` + 本人资源过滤;普通用户可见自己 key 明文,管理员看脱敏。
- **feature 矩阵**:default(sqlite-kv)/ no-default(JSON FileStore)/ sea-orm+postgres /
  sea-orm+mysql 是四个独立产品面,CI 逐个 check 且互不取消;动存储层/feature gate 时
  四个面都要在脑内过一遍编译影响。

## 3. 开发方法( Think Before Coding / 简化优先 / 手术式修改)

- **思考优先**:假设显式;多重解释时呈现并询问,不沉默选择;存在更简单方法必须说出来。
- **范围只从用户请求的原话出发**:编码前回答三件事——用户要求什么 / 我将构建什么 /
  我将放弃什么。**方法削减必须明说放弃了什么,不许沉默削减。**
- **简化优先**:最少的代码解决问题;没有超范围功能;单用途不抽象;200 行能变 50 行就重写。
- **手术式修改**:只改必须改的;不"顺手"改进邻近代码;匹配现有风格;删除自己改动孤立
  出来的导入/变量,但不动预先存在的死代码(可提及)。
- **bug fix 只修已复现的缺陷**;hunch(猜相邻代码也有问题)不可作为扩大范围的理由——
  验证过确实同病才能一并修,否则留给用户。
- **参考实现优先**:实现新端点/转换前,比较至少三个主流网关(new-api/one-api 类)如何处理,
  引用上游规范;偏离需命名并给理由("他们做 X 但我们需要 Y,因为 Z")。

## 4. 验证纪律

- **E2E 是最高优先级信号**:面向用户真实旅程(`tests/e2e/run-e2e.mjs` 直连生产),
  不 mock 网络;前端 UI 改动必跑 Playwright E2E 或在 PR 描述里说明为何跑不了。
- **完成态定义 = CI 绿。** push 不是终点;push 后盯 CI,红了修根因,不许带着红 CI
  结束回合,也不许 push 完就 idle。
- **CI 看板查询**(本机无 `gh`):
  ```
  curl -s "https://api.github.com/repos/ojbkxc/AIGX/actions/runs?head_sha=<SHA>&per_page=5"
  ```
- **CI 三关**:lint(fmt+clippy)→ frontend(tsc+vite build)→ rust-matrix(4 feature 组合
  `cargo check --locked`)+ `cargo test --locked` + `cargo build --release --locked`。
  推送前把改动会触碰的关卡在脑内预演一遍,减少反复红绿。
- **不许宣称被阻塞**:限制是"从真实失败挣来的结论",不是从配置/文档读来的字段。没实际
  尝试就说 "not attempted",不许说 "we can't";被挡下要引用真实报错。

## 5. 自动迭代闭环

无人值守迭代严格走以下循环:

```
理解任务 → 就绪判定 → 手术式实现 → 前端本地验证(tsc+build)
    ↑                                          │
    └──── 修根因(≤2 次仍红则停下报告)←── CI 红 ←─ push main → 盯 CI → 绿
                                                                  │
                                              行为变更写入 PR/commit 描述 ──→ 完
```

1. **就绪判定**:没有明确 issue/方案的非平凡改动,先写简短方案(目标/取舍/放弃项)再动手。
2. **push 方式**:用用户提供的 PAT 拼 URL 一次性推送(格式 `git push
   https://<user>:<token>@github.com/ojbkxc/AIGX.git main`);凭据见用户私有记录,严禁
   入库或写入任何文件;禁止交互式凭据管理器(无人值守会永久挂起)。
3. **commit/push 前自检**:`git status` 只含自己的文件 → diff 对照第 1/2/3 节 → commit
   信息写清"改了什么、为什么"(中文,与历史风格一致)。
4. **CI 失败处理**:只修根因;同一修复盲试不超过 2 次,仍红则停下报告并附 run URL。
5. **行为变更必须在描述中陈述**:发布说明从 PR/commit 内容汇编——验证变严、过滤变宽、
   默认值变化、schema/线路重塑,都要写"升级后现有调用方体验到什么变化"。不要开单独
   tracking issue;描述就是记录。
6. **Handler family 同步修复**:修改按请求机制(限流/auth/metrics/头转发)时,grep 整个
   handler family,流式与非流式兄弟路径在同一提交里接上,或明确声明哪个延迟及原因。
7. **workaround 必须留痕**:打补丁/加版本上限/临时禁用时,在代码注释写明 workaround
   内容、原因、后续方向(完整 URL 引用相关 issue/文档),不留"以后再说"的空话。
8. **结束卫生**:不留未提交改动;最终答复带 CI 结论或 commit SHA;E2E 失败附截图路径
   (`tests/e2e/screenshots/`)。

## 6. Ask first(先问再做)

- 新增依赖(Rust crate / npm 包)。
- 改 RBAC 权限矩阵、计费/定价逻辑、payment 流程。
- 破坏性更改(API 形状、磁盘格式、线路协议、默认值转移)——需用户确认是否版本化/门控。
- 删除超过 100 行的批量清理;重命名公共 API。
- 手动部署生产(systemd 重启、SSH 上服务器)——先问,部署后按硬约束 8 检查 config。

## 7. 参考文件

- 开发者总览与迁移计划:`CLAUDE.md`
- 架构:`ARCHITECTURE-HORIZON-2100.md`;后端 API:`API-ARCHITECTURE-2100.md`;
  前端演进:`FRONTEND-EVOLUTION-V3.md`;TS 迁移:`frontend/TYPESCRIPT-MIGRATION-PLAN.md`
- 功能对标(本机):`C:\GitHub\rustapi\new-api-main`(用户中心/充值/权限)、
  `C:\GitHub\rustapi\open-webui-main`(UI 审美基准)、`C:\GitHub\v2board`(账户/找回密码)
