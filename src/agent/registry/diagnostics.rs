//! `diagnostics` 域工具声明（#35 批2 拆模块：从 registry.rs 机械切出，块内零改动）。
//!
//! 组装点在 [`super::build_tools`]；顺序敏感，勿单独调序。执行体复用 `super::*`。

use super::*;
use RiskLevel::ReadOnly;

pub(super) fn tools() -> Vec<ToolDef> {
    vec![
        // ── 诊断 ×3 ──
        td(
            "aigx_diagnostics_summary",
            "系统体检摘要",
            "只读：系统体检摘要（渠道统计/今日用量/24h 错误概况/运行信息）",
            ReadOnly,
            "",
            &["/api/diagnostics/summary"],
            no_args,
            |s, h, _a| {
                Box::pin(async move {
                    out(admin::handle_diagnostics_summary(State(s.clone()), h.clone()).await)
                })
            },
        ),
        td(
            "aigx_diagnostics_channels",
            "启用渠道批量测活",
            "只读：启用渠道批量测活（并发探测，约需 30 秒）",
            ReadOnly,
            "",
            &["/api/diagnostics/channels"],
            no_args,
            |s, h, _a| {
                Box::pin(async move {
                    out(admin::handle_diagnostics_channels(State(s.clone()), h.clone()).await)
                })
            },
        ),
        td(
            "aigx_diagnostics_breakers",
            "熔断器快照",
            "只读：熔断器状态快照（三态/失败计数/冷却剩余）",
            ReadOnly,
            "",
            &["/api/diagnostics/breakers"],
            no_args,
            |s, h, _a| {
                Box::pin(async move {
                    out(admin::handle_diagnostics_breakers(State(s.clone()), h.clone()).await)
                })
            },
        ),
    ]
}
