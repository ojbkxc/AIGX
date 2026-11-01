# Agent2API → AIGX 凭证字段模板漂移报告

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
