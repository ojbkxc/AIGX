//! 批2 并行单元 U-D（#21：logs/cache/traffic 清理族 + agg 重算）的工具声明文件 —— **文件独占**：仅本单元 owner 编辑。
//!
//! 四条工具：
//! - `aigx_logs_purge`（**HIGH**）：按 `before` 时间窗 + 单次限量定量清理 request/audit 日志，
//!   或终止一条在途请求。**删日志＝销毁证据** ⇒ 只做 `clear` 语义但**强制 `before`+`max`**，
//!   且清理前先写一条 `logs.purge` audit（`before <= now` 保证该记录必落在删除窗口外、必然存活）；
//!   按显式 id 的选择性删除**不作工具**（lead 裁定 A）。
//! - `aigx_agg_maintenance`（**HIGH**）：日聚合重建（rebuild）/ 报表时区迁移（tz-migrate）——
//!   重写聚合表、影响域＝全库（资金判据第 4 条同型），故 HIGH。
//! - `aigx_cache_clear`（LOW）：清可再生缓存（响应缓存）/ 更新包产物（保留当前版本）。
//! - `aigx_traffic_clear`（LOW）：清调试报文环形缓冲 / 解除会话粘滞。
//!
//! 边界：LOW 两条只调**已 pub** 的 handler（`cache.rs`/`routing.rs`/`update.rs`/`debug.rs`），
//! 本单元**不修改**这些非独占文件。执行体复用 `super::*`；assembler 在 [`super::build_tools`]，
//! owner 不回改 `mod.rs`。

use super::*;
use RiskLevel::{HighRisk, LowRisk};

pub(super) fn tools() -> Vec<ToolDef> {
    vec![
        // ── G5 清理/删除：销毁证据 ⇒ HIGH + 强制 before/限量 + 前置 audit ──
        td(
            "aigx_logs_purge",
            "清理日志",
            "高危写：按 before 时间窗定量清理请求/审计日志（clear-requests/clear-audits），或终止一条在途请求（terminate-inflight）。\
             删日志＝销毁证据：只删 created_at < before 的行、单次限量（默认 1000/硬上限 10000），且清理前先写一条 logs.purge 审计；\
             不接受按 id 的选择性删除。不可逆，需审批。",
            HighRisk,
            "action",
            &[
                "/api/logs/requests/clear",
                "/api/logs/audits/clear",
                "/api/logs/inflight/{id}/terminate",
            ],
            logs_purge_schema,
            |s, h, a| {
                Box::pin(async move {
                    let action = require_arg(a, "action")?;
                    match action.as_str() {
                        "clear-requests" | "clear-audits" => {
                            let before =
                                a.get("before").and_then(|v| v.as_i64()).ok_or_else(|| {
                                    (
                                        -32602,
                                        "clear-requests/clear-audits 需要 before（unix 秒，须 ≤ 当前时间）"
                                            .to_string(),
                                    )
                                })?;
                            let max = a
                                .get("max")
                                .and_then(|v| v.as_u64())
                                .map(|v| v as usize)
                                .unwrap_or(admin::logs::PURGE_DEFAULT_MAX);
                            let target = if action == "clear-requests" {
                                admin::logs::PurgeTarget::Requests
                            } else {
                                admin::logs::PurgeTarget::Audits
                            };
                            out(admin::logs::purge_logs_before(s, h, target, before, max)
                                .await
                                .map(Json))
                        }
                        "terminate-inflight" => {
                            let id = require_arg(a, "id")?;
                            out(admin::handle_terminate_inflight(
                                State(s.clone()),
                                h.clone(),
                                Path(id),
                            )
                            .await)
                        }
                        other => Err((
                            -32602,
                            format!(
                                "Invalid action for aigx_logs_purge: {other} \
                                 (expect clear-requests/clear-audits/terminate-inflight)"
                            ),
                        )),
                    }
                })
            },
        ),
        // ── G4 聚合重算：重写全库聚合，HIGH ──
        td(
            "aigx_agg_maintenance",
            "日聚合维护",
            "高危写：日聚合重建（rebuild）或报表时区迁移（tz-migrate）。重写全库聚合表、影响域＝全库\
             （资金判据第 4 条同型；tz-migrate 需先在「设置→报表」保存目标偏移），需审批。",
            HighRisk,
            "action",
            &["/api/logs/agg/rebuild", "/api/logs/agg/tz-migrate"],
            agg_maintenance_schema,
            |s, h, a| {
                Box::pin(async move {
                    match require_arg(a, "action")?.as_str() {
                        "rebuild" => {
                            out(admin::handle_agg_rebuild(State(s.clone()), h.clone()).await)
                        }
                        "tz-migrate" => {
                            let body = admin::logs::TzMigrateRequest {
                                offset_from: a
                                    .get("offset_from")
                                    .and_then(|v| v.as_i64())
                                    .map(|v| v as i32),
                                offset_to: a
                                    .get("offset_to")
                                    .and_then(|v| v.as_i64())
                                    .map(|v| v as i32),
                            };
                            out(admin::handle_agg_tz_migrate(
                                State(s.clone()),
                                h.clone(),
                                Json(body),
                            )
                            .await)
                        }
                        other => Err((
                            -32602,
                            format!(
                                "Invalid action for aigx_agg_maintenance: {other} \
                                 (expect rebuild/tz-migrate)"
                            ),
                        )),
                    }
                })
            },
        ),
        // ── G5 清理（LOW）：可再生缓存/产物，不碰持久业务数据 ──
        td(
            "aigx_cache_clear",
            "清理缓存",
            "写操作（可回滚）：response-cache=清空响应缓存（命中即重建）；\
             update-artifacts=清理已下载的更新包/回滚候选（保留当前版本，可重新下载）。\
             不碰任何持久业务数据，故 LowRisk。",
            LowRisk,
            "action",
            &["/api/cache/clear", "/api/update/cleanup-artifacts"],
            cache_clear_schema,
            |s, h, a| {
                Box::pin(async move {
                    match require_arg(a, "action")?.as_str() {
                        "response-cache" => {
                            out(admin::handle_cache_clear(State(s.clone()), h.clone()).await)
                        }
                        "update-artifacts" => {
                            out(admin::update::handle_update_cleanup_artifacts(
                                State(s.clone()),
                                h.clone(),
                            )
                            .await)
                        }
                        other => Err((
                            -32602,
                            format!(
                                "Invalid action for aigx_cache_clear: {other} \
                                 (expect response-cache/update-artifacts)"
                            ),
                        )),
                    }
                })
            },
        ),
        // ── G5 清理（LOW）：调试环形缓冲 / 会话粘滞，均可再生 ──
        td(
            "aigx_traffic_clear",
            "清理流量与粘滞",
            "写操作（可回滚）：debug-traffic=清空上游原始报文环形缓冲（仅调试用、可再生）；\
             route-sticky=解除会话粘滞路由（全部或指定 id，下次请求重新选路）。不碰持久业务数据，故 LowRisk。",
            LowRisk,
            "action",
            &["/api/debug/traffic", "/api/routing/sticky/clear"],
            traffic_clear_schema,
            |s, h, a| {
                Box::pin(async move {
                    match require_arg(a, "action")?.as_str() {
                        "debug-traffic" => {
                            out(admin::handle_clear_debug_traffic(State(s.clone()), h.clone())
                                .await)
                        }
                        "route-sticky" => {
                            let body = admin::routing::RouteStickyClearRequest {
                                id: a.get("id").and_then(|v| v.as_str()).map(str::to_string),
                                all: a.get("all").and_then(|v| v.as_bool()),
                            };
                            out(admin::handle_clear_route_sticky(
                                State(s.clone()),
                                h.clone(),
                                Json(body),
                            )
                            .await)
                        }
                        other => Err((
                            -32602,
                            format!(
                                "Invalid action for aigx_traffic_clear: {other} \
                                 (expect debug-traffic/route-sticky)"
                            ),
                        )),
                    }
                })
            },
        ),
    ]
}

// ── schema ──────────────────────────────────────────────────────────

fn logs_purge_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "action": { "type": "string", "enum": ["clear-requests", "clear-audits", "terminate-inflight"],
                "description": "clear-requests/clear-audits=按 before 窗口定量清理（先写 audit 再删）；terminate-inflight=终止一条在途请求" },
            "before": { "type": "integer",
                "description": "清理窗口上界（unix 秒，必须 ≤ 当前时间）；只删 created_at < before 的日志。clear-* 必填" },
            "max": { "type": "integer", "minimum": 1, "maximum": 10000,
                "description": "单次清理条数上限（默认 1000，硬上限 10000）" },
            "id": { "type": "string", "description": "terminate-inflight 的在途请求 ID" }
        },
        "required": ["action"]
    })
}

fn agg_maintenance_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "action": { "type": "string", "enum": ["rebuild", "tz-migrate"],
                "description": "rebuild=按当前口径全量重算历史日聚合；tz-migrate=按目标时区偏移重算存量聚合（需先保存目标偏移）" },
            "offset_from": { "type": "integer",
                "description": "tz-migrate：旧口径 UTC 偏移小时（缺省 0 = 生产现状）" },
            "offset_to": { "type": "integer",
                "description": "tz-migrate：新口径 UTC 偏移小时（缺省=当前生效口径）" }
        },
        "required": ["action"]
    })
}

fn cache_clear_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "action": { "type": "string", "enum": ["response-cache", "update-artifacts"],
                "description": "response-cache=清空响应缓存；update-artifacts=清理更新包/回滚候选（保留当前版本）" }
        },
        "required": ["action"]
    })
}

fn traffic_clear_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "action": { "type": "string", "enum": ["debug-traffic", "route-sticky"],
                "description": "debug-traffic=清空上游原始报文环形缓冲；route-sticky=解除会话粘滞路由" },
            "id": { "type": "string", "description": "route-sticky：要解除的粘滞 id（留空 + all=true = 全部解除）" },
            "all": { "type": "boolean", "description": "route-sticky：是否解除全部（默认 false）" }
        },
        "required": ["action"]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 「列」侧：四条工具声明台账——name/risk/target_key/covers 逐条钉死；
    /// 高危不得进 MCP 面，低危必须进（MCP 只暴露非高危）。
    #[test]
    fn ud_tools_declared_with_expected_risk_and_covers() {
        let cases: Vec<(&str, RiskLevel, Vec<&str>)> = vec![
            (
                "aigx_logs_purge",
                RiskLevel::HighRisk,
                vec![
                    "/api/logs/requests/clear",
                    "/api/logs/audits/clear",
                    "/api/logs/inflight/{id}/terminate",
                ],
            ),
            (
                "aigx_agg_maintenance",
                RiskLevel::HighRisk,
                vec!["/api/logs/agg/rebuild", "/api/logs/agg/tz-migrate"],
            ),
            (
                "aigx_cache_clear",
                RiskLevel::LowRisk,
                vec!["/api/cache/clear", "/api/update/cleanup-artifacts"],
            ),
            (
                "aigx_traffic_clear",
                RiskLevel::LowRisk,
                vec!["/api/debug/traffic", "/api/routing/sticky/clear"],
            ),
        ];
        for (name, risk, covers) in cases {
            let t = find_tool(name).unwrap_or_else(|| panic!("{name} 未注册"));
            assert_eq!(t.risk, risk, "{name} risk 口径");
            assert_eq!(t.target_key, "action", "{name} target_key");
            for c in &covers {
                assert!(t.covers.contains(c), "{name} 缺 covers {c}");
            }
            let in_mcp = mcp_tools().any(|m| m.name == name);
            assert_eq!(
                in_mcp,
                risk != RiskLevel::HighRisk,
                "{name} 的 MCP 可见性与 risk 不符（高危必须不可见）"
            );
        }
    }

    /// 「调」侧（执行体源锚定，防「列对了、执行体接错 handler」）：四条工具每个 action
    /// 都必须真的接到对应 handler 符号上，且删日志族必须走 [`admin::logs::purge_logs_before`]
    /// 这条带 before/限量/前置 audit 的唯一入口。
    ///
    /// 单测不构造 `AppState`，故用源码锚定（同 `exec_entries_are_structurally_gated` 手法）。
    /// needle 运行时拼接：本测试自己被 `include_str!` 扫到，写字面量会自匹配。
    #[test]
    fn ud_exec_bodies_route_to_expected_handlers() {
        let src = include_str!("w_logs.rs");
        let count = |a: &str, b: &str| src.matches(&[a, b].concat()).count();
        // clear 语义只走带约束的 purge 入口
        assert_eq!(
            count("admin::logs::purge_logs", "_before("),
            1,
            "clear-* 必须且只能走 purge_logs_before（before+限量+前置 audit 的唯一入口）"
        );
        // 各 action 的落点
        for (a, b, label) in [
            (
                "admin::handle_terminate",
                "_inflight(",
                "terminate-inflight",
            ),
            ("admin::handle_agg", "_rebuild(", "agg rebuild"),
            ("admin::handle_agg_tz", "_migrate(", "agg tz-migrate"),
            ("admin::handle_cache", "_clear(", "cache clear"),
            (
                "admin::update::handle_update_cleanup",
                "_artifacts(",
                "update-artifacts",
            ),
            ("admin::handle_clear_debug", "_traffic(", "debug-traffic"),
            ("admin::handle_clear_route", "_sticky(", "route-sticky"),
        ] {
            assert!(count(a, b) >= 1, "{label} 执行体没接到预期 handler 符号");
        }
        // 裁定 A：绝不把按 id 的选择性删除 handler 接进来
        for (a, b) in [
            ("admin::handle_delete_request", "_logs("),
            ("admin::handle_delete_audit", "_logs("),
        ] {
            assert_eq!(count(a, b), 0, "裁定 A：不得接按 id 的选择性删除 handler");
        }
    }
}
