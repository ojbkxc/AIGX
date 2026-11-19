//! 凭据续期就绪检查 —— 对齐 Agent2API v3.5.0-beta.2 `plugin_host::rpc_refresh`。
//!
//! 上游 `rpc_refresh` 在调 `models.refresh` 前检查原账号的续期登记是否就绪：
//! 只观察既有登记，不补建槽、不恢复旧代次、不开放普通插件换证。
//!
//! AIGX 的架构不同：续期由 `account_session` 层管理，不在 `plugin_host` 内
//! 直接访问 `AccountStore`。本模块提供接口形状与错误码对齐，实际续期逻辑
//! 由上层（bridge / account_session）在调插件前自行检查。

/// 续期就绪错误码（对齐上游）。
pub(crate) const CODE_REGISTRATION_REQUIRED: &str = "refresh_registration_required";
pub(crate) const CODE_REFRESH_UNCONFIRMED: &str = "refresh_unconfirmed";
pub(crate) const CODE_REFRESH_UNAVAILABLE: &str = "credential_refresh_unavailable";

/// 续期就绪检查结果。
///
/// AIGX 的 `plugin_host` 不直接持有 `AccountStore`，故本函数只做协议层面的
/// 占位：返回 `Ok(())` 表示「不阻断」，实际续期由上层在调插件前自行处理。
///
/// 对齐上游 `rpc_refresh::readiness` 的签名形状与错误码，但不含 provider 专属
/// 逻辑（codearts / cline-free），那些由 AIGX 的 `account_session` 层实现。
pub(crate) fn readiness(_provider: &str, _account: &str) -> Result<(), RefreshError> {
    // AIGX：续期由上层 account_session 管理，plugin_host 不阻断。
    Ok(())
}

/// 续期错误（对齐上游 `GatewayError` 的 code + message）。
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub(crate) struct RefreshError {
    pub code: &'static str,
    pub message: String,
}

impl RefreshError {
    pub fn registration(provider: &str) -> Self {
        Self {
            code: CODE_REGISTRATION_REQUIRED,
            message: format!(
                "{} 当前账号没有可用的自动续期登记，请重新登录或更新凭据",
                provider
            ),
        }
    }

    pub fn unconfirmed(provider: &str) -> Self {
        Self {
            code: CODE_REFRESH_UNCONFIRMED,
            message: format!("{} 上次续期来源未确认，请先查看原操作状态", provider),
        }
    }

    pub fn unavailable(provider: &str) -> Self {
        Self {
            code: CODE_REFRESH_UNAVAILABLE,
            message: format!("{} 原账号续期前提暂不可确认，未发送换证请求", provider),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readiness_passes_by_default() {
        assert!(readiness("any", "acct").is_ok());
    }

    #[test]
    fn error_codes_match_upstream() {
        assert_eq!(CODE_REGISTRATION_REQUIRED, "refresh_registration_required");
        assert_eq!(CODE_REFRESH_UNCONFIRMED, "refresh_unconfirmed");
        assert_eq!(CODE_REFRESH_UNAVAILABLE, "credential_refresh_unavailable");
    }

    #[test]
    fn registration_error_has_provider_name() {
        let err = RefreshError::registration("codearts");
        assert!(err.message.contains("codearts"));
    }
}
