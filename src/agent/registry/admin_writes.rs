//! `admin_writes` 域工具声明（#35 批2 拆模块：从 registry.rs 机械切出，块内零改动）。
//!
//! 组装点在 [`super::build_tools`]；顺序敏感，勿单独调序。执行体复用 `super::*`。

use super::*;
use RiskLevel::{HighRisk, LowRisk};

pub(super) fn tools() -> Vec<ToolDef> {
    vec![
        // ── 低危写（渠道/密钥）──
        td(
            "aigx_channel_enable",
            "启用渠道",
            "写操作（可回滚）：启用渠道",
            LowRisk,
            "channel_id",
            &["/api/channels/{id}"],
            channel_id_args,
            |s, h, a| {
                Box::pin(async move {
                    let id = require_arg(a, "channel_id")?;
                    out(admin::handle_patch_channel(
                        State(s.clone()),
                        h.clone(),
                        Path(id),
                        Json(json!({ "enabled": true })),
                    )
                    .await)
                })
            },
        ),
        td(
            "aigx_channel_disable",
            "禁用渠道",
            "写操作（可回滚）：禁用渠道",
            LowRisk,
            "channel_id",
            &["/api/channels/{id}"],
            channel_id_args,
            |s, h, a| {
                Box::pin(async move {
                    let id = require_arg(a, "channel_id")?;
                    out(admin::handle_patch_channel(
                        State(s.clone()),
                        h.clone(),
                        Path(id),
                        Json(json!({ "enabled": false })),
                    )
                    .await)
                })
            },
        ),
        td(
            "aigx_channel_test",
            "测试渠道连通性",
            "写操作（可回滚）：测试渠道连通性并落库结果",
            LowRisk,
            "channel_id",
            &["/api/channels/{id}/test"],
            channel_id_args,
            |s, h, a| {
                Box::pin(async move {
                    let id = require_arg(a, "channel_id")?;
                    out(admin::handle_test_channel(State(s.clone()), h.clone(), Path(id)).await)
                })
            },
        ),
        td(
            "aigx_reset_circuit_breaker",
            "重置渠道熔断器",
            "写操作（可回滚）：重置渠道熔断器",
            LowRisk,
            "channel_id",
            &["/api/channels/{id}/reset-circuit"],
            channel_id_args,
            |s, h, a| {
                Box::pin(async move {
                    let id = require_arg(a, "channel_id")?;
                    out(admin::handle_reset_channel_circuit(
                        State(s.clone()),
                        h.clone(),
                        Path(id),
                    )
                    .await)
                })
            },
        ),
        td(
            "aigx_channel_add",
            "新增渠道",
            "写操作（可回滚）：新增渠道（name/base_url/api_key/type）",
            LowRisk,
            "name",
            &["/api/channels"],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "渠道名" },
                        "base_url": { "type": "string", "description": "上游 base URL" },
                        "api_key": { "type": "string", "description": "上游 API key" },
                        "channel_type": { "type": "string", "description": "openai_compatible/anthropic/gemini/zai/cloudflare/account（account = 账号型渠道，凭证在账号池里加）" }
                    },
                    "required": ["name", "base_url", "api_key"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let body: admin::channels::ChannelRequest = parse_tool_args("aigx_channel_add", a)?;
                    out(admin::handle_add_channel(State(s.clone()), h.clone(), Json(body)).await)
                })
            },
        ),
        td(
            "aigx_channel_update",
            "部分更新渠道",
            "写操作（可回滚）：部分更新渠道字段（name/base_url/priority/weight/models/enabled…）",
            LowRisk,
            "channel_id",
            &["/api/channels/{id}"],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "channel_id": { "type": "string", "description": "渠道 ID" },
                        "name": { "type": "string" },
                        "base_url": { "type": "string" },
                        "priority": { "type": "integer" },
                        "weight": { "type": "integer" },
                        "models": { "type": "array", "items": { "type": "string" } },
                        "enabled": { "type": "boolean" }
                    },
                    "required": ["channel_id"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let id = require_channel_id(a)?;
                    let mut patch = a.clone();
                    if let Some(obj) = patch.as_object_mut() {
                        obj.remove("channel_id");
                    }
                    out(admin::handle_patch_channel(
                        State(s.clone()),
                        h.clone(),
                        Path(id),
                        Json(patch),
                    )
                    .await)
                })
            },
        ),
        td(
            "aigx_key_add",
            "新增访问令牌",
            "写操作（可回滚）：新增 API 访问令牌（POST /api/tokens；#35 批1 收敛：原指向无路由的 \
             legacy /api/keys，现与 `aigx_list_tokens` 同数据源 api_key_store）",
            LowRisk,
            "name",
            &["/api/tokens"],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "密钥名" },
                        "user_id": { "type": "string", "description": "归属用户 ID（可空）" },
                        "group": { "type": "string", "description": "分组（可空）" }
                    },
                    "required": ["name"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let body: admin::tokens::KeyRequest = parse_tool_args("aigx_key_add", a)?;
                    out(admin::handle_add_token(State(s.clone()), h.clone(), Json(body)).await)
                })
            },
        ),
        // ── 高危写（审批矩阵人工确认）──
        td(
            "aigx_user_delete",
            "删除用户",
            "高危写：删除用户（不可逆，需审批）",
            HighRisk,
            "user_id",
            &["/api/users/{id}"],
            || {
                json!({
                    "type": "object",
                    "properties": { "user_id": { "type": "string", "description": "用户 ID" } },
                    "required": ["user_id"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let id = require_arg(a, "user_id")?;
                    out(admin::handle_delete_user(State(s.clone()), h.clone(), Path(id)).await)
                })
            },
        ),
        td(
            "aigx_user_manage",
            "启用/禁用用户",
            "高危写：启用/禁用用户（需审批）",
            HighRisk,
            "user_id",
            &["/api/users/manage"],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "user_id": { "type": "string", "description": "用户 ID" },
                        "action": { "type": "string", "enum": ["enable", "disable"], "description": "enable/disable" }
                    },
                    "required": ["user_id", "action"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let body: admin::users::ManageUserRequest = parse_tool_args("aigx_user_manage", a)?;
                    out(admin::handle_manage_user(State(s.clone()), h.clone(), Json(body)).await)
                })
            },
        ),
        td(
            "aigx_channel_delete",
            "删除渠道",
            "高危写：删除渠道（不可逆，需审批）",
            HighRisk,
            "channel_id",
            &["/api/channels/{id}"],
            channel_id_args,
            |s, h, a| {
                Box::pin(async move {
                    let id = require_arg(a, "channel_id")?;
                    // Agent 路径免密码：直调 core（删除 + 审计），不走 HTTP handler 的
                    // 密码二次认证——高危工具已经审批矩阵人工确认（runner.rs），
                    // 审批本身即同等强度的身份复核；此处仍有 verify_admin 兜底。
                    out(admin::channels::delete_channel_core(s, h, &id).await)
                })
            },
        ),
        td(
            "aigx_order_delete",
            "删除订单",
            "高危写：删除订单（不可逆，需审批）",
            HighRisk,
            "trade_no",
            &["/api/orders/{id}"],
            || {
                json!({
                    "type": "object",
                    "properties": { "trade_no": { "type": "string", "description": "订单号" } },
                    "required": ["trade_no"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let trade_no = require_arg(a, "trade_no")?;
                    out(admin::handle_delete_order(State(s.clone()), h.clone(), Path(trade_no)).await)
                })
            },
        ),
        td(
            "aigx_pricing_upsert",
            "新增/更新模型定价（旧版）",
            "高危写：新增/更新模型定价（需审批；推荐 aigx_model_price_upsert）",
            HighRisk,
            "model_name",
            &["/api/pricing"],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "model_name": { "type": "string" },
                        "input_price": { "type": "number" },
                        "output_price": { "type": "number" },
                        "price_type": { "type": "string", "description": "token/image 等" }
                    },
                    "required": ["model_name", "input_price", "output_price", "price_type"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let body: admin::pricing::PriceRequest = parse_tool_args("aigx_pricing_upsert", a)?;
                    out(admin::handle_add_pricing(State(s.clone()), h.clone(), Json(body)).await)
                })
            },
        ),
        td(
            "aigx_pricing_delete",
            "删除模型定价（旧版）",
            "高危写：删除模型定价（需审批；推荐 aigx_model_price_remove）",
            HighRisk,
            "model_name",
            &["/api/pricing/{id}"],
            || {
                json!({
                    "type": "object",
                    "properties": { "model_name": { "type": "string" } },
                    "required": ["model_name"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let model_name = require_arg(a, "model_name")?;
                    out(admin::handle_delete_pricing(
                        State(s.clone()),
                        h.clone(),
                        Path(model_name),
                    )
                    .await)
                })
            },
        ),
        td(
            "aigx_logs_cleanup",
            "清理过期日志",
            "高危写：按保留天数清理请求/审计日志（需审批）",
            HighRisk,
            "",
            &["/api/logs/cleanup"],
            || {
                json!({
                    "type": "object",
                    "properties": { "days": { "type": "integer", "minimum": 1, "description": "保留天数" } },
                    "required": ["days"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let days = a.get("days").and_then(|v| v.as_u64());
                    out(admin::handle_cleanup_logs(
                        State(s.clone()),
                        h.clone(),
                        Some(Json(admin::logs::LogCleanupBody { days })),
                    )
                    .await)
                })
            },
        ),
        td(
            "aigx_key_delete",
            "删除访问令牌",
            "高危写：删除 API 访问令牌（DELETE /api/tokens/{id}；不可逆，需审批。#35 批1 收敛：原指向无路由的 legacy /api/keys/{id}）",
            HighRisk,
            "key_id",
            &["/api/tokens/{id}"],
            || {
                json!({
                    "type": "object",
                    "properties": { "key_id": { "type": "string", "description": "令牌 ID" } },
                    "required": ["key_id"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let id = require_arg(a, "key_id")?;
                    out(admin::handle_delete_token(State(s.clone()), h.clone(), Path(id)).await)
                })
            },
        ),
        td(
            "aigx_group_upsert",
            "新增/更新分组",
            "高危写：新增/更新用户分组倍率（需审批）",
            HighRisk,
            "name",
            &["/api/groups"],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "name": { "type": "string" },
                        "ratio": { "type": "number" }
                    },
                    "required": ["name", "ratio"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let body: admin::legacy::GroupRequest = parse_tool_args("aigx_group_upsert", a)?;
                    out(admin::handle_upsert_group(State(s.clone()), h.clone(), Json(body)).await)
                })
            },
        ),
    ]
}
