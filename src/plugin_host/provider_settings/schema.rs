// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
// Source: desktop-tauri/src-tauri/server/src/server/core/plugin_host/provider_settings/schema.rs
//!
//! 插件「提供商设置」的声明与校验（A2A `settings.schema.v1` 形态）。
//!
//! 插件在握手里为某个 provider 声明一份 `settings_schema`（字段清单 +
//! 类型/范围/正则/选项），网关据此渲染表单并在保存时逐字段校验。本模块
//! 只做**声明解析与值归一**，不碰存储与宿主（那是 `mod.rs` 的事）。
//!
//! ## 与 A2A 的差异
//!
//! A2A 的 `setting_key` 依赖 `config::PLUGIN_SETTING_GATEWAY_PROMPT` 等
//! 常量与 `rpc_cache::SETTING_MODEL_CATALOG`（AIGX 无这些键）；AIGX 版只
//! 保留通用规则：标识符字符集、拒绝 `host.` 前缀、拒绝含凭据词的键
//! （凭据应走账号入口，不进「设置」）。

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 一份提供商设置声明。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Schema {
    /// 声明版本（当前恒 1）。
    pub version: u32,
    /// 分组标题（字符串或本地化对象）。
    pub title: Value,
    #[serde(default)]
    pub description: Value,
    /// 多个 provider 共用同一份存储命名空间时指定（缺省 = 各自独立）。
    #[serde(default)]
    pub storage_provider: Option<String>,
    /// 是否允许插件在运行期上报「覆盖值」（A2A `runtime_overrides`）。
    #[serde(default)]
    pub runtime_overrides: bool,
    /// 字段清单。
    pub fields: Vec<Field>,
}

/// 一个可编辑字段。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Field {
    pub key: String,
    pub label: Value,
    #[serde(default)]
    pub description: Value,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub default: Value,
    #[serde(default)]
    pub trim: bool,
    #[serde(default)]
    pub empty_is_default: bool,
    #[serde(default)]
    pub min_length: Option<usize>,
    #[serde(default)]
    pub max_length: Option<usize>,
    #[serde(default)]
    pub pattern: Option<String>,
    #[serde(default)]
    pub minimum: Option<f64>,
    #[serde(default)]
    pub maximum: Option<f64>,
    #[serde(default)]
    pub options: Vec<Choice>,
}

/// select 的一个候选项。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Choice {
    pub value: Value,
    pub label: Value,
}

/// 标识符合法性（provider / 字段键 / 存储命名空间）。
pub fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        && !matches!(value, "__proto__" | "prototype" | "constructor")
}

/// 文案（标题/说明/标签）合法性：纯字符串或本地化对象。
pub fn text(value: &Value, optional: bool) -> bool {
    let valid =
        |v: &str| v.len() <= 8192 && !v.contains('\0') && (optional || !v.trim().is_empty());
    match value {
        Value::Null => optional,
        Value::String(v) => valid(v),
        Value::Object(map) => {
            !map.is_empty()
                && map.len() <= 16
                && map
                    .iter()
                    .all(|(locale, v)| identifier(locale) && v.as_str().is_some_and(valid))
        }
        _ => false,
    }
}

/// 字段键是否允许（拒绝 `host.` 前缀、网关保留键与凭据词）。
fn setting_key(key: &str) -> bool {
    let normalized = key.to_ascii_lowercase().replace(['_', '-', '.'], "");
    identifier(key)
        && !key.starts_with("host.")
        // 网关保留键：插件问它是「读网关持有的目录」，不是自己的可写设置
        //（= 上游 `schema.rs` 的保留键组，AIGX 现只有 modelCatalog）。
        && key != crate::plugin_host::model_catalog::SETTING_MODEL_CATALOG
        && normalized != "key"
        && ![
            "token",
            "secret",
            "password",
            "cookie",
            "authorization",
            "apikey",
            "credential",
        ]
        .iter()
        .any(|word| normalized.contains(word))
}

impl Field {
    /// 归一一个值：`Ok(Null)` = 清空；`Err(文案)` = 不合规。
    pub fn normalize(&self, value: &Value) -> Result<Value, String> {
        if value.is_null() {
            return Ok(Value::Null);
        }
        if value.to_string().len() > 16 * 1024 {
            return Err("设置值超过大小限制".into());
        }
        match self.kind.as_str() {
            "string" => {
                let raw = value.as_str().ok_or("设置值必须是文本")?;
                let text = if self.trim { raw.trim() } else { raw };
                if text.is_empty() && self.empty_is_default {
                    return Ok(Value::Null);
                }
                let length = text.chars().count();
                if text.contains('\0')
                    || length < self.min_length.unwrap_or(0)
                    || length > self.max_length.unwrap_or(8192)
                {
                    return Err("文本长度不符合插件声明的范围".into());
                }
                if let Some(pattern) = &self.pattern {
                    let regex = regex::Regex::new(pattern).map_err(|_| "插件设置校验规则无效")?;
                    if !regex.is_match(text) {
                        return Err("文本格式不符合插件声明的规则".into());
                    }
                }
                Ok(Value::String(text.into()))
            }
            "number" => {
                let number = value
                    .as_f64()
                    .filter(|v| v.is_finite())
                    .ok_or("设置值必须是数字")?;
                if self.minimum.is_some_and(|min| number < min)
                    || self.maximum.is_some_and(|max| number > max)
                {
                    return Err("数字不在插件声明的范围内".into());
                }
                Ok(value.clone())
            }
            "boolean" if value.is_boolean() => Ok(value.clone()),
            "select" if self.options.iter().any(|option| option.value == *value) => {
                Ok(value.clone())
            }
            _ => Err("设置值不符合插件声明的类型或选项".into()),
        }
    }
}

impl Schema {
    /// 存储命名空间（`storage_provider` 缺省 = provider 自己）。
    pub fn owner<'a>(&'a self, provider: &'a str) -> &'a str {
        self.storage_provider.as_deref().unwrap_or(provider)
    }

    /// 声明本身是否自洽（版本/标题/字段/范围/选项）。
    pub fn validate(&self) -> Result<(), String> {
        if self.version != 1
            || !text(&self.title, false)
            || !text(&self.description, true)
            || self.fields.is_empty()
            || self.fields.len() > 64
            || self
                .storage_provider
                .as_deref()
                .is_some_and(|owner| !identifier(owner))
        {
            return Err("插件设置声明的版本、标题或范围无效".into());
        }
        let mut keys = HashSet::new();
        for field in &self.fields {
            let mut choices = HashSet::new();
            if field
                .options
                .iter()
                .any(|option| !choices.insert(option.value.to_string()))
            {
                return Err("插件设置选项值不能重复".into());
            }
            let kind_ok = matches!(
                field.kind.as_str(),
                "string" | "number" | "boolean" | "select"
            );
            if !setting_key(&field.key)
                || !keys.insert(&field.key)
                || !text(&field.label, false)
                || !text(&field.description, true)
                || !kind_ok
                // 非 string 不得带 string 专属约束
                || (field.kind != "string"
                    && (field.trim
                        || field.empty_is_default
                        || field.min_length.is_some()
                        || field.max_length.is_some()
                        || field.pattern.is_some()))
                || (field.kind != "number" && (field.minimum.is_some() || field.maximum.is_some()))
                || (field.kind != "select" && !field.options.is_empty())
                || field.min_length.unwrap_or(0) > field.max_length.unwrap_or(8192)
                || field.max_length.unwrap_or(8192) > 8192
                || field
                    .minimum
                    .zip(field.maximum)
                    .is_some_and(|(min, max)| min > max)
                || field.options.len() > 64
                || (field.kind == "select" && field.options.is_empty())
                || field.options.iter().any(|option| {
                    !text(&option.label, false)
                        || !option.value.is_string()
                        || option.value.as_str().is_some_and(|v| {
                            v.is_empty() || v.len() > 1024 || v.contains('\0')
                        })
                })
            {
                return Err("插件设置字段、选项或限制无效；凭据应通过账号入口保存".into());
            }
            if let Some(pattern) = &field.pattern {
                if pattern.len() > 1024 || regex::Regex::new(pattern).is_err() {
                    return Err("插件设置校验规则无效".into());
                }
            }
            if field.normalize(&field.default)? != field.default {
                return Err("插件设置默认值不是规范值".into());
            }
        }
        Ok(())
    }

    /// 声明的字段键集合（`configure` 用来拒绝未声明字段）。
    #[allow(dead_code)] // 预留/线表符号（见 main.rs 债务清单）
    pub fn has_field(&self, key: &str) -> bool {
        self.fields.iter().any(|field| field.key == key)
    }

    /// 按声明归一一份「有效值」（缺省补 default，非法记入 `invalid`）。
    pub fn effective(
        &self,
        values: &serde_json::Map<String, Value>,
    ) -> (
        serde_json::Map<String, Value>,
        serde_json::Map<String, Value>,
    ) {
        let mut effective = serde_json::Map::new();
        let mut invalid = serde_json::Map::new();
        for field in &self.fields {
            match field.normalize(values.get(&field.key).unwrap_or(&Value::Null)) {
                Ok(value) => {
                    effective.insert(
                        field.key.clone(),
                        if value.is_null() {
                            field.default.clone()
                        } else {
                            value
                        },
                    );
                }
                Err(_) => {
                    effective.insert(field.key.clone(), Value::Null);
                    invalid.insert(field.key.clone(), Value::Bool(true));
                }
            }
        }
        (effective, invalid)
    }
}

/// 从 provider 上报的 `settings_schema`（+ 可选 `settings_group`）解析声明。
///
/// `Ok(None)` = 该 provider 没声明设置（正常）；`Err` = 声明存在但非法
/// （拒绝加载该 provider 的设置面，不影响 provider 本身可用）。
pub fn parse(provider_id: &str, schema: &Value) -> Result<Option<Schema>, String> {
    if schema.is_null() {
        return Ok(None);
    }
    if !identifier(provider_id) {
        return Err("插件设置提供商标识无效".into());
    }
    if schema.to_string().len() > 128 * 1024 {
        return Err("插件设置声明超过大小限制".into());
    }
    let parsed: Schema =
        serde_json::from_value(schema.clone()).map_err(|_| "插件设置声明格式无效")?;
    parsed.validate()?;
    Ok(Some(parsed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn schema(fields: Value) -> Value {
        json!({ "version": 1, "title": "设置", "fields": fields })
    }

    #[test]
    fn parse_accepts_valid_schema() {
        let s = schema(json!([
            { "key": "region", "label": "区域", "type": "string", "max_length": 8 },
            { "key": "retries", "label": "重试", "type": "number", "minimum": 0.0, "maximum": 10.0 },
            { "key": "enabled", "label": "启用", "type": "boolean", "default": false }
        ]));
        let parsed = parse("kiro", &s).unwrap().expect("应有声明");
        assert_eq!(parsed.fields.len(), 3);
        assert!(parsed.has_field("region"));
        assert!(!parsed.has_field("nope"));
    }

    #[test]
    fn parse_null_is_none() {
        assert!(parse("kiro", &Value::Null).unwrap().is_none());
    }

    #[test]
    fn parse_rejects_bad_identifier_and_credential_keys() {
        assert!(parse("bad id!", &schema(json!([]))).is_err());
        // 含凭据词的键拒绝（凭据应走账号入口）
        for key in ["api_key", "token", "password", "host.foo"] {
            let s = schema(json!([{ "key": key, "label": "x", "type": "string" }]));
            assert!(parse("kiro", &s).is_err(), "键 {key} 应被拒绝");
        }
    }

    /// 网关保留键 `modelCatalog` 不得出现在插件设置声明里（上游
    /// `schema.rs` 同一组保留键）：它是「读网关持有的目录」的通道，
    /// 不是插件自己的可写设置。正向对照：同形状的普通键可被接受。
    #[test]
    fn parse_rejects_reserved_model_catalog_key() {
        let reserved: &str = crate::plugin_host::model_catalog::SETTING_MODEL_CATALOG;
        let s = schema(json!([{ "key": reserved, "label": "目录", "type": "string" }]));
        assert!(parse("kiro", &s).is_err(), "保留键应被拒绝");
        // 正向对照：同形状但非保留键可接受
        let ok = schema(json!([{ "key": "catalogRegion", "label": "目录", "type": "string" }]));
        assert!(parse("kiro", &ok).unwrap().is_some());
    }

    #[test]
    fn parse_rejects_duplicate_and_empty_fields() {
        assert!(parse("kiro", &schema(json!([]))).is_err(), "空字段清单应拒");
        let dup = schema(json!([
            { "key": "a", "label": "A", "type": "string" },
            { "key": "a", "label": "B", "type": "string" }
        ]));
        assert!(parse("kiro", &dup).is_err());
    }

    #[test]
    fn normalize_enforces_type_and_range() {
        let s = parse(
            "p",
            &schema(json!([
                { "key": "n", "label": "N", "type": "number", "minimum": 1.0, "maximum": 5.0 },
                { "key": "t", "label": "T", "type": "string", "max_length": 3 },
                { "key": "b", "label": "B", "type": "boolean" }
            ])),
        )
        .unwrap()
        .unwrap();
        let n = &s.fields[0];
        assert_eq!(n.normalize(&json!(3)).unwrap(), json!(3));
        assert!(n.normalize(&json!(9)).is_err(), "超上限应拒");
        assert!(n.normalize(&json!("x")).is_err(), "非数字应拒");
        let t = &s.fields[1];
        assert!(t.normalize(&json!("abcd")).is_err(), "超长应拒");
        assert_eq!(t.normalize(&Value::Null).unwrap(), Value::Null);
        let b = &s.fields[2];
        assert!(b.normalize(&json!("yes")).is_err(), "非布尔应拒");
    }

    #[test]
    fn effective_fills_defaults_and_flags_invalid() {
        let s = parse(
            "p",
            &schema(json!([
                { "key": "a", "label": "A", "type": "string", "default": "x" },
                { "key": "n", "label": "N", "type": "number", "minimum": 0.0 }
            ])),
        )
        .unwrap()
        .unwrap();
        let mut values = serde_json::Map::new();
        values.insert("n".into(), json!("bad"));
        let (effective, invalid) = s.effective(&values);
        assert_eq!(effective.get("a"), Some(&json!("x")), "缺省补 default");
        assert_eq!(invalid.get("n"), Some(&json!(true)), "非法值记 invalid");
    }

    #[test]
    fn select_requires_declared_choice() {
        let s = parse(
            "p",
            &schema(json!([{ "key": "mode", "label": "模式", "type": "select",
                "options": [{ "value": "a", "label": "A" }, { "value": "b", "label": "B" }] }])),
        )
        .unwrap()
        .unwrap();
        let f = &s.fields[0];
        assert_eq!(f.normalize(&json!("a")).unwrap(), json!("a"));
        assert!(f.normalize(&json!("c")).is_err(), "未声明选项应拒");
    }
}
