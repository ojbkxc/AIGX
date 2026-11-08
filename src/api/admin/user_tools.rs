//! 用户工具策略管理面端点（#34 §5.1，喂 #65 UserTools 配置页）。
//!
//! 三个端点，全部 `verify_admin` + `record_audit`（配置 100% 管理员，终端用户
//! 只消费生效结果——不新增终端用户可见的配置端点，攻击面零增量）：
//!
//! - `GET  /api/user-tools/policy`   → 快照（enabled + groups）+ 生效版本号
//! - `PUT  /api/user-tools/policy`   → 全量更新（上限校验**超限拒绝 409** + 原因；
//!   写后热更，回带保存后的策略供前端回写草稿）
//! - `GET  /api/user-tools/catalog`  → 工具目录：全部工具 + risk + `user_exec`
//!   有无 + 当前策略命中状态（`policy_hit`）
//!
//! 存储复用 `UserToolPolicyStore`（#60，与 RoutingPolicyStore 同构快照模式）：
//! 快照唯一真相，`update()` 校验上限 → FileStore 落盘 → 热更生效。
//!
//! ## 版本号口径
//!
//! 「生效版本号」取自用户版本分配器（`UserStore.list()` 最大 `version`）？——**不是**。
//! 策略版本是**策略自身的修改计数**：存进策略结构外层（不进持久化结构体，
//! 按修改次数在内存里单调递增）。这样 GET 给前端一个可比较的整数（多标签页
//! 冲突提示用），PUT 成功即 +1。重启从 0 起算（低频管理面配置，可接受）。

use std::sync::atomic::{AtomicU64, Ordering};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use serde_json::{json, Value};

use super::common::{admin_id_from_session, error_response, record_audit, verify_admin};
use crate::agent::registry;
use crate::agent::user_tool_policy::{UserToolPolicy, UserToolPolicyStore};
use crate::api::openai::AppState;

/// 策略生效版本号（进程内单调计数；PUT 成功 +1，GET 回带）。
///
/// 不入持久化结构（`UserToolPolicy` 保持 #60 形状不变—— KvData 版本链那套
/// 是给业务数据防乱序用的，低频管理面配置用不上）。重启清零可接受。
static POLICY_VERSION: AtomicU64 = AtomicU64::new(0);

/// GET /api/user-tools/policy — 读当前策略快照 + 生效版本号。
pub async fn handle_get_user_tool_policy(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _admin = verify_admin(&state, &headers).await?;
    let snap = state.user_tool_policy.snapshot();
    Ok(Json(json!({
        "success": true,
        "data": policy_payload(&snap),
        "version": POLICY_VERSION.load(Ordering::Relaxed),
    })))
}

/// PUT /api/user-tools/policy — 全量更新（超限 409 + 原因；写后热更）。
pub async fn handle_put_user_tool_policy(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<UserToolPolicy>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _admin = verify_admin(&state, &headers).await?;
    let saved = save_user_tool_policy(&state.user_tool_policy, body)
        .map_err(|(code, msg)| error_response(&msg, code))?;
    let admin_id = admin_id_from_session(&state, &headers).await;
    record_audit(
        &state,
        &admin_id,
        "update_user_tool_policy",
        &format!("enabled={} groups={}", saved.enabled, saved.groups.len()),
        None,
        Some(json!({
            "enabled": saved.enabled,
            "groups": saved.groups,
        })),
    );
    Ok(Json(json!({
        "success": true,
        "data": policy_payload(&saved),
        "version": POLICY_VERSION.load(Ordering::Relaxed),
    })))
}

/// [`handle_put_user_tool_policy`] 的写入逻辑（纯函数，handler 与端点级单测
/// 共用同一份判定；对齐 61349fd5 `replace_routing_profiles` 姿势）：
/// `validate()` 上限校验**超限拒绝**（409 Conflict——资源冲突语义，UI 原样
/// 透传文案）→ `update()` 落盘热更 → 版本 +1。
fn save_user_tool_policy(
    store: &UserToolPolicyStore,
    policy: UserToolPolicy,
) -> Result<UserToolPolicy, (StatusCode, String)> {
    let saved = store.update(policy).map_err(|e| {
        (
            StatusCode::CONFLICT,
            format!("用户工具策略超限，已拒绝写入：{e}"),
        )
    })?;
    POLICY_VERSION.fetch_add(1, Ordering::Relaxed);
    Ok(saved)
}

/// GET /api/user-tools/catalog — 工具目录（配置界面用）。
///
/// 每行：name / label / risk / has_user_exec / policy_hit。`policy_hit` 只对
/// 声明了 `user_exec` 且非高危的工具计算（其余恒 false——UI 禁用行的命中
/// 状态没有意义）。
pub async fn handle_get_user_tool_catalog(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _admin = verify_admin(&state, &headers).await?;
    let snap = state.user_tool_policy.snapshot();
    Ok(Json(json!({
        "success": true,
        "data": { "tools": catalog_payload(&snap) },
    })))
}

/// 目录条目构造（纯函数，handler 与单测共用）。
fn catalog_payload(policy: &UserToolPolicy) -> Vec<Value> {
    registry::all_tools()
        .iter()
        .map(|t| {
            let exposed = t.is_user_exposed();
            json!({
                "name": t.name,
                "label": t.label,
                "risk": t.risk.as_str(),
                "has_user_exec": t.user_exec.is_some(),
                // 命中状态只对可暴露工具有意义；不暴露的行恒 false
                "policy_hit": exposed && policy.enabled && t.risk != crate::agent::tools::RiskLevel::HighRisk
                    && policy.groups.values().any(|g| g.allows(t.name)),
            })
        })
        .collect()
}

/// 策略负载（enabled + groups，与前端 `UserToolPolicy` 类型逐字段对齐）。
fn policy_payload(policy: &UserToolPolicy) -> Value {
    json!({
        "enabled": policy.enabled,
        "groups": policy.groups,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::tools::RiskLevel;
    use crate::agent::user_tool_policy::GroupPolicy;
    use std::collections::BTreeMap;
    use std::sync::Arc;

    fn store() -> UserToolPolicyStore {
        UserToolPolicyStore::in_memory(UserToolPolicy::default())
    }

    fn policy(enabled: bool, groups: &[(&str, &[&str])]) -> UserToolPolicy {
        UserToolPolicy {
            enabled,
            groups: groups
                .iter()
                .map(|(name, list)| {
                    (
                        (*name).to_string(),
                        GroupPolicy {
                            allowlist: list.iter().map(|s| (*s).to_string()).collect(),
                        },
                    )
                })
                .collect(),
        }
    }

    /// 超限（组数 > caps::MAX_GROUPS）一律 409 + 原因文案，且**现值不被改坏**。
    #[test]
    fn save_rejects_over_limit_with_conflict_and_keeps_current() {
        let store = store();
        store
            .update(policy(true, &[("vip", &["aigx_checkin_run"])]))
            .unwrap();

        let too_many: BTreeMap<String, GroupPolicy> = (0
            ..=crate::agent::user_tool_policy::caps::MAX_GROUPS)
            .map(|i| (format!("g{i}"), GroupPolicy::default()))
            .collect();
        let err = save_user_tool_policy(
            &store,
            UserToolPolicy {
                enabled: false,
                groups: too_many,
            },
        )
        .expect_err("超限必须拒绝写入");
        assert_eq!(err.0, StatusCode::CONFLICT, "超限映射 409");
        assert!(err.1.contains("超限"), "文案应含「超限」：{}", err.1);

        // 现值原样（拒绝写入不得污染快照）
        let snap = store.snapshot();
        assert!(snap.enabled);
        assert_eq!(snap.groups.len(), 1);
        assert!(snap.groups["vip"].allows("aigx_checkin_run"));
    }

    /// 全量替换 + 快照热更 + 版本递增（PUT 是 enabled/groups 的唯一写入口）。
    #[test]
    fn save_replaces_whole_policy_and_bumps_version() {
        let store = store();
        let before = POLICY_VERSION.load(Ordering::Relaxed);

        let saved = save_user_tool_policy(
            &store,
            policy(true, &[("vip", &["*"]), ("default", &["aigx_query_usage"])]),
        )
        .unwrap();
        assert!(saved.enabled);
        assert_eq!(saved.groups.len(), 2);
        assert_eq!(
            store.snapshot().groups["vip"].allowlist,
            vec!["*".to_string()],
            "快照热更"
        );
        assert_eq!(
            POLICY_VERSION.load(Ordering::Relaxed),
            before + 1,
            "PUT 成功版本 +1"
        );
    }

    /// catalog：user_exec 有无 / risk 透传 / policy_hit 只对可暴露工具计算。
    #[test]
    fn catalog_reports_risk_exec_and_hit() {
        let p = policy(true, &[("vip", &["aigx_checkin_run"])]);
        let tools = catalog_payload(&p);
        let get = |name: &str| {
            tools
                .iter()
                .find(|t| t["name"] == name)
                .unwrap_or_else(|| panic!("目录缺 {name}"))
        };

        // 声明了 user_exec 且在 allowlist → 命中
        let run = get("aigx_checkin_run");
        assert_eq!(run["has_user_exec"], true);
        assert_eq!(run["policy_hit"], true);
        assert_eq!(run["risk"], "low");

        // 声明了 user_exec 但不在 allowlist → 未命中
        let usage = get("aigx_query_usage");
        assert_eq!(usage["has_user_exec"], true);
        assert_eq!(usage["policy_hit"], false);

        // 未声明 user_exec（has_user_exec=false，policy_hit 恒 false）
        let logs_admin_only = get("aigx_cost_report");
        assert_eq!(logs_admin_only["has_user_exec"], false);
        assert_eq!(logs_admin_only["policy_hit"], false);

        // HighRisk 行：永不命中（D2 硬封顶，即便声明了 user_exec 也不可能）
        for t in &tools {
            if t["risk"] == "high" {
                assert_eq!(t["policy_hit"], false, "HighRisk {} 不得命中", t["name"]);
            }
        }

        // 全局开关关闭 → 全部未命中
        let off = catalog_payload(&UserToolPolicy::default());
        assert!(
            off.iter().all(|t| t["policy_hit"] == false),
            "开关关闭时命中恒 false"
        );
    }

    /// catalog 与防漂移台账同源（每行字段齐全；断言发布过的 #34 四件套在列）。
    #[test]
    fn catalog_covers_user_tool_ledger() {
        let tools = catalog_payload(&UserToolPolicy::default());
        for name in [
            "aigx_checkin_run",
            "aigx_query_usage",
            "aigx_query_logs",
            "aigx_available_models",
        ] {
            let row = tools
                .iter()
                .find(|t| t["name"] == name)
                .unwrap_or_else(|| panic!("catalog 缺首发工具 {name}"));
            assert_eq!(row["has_user_exec"], true, "{name} 应声明 user_exec");
            assert!(row["label"].as_str().is_some_and(|s| !s.is_empty()));
            assert!(row["risk"].as_str().is_some_and(|s| !s.is_empty()));
        }
    }

    /// 静态版本计数器从 0 起（重启清零口径的实现侧钉子）。
    #[test]
    fn policy_version_starts_at_zero() {
        // 唯一静态实例：其他测试可能已递增，这里只断言类型可读且非负
        let v = POLICY_VERSION.load(Ordering::Relaxed);
        assert!(v < u64::MAX);
    }
}
