//! `account_sessions` 域工具声明（#35 批2 拆模块：从 registry.rs 机械切出，块内零改动）。
//!
//! 组装点在 [`super::build_tools`]；顺序敏感，勿单独调序。执行体复用 `super::*`。

use super::*;
use RiskLevel::{HighRisk, LowRisk, ReadOnly};

pub(super) fn tools() -> Vec<ToolDef> {
    vec![
        // ── 账号池（账号会话）──
        td(
            "aigx_list_account_sessions",
            "列出账号池账号",
            "只读：列出账号池账号（channel_id/search 过滤）",
            ReadOnly,
            "",
            &["/api/account-sessions"],
            || json!({ "type":"object","properties":{ "channel_id":{"type":"string"}, "search":{"type":"string"} }, "required":[] }),
            |s, h, a| {
                Box::pin(async move {
                    let q = crate::api::admin::mcp::args_to_string_map(a);
                    out(admin::account_sessions::handle_list_account_sessions(
                        State(s.clone()),
                        h.clone(),
                        Query(q),
                    )
                    .await)
                })
            },
        ),
        td(
            "aigx_account_session_stats",
            "账号池统计",
            "只读：账号池统计（成功率/冷却/并发）",
            ReadOnly,
            "",
            &["/api/account-sessions/stats"],
            no_args,
            |s, h, _a| {
                Box::pin(async move {
                    out(admin::account_sessions::handle_account_session_stats(
                        State(s.clone()),
                        h.clone(),
                    )
                    .await)
                })
            },
        ),
        td(
            "aigx_account_session_test",
            "单号探活",
            "写操作（可回滚）：单号探活（走真实推理链路）",
            LowRisk,
            "account_id",
            &["/api/account-sessions/{id}/test"],
            account_id_args,
            |s, h, a| {
                Box::pin(async move {
                    let id = require_arg(a, "account_id")?;
                    out(admin::account_sessions::handle_test_account_session(
                        State(s.clone()),
                        h.clone(),
                        Path(id),
                    )
                    .await)
                })
            },
        ),
        td(
            "aigx_account_session_enable",
            "启用账号",
            "写操作（可回滚）：启用账号池账号",
            LowRisk,
            "account_id",
            &["/api/account-sessions/{id}"],
            account_id_args,
            |s, h, a| {
                Box::pin(async move {
                    let id = require_arg(a, "account_id")?;
                    out(admin::account_sessions::handle_patch_account_session(
                        State(s.clone()),
                        h.clone(),
                        Path(id),
                        Json(admin::account_sessions::AccountSessionPatch::status_only(
                            "enabled",
                        )),
                    )
                    .await)
                })
            },
        ),
        td(
            "aigx_account_session_disable",
            "禁用账号",
            "写操作（可回滚）：禁用账号池账号",
            LowRisk,
            "account_id",
            &["/api/account-sessions/{id}"],
            account_id_args,
            |s, h, a| {
                Box::pin(async move {
                    let id = require_arg(a, "account_id")?;
                    out(admin::account_sessions::handle_patch_account_session(
                        State(s.clone()),
                        h.clone(),
                        Path(id),
                        Json(admin::account_sessions::AccountSessionPatch::status_only(
                            "disabled",
                        )),
                    )
                    .await)
                })
            },
        ),
        td(
            "aigx_account_session_reset_cooldown",
            "清除账号冷却",
            "写操作（可回滚）：清除账号冷却与最近错误",
            LowRisk,
            "account_id",
            &["/api/account-sessions/{id}/reset-cooldown"],
            account_id_args,
            |s, h, a| {
                Box::pin(async move {
                    let id = require_arg(a, "account_id")?;
                    out(
                        admin::account_sessions::handle_reset_account_session_cooldowns(
                            State(s.clone()),
                            h.clone(),
                            Path(id),
                            // Agent 侧一律全清（不带按模型清除的 body）
                            None,
                        )
                        .await,
                    )
                })
            },
        ),
        td(
            "aigx_account_session_add",
            "新增账号",
            "写操作（可回滚）：新增账号池账号（channel_id/provider/api_key…）",
            LowRisk,
            "name",
            &["/api/account-sessions"],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "channel_id": { "type": "string" },
                        "provider": { "type": "string" },
                        "api_key": { "type": "string", "description": "登录态/密钥凭证（必填）" },
                        "name": { "type": "string" },
                        "base_url": { "type": "string" },
                        "proxy": { "type": "string" },
                        "priority": { "type": "integer" },
                        "weight": { "type": "integer" },
                        "max_concurrency": { "type": "integer" },
                        "models": { "type": "array", "items": { "type": "string" } }
                    },
                    "required": ["channel_id", "api_key"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    // 账号必须有凭证，空 key 建号没有意义（前端表单同样禁止）
                    require_arg(a, "api_key")?;
                    let body: admin::account_sessions::AccountSessionRequest =
                        parse_tool_args("aigx_account_session_add", a)?;
                    out(admin::account_sessions::handle_create_account_session(
                        State(s.clone()),
                        h.clone(),
                        Json(body),
                    )
                    .await)
                })
            },
        ),
        td(
            "aigx_account_session_update",
            "更新账号",
            "写操作（可回滚）：更新账号属性（name/status/priority/weight/max_concurrency）",
            LowRisk,
            "account_id",
            &["/api/account-sessions/{id}"],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "account_id": { "type": "string" },
                        "name": { "type": "string" },
                        "status": { "type": "string" },
                        "priority": { "type": "integer" },
                        "weight": { "type": "integer" },
                        "max_concurrency": { "type": "integer" }
                    },
                    "required": ["account_id"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let id = require_arg(a, "account_id")?;
                    let body: admin::account_sessions::AccountSessionPatch =
                        parse_tool_args("aigx_account_session_update", a)?;
                    out(admin::account_sessions::handle_patch_account_session(
                        State(s.clone()),
                        h.clone(),
                        Path(id),
                        Json(body),
                    )
                    .await)
                })
            },
        ),
        td(
            "aigx_account_session_delete",
            "删除账号",
            "高危写：删除账号池账号（不可逆，需审批）",
            HighRisk,
            "account_id",
            &["/api/account-sessions/{id}"],
            account_id_args,
            |s, h, a| {
                Box::pin(async move {
                    let id = require_arg(a, "account_id")?;
                    out(admin::account_sessions::handle_delete_account_session(
                        State(s.clone()),
                        h.clone(),
                        Path(id),
                    )
                    .await)
                })
            },
        ),
        td(
            "aigx_account_session_bulk_import",
            "批量导入账号",
            "高危写：批量导入账号池账号（每行一个凭证，需审批）",
            HighRisk,
            "channel_id",
            &["/api/account-sessions/bulk"],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "channel_id": { "type": "string" },
                        "provider": { "type": "string" },
                        "credentials": { "type": "string", "description": "多行凭证，每行一个" }
                    },
                    "required": ["channel_id", "credentials"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let body: admin::account_sessions::BulkImportRequest =
                        parse_tool_args("aigx_account_session_bulk_import", a)?;
                    out(
                        admin::account_sessions::handle_bulk_import_account_sessions(
                            State(s.clone()),
                            h.clone(),
                            Json(body),
                        )
                        .await,
                    )
                })
            },
        ),
    ]
}
