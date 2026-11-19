//! 凭证出生版本（对齐 Agent2API v3.5.0-beta.2
//! `core/account_store/credential_version.rs` 的通用契约）。
//!
//! 参考树把「凭证出生代次」作为账号行里的**宿主私有字段**（`_hostCredentialVersion`）
//! 与公开的 `accountCredentialEpoch` 分开：只有真实写入事务返回的版本戳才算数，
//! 缺失版本**不是第 0 代**，插件 JSON / 日志 / ID 都不能用来重建来源。
//!
//! AIGX 的账号存储是 `FileStore` + 内存会话（无 SQL 行），因此这里提供**与存储无关**
//! 的版本模型与字段剥离工具，供账号落盘/导入/插件互操作复用：
//! - 常量 [`PRIVATE_FIELD`] / [`EPOCH_KEY`]；
//! - [`CredentialVersion`]：epoch（按类型持久）+ revision（每次真实写入推进）；
//! - [`RowProvider`]：固定只读来源集合（不含任意 provider JSON）；
//! - [`strip_private`] / [`strip_imported_row_version`]：剥离旧导入记录的宿主标记。

// 本模块的公开 API 覆盖 beta.2 的完整契约面，部分符号为后续接线/线表预留。
#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// 只存在于加密账号行顶层的宿主私有版本字段（插件 secret / 公开字段里的同名项
/// 不是版本凭据）。
pub const PRIVATE_FIELD: &str = "_hostCredentialVersion";
/// 公开的凭证代次键。
pub const EPOCH_KEY: &str = "accountCredentialEpoch";

/// 该键是否是宿主私有版本字段。
pub fn private_field(key: &str) -> bool {
    key == PRIVATE_FIELD
}

/// 从字段表移除宿主私有版本字段（返回是否真的移除了）。
pub fn strip_private(fields: &mut Map<String, Value>) -> bool {
    fields.remove(PRIVATE_FIELD).is_some()
}

/// 固定只读来源集合：这些 provider 的旧导入记录可以剥离宿主标记，
/// 但**不签发**任何发送许可，也不接受任意 provider JSON。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowProvider {
    MonkeyCode,
    MonkeyCodeIntl,
    DoubaoWork,
    CatPaw,
    ClineFree,
}

impl RowProvider {
    pub fn parse(provider: &str) -> Option<Self> {
        match provider {
            "monkeycode" => Some(Self::MonkeyCode),
            "monkeycode-intl" => Some(Self::MonkeyCodeIntl),
            "doubaowork" | "doubao" => Some(Self::DoubaoWork),
            "catpaw" => Some(Self::CatPaw),
            "cline-free" => Some(Self::ClineFree),
            _ => None,
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Self::MonkeyCode => "monkeycode",
            Self::MonkeyCodeIntl => "monkeycode-intl",
            Self::DoubaoWork => "doubaowork",
            Self::CatPaw => "catpaw",
            Self::ClineFree => "cline-free",
        }
    }
}

/// 凭证出生版本：`epoch` 标识一次纳管代次，`revision` 在同一代内每次真实写入推进。
///
/// 缺失版本（`None`）**不是**第 0 代；调用方必须用真实写入事务返回的版本。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialVersion {
    pub epoch: u64,
    pub revision: u64,
}

impl CredentialVersion {
    /// 新建一代（纳管入口）：revision 从 1 起，避免把「无记录」误当 revision 0。
    pub fn born(epoch: u64) -> Self {
        Self { epoch, revision: 1 }
    }

    /// 同一代内的下一次真实写入。
    pub fn bump(self) -> Option<Self> {
        Some(Self {
            epoch: self.epoch,
            revision: self.revision.checked_add(1)?,
        })
    }

    /// 从账号行顶层读取；缺失返回 `None`（不构造第 0 代）。
    pub fn from_row(fields: &Map<String, Value>) -> Option<Self> {
        let raw = fields.get(PRIVATE_FIELD)?;
        Some(Self {
            epoch: raw.get(EPOCH_KEY).and_then(Value::as_u64)?,
            revision: raw.get("revision").and_then(Value::as_u64)?,
        })
    }

    /// 写入账号行顶层私有字段。
    pub fn write_row(self, fields: &mut Map<String, Value>) {
        let mut object = Map::new();
        object.insert(EPOCH_KEY.to_string(), Value::from(self.epoch));
        object.insert("revision".to_string(), Value::from(self.revision));
        fields.insert(PRIVATE_FIELD.to_string(), Value::Object(object));
    }
}

/// 仅剥离固定来源旧导入记录的顶层宿主标记；不改业务字段，不签发来源。
pub fn strip_imported_row_version(provider: &str, fields: &mut Map<String, Value>) {
    if RowProvider::parse(provider).is_some() {
        strip_private(fields);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_version_is_not_generation_zero() {
        let fields = Map::new();
        assert!(CredentialVersion::from_row(&fields).is_none());
    }

    #[test]
    fn version_roundtrips_and_bumps() {
        let mut fields = Map::new();
        let v = CredentialVersion::born(7);
        v.write_row(&mut fields);
        assert_eq!(CredentialVersion::from_row(&fields), Some(v));
        let next = v.bump().unwrap();
        assert_eq!(next.epoch, 7);
        assert_eq!(next.revision, 2);
    }

    #[test]
    fn strip_only_fixed_providers() {
        let mut fields = Map::new();
        fields.insert(PRIVATE_FIELD.to_string(), Value::from(1));
        // 未知 provider 不剥离
        strip_imported_row_version("some-other", &mut fields);
        assert!(fields.contains_key(PRIVATE_FIELD));
        strip_imported_row_version("cline-free", &mut fields);
        assert!(!fields.contains_key(PRIVATE_FIELD));
    }

    #[test]
    fn row_provider_parse_ids_roundtrip() {
        for id in [
            "monkeycode",
            "monkeycode-intl",
            "doubaowork",
            "catpaw",
            "cline-free",
        ] {
            let p = RowProvider::parse(id).expect("known provider");
            assert_eq!(p.id(), if id == "doubaowork" { "doubaowork" } else { id });
        }
        assert!(RowProvider::parse("nope").is_none());
    }
}
