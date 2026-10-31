# Agent2API → AIGX 供应商漂移报告

生成时间：2026-11-01

由 `tools/sync_agent2api_providers.py --report` 生成，勿手改。

## 基准表缺条目（上游新增，需补 `AGENT2API_PROVIDERS`）

- （无）

## 基准表多余条目（上游已删，确认后可移除）

- （无）

## 登录态条目缺失（SESSION_PROVIDERS 未覆盖）

- （无）

## 添加账号表单缺失（addAccountConfigs 未覆盖）

- （无）

## 处理指引

1. `--write` 回写基准表（有测试断言覆盖，缺家会红）。
2. 登录态条目缺失的，在 `registry.rs` 的 `SESSION_PROVIDERS` 补 `SessionMeta`，
   并确认 `src/account_login/` 有对应登录实现。
3. 表单缺失的，按上游 `add-account-configs.ts` 把字段收敛进
   `frontend/src/lib/addAccountConfigs.ts`（数据照抄，渲染不用抄）。
4. 能力层（refresh / balance / checkin / models）按 `docs/porting-contract.md` 移植。
