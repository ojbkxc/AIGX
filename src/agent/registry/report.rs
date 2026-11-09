//! `report` 域工具声明（#35 批2 拆模块：从 registry.rs 机械切出，块内零改动）。
//!
//! 组装点在 [`super::build_tools`]；顺序敏感，勿单独调序。执行体复用 `super::*`。

use super::*;
use RiskLevel::ReadOnly;

pub(super) fn tools() -> Vec<ToolDef> {
    vec![
        // ── 报表（一个工具按 kind 分派 7 个报表）──
        td(
            "aigx_dashboard_report",
            "运维报表",
            "只读：运维报表（report 选 overview/models/heatmap/top_sources/token_trend/cache_rates/daily_trend；普通用户自动只看自己）",
            ReadOnly,
            "",
            &[
                "/api/dashboard/report/overview",
                "/api/dashboard/report/models",
                "/api/dashboard/report/heatmap",
                "/api/dashboard/report/top_sources",
                "/api/dashboard/report/token_trend",
                "/api/dashboard/report/cache_rates",
                "/api/dashboard/report/daily_trend",
            ],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "report": { "type": "string", "enum": ["overview", "models", "heatmap", "top_sources", "token_trend", "cache_rates", "daily_trend"], "description": "报表种类" },
                        "days": { "type": "integer", "minimum": 1, "maximum": 365, "description": "时间范围（天），默认 30" }
                    },
                    "required": ["report"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let kind = require_arg(a, "report")?;
                    let q: admin::dashboard::ReportQuery =
                        parse_tool_args("aigx_dashboard_report", a)?;
                    out(match kind.as_str() {
                        "overview" => admin::dashboard::handle_report_overview(State(s.clone()), h.clone(), Query(q)).await,
                        "models" => admin::dashboard::handle_report_models(State(s.clone()), h.clone(), Query(q)).await,
                        "heatmap" => admin::dashboard::handle_report_heatmap(State(s.clone()), h.clone(), Query(q)).await,
                        "top_sources" => admin::dashboard::handle_report_top_sources(State(s.clone()), h.clone(), Query(q)).await,
                        "token_trend" => admin::dashboard::handle_report_token_trend(State(s.clone()), h.clone(), Query(q)).await,
                        "cache_rates" => admin::dashboard::handle_report_cache_rates(State(s.clone()), h.clone(), Query(q)).await,
                        "daily_trend" => admin::dashboard::handle_report_daily_trend(State(s.clone()), h.clone(), Query(q)).await,
                        _ => {
                            return Err((-32602, format!("Invalid params: unknown report kind {kind}")));
                        }
                    })
                })
            },
        ),
    ]
}
