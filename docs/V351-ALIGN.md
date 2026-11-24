# v3.5.1 对齐台账（AIGX × Agent2API 最新正式版）

> 上游：`D:\GitHub\Agent2API` tag **v3.5.1** = `33f5e797`（发布 2026-11-23T17:46Z，
> `prerelease=false`）。`origin/main` 与 `origin/beta` 同指向它；
> `git ls-remote --tags origin` 无更新 tag —— 这就是当前最新正式版。
> AIGX：基线 v1.0.94。

## 1. 已对齐（3 项，均有测试）

| 上游变更 | AIGX 落点 | 提交 |
|---|---|---|
| `wasm/manifest.rs`：新增能力 `login.sms.alicom4.v1` + 依赖 `login.sms.v1`/`login.captcha.alicom4.v1` + codar 专属闸 | `src/plugin_host/wasm/manifest.rs`：CAPABILITIES 54→55、依赖表、provider 闸；parity 集合 55:55 双向零差集 | `9b883367` |
| 新增 `plugin_host/usage_policy.rs`：插件申报 `usageQuery{concurrency,minIntervalMs}`，宿主按家限流 | `src/plugin_host/usage_policy.rs`（解析/钳制 1–64、0–10s）+ `WasmHost.usage_policies` + `sweep` 分组限流（组内 `min(3, 申报值)`、相邻启动间隔 ≥ 申报值；未申报走 0 间隔，行为不变） | `7a5c164b` |
| `upstream/error_policy_finish.rs` + `route_sticky.rs`：`forget_sticky` —— 候选失败后**解除该会话绑定**，但仅当「绑定的正是该候选」且世代未变、且非 pinned | `AffinityCache::evict_if(session, model, channel_id)`；`mark_cooldown` 的亲和清除改为条件删除（AIGX 无 pinned/世代概念，用 channel_id 匹配等价替代） | `bbc80d05` |

## 2. 不移植（逐项写理由）

| 上游变更 | 不移植理由 |
|---|---|
| `core/providers/codex_subscription/{mod,http,images,errors,models,request}.rs`（+ 大量） | 订阅类 provider。`docs/BETA7-ALIGN-PARITY.md` §3 既定结论：chatgpt/claude/codex_subscription 依赖订阅登录链 + Responses 线协议，AIGX 未引入该产品面。`sync_agent2api_capabilities.py` 的 `CATALOG_EXEMPT` 已登记豁免 |
| `core/model_pricing/{mod,types,fx}.rs`（模型计价与费用估算体系） | AIGX 的计价面是 `src/pricing/`（exchange_rate / price_sync，与 `PricingStore` 直连），不是上游这套 `model_pricing` 快照体系。搬过来等于并存第二套计价模型，收益是重复、风险是双写不一致。需要先有「AIGX 引入 model_pricing」的产品决策 |
| `plugin_host/wasm/desktop_launch/windows/platform.rs`（+222） | 桌面客户端启动/恢复链。AIGX 是网关，没有桌面宿主；`desktop_launch` 在 AIGX 一直是「不适用」项（见 §3 的 parity 基线） |
| `plugin_host/wasm/{login.rs,codearts_login.rs}` 的 alicom4 挑战处理 | AIGX 无 `login/sms` 子系统（`sms_verify`/`sms_action` 在 AIGX 侧零命中），连短信登录链都没有；能力声明按上游放行（前向兼容），实际流程不在范围内 |
| `core/providers/adapter.rs` 的 `UpstreamResponse::Images` 构造 | 服务订阅 provider 的图片端点（与 codex_subscription 同族），随其一起不引入 |
| `core/providers/readiness.rs` 的 1ms 边界修正 | 上游把 `base + GATE_TTL_MS - 1` 的边界测试改成 `hold_at(..., base)` 避免跨毫秒。AIGX 没有对应 gauge 测试夹具，等价修正已在其自身测试里成立 |

## 3. 待办 / 复核方法

**待办**：无阻塞项。若上游再发新版本，按下面流程复核：

```bash
# 1. 版本锚点（别信本地缓存的 tag 列表）
git -C D:/GitHub/Agent2API fetch origin --tags
git -C D:/GitHub/Agent2API ls-remote --tags origin | Select-String 'v3\.'
curl -s https://api.github.com/repos/aimod-cc/Agent2API/releases?per_page=5

# 2. 宿主面全量比对（parity 工具必须跑，且要逐项而非只看计数）
node tools/account-pool-plugin-parity.mjs            # 人读报告
node tools/account-pool-plugin-parity.mjs --check    # 与基线比对

# 3. 能力表逐项（--check 曾因基线增量判定而漏报 alicom4，见 9171a2dc）
node -e "…比对两侧 CAPABILITIES 全集…"               # 见该提交的复现脚本思路
```

**教训（已入库）**：`--check` 以基线文件为参照做增量判定，**上游新增能力时不会自动变红**——
`login.sms.alicom4.v1` 就是靠人工全集比对才发现的（修复见 `9171a2dc`：四项差集进 baselineShape）。

## 4. 与其它文档的关系

- v3.5.0 正式版的对齐记录在 [`V350-ALIGN.md`](V350-ALIGN.md)（含 beta.8.2→v3.5.0 的 9 项判定）。
- beta.7/beta.8 的宿主面审计在 [`BETA8-ACCOUNT-POOL-ERROR-RULES.md`](BETA8-ACCOUNT-POOL-ERROR-RULES.md)
  与 `ACCOUNT-POOL-PLUGIN-PARITY.md`。
- 本文只覆盖 **v3.5.0 → v3.5.1** 这一跳；更早的判定不重复。
