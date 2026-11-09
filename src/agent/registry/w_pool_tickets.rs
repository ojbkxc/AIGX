//! 批2 并行单元 U-E（#22：proxy_pool_manage + ticket_ops）的工具声明文件 —— **文件独占**：仅本单元 owner 编辑。
//!
//! 两条写侧工具（执行体只调用 `src/api/admin/` 下已 `pub` 的 handler，不改他人文件）：
//! - `aigx_proxy_pool_manage`：代理池条目增改 / 启停 / 删除 / 同步 Clash；
//! - `aigx_ticket_ops`：管理员工单回复 / 关闭。
//!
//! risk 口径（57 §8.3）：本仓 `ToolDef.risk` 是**单值**、无 per-action 机制；一条工具内含
//! 高危 action ⇒ 整条取最高级（同 U-F 的 plan / redemption / token 先例）。故
//! `aigx_proxy_pool_manage` 因含 `remove`（不可逆删除，且有账号引用时拒绝）与
//! `sync-clash`（严格镜像会摘除已消失的条目）整体判 `HighRisk`（自动进审批）；
//! `aigx_ticket_ops` 亦判 `HighRisk` —— 判据 4「影响域溢出到第三方」：`reply` 写入
//! **用户可见的工单消息行**（用户经 `GET /api/tickets?id=` 读到），`close` 亦为用户可见状态；
//! 即 AI 可直接对外（真实用户）发话，与 G6 通知族被移出 AI 面同族。见下表调用链取证。
//!
//! `aigx_ticket_ops` 触达用户的取证（调用链逐跳，非仅函数体 grep）：
//! - `admin::handle_admin_reply_ticket`（`api/admin/tickets.rs:271`）→ `:288`
//!   `state.ticket_store.reply_by_admin` → `ticket/mod.rs:204` → `append_message`
//!   （`ticket/mod.rs:212-246`）→ `persist_message`（`ticket/mod.rs:106-108`）写
//!   `ticket_msg:*`，并把 `reply_status` 置 `REPLY_WAIT_USER`（`:228-232`）。
//! - `admin::handle_admin_close_ticket`（`tickets.rs:294`）→ `:301` `ticket_store.close`
//!   → `ticket/mod.rs:249-258` 写 `ticket:*`（`status=1`）。
//! - 用户侧可见出口（**因果证据**）：`handle_user_list_tickets`（`tickets.rs:55-88`）
//!   在 `q.id` 非空时鉴权后取 `ticket_store.messages(&ticket.id)`，把 `message_json`
//!   数组塞进 `data["message"]` 回给调用者（`tickets.rs:69-79`）⇒ 管理员（= AI 经审批后）
//!   写入的文本会被**该工单的真实用户**读到。**注意这是 pull：用户主动轮询自己的工单时
//!   才读到，不是推送**（无 webhook / 邮件 / 站内通知），不要把本条误读成「有外发通道」。
//! - 全链**无** `notify`/`email`/`webhook`/`push`/站内通知行（`ticket/mod.rs` 内 0 命中；
//!   `ticket_store` 的写入点仅 `tickets.rs` 各 handler + `users.rs:370` 删除）。
//!   `tickets.rs` 内 `email` 仅作管理员列表的**只读查询过滤**（`:204` 字段、`:238-241` 用其
//!   查用户），**零外发通道**。
//!
//! 该结论经 **lead 独立复核**（2026-11-09，tip `f992bbea`；重走两遍调用链、并另跑
//! `grep -n "notify\|email\|mail\|webhook\|slack\|telegram\|push\|站内\|通知"
//! src/api/admin/tickets.rs` 确认仅命中只读查询）⇒ 由「owner 主张」升级为
//! **双人独立核到同一结论**；owner 原始取证时点为 2026-11-09（提交 `f0b51ec2` 前）。
//!
//! `handle_pool_test` / `handle_pool_test_account` **不在本工具**：57 §2 G3 与任务 #5 把探活族
//! 归 `aigx_probe`（U-A，HIGH），本工具不重复覆盖其路由（同一路由两工具 owns 会让模型选错）。
//!
//! 执行体复用 `super::*`；assembler 在 [`super::build_tools`]，本文件 owner 不回改 `mod.rs`。

use super::*;
use crate::proxy_pool as pool;
use RiskLevel::HighRisk;

/// `aigx_proxy_pool_manage` 的 action 白名单（schema enum 与分派表同源，防「列了却调不到」）。
const PROXY_POOL_MANAGE_ACTIONS: [&str; 6] = [
    "create",
    "update",
    "enable",
    "disable",
    "remove",
    "sync-clash",
];

/// `aigx_ticket_ops` 的 action 白名单。
const TICKET_OPS_ACTIONS: [&str; 2] = ["reply", "close"];

pub(super) fn tools() -> Vec<ToolDef> {
    vec![
        td(
            "aigx_proxy_pool_manage",
            "代理池管理",
            "高危写：代理池条目增改 / 启停 / 删除 / 同步 Clash（remove 不可逆、sync-clash 会摘除已消失的镜像条目；需审批）",
            HighRisk,
            "id",
            &[
                "/api/proxy-pool",
                "/api/proxy-pool/update",
                "/api/proxy-pool/enable",
                "/api/proxy-pool/remove",
                "/api/proxy-pool/sync-clash",
            ],
            proxy_pool_manage_schema,
            |s, h, a| {
                Box::pin(async move {
                    let action = require_arg(a, "action")?;
                    match action.as_str() {
                        "create" => {
                            let body: pool::EntryInput =
                                parse_tool_args("aigx_proxy_pool_manage", a)?;
                            out(admin::proxy_pool_api::handle_pool_create(
                                State(s.clone()),
                                h.clone(),
                                Json(body),
                            )
                            .await)
                        }
                        "update" => {
                            let body: pool::EntryInput =
                                parse_tool_args("aigx_proxy_pool_manage", a)?;
                            out(admin::proxy_pool_api::handle_pool_update(
                                State(s.clone()),
                                h.clone(),
                                Json(body),
                            )
                            .await)
                        }
                        "enable" | "disable" => {
                            let id = require_arg(a, "id")?;
                            let body: admin::proxy_pool_api::EnableBody = parse_tool_args(
                                "aigx_proxy_pool_manage",
                                &json!({ "id": id, "enabled": action.as_str() == "enable" }),
                            )?;
                            out(admin::proxy_pool_api::handle_pool_enable(
                                State(s.clone()),
                                h.clone(),
                                Json(body),
                            )
                            .await)
                        }
                        "remove" => {
                            let id = require_arg(a, "id")?;
                            let body: admin::proxy_pool_api::IdBody =
                                parse_tool_args("aigx_proxy_pool_manage", &json!({ "id": id }))?;
                            out(admin::proxy_pool_api::handle_pool_remove(
                                State(s.clone()),
                                h.clone(),
                                Json(body),
                            )
                            .await)
                        }
                        "sync-clash" => {
                            out(admin::proxy_pool_api::handle_pool_sync_clash(
                                State(s.clone()),
                                h.clone(),
                            )
                            .await)
                        }
                        other => Err((
                            -32602,
                            format!(
                                "Invalid action for aigx_proxy_pool_manage: {other} \
                                 (expect create/update/enable/disable/remove/sync-clash)"
                            ),
                        )),
                    }
                })
            },
        ),
        td(
            "aigx_ticket_ops",
            "工单运维",
            "高危写：回复工单（reply，写入用户可见的工单消息、可重开已关闭工单）与关闭工单（close，用户可见状态；回复即对真实用户发话，需审批）",
            HighRisk,
            "id",
            &["/api/admin/tickets/reply", "/api/admin/tickets/close"],
            ticket_ops_schema,
            |s, h, a| {
                Box::pin(async move {
                    let action = require_arg(a, "action")?;
                    match action.as_str() {
                        "reply" => {
                            let body: admin::tickets::ReplyTicketRequest =
                                parse_tool_args("aigx_ticket_ops", a)?;
                            out(admin::handle_admin_reply_ticket(
                                State(s.clone()),
                                h.clone(),
                                Json(body),
                            )
                            .await)
                        }
                        "close" => {
                            let body: admin::tickets::CloseTicketRequest =
                                parse_tool_args("aigx_ticket_ops", a)?;
                            out(admin::handle_admin_close_ticket(
                                State(s.clone()),
                                h.clone(),
                                Json(body),
                            )
                            .await)
                        }
                        other => Err((
                            -32602,
                            format!(
                                "Invalid action for aigx_ticket_ops: {other} (expect reply/close)"
                            ),
                        )),
                    }
                })
            },
        ),
    ]
}

// ── schema ──────────────────────────────────────────────────────────

fn proxy_pool_manage_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "action": {
                "type": "string",
                "enum": PROXY_POOL_MANAGE_ACTIONS,
                "description": "create=新建条目；update=编辑；enable/disable=启停；remove=删除（不可逆）；sync-clash=镜像本机 Clash 出口"
            },
            "id": { "type": "string", "description": "条目 ID（update/enable/disable/remove 必填）" },
            "name": { "type": "string", "description": "条目名（create/update）" },
            "enabled": { "type": "boolean", "description": "是否启用（create）" },
            "protocol": { "type": "string", "enum": ["http", "https", "socks5", "socks5h"], "description": "代理协议" },
            "host": { "type": "string", "description": "代理主机地址" },
            "port": { "type": "integer", "minimum": 1, "maximum": 65535, "description": "代理端口" },
            "username": { "type": "string", "description": "认证用户名（可选）" },
            "password": { "type": "string", "description": "认证密码（可选；编辑时留空=保留旧密码，不回显）" }
        },
        "required": ["action"]
    })
}

fn ticket_ops_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "action": {
                "type": "string",
                "enum": TICKET_OPS_ACTIONS,
                "description": "reply=回复工单（可重开已关闭工单）；close=关闭工单"
            },
            "id": { "type": "string", "description": "工单 ID" },
            "message": { "type": "string", "description": "回复内容（reply 必填，不能为空）" }
        },
        "required": ["action", "id"]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const POOL_COVERS: &[&str] = &[
        "/api/proxy-pool",
        "/api/proxy-pool/update",
        "/api/proxy-pool/enable",
        "/api/proxy-pool/remove",
        "/api/proxy-pool/sync-clash",
    ];
    const TICKET_COVERS: &[&str] = &["/api/admin/tickets/reply", "/api/admin/tickets/close"];

    /// 「列」侧：两条工具声明台账——名称/risk/target_key/covers 逐条钉死，
    /// 两条均 HighRisk ⇒ 均不得进 MCP 面（三面共用同一 risk 口径）。
    #[test]
    fn ue_tools_declared_with_expected_risk_and_covers() {
        let pool_tool = find_tool("aigx_proxy_pool_manage").expect("代理池管理工具未注册");
        assert_eq!(
            pool_tool.risk,
            RiskLevel::HighRisk,
            "含 remove/sync-clash ⇒ 整条 HighRisk（进审批）"
        );
        assert_eq!(pool_tool.target_key, "id", "target_key");
        for c in POOL_COVERS {
            assert!(pool_tool.covers.contains(c), "缺 covers {c}");
        }
        assert!(
            mcp_tools().all(|t| t.name != "aigx_proxy_pool_manage"),
            "高危不得进 MCP 面"
        );

        let ticket_tool = find_tool("aigx_ticket_ops").expect("工单运维工具未注册");
        assert_eq!(
            ticket_tool.risk,
            RiskLevel::HighRisk,
            "判据 4：reply 写入用户可见的工单消息行 ⇒ 影响域溢出到第三方，整条 HighRisk"
        );
        assert_eq!(ticket_tool.target_key, "id", "target_key");
        for c in TICKET_COVERS {
            assert!(ticket_tool.covers.contains(c), "缺 covers {c}");
        }
        assert!(
            mcp_tools().all(|t| t.name != "aigx_ticket_ops"),
            "高危不得进 MCP 面"
        );
    }

    /// 「调」侧：执行体必须逐条路由到 handler，且未知 action 兜底拒绝。
    ///
    /// 单测不构造 `AppState`（无内存态构造器；同 `exec_entries_are_structurally_gated`
    /// 与 U-F 的 `...route_through_masked_outcome` 手法），用源码锚定。needle 运行时
    /// 拼接：本测试自身就在被扫文件里，写字面量会自匹配。
    #[test]
    fn ue_exec_routes_every_declared_action() {
        let src = include_str!("w_pool_tickets.rs");

        // schema enum 与分派常量同源（「列了却调不到」/「调了却没列」双向锁死）。
        let pool_schema = proxy_pool_manage_schema();
        let pool_enum: Vec<String> = pool_schema["properties"]["action"]["enum"]
            .as_array()
            .expect("action.enum 为数组")
            .iter()
            .map(|v| v.as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(
            pool_enum,
            PROXY_POOL_MANAGE_ACTIONS
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>(),
            "proxy pool schema enum 与分派常量不一致"
        );
        let ticket_schema = ticket_ops_schema();
        let ticket_enum: Vec<String> = ticket_schema["properties"]["action"]["enum"]
            .as_array()
            .expect("action.enum 为数组")
            .iter()
            .map(|v| v.as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(
            ticket_enum,
            TICKET_OPS_ACTIONS
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>(),
            "ticket schema enum 与分派常量不一致"
        );

        for needle in [
            ["admin::proxy_pool_api::handle_pool_", "create("].concat(),
            ["admin::proxy_pool_api::handle_pool_", "update("].concat(),
            ["admin::proxy_pool_api::handle_pool_", "enable("].concat(),
            ["admin::proxy_pool_api::handle_pool_", "remove("].concat(),
            ["admin::proxy_pool_api::handle_pool_", "sync_clash("].concat(),
            ["admin::handle_admin_", "reply_ticket("].concat(),
            ["admin::handle_admin_", "close_ticket("].concat(),
        ] {
            assert!(src.contains(&needle), "执行体缺路由: {needle}");
        }
        // 未知 action 兜底（两条工具各一个 default 臂）。
        assert!(
            src.contains(&["Invalid action for aigx_proxy_pool_", "manage"].concat()),
            "代理池工具缺未知 action 兜底拒绝"
        );
        assert!(
            src.contains(&["Invalid action for aigx_ticket_", "ops"].concat()),
            "工单工具缺未知 action 兜底拒绝"
        );
    }

    /// 路由不重复覆盖：本工具**不含**探活路由（`/api/proxy-pool/test*` 归 U-A 的 aigx_probe）。
    /// 双工具 owns 同一路由 = 模型选错面，本测把这条边界钉死。
    #[test]
    fn ue_pool_manage_does_not_own_probe_routes() {
        let t = find_tool("aigx_proxy_pool_manage").expect("未注册");
        for c in t.covers {
            assert!(
                !c.ends_with("/test") && !c.ends_with("/test-account"),
                "探活路由 {c} 应归 aigx_probe（U-A），不应由本工具 owns"
            );
        }
    }
}
