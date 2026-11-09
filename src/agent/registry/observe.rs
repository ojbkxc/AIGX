//! `observe` 域工具声明（#35 批2 拆模块：从 registry.rs 机械切出，块内零改动）。
//!
//! 组装点在 [`super::build_tools`]；顺序敏感，勿单独调序。执行体复用 `super::*`。

use super::*;
use RiskLevel::ReadOnly;

pub(super) fn tools() -> Vec<ToolDef> {
    vec![
        // ── 能力自发现（新 API 落进 OpenAPI 文档即可被搜到）──
        td(
            "aigx_openapi_search",
            "搜索管理 API",
            "只读：按关键词搜索管理面 API 清单（路径/方法/摘要），用于发现尚未封装成工具的端点",
            ReadOnly,
            "",
            &["/api/openapi.json"],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "关键词（路径/摘要/标签子串，可中文）" },
                        "tag": { "type": "string", "description": "按标签过滤（如 账号池/渠道/定价）" },
                        "limit": { "type": "integer", "minimum": 1, "maximum": 50, "description": "返回条数上限，默认 20" }
                    },
                    "required": ["query"]
                })
            },
            |_s, _h, a| {
                Box::pin(async move {
                    let query = require_arg(a, "query")?;
                    let tag = a.get("tag").and_then(|v| v.as_str()).map(str::to_string);
                    let limit = a.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as usize;
                    let items = crate::api::admin::openapi::search_openapi(&query, tag.as_deref(), limit);
                    Ok(ToolOutcome {
                        text: crate::api::admin::common::truncate_tool_text(
                            json!({ "success": true, "data": { "query": query, "total": items.len(), "items": items } })
                                .to_string(),
                        ),
                        ok: true,
                    })
                })
            },
        ),
        // ══ #35 批1：只读观测补齐（按域聚合，一条工具 owns 多条同域路由）══
        //
        // 本段全部是「既有管理面 GET handler 直调」：不新增路由、不改 handler、
        // 不引入写侧。写侧（POST/PUT/DELETE）按批 2 的高危审批基线逐条论证。
        // 分派口径：`view`/`key` 走 enum 白名单（防模型瞎传），未知值直接 -32602。
        td(
            "aigx_dashboard_stats",
            "运维大盘视图",
            "只读：运维大盘视图（view 选 realtime/channelHealth/consumptionTrend/modelDistribution/cacheSavings/userRanking/schedulerStatus/cacheTrend24h；days 或 start/end 决定窗口）",
            ReadOnly,
            "view",
            &[
                "/api/dashboard/realtime",
                "/api/dashboard/channel_health",
                "/api/dashboard/consumption_trend",
                "/api/dashboard/model_distribution",
                "/api/dashboard/cache_savings",
                "/api/dashboard/user_ranking",
                "/api/dashboard/scheduler_status",
                "/api/dashboard/report/cache_trend_24h",
            ],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "view": { "type": "string", "enum": ["realtime","channelHealth","consumptionTrend","modelDistribution","cacheSavings","userRanking","schedulerStatus","cacheTrend24h"], "description": "视图种类" },
                        "days": { "type": "integer", "minimum": 1, "maximum": 365, "description": "时间范围（天），默认 30；仅趋势/分布/健康/排行视图消费" },
                        "start": { "type": "string", "description": "起始日 YYYY-MM-DD（给了就覆盖 days）" },
                        "end": { "type": "string", "description": "结束日 YYYY-MM-DD" }
                    },
                    "required": ["view"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let view = require_arg(a, "view")?;
                    let q: admin::dashboard::DashboardQuery =
                        parse_tool_args("aigx_dashboard_stats", a)?;
                    out(match view.as_str() {
                        "realtime" => admin::handle_realtime(State(s.clone()), h.clone()).await,
                        "channelHealth" => admin::handle_channel_health(State(s.clone()), h.clone(), Query(q)).await,
                        "consumptionTrend" => admin::handle_consumption_trend(State(s.clone()), h.clone(), Query(q)).await,
                        "modelDistribution" => admin::handle_model_distribution(State(s.clone()), h.clone(), Query(q)).await,
                        "cacheSavings" => admin::handle_cache_savings(State(s.clone()), h.clone(), Query(q)).await,
                        "userRanking" => admin::handle_user_ranking(State(s.clone()), h.clone(), Query(q)).await,
                        "schedulerStatus" => admin::handle_scheduler_status(State(s.clone()), h.clone()).await,
                        "cacheTrend24h" => admin::handle_report_cache_trend_24h(State(s.clone()), h.clone()).await,
                        _ => return Err((-32602, format!("Invalid params: unknown view {view}"))),
                    })
                })
            },
        ),
        td(
            "aigx_system_health",
            "系统资源与健康",
            "只读：系统资源快照与健康检查（view 选 system=CPU/内存/负载进程、app=精简健康态、probe=healthz 存活探针）",
            ReadOnly,
            "view",
            &[
                "/api/monitor/system",
                "/api/monitor/health",
                "/api/monitor/healthz",
            ],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "view": { "type": "string", "enum": ["system", "app", "probe"], "description": "视图种类" }
                    },
                    "required": ["view"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let view = require_arg(a, "view")?;
                    out(match view.as_str() {
                        "system" => admin::handle_monitor_system(State(s.clone()), h.clone()).await,
                        "app" => admin::handle_health(State(s.clone()), h.clone()).await,
                        "probe" => admin::handle_healthz(State(s.clone()), h.clone()).await,
                        _ => return Err((-32602, format!("Invalid params: unknown view {view}"))),
                    })
                })
            },
        ),
        td(
            "aigx_security_watch",
            "安全事件观测",
            "只读：安全汇总与安全事件流（view 选 summary/events；events 可按 type/range 过滤并分页）",
            ReadOnly,
            "view",
            &["/api/monitor/security", "/api/monitor/security/events"],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "view": { "type": "string", "enum": ["summary", "events"], "description": "视图种类" },
                        "type": { "type": "string", "description": "事件类型过滤（仅 events）" },
                        "range": { "type": "string", "description": "时间范围（仅 events，如 today/7d）" },
                        "page": { "type": "integer", "minimum": 1 },
                        "page_size": { "type": "integer", "minimum": 1 }
                    },
                    "required": ["view"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let view = require_arg(a, "view")?;
                    let q: admin::security::SecurityEventsQuery =
                        parse_tool_args("aigx_security_watch", a)?;
                    out(match view.as_str() {
                        "summary" => admin::handle_security_summary(State(s.clone()), h.clone()).await,
                        "events" => admin::handle_security_events(State(s.clone()), h.clone(), Query(q)).await,
                        _ => return Err((-32602, format!("Invalid params: unknown view {view}"))),
                    })
                })
            },
        ),
        td(
            "aigx_log_watch",
            "运行日志与审计",
            "只读：运行日志/在途请求/审计日志/清理预演（view 选 runtime/inflight/audits/clearPreview；runtime 用 since_id 增量拉，audits 分页，clearPreview 只统计不落刀）",
            ReadOnly,
            "view",
            &[
                "/api/logs/runtime",
                "/api/logs/inflight",
                "/api/logs/audits",
                "/api/logs/requests/clear-preview",
            ],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "view": { "type": "string", "enum": ["runtime", "inflight", "audits", "clearPreview"], "description": "视图种类" },
                        "since_id": { "type": "integer", "minimum": 0, "description": "运行日志未读水位（只回 id 大于此值的行）" },
                        "page": { "type": "integer", "minimum": 1, "description": "审计日志页码" },
                        "size": { "type": "integer", "minimum": 1, "description": "审计日志每页条数" },
                        "days": { "type": "integer", "minimum": 0, "description": "clearPreview 只统计最近 N 天（0/缺省=全部）" }
                    },
                    "required": ["view"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let view = require_arg(a, "view")?;
                    out(match view.as_str() {
                        "runtime" => {
                            let q: admin::logs::RuntimeLogsQuery =
                                parse_tool_args("aigx_log_watch", a)?;
                            admin::handle_get_runtime_logs(State(s.clone()), h.clone(), Query(q)).await
                        }
                        "inflight" => admin::handle_list_inflight(State(s.clone()), h.clone()).await,
                        "audits" => {
                            let q: admin::logs::AuditLogQuery =
                                parse_tool_args("aigx_log_watch", a)?;
                            admin::handle_list_audit_logs(State(s.clone()), h.clone(), Query(q)).await
                        }
                        "clearPreview" => {
                            let q: admin::logs::ClearPreviewQuery =
                                parse_tool_args("aigx_log_watch", a)?;
                            admin::handle_clear_preview(State(s.clone()), h.clone(), Query(q)).await
                        }
                        _ => return Err((-32602, format!("Invalid params: unknown view {view}"))),
                    })
                })
            },
        ),
        td(
            "aigx_alert_console",
            "告警台",
            "只读：当前活跃告警与历史告警（view 选 active/history；history 可 limit 限条数，上限 500）",
            ReadOnly,
            "view",
            &["/api/alerts/active", "/api/alerts/history"],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "view": { "type": "string", "enum": ["active", "history"], "description": "视图种类" },
                        "limit": { "type": "integer", "minimum": 1, "maximum": 500, "description": "history 返回条数，默认 100" }
                    },
                    "required": ["view"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let view = require_arg(a, "view")?;
                    let q: admin::legacy::AlertHistoryQuery =
                        parse_tool_args("aigx_alert_console", a)?;
                    out(match view.as_str() {
                        "active" => admin::handle_alerts_active(State(s.clone()), h.clone()).await,
                        "history" => admin::handle_alerts_history(State(s.clone()), h.clone(), Query(q)).await,
                        _ => return Err((-32602, format!("Invalid params: unknown view {view}"))),
                    })
                })
            },
        ),
        td(
            "aigx_plugin_status",
            "插件/内置供应商目录状态",
            "只读：插件清单与在途/更新状态（view 选 list/progress/updates/release；内置 provider 目录随版本发布，progress/updates/release 为空态说明）",
            ReadOnly,
            "view",
            &[
                "/api/plugins",
                "/api/plugins/progress",
                "/api/plugins/update",
                "/api/plugins/release",
            ],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "view": { "type": "string", "enum": ["list", "progress", "updates", "release"], "description": "视图种类" }
                    },
                    "required": ["view"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let view = require_arg(a, "view")?;
                    out(match view.as_str() {
                        "list" => admin::plugins::handle_list_plugins(State(s.clone()), h.clone()).await,
                        "progress" => {
                            admin::plugins::handle_get_plugin_progress(State(s.clone()), h.clone()).await
                        }
                        "updates" => {
                            admin::plugins::handle_check_plugin_updates(State(s.clone()), h.clone()).await
                        }
                        "release" => {
                            admin::plugins::handle_refresh_plugin_release(State(s.clone()), h.clone()).await
                        }
                        _ => return Err((-32602, format!("Invalid params: unknown view {view}"))),
                    })
                })
            },
        ),
        td(
            "aigx_usage_analytics",
            "用量分析",
            "只读：用量汇总/趋势/按模型分布（view 选 summary/trend/models；summary 可带 user/channel/model 等过滤参数）",
            ReadOnly,
            "view",
            &["/api/usage/summary", "/api/usage/trend", "/api/usage/models"],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "view": { "type": "string", "enum": ["summary", "trend", "models"], "description": "视图种类" },
                        "user": { "type": "string", "description": "按用户过滤（仅 summary）" },
                        "channel": { "type": "string", "description": "按渠道过滤（仅 summary）" },
                        "model": { "type": "string", "description": "按模型过滤（仅 summary）" }
                    },
                    "required": ["view"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let view = require_arg(a, "view")?;
                    out(match view.as_str() {
                        "summary" => {
                            let q = crate::api::admin::mcp::args_to_string_map(a);
                            admin::handle_usage_summary(State(s.clone()), h.clone(), Query(q)).await
                        }
                        "trend" => admin::handle_usage_trend(State(s.clone()), h.clone()).await,
                        "models" => admin::handle_usage_models(State(s.clone()), h.clone()).await,
                        _ => return Err((-32602, format!("Invalid params: unknown view {view}"))),
                    })
                })
            },
        ),
        td(
            "aigx_network_status",
            "网络层状态",
            "只读：网络层（CF 账号池）状态与账号明细（view 选 status/accounts；含池规模/健康统计/活跃数，仅管理员）",
            ReadOnly,
            "view",
            &["/api/network/status", "/api/network/accounts"],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "view": { "type": "string", "enum": ["status", "accounts"], "description": "视图种类" }
                    },
                    "required": ["view"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let view = require_arg(a, "view")?;
                    out(match view.as_str() {
                        "status" => admin::health_check(State(s.clone()), h.clone()).await,
                        "accounts" => admin::list_network_accounts(State(s.clone()), h.clone()).await,
                        _ => return Err((-32602, format!("Invalid params: unknown view {view}"))),
                    })
                })
            },
        ),
        td(
            "aigx_routing_watch",
            "路由决策观测",
            "只读：粘性映射/排队快照/单请求路由回放（view 选 sticky/queue/replay；replay 需 request_id，`route_replay` 开关关闭的历史请求回放字段为缺省）",
            ReadOnly,
            "view",
            &[
                "/api/routing/sticky",
                "/api/routing/queue",
                "/api/routing/replay/{request_id}",
            ],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "view": { "type": "string", "enum": ["sticky", "queue", "replay"], "description": "视图种类" },
                        "request_id": { "type": "string", "description": "回放目标请求 ID（view=replay 必填）" }
                    },
                    "required": ["view"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let view = require_arg(a, "view")?;
                    out(match view.as_str() {
                        "sticky" | "queue" => {
                            let q = crate::api::admin::mcp::args_to_string_map(a);
                            if view == "sticky" {
                                admin::handle_get_route_sticky(State(s.clone()), h.clone(), Query(q)).await
                            } else {
                                admin::handle_routing_queue(State(s.clone()), h.clone(), Query(q)).await
                            }
                        }
                        "replay" => {
                            let id = require_arg(a, "request_id")?;
                            admin::logs::handle_get_route_replay(State(s.clone()), h.clone(), Path(id)).await
                        }
                        _ => return Err((-32602, format!("Invalid params: unknown view {view}"))),
                    })
                })
            },
        ),
        td(
            "aigx_upstream_watch",
            "上游账号池接入观测",
            "只读：上游账号（Agent2API 型）接入侧观测（view 选 connections/importSources/desktopSources/usageSnapshot；不含登录流程与导出）",
            ReadOnly,
            "view",
            &[
                "/api/account-sessions/connections",
                "/api/account-sessions/import/sources",
                "/api/account-sessions/desktop/sources",
                "/api/account-sessions/usage/snapshot",
            ],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "view": { "type": "string", "enum": ["connections", "importSources", "desktopSources", "usageSnapshot"], "description": "视图种类" }
                    },
                    "required": ["view"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let view = require_arg(a, "view")?;
                    out(match view.as_str() {
                        "connections" => {
                            admin::account_sessions::handle_account_session_connections(
                                State(s.clone()),
                                h.clone(),
                            )
                            .await
                        }
                        "importSources" => {
                            admin::account_sessions::handle_scan_import_sources(
                                State(s.clone()),
                                h.clone(),
                            )
                            .await
                        }
                        "desktopSources" => {
                            admin::desktop_import::handle_desktop_sources(
                                State(s.clone()),
                                h.clone(),
                            )
                            .await
                        }
                        "usageSnapshot" => {
                            admin::account_sessions::handle_usage_snapshot(
                                State(s.clone()),
                                h.clone(),
                            )
                            .await
                        }
                        _ => return Err((-32602, format!("Invalid params: unknown view {view}"))),
                    })
                })
            },
        ),
        td(
            "aigx_subscription_watch",
            "订阅与套餐观测",
            "只读：套餐清单/调用者自身订阅/指定用户的订阅（view 选 plans/self/userSubscriptions；后者需 user_id）",
            ReadOnly,
            "view",
            &[
                "/api/subscription/plans",
                "/api/subscription/self",
                "/api/subscription/admin/users/{id}/subscriptions",
            ],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "view": { "type": "string", "enum": ["plans", "self", "userSubscriptions"], "description": "视图种类" },
                        "user_id": { "type": "string", "description": "用户 ID（view=userSubscriptions 必填）" }
                    },
                    "required": ["view"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let view = require_arg(a, "view")?;
                    out(match view.as_str() {
                        "plans" => admin::handle_subscription_plans(State(s.clone()), h.clone()).await,
                        "self" => admin::handle_subscription_self(State(s.clone()), h.clone()).await,
                        "userSubscriptions" => {
                            let id = require_arg(a, "user_id")?;
                            admin::handle_subscription_admin_list(State(s.clone()), h.clone(), Path(id)).await
                        }
                        _ => return Err((-32602, format!("Invalid params: unknown view {view}"))),
                    })
                })
            },
        ),
        td(
            "aigx_channel_detail",
            "渠道明细",
            "只读：单渠道上游余额与健康归档（view 选 balance/healthArchive，需 channel_id；与 `aigx_account_balance`（账号池侧）分属两个对象）",
            ReadOnly,
            "channel_id",
            &["/api/channels/{id}/balance", "/api/channels/{id}/health-archive"],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "channel_id": { "type": "string", "description": "渠道 ID" },
                        "view": { "type": "string", "enum": ["balance", "healthArchive"], "description": "视图种类" }
                    },
                    "required": ["channel_id", "view"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let id = require_channel_id(a)?;
                    let view = require_arg(a, "view")?;
                    out(match view.as_str() {
                        "balance" => admin::handle_get_channel_balance(State(s.clone()), h.clone(), Path(id)).await,
                        "healthArchive" => {
                            admin::handle_channel_health_archive(State(s.clone()), h.clone(), Path(id)).await
                        }
                        _ => return Err((-32602, format!("Invalid params: unknown view {view}"))),
                    })
                })
            },
        ),
        td(
            "aigx_config_view",
            "配置现状视图",
            "只读：按 key 读一份配置/现状视图（全局热生效配置的读取面；写侧另有一批工具，本工具不改任何东西）。key 取值：settings=模型映射、limits=限额、securityPolicy=安全策略、sanitize=脱敏规则、gatewaySettings/gatewayPrompt=网关设置与系统提示词、updateSettings=更新设置、logsRetention=日志保留、queue=队列、ratelimit=限流、ratios=倍率、ipFilter=IP 黑白名单、notify=通知配置（凭据字段已脱敏）、clineHeaders=转发头、userToolsPolicy/userToolsCatalog=终端用户工具面、routing/routingDefaults/routingProfiles=路由策略、alertsRules=告警规则、pricingSyncConfig/exchangeRates=定价同步与汇率、modelMetadata=模型元数据、debug/debugTraffic=调试开关与在途抓包、reportSettings=报表时区偏移与存量口径、checkinKeepalive/checkinBenefits=签到保活与权益",
            ReadOnly,
            "key",
            &[
                "/api/settings",
                "/api/limits",
                "/api/security/policy",
                "/api/sanitize",
                "/api/gateway/settings",
                "/api/gateway/prompt",
                "/api/update/settings",
                "/api/logs/retention",
                "/api/queue",
                "/api/ratelimit/config",
                "/api/ratios",
                "/api/ip/filter",
                "/api/notify/config",
                "/api/cline-headers",
                "/api/user-tools/policy",
                "/api/user-tools/catalog",
                "/api/routing",
                "/api/routing/defaults",
                "/api/routing/profiles",
                "/api/alerts/rules",
                "/api/pricing/sync-config",
                "/api/pricing/exchange-rates",
                "/api/models/metadata",
                "/api/debug",
                "/api/debug/traffic",
                "/api/report/settings",
                "/api/checkin-center/benefits/keepalive",
                "/api/checkin-center/benefits",
            ],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "key": { "type": "string", "enum": [
                            "settings","limits","securityPolicy","sanitize","gatewaySettings","gatewayPrompt",
                            "updateSettings","logsRetention","queue","ratelimit","ratios","ipFilter","notify",
                            "clineHeaders","userToolsPolicy","userToolsCatalog","routing","routingDefaults",
                            "routingProfiles","alertsRules","pricingSyncConfig","exchangeRates","modelMetadata",
                            "debug","debugTraffic","reportSettings","checkinKeepalive","checkinBenefits"
                        ], "description": "配置视图 key" }
                    },
                    "required": ["key"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let key = require_arg(a, "key")?;
                    out(match key.as_str() {
                        "settings" => admin::handle_get_settings(State(s.clone()), h.clone()).await,
                        "limits" => admin::handle_get_limits(State(s.clone()), h.clone()).await,
                        "securityPolicy" => admin::handle_get_security_policy(State(s.clone()), h.clone()).await,
                        "sanitize" => admin::handle_get_sanitize(State(s.clone()), h.clone()).await,
                        "gatewaySettings" => admin::handle_get_gateway_settings(State(s.clone()), h.clone()).await,
                        "gatewayPrompt" => admin::handle_get_gateway_prompt(State(s.clone()), h.clone()).await,
                        "updateSettings" => {
                            admin::update::handle_get_update_settings(State(s.clone()), h.clone()).await
                        }
                        "logsRetention" => admin::handle_get_log_retention(State(s.clone()), h.clone()).await,
                        "queue" => admin::handle_get_queue(State(s.clone()), h.clone()).await,
                        "ratelimit" => admin::handle_get_ratelimit_config(State(s.clone()), h.clone()).await,
                        "ratios" => admin::handle_get_ratios(State(s.clone()), h.clone()).await,
                        "ipFilter" => admin::handle_get_ip_filter(State(s.clone()), h.clone()).await,
                        "notify" => admin::handle_get_notify_config(State(s.clone()), h.clone()).await,
                        "clineHeaders" => admin::handle_get_cline_headers(State(s.clone()), h.clone()).await,
                        "userToolsPolicy" => {
                            admin::user_tools::handle_get_user_tool_policy(State(s.clone()), h.clone()).await
                        }
                        "userToolsCatalog" => {
                            admin::user_tools::handle_get_user_tool_catalog(State(s.clone()), h.clone()).await
                        }
                        "routing" => admin::handle_get_routing(State(s.clone()), h.clone()).await,
                        "routingDefaults" => admin::handle_routing_defaults(State(s.clone()), h.clone()).await,
                        "routingProfiles" => {
                            admin::handle_get_routing_profiles(State(s.clone()), h.clone()).await
                        }
                        "alertsRules" => admin::handle_alert_rules_list(State(s.clone()), h.clone()).await,
                        "pricingSyncConfig" => {
                            admin::handle_get_price_sync_config(State(s.clone()), h.clone()).await
                        }
                        "exchangeRates" => admin::handle_get_exchange_rates(State(s.clone()), h.clone()).await,
                        "modelMetadata" => {
                            admin::handle_get_model_metadata(State(s.clone()), h.clone()).await
                        }
                        "debug" => admin::handle_get_debug(State(s.clone()), h.clone()).await,
                        "debugTraffic" => admin::handle_get_debug_traffic(State(s.clone()), h.clone()).await,
                        "reportSettings" => {
                            admin::handle_get_report_settings(State(s.clone()), h.clone()).await
                        }
                        "checkinKeepalive" => admin::handle_get_keepalive(State(s.clone()), h.clone()).await,
                        "checkinBenefits" => admin::handle_benefits_list(State(s.clone()), h.clone()).await,
                        _ => return Err((-32602, format!("Invalid params: unknown config key {key}"))),
                    })
                })
            },
        ),
        td(
            "aigx_agent_self_status",
            "助手自身状态",
            "只读：AI 运维助手自身的运行态（view 选 runtime=生效配置+disabledTools+effectiveModel、saved=设置页保存值、assistant=聊天助手工具面）。附带 pendingApprovals（当前挂起的人工审批数）——AI 看不到自己的开关与审批队列是运维盲区（#35 批1）。**不提供写侧**：助手不改自己的边界",
            ReadOnly,
            "view",
            &[
                "/api/agent/config",
                "/api/agent/settings",
                "/api/assistant/tools",
            ],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "view": { "type": "string", "enum": ["runtime", "saved", "assistant"], "description": "视图种类" }
                    },
                    "required": ["view"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let view = require_arg(a, "view")?;
                    let pending = s
                        .agent_state
                        .as_deref()
                        .map(|agent| agent.approvals.pending_count())
                        .unwrap_or(0);
                    let r = match view.as_str() {
                        "runtime" => {
                            crate::agent::api::handle_get_config(State(s.clone()), h.clone()).await
                        }
                        "saved" => admin::handle_get_agent_settings(State(s.clone()), h.clone()).await,
                        "assistant" => {
                            admin::assistant::handle_assistant_tools(State(s.clone()), h.clone()).await
                        }
                        _ => return Err((-32602, format!("Invalid params: unknown view {view}"))),
                    };
                    let r = match r {
                        Ok(Json(v)) => Json(with_pending_approvals(v, pending)),
                        e => return out(e),
                    };
                    out(Ok(r))
                })
            },
        ),
        td(
            "aigx_scheduled_task_list",
            "定时任务清单",
            "只读：全部间隔型定时任务的开关/间隔/上次与下次运行状态（触发与改配置是写侧，批 2 走审批）",
            ReadOnly,
            "",
            &["/api/scheduled-tasks"],
            no_args,
            |s, h, _a| Box::pin(async move { out(scheduled_tasks_status(s, h).await) }),
        ),
        td(
            "aigx_proxy_pool_status",
            "代理池清单",
            "只读：代理池条目与可用性统计（增删改/测活/sync-clash 属写侧，批 2）",
            ReadOnly,
            "",
            &["/api/proxy-pool"],
            no_args,
            |s, h, _a| {
                Box::pin(async move {
                    out(admin::proxy_pool_api::handle_pool_list(State(s.clone()), h.clone()).await)
                })
            },
        ),
        td(
            "aigx_cache_stats",
            "缓存命中率",
            "只读：响应缓存统计（命中率/条目数/节省量；清缓存是写侧，批 2）",
            ReadOnly,
            "",
            &["/api/cache/stats"],
            no_args,
            |s, h, _a| {
                Box::pin(async move { out(admin::handle_cache_stats(State(s.clone()), h.clone()).await) })
            },
        ),
        td(
            "aigx_storage_overview",
            "存储占用",
            "只读：存储占用总览（KV/文件/日志各段大小；备份导入导出属写侧与二进制流，不给）",
            ReadOnly,
            "",
            &["/api/storage"],
            no_args,
            |s, h, _a| {
                Box::pin(async move { out(admin::handle_storage_overview(State(s.clone()), h.clone()).await) })
            },
        ),
        td(
            "aigx_ticket_console",
            "工单台",
            "只读：管理员工单列表（可按 id/status/reply_status 过滤并分页；回复与关闭是写侧，批 2）",
            ReadOnly,
            "",
            &["/api/admin/tickets"],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "id": { "type": "string", "description": "按工单 ID 精确查" },
                        "status": { "type": "integer", "description": "工单状态过滤" },
                        "reply_status": { "type": "integer", "description": "回复状态过滤" },
                        "page": { "type": "integer", "minimum": 1 },
                        "page_size": { "type": "integer", "minimum": 1 }
                    },
                    "required": []
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let q: admin::tickets::AdminTicketQuery =
                        parse_tool_args("aigx_ticket_console", a)?;
                    out(admin::handle_admin_list_tickets(State(s.clone()), h.clone(), Query(q)).await)
                })
            },
        ),
        td(
            "aigx_pricing_gaps",
            "缺定价模型清单",
            "只读：出现在流量里但没有定价的模型（定价漂移排障入口；同步/估算/改价是写侧，批 2）",
            ReadOnly,
            "",
            &["/api/pricing/missing-models"],
            no_args,
            |s, h, _a| {
                Box::pin(async move {
                    out(admin::handle_missing_pricing_models(State(s.clone()), h.clone()).await)
                })
            },
        ),
        td(
            "aigx_token_activity",
            "令牌今日消耗",
            "只读：今日 API 令牌消耗概览（回读明文 key 的 /api/tokens/{id}/key 不给助手）",
            ReadOnly,
            "",
            &["/api/tokens/today"],
            no_args,
            |s, h, _a| {
                Box::pin(async move { out(admin::handle_tokens_today(State(s.clone()), h.clone()).await) })
            },
        ),
    ]
}
