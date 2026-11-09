//! `models` 域工具声明（#35 批2 拆模块：从 registry.rs 机械切出，块内零改动）。
//!
//! 组装点在 [`super::build_tools`]；顺序敏感，勿单独调序。执行体复用 `super::*`。

use super::*;
use RiskLevel::{HighRisk, LowRisk, ReadOnly};

pub(super) fn tools() -> Vec<ToolDef> {
    vec![
        // ── 统一模型管理 ──
        td(
            "aigx_models_view",
            "模型管理视图",
            "只读：统一模型管理视图（渠道+账号池模型合流，含启停/来源/定价/别名）",
            ReadOnly,
            "",
            &["/api/models/manage"],
            no_args,
            |s, h, _a| {
                Box::pin(async move {
                    out(admin::model_manage::handle_manage_view(State(s.clone()), h.clone()).await)
                })
            },
        ),
        td(
            "aigx_model_set_state",
            "模型启停",
            "写操作（可回滚）：按 (provider, model) 启用/禁用模型",
            LowRisk,
            "id",
            &["/api/models/state"],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "provider": { "type": "string" },
                        "id": { "type": "string", "description": "模型 ID" },
                        "enabled": { "type": "boolean" }
                    },
                    "required": ["provider", "id", "enabled"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let body: admin::model_manage::StateRequest =
                        parse_tool_args("aigx_model_set_state", a)?;
                    out(admin::model_manage::handle_set_state(
                        State(s.clone()),
                        h.clone(),
                        Json(body),
                    )
                    .await)
                })
            },
        ),
        td(
            "aigx_model_price_upsert",
            "设置模型定价",
            "高危写：设置模型定价与倍率（需审批）",
            HighRisk,
            "model",
            &["/api/models/price"],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "model": { "type": "string" },
                        "input_price": { "type": "number" },
                        "output_price": { "type": "number" },
                        "cache_price": { "type": "number" },
                        "price_type": { "type": "string", "description": "token/count" },
                        "model_ratio": { "type": "number" }
                    },
                    "required": ["model", "input_price", "output_price"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let body: admin::model_manage::PriceBody =
                        parse_tool_args("aigx_model_price_upsert", a)?;
                    out(admin::model_manage::handle_upsert_model_price(
                        State(s.clone()),
                        h.clone(),
                        Json(body),
                    )
                    .await)
                })
            },
        ),
        td(
            "aigx_model_price_remove",
            "删除模型定价",
            "高危写：删除模型定价（需审批）",
            HighRisk,
            "model",
            &["/api/models/price/remove"],
            || {
                json!({
                    "type": "object",
                    "properties": { "model": { "type": "string" } },
                    "required": ["model"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let body: admin::model_manage::PriceRemoveBody =
                        parse_tool_args("aigx_model_price_remove", a)?;
                    out(admin::model_manage::handle_remove_price(
                        State(s.clone()),
                        h.clone(),
                        Json(body),
                    )
                    .await)
                })
            },
        ),
        td(
            "aigx_model_alias_set",
            "设置模型别名",
            "写操作（可回滚）：设置全局模型别名（alias → target）",
            LowRisk,
            "alias",
            &["/api/models/alias"],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "alias": { "type": "string" },
                        "target": { "type": "string" }
                    },
                    "required": ["alias", "target"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let body: admin::model_manage::AliasBody =
                        parse_tool_args("aigx_model_alias_set", a)?;
                    out(admin::model_manage::handle_set_alias(
                        State(s.clone()),
                        h.clone(),
                        Json(body),
                    )
                    .await)
                })
            },
        ),
        td(
            "aigx_model_alias_remove",
            "删除模型别名",
            "写操作（可回滚）：删除全局模型别名",
            LowRisk,
            "alias",
            &["/api/models/alias/remove"],
            || {
                json!({
                    "type": "object",
                    "properties": { "alias": { "type": "string" } },
                    "required": ["alias"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let body: admin::model_manage::AliasRemoveBody =
                        parse_tool_args("aigx_model_alias_remove", a)?;
                    out(admin::model_manage::handle_remove_alias(
                        State(s.clone()),
                        h.clone(),
                        Json(body),
                    )
                    .await)
                })
            },
        ),
        td(
            "aigx_models_meta_list",
            "模型元信息",
            "只读：模型元信息覆盖（owned_by/上下文/能力）",
            ReadOnly,
            "",
            &["/api/models/meta"],
            no_args,
            |s, h, _a| {
                Box::pin(async move {
                    out(admin::pricing::handle_model_meta_list(State(s.clone()), h.clone()).await)
                })
            },
        ),
        td(
            "aigx_available_models",
            "可用模型清单",
            "只读：用户侧可用模型聚合（启用渠道模型并集）",
            ReadOnly,
            "",
            &["/api/models/available"],
            no_args,
            |s, h, _a| {
                Box::pin(async move {
                    out(admin::handle_available_models(State(s.clone()), h.clone()).await)
                })
            },
        )
        // #34 用户域：只读公开目录（`handle_available_models` 内部 verify_user +
        // 用户分组模型白名单过滤，与管理员面共用同一份可用模型口径）。
        .user_exec(|s, h, _a| {
            Box::pin(async move {
                out(admin::handle_available_models(State(s.clone()), h.clone()).await)
            })
        }),
    ]
}
