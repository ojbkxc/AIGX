//! `update_ops` 域工具声明（#35 批2 拆模块：从 registry.rs 机械切出，块内零改动）。
//!
//! 组装点在 [`super::build_tools`]；顺序敏感，勿单独调序。执行体复用 `super::*`。

use super::*;
use RiskLevel::{HighRisk, LowRisk, ReadOnly};

pub(super) fn tools() -> Vec<ToolDef> {
    vec![
        // ── 服务器自动更新 ──
        td(
            "aigx_update_status",
            "更新状态",
            "只读：服务器更新状态（版本/下载进度/待重启）",
            ReadOnly,
            "",
            &["/api/update/status"],
            no_args,
            |s, h, _a| {
                Box::pin(async move {
                    out(admin::update::handle_update_status(State(s.clone()), h.clone()).await)
                })
            },
        ),
        td(
            "aigx_check_update",
            "检查更新",
            "写操作（可回滚）：立即检查 GitHub Release（只读网络请求）",
            LowRisk,
            "",
            &["/api/update/check"],
            no_args,
            |s, h, _a| {
                Box::pin(async move {
                    out(admin::update::handle_update_check(State(s.clone()), h.clone()).await)
                })
            },
        ),
        td(
            "aigx_apply_update",
            "应用更新",
            "高危写：下载并应用更新（需审批，会重启服务）",
            HighRisk,
            "",
            &["/api/update/apply"],
            no_args,
            |s, h, _a| {
                Box::pin(async move {
                    // handler 新增了可选 body（force 标记）—— Agent 走默认路径
                    out(admin::update::handle_update_apply(State(s.clone()), h.clone(), None).await)
                })
            },
        ),
        td(
            "aigx_restart_service",
            "重启服务",
            "高危写：重启网关服务（需审批）",
            HighRisk,
            "",
            &["/api/update/restart"],
            no_args,
            |s, h, _a| {
                Box::pin(async move {
                    out(admin::update::handle_update_restart(State(s.clone()), h.clone()).await)
                })
            },
        ),
        td(
            "aigx_rollback_update",
            "回滚更新",
            "高危写：回滚到上一版本（需审批）",
            HighRisk,
            "",
            &["/api/update/rollback"],
            no_args,
            |s, h, _a| {
                Box::pin(async move {
                    out(admin::update::handle_update_rollback(State(s.clone()), h.clone()).await)
                })
            },
        ),
    ]
}
