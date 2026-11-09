//! 批2 并行单元 U-F（#23：plan_manage + redemption_manage + token reset-used）的工具声明文件 —— **文件独占**：仅本单元 owner 编辑。
//!
//! 三条高危写工具（全部进审批矩阵，`runner.rs` 只对 HighRisk 挂审批）：
//! - `aigx_plan_manage`：套餐模板 upsert/delete + 按套餐发放 API Key（issue 出券）；
//! - `aigx_redemption_manage`：批量生成兑换码（create-batch）+ 删除兑换码（delete）；
//! - `aigx_token_ops`：重置令牌已用量（reset-used）+ 轮换密钥（rotate）。
//!
//! 凭据边界（57 §8 判据 5）：issue 返回 `plain_key`、rotate 返回 `key`、
//! create-batch 返回 `code`（可兑换价值凭证）——一律在**工具输出边界**过
//! [`mask_credential_fields`] 脱敏（口径 = `admin::tokens::mask_key_str`，与
//! `mask_token` 同源），明文不回灌模型；人类管理端 handler 的响应不经此函数，
//! 明文保持不变。
//!
//! 执行体复用 `super::*`；assembler 在 [`super::build_tools`]，本文件 owner 不回改 `mod.rs`。

use super::*;
use crate::api::admin::tokens::mask_key_str;
use RiskLevel::HighRisk;

pub(super) fn tools() -> Vec<ToolDef> {
    vec![
        // ── 套餐：模板增改删 + 发券 ──────────────────────────────────────
        td(
            "aigx_plan_manage",
            "套餐管理",
            "高危写：套餐模板增改删 + 按套餐发放 API Key（issue 出券；出券/删除不可逆，需审批）",
            HighRisk,
            "id",
            &["/api/plans", "/api/plans/{id}", "/api/plans/{id}/issue"],
            plan_manage_schema,
            |s, h, a| {
                Box::pin(async move {
                    match require_arg(a, "action")?.as_str() {
                        "upsert" => {
                            let body: admin::plans::PlanRequest =
                                parse_tool_args("aigx_plan_manage", a)?;
                            out(admin::handle_upsert_plan(State(s.clone()), h.clone(), Json(body))
                                .await)
                        }
                        "delete" => {
                            let id = require_arg(a, "id")?;
                            out(admin::handle_delete_plan(State(s.clone()), h.clone(), Path(id))
                                .await)
                        }
                        "issue" => {
                            let id = require_arg(a, "id")?;
                            let name = a
                                .get("token_name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let res = admin::handle_issue_plan_key(
                                State(s.clone()),
                                h.clone(),
                                Path(id),
                                Json(admin::plans::IssueKeyRequest { name }),
                            )
                            .await;
                            Ok(masked_outcome(res))
                        }
                        other => Err((
                            -32602,
                            format!("Invalid action for aigx_plan_manage: {other} (expect upsert/delete/issue)"),
                        )),
                    }
                })
            },
        ),
        // ── 兑换码：批量生成 + 删除（redeem 不入 AI 面：判据1 给本人入账的不可逆价值变更）──
        td(
            "aigx_redemption_manage",
            "兑换码管理",
            "高危写：批量生成兑换码（create-batch，生成即价值凭证）+ 删除未使用兑换码（delete，不可逆，需审批）",
            HighRisk,
            "id",
            &["/api/redemptions/batch", "/api/redemptions/{id}"],
            redemption_manage_schema,
            |s, h, a| {
                Box::pin(async move {
                    match require_arg(a, "action")?.as_str() {
                        "create-batch" => {
                            let body: admin::redemptions::BatchRedemptionRequest =
                                parse_tool_args("aigx_redemption_manage", a)?;
                            let res = admin::handle_batch_redemptions(
                                State(s.clone()),
                                h.clone(),
                                Json(body),
                            )
                            .await;
                            Ok(masked_outcome(res))
                        }
                        "delete" => {
                            let id = require_arg(a, "id")?;
                            out(admin::handle_delete_redemption(
                                State(s.clone()),
                                h.clone(),
                                Path(id),
                            )
                            .await)
                        }
                        other => Err((
                            -32602,
                            format!("Invalid action for aigx_redemption_manage: {other} (expect create-batch/delete)"),
                        )),
                    }
                })
            },
        ),
        // ── 令牌：重置已用量 + 轮换密钥（rotate handler 在 legacy.rs，仅调用不改文件）──
        td(
            "aigx_token_ops",
            "令牌运维",
            "高危写：重置令牌已用量（reset-used）+ 轮换密钥（rotate 出明文，旧 key 立即失效；不可逆，需审批）",
            HighRisk,
            "id",
            &["/api/tokens/{id}/reset_used", "/api/tokens/{id}/rotate"],
            token_ops_schema,
            |s, h, a| {
                Box::pin(async move {
                    let id = require_arg(a, "id")?;
                    match require_arg(a, "action")?.as_str() {
                        "reset-used" => {
                            out(admin::handle_reset_token_used(
                                State(s.clone()),
                                h.clone(),
                                Path(id),
                            )
                            .await)
                        }
                        "rotate" => {
                            let res =
                                admin::handle_rotate_token(State(s.clone()), h.clone(), Path(id))
                                    .await;
                            Ok(masked_outcome(res))
                        }
                        other => Err((
                            -32602,
                            format!("Invalid action for aigx_token_ops: {other} (expect reset-used/rotate)"),
                        )),
                    }
                })
            },
        ),
    ]
}

// ── schema ──────────────────────────────────────────────────────────

fn plan_manage_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "action": { "type": "string", "enum": ["upsert", "delete", "issue"],
                "description": "upsert=创建/更新套餐模板；delete=删除套餐模板；issue=按模板发放 API Key（出券）" },
            "id": { "type": "string", "description": "套餐 ID（delete/issue 必填；upsert 留空=新建）" },
            "name": { "type": "string", "description": "套餐名（upsert）" },
            "price": { "type": "number", "description": "售价（元，展示用）" },
            "quota": { "type": "integer", "description": "额度上限（0=不限）" },
            "duration_days": { "type": "integer", "description": "有效期天数（0=永不过期）" },
            "group": { "type": "string", "description": "计费分组（默认 default）" },
            "allowed_models": { "type": "array", "items": { "type": "string" }, "description": "模型白名单（空=不限）" },
            "description": { "type": "string", "description": "描述（展示用）" },
            "enabled": { "type": "boolean", "description": "是否上架" },
            "plan_type": { "type": "string", "enum": ["once", "subscription"],
                "description": "once=按量发 key；subscription=时长订阅" },
            "token_name": { "type": "string", "description": "issue 时发放的令牌名（留空=套餐名-序号）" }
        },
        "required": ["action"]
    })
}

fn redemption_manage_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "action": { "type": "string", "enum": ["create-batch", "delete"],
                "description": "create-batch=批量生成兑换码；delete=删除未使用的兑换码" },
            "id": { "type": "string", "description": "兑换码 ID（delete 必填）" },
            "count": { "type": "integer", "minimum": 1, "description": "生成张数（create-batch）" },
            "quota": { "type": "integer", "description": "每张面额（配额单位，create-batch）" },
            "name": { "type": "string", "description": "备注名（create-batch）" },
            "expires_at": { "type": "integer", "description": "过期时间戳秒（0=永不过期）" }
        },
        "required": ["action"]
    })
}

fn token_ops_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "action": { "type": "string", "enum": ["reset-used", "rotate"],
                "description": "reset-used=重置令牌已用量；rotate=轮换密钥（旧 key 立即失效；新 key 在工具输出中脱敏）" },
            "id": { "type": "string", "description": "令牌 ID" }
        },
        "required": ["action", "id"]
    })
}

// ── 凭据脱敏（工具输出边界）────────────────────────────────────────────

/// 本单元三条工具响应内**全部**凭据类字段（逐条枚举，防「只修一处」复发）：
/// - `plain_key`：`aigx_plan_manage` issue 出的 API Key（`plans.rs`）；
/// - `key`：`aigx_token_ops` rotate 出的新密钥（`legacy.rs`）；
/// - `code`：`aigx_redemption_manage` create-batch 出的兑换码（可兑换价值凭证）。
const CREDENTIAL_FIELDS: [&str; 3] = ["plain_key", "key", "code"];

/// 对 `HandlerResult` 的 Ok 值先做凭据脱敏、再包成工具结果；Err 走原
/// [`handler_to_outcome`]（错误文案侧已有 `sanitize_error_message`）。
fn masked_outcome(res: HandlerResult) -> ToolOutcome {
    handler_to_outcome(res.map(|Json(v)| Json(mask_credential_fields(v))))
}

/// 工具输出边界的凭据脱敏：递归把 [`CREDENTIAL_FIELDS`] 里的字符串值换成
/// `admin::tokens::mask_key_str` 的结果（与 `mask_token` 同源口径）。人类管理端
/// handler 的响应**不经**此函数——明文只在工具输出边界被遮蔽。
fn mask_credential_fields(mut v: Value) -> Value {
    mask_walk(&mut v);
    v
}

fn mask_walk(v: &mut Value) {
    match v {
        Value::Object(map) => {
            for (k, child) in map.iter_mut() {
                if CREDENTIAL_FIELDS.contains(&k.as_str()) {
                    if let Some(s) = child.as_str() {
                        *child = Value::String(mask_key_str(s));
                    }
                } else {
                    mask_walk(child);
                }
            }
        }
        Value::Array(items) => {
            for it in items.iter_mut() {
                mask_walk(it);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAN_COVERS: &[&str] = &["/api/plans", "/api/plans/{id}", "/api/plans/{id}/issue"];
    const REDEMPTION_COVERS: &[&str] = &["/api/redemptions/batch", "/api/redemptions/{id}"];
    const TOKEN_COVERS: &[&str] = &["/api/tokens/{id}/reset_used", "/api/tokens/{id}/rotate"];

    /// 「列」侧：三条工具声明台账——名称/risk/target_key/covers 逐条钉死，
    /// 且高危不得进 MCP 面。
    #[test]
    fn uf_tools_declared_high_risk_with_covers() {
        let cases: [(&str, &[&str]); 3] = [
            ("aigx_plan_manage", PLAN_COVERS),
            ("aigx_redemption_manage", REDEMPTION_COVERS),
            ("aigx_token_ops", TOKEN_COVERS),
        ];
        for (name, covers) in cases {
            let t = find_tool(name).unwrap_or_else(|| panic!("{name} 未注册"));
            assert_eq!(
                t.risk,
                RiskLevel::HighRisk,
                "{name} 必须 HighRisk（进审批）"
            );
            assert_eq!(t.target_key, "id", "{name} target_key");
            for c in covers {
                assert!(t.covers.contains(c), "{name} 缺 covers {c}");
            }
            assert!(
                mcp_tools().all(|m| m.name != name),
                "{name} 高危不得进 MCP 面"
            );
        }
    }

    /// 「调」侧（输出边界，纯函数）：任一凭据字段都不得残留完整明文——
    /// 逐字符硬断言，而非只看 mask 后的形状。
    #[test]
    fn uf_credential_fields_are_masked_no_plaintext_survives() {
        let key = "sk-ABCDEFGHIJKLMNOP";
        let code = "ABCDEFGHJKLMNPQR";
        let raw = json!({
            "success": true,
            "data": {
                "id": "t1",
                "plain_key": key,
                "key": key,
                "code": code,
                "name": "keep-me",
                "quota": 100
            }
        });
        let masked = mask_credential_fields(raw);
        let s = masked.to_string();
        assert!(!s.contains(key), "模型可见输出残留完整明文 key: {s}");
        assert!(!s.contains(code), "模型可见输出残留完整明文兑换码: {s}");
        // 口径 = mask_key_str（与 mask_token 同源）
        assert_eq!(masked["data"]["plain_key"], json!(mask_key_str(key)));
        assert_eq!(masked["data"]["key"], json!(mask_key_str(key)));
        assert_eq!(masked["data"]["code"], json!(mask_key_str(code)));
        // 非凭据字段原样保留
        assert_eq!(masked["data"]["name"], json!("keep-me"));
        assert_eq!(masked["data"]["quota"], json!(100));
        assert_eq!(masked["data"]["id"], json!("t1"));
        assert_eq!(masked["success"].as_bool(), Some(true));
    }

    /// 数组内的凭据也脱敏（create-batch 返回 `Vec<Redemption>`，`code` 在数组元素里）。
    /// **逐元素**断言：批量出券 ⇒ 每一条都必须被掩码，不能只查第一条
    /// （「只修一处＝补一半」的同型陷阱）。
    #[test]
    fn uf_credential_masking_covers_every_array_element() {
        let codes = ["ABCDEFGHJKLMNPQR", "ZYXWVUTSRQPONML2", "23456789ABCDEFGH"];
        let raw = json!({
            "success": true,
            "data": [
                { "id": "r1", "code": codes[0], "quota": 10 },
                { "id": "r2", "code": codes[1], "quota": 20 },
                { "id": "r3", "code": codes[2], "quota": 30 },
            ]
        });
        let masked = mask_credential_fields(raw);
        let s = masked.to_string();
        for c in codes {
            assert!(!s.contains(c), "数组内兑换码未脱敏（残留完整明文）: {c}");
        }
        for (i, c) in codes.iter().enumerate() {
            assert_eq!(
                masked["data"][i]["code"],
                json!(mask_key_str(c)),
                "data[{i}].code 未按口径掩码"
            );
            assert_eq!(
                masked["data"][i]["id"],
                json!(format!("r{}", i + 1)),
                "data[{i}].id（非凭据字段）不应被改"
            );
        }
    }

    /// 双侧同时钉住：工具输出边界脱敏（上面两条）＋ 人类管理端 handler 仍返回明文。
    /// 静态锚定：三个凭据出口（plain_key / key / code）**逐个正向锚住**，脱敏只出现在
    /// 工具层；三个 handler 源里既有明文出口、又不得引用脱敏函数
    /// （只锚两个出口＝只锚一半，本仓复发形态）。
    #[test]
    fn uf_mask_only_at_tool_boundary_not_in_handlers() {
        let plans = include_str!("../../api/admin/plans.rs");
        let legacy = include_str!("../../api/admin/legacy.rs");
        let redemptions = include_str!("../../api/admin/redemptions.rs");
        let redemption_mod = include_str!("../../redemption/mod.rs");
        // 人类出口仍是明文（未被打码）——三个字段逐个锚：
        assert!(
            plans.contains("\"plain_key\": k.key"),
            "plan issue 人类出口应保持明文"
        );
        assert!(
            legacy.contains("\"key\": k.key"),
            "rotate 人类出口应保持明文"
        );
        assert!(
            redemption_mod.contains("pub code: String"),
            "兑换码字段应明文序列化（人类出口，redemption/mod.rs）"
        );
        assert!(
            redemptions.contains("\"data\": codes"),
            "redemption create-batch 人类出口应保持明文（返回未打码的兑换码）"
        );
        // handler 层不得引用工具层脱敏（掩码只在工具输出边界）：
        for (tag, src) in [
            ("plans", plans),
            ("legacy", legacy),
            ("redemptions", redemptions),
        ] {
            assert!(
                !src.contains("mask_credential_fields") && !src.contains("mask_key_str"),
                "{tag} handler 不得引用工具层脱敏函数"
            );
        }
    }

    /// 「调」侧（执行体源锚定，防「列过滤了、执行侧漏过滤」）：
    /// issue / rotate / create-batch 三处凭据出口都必须走 [`masked_outcome`]。
    /// 单测不构造 `AppState`，故用源码锚定（同 `exec_entries_are_structurally_gated` 手法）。
    ///
    /// needle 在运行时拼接：本测试自己就在被扫描的文件里，写字面量会自匹配。
    #[test]
    fn uf_credential_exits_route_through_masked_outcome() {
        let src = include_str!("w_plans.rs");
        let needle: String = ["Ok(", "masked_outcome("].concat();
        let n = src.matches(&needle).count();
        assert_eq!(
            n, 3,
            "issue/rotate/create-batch 三处凭据出口都须走 masked_outcome，实际 {n} 处"
        );
    }
}
