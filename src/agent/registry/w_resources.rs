//! 批2 并行单元 U-B（#33：plugins + scheduled_tasks + network）的工具声明文件 —— **文件独占**：仅本单元 owner 编辑。
//!
//! 三条写侧工具（执行体只调用 `src/api/admin/` 下已 `pub` 的 handler，不改他人文件）：
//! - `aigx_plugin_manage`：内置 provider 目录启停 / 包级启停 / 卸载（`plugins.rs`）；
//! - `aigx_scheduled_task_manage`：间隔型定时任务改配置 / 立即执行（`scheduled_tasks.rs`）；
//! - `aigx_network_manage`：网络层 CF 账号池增删 / 重启（`network.rs`）。
//!
//! risk 口径（57 §8.3）：本仓 `ToolDef.risk` 是**单值**、无 per-action 机制；一条工具内含
//! 高危 action ⇒ 整条取最高级（同 U-E/U-F 先例）。三条工具各含至少一个不可逆/真动作：
//! plugins `uninstall`、scheduled `run`（会真的发起任务动作）、network `remove-account`
//! （删除账号池条目，无快照）⇒ 三条整条判 `HighRisk`（自动进审批）。
//!
//! **不含** proxy-pool / tickets（U-E 的 `w_pool_tickets.rs`）、不含 notify 台账（U-G 已定案）。
//! `aigx_plugin_manage` 只覆盖 `/api/plugins/{enable,provider,uninstall}` 三条写路由；
//! 只读的 `/api/plugins`、`/api/plugins/{update,release,progress}` 归 `aigx_plugin_view`（observe.rs）。
//! `aigx_network_manage` 只覆盖写路由；只读的 `/api/network/{status,accounts}` 归
//! `aigx_network_status`（observe.rs）。本工具不重复 owns 探活/只读路由（同一路由两工具
//! owns 会让模型选错面）。
//!
//! 执行体复用 `super::*`；assembler 在 [`super::build_tools`]，本文件 owner 不回改 `mod.rs`。

use super::*;
use axum::body::Bytes;
use RiskLevel::HighRisk;

/// `aigx_plugin_manage` 的 action 白名单（schema enum 与分派表同源，防「列了却调不到」）。
const PLUGIN_MANAGE_ACTIONS: [&str; 4] = ["enable", "disable", "provider-enable", "uninstall"];

/// `aigx_scheduled_task_manage` 的 action 白名单。
const SCHEDULED_TASK_MANAGE_ACTIONS: [&str; 2] = ["configure", "run"];

/// `aigx_network_manage` 的 action 白名单。
const NETWORK_MANAGE_ACTIONS: [&str; 3] = ["add-account", "remove-account", "restart"];

pub(super) fn tools() -> Vec<ToolDef> {
    vec![
        td(
            "aigx_plugin_manage",
            "插件管理",
            "高危写：内置 provider 目录的按家启停（provider-enable，数据面选号即时生效）/ 包级启停（enable/disable，内置包不支持，返回降级说明）/ 卸载（uninstall，内置包不支持）。真实动作是 provider-enable；需审批",
            HighRisk,
            "provider",
            &[
                "/api/plugins/enable",
                "/api/plugins/provider",
                "/api/plugins/uninstall",
            ],
            plugin_manage_schema,
            |s, h, a| {
                Box::pin(async move {
                    let action = require_arg(a, "action")?;
                    // 包 id：AIGX 只有内置包 `builtin`，缺省即它（传别的值 handler 报 404）。
                    let pack_id = a
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or("builtin")
                        .to_string();
                    match action.as_str() {
                        // 包级启停：内置包编译在二进制里，handler 恒返回降级说明
                        // （整包停用会让网关无上游可用）。enable/disable 走同一条路由
                        // （前端也只调这一条，`enabled` 由前端提示、后端不解）。
                        "enable" | "disable" => out(admin::plugins::handle_set_plugin_enabled(
                            State(s.clone()),
                            h.clone(),
                            Json(admin::plugins::PackIdBody { id: pack_id }),
                        )
                        .await),
                        "provider-enable" => {
                            let provider = require_arg(a, "provider")?;
                            let enabled =
                                a.get("enabled").and_then(Value::as_bool).ok_or_else(|| {
                                    (
                                        -32602,
                                        "Invalid params: provider-enable requires boolean `enabled`"
                                            .to_string(),
                                    )
                                })?;
                            out(admin::plugins::handle_set_plugin_provider_enabled(
                                State(s.clone()),
                                h.clone(),
                                Json(admin::plugins::ProviderBody {
                                    id: pack_id,
                                    provider,
                                    enabled,
                                }),
                            )
                            .await)
                        }
                        // 卸载：内置包不支持，handler 恒返回降级说明。
                        "uninstall" => out(admin::plugins::handle_uninstall_plugin(
                            State(s.clone()),
                            h.clone(),
                            Json(admin::plugins::PackIdBody { id: pack_id }),
                        )
                        .await),
                        other => Err((
                            -32602,
                            format!(
                                "Invalid action for aigx_plugin_manage: {other} \
                                 (expect enable/disable/provider-enable/uninstall)"
                            ),
                        )),
                    }
                })
            },
        ),
        td(
            "aigx_scheduled_task_manage",
            "定时任务管理",
            "高危写：改一条间隔型定时任务的开关/间隔（configure），或立即执行一条后端任务（run，会真的发起该任务的动作：凭证维护/模型目录刷新/版本检查）。run 会落地真实动作，需审批",
            HighRisk,
            "id",
            &["/api/scheduled-tasks/{id}", "/api/scheduled-tasks/{id}/run"],
            scheduled_task_manage_schema,
            |s, h, a| {
                Box::pin(async move {
                    let action = require_arg(a, "action")?;
                    let id = require_arg(a, "id")?;
                    match action.as_str() {
                        "configure" => {
                            // 只转发显式给出的字段（缺省字段不动）；空 patch 由 handler 判 400
                            // （「没有需要更新的字段」），不在工具侧重造一套校验。
                            let mut patch = serde_json::Map::new();
                            if let Some(v) = a.get("enabled") {
                                patch.insert("enabled".to_string(), v.clone());
                            }
                            if let Some(v) = a.get("interval") {
                                patch.insert("interval".to_string(), v.clone());
                            }
                            let body = Bytes::from(Value::Object(patch).to_string().into_bytes());
                            scheduled_result(
                                admin::scheduled_tasks::configure_handler(
                                    State(s.clone()),
                                    Path(id),
                                    h.clone(),
                                    body,
                                )
                                .await,
                            )
                            .await
                        }
                        "run" => {
                            scheduled_result(
                                admin::scheduled_tasks::run_handler(
                                    State(s.clone()),
                                    Path(id),
                                    h.clone(),
                                )
                                .await,
                            )
                            .await
                        }
                        other => Err((
                            -32602,
                            format!(
                                "Invalid action for aigx_scheduled_task_manage: {other} \
                                 (expect configure/run)"
                            ),
                        )),
                    }
                })
            },
        ),
        td(
            "aigx_network_manage",
            "网络层账号运维",
            "高危写：网络层 CF 账号池的增删（add-account/remove-account，remove 不可逆）与重启（restart，复位全部渠道断路器与健康状态）。remove-account 需审批",
            HighRisk,
            "account_id",
            &["/api/network/accounts/{account_id}", "/api/network/restart"],
            network_manage_schema,
            |s, h, a| {
                Box::pin(async move {
                    let action = require_arg(a, "action")?;
                    match action.as_str() {
                        "add-account" => {
                            let account_id = require_arg(a, "account_id")?;
                            // 请求体与 handler 的 AccountConfigRequest 同源反序列化
                            // （含 camelCase 别名归一；`name` 是结构体必填字段，
                            // 缺了会退化成泛化的反序列化错误，故先给指名道姓的 -32602）。
                            // `api_token` 仅入参，handler 不回显。
                            require_arg(a, "name")?;
                            let body: admin::network::AccountConfigRequest =
                                parse_tool_args("aigx_network_manage", a)?;
                            out(admin::add_network_account(
                                State(s.clone()),
                                h.clone(),
                                Path(account_id),
                                Some(Json(body)),
                            )
                            .await)
                        }
                        "remove-account" => {
                            let account_id = require_arg(a, "account_id")?;
                            out(admin::remove_network_account(
                                State(s.clone()),
                                h.clone(),
                                Path(account_id),
                            )
                            .await)
                        }
                        "restart" => out(admin::restart_network(State(s.clone()), h.clone()).await),
                        other => Err((
                            -32602,
                            format!(
                                "Invalid action for aigx_network_manage: {other} \
                                 (expect add-account/remove-account/restart)"
                            ),
                        )),
                    }
                })
            },
        ),
    ]
}

// ── 执行体辅助 ──────────────────────────────────────────────────────

/// `configure_handler` / `run_handler` 返回 `Result<Response, …>`（成功分支已
/// `into_response`，进不了 [`out`] 的 `HandlerResult` 口径）：成功分支走
/// [`response_to_outcome`]，失败分支复用 [`handler_to_outcome`]（同一套脱敏）。
async fn scheduled_result(
    result: Result<axum::response::Response, (axum::http::StatusCode, Json<Value>)>,
) -> ExecResult {
    match result {
        Ok(resp) => Ok(response_to_outcome(resp).await),
        Err(e) => Ok(handler_to_outcome(Err(e))),
    }
}

/// 读完整响应体转成工具结果（同 `w_models.rs::response_to_outcome` 口径：2xx 原样
/// 16K 截断；非 2xx 过 `sanitize_error_message`，不把上游/后端错误原文回灌模型）。
async fn response_to_outcome(resp: axum::response::Response) -> ToolOutcome {
    let status = resp.status();
    let bytes = match axum::body::to_bytes(resp.into_body(), 256 * 1024).await {
        Ok(b) => b,
        Err(e) => {
            return ToolOutcome {
                text: format!("读取响应失败：{e}"),
                ok: false,
            }
        }
    };
    let text = String::from_utf8_lossy(&bytes).to_string();
    if status.is_success() {
        ToolOutcome {
            text: crate::api::admin::common::truncate_tool_text(text),
            ok: true,
        }
    } else {
        let msg = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
            .unwrap_or(text);
        ToolOutcome {
            text: crate::error_translate::sanitize_error_message(&msg),
            ok: false,
        }
    }
}

// ── schema ──────────────────────────────────────────────────────────

fn plugin_manage_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "action": {
                "type": "string",
                "enum": PLUGIN_MANAGE_ACTIONS,
                "description": "provider-enable=按家启停（真实动作）；enable/disable=包级启停（内置包不支持，返回降级说明）；uninstall=卸载（内置包不支持）"
            },
            "provider": { "type": "string", "description": "provider-enable 必填：provider kind（如 qoder/loomy/cline）" },
            "enabled": { "type": "boolean", "description": "provider-enable 必填：true=启用该家 / false=停用（数据面选号即时生效）" },
            "id": { "type": "string", "description": "插件包 id，缺省 builtin（AIGX 只有内置包）" }
        },
        "required": ["action"]
    })
}

fn scheduled_task_manage_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "action": {
                "type": "string",
                "enum": SCHEDULED_TASK_MANAGE_ACTIONS,
                "description": "configure=改开关/间隔；run=立即执行一条后端任务（真的发起该任务动作）"
            },
            "id": {
                "type": "string",
                "description": "任务 id：credentialMaintenance/modelRefresh/updateCheck（后端，可 run）；logsAutoRefresh/requestsAutoRefresh/reportAutoRefresh（前端，run 无效）"
            },
            "enabled": { "type": "boolean", "description": "configure：是否启用（缺省=不改）" },
            "interval": { "type": "integer", "description": "configure：间隔（后端任务单位=分钟 1..1440；前端任务单位=秒 1..600；缺省=不改）" }
        },
        "required": ["action", "id"]
    })
}

fn network_manage_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "action": {
                "type": "string",
                "enum": NETWORK_MANAGE_ACTIONS,
                "description": "add-account=新增 CF 账号；remove-account=删除（不可逆）；restart=复位全部渠道断路器与健康状态"
            },
            "account_id": { "type": "string", "description": "add-account/remove-account 必填：CF 账号 id（删除时也接受内部 UUID）" },
            "name": { "type": "string", "description": "add-account 必填：账号显示名" },
            "api_token": { "type": "string", "description": "add-account：CF API token（仅入参，不回显）" },
            "status": { "type": "string", "description": "add-account：账号状态，缺省 active" }
        },
        "required": ["action"]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLUGIN_COVERS: &[&str] = &[
        "/api/plugins/enable",
        "/api/plugins/provider",
        "/api/plugins/uninstall",
    ];
    const SCHEDULED_COVERS: &[&str] =
        &["/api/scheduled-tasks/{id}", "/api/scheduled-tasks/{id}/run"];
    const NETWORK_COVERS: &[&str] = &["/api/network/accounts/{account_id}", "/api/network/restart"];

    fn action_enum(schema: Value) -> Vec<String> {
        schema["properties"]["action"]["enum"]
            .as_array()
            .expect("action.enum 为数组")
            .iter()
            .map(|v| v.as_str().unwrap_or_default().to_string())
            .collect()
    }

    fn as_strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    /// 「列」侧：三条工具声明台账——名称/risk/target_key/covers 逐条钉死，
    /// 高危不得进 MCP 面（三面共用同一 risk 口径）。
    #[test]
    fn ub_tools_declared_with_expected_risk_and_covers() {
        for (name, target_key, covers) in [
            ("aigx_plugin_manage", "provider", PLUGIN_COVERS),
            ("aigx_scheduled_task_manage", "id", SCHEDULED_COVERS),
            ("aigx_network_manage", "account_id", NETWORK_COVERS),
        ] {
            let tool = find_tool(name).unwrap_or_else(|| panic!("工具未注册: {name}"));
            assert_eq!(
                tool.risk,
                RiskLevel::HighRisk,
                "{name} 含不可逆/真动作 ⇒ 整条 HighRisk（进审批）"
            );
            assert_eq!(tool.target_key, target_key, "{name} target_key");
            assert_eq!(tool.covers.len(), covers.len(), "{name} covers 条数");
            for c in covers {
                assert!(tool.covers.contains(c), "{name} 缺 covers {c}");
            }
            assert!(
                mcp_tools().all(|m| m.name != name),
                "{name} 高危不得进 MCP 面"
            );
        }
    }

    /// 「调」侧：schema enum 与分派常量同源（「列了却调不到」/「调了却没列」双向锁死），
    /// 执行体逐条路由到 handler，且未知 action 兜底拒绝。
    ///
    /// 单测不构造 `AppState`（无内存态构造器；同 `ue_exec_routes_every_declared_action`
    /// 手法），用源码锚定。needle 运行时拼接：本测试自身就在被扫文件里，写字面量会自匹配。
    #[test]
    fn ub_exec_routes_every_declared_action_and_rejects_unknown() {
        assert_eq!(
            action_enum(plugin_manage_schema()),
            as_strings(&PLUGIN_MANAGE_ACTIONS),
            "plugin schema enum 与分派常量不一致"
        );
        assert_eq!(
            action_enum(scheduled_task_manage_schema()),
            as_strings(&SCHEDULED_TASK_MANAGE_ACTIONS),
            "scheduled schema enum 与分派常量不一致"
        );
        assert_eq!(
            action_enum(network_manage_schema()),
            as_strings(&NETWORK_MANAGE_ACTIONS),
            "network schema enum 与分派常量不一致"
        );

        let src = include_str!("w_resources.rs");
        for needle in [
            ["admin::plugins::handle_set_plugin_", "enabled("].concat(),
            ["admin::plugins::handle_set_plugin_provider_", "enabled("].concat(),
            ["admin::plugins::handle_uninstall_", "plugin("].concat(),
            ["admin::scheduled_tasks::configure_", "handler("].concat(),
            ["admin::scheduled_tasks::run_", "handler("].concat(),
            ["admin::add_network_", "account("].concat(),
            ["admin::remove_network_", "account("].concat(),
            ["admin::restart_network", "("].concat(),
        ] {
            assert!(src.contains(&needle), "执行体缺路由: {needle}");
        }
        // 未知 action 兜底（三条工具各一个 default 臂）。
        for needle in [
            ["Invalid action for aigx_plugin_", "manage"].concat(),
            ["Invalid action for aigx_scheduled_task_", "manage"].concat(),
            ["Invalid action for aigx_network_", "manage"].concat(),
        ] {
            assert!(src.contains(&needle), "缺未知 action 兜底拒绝: {needle}");
        }
    }

    /// 路由边界：network 写工具只 owns 写路由；只读的 `/api/network/status`、
    /// `/api/network/accounts` 归 `aigx_network_status`（observe.rs），本工具不重复 owns。
    /// 双工具 owns 同一路由 = 模型选错面，本测把这条边界钉死。
    #[test]
    fn ub_network_manage_does_not_own_read_routes() {
        let t = find_tool("aigx_network_manage").expect("未注册");
        assert!(
            !t.covers.contains(&"/api/network/status"),
            "只读 /api/network/status 应归 aigx_network_status"
        );
        assert!(
            !t.covers.contains(&"/api/network/accounts"),
            "只读 /api/network/accounts 应归 aigx_network_status"
        );
    }
}
