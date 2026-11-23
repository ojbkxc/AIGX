# beta.8 账号池插件面增量对齐（错误规则申报层）

> 日期：2026-11-24
> 上游：`D:\GitHub\Agent2API` tag `v3.5.0-beta.8.2`（`cc5d8b9b`）
> AIGX：基线 v1.0.93（`fb7afdb5`）+ `fd61d337`（并发会话的观测性修复）
> 范围：账号池相关的 wasm 插件宿主能力（`plugin_host/**`）+ 错误策略决策层

## 0. 本轮落地

| 项 | 内容 |
|---|---|
| `credentials.legacy-import.v1` 删除 | 上游 beta.8 删除了 `legacy_credentials` 模块与该能力（55→54），AIGX `CAPABILITIES` 同步删除；`services.rs::UNSERVED_METHODS` 的 `credentials.legacy.take` 条目转为「已随上游删除」占位（指名报错而非 NO_METHOD，静态自检跳过能力断言） |
| 错误规则申报层（`error_rules`） | 新建 `src/plugin_host/error_rules.rs`（对齐上游同路径文件）：解析插件 `init` 握手里的 `errorRules`（默认错误规则，≤32 条/provider）与 `errorClassifications`（识别映射，≤64 条/provider），清洗/裁剪边界与上游一致（状态码 400–599、业务码≤64 个、冷却 1–86400s 钳制、非 Auth 的 RefreshThenSwitch 降级 SwitchAccount、id 规范化 `plugin:{provider}:{local}` ≤128 字符） |
| `WasmHost` 存储 | `host.rs` 新增 `error_rules: Mutex<BTreeMap<String, DeclaredErrorPolicy>>` 字段 + `error_rules()` 读取口；握手成功后解析并落表（解析失败不阻断装载）；`host_providers()` 列出全部已启动 provider |
| 三层解析 | `error_policy.rs`：新增 `PluginErrorRule` / `PluginErrorClassification` / `DeclaredErrorPolicy` / `RuleSource`；`resolve_with`（例外 > 插件默认 > 全局，插件层按 model>code>statuses 特异性评分，被同 id 例外屏蔽的插件规则不参与）；`infer_exception_kind`（Unknown/400·403·409 反推大类）；`decide_with_declared`（识别映射优先改写 kind、hard_stop 建议仅对非例外来源生效、Auto 作用域采纳映射的 scope 建议）；`ErrorDecision` 新增 `rule_source` |
| 管理面视图 | `GET /api/routing/errors` 的 `error_policy_view` 新增 `providerRules` 字段（全部已启动插件的申报规则）；前端 `RoutingErrorsPanel` 新增「插件申报的默认规则（只读）」卡片（含「已被我的例外覆盖」徽标），i18n zh/en 同步 +4 键 |
| Keys 页搜索框吞键修复 | `useListKeyboard`：输入态（含搜索框）一律早退，不再把 j/k 当行间导航吞掉（此前在 API 密钥搜索框输入 `ojbkxc` 到 `j` 就打不进去）；非输入态 j/k/↑↓/Enter 导航不变；+3 回归测试 |
| 并发提交 `fd61d337` 的编译修复 | 该提交本身红灯：① `anthropic.rs` 用 `use super::openai::resolve::attempt_provider` 但 `mod resolve` 是私有的 → 升 `pub(crate)`（`openai/mod.rs`）；② `anthropic.rs`/`chat.rs` 4 处 `ch_ref.as_deref()`（`Option<Channel>` 无 Deref）→ `as_ref()`；③ `hub_fallback.rs` 测试用了不存在的 `ChatFormat::default()` → 手工最小构造；④ `provider()` dead_code → `#[allow(dead_code)]` + 注释（接线后移除） |

## 1. 验证（本机 mingw cargo check 允许口径内）

| 门禁 | 结果 |
|---|---|
| `cargo check` | ✅ EXIT=0 |
| `cargo clippy --bin aigx -- -D warnings` | ✅ EXIT=0 |
| `rustfmt --edition 2021 --check`（全部本轮触碰文件） | ✅ EXIT=0 |
| `cargo test --bin aigx` | ✅ **2510 passed / 0 failed**（含新增 `error_rules` 5 测 + `error_policy` 6 新测 + `hub_fallback` 4 测修复后全绿） |
| 前端 `tsc --noEmit` / `eslint --max-warnings 0` | ✅ 0 / 0 |
| 前端 `vitest run` | 174 passed；`Routing.test.tsx` 3 个失败为**存量**（v1.0.93 rail 重命名改动未同步 spec，stash 验证与本轮无关） |
| `tools/account-pool-plugin-parity.mjs --check` | ✅ 无新增漂移（可声明能力 54/54 对齐 beta.8.2） |

## 2. 与上游的偏差（如实声明）

| 项 | 上游 | AIGX |
|---|---|---|
| 握手申报载体 | `init` 应答里 providers 元数据数组原生带 `errorRules`/`errorClassifications` | 同样从 `init` 应答解析，但 AIGX 的 `WasmProviderInfo` 反序列化不消费这两个键，`error_rules::parse` 直接吃原始 JSON（`value` 或 `value.errorRules`），形状兼容两者 |
| `decide_with_declared` 证据层 | `evidence::candidate_codes(body)` 多候选码集 | AIGX 决策面只有单一 `code` 字符串（bridge 无 body 候选码提取），识别映射按「状态码或单码」匹配 |
| 全量 `cargo clippy --all-targets` / e2e | CI 权威 | 本机只跑 `--bin aigx`（AGENTS.md Rust 严格口径：本机 check/test 允许，全量交 CI/远端）；e2e 需重建 static，本波未做（未 bump 版本） |

## 3. 剩余缺口（beta.8.2 对照，54 项方法差）

维持 `docs/ACCOUNT-POOL-PLUGIN-PARITY.md` §3/§4 的分组与理由不变
（R2 遗留：`credentials.legacy.take` 随上游删除已不适用；R3 provider 专属流、
登录协议 ABI、managed_task 关闭 ABI、协议桥等按原决策不移植）。
基线文件 `tools/account-pool-plugin-parity.baseline.json` 已随本轮更新。

## 4. 提交口径

本轮触碰文件（git status 实测）：

```text
M src/error_policy.rs                    # 类型 + 三层解析 + decide_with_declared + 6 测
A src/plugin_host/error_rules.rs         # 握手申报解析（285 行 + 5 测）
M src/plugin_host/mod.rs                 # mod error_rules 声明
M src/plugin_host/wasm/host.rs           # error_rules 存储 + error_rules() + host_providers()
M src/plugin_host/wasm/mod.rs            # re-export host_providers
M src/plugin_host/wasm/manifest.rs       # CAPABILITIES 55→54 + 测试改名
M src/plugin_host/wasm/services.rs       # UNSERVED_METHODS 占位 + 静态自检豁免
M src/api/admin/settings.rs              # providerRules 视图字段
M src/api/anthropic.rs                   # as_deref→as_ref ×2（并发提交修复）
M src/api/openai/chat.rs                 # as_deref→as_ref ×2（同上）
M src/api/openai/mod.rs                  # resolve 升 pub(crate)（同上）
M src/bridge/hub_fallback.rs             # 测试 ChatFormat 构造 + provider() allow（同上）
M frontend/src/components/RoutingErrorsPanel.tsx  # 插件默认规则只读卡
M frontend/src/hooks/useListKeyboard.ts  # 吞键修复
A frontend/src/hooks/useListKeyboard.test.tsx     # 3 回归测
M frontend/src/i18n/zh.json / en.json    # +4 键
M tools/account-pool-plugin-parity.baseline.json  # 54 项基线
```

注意：工作区仍有**另一会话**的未提交改动（`src/account_session/*`、
`src/api/admin/account_sessions.rs`、`src/channel/mod.rs`、`frontend/src/pages/AccountSessions.tsx`、
`frontend/src/types/index.ts`），提交必须用 pathspec 限定上面清单，绝不 `git add -A`。
