//! 脚本包 options 的校验 —— 字节上限 + 设置 schema 逐键对照。
//!
//! ## 为什么 options 要单独校验
//!
//! options 经管理面编辑、随宿主 init 全量下发、由每个脚本在每次调用时读取：
//! 无上限的配置体既拖慢每次握手（宿主读 `registry.json` 投影），也给了
//! 「把数据当配置塞」的滥用面。上限 [`MAX_SETTINGS_BYTES`]（256KiB）与
//! types.rs 的契约常量同源。
//!
//! ## schema 来源的现状
//!
//! [`ScriptSettingField`] 是包自报的形态（string/number/boolean/select…），
//! 但**当前包清单不带 schema**——所以校验分两层：
//!
//! - [`validate_values`]：永远执行（大小 + 顶层必须是对象 + 键非空）；
//! - [`validate_against_schema`]：调用方持有 schema 时执行（管理面把包
//!   清单里的 schema 传进来即可），无 schema 时只做第一层。
//!
//! 未知键不拒绝：包升级可能删字段，旧配置残留不应让升级失败（向前兼容）。

use serde_json::{Map, Value};

use super::types::{ScriptSettingField, MAX_SETTINGS_BYTES};

/// 校验一份 options（字节上限 + 键非空）。
pub fn validate_values(values: &Map<String, Value>) -> Result<(), String> {
    for key in values.keys() {
        if key.trim().is_empty() {
            return Err("options 的键不能为空".into());
        }
    }
    let bytes = serde_json::to_vec(values).map_err(|e| format!("options 无法序列化：{e}"))?;
    if bytes.len() > MAX_SETTINGS_BYTES {
        return Err(format!(
            "options 过大：{} bytes（上限 {MAX_SETTINGS_BYTES}）",
            bytes.len()
        ));
    }
    Ok(())
}

/// 按包声明的 schema 逐键对照（类型 / 必填 / select 候选）。
///
/// 未在 schema 里的键放行（见模块注释「未知键不拒绝」）。
pub fn validate_against_schema(
    values: &Map<String, Value>,
    schema: &[ScriptSettingField],
) -> Result<(), String> {
    validate_values(values)?;
    for field in schema {
        let value = values.get(&field.key);
        let Some(value) = value else {
            if field.required {
                return Err(format!("缺少必填配置项：{}（{}）", field.key, field.label));
            }
            continue;
        };
        if value.is_null() {
            if field.required {
                return Err(format!(
                    "必填配置项不能为空：{}（{}）",
                    field.key, field.label
                ));
            }
            continue;
        }
        let kind = field.kind.as_deref().unwrap_or("");
        let ok = match kind {
            "string" | "text" | "password" => value.is_string(),
            "number" | "integer" => value.is_number(),
            "boolean" => value.is_boolean(),
            "select" => {
                value.is_string()
                    && (field.choices.is_empty()
                        || value
                            .as_str()
                            .is_some_and(|s| field.choices.iter().any(|c| c == s)))
            }
            "array" => value.is_array(),
            "object" => value.is_object(),
            // 未知 kind：不猜类型（包自报的扩展类型原样透传）
            _ => true,
        };
        if !ok {
            return Err(format!(
                "配置项 {}（{}）类型不符：期望 {kind}",
                field.key, field.label
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn map(pairs: &[(&str, Value)]) -> Map<String, Value> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect()
    }

    #[test]
    fn accepts_small_object() {
        let v = map(&[("region", json!("cn")), ("retries", json!(3))]);
        assert!(validate_values(&v).is_ok());
    }

    #[test]
    fn rejects_oversize_options() {
        let big = "x".repeat(MAX_SETTINGS_BYTES + 1);
        let v = map(&[("blob", json!(big))]);
        let err = validate_values(&v).unwrap_err();
        assert!(err.contains("过大"), "错误应说明大小：{err}");
    }

    #[test]
    fn rejects_empty_key() {
        let v = map(&[(" ", json!(1))]);
        assert!(validate_values(&v).is_err());
    }

    #[test]
    fn schema_type_and_required_checks() {
        let schema = vec![
            ScriptSettingField {
                key: "region".into(),
                label: "区域".into(),
                kind: Some("select".into()),
                required: true,
                choices: vec!["cn".into(), "us".into()],
                ..Default::default()
            },
            ScriptSettingField {
                key: "retries".into(),
                label: "重试次数".into(),
                kind: Some("number".into()),
                ..Default::default()
            },
        ];
        // 合规
        assert!(validate_against_schema(
            &map(&[("region", json!("cn")), ("retries", json!(2))]),
            &schema
        )
        .is_ok());
        // 缺必填
        assert!(validate_against_schema(&map(&[("retries", json!(2))]), &schema).is_err());
        // select 越界
        assert!(validate_against_schema(&map(&[("region", json!("moon"))]), &schema).is_err());
        // 类型不符
        assert!(validate_against_schema(
            &map(&[("region", json!("cn")), ("retries", json!("x"))]),
            &schema
        )
        .is_err());
        // 未知键放行
        assert!(validate_against_schema(
            &map(&[("region", json!("cn")), ("legacy", json!(true))]),
            &schema
        )
        .is_ok());
    }

    #[test]
    fn unknown_kind_passes_through() {
        let schema = vec![ScriptSettingField {
            key: "x".into(),
            label: "X".into(),
            kind: Some("someday-format".into()),
            ..Default::default()
        }];
        assert!(validate_against_schema(&map(&[("x", json!({"a": 1}))]), &schema).is_ok());
    }
}
