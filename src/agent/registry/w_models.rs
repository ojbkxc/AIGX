//! 批2 并行单元 U-A（#5：models 写侧 + G3 探活族 + G4 刷新族）的工具声明文件 ——
//! **文件独占**：仅本单元 owner 编辑。
//!
//! 四条工具：
//! - `aigx_model_manage`（**HIGH**）：别名思考等级映射（alias-reasoning）/ 别名启停
//!   （alias-state）/ 能力标签覆盖（capabilities）/ 自定义模型增删（custom-add /
//!   custom-remove）。别名变更与自定义模型删除会改变**路由解析结果**（一个模型名解到
//!   谁、还存不存在），删错＝线上请求打空 ⇒ 整条工具 HighRisk 进审批。
//! - `aigx_model_meta_manage`（LOW）：模型元信息覆盖 set/delete（owned_by/上下文/
//!   能力/思考等级候选）——只改展示与推断元数据，可被内置推断覆盖、可逆 ⇒ LowRisk。
//! - `aigx_probe`（**HIGH**）：上游探活——模型测试（model-test）、渠道对话调试
//!   （chat-test）、拉取渠道模型（fetch-models）、代理连通性（pool-test /
//!   pool-test-account）、账号可用性（account-test）。**每次调用都会真实打上游**，
//!   chat-test 走完整转发链并**可能产生真实计费** ⇒ 不敢当无副作用，整族 HighRisk。
//! - `aigx_refresh`（LOW，`target_key=view`）：刷新/重算——模型目录（models）、
//!   models.dev 元数据（metadata）、用量汇总（usage）、价格同步（pricing）。
//!   纯投影/幂等重算：重跑不会让任何用户余额变少、权益变没 ⇒ LowRisk（判据沿用 57 §2 G4）。
//!
//! 边界：本单元**只调用**已 `pub` 的 handler，**不修改** `model_manage.rs` / `pricing.rs` /
//! `model_test.rs` / `model_metadata.rs` / `legacy.rs`（U-C）/ `proxy_pool_api.rs`（U-E）
//! 等非独占文件。执行体复用 `super::*`；assembler 在 [`super::build_tools`]，owner 不回改 `mod.rs`。

use super::*;
use RiskLevel::{HighRisk, LowRisk};

pub(super) fn tools() -> Vec<ToolDef> {
    vec![
        // ── 模型写侧：别名/能力/自定义模型（改路由解析 ⇒ HIGH）──────────────
        td(
            "aigx_model_manage",
            "模型写侧管理",
            "高危写：模型写侧统一入口——别名思考等级映射（alias-reasoning）、别名启停\
             （alias-state）、能力标签覆盖（capabilities）、自定义模型增删（custom-add/custom-remove）。\
             别名变更与自定义模型删除会改变模型名的路由解析结果（解到谁 / 是否还存在），属高危写，需审批。",
            HighRisk,
            "",
            &[
                "/api/models/alias/reasoning",
                "/api/models/alias/state",
                "/api/models/capabilities",
                "/api/models/custom",
                "/api/models/custom/remove",
            ],
            model_manage_schema,
            |s, h, a| {
                Box::pin(async move {
                    match require_arg(a, "action")?.as_str() {
                        "alias-state" => {
                            let body: admin::model_manage::AliasStateBody =
                                parse_tool_args("aigx_model_manage", a)?;
                            out(admin::model_manage::handle_set_alias_state(
                                State(s.clone()),
                                h.clone(),
                                Json(body),
                            )
                            .await)
                        }
                        "alias-reasoning" => {
                            let body: admin::model_manage::AliasReasoningBody =
                                parse_tool_args("aigx_model_manage", a)?;
                            out(admin::model_manage::handle_set_alias_reasoning(
                                State(s.clone()),
                                h.clone(),
                                Json(body),
                            )
                            .await)
                        }
                        "capabilities" => {
                            let body: admin::model_manage::CapabilitiesBody =
                                parse_tool_args("aigx_model_manage", a)?;
                            out(admin::model_manage::handle_set_capabilities(
                                State(s.clone()),
                                h.clone(),
                                Json(body),
                            )
                            .await)
                        }
                        "custom-add" => {
                            let body: admin::model_manage::CustomModelBody =
                                parse_tool_args("aigx_model_manage", a)?;
                            out(admin::model_manage::handle_add_custom_model(
                                State(s.clone()),
                                h.clone(),
                                Json(body),
                            )
                            .await)
                        }
                        "custom-remove" => {
                            let body: admin::model_manage::CustomModelRemoveBody =
                                parse_tool_args("aigx_model_manage", a)?;
                            out(admin::model_manage::handle_remove_custom_model(
                                State(s.clone()),
                                h.clone(),
                                Json(body),
                            )
                            .await)
                        }
                        other => Err((
                            -32602,
                            format!(
                                "Invalid action for aigx_model_manage: {other} \
                                 (expect alias-state/alias-reasoning/capabilities/custom-add/custom-remove)"
                            ),
                        )),
                    }
                })
            },
        ),
        // ── 模型元信息覆盖（改展示/推断元数据，可逆 ⇒ LOW）──────────────────
        td(
            "aigx_model_meta_manage",
            "模型元信息覆盖",
            "写操作（可回滚）：设置/删除某模型的元信息覆盖（owned_by/上下文长度/能力标签/思考等级候选）。\
             覆盖写入 ModelMetadataRegistry，只影响展示与推断元数据、可被内置推断覆盖、可删除，故 LowRisk。",
            LowRisk,
            "model_name",
            &["/api/models/meta/{model}"],
            model_meta_schema,
            |s, h, a| {
                Box::pin(async move {
                    let model_name = require_arg(a, "model_name")?;
                    match require_arg(a, "action")?.as_str() {
                        "set" => {
                            let body: admin::pricing::ModelMetaRequest =
                                parse_tool_args("aigx_model_meta_manage", a)?;
                            out(admin::pricing::handle_model_meta_set(
                                State(s.clone()),
                                h.clone(),
                                Path(model_name),
                                Json(body),
                            )
                            .await)
                        }
                        "delete" => {
                            out(admin::pricing::handle_model_meta_delete(
                                State(s.clone()),
                                h.clone(),
                                Path(model_name),
                            )
                            .await)
                        }
                        other => Err((
                            -32602,
                            format!(
                                "Invalid action for aigx_model_meta_manage: {other} (expect set/delete)"
                            ),
                        )),
                    }
                })
            },
        ),
        // ── G3 探活族：真实打上游、可能计费 ⇒ HIGH ────────────────────────
        td(
            "aigx_probe",
            "上游探活",
            "高危写：向上游发真实请求的探活族——模型测试（model-test）、渠道对话调试（chat-test）、\
             拉取渠道模型（fetch-models）、代理池连通性（pool-test/pool-test-account）、账号可用性（account-test）。\
             每次调用都会真实打上游；chat-test 走完整转发链并可能产生真实上游计费——非无副作用，需审批。",
            HighRisk,
            "",
            &[
                "/api/models/test",
                "/api/channels/chat_test",
                "/api/channels/fetch_models",
                "/api/proxy-pool/test",
                "/api/proxy-pool/test-account",
                "/api/accounts/test",
            ],
            probe_schema,
            |s, h, a| {
                Box::pin(async move {
                    match require_arg(a, "action")?.as_str() {
                        "model-test" => {
                            let body: admin::model_test::ModelTestRequest =
                                parse_tool_args("aigx_probe", a)?;
                            masked_outcome(
                                admin::handle_model_test(State(s.clone()), h.clone(), Json(body))
                                    .await,
                            )
                        }
                        "chat-test" => {
                            // 强制非流式：探活要的是一次性确定结论，不做 SSE 增量
                            // （handler 的 `stream=true` 默认会回 SSE，工具侧吃不下增量）。
                            let mut v = a.clone();
                            if let Some(obj) = v.as_object_mut() {
                                obj.insert("stream".to_string(), json!(false));
                            }
                            let body: admin::legacy::ChannelChatTestRequest =
                                parse_tool_args("aigx_probe", &v)?;
                            let resp = admin::handle_channel_chat_test(
                                State(s.clone()),
                                h.clone(),
                                Json(body),
                            )
                            .await;
                            Ok(response_to_outcome(resp).await)
                        }
                        "fetch-models" => {
                            let body: admin::legacy::FetchModelsRequest =
                                parse_tool_args("aigx_probe", a)?;
                            masked_outcome(
                                admin::handle_fetch_channel_models(
                                    State(s.clone()),
                                    h.clone(),
                                    Json(body),
                                )
                                .await,
                            )
                        }
                        "pool-test" => {
                            let body: admin::proxy_pool_api::IdBody =
                                parse_tool_args("aigx_probe", a)?;
                            masked_outcome(
                                admin::proxy_pool_api::handle_pool_test(
                                    State(s.clone()),
                                    h.clone(),
                                    Json(body),
                                )
                                .await,
                            )
                        }
                        "pool-test-account" => {
                            let body: admin::proxy_pool_api::TestAccountBody =
                                parse_tool_args("aigx_probe", a)?;
                            masked_outcome(
                                admin::proxy_pool_api::handle_pool_test_account(
                                    State(s.clone()),
                                    h.clone(),
                                    Json(body),
                                )
                                .await,
                            )
                        }
                        "account-test" => {
                            let body: admin::legacy::AccountRequest =
                                parse_tool_args("aigx_probe", a)?;
                            masked_outcome(
                                admin::handle_test_account(State(s.clone()), h.clone(), Json(body))
                                    .await,
                            )
                        }
                        other => Err((
                            -32602,
                            format!(
                                "Invalid action for aigx_probe: {other} \
                                 (expect model-test/chat-test/fetch-models/pool-test/pool-test-account/account-test)"
                            ),
                        )),
                    }
                })
            },
        ),
        // ── G4 刷新/重算：纯投影、幂等 ⇒ LOW ─────────────────────────────
        td(
            "aigx_refresh",
            "刷新与重算",
            "写操作（可回滚）：幂等刷新/重算——模型目录（models）、models.dev 元数据（metadata）、\
             用量汇总（usage）、价格同步（pricing）。重跑不会让任何用户余额变少或权益变没，故 LowRisk。",
            LowRisk,
            "view",
            &[
                "/api/models/refresh",
                "/api/models/metadata/refresh",
                "/api/usage/summary",
                "/api/pricing/sync",
            ],
            refresh_schema,
            |s, h, a| {
                Box::pin(async move {
                    match require_arg(a, "view")?.as_str() {
                        "models" => {
                            out(admin::handle_refresh_models(State(s.clone()), h.clone()).await)
                        }
                        "metadata" => {
                            out(admin::handle_refresh_model_metadata(
                                State(s.clone()),
                                h.clone(),
                            )
                            .await)
                        }
                        "usage" => {
                            out(admin::handle_refresh_usage(State(s.clone()), h.clone()).await)
                        }
                        "pricing" => {
                            out(admin::handle_trigger_price_sync(State(s.clone()), h.clone())
                                .await)
                        }
                        other => Err((
                            -32602,
                            format!(
                                "Invalid action for aigx_refresh: {other} \
                                 (expect models/metadata/usage/pricing)"
                            ),
                        )),
                    }
                })
            },
        ),
    ]
}

// ── schema ──────────────────────────────────────────────────────────

fn model_manage_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "action": { "type": "string",
                "enum": ["alias-reasoning", "alias-state", "capabilities", "custom-add", "custom-remove"],
                "description": "alias-reasoning=设置别名的思考等级；alias-state=启停别名；capabilities=覆盖模型能力标签；custom-add/custom-remove=增删自定义模型" },
            "alias": { "type": "string", "description": "alias-state：别名（必填）" },
            "enabled": { "type": "boolean", "description": "alias-state：启用/禁用（必填）" },
            "aliases": { "type": "array", "items": { "type": "string" },
                "description": "alias-reasoning：要设置思考等级的别名列表（必填）" },
            "effort": { "type": "string", "description": "alias-reasoning：思考强度（off/low/medium/high）" },
            "model": { "type": "string", "description": "capabilities：模型名（必填）" },
            "capabilities": { "type": "array", "items": { "type": "string" },
                "description": "capabilities：能力标签集（chat/vision/tools/reasoning/embedding/rerank/image/audio/video；空数组=无标签）" },
            "provider": { "type": "string", "description": "custom-add/custom-remove：provider（渠道类型或账号 provider，必填）" },
            "id": { "type": "string", "description": "custom-add/custom-remove：模型 ID（必填）" }
        },
        "required": ["action"]
    })
}

fn model_meta_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "action": { "type": "string", "enum": ["set", "delete"],
                "description": "set=设置元信息覆盖；delete=删除元信息覆盖" },
            "model_name": { "type": "string", "description": "模型名（路径参数）" },
            "owned_by": { "type": "string", "description": "set：归属方（必填）" },
            "context_length": { "type": "integer", "description": "set：上下文长度（token）" },
            "capabilities": { "type": "array", "items": { "type": "string" }, "description": "set：能力标签集" },
            "reasoning_levels": { "type": "array", "items": { "type": "string" }, "description": "set：思考等级候选" }
        },
        "required": ["action", "model_name"]
    })
}

fn probe_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "action": { "type": "string",
                "enum": ["model-test", "chat-test", "fetch-models", "pool-test", "pool-test-account", "account-test"],
                "description": "model-test=钉 provider+model 走真实链路测试；chat-test=渠道对话调试（会真实转发、可能计费）；fetch-models=按渠道/凭据拉取上游模型清单；pool-test=测代理池条目连通性；pool-test-account=测账号生效代理；account-test=测账号可用性" },
            "provider": { "type": "string", "description": "model-test：provider（必填）" },
            "model": { "type": "string", "description": "model-test：模型 id；chat-test：目标模型（留空=渠道首个）" },
            "account_id": { "type": "string", "description": "model-test / pool-test-account：钉住的账号 id" },
            "channel_id": { "type": "string", "description": "model-test / fetch-models / chat-test：钉住的渠道 id（chat-test 留空=自动选路）" },
            "prompt": { "type": "string", "description": "model-test：测试提示词（缺省「你好」）" },
            "system_prompt": { "type": "string", "description": "model-test：系统提示词" },
            "reasoning_effort": { "type": "string", "description": "model-test / chat-test：思考强度 off/low/medium/high" },
            "message": { "type": ["string", "array"], "description": "chat-test：当前用户消息（字符串或 OpenAI content blocks，必填）" },
            "history": { "type": "array", "items": { "type": "object" }, "description": "chat-test：对话历史（role/content）" },
            "protocol": { "type": "string", "enum": ["openai", "anthropic"], "description": "chat-test：上游协议（缺省 openai）" },
            "temperature": { "type": "number", "description": "chat-test：采样温度" },
            "top_p": { "type": "number", "description": "chat-test：top_p" },
            "max_tokens": { "type": "integer", "description": "chat-test：最大生成 token" },
            "channel_type": { "type": "string", "description": "fetch-models：渠道类型（或直接给 channel_id）" },
            "base_url": { "type": "string", "description": "fetch-models：上游 base url（内网 SSRF 会被拒）" },
            "api_key": { "type": "string", "description": "fetch-models：上游凭据（仅入参，不回显）" },
            "cleanup": { "type": "boolean", "description": "fetch-models：拉取后是否清理失效映射（缺省 true）" },
            "id": { "type": "string", "description": "pool-test：代理池条目 id（必填）" },
            "proxy": { "type": "string", "description": "pool-test-account：临时代理串（pool:/rotate 编码或 URL；与 account_id 二选一）" },
            "name": { "type": "string", "description": "account-test：账号名" },
            "api_token": { "type": "string", "description": "account-test：账号凭据（仅入参，不回显）" },
            "status": { "type": "string", "description": "account-test：账号状态（可空）" }
        },
        "required": ["action"]
    })
}

fn refresh_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "view": { "type": "string", "enum": ["models", "metadata", "usage", "pricing"],
                "description": "models=刷新渠道模型目录；metadata=刷新 models.dev 元数据；usage=重算用量汇总；pricing=触发价格同步" }
        },
        "required": ["view"]
    })
}

// ── 执行体辅助 ──────────────────────────────────────────────────────

/// `handle_channel_chat_test` 返回 `axum::response::Response`（成功是 JSON、流式是
/// SSE），进不了 [`out`] 的 `HandlerResult` 口径。本辅助读完整响应体转成工具结果：
/// - 非 2xx：响应体是 `{success:false,error:…}` ⇒ 过 `sanitize_error_message`（与
///   [`handler_to_outcome`] 同口径），避免把上游错误原文（可能含凭据）回灌模型；
/// - 2xx：原样文本，按 [`crate::api::admin::common::truncate_tool_text`] 16K 截断。
///
/// 两条分支的文本都再过 [`mask_proxy_credentials`]：上游回显里可能出现带口令的 URL。
async fn response_to_outcome(resp: axum::response::Response) -> ToolOutcome {
    let status = resp.status();
    let bytes = match axum::body::to_bytes(resp.into_body(), 256 * 1024).await {
        Ok(b) => b,
        Err(e) => {
            return ToolOutcome {
                text: format!("读取上游响应失败：{e}"),
                ok: false,
            }
        }
    };
    let text = String::from_utf8_lossy(&bytes).to_string();
    if status.is_success() {
        ToolOutcome {
            text: crate::api::admin::common::truncate_tool_text(mask_proxy_credentials(&text)),
            ok: true,
        }
    } else {
        let msg = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
            .unwrap_or(text);
        ToolOutcome {
            text: crate::error_translate::sanitize_error_message(&mask_proxy_credentials(&msg)),
            ok: false,
        }
    }
}

// ── 探活输出的凭据脱敏（工具输出边界）────────────────────────────────
//
// 探活族会把上游/代理的原始错误文本回灌给模型。**已枚举**的凭据面（逐条）：
// 1. `pool-test` → 顶层 `error`：`proxy_pool/mod.rs:653` 的
//    `format!("invalid proxy '{url}': {e}")`，`url` 来自 `entry_url()`（`user:pass@` 明文）；
// 2. `pool-test` → `items[].lastTest.error`：**同一根因的第二个出口**——
//    `record_test()` 把上一条 error 落库，`public()` 原样带回 `lastTest`；
// 3. `pool-test-account` → 顶层 `error`（同 1）；
// 4. `pool-test-account` → `proxy`（label；`describe_proxy` 已脱敏，此处二次兜底）；
// 5. `fetch-models` → `data.url`：用户传入的 `base_url` 若内嵌 `user:pass@` 会原样回显；
// 6. `model-test` / `chat-test` → `content` / `error`：上游回显里可能带 URL 凭据。
//
// ⇒ 处置不是「点名改 `error` 一处」（本仓「只修一处＝补一半」复发过三次），而是**递归
// 遍历整条工具输出的每一个字符串**做内嵌 URL 凭据脱敏；非 URL 文本不受影响。
// 人类管理端（`proxy_pool_api.rs` / `proxy_pool/mod.rs`）**保持原样**——脱敏只在工具输出边界。

/// 对 `HandlerResult` 的 Ok 值先做凭据脱敏、再包成工具结果；Err 走原 [`handler_to_outcome`]。
fn masked_outcome(res: HandlerResult) -> ExecResult {
    Ok(handler_to_outcome(
        res.map(|Json(v)| Json(mask_probe_output(v))),
    ))
}

/// 递归脱敏：对 JSON 树里每个字符串跑 [`mask_proxy_credentials`]。
fn mask_probe_output(mut v: Value) -> Value {
    mask_walk(&mut v);
    v
}

fn mask_walk(v: &mut Value) {
    match v {
        Value::Object(map) => {
            for (_, child) in map.iter_mut() {
                mask_walk(child);
            }
        }
        Value::Array(items) => {
            for it in items.iter_mut() {
                mask_walk(it);
            }
        }
        Value::String(s) => {
            let masked = mask_proxy_credentials(s);
            *s = masked;
        }
        _ => {}
    }
}

/// 内嵌 URL 的代理凭据脱敏：把 `scheme://user:pass@host:port` 的口令段换成 `***`
/// （保留 scheme / user / host / port，形态对齐 `pool::describe_proxy` 的「不泄露
/// 认证信息」）。
///
/// 纯字符串扫描（不引 regex）：`://` 前的标识符段须是合法 scheme（字母开头，后续
/// 字母数字 `+ - .`），authority 段取到第一个终止符（空白、引号、`/ ? # < > ,`），
/// authority 内取**最后一个** `@`，其左侧须含 `:`（= userinfo）才脱敏。
/// 无 `://` / 无 `@` / userinfo 无 `:` 的字符串原样返回。
fn mask_proxy_credentials(s: &str) -> String {
    const TERM: &[char] = &[
        ' ', '\t', '\n', '\r', '"', '\'', '<', '>', '/', '?', '#', ',', '}', ']',
    ];
    let is_scheme_char = |c: char| c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.';
    let mut out = String::with_capacity(s.len());
    let mut i = 0usize;
    while i < s.len() {
        let Some(rel) = s[i..].find("://") else {
            out.push_str(&s[i..]);
            break;
        };
        let scheme_end = i + rel;
        // scheme 段：从 scheme_end 往前退到第一个非 scheme 字符之后
        let scheme_start = s[..scheme_end]
            .char_indices()
            .rev()
            .find(|(_, c)| !is_scheme_char(*c))
            .map(|(idx, c)| idx + c.len_utf8())
            .unwrap_or(0);
        let scheme = &s[scheme_start..scheme_end];
        if scheme.is_empty() || !scheme.starts_with(|c: char| c.is_ascii_alphabetic()) {
            // 不是 URL scheme（例如被折行的普通文本）：原样透出这一段，继续扫描
            out.push_str(&s[i..scheme_end + 3]);
            i = scheme_end + 3;
            continue;
        }
        let auth_start = scheme_end + 3;
        let auth_end = s[auth_start..]
            .find(|c| TERM.contains(&c))
            .map(|p| auth_start + p)
            .unwrap_or(s.len());
        let authority = &s[auth_start..auth_end];
        out.push_str(&s[i..scheme_end]);
        out.push_str("://");
        match authority.rfind('@') {
            Some(at) if authority[..at].contains(':') => {
                // userinfo = `user:pass`；只留 `user`，口令换 `***`
                let first_colon = authority[..at].find(':').unwrap_or(at);
                out.push_str(&authority[..first_colon]);
                out.push_str(":***@");
                out.push_str(&authority[at + 1..]);
            }
            _ => out.push_str(authority),
        }
        i = auth_end;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const MODEL_MANAGE_COVERS: &[&str] = &[
        "/api/models/alias/reasoning",
        "/api/models/alias/state",
        "/api/models/capabilities",
        "/api/models/custom",
        "/api/models/custom/remove",
    ];
    const MODEL_META_COVERS: &[&str] = &["/api/models/meta/{model}"];
    const PROBE_COVERS: &[&str] = &[
        "/api/models/test",
        "/api/channels/chat_test",
        "/api/channels/fetch_models",
        "/api/proxy-pool/test",
        "/api/proxy-pool/test-account",
        "/api/accounts/test",
    ];
    const REFRESH_COVERS: &[&str] = &[
        "/api/models/refresh",
        "/api/models/metadata/refresh",
        "/api/usage/summary",
        "/api/pricing/sync",
    ];

    /// 「列」侧：四条工具声明台账——name/risk/target_key/covers 逐条钉死；
    /// 高危不得进 MCP 面，低危必须进（MCP 只暴露非高危）。
    #[test]
    fn ua_tools_declared_with_expected_risk_and_covers() {
        let cases: Vec<(&str, RiskLevel, &str, &[&str])> = vec![
            (
                "aigx_model_manage",
                RiskLevel::HighRisk,
                "",
                MODEL_MANAGE_COVERS,
            ),
            (
                "aigx_model_meta_manage",
                RiskLevel::LowRisk,
                "model_name",
                MODEL_META_COVERS,
            ),
            ("aigx_probe", RiskLevel::HighRisk, "", PROBE_COVERS),
            ("aigx_refresh", RiskLevel::LowRisk, "view", REFRESH_COVERS),
        ];
        for (name, risk, target_key, covers) in cases {
            let t = find_tool(name).unwrap_or_else(|| panic!("{name} 未注册"));
            assert_eq!(t.risk, risk, "{name} risk 口径");
            assert_eq!(t.target_key, target_key, "{name} target_key");
            for c in covers {
                assert!(t.covers.contains(c), "{name} 缺 covers {c}");
            }
            let in_mcp = mcp_tools().any(|m| m.name == name);
            assert_eq!(
                in_mcp,
                risk != RiskLevel::HighRisk,
                "{name} 的 MCP 可见性与 risk 不符（高危必须不可见）"
            );
        }
    }

    /// 探活族的**计费提醒**（57 §2 G3）：`aigx_probe` 的描述必须写明「打上游 / 可能计费」，
    /// 不得写成无副作用——文案是给模型判断的依据，写错＝模型把它当安全只读工具反复调。
    #[test]
    fn ua_probe_description_flags_upstream_billing() {
        let t = find_tool("aigx_probe").expect("aigx_probe 未注册");
        assert!(
            t.description.contains("真实") && t.description.contains("计费"),
            "探活工具描述必须点明真实打上游 + 可能计费，实际: {}",
            t.description
        );
        assert!(
            t.description.contains("高危写"),
            "探活族必须标高危写（真实打上游、可能计费）"
        );
    }

    /// 「调」侧（执行体源锚定，防「列对了、执行体接错 handler」）：四条工具每个 action
    /// 都必须真的接到对应 handler 符号上。单测不构造 `AppState`，故用源码锚定
    /// （同 `exec_entries_are_structurally_gated` 手法）。needle 运行时拼接：本测试自己
    /// 就在被 `include_str!` 扫描的文件里，写字面量会自匹配。
    #[test]
    fn ua_exec_bodies_route_to_expected_handlers() {
        let src = include_str!("w_models.rs");
        let count = |a: &str, b: &str| src.matches(&[a, b].concat()).count();
        // aigx_model_manage 五个 action
        for (a, b, label) in [
            (
                "admin::model_manage::handle_set_alias",
                "_state(",
                "alias-state",
            ),
            (
                "admin::model_manage::handle_set_alias",
                "_reasoning(",
                "alias-reasoning",
            ),
            (
                "admin::model_manage::handle_set",
                "_capabilities(",
                "capabilities",
            ),
            (
                "admin::model_manage::handle_add",
                "_custom_model(",
                "custom-add",
            ),
            (
                "admin::model_manage::handle_remove",
                "_custom_model(",
                "custom-remove",
            ),
            // aigx_model_meta_manage
            ("admin::pricing::handle_model_meta", "_set(", "meta set"),
            (
                "admin::pricing::handle_model_meta",
                "_delete(",
                "meta delete",
            ),
            // aigx_probe
            ("admin::handle_model", "_test(", "model-test"),
            ("admin::handle_channel_chat", "_test(", "chat-test"),
            ("admin::handle_fetch_channel", "_models(", "fetch-models"),
            ("admin::proxy_pool_api::handle_pool", "_test(", "pool-test"),
            (
                "admin::proxy_pool_api::handle_pool_test",
                "_account(",
                "pool-test-account",
            ),
            ("admin::handle_test", "_account(", "account-test"),
            // aigx_refresh
            ("admin::handle_refresh", "_models(", "refresh models"),
            (
                "admin::handle_refresh_model",
                "_metadata(",
                "refresh metadata",
            ),
            ("admin::handle_refresh", "_usage(", "refresh usage"),
            (
                "admin::handle_trigger_price",
                "_sync(",
                "trigger price sync",
            ),
        ] {
            assert!(count(a, b) >= 1, "{label} 执行体没接到预期 handler 符号");
        }
        // chat-test 那条 handler 返回 Response，必须走 response_to_outcome 适配
        assert_eq!(
            count("response_to", "_outcome("),
            // 1 处定义 + 1 处调用
            2,
            "chat-test 必须经 response_to_outcome 适配 Response→ToolOutcome"
        );
    }

    /// 「调」侧（纯函数）：response_to_outcome 的两条分支——非 2xx 走
    /// sanitize_error_message，2xx 原样透出。
    #[tokio::test]
    async fn ua_probe_response_to_outcome_sanitizes_errors() {
        // 2xx：JSON 原样
        let ok_resp = axum::response::Response::builder()
            .status(axum::http::StatusCode::OK)
            .body(axum::body::Body::from(
                "{\"success\":true,\"data\":{\"content\":\"hi\"}}",
            ))
            .unwrap();
        let ok = response_to_outcome(ok_resp).await;
        assert!(ok.ok, "2xx 应判成功");
        assert!(ok.text.contains("hi"), "2xx 文本应原样透出: {}", ok.text);
        // 非 2xx：取 error 字段并脱敏
        let err_resp = axum::response::Response::builder()
            .status(axum::http::StatusCode::BAD_GATEWAY)
            .body(axum::body::Body::from(
                "{\"success\":false,\"error\":\"Upstream HTTP 502: bad\"}",
            ))
            .unwrap();
        let err = response_to_outcome(err_resp).await;
        assert!(!err.ok, "非 2xx 应判失败");
        assert!(
            err.text.contains("Upstream HTTP 502"),
            "错误文本应含原因: {}",
            err.text
        );
        assert!(
            !err.text.contains("{\"success\""),
            "错误分支应取 error 字段，不回灌整段 JSON: {}",
            err.text
        );
    }

    /// 探活族输出里**每一个**凭据出口都必须脱敏（「只修一处＝补一半」的正面处置）：
    /// 顶层 `error`、`items[].lastTest.error`（同根因的第二个出口：落库后经
    /// `pool::public()` 原样带回）、`data.url`（用户传的 base_url 可能内嵌凭据）、
    /// `proxy` label——**逐处**断言明文口令不残留。
    #[test]
    fn ua_probe_output_masks_credentials_at_every_exit() {
        let secret = "s3cr3t-P4ss";
        let url = format!("http://proxyuser:{secret}@203.0.113.9:8080");
        let masked_form = "http://proxyuser:***@203.0.113.9:8080";
        let raw = json!({
            "success": false,
            "error": format!("invalid proxy '{url}': builder error"),
            "proxy": masked_form,
            "data": { "url": url.clone() },
            "items": [
                { "id": "px_1", "name": "hk",
                  "lastTest": { "error": format!("invalid proxy '{url}': bad"), "ip": "1.2.3.4" } },
                { "id": "px_2", "name": "us",
                  "lastTest": { "error": Value::Null, "ip": "5.6.7.8" } }
            ]
        });
        let masked = mask_probe_output(raw);
        let s = masked.to_string();
        // 明文口令在整棵树上**一个都不许剩**
        assert!(!s.contains(secret), "探活输出残留完整明文代理口令: {s}");
        assert!(
            !s.contains(&format!("proxyuser:{secret}")),
            "探活输出残留明文 user:pass: {s}"
        );
        // 逐处断言形状（不是只看「没有明文」，而是「被正确遮到 host 前」）
        assert_eq!(
            masked["error"],
            json!(format!("invalid proxy '{masked_form}': builder error"))
        );
        assert_eq!(
            masked["items"][0]["lastTest"]["error"],
            json!(format!("invalid proxy '{masked_form}': bad"))
        );
        assert_eq!(masked["data"]["url"], json!(masked_form));
        assert_eq!(masked["proxy"], json!(masked_form));
        // 非凭据字段原样保留
        assert_eq!(masked["items"][0]["name"], json!("hk"));
        assert_eq!(masked["items"][0]["lastTest"]["ip"], json!("1.2.3.4"));
        assert_eq!(masked["items"][1]["lastTest"]["error"], Value::Null);
        assert_eq!(masked["success"], json!(false));
    }

    /// `mask_proxy_credentials` 纯函数边界：无 `@` / userinfo 无 `:` / 非 URL 文本 /
    /// 一个串里多个 URL / 无 `://` 一律不得误伤；带 `'` 引号包裹的真实形态必须命中。
    #[test]
    fn ua_mask_proxy_credentials_edges() {
        let m = mask_proxy_credentials;
        // 命中：引号包裹（错误消息真实形态）
        assert_eq!(
            m("invalid proxy 'socks5://u:pw@10.0.0.1:1080': bad"),
            "invalid proxy 'socks5://u:***@10.0.0.1:1080': bad"
        );
        // 无口令（只有 user）
        assert_eq!(m("http://user@host:80/x"), "http://user@host:80/x");
        // 无 userinfo
        assert_eq!(m("http://host:80/x"), "http://host:80/x");
        // 非 URL 文本（无 ://）原样
        assert_eq!(m("连接超时（8 秒）"), "连接超时（8 秒）");
        // 非 scheme 的 `://`（`3` 不以字母开头 ⇒ 不是 URL scheme）原样
        assert_eq!(m("比率 3://x:y@z"), "比率 3://x:y@z");
        // 一个串里多个 URL，逐段脱敏
        assert_eq!(
            m("first http://u1:p1@h1:1 then http://u2:p2@h2:2"),
            "first http://u1:***@h1:1 then http://u2:***@h2:2"
        );
        // 口令本身含 `@`（畸形但需不 panic，取最后一个 @ 作分隔）
        assert_eq!(m("http://u:p@ss@h:80"), "http://u:***@h:80");
    }

    /// 双侧同时钉住：工具输出边界脱敏 ＋ **人类管理端接口不变**（脱敏只在工具层）。
    /// 静态锚定：泄漏源仍在 `proxy_pool/mod.rs`（管理端原样），工具层不得改它，
    /// `proxy_pool_api.rs` 也不得引用工具层脱敏函数。
    #[test]
    fn ua_mask_only_at_tool_boundary_not_in_handlers() {
        let pool = include_str!("../../proxy_pool/mod.rs");
        let api = include_str!("../../api/admin/proxy_pool_api.rs");
        // 管理端错误文本仍是明文（本单元不改 handler 层）
        assert!(
            pool.contains("invalid proxy '"),
            "管理端 proxy_pool/mod.rs 的原始错误文本不应被改"
        );
        // 人类管理端不得引用工具层脱敏函数（掩码只在工具输出边界）
        for (tag, src) in [("proxy_pool/mod.rs", pool), ("proxy_pool_api.rs", api)] {
            assert!(
                !src.contains("mask_proxy_credentials") && !src.contains("mask_probe_output"),
                "{tag} 不得引用工具层脱敏函数"
            );
        }
    }

    /// 「调」侧（执行体源锚定）：探活族**每个**返回 `HandlerResult` 的 action 都必须走
    /// [`masked_outcome`]（chat-test 走 `response_to_outcome`，其内部也脱敏）。
    /// needle 运行时拼接防自匹配（且本测试函数名刻意不以脱敏函数名结尾，免得自匹配）。
    #[test]
    fn ua_probe_arms_wrap_output_via_mask_helper() {
        let src = include_str!("w_models.rs");
        let n = src.matches(&["masked", "_outcome("].concat()).count();
        // 1 处定义 + 5 处调用（model-test/fetch-models/pool-test/pool-test-account/account-test）
        assert_eq!(
            n, 6,
            "探活族除 chat-test 外的 5 个 action 都须经工具输出边界脱敏，实际 {n} 处"
        );
    }
}
