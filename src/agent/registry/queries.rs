//! `queries` 域工具声明（#35 批2 拆模块：从 registry.rs 机械切出，块内零改动）。
//!
//! 组装点在 [`super::build_tools`]；顺序敏感，勿单独调序。执行体复用 `super::*`。

use super::*;
use RiskLevel::ReadOnly;

pub(super) fn tools() -> Vec<ToolDef> {
    vec![
        // ── 查询 ×11 ──
        td(
            "aigx_list_channels",
            "列出渠道",
            "只读：列出渠道（search 模糊过滤 + page/page_size 分页）",
            ReadOnly,
            "",
            &["/api/channels"],
            || {
                json!({ "type":"object","properties":{ "search":{"type":"string","description":"按名称/类型/base_url/模型模糊过滤"}, "page":{"type":"integer","minimum":1}, "page_size":{"type":"integer","minimum":1} }, "required":[] })
            },
            |s, h, a| {
                Box::pin(async move {
                    let q = crate::api::admin::mcp::args_to_string_map(a);
                    out(admin::handle_list_channels(State(s.clone()), h.clone(), Query(q)).await)
                })
            },
        ),
        td(
            "aigx_list_users",
            "列出用户",
            "只读：列出用户（分页，敏感字段脱敏）",
            ReadOnly,
            "",
            &["/api/users"],
            paged,
            |s, h, a| {
                Box::pin(async move {
                    let q = crate::api::admin::mcp::parse_struct_args::<
                        admin::users::ListUsersQuery,
                    >("aigx_list_users", a)?;
                    out(admin::handle_list_users(State(s.clone()), h.clone(), Query(q)).await)
                })
            },
        ),
        td(
            "aigx_list_tokens",
            "列出访问令牌",
            "只读：列出 API 访问令牌（管理员可携 all=1 查全部，脱敏）",
            ReadOnly,
            "",
            &["/api/tokens"],
            paged,
            |s, h, a| {
                Box::pin(async move {
                    let q = crate::api::admin::mcp::parse_struct_args::<
                        crate::api::admin::tokens::ListTokensQuery,
                    >("aigx_list_tokens", a)?;
                    out(admin::handle_list_tokens(State(s.clone()), h.clone(), Query(q)).await)
                })
            },
        ),
        td(
            "aigx_list_accounts",
            "列出 CF 账号池",
            "只读：列出 CF 账号池（脱敏）",
            ReadOnly,
            "",
            &["/api/accounts"],
            no_args,
            |s, h, _a| {
                Box::pin(async move {
                    out(admin::handle_list_accounts(State(s.clone()), h.clone()).await)
                })
            },
        ),
        td(
            "aigx_list_orders",
            "列出订单",
            "只读：列出订单（keyword 过滤 + 分页）",
            ReadOnly,
            "",
            &["/api/orders"],
            || {
                json!({ "type":"object","properties":{ "keyword":{"type":"string"}, "page":{"type":"integer","minimum":1}, "size":{"type":"integer","minimum":1} }, "required":[] })
            },
            |s, h, a| {
                Box::pin(async move {
                    let q = crate::api::admin::mcp::args_to_string_map(a);
                    out(admin::handle_list_orders(State(s.clone()), h.clone(), Query(q)).await)
                })
            },
        ),
        td(
            "aigx_list_plans",
            "列出套餐模板",
            "只读：列出套餐模板",
            ReadOnly,
            "",
            &["/api/plans"],
            no_args,
            |s, h, _a| {
                Box::pin(async move { out(admin::handle_list_plans(State(s.clone()), h.clone()).await) })
            },
        ),
        td(
            "aigx_list_redemptions",
            "列出兑换码",
            "只读：列出兑换码（分页）",
            ReadOnly,
            "",
            &["/api/redemptions"],
            paged,
            |s, h, a| {
                Box::pin(async move {
                    let q = crate::api::admin::mcp::parse_struct_args::<
                        admin::redemptions::AuditLogQuery,
                    >("aigx_list_redemptions", a)?;
                    out(admin::handle_list_redemptions(State(s.clone()), h.clone(), Query(q)).await)
                })
            },
        ),
        td(
            "aigx_list_groups",
            "列出用户分组",
            "只读：列出用户分组",
            ReadOnly,
            "",
            &["/api/groups"],
            no_args,
            |s, h, _a| {
                Box::pin(async move { out(admin::handle_list_groups(State(s.clone()), h.clone()).await) })
            },
        ),
        td(
            "aigx_query_logs",
            "查询请求日志",
            "只读：查询请求日志（user/model/channel/start/end 过滤 + 分页）",
            ReadOnly,
            "",
            &["/api/logs/requests"],
            || {
                json!({ "type":"object","properties":{ "user":{"type":"string"}, "model":{"type":"string"}, "channel":{"type":"string"}, "start":{"type":"integer"}, "end":{"type":"integer"}, "page":{"type":"integer","minimum":1}, "size":{"type":"integer","minimum":1} }, "required":[] })
            },
            |s, h, a| {
                Box::pin(async move {
                    let q = crate::api::admin::mcp::parse_struct_args::<
                        admin::logs::RequestLogQuery,
                    >("aigx_query_logs", a)?;
                    out(admin::handle_list_request_logs(State(s.clone()), h.clone(), Query(q)).await)
                })
            },
        )
        .user_exec(|s, h, a| Box::pin(async move { out(user_logs_domain(s, h, a).await) })),
        td(
            "aigx_query_usage",
            "本人额度与用量",
            "只读：当前调用者的额度与近 30 天用量（配额/已用/剩余/请求数/tokens）",
            ReadOnly,
            "",
            &["/api/user/records"],
            no_args,
            |s, h, _a| Box::pin(async move { out(user_usage_domain(s, h).await) }),
        )
        .user_exec(|s, h, _a| Box::pin(async move { out(user_usage_domain(s, h).await) })),
        td(
            "aigx_get_pricing",
            "列出模型定价",
            "只读：列出全部模型定价",
            ReadOnly,
            "",
            &["/api/pricing"],
            no_args,
            |s, h, _a| {
                Box::pin(async move { out(admin::handle_list_pricing(State(s.clone()), h.clone()).await) })
            },
        ),
        td(
            "aigx_cost_report",
            "成本用量报表",
            "只读：成本/用量聚合报表（按模型统计调用量与成本）",
            ReadOnly,
            "",
            &["/api/logs/requests"],
            no_args,
            |s, _h, _a| Box::pin(async move { out(crate::agent::tools::cost_report(s).await) }),
        ),
    ]
}
