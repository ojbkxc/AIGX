# Agent2API → AIGX 凭证字段模板漂移报告

> **口径（2026-11-24 标注）**：本报告是 2026-11-01 在 **legacy 布局**上游
> （`core/providers/<家>/credentials.rs`，实测 3.0.8）上生成的快照 —— 9 家全解析、
> 无漂移。上游 **v3.4.2 起**主仓已无这 9 家的 `credentials.rs`（只剩
> `chatgpt_subscription` / `codex_subscription`），凭证模板随执行体搬进闭源插件仓
> （`agent2api-providers`），所以**这份报告无法用当前上游重生成**：
> `--report` 在解析到 0 家时会直接拒绝写入（避免用空报告覆盖本节）。
> 当前结论请见 `docs/V351-VERIFY.md`。

生成时间：2026-11-01 16:25

本报告由 `tools/sync_agent2api_credential_fields.py --report` 生成。
字段模板 = 各家账号记录里除通用键（accessToken/refreshToken/expiresAt/userId）
外的具名凭证字段（AIGX `Credential.fields` 应与上游落库键对齐）。

上游可解析模板：9 家。

## AIGX 缺模板的家

（无）

## 字段级差异

（无）

## AIGX 有声明但上游未解析到的家

这些家在上游没有 `providers/<家>/credentials.rs`（或解析失败）——
原样保留，请人工核对是否为 AIGX 独有能力。

（无）

## 处理方式

运行 `--write` 让生成区与上游对齐（保留上游解析不到的家）；
通用键映射与转发/续期链路的字段消费在 `account_session` 各模块，
不属于本工具重写范围。
