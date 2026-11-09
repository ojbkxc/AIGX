//! `checkin` 域工具声明（#35 批2 拆模块：从 registry.rs 机械切出，块内零改动）。
//!
//! 组装点在 [`super::build_tools`]；顺序敏感，勿单独调序。执行体复用 `super::*`。

use super::*;
use RiskLevel::{HighRisk, LowRisk, ReadOnly};

pub(super) fn tools() -> Vec<ToolDef> {
    vec![
        // ── 签到中心 ──
        td(
            "aigx_checkin_overview",
            "签到总览",
            "只读：签到中心聚合快照（今日进度/计划/台账摘要）",
            ReadOnly,
            "",
            &["/api/checkin-center"],
            no_args,
            |s, h, _a| {
                Box::pin(async move {
                    out(
                        admin::checkin_center::handle_checkin_center(State(s.clone()), h.clone())
                            .await,
                    )
                })
            },
        ),
        td(
            "aigx_checkin_run",
            "立即签到",
            "写操作（可回滚）：立即执行签到（可按账号/供应商范围）",
            LowRisk,
            "",
            &["/api/checkin-center/run"],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "account_ids": { "type": "array", "items": { "type": "string" } },
                        "providers": { "type": "array", "items": { "type": "string" } }
                    },
                    "required": []
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let body: admin::checkin_center::RunRequest =
                        parse_tool_args("aigx_checkin_run", a)?;
                    out(admin::checkin_center::handle_checkin_run(
                        State(s.clone()),
                        h.clone(),
                        Some(Json(body)),
                    )
                    .await)
                })
            },
        )
        // #34 用户域：本人每日签到（`handle_do_checkin` 内部 verify_user，只给
        // 自己入账；管理面的账号池签到走上面的 `exec`，两条执行体互不串域）。
        .user_exec(|s, h, _a| {
            Box::pin(
                async move { out(admin::handle_do_checkin(State(s.clone()), h.clone()).await) },
            )
        }),
        td(
            "aigx_checkin_records",
            "签到台账",
            "只读：签到记录台账（分页）",
            ReadOnly,
            "",
            &["/api/checkin-center/records"],
            paged,
            |s, h, a| {
                Box::pin(async move {
                    let q = crate::api::admin::mcp::parse_struct_args::<
                        admin::checkin_center::RecordsQuery,
                    >("aigx_checkin_records", a)?;
                    out(admin::checkin_center::handle_checkin_records(
                        State(s.clone()),
                        h.clone(),
                        Query(q),
                    )
                    .await)
                })
            },
        ),
        td(
            "aigx_checkin_records_clear",
            "清空签到台账",
            "高危写：清空签到记录台账（不可逆，需审批）",
            HighRisk,
            "",
            &["/api/checkin-center/records"],
            no_args,
            |s, h, _a| {
                Box::pin(async move {
                    let q = crate::api::admin::mcp::parse_struct_args::<
                        admin::checkin_center::RecordsQuery,
                    >("aigx_checkin_records_clear", &json!({}))?;
                    out(admin::checkin_center::handle_clear_checkin_records(
                        State(s.clone()),
                        h.clone(),
                        Query(q),
                    )
                    .await)
                })
            },
        ),
        td(
            "aigx_checkin_settings_get",
            "读签到设置",
            "只读：读取签到中心设置",
            ReadOnly,
            "",
            &["/api/checkin-center/settings"],
            no_args,
            |s, h, _a| {
                Box::pin(async move {
                    out(admin::checkin_center::handle_get_checkin_settings(
                        State(s.clone()),
                        h.clone(),
                    )
                    .await)
                })
            },
        ),
        td(
            "aigx_checkin_settings_set",
            "改签到设置",
            "写操作（可回滚）：修改签到中心设置",
            LowRisk,
            "",
            &["/api/checkin-center/settings"],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "enabled": { "type": "boolean" },
                        "interval": { "type": "integer" }
                    },
                    "required": []
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let body: crate::checkin::store::CheckinSettings =
                        parse_tool_args("aigx_checkin_settings_set", a)?;
                    out(admin::checkin_center::handle_update_checkin_settings(
                        State(s.clone()),
                        h.clone(),
                        Json(body),
                    )
                    .await)
                })
            },
        ),
        td(
            "aigx_checkin_schedules_list",
            "签到计划列表",
            "只读：列出定时签到计划",
            ReadOnly,
            "",
            &["/api/checkin-center/schedules"],
            no_args,
            |s, h, _a| {
                Box::pin(async move {
                    out(admin::checkin_center::handle_list_checkin_schedules(
                        State(s.clone()),
                        h.clone(),
                    )
                    .await)
                })
            },
        ),
        td(
            "aigx_checkin_schedule_upsert",
            "新增/改签到计划",
            "写操作（可回滚）：新增或修改定时签到计划",
            LowRisk,
            "id",
            &["/api/checkin-center/schedules"],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "id": { "type": "string" },
                        "label": { "type": "string" },
                        "enabled": { "type": "boolean" },
                        "time": { "type": "string", "description": "HH:MM" },
                        "providers": { "type": "array", "items": { "type": "string" } }
                    },
                    "required": ["label", "time"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let body: admin::checkin_center::ScheduleBody =
                        parse_tool_args("aigx_checkin_schedule_upsert", a)?;
                    out(admin::checkin_center::handle_upsert_checkin_schedule(
                        State(s.clone()),
                        h.clone(),
                        Json(body),
                    )
                    .await)
                })
            },
        ),
        td(
            "aigx_checkin_schedule_remove",
            "删除签到计划",
            "写操作（可回滚）：删除定时签到计划",
            LowRisk,
            "id",
            &["/api/checkin-center/schedules"],
            || {
                json!({
                    "type": "object",
                    "properties": { "id": { "type": "string" } },
                    "required": ["id"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let body: admin::checkin_center::ScheduleIdBody =
                        parse_tool_args("aigx_checkin_schedule_remove", a)?;
                    out(admin::checkin_center::handle_remove_checkin_schedule(
                        State(s.clone()),
                        h.clone(),
                        Json(body),
                    )
                    .await)
                })
            },
        ),
        td(
            "aigx_checkin_schedule_run",
            "立即执行签到计划",
            "写操作（可回滚）：立即执行指定定时签到计划",
            LowRisk,
            "id",
            &["/api/checkin-center/schedules"],
            || {
                json!({
                    "type": "object",
                    "properties": { "id": { "type": "string" } },
                    "required": ["id"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let body: admin::checkin_center::ScheduleIdBody =
                        parse_tool_args("aigx_checkin_schedule_run", a)?;
                    out(admin::checkin_center::handle_run_checkin_schedule(
                        State(s.clone()),
                        h.clone(),
                        Json(body),
                    )
                    .await)
                })
            },
        ),
    ]
}
