# Agent2API → AIGX 移植对照手册

> 本文档记录从 [aimod-cc/Agent2API](https://github.com/aimod-cc/Agent2API)（本地副本 `C:\GitHub\Agent2API-main`）移植到 AIGX 的功能与代码映射，以及后续跟随上游更新的手工同步方法。
> 上游是独立仓库（Tauri/Rust 混合架构，无 Rust 可直接 git 合并），同步只能按本表逐文件手工移植。

## 已移植功能

### 1. 统一模型管理（Agent2API models-page）

| AIGX | Agent2API 蓝本 |
|---|---|
| `src/model/manage.rs`（ModelRules 启停规则 + build_view 聚合） | `core/model_rules/mod.rs` + `core/providers/catalog/view.rs` |
| `src/api/admin/model_manage.rs`（`/api/models/manage` + state/price/alias 写接口） | `server/src/server/api/model_manage.rs` |
| `frontend/src/pages/ModelManage.tsx` | `ui-islands/src/islands/models-page.tsx` |
| 前端 API/类型：`frontend/src/api/index.ts`（listManageModels 等）、`frontend/src/types/index.ts`（ManageView） | `models-panel-state.ts` 等 |

差异：AIGX 聚合粒度为 `(provider, model_id)`，数据源是渠道模型 + 账号池模型（Agent2API 是各家 provider manifest）；定价走 AIGX 自己的 `pricing::ModelPrice`（Agent2API 的 rate 只是上游目录展示，不计费）。

### 2. 签到中心（Agent2API checkin-page）

| AIGX | Agent2API 蓝本 |
|---|---|
| `src/checkin/store.rs`（台账按日分桶 KV + 计划 + 设置） | `checkin_records` 表（schema v11）+ kv `autoCheckin.schedules` |
| `src/checkin/claim.rs`（各 provider 签到执行体，白名单制） | `core/billing/checkin.rs` |
| `src/checkin/scheduler.rs`（30s 轮询、UTC+8、当天去重、启动补签） | `core/auto_checkin.rs` |
| `src/api/admin/checkin_center.rs`（`/api/checkin-center/*`） | `api/checkin_{center,records,schedules}.rs` |
| `frontend/src/pages/CheckinCenter.tsx`（center/schedule/records 三 tab） | `ui-islands/src/islands/checkin-page.tsx` |

**首批 claim 白名单**（`src/checkin/claim.rs`）：`loomy / kuku / qoder / qoder-intl / xiaohuanxiong`。
未接入家的原因：需要各家签名算法（COSY/V11/HMAC）、设备指纹或风控头，未实测的照抄只会每天发失败请求。**后续扩展方法**：在 `claim.rs` 加分支 + `CHECKIN_PROVIDERS` 白名单加名，判据翻译成统一 `ClaimResult{success, msg, outcome}`。

注意区分：`/api/checkin`（用户签到送配额，`src/user/checkin.rs`）与 `/api/checkin-center`（管理员替上游账号签到）是两套并存系统。

### 3. 报表并入仪表盘（Agent2API report-page）

| AIGX | Agent2API 蓝本 |
|---|---|
| `src/api/admin/dashboard.rs` 尾部 `handle_report_{heatmap,top_sources,token_trend,overview}` | `api/stats_api.rs` + `request_stats/report.rs` |
| `frontend/src/pages/Dashboard.tsx`（Heatmap/TokenTrendChart 组件 + Top 渠道/模型卡 + 概览卡） | `report-page.tsx` + `report-charts.ts` |

差异：AIGX 日志无 provider/账号维度，Top 提供商/Top 账号统一为「Top 渠道」（`channel_name`）；权限走 `verify_user` + `is_admin_user` 收敛（普通用户只见自己的 `user_id` 数据）。

### 4. 定价倍率表格化

`frontend/src/pages/Pricing.tsx`：主视图表格（模型名/倍率/增删行）+ JSON 高级模式折叠项。后端零改动（`pricing::RatioConfig` 的 HashMap 结构天然适配），费用公式 `基础费用 × model_ratio × group_ratio`（`src/pricing/mod.rs` `calculate_cost`）不变。

## 供应商对齐状态

`src/account_session/registry.rs` 的 `AGENT2API_PROVIDERS` 已对齐上游 24 家全名单（workbuddy/-intl、raccoon、catpaw、autoclaw/-intl、qoder/-intl、cline-free/pass、accio/-cn、zcode/-intl、codearts、trae、loomy、kuku、monkeycode/-intl、commandcode、antigravity、officeace、orcaterm）。

## 后续手工同步上游的方法

1. 拉上游最新代码：`git -C C:\GitHub\Agent2API-main pull`（或重新下载 zip）。
2. 关注三个目录的变更：
   - `server/src/core/providers/<name>/` — provider 协议/端点变更 → 对照 `src/account_session/registry.rs` 与 `src/bridge/`
   - `server/src/core/billing/checkin.rs` — 签到接口变更/新增家 → 移植到 `src/checkin/claim.rs`
   - `server/src/core/providers/mod.rs` 的 `PROVIDERS` — 新增 provider → 同步 `AGENT2API_PROVIDERS` + registry 条目
3. 登录态型 provider 需同步确认 `src/account_login/` 有登录实现，否则账号加不进来。
4. 移植后本地 `cd frontend && npm run build` 验证前端，Rust 侧直接 push 走 CI 编译验证（本机不编译）。
