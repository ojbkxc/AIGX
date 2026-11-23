# v3.5.0 正式版对齐（AIGX · 2026-11-24）

> 上游：`D:\GitHub\Agent2API` tag `v3.5.0`（`cc5d8b9b`）
> 实质变更只有一个提交 `a4ea2c11`（`cc5d8b9b` 纯版本号 3.5.0-beta.8.2→3.5.0）
> AIGX：基线 v1.0.93 + `fd61d337`/`1caec02b`（并发观测性修复）

## 0. v3.5.0 相对 beta.8.2 的服务端变更映射

| 上游变更（a4ea2c11） | AIGX 处置 |
|---|---|
| `accounts_list/balance.rs`：`ratio_of` 窗口行排序（同组聚拢、组内 5h→daily→weekly→monthly）+「周/月耗尽但无短窗口行的组按 0% 计入总百分比」（`exhausted_without_short`） | ✅ 已移植 [accounts_list.rs](../src/accounts_list.rs)（含上游两条测试向量，12 测全绿） |
| `plugin_host/install.rs` + `operations/management.rs`：单提供商包启停折叠——「包开+唯一子停」读入时折合为包停用；开包时清残留子停；子开关与包开关合一 | ✅ 已移植：[storage.rs](../src/plugin_host/script/storage.rs) `load_packages` 读取时归一化（+1 测，多提供商对照）；[operations.rs](../src/plugin_host/script/operations.rs) `set_enabled` 开包清子停、`set_provider_enabled` 单提供商包折到包开关 |
| `script/source.rs`：`download(allow_catalog_fallback)`——更新/重装**绝不**回退目录缓存 commit（防静默装旧提交后永远「可更新」）；`fetch_github_commit` 拆出可测 | ➖ 不适用：AIGX 的 `resolve_github` 本来就**只信 api.github.com 实时应答**，没有目录缓存回退路径（`github_outcome` 只用于只读检查，安装链 `install.rs` 不读目录 commit）。缓存回退 bug 在 AIGX 不存在 |
| `script/updates.rs`：缓存快照 + 指纹防写竞态 + `record_installed` 实时同步 | ➖ 结构不适用：AIGX `check_update_outcomes` 每次实时算（无磁盘缓存可竞态）；上游缓存是为桌面端高频轮询设计，AIGX 检查入口走维护任务租约已互斥 |
| `script/operations.rs`（+102）：安装登记与更新缓存联动 | 同上➖（AIGX 无更新缓存） |
| `upstream/error_policy_finish.rs` + `provider_loop.rs` + `plugin_forward.rs`：`persist_account_action` 收口（挂起/冷却落库统一入口，`!suspend && stop` 才写冷却） | ➖ 不适用：AIGX 无 `suspend_upstream_account`/`AccountStore` 桌面端账号挂起链（我们的数据面在 `error_policy.rs` 决策 + `ChannelStore::mark_cooldown` 渠道冷却，语义等价且已有） |
| `update/integrity.rs`：下载计划补 `urls: [url]` 数组（旧版 Android 客户端兼容） | ➖ 不适用：AIGX 的更新端点是自身 Release 管线，无 v3.4.2 Android 客户端存量 |
| `static_files.rs` +615 行（面板入口防缓存/SW 版本）、i18n 六语、手机端 | 手机端/六语不在 AIGX 范围（zh/en 双语 + 单页管理台）；SW 防缓存思路在 AIGX 无对应件 |
| wasm 宿主面（`plugin_host/wasm/**`） | **零 diff**（v3.5.0 与 beta.8.2 完全一致）；parity 基线 `--check` 对 v3.5.0 全绿 |

## 1. 本轮实现明细

### 1.1 `ratio_of`（账号余额百分比，悬停窗口行）

- `rows` 先按「组首次出现下标」聚拢，组内按 `5h(0)→daily(1)→weekly(2)→monthly(3)→其它(4)` 排序（`window_order_rank`）。
- `exhausted_without_short`：某组周/月耗尽但没有可用短窗口行 → 总百分比链尾补 `0.0`。
- 测试：`ratio_of_orders_short_window_before_weekly_within_each_group`（上游向量：5 行乱序 → 组聚拢 + 13.13% 取最小）、`ratio_of_counts_exhausted_group_without_short_window_as_zero`（0%）。

### 1.2 单提供商包启停折叠（脚本插件）

三处协同（与上游 `normalize_single_provider` 同语义，按 AIGX 的
`disabled_providers: Vec<String>` 形状落地）：

1. `storage::load_packages`：读取时发现「`enabled=true` 且唯一 provider 在 `disabled_providers`」→ 折合 `enabled=false` + 清空子停（保留用户停用选择）。
2. `operations::set_enabled`：重新启用单提供商包时清残留子停（防「包开了却不路由」）。
3. `operations::set_provider_enabled`：单提供商包的子开关直接写包开关；多提供商维持子开关语义。

测试：`load_normalizes_single_provider_conflict`（矛盾态折合 + 多提供商对照不动）。

## 2. 验证

| 门禁 | 结果 |
|---|---|
| `cargo check` / `clippy --bin aigx -D warnings` | ✅ 0 / 0 |
| `rustfmt --check`（本轮全部触碰文件） | ✅ 0 |
| `cargo test --bin aigx` | ✅ **2515 passed / 0 failed**（本轮 +5：accounts_list 2、storage 1、purge 明细 2 含 fixture 修正；上轮 2512 → 2513 含 PurgeConsole 明细 3） |
| script 全域 | ✅ 276 passed / 0 |
| 前端 tsc / eslint 0 警 / i18n zh=en | ✅ 全绿 |
| `account-pool-plugin-parity.mjs --check`（基线 = v3.5.0 wasm 面） | ✅ 无新增漂移 |

## 3. 破甲控制台（同轮完成，目标主线）

预览命中明细（Preview hit detail）整链路：

- 后端 `AppliedDetail`（normalized / dead_zone_rules / dead_zone_replacements /
  synonym_rules / synonym_replacements / structured_appended / skill_names≤16 /
  skill_truncated），`rewrite_texts`/`rewrite_texts_counted` 合并为
  `rewrite_texts_detailed`（计数落桶，替换语义逐字节不变），
  `inject_skills` 记命中技能名，`normalize_system_counted` 记被归一化条数。
- `POST /api/purge/preview` 响应加 `detail` 字段（`applied` 形状不变）。
- 前端 PurgeConsole 预览卡新增 chip 行（归一化 N / 死区词 R/S / 同义词 R/S /
  结构化已追加 / 命中技能：…（已截断）），旧后端无 `detail` 时整块不渲染。
- types 加 `PurgeAppliedDetail`；i18n zh/en +7 键；CSS `purge-preview-detail`。

## 4. 剩余（v3.5.0 明确不移植，理由如上表➖）

上游本轮其余服务端变更是**桌面端/手机端**形态修复（账号挂起持久化通道、
Android 下载兼容、SW 防缓存），AIGX 是服务端网关：数据面等价能力已在
`error_policy.rs` + `ChannelStore` 存在，移植反而引入双写。
