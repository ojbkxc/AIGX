//! `account_pool` 域工具声明（#35 批2 拆模块：从 registry.rs 机械切出，块内零改动）。
//!
//! 组装点在 [`super::build_tools`]；顺序敏感，勿单独调序。执行体复用 `super::*`。

use super::*;
use RiskLevel::{LowRisk, ReadOnly};

pub(super) fn tools() -> Vec<ToolDef> {
    vec![
        // ── 账号余额 / token 刷新 / 模型目录 ──
        td(
            "aigx_account_balance",
            "账号余额",
            "只读：账号级余额/额度查询（qoder/autoclaw/小浣熊/zcode）",
            ReadOnly,
            "account_id",
            &["/api/account-sessions/{id}/balance"],
            account_id_args,
            |s, h, a| {
                Box::pin(async move {
                    let id = require_arg(a, "account_id")?;
                    out(admin::account_sessions::handle_account_balance(
                        State(s.clone()),
                        h.clone(),
                        Path(id),
                    )
                    .await)
                })
            },
        ),
        td(
            "aigx_account_refresh_token",
            "刷新账号 token",
            "写操作（可回滚）：刷新账号 access/refresh token（force=true 无条件刷）",
            LowRisk,
            "account_id",
            &["/api/account-sessions/{id}/refresh"],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "account_id": { "type": "string" },
                        "force": { "type": "boolean", "description": "true=无条件刷；默认仅临期才刷" }
                    },
                    "required": ["account_id"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let id = require_arg(a, "account_id")?;
                    let force = a.get("force").and_then(|v| v.as_bool()).unwrap_or(false);
                    out(admin::account_sessions::handle_account_refresh(
                        State(s.clone()),
                        h.clone(),
                        Path(id),
                        Json(admin::account_sessions::RefreshBody { force }),
                    )
                    .await)
                })
            },
        ),
        td(
            "aigx_account_catalog_sync",
            "同步模型目录",
            "写操作（可回滚）：同步账号上游模型目录（可按账号/供应商，apply 写回白名单）",
            LowRisk,
            "account_id",
            &["/api/account-sessions/catalog/sync"],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "account_id": { "type": "string" },
                        "provider": { "type": "string" },
                        "force": { "type": "boolean", "description": "忽略缓存强制回源" },
                        "apply": { "type": "boolean", "description": "写回账号模型白名单" },
                        "replace": { "type": "boolean", "description": "apply 时整体替换而非合并" }
                    },
                    "required": []
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let body: admin::account_sessions::CatalogSyncRequest =
                        parse_tool_args("aigx_account_catalog_sync", a)?;
                    out(admin::account_sessions::handle_sync_account_catalog(
                        State(s.clone()),
                        h.clone(),
                        Json(body),
                    )
                    .await)
                })
            },
        ),
        td(
            "aigx_account_catalog_providers",
            "目录支持供应商",
            "只读：支持模型目录发现的供应商清单",
            ReadOnly,
            "",
            &["/api/account-sessions/catalog/providers"],
            no_args,
            |s, h, _a| {
                Box::pin(async move {
                    out(admin::account_sessions::handle_catalog_providers(
                        State(s.clone()),
                        h.clone(),
                    )
                    .await)
                })
            },
        ),
        td(
            "aigx_account_rotation_get",
            "读账号轮换策略",
            "只读：读取账号池轮换/冷却策略",
            ReadOnly,
            "",
            &["/api/account-sessions/rotation"],
            no_args,
            |s, h, _a| {
                Box::pin(async move {
                    out(admin::account_sessions::handle_get_rotation_policy(
                        State(s.clone()),
                        h.clone(),
                    )
                    .await)
                })
            },
        ),
        td(
            "aigx_account_rotation_set",
            "改账号轮换策略",
            "写操作（可回滚）：修改账号池轮换/冷却策略",
            LowRisk,
            "",
            &["/api/account-sessions/rotation"],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "base_cooldown_secs": { "type": "integer" },
                        "max_cooldown_secs": { "type": "integer" },
                        "max_attempts": { "type": "integer" }
                    },
                    "required": []
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let body: admin::account_sessions::RotationPolicyRequest =
                        parse_tool_args("aigx_account_rotation_set", a)?;
                    out(admin::account_sessions::handle_set_rotation_policy(
                        State(s.clone()),
                        h.clone(),
                        Json(body),
                    )
                    .await)
                })
            },
        ),
        td(
            "aigx_account_providers",
            "账号供应商清单",
            "只读：账号池支持的供应商清单（57 家）",
            ReadOnly,
            "",
            &["/api/account-sessions/providers"],
            no_args,
            |s, h, _a| {
                Box::pin(async move {
                    out(admin::account_sessions::handle_list_account_providers(
                        State(s.clone()),
                        h.clone(),
                    )
                    .await)
                })
            },
        ),
    ]
}
