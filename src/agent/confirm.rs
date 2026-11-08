//! 终端用户写工具**二次确认**（#34 P0，设计稿 §3.3 第 6 步）。
//!
//! 终端用户消息是不可信输入（prompt injection 可诱导模型调用**已允许的**写工具，
//! §2.1 缺口 G1）。判定层放行 `LowRisk` 写工具后，回灌层在**首次执行前**先向客户端
//! 推一条 `confirm_request` 事件，由用户在聊天中显式确认；确认通过才落到 `user_exec`。
//!
//! ## 与审批矩阵（[`crate::agent::approval`]）的分工
//!
//! 事件**形状同构**（`request_id` + 工具名 + 参数摘要），但**不进 `AgentApprovals`
//! 挂起表**：
//! - 审批矩阵是管理员会话专属（D8 不给终端用户开高危申请通道）；
//! - 审批等待 300s 长挂起，终端用户会话不应长挂起 —— 本确认超时 **30s**；
//! - 确认拒绝/超时**不算执行**，因此**不计费**（与「已执行必计费」对称，§6.2）。
//!
//! ## P0 铁律：不可降级
//!
//! [`write_requires_confirm`] 只接受风险等级、不接受任何策略/开关参数——**没有配置项
//! 能关掉写工具的确认环节**。这是刻意的签名设计：一旦引入可关闭的开关，prompt
//! injection 就有了绕过面。

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::agent::tools::RiskLevel;

/// 事件名（流式响应按此 `type` 分支；前端据此弹确认卡）。
pub const CONFIRM_EVENT: &str = "confirm_request";

/// 确认超时（秒）。远短于审批矩阵的 300s。**常量，不可配置放宽**。
pub const CONFIRM_TIMEOUT_SECS: u64 = 30;

/// 确认结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfirmDecision {
    /// 用户确认执行。
    Approved,
    /// 用户拒绝执行。
    Denied,
    /// 超时未响应（等同拒绝，不计费）。
    Timeout,
}

impl ConfirmDecision {
    /// 稳定字符串（审计/日志依赖，改动视为契约变更）。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Approved => "approved",
            Self::Denied => "denied",
            Self::Timeout => "timeout",
        }
    }

    /// 是否放行执行。
    pub fn approved(&self) -> bool {
        matches!(self, Self::Approved)
    }
}

/// `confirm_request` 事件载荷。
///
/// 字段与 [`crate::agent::runner::AgentEvent::ApprovalRequest`] 同构
/// （`request_id` / 工具名 / 参数），另带人类可读标签、超时与时间戳，便于前端
/// 复用审批卡的展示逻辑；**不落盘**（终端用户确认是瞬时交互，无需重启留痕）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfirmRequest {
    /// 一次性确认 ID（调用方回灌决策时带上）。
    pub request_id: String,
    /// 工具全名（`aigx_*`）。
    pub tool: String,
    /// 工具中文短名（`ToolDef.label`，确认卡标题用）。
    pub label: String,
    /// 参数摘要（脱敏由调用方负责，这里只存文本）。
    pub args_summary: String,
    /// 超时秒数（= [`CONFIRM_TIMEOUT_SECS`]，随事件下发便于前端倒计时）。
    pub timeout_secs: u64,
    /// 创建时间（unix 秒）。
    pub created_at: i64,
}

impl ConfirmRequest {
    /// 生成一次确认请求（`request_id` 32 位十六进制随机，与审批矩阵同构）。
    pub fn new(tool: &str, label: &str, args_summary: &str) -> Self {
        Self {
            request_id: format!("{:032x}", rand::random::<u128>()),
            tool: tool.to_string(),
            label: label.to_string(),
            args_summary: args_summary.to_string(),
            timeout_secs: CONFIRM_TIMEOUT_SECS,
            created_at: chrono::Utc::now().timestamp(),
        }
    }

    /// 等待超时（供回灌层 `tokio::time::timeout` 使用）。
    pub fn timeout(&self) -> Duration {
        Duration::from_secs(self.timeout_secs)
    }
}

/// 该风险等级的工具在用户域执行前是否必须先过二次确认。
///
/// - `ReadOnly` → `false`（只读无副作用，直接执行）；
/// - `LowRisk` / `HighRisk` → `true`（**P0 铁律，不可降级**）。
///
/// 函数签名刻意不含策略/开关参数：没有任何配置能关闭写工具确认。
pub fn write_requires_confirm(risk: RiskLevel) -> bool {
    !matches!(risk, RiskLevel::ReadOnly)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_writes_require_confirm() {
        assert!(!write_requires_confirm(RiskLevel::ReadOnly));
        // P0 铁律：写工具（低危/高危）恒需确认，不可降级
        assert!(write_requires_confirm(RiskLevel::LowRisk));
        assert!(write_requires_confirm(RiskLevel::HighRisk));
    }

    #[test]
    fn event_name_and_timeout_are_contract() {
        assert_eq!(CONFIRM_EVENT, "confirm_request");
        assert_eq!(CONFIRM_TIMEOUT_SECS, 30);
        // 远短于审批矩阵 300s（编译期校验）
        const { assert!(CONFIRM_TIMEOUT_SECS < 300) };
    }

    #[test]
    fn new_request_carries_timeout_and_ids() {
        let r = ConfirmRequest::new("aigx_checkin_run", "立即签到", "{}");
        assert_eq!(r.tool, "aigx_checkin_run");
        assert_eq!(r.label, "立即签到");
        assert_eq!(r.timeout_secs, CONFIRM_TIMEOUT_SECS);
        assert_eq!(r.timeout(), Duration::from_secs(30));
        assert_eq!(r.request_id.len(), 32);
        assert!(r.request_id.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn decision_strings_stable_and_roundtrip() {
        assert_eq!(ConfirmDecision::Approved.as_str(), "approved");
        assert_eq!(ConfirmDecision::Denied.as_str(), "denied");
        assert_eq!(ConfirmDecision::Timeout.as_str(), "timeout");
        assert!(ConfirmDecision::Approved.approved());
        assert!(!ConfirmDecision::Denied.approved());
        assert!(!ConfirmDecision::Timeout.approved());

        let r = ConfirmRequest::new("aigx_checkin_run", "立即签到", "{\"k\":1}");
        let json = serde_json::to_string(&r).unwrap();
        let back: ConfirmRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(r, back);
    }
}
