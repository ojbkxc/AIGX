# AIGX 质量台账（"挑不出毛病" 清单）

> 口径：把已知缺陷/风险逐条登记，写明**状态**与**为什么现在这么处理**，避免同一问题被反复"发现"。
> 状态：✅ 已修 ｜ 🔧 进行中 ｜ ⏳ 已登记待排期 ｜ 🚫 确认不适用（附理由）
> 更新：2026-11-27

## 1. 依赖安全

| 项 | 影响 | 处理 | 状态 |
|---|---|---|---|
| `react-router-dom` 6.30.2–6.30.5 开放重定向→XSS（GHSA-wrjc-x8rr-h8h6 系列） | 中 | 升到 6.30.6（同 semver 小版本） | ✅ 已升 6.30.6（2026-11-27，`npm audit --omit=dev --audit-level=high` 已通过） |
| `react-router` < 7.18.0：`<Link>`/`useNavigate` 反斜杠开放重定向 + `deserializeErrors()` SSR 水合注入 | 中 | 需升到 7.18+（**破坏性大版本**）。AIGX 是纯 CSR（无 SSR、不调 `deserializeErrors`），现实不可达 | 🚫 暂不升级，升 v7 单独立项 |
| `i18next` < 26.3.4：`deepExtend` 递归进内建对象（DoS） | 中 | 需升到 27.x（**破坏性大版本**）。资源包是仓库内 JSON，非用户可控输入 | 🚫 暂不升级 |

CI 已加闸：`npm audit --omit=dev --audit-level=high`（只拦 high/critical，moderate 不阻断但有本表兜底）。

## 2. 测试覆盖盲区

| 模块 | 现状 | 状态 |
|---|---|---|
| `src/graphql` | 零测试 | ✅ 4 条（camelCase 反序列化 / errors / 取首个账号 / 缺 data 报错） |
| `src/oauth/{github,google,linuxdo}` | 零测试 | ✅ 已补（ready 判空 + 响应解析） |
| `src/web`（静态服务 / SPA fallback） | 零测试 | ✅ 已补（SPA 回退 / `/api` 404 / 缓存头 / 缺 index.html） |
| `src/payment` 的 stripe / order_store | 无测试 | ✅ stripe 5 条（签名/防重放）+ order_store 7 条（CAS 幂等/持久化/分页） |
| `src/db` 迁移层 | 无测试 | ✅ `db/mod.rs` URL 解析/脱敏 5 条；`migration/mod.rs` 新增迁移清单自检（名称唯一+时间戳升序），随 `sea-orm` feature 编译（`cargo test --features sea-orm`） |

补测优先级：先 `web`（几行就能覆盖 `/api/*` 404 与 SPA fallback）与 `oauth`（回调参数校验）。

## 3. 本地 / CI 一致性

| 项 | 说明 | 状态 |
|---|---|---|
| `PurgeConsole.test.ts` 在 Windows 本地失败（`default.resolve is not a function`） | 默认 jsdom 环境把 `node:fs`/`node:path` 解析成空模块；该文件是纯逻辑、需读后端源码 | ✅ 加 `// @vitest-environment node`（本地 219/219 通过） |
| 本机不能编译 Rust | 无 MSVC 工具链；项目约定禁止本地 `cargo check/build/test/clippy`，只跑 `cargo fmt --check` | ✅ 已写入 CLAUDE.md / AGENTS.md |
| `static/` 漂移 | 前端源码改动必须在**同一个提交**里重建 `static/`，CI 有闸门 | ✅ AGENTS.md 写明 |

## 4. 文档一致性

| 项 | 问题 | 状态 |
|---|---|---|
| `CLAUDE.md` | 技术栈写成 React 19 / TS 6 / shadcn（实际 18.2 / 5.3 / 自研组件）；"不支持单元测试" 不成立 | ✅ 已重写 |
| `AGENTS.md` | 工作区含不存在的 `aigx-net`；feature 矩阵 4 组（实际 6）；E2E 描述为"直连生产"（已改真后端+mock） | ✅ 已重写 |
| 5 份对齐台账 | 结论与源码不符（详见各自横幅） | ✅ 加过时横幅并指向 `V351-GAP-AUDIT-2026-11-26.md` |
| `docs/V351-ALIGN.md` | 部分结论待重新核实 | ✅ 2026-11-27 复核：用 parity 工具重跑宿主面（能力 55:55、双向差集 0），刷新基线使 `--check` 转绿，并追加 §5 复核记录；「上游有、AIGX 无」的 38 个方法逐项归入既有 B-P*/B-A* 台账 |

## 5. 性能

见 [`V351-PERF-AUDIT-2026-11-26.md`](./V351-PERF-AUDIT-2026-11-26.md)：已落地 / 进行中 / 未排期三段分明。
其中 `Arc<ChatFormat>`、`Arc<Channel>`、Drop 路径 BPE 正在第三轮团队里做。

## 6. 发布与运维

| 项 | 说明 | 状态 |
|---|---|---|
| CI 并发 | 连续 push 会叠加全套运行；已加 `concurrency`（同 ref 只保留最新一次） | ✅ |
| 生产配置被重置 | 新二进制首启可能把 `~/.aigx/config.toml` 重置回 `127.0.0.1:8080` | ✅ 根因修复：配置存在但读不出/解析失败时，先备份原文件到 `config.toml.broken-<ts>`，并置 `protect_from_overwrite` 拒绝后续 `save/save_with` 覆盖（`src/config.rs`，含 2 条回归测试）；AGENTS 硬约束 8 同步更新 |
| `origin` remote 内嵌 PAT | 明文 token 存在本地 `.git/config`（不入库）。业主 2026-11-27 裁定接受现状风险，择机自行轮换；建议轮换后改用凭据管理器/SSH | 🚫 已接受风险（用户裁定） |

## 7. parity「待核实」项复核（2026-11-27）

结论与依据见 [`V351-GAP-AUDIT-2026-11-26.md` §8](./V351-GAP-AUDIT-2026-11-26.md)。要点：

| 项 | 结论 | 一句话理由 |
|---|---|---|
| B-F6 `/api/model-access/{models,keys}` | 🚫 | 上游无鉴权、仅供 localhost 桌面壳；AIGX 公网暴露＝泄露 Key。等价能力是 `/v1/models` + `/api/tokens` |
| B-F7 `/api/advertisements` | 🚫 | 桌面运营广告位 + CDN feed，AIGX 无广告面 |
| B-F8 `/api/zcode/version` | 🚫 | 桌面面板兼容端点，上游 PUT 已废弃；AIGX 无版本同步宿主 |
| B-A5 逐账号 growth | 🚫 | 执行体在上游闭源插件（`workbuddy::admin`），网关只转发 |
| B-O1 峰谷分时/`same_as`/通配 | 🚫 | 上游独立计价体系，V351-ALIGN §2 已判定不移植；需产品决策另立项 |

## 8. 国际化

| 项 | 说明 | 状态 |
|---|---|---|
| zh-Hant | 5876 键全量，已修繁简转换用字错误；未上架 `SUPPORTED_LANGS`（等质量确认） | ✅ 语言包完成 |
| ja / ko / pt-BR | 业主 2026-11-27 裁定：**只做中文与英文**。已删除三个空占位包与动态 import（`AppLanguage` 仅 `zh/en/zh-Hant`），切换器只列简体中文/English | 🚫 不提供（用户裁定，已移除） |
