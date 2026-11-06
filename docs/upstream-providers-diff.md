# Agent2API → AIGX 供应商漂移报告

生成时间：2026-11-06

由 `tools/sync_agent2api_providers.py --report` 生成，勿手改。

## 基准表缺条目（上游新增，需补 `AGENT2API_PROVIDERS`）

- （无）

## 基准表多余条目（上游已删，确认后可移除）

- （无）

## 登录态条目缺失（SESSION_PROVIDERS 未覆盖）

- （无）

## 添加账号表单缺失（addAccountConfigs 未覆盖）

- （无）

## 表单豁免（能力边界，有意不实现）

- `lobsterai`：AIGX 无 lobsterai OAuth web 登录实现（上游靠插件 providers::lobsterai::oauth）；已有账号走 account_session/import.rs 通用导入，注册表条目与 catalog spec 保留
- `qwenwork`：AIGX 无 qwenwork 后端（上游由闭源插件 agent2api-providers 实现，AIGX 无插件机制）；注册表不加条目
- `qwenwork-intl`：AIGX 无 qwenwork-intl 后端（上游国际版已停用，由闭源插件实现）；注册表不加条目

## 处理指引

1. `--write` 回写基准表（有测试断言覆盖，缺家会红）。
2. 登录态条目缺失的，在 `registry.rs` 的 `SESSION_PROVIDERS` 补 `SessionMeta`，
   并确认 `src/account_login/` 有对应登录实现。
3. 表单缺失的，按上游 `add-account-configs.ts` 把字段收敛进
   `frontend/src/lib/addAccountConfigs.ts`（数据照抄，渲染不用抄）。
4. 能力层（refresh / balance / checkin / models）按 `docs/porting-contract.md` 移植。
5. 表单豁免集（`INTENTIONAL_FORM_GAPS`）是方案 C 能力边界的可执行标注——
   AIGX 无实现的上游入口不自创、不显示。新增豁免需在脚本里登记原因。
