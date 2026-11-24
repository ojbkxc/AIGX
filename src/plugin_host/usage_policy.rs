//! 插件申报的余额查询策略（对齐 Agent2API v3.5.1 `plugin_host/usage_policy.rs`）。
//!
//! 插件在 `init` 握手里可申报 `usage_query` / `usageQuery`：
//! - `concurrency`：该家最多允许多少个余额查询同时在飞（宿主上限内取更小值）；
//! - `minIntervalMs`：同一家相邻两次查询的最小间隔（毫秒）。
//!
//! 为什么要这个：余额接口普遍是「按家限流的第三方接口」，账号池一轮巡检
//! 会把同一家的账号一起查（[`crate::account_session::usage_query::sweep`]），
//! 插件比宿主更清楚自己那家的脾气。宿主侧只做**裁剪**，不做解释：
//! 并发钳到 1–64，间隔钳到 0–10s，超界值不报错、按边界生效（与上游同口径）。

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;

/// 单个 provider 的余额查询策略。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UsageQueryPolicy {
    /// 并发上限；`None` = 插件没申报，用宿主默认。
    pub concurrency: Option<usize>,
    /// 相邻查询最小间隔（毫秒）；0 = 不限。
    pub min_interval_ms: u64,
}

/// 并发上限的硬边界（上游同为 64）。
const MAX_CONCURRENCY: usize = 64;
/// 最小间隔的硬边界（上游同为 10s）。
const MAX_MIN_INTERVAL_MS: u64 = 10_000;

/// 握手 JSON 里单家的原始形状（camelCase 与 snake_case 都认）。
#[derive(Deserialize)]
struct RawPolicy {
    #[serde(default)]
    concurrency: Option<usize>,
    #[serde(default, alias = "minIntervalMs")]
    min_interval_ms: Option<u64>,
}

/// 解析单家申报：两项都没给/给了 0 且无并发 → `None`（不占表位）。
fn parse_provider(item: &Value) -> Option<UsageQueryPolicy> {
    let raw = item.get("usage_query").or_else(|| item.get("usageQuery"))?;
    let parsed: RawPolicy = serde_json::from_value(raw.clone()).ok()?;
    let concurrency = parsed
        .concurrency
        .map(|value| value.clamp(1, MAX_CONCURRENCY));
    let min_interval_ms = parsed.min_interval_ms.unwrap_or(0).min(MAX_MIN_INTERVAL_MS);
    (concurrency.is_some() || min_interval_ms > 0).then_some(UsageQueryPolicy {
        concurrency,
        min_interval_ms,
    })
}

/// 解析整份握手申报（数组元素按 `id` 归属 provider）。
pub(crate) fn parse(value: &Value) -> BTreeMap<String, UsageQueryPolicy> {
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
        if let Some(policy) = parse_provider(item) {
            map.insert(id.to_string(), policy);
        }
    }
    map
}

/// 取某 provider 的策略（未装插件 / 未申报 → 默认）。
pub fn for_provider(provider: &str) -> UsageQueryPolicy {
    let Some(host) = super::wasm::host_for_provider(provider) else {
        return UsageQueryPolicy::default();
    };
    host.usage_query_policy(provider)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_both_key_shapes_and_clamps() {
        let value = serde_json::json!([
            { "id": "acme", "usageQuery": { "concurrency": 999, "minIntervalMs": 999_999 } },
            { "id": "beta", "usage_query": { "concurrency": 0, "min_interval_ms": 1500 } },
            { "id": "gamma" },
            { "id": "delta", "usageQuery": { "concurrency": 4 } },
        ]);
        let map = parse(&value);
        // acme：两项都超界 → 钳到 64 / 10s
        assert_eq!(
            map["acme"],
            UsageQueryPolicy {
                concurrency: Some(64),
                min_interval_ms: 10_000
            }
        );
        // beta：并发 0 钳到 1；间隔 1500 保留
        assert_eq!(
            map["beta"],
            UsageQueryPolicy {
                concurrency: Some(1),
                min_interval_ms: 1500
            }
        );
        // gamma：没申报 → 不入表
        assert!(!map.contains_key("gamma"), "无申报不应占表位: {map:?}");
        // delta：只给并发
        assert_eq!(
            map["delta"],
            UsageQueryPolicy {
                concurrency: Some(4),
                min_interval_ms: 0
            }
        );
    }

    #[test]
    fn ignores_malformed_and_empty_claims() {
        let value = serde_json::json!([
            { "id": "", "usageQuery": { "concurrency": 2 } },
            { "id": "x", "usageQuery": { "concurrency": 2, "minIntervalMs": 0 } },
            { "usageQuery": { "concurrency": 2 } },
            { "id": "y", "usageQuery": "not-an-object" },
            { "id": "z", "usageQuery": { "minIntervalMs": 5 } },
        ]);
        let map = parse(&value);
        // x：并发 2 + 间隔 0 → 仍算申报（有并发）
        assert_eq!(map["x"].concurrency, Some(2));
        assert_eq!(map["x"].min_interval_ms, 0);
        // 空 id / 无 id / 非对象 → 跳过
        assert!(!map.contains_key(""));
        assert_eq!(map.len(), 2, "只应留下 x 与 z: {map:?}");
        assert_eq!(map["z"].min_interval_ms, 5);
    }

    #[test]
    fn non_array_is_empty() {
        assert!(parse(&serde_json::json!({"usageQuery": {"concurrency": 2}})).is_empty());
        assert!(parse(&serde_json::json!(null)).is_empty());
    }

    #[test]
    fn unknown_provider_falls_back_to_default() {
        // 未启动的 provider 没有宿主 → 默认策略（不限并发/不限间隔）
        let policy = for_provider("no-such-provider-zzz");
        assert_eq!(policy, UsageQueryPolicy::default());
    }
}
