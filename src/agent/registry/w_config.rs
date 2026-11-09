//! 批2 并行单元 U-C（#34：`aigx_config_set` 配置写侧统一入口 + G8 enum 尾巴）的工具声明文件 —— **文件独占**：仅本单元 owner 编辑。
//!
//! ## 一条工具，key 白名单（lead 裁定③）
//!
//! 配置写侧与批1 的只读 `aigx_config_view` 对称：**一条 `aigx_config_set` + key enum**，
//! enum 集合 = 读侧同名子集（能读才能写，写侧不许有读侧没有的 key）。每个 key 直调
//! 对应的既有 `pub` 管理面 handler（内部 `verify_admin`），不新增路由、不改 handler。
//!
//! ## 风险与安全控制面（lead 裁定②）
//!
//! 全局热生效配置一律 `HighRisk`（`runner.rs` 只对 HighRisk 挂审批 ⇒ 自动进审批矩阵）。
//! 其中 `ipFilter` 与 `routing` 属**安全控制面**：AI 关 WAF / 解粘滞 / 改 IP 黑白名单
//! = 自我解除外部约束，描述里写死禁令——**不得用于解除自身访问限制**。
//!
//! ## 覆盖台账
//!
//! `covers` 列全部写路径；补掉一条缺口就删 `tests.rs` 里对应台账行，否则闸门 B/E 红。
//!
//! 执行体复用 `super::*`；assembler 在 [`super::build_tools`]，本文件 owner 不回改 `mod.rs`。

use super::*;
use serde::de::DeserializeOwned;
use RiskLevel::HighRisk;

/// 全部可写 key（与读侧 `aigx_config_view` 同名；顺序 = 读侧 enum 顺序 + 本批新补 4 个）。
/// 本批对读侧 enum 新增 4 key：`zcodeCaptchaConfig`（G8）、`chatSettings`、`zcodeCaptchaPool`、
/// `networkConfig`；`logsRetention` 读侧本就有（写侧沿用同名）。`userToolsPolicy` **不在**本表
/// ——它改的是 AI 自身可执行面，已按 lead 裁定移出 AI 面。
/// 本文件的 `uc_every_key_has_dispatch_arm` 逐个锚住「key 不落空」。
const CONFIG_SET_KEYS: [&str; 26] = [
    "settings",
    "limits",
    "securityPolicy",
    "sanitize",
    "gatewaySettings",
    "gatewayPrompt",
    "updateSettings",
    "logsRetention",
    "queue",
    "ratelimit",
    "ratios",
    "ipFilter",
    "notify",
    "clineHeaders",
    "routing",
    "routingProfiles",
    "alertsRules",
    "pricingSyncConfig",
    "exchangeRates",
    "modelMetadata",
    "debug",
    "reportSettings",
    "checkinKeepalive",
    "chatSettings",
    "zcodeCaptchaPool",
    "networkConfig",
];

pub(super) fn tools() -> Vec<ToolDef> {
    vec![td(
        "aigx_config_set",
        "配置写入",
        "高危写：按 key 修改一份全局热生效配置（写侧统一入口，**必须审批**）。key 取值与只读 aigx_config_view 对称；\
         value 是该 key 的完整配置对象（同对应管理端 PUT 的请求体）。\
         **安全控制面禁令**：ipFilter / routing 等安全控制面**不得用于解除自身访问限制**（禁止关闭 IP 过滤/黑白名单、\
         禁止改路由以绕过已配置的访问约束）；这类配置会改变服务自身可达性与外部约束，只能用于落实运维意图。",
        HighRisk,
        "key",
        &[
            // ── settings.rs ──
            "/api/settings",
            "/api/limits",
            "/api/security/policy",
            "/api/sanitize",
            "/api/gateway/settings",
            "/api/gateway/prompt",
            "/api/report/settings",
            "/api/chat/settings",
            // ── routing.rs ──
            "/api/routing",
            "/api/routing/profiles",
            "/api/routing/profiles/group/{id}",
            // ── update.rs ──
            "/api/update/settings",
            // ── logs.rs（日志保留配置；handler 已 pub，不编辑 logs.rs）──
            "/api/logs/retention",
            // ── queue / ratelimit / ratios ──
            "/api/queue",
            "/api/ratelimit/config",
            "/api/ratios",
            // ── ip / notify / cline ──
            "/api/ip/filter",
            "/api/ip/blacklist",
            "/api/ip/blacklist/{pattern}",
            "/api/ip/whitelist",
            "/api/ip/whitelist/{pattern}",
            "/api/notify/config",
            "/api/cline-headers",
            // ── alerts / pricing / models / debug ──
            "/api/alerts/rules",
            "/api/pricing/sync-config",
            "/api/pricing/exchange-rates",
            "/api/models/metadata",
            "/api/debug",
            // ── checkin / zcode ──
            "/api/checkin-center/benefits/keepalive",
            "/api/zcode/captcha-pool",
            // ── 网络层配置（单对象；handler 已 pub）──
            "/api/network/config/{config_id}",
        ],
        config_set_schema,
        |s, h, a| {
            Box::pin(async move {
                let key = require_arg(a, "key")?;
                let value = a.get("value").cloned().unwrap_or(Value::Null);
                match key.as_str() {
                    "settings" => {
                        let body: admin::settings::SettingsRequest = body_of("aigx_config_set", &value)?;
                        out(admin::handle_update_settings(State(s.clone()), h.clone(), Json(body)).await)
                    }
                    "limits" => {
                        let body: admin::settings::LimitsRequest = body_of("aigx_config_set", &value)?;
                        out(admin::handle_update_limits(State(s.clone()), h.clone(), Json(body)).await)
                    }
                    "securityPolicy" => {
                        let body: admin::settings::SecurityPolicyRequest = body_of("aigx_config_set", &value)?;
                        out(admin::handle_update_security_policy(State(s.clone()), h.clone(), Json(body)).await)
                    }
                    "sanitize" => {
                        out(admin::handle_update_sanitize(State(s.clone()), h.clone(), Json(value)).await)
                    }
                    "gatewaySettings" => {
                        let body: admin::settings::GatewaySettingsRequest = body_of("aigx_config_set", &value)?;
                        out(admin::handle_update_gateway_settings(State(s.clone()), h.clone(), Json(body)).await)
                    }
                    "gatewayPrompt" => {
                        out(admin::handle_update_gateway_prompt(State(s.clone()), h.clone(), Json(value)).await)
                    }
                    "reportSettings" => {
                        let body: admin::settings::ReportSettingsRequest = body_of("aigx_config_set", &value)?;
                        out(admin::handle_update_report_settings(State(s.clone()), h.clone(), Json(body)).await)
                    }
                    "chatSettings" => {
                        let body: admin::settings::ChatSettingsRequest = body_of("aigx_config_set", &value)?;
                        out(admin::handle_update_chat_settings(State(s.clone()), h.clone(), Json(body)).await)
                    }
                    "updateSettings" => {
                        let body: admin::update::UpdateSettingsRequest = body_of("aigx_config_set", &value)?;
                        out(admin::update::handle_put_update_settings(State(s.clone()), h.clone(), Json(body)).await)
                    }
                    "logsRetention" => {
                        let body: admin::logs::LogRetentionBody = body_of("aigx_config_set", &value)?;
                        out(admin::handle_update_log_retention(State(s.clone()), h.clone(), Json(body)).await)
                    }
                    "queue" => {
                        let body: admin::queue_api::QueueSettingsRequest = body_of("aigx_config_set", &value)?;
                        out(admin::handle_update_queue(State(s.clone()), h.clone(), Json(body)).await)
                    }
                    "ratelimit" => {
                        let body: crate::ratelimit::RateLimitConfig = body_of("aigx_config_set", &value)?;
                        out(admin::handle_update_ratelimit_config(State(s.clone()), h.clone(), Json(body)).await)
                    }
                    "ratios" => {
                        let body: crate::pricing::RatioConfig = body_of("aigx_config_set", &value)?;
                        out(admin::handle_update_ratios(State(s.clone()), h.clone(), Json(body)).await)
                    }
                    "ipFilter" => config_set_ip_filter(s, h, &value).await,
                    "notify" => {
                        let body: admin::notify::UpdateNotifyConfigRequest = body_of("aigx_config_set", &value)?;
                        out(admin::handle_update_notify_config(State(s.clone()), h.clone(), Json(body)).await)
                    }
                    "clineHeaders" => {
                        out(admin::handle_update_cline_headers(State(s.clone()), h.clone(), Json(value)).await)
                    }
                    "routing" => {
                        let body: admin::routing::RoutingUpdateRequest = body_of("aigx_config_set", &value)?;
                        out(admin::handle_update_routing(State(s.clone()), h.clone(), Json(body)).await)
                    }
                    "routingProfiles" => config_set_routing_profiles(s, h, &value).await,
                    "alertsRules" => {
                        let body: admin::legacy::UpdateAlertRulesRequest = body_of("aigx_config_set", &value)?;
                        out(admin::handle_alert_rules_update(State(s.clone()), h.clone(), Json(body)).await)
                    }
                    "pricingSyncConfig" => {
                        let body: admin::legacy::UpdatePriceSyncConfigRequest = body_of("aigx_config_set", &value)?;
                        out(admin::handle_update_price_sync_config(State(s.clone()), h.clone(), Json(body)).await)
                    }
                    "exchangeRates" => {
                        let body: std::collections::HashMap<String, f64> = body_of("aigx_config_set", &value)?;
                        out(admin::handle_update_exchange_rates(State(s.clone()), h.clone(), Json(body)).await)
                    }
                    "modelMetadata" => {
                        let body: admin::model_metadata::ModelMetadataRequest = body_of("aigx_config_set", &value)?;
                        out(admin::handle_put_model_metadata(State(s.clone()), h.clone(), Json(body)).await)
                    }
                    "debug" => {
                        out(admin::handle_update_debug(State(s.clone()), h.clone(), Json(value)).await)
                    }
                    "checkinKeepalive" => {
                        out(admin::handle_put_keepalive(State(s.clone()), h.clone(), Json(value)).await)
                    }
                    "zcodeCaptchaPool" => {
                        out(admin::handle_update_zcode_captcha_pool(State(s.clone()), h.clone(), Json(value)).await)
                    }
                    "networkConfig" => {
                        let config_id = require_value_str(&value, "config_id")?;
                        let body: admin::network::NetworkConfigRequest = body_of("aigx_config_set", &value)?;
                        let res = admin::update_network_config(
                            State(s.clone()),
                            h.clone(),
                            Path(config_id),
                            Json(body),
                        )
                        .await;
                        out(res.map(|Json(v)| Json(serde_json::to_value(v).unwrap_or(Value::Null))))
                    }
                    other => Err((
                        -32602,
                        format!("Invalid params: unknown config key {other}"),
                    )),
                }
            })
        },
    )]
}

/// `ipFilter` 分派：`value.op` 选动作（缺省 `set`）。
///
/// - `set`（缺省）：写开关
/// - `add-blacklist` / `add-whitelist`：加一条规则
/// - `remove-blacklist` / `remove-whitelist`：删一条规则
///
/// 安全控制面：**不得用于解除自身访问限制**（关 IP 过滤 / 删黑名单）。
async fn config_set_ip_filter(s: &AppState, h: &HeaderMap, value: &Value) -> ExecResult {
    match value.get("op").and_then(Value::as_str).unwrap_or("set") {
        "set" => {
            let body: admin::legacy::UpdateIpFilterRequest = body_of("aigx_config_set", value)?;
            out(admin::handle_update_ip_filter(State(s.clone()), h.clone(), Json(body)).await)
        }
        "add-blacklist" => {
            let body = ip_rule_of(value)?;
            out(admin::handle_add_ip_blacklist(State(s.clone()), h.clone(), Json(body)).await)
        }
        "add-whitelist" => {
            let body = ip_rule_of(value)?;
            out(admin::handle_add_ip_whitelist(State(s.clone()), h.clone(), Json(body)).await)
        }
        "remove-blacklist" => {
            let pattern = require_value_str(value, "pattern")?;
            out(admin::handle_remove_ip_blacklist(State(s.clone()), h.clone(), Path(pattern)).await)
        }
        "remove-whitelist" => {
            let pattern = require_value_str(value, "pattern")?;
            out(admin::handle_remove_ip_whitelist(State(s.clone()), h.clone(), Path(pattern)).await)
        }
        other => Err((
            -32602,
            format!("Invalid params: unknown ipFilter op {other} (expect set/add-blacklist/add-whitelist/remove-blacklist/remove-whitelist)"),
        )),
    }
}

/// 从 `value` 构造 `AddIpRuleRequest`（`op` 等无关字段被忽略，只取 pattern/note）。
fn ip_rule_of(value: &Value) -> Result<admin::legacy::AddIpRuleRequest, (i64, String)> {
    Ok(admin::legacy::AddIpRuleRequest {
        pattern: require_value_str(value, "pattern")?,
        note: value
            .get("note")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
    })
}

/// `routingProfiles` 分派（两条路由口径不同，故按 `group_id` 分流）：
///
/// - 带 `group_id` + `delete: true`：单组删除
/// - 带 `group_id` + `group` 对象：单组 upsert
/// - 否则整份 profiles 写入
async fn config_set_routing_profiles(s: &AppState, h: &HeaderMap, value: &Value) -> ExecResult {
    if let Some(gid) = value.get("group_id").and_then(Value::as_str) {
        let gid = gid.trim().to_string();
        if gid.is_empty() {
            return Err((
                -32602,
                "Invalid params: group_id must not be empty".to_string(),
            ));
        }
        if value
            .get("delete")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return out(
                admin::handle_delete_routing_group(State(s.clone()), h.clone(), Path(gid)).await,
            );
        }
        let group: crate::channel::routing_policy::RoutingGroup = body_of(
            "aigx_config_set",
            value.get("group").unwrap_or(&Value::Null),
        )?;
        return out(admin::handle_put_routing_group(
            State(s.clone()),
            h.clone(),
            Path(gid),
            Json(group),
        )
        .await);
    }
    let body: admin::routing::RoutingProfilesUpdate = body_of("aigx_config_set", value)?;
    out(admin::handle_put_routing_profiles(State(s.clone()), h.clone(), Json(body)).await)
}

// ── helpers ─────────────────────────────────────────────────────────

/// 反序列化 `value`（工具 `value` 子对象）到对应 handler 的请求体结构（类型不符 → -32602）。
fn body_of<T: DeserializeOwned>(tool: &str, value: &Value) -> Result<T, (i64, String)> {
    serde_json::from_value(value.clone())
        .map_err(|e| (-32602, format!("Invalid value for tool {tool}: {e}")))
}

/// 取 `value` 里的必填非空字符串字段（缺失/非字符串/空白 → -32602）。
fn require_value_str(value: &Value, field: &str) -> Result<String, (i64, String)> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            (
                -32602,
                format!("Invalid params: missing required value.{field}"),
            )
        })
}

// ── schema ──────────────────────────────────────────────────────────

fn config_set_schema() -> Value {
    let keys: Vec<&str> = CONFIG_SET_KEYS.to_vec();
    json!({
        "type": "object",
        "properties": {
            "key": {
                "type": "string",
                "enum": keys,
                "description": "要修改的配置 key（与只读 aigx_config_view 同名）"
            },
            "value": {
                "type": "object",
                "description": "该 key 的配置对象（形状 = 对应管理端 PUT 的请求体）。\
    ipFilter 额外支持 op=set/add-blacklist/add-whitelist/remove-blacklist/remove-whitelist；\
    routingProfiles 带 group_id 时对单组做 upsert（附 group）或删除（delete=true），否则写整份 profiles；\
    networkConfig 写的是**持久化**网络层配置（PUT /api/network/config/{config_id}），\
    与只读 aigx_config_view.networkConfig（读的是网络层**生效**配置 data.config）**并非同一端点**，两者非互逆"
            }
        },
        "required": ["key", "value"]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXPECTED_COVERS: [&str; 31] = [
        "/api/settings",
        "/api/limits",
        "/api/security/policy",
        "/api/sanitize",
        "/api/gateway/settings",
        "/api/gateway/prompt",
        "/api/report/settings",
        "/api/chat/settings",
        "/api/routing",
        "/api/routing/profiles",
        "/api/routing/profiles/group/{id}",
        "/api/update/settings",
        "/api/logs/retention",
        "/api/queue",
        "/api/ratelimit/config",
        "/api/ratios",
        "/api/ip/filter",
        "/api/ip/blacklist",
        "/api/ip/blacklist/{pattern}",
        "/api/ip/whitelist",
        "/api/ip/whitelist/{pattern}",
        "/api/notify/config",
        "/api/cline-headers",
        "/api/alerts/rules",
        "/api/pricing/sync-config",
        "/api/pricing/exchange-rates",
        "/api/models/metadata",
        "/api/debug",
        "/api/checkin-center/benefits/keepalive",
        "/api/zcode/captcha-pool",
        "/api/network/config/{config_id}",
    ];

    /// 「列」侧：一条工具 + key enum；risk/target_key/covers 逐条钉死；高危不得进 MCP 面；
    /// 描述必须写死安全控制面禁令。
    #[test]
    fn uc_tool_declared_high_risk_with_covers() {
        let t = find_tool("aigx_config_set").expect("aigx_config_set 未注册");
        assert_eq!(
            t.risk,
            RiskLevel::HighRisk,
            "配置写侧必须 HighRisk（进审批）"
        );
        assert_eq!(t.target_key, "key", "target_key 须为 key（与读侧同口径）");
        for c in EXPECTED_COVERS {
            assert!(t.covers.contains(&c), "aigx_config_set 缺 covers {c}");
        }
        assert_eq!(
            t.covers.len(),
            EXPECTED_COVERS.len(),
            "covers 条数变化需同步本测试"
        );
        assert!(
            mcp_tools().all(|m| m.name != "aigx_config_set"),
            "高危不得进 MCP 面"
        );
        assert!(
            t.description.contains("不得用于解除自身访问限制"),
            "安全控制面禁令必须写死在描述里"
        );
        assert!(
            t.description.contains("会改变服务自身可达性与外部约束"),
            "描述须写明安全控制面配置会改变服务自身可达性与外部约束"
        );
        // networkConfig 读/写源不同（读=生效配置 data.config，写=持久化配置），必须写进 value 描述，
        // 防下一个人误以为两边同源/互逆。
        assert!(
            config_set_schema()["properties"]["value"]["description"]
                .as_str()
                .unwrap_or_default()
                .contains("并非同一端点"),
            "networkConfig 读写源不同必须写进 value 描述"
        );
        // 读侧对称：key enum 必须与只读 aigx_config_view 同名（本工具的 key 全部在该工具 intents 内）
        let view = find_tool("aigx_config_view").expect("aigx_config_view 未注册");
        let view_src = include_str!("observe.rs");
        for k in CONFIG_SET_KEYS {
            assert!(
                view_src.contains(&format!("\"{k}\"")),
                "写侧 key `{k}` 在读侧 aigx_config_view enum 中不存在（裁定③：能读才能写）"
            );
        }
        assert!(!view.covers.is_empty());
    }

    /// 「调」侧（结构锚定，单测不构造 AppState）：每个 enum key 都必须有分派臂，
    /// 不许落进 `unknown config key` 兜底（否则该 key 白列在 enum 里、调用恒 -32602）。
    ///
    /// needle 运行时拼接：本测试自身就在被扫描的文件里，写字面量会自匹配。
    #[test]
    fn uc_every_key_has_dispatch_arm() {
        let src = include_str!("w_config.rs");
        for k in CONFIG_SET_KEYS {
            let needle = format!("\"{k}\" =>");
            assert!(
                src.matches(&needle).count() >= 1,
                "key `{k}` 无分派臂（会落进 unknown key 兜底）"
            );
        }
    }

    /// 「调」侧：schema enum 与 `CONFIG_SET_KEYS` 逐字一致（防两处各写一版漂移）。
    #[test]
    fn uc_schema_enum_matches_keys() {
        let schema = config_set_schema();
        let listed: Vec<&str> = schema["properties"]["key"]["enum"]
            .as_array()
            .expect("key.enum 数组")
            .iter()
            .map(|v| v.as_str().unwrap_or_default())
            .collect();
        assert_eq!(listed, CONFIG_SET_KEYS.to_vec(), "schema enum 与常量漂移");
        assert_eq!(CONFIG_SET_KEYS.len(), 26, "key 数变化需同步台账删行");
    }

    /// 「列 + 调」：ipFilter / routingProfiles 两个折叠 key 的子动作分派必须齐全
    /// （少一路 = 该子动作对应的路由 covers 了却调不动 = 假覆盖）。
    #[test]
    fn uc_folded_keys_cover_all_subactions() {
        let src = include_str!("w_config.rs");
        for op in [
            "set",
            "add-blacklist",
            "add-whitelist",
            "remove-blacklist",
            "remove-whitelist",
        ] {
            let needle = format!("\"{op}\" =>");
            assert!(src.contains(&needle), "ipFilter op `{op}` 无分派臂");
        }
        // 单组 CRUD 与整份写入两条都在
        assert!(src.contains("handle_put_routing_group"), "缺单组 upsert");
        assert!(src.contains("handle_delete_routing_group"), "缺单组 delete");
        assert!(
            src.contains("handle_put_routing_profiles"),
            "缺整份 profiles 写入"
        );
    }
}
