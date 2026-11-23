//! 插件申报的「错误识别映射 + 默认错误处理例外」（对齐 Agent2API
//! v3.5.0-beta.8 `plugin_host/error_rules.rs`）。
//!
//! 插件在 `init` 握手 JSON 里可申报两类数据：
//! - `errorRules`：默认错误处理规则（错误大类 + 状态码/业务码条件 + 动作 +
//!   冷却）。它们参与统一错误策略的三层解析（用户例外 > 插件默认 > 全局），
//!   用户可以按 `plugin:{provider}:{本地 id}` 改写成自己的例外。
//! - `errorClassifications`：识别映射（状态码/业务码 → 错误大类，可带
//!   作用域与 hard-stop 建议），用于把 AIGX 静态推断不出的上游错误归类。
//!
//! 安全口径（与上游一致）：只信任握手 JSON，但对条数（规则 32 / 映射 64）、
//! 状态码范围（400–599，≤50）、业务码数量（≤64）与冷却边界（1–86400s）做
//! 严格裁剪；单个条目格式有误时跳过该条，插件正常装载。

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;

use crate::error_policy::{
    CooldownMode, CooldownScope, DeclaredErrorPolicy, ErrorAction, ErrorKind, ErrorRule,
    PluginErrorClassification, PluginErrorRule,
};

/// 每个 provider 最多 32 条默认规则。
const MAX_RULES_PER_PROVIDER: usize = 32;
/// 每个 provider 最多 64 条识别映射。
const MAX_CLASSIFICATIONS_PER_PROVIDER: usize = 64;

/// 握手 JSON 里一条识别映射的原始形状（camelCase + `hard_stop` 别名）。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawClassification {
    kind: ErrorKind,
    #[serde(default)]
    statuses: Vec<u16>,
    #[serde(default)]
    codes: Vec<String>,
    #[serde(default)]
    scope: Option<CooldownScope>,
    #[serde(default, alias = "hard_stop")]
    hard_stop: bool,
}

/// 握手 JSON 里一条默认规则的原始形状。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawRule {
    id: String,
    name: String,
    #[serde(default)]
    reason: String,
    kind: ErrorKind,
    #[serde(default)]
    statuses: Vec<u16>,
    #[serde(default)]
    code: String,
    #[serde(default)]
    model: String,
    #[serde(default = "yes")]
    enabled: bool,
    action: ErrorAction,
    cooldown: CooldownMode,
    #[serde(default = "default_fallback", alias = "fallback_seconds")]
    fallback_seconds: u64,
    #[serde(default)]
    exponential: bool,
    #[serde(default = "default_cap", alias = "fallback_cap_seconds")]
    fallback_cap_seconds: u64,
    #[serde(default = "default_scope")]
    scope: CooldownScope,
}

fn yes() -> bool {
    true
}
fn default_fallback() -> u64 {
    60
}
fn default_cap() -> u64 {
    1800
}
fn default_scope() -> CooldownScope {
    CooldownScope::Auto
}

/// 文本清洗：去首尾空白与控制字符，截到上限。
fn clean_text(raw: &str, max_chars: usize) -> String {
    raw.trim()
        .chars()
        .filter(|ch| !ch.is_control())
        .take(max_chars)
        .collect()
}

/// 插件本地 id → 网关侧规范化 id（`plugin:{provider}:{local}`，≤128 字符）。
fn canonical_id(provider: &str, local_id: &str) -> Option<String> {
    let local: String = local_id
        .trim()
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
        .take(64)
        .collect();
    if local.is_empty() {
        return None;
    }
    let full = format!("plugin:{provider}:{local}");
    (full.len() <= 128).then_some(full)
}

/// 解析一个 provider 的申报（跳过无效条目，不整包失败）。
fn parse_provider(provider: &str, item: &Value) -> DeclaredErrorPolicy {
    let mut out = DeclaredErrorPolicy::default();
    if let Some(items) = item
        .get("error_classifications")
        .or_else(|| item.get("errorClassifications"))
        .and_then(Value::as_array)
    {
        for raw in items.iter().take(MAX_CLASSIFICATIONS_PER_PROVIDER) {
            let Ok(entry) = serde_json::from_value::<RawClassification>(raw.clone()) else {
                continue;
            };
            let statuses: Vec<u16> = entry
                .statuses
                .into_iter()
                .filter(|status| (400..600).contains(status))
                .take(32)
                .collect();
            let codes: Vec<String> = entry
                .codes
                .into_iter()
                .map(|code| clean_text(&code, 128))
                .filter(|code| !code.is_empty())
                .take(64)
                .collect();
            if statuses.is_empty() && codes.is_empty() {
                continue;
            }
            out.classifications.push(PluginErrorClassification {
                kind: entry.kind,
                statuses,
                codes,
                scope: entry.scope,
                hard_stop: entry.hard_stop,
            });
        }
    }
    if let Some(items) = item
        .get("error_rules")
        .or_else(|| item.get("errorRules"))
        .and_then(Value::as_array)
    {
        let mut seen = std::collections::HashSet::new();
        for raw in items.iter().take(MAX_RULES_PER_PROVIDER) {
            let Ok(entry) = serde_json::from_value::<RawRule>(raw.clone()) else {
                continue;
            };
            let local_id = clean_text(&entry.id, 64);
            let Some(full_id) = canonical_id(provider, &local_id) else {
                continue;
            };
            if !seen.insert(full_id.clone()) {
                continue;
            }
            let name = clean_text(&entry.name, 100);
            if name.is_empty() {
                continue;
            }
            // 非 Auth 的 RefreshThenSwitch 一律降级 SwitchAccount（上游同口径：
            // 只有登录失效可以触发凭证续期）。
            let action = if entry.kind != ErrorKind::Auth
                && entry.action == ErrorAction::RefreshThenSwitch
            {
                ErrorAction::SwitchAccount
            } else {
                entry.action
            };
            let fallback_seconds = entry.fallback_seconds.clamp(1, 86_400);
            let fallback_cap_seconds = entry.fallback_cap_seconds.clamp(fallback_seconds, 86_400);
            let statuses: Vec<u16> = entry
                .statuses
                .into_iter()
                .filter(|status| (400..600).contains(status))
                .take(50)
                .collect();
            let code = clean_text(&entry.code, 256);
            let model = clean_text(&entry.model, 256);
            let reason = clean_text(&entry.reason, 240);
            out.rules.push(PluginErrorRule {
                rule: ErrorRule {
                    id: full_id,
                    name,
                    kind: entry.kind,
                    statuses,
                    code,
                    provider: provider.to_string(),
                    model,
                    enabled: entry.enabled,
                    action,
                    cooldown: entry.cooldown,
                    fallback_seconds,
                    exponential: entry.exponential,
                    fallback_cap_seconds,
                    scope: entry.scope,
                },
                plugin_rule_id: local_id,
                reason,
            });
        }
    }
    out
}

/// 解析整份握手申报：数组元素按 `id` 归属 provider。
/// AIGX 的 Wasm 宿主一包一 provider（握手只返回一个 providers[0]），
/// 这里保持上游的数组形状，取 `items[*].id` 匹配。
pub(crate) fn parse(value: &Value) -> BTreeMap<String, DeclaredErrorPolicy> {
    let mut map = BTreeMap::new();
    let Some(items) = value.as_array() else {
        return map;
    };
    for item in items {
        let Some(id) = item["id"]
            .as_str()
            .map(str::trim)
            .filter(|id| !id.is_empty() && id.len() <= 128)
        else {
            continue;
        };
        let declared = parse_provider(id, item);
        if !declared.rules.is_empty() || !declared.classifications.is_empty() {
            map.insert(id.to_string(), declared);
        }
    }
    map
}

/// 取某 provider 的申报（未装插件 / 未申报 → 空策略）。
pub fn for_provider(provider: &str) -> DeclaredErrorPolicy {
    let Some(host) = super::wasm::host_for_provider(provider) else {
        return DeclaredErrorPolicy::default();
    };
    host.error_rules()
}

/// 全部已启动插件的默认规则（管理面 `/api/routing/errors` 视图用）。
pub fn all_provider_rules() -> Vec<PluginErrorRule> {
    let mut out = Vec::new();
    for provider in super::wasm::host_providers() {
        out.extend(for_provider(&provider).rules);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule_json(id: &str, kind: &str) -> Value {
        serde_json::json!({
            "id": id, "name": format!("规则 {id}"), "kind": kind,
            "action": "switch_account", "cooldown": "upstream",
            "fallbackSeconds": 120, "reason": "上游限流特征",
        })
    }

    #[test]
    fn parse_reads_rules_and_classifications() {
        let value = serde_json::json!([
            { "id": "acme", "errorRules": [ rule_json("rl", "rate_limit") ],
              "errorClassifications": [ { "kind": "rate_limit", "statuses": [429] } ] },
            { "id": "no-data" },
            { "id": "", "errorRules": [ rule_json("x", "auth") ] },
        ]);
        let map = parse(&value);
        assert_eq!(map.len(), 1, "无申报与空 id 的条目不入表: {map:?}");
        let acme = &map["acme"];
        assert_eq!(acme.rules.len(), 1);
        assert_eq!(acme.rules[0].rule.id, "plugin:acme:rl");
        assert_eq!(acme.rules[0].plugin_rule_id, "rl");
        assert_eq!(acme.rules[0].reason, "上游限流特征");
        assert_eq!(acme.classifications.len(), 1);
        assert_eq!(acme.classifications[0].kind, ErrorKind::RateLimit);
    }

    #[test]
    fn canonical_id_is_ascii_only_and_bounded() {
        assert_eq!(
            canonical_id("acme", "my_rule-1"),
            Some("plugin:acme:my_rule-1".into())
        );
        assert!(canonical_id("acme", "").is_none());
        assert!(canonical_id("acme", "空格 不行").is_none());
        let long = "a".repeat(200);
        // 本地超长（>64 字符）截断后 id 仍 ≤128
        let id = canonical_id("acme", &long).expect("截断后仍合法");
        assert!(id.len() <= 128);
    }

    #[test]
    fn invalid_entries_are_skipped_not_fatal() {
        let value = serde_json::json!([
            { "id": "acme", "errorRules": [
                { "id": "ok", "name": "有效", "kind": "auth",
                  "action": "refresh_then_switch", "cooldown": "none" },
                { "id": "bad", "kind": "auth" },            // 缺 name → 跳过
                rule_json("dup", "auth"),                   // 与下面 id 冲突 → 后者跳过
                rule_json("dup", "auth"),
            ]},
        ]);
        let map = parse(&value);
        let acme = &map["acme"];
        assert_eq!(acme.rules.len(), 2, "只有 ok 与首个 dup: {:?}", acme.rules);
        // 非 Auth 的 RefreshThenSwitch 降级 SwitchAccount
        assert_eq!(acme.rules[0].rule.action, ErrorAction::RefreshThenSwitch);
    }

    #[test]
    fn non_auth_refresh_downgrades_to_switch() {
        let value = serde_json::json!([
            { "id": "acme", "errorRules": [
                { "id": "r", "name": "限流却要续期", "kind": "rate_limit",
                  "action": "refresh_then_switch", "cooldown": "upstream" },
            ]},
        ]);
        let map = parse(&value);
        assert_eq!(map["acme"].rules[0].rule.action, ErrorAction::SwitchAccount);
    }

    #[test]
    fn statuses_out_of_range_and_overlong_codes_are_clamped() {
        let value = serde_json::json!([
            { "id": "acme", "errorRules": [
                { "id": "r", "name": "范围外状态码", "kind": "auth",
                  "action": "return_error", "cooldown": "none",
                  "statuses": [302, 429, 600],
                  "fallbackSeconds": 999_999, "fallbackCapSeconds": 0 },
            ]},
        ]);
        let map = parse(&value);
        let rule = &map["acme"].rules[0].rule;
        assert_eq!(rule.statuses, vec![429u16], "302/600 被裁掉");
        assert_eq!(rule.fallback_seconds, 86_400, "冷却钳到上限");
        assert_eq!(rule.fallback_cap_seconds, 86_400, "cap ≥ fallback");
    }
}
