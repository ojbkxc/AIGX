//! 终端用户工具权限（#34 P0 存储与判定层）。
//!
//! 设计稿：`21-tool-permission-design.md` §3/§4（lead 已拍板 D1-D8）。
//!
//! 本模块只做两件事：
//! 1. [`UserToolPolicyStore`]——与 [`crate::channel::routing_policy::RoutingPolicyStore`]
//!    同构的进程级快照（`RwLock<Arc<..>>` + `snapshot()` + 超限拒绝写入），
//!    FileStore 键 [`USER_TOOL_POLICY_KEY`]，老数据缺字段回落默认值（零迁移）。
//! 2. [`user_tool_allowed`]——纯函数判定（零 IO / 零锁），短路顺序见函数注释。
//!
//! **本批不含**：计费接线、配额——那些在后续批次接入回灌层时才引入。
//! `ToolDef.user_exec` 声明见 [`crate::agent::registry`]，写工具二次确认事件形状
//! 见 [`crate::agent::confirm`]；此处 `has_user_exec` 由调用方传入。
//!
//! 默认全关（契约 §0）：`enabled=false` 时判定一律拒绝，与历史纯聊天逐字节等价。

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::agent::tools::RiskLevel;
use crate::storage::FileStore;

/// 持久化键。
pub const USER_TOOL_POLICY_KEY: &str = "user_tool_policy";

/// 策略上限（**超限拒绝写入**，不静默截断；对齐 routing_policy.rs 口径）。
pub mod caps {
    /// 组数上限
    pub const MAX_GROUPS: usize = 500;
    /// 单组 allowlist 条目数上限
    pub const MAX_TOOLS_PER_GROUP: usize = 256;
}

/// 单组策略。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupPolicy {
    /// 允许的工具全名（`aigx_*`）；含 `"*"` 时该组获得全部 LowRisk 及以下工具。
    /// 空 = 该组无可用工具。
    #[serde(default)]
    pub allowlist: Vec<String>,
}

impl GroupPolicy {
    /// allowlist 是否放行该工具（`"*"` 通配）。
    pub fn allows(&self, tool_name: &str) -> bool {
        self.allowlist.iter().any(|t| t == "*" || t == tool_name)
    }
}

/// 终端用户工具策略（FileStore KV 快照的唯一真相）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserToolPolicy {
    /// 全局开关。**默认 false**：所有新代码路径短路 = 与历史逐字节等价
    ///（老 KV 数据无该字段 → 反序列化即 false，零迁移）。
    #[serde(default)]
    pub enabled: bool,
    /// 组名 → 组策略。未配置的组无任何工具。
    #[serde(default)]
    pub groups: BTreeMap<String, GroupPolicy>,
}

impl UserToolPolicy {
    /// 上限校验：超限返回 `Err`（供 [`UserToolPolicyStore::update`] 拒绝写入）。
    pub fn validate(&self) -> Result<(), String> {
        if self.groups.len() > caps::MAX_GROUPS {
            return Err(format!(
                "组数 {} 超过上限 {}",
                self.groups.len(),
                caps::MAX_GROUPS
            ));
        }
        for (name, g) in &self.groups {
            if g.allowlist.len() > caps::MAX_TOOLS_PER_GROUP {
                return Err(format!(
                    "组 {name} 的 allowlist 条目数 {} 超过上限 {}",
                    g.allowlist.len(),
                    caps::MAX_TOOLS_PER_GROUP
                ));
            }
        }
        Ok(())
    }
}

/// 判定结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolPermission {
    Allow,
    Deny(DenyReason),
}

/// 拒绝原因（供审计/回灌 LLM 使用，附带稳定字符串）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DenyReason {
    /// 全局开关关闭（默认态）。
    GloballyDisabled,
    /// 工具未声明用户域执行体。
    NoUserExec,
    /// 高危工具硬封顶（不可配置放宽，D2）。
    HighRisk,
    /// 用户分组不在策略中（或该组策略未启用）。
    GroupNotConfigured,
    /// 组 allowlist 未命中。
    NotInAllowlist,
}

impl DenyReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::GloballyDisabled => "user_tools_disabled",
            Self::NoUserExec => "no_user_exec",
            Self::HighRisk => "high_risk_forbidden",
            Self::GroupNotConfigured => "group_not_configured",
            Self::NotInAllowlist => "not_in_allowlist",
        }
    }
}

/// 终端用户工具判定（纯函数，零 IO / 零锁）。
///
/// 短路顺序（任一命中立即返回）：
/// 1. 全局开关 `enabled == false` → [`DenyReason::GloballyDisabled`]；
/// 2. `has_user_exec == false` → [`DenyReason::NoUserExec`]；
/// 3. `tool_risk == HighRisk` → [`DenyReason::HighRisk`]（硬封顶）；
/// 4. `user_group` 不在 `groups` → [`DenyReason::GroupNotConfigured`]；
/// 5. 组 allowlist 非 `["*"]` 且不含该工具 → [`DenyReason::NotInAllowlist`]；
/// 6. 其余 → [`ToolPermission::Allow`]（`ReadOnly` / `LowRisk` 均可）。
///
/// 写工具二次确认不在此处：判定通过后由回灌层决定是否挂 `confirm_request`。
pub fn user_tool_allowed(
    policy: &UserToolPolicy,
    user_group: &str,
    tool_name: &str,
    tool_risk: RiskLevel,
    has_user_exec: bool,
) -> ToolPermission {
    if !policy.enabled {
        return ToolPermission::Deny(DenyReason::GloballyDisabled);
    }
    if !has_user_exec {
        return ToolPermission::Deny(DenyReason::NoUserExec);
    }
    if tool_risk == RiskLevel::HighRisk {
        return ToolPermission::Deny(DenyReason::HighRisk);
    }
    let Some(group) = policy.groups.get(user_group) else {
        return ToolPermission::Deny(DenyReason::GroupNotConfigured);
    };
    if !group.allows(tool_name) {
        return ToolPermission::Deny(DenyReason::NotInAllowlist);
    }
    ToolPermission::Allow
}

/// 终端用户工具策略运行时持有者：进程级快照，保存即生效。
///
/// 数据面每请求 `snapshot()`（Arc clone），读写均无锁竞争。
/// 快照是唯一真相，不设进程级静态镜像（防两份真相漂移）。
pub struct UserToolPolicyStore {
    current: parking_lot::RwLock<Arc<UserToolPolicy>>,
    store: Option<Arc<FileStore>>,
}

impl UserToolPolicyStore {
    /// 无持久化构建（测试用；配置不落盘）。
    pub fn in_memory(policy: UserToolPolicy) -> Self {
        Self {
            current: parking_lot::RwLock::new(Arc::new(policy)),
            store: None,
        }
    }

    /// 带持久化构建（启动时调用，自动加载已存配置）。
    pub fn with_store(store: Arc<FileStore>) -> Self {
        let policy = store
            .get::<UserToolPolicy>(USER_TOOL_POLICY_KEY)
            .ok()
            .flatten()
            .unwrap_or_default();
        Self {
            current: parking_lot::RwLock::new(Arc::new(policy)),
            store: Some(store),
        }
    }

    /// 当前生效配置快照（每请求调用，Arc clone 廉价）。
    pub fn snapshot(&self) -> Arc<UserToolPolicy> {
        self.current.read().clone()
    }

    /// 更新并持久化（返回生效值）。
    ///
    /// 写入前做上限校验：**超限拒绝写入**（返回 Err），不静默截断——
    /// 静默截断会让界面显示的配置与实际生效的不一致。
    pub fn update(&self, policy: UserToolPolicy) -> anyhow::Result<UserToolPolicy> {
        policy
            .validate()
            .map_err(|e| anyhow::anyhow!("用户工具策略超限：{e}"))?;
        if let Some(store) = &self.store {
            store.put(USER_TOOL_POLICY_KEY, &policy)?;
        }
        *self.current.write() = Arc::new(policy.clone());
        Ok(policy)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    const TOOL: &str = "aigx_checkin_run";

    #[test]
    fn deny_when_globally_disabled() {
        // 默认态：即便组与工具都就绪也拒绝
        let p = policy(false, &[("vip", &[TOOL])]);
        assert_eq!(
            user_tool_allowed(&p, "vip", TOOL, RiskLevel::ReadOnly, true),
            ToolPermission::Deny(DenyReason::GloballyDisabled)
        );
        // 默认构造（零迁移）同样关闭
        assert!(!UserToolPolicy::default().enabled);
    }

    #[test]
    fn deny_when_no_user_exec() {
        let p = policy(true, &[("vip", &[TOOL])]);
        assert_eq!(
            user_tool_allowed(&p, "vip", TOOL, RiskLevel::ReadOnly, false),
            ToolPermission::Deny(DenyReason::NoUserExec)
        );
    }

    #[test]
    fn deny_high_risk_even_if_allowlisted() {
        // D2 硬封顶：即便显式配置 "*" 也拒绝
        let p = policy(true, &[("vip", &["*"])]);
        assert_eq!(
            user_tool_allowed(&p, "vip", "aigx_user_delete", RiskLevel::HighRisk, true),
            ToolPermission::Deny(DenyReason::HighRisk)
        );
    }

    #[test]
    fn deny_unknown_group() {
        let p = policy(true, &[("vip", &[TOOL])]);
        assert_eq!(
            user_tool_allowed(&p, "default", TOOL, RiskLevel::LowRisk, true),
            ToolPermission::Deny(DenyReason::GroupNotConfigured)
        );
    }

    #[test]
    fn deny_empty_allowlist() {
        let p = policy(true, &[("vip", &[])]);
        assert_eq!(
            user_tool_allowed(&p, "vip", TOOL, RiskLevel::LowRisk, true),
            ToolPermission::Deny(DenyReason::NotInAllowlist)
        );
    }

    #[test]
    fn deny_not_in_allowlist() {
        let p = policy(true, &[("vip", &["aigx_other_tool"])]);
        assert_eq!(
            user_tool_allowed(&p, "vip", TOOL, RiskLevel::ReadOnly, true),
            ToolPermission::Deny(DenyReason::NotInAllowlist)
        );
    }

    #[test]
    fn allow_readonly_and_lowrisk_when_listed() {
        let p = policy(true, &[("vip", &[TOOL])]);
        assert_eq!(
            user_tool_allowed(&p, "vip", TOOL, RiskLevel::ReadOnly, true),
            ToolPermission::Allow
        );
        assert_eq!(
            user_tool_allowed(&p, "vip", TOOL, RiskLevel::LowRisk, true),
            ToolPermission::Allow
        );
    }

    #[test]
    fn wildcard_allows_any_lowrisk() {
        let p = policy(true, &[("vip", &["*"])]);
        assert_eq!(
            user_tool_allowed(&p, "vip", "aigx_query_usage", RiskLevel::LowRisk, true),
            ToolPermission::Allow
        );
    }

    #[test]
    fn deny_reason_as_str_stable() {
        // 回灌/审计依赖这些字符串，改动视为契约变更
        assert_eq!(DenyReason::GloballyDisabled.as_str(), "user_tools_disabled");
        assert_eq!(DenyReason::NoUserExec.as_str(), "no_user_exec");
        assert_eq!(DenyReason::HighRisk.as_str(), "high_risk_forbidden");
        assert_eq!(
            DenyReason::GroupNotConfigured.as_str(),
            "group_not_configured"
        );
        assert_eq!(DenyReason::NotInAllowlist.as_str(), "not_in_allowlist");
    }

    #[test]
    fn deserialize_missing_fields_uses_defaults() {
        // 老 KV 数据无新字段 → 默认关、无组（零迁移）
        let v: UserToolPolicy = serde_json::from_str("{}").unwrap();
        assert!(!v.enabled);
        assert!(v.groups.is_empty());
        // 组缺 allowlist 亦回落空
        let v: UserToolPolicy =
            serde_json::from_str(r#"{"enabled":true,"groups":{"vip":{}}}"#).unwrap();
        assert!(v.groups["vip"].allowlist.is_empty());
    }

    #[test]
    fn validate_rejects_too_many_groups() {
        let groups: BTreeMap<String, GroupPolicy> = (0..=caps::MAX_GROUPS)
            .map(|i| (format!("g{i}"), GroupPolicy::default()))
            .collect();
        let p = UserToolPolicy {
            enabled: true,
            groups,
        };
        assert!(p.validate().is_err());
    }

    #[test]
    fn validate_rejects_too_many_tools_in_group() {
        let allowlist = vec!["t".to_string(); caps::MAX_TOOLS_PER_GROUP + 1];
        let p = UserToolPolicy {
            enabled: true,
            groups: [("vip".to_string(), GroupPolicy { allowlist })]
                .into_iter()
                .collect(),
        };
        assert!(p.validate().is_err());
    }

    #[test]
    fn validate_accepts_at_cap() {
        let p = UserToolPolicy {
            enabled: true,
            groups: (0..caps::MAX_GROUPS)
                .map(|i| (format!("g{i}"), GroupPolicy::default()))
                .collect(),
        };
        assert!(p.validate().is_ok());
    }

    #[test]
    fn store_update_hot_swap_and_rejects_over_cap() {
        let store = UserToolPolicyStore::in_memory(UserToolPolicy::default());
        assert!(!store.snapshot().enabled);

        let next = policy(true, &[("vip", &[TOOL])]);
        store.update(next).unwrap();
        let snap = store.snapshot();
        assert!(snap.enabled);
        assert_eq!(
            user_tool_allowed(&snap, "vip", TOOL, RiskLevel::LowRisk, true),
            ToolPermission::Allow
        );

        // 超限：拒绝写入且快照保持原值
        let too_many = UserToolPolicy {
            enabled: false,
            groups: (0..=caps::MAX_GROUPS)
                .map(|i| (format!("g{i}"), GroupPolicy::default()))
                .collect(),
        };
        assert!(store.update(too_many).is_err());
        assert!(store.snapshot().enabled, "拒绝写入后快照不应被改动");
    }
}
