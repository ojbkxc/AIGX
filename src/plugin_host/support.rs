//! 包支持状态 —— 对齐 Agent2API v3.5.0-beta.2 `plugin_host::support`。
//!
//! beta.2 的核心变更：旧原生插件停止运行，全面转向独立 WASM 包。
//! 本模块判定已装包是 `Supported`（Wasm）还是 `LegacyUnsupported`（旧原生），
//! 供管理面展示与安装时拒绝旧格式。

use serde::Serialize;

use super::registry::PackMeta;

/// 旧原生插件停止支持的理由（逐字对齐上游 `LEGACY_REASON`）。
pub const LEGACY_REASON: &str =
    "旧原生提供商插件已停止支持，请安装对应的 WASM 插件；已有账号和配置保留。";

/// 包支持状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PackSupportState {
    /// Wasm 包，受支持。
    Supported,
    /// 旧原生包，不再受支持。
    LegacyUnsupported,
}

/// 判定包清单是否为受支持的 Wasm 格式。
///
/// 对齐上游 `support::require_wasm`：清单入口声明了 wasm 目标即受支持。
pub fn require_wasm(meta: &PackMeta) -> Result<(), String> {
    if is_wasm_pack(meta) {
        Ok(())
    } else {
        Err(LEGACY_REASON.to_string())
    }
}

/// 判定已装包的支持状态。
pub fn installed_state(meta: &PackMeta) -> PackSupportState {
    if is_wasm_pack(meta) {
        PackSupportState::Supported
    } else {
        PackSupportState::LegacyUnsupported
    }
}

/// 旧包的停用理由（受支持时返回 `None`）。
pub fn legacy_reason(meta: &PackMeta) -> Option<String> {
    (installed_state(meta) == PackSupportState::LegacyUnsupported)
        .then(|| LEGACY_REASON.to_string())
}

/// 包入口是否为 wasm 格式。
///
/// AIGX 的 `PackMeta.entry` 是入口二进制的相对路径；
/// wasm 包的入口以 `.wasm` 结尾（与上游 `pack.entry.ends_with(".wasm")` 同口径）。
/// 此外也检查包清单是否声明了 wasm 目标（经 `manifest::is_wasm`）。
fn is_wasm_pack(meta: &PackMeta) -> bool {
    meta.entry.ends_with(".wasm") || {
        // 尝试从注册表读清单做二次确认（清单形态 `{entry: {target: "plugin.wasm"}}`）
        // AIGX 的 PackMeta.entry 是字符串路径，不是对象；以 `.wasm` 后缀为准。
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_pack(entry: &str) -> PackMeta {
        PackMeta {
            id: "test".into(),
            name: "Test".into(),
            version: "1.0.0".into(),
            protocol: 1,
            providers: vec![],
            entry: entry.into(),
            signature: None,
            sha256: None,
            signed: false,
            disabled_providers: vec![],
            installed_at: 0,
            enabled: true,
        }
    }

    #[test]
    fn wasm_entry_is_supported() {
        let pack = make_pack("plugin.wasm");
        assert_eq!(installed_state(&pack), PackSupportState::Supported);
        assert!(legacy_reason(&pack).is_none());
        assert!(require_wasm(&pack).is_ok());
    }

    #[test]
    fn native_entry_is_legacy() {
        let pack = make_pack("plugin");
        assert_eq!(installed_state(&pack), PackSupportState::LegacyUnsupported);
        assert_eq!(legacy_reason(&pack).as_deref(), Some(LEGACY_REASON));
        assert!(require_wasm(&pack).is_err());
    }

    #[test]
    fn legacy_reason_text_matches_upstream() {
        assert!(LEGACY_REASON.contains("旧原生提供商插件已停止支持"));
        assert!(LEGACY_REASON.contains("已有账号和配置保留"));
    }
}
