// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//! 公开视图校验 —— 防止插件把凭据漏进「可展示给用户」的字段。
//!
//! 与上游逐字一致：
//!
//! 1. 从 `secret` 里递归收集所有「敏感键下的 ≥8 字符字符串」；
//! 2. 在 `public` 里递归检查——敏感键只允许布尔/时间戳/尾号等安全标量，
//!    其余字符串不得**包含**任何一个 secret 片段；
//! 3. `secret` 必须是对象（≤256 KiB）、`public` 必须是对象（≤64 KiB）。

use serde_json::Value;

use super::wire::WireError;

fn normalized(key: &str) -> String {
    key.to_ascii_lowercase().replace(['_', '-'], "")
}

fn secret_key(key: &str) -> bool {
    let key = normalized(key);
    key.contains("token")
        || key.contains("password")
        || key.contains("cookie")
        || key.contains("secret")
        || key.contains("authorization")
        || key.contains("apikey")
        || key == "key"
}

fn safe_scalar(key: &str, value: &Value) -> bool {
    match normalized(key).as_str() {
        "hasaccesstoken"
        | "hasrefreshtoken"
        | "hasbalancetoken"
        | "hastoken"
        | "hasapikey"
        | "hascookie"
        | "refreshtokenlonglived" => value.is_boolean(),
        "tokenexpiresat" | "tokenexpireat" | "accesstokenexpiresat" | "refreshtokenexpiresat" => {
            value.is_null() || value.as_i64().is_some_and(|value| value >= 0)
        }
        "tokentail" => {
            value.is_null() || value.as_str().is_some_and(|text| text.chars().count() <= 6)
        }
        _ => false,
    }
}

/// 校验公开视图不含凭据内容。
pub fn validate(public: &Value, secret: &Value) -> Result<(), WireError> {
    fn collect(value: &Value, protected: bool, out: &mut Vec<String>, depth: usize) {
        if depth > 32 {
            return;
        }
        match value {
            Value::Object(map) => {
                for (key, value) in map {
                    collect(value, protected || secret_key(key), out, depth + 1);
                }
            }
            Value::Array(items) => {
                for item in items {
                    collect(item, protected, out, depth + 1);
                }
            }
            Value::String(text) if protected && text.len() >= 8 => out.push(text.clone()),
            _ => {}
        }
    }
    fn check(value: &Value, secrets: &[String], depth: usize) -> bool {
        if depth > 16 {
            return false;
        }
        match value {
            Value::Object(map) => map.iter().all(|(key, value)| {
                (!secret_key(key) || safe_scalar(key, value)) && check(value, secrets, depth + 1)
            }),
            Value::Array(items) => items.iter().all(|item| check(item, secrets, depth + 1)),
            Value::String(text) => !secrets.iter().any(|secret| text.contains(secret)),
            _ => true,
        }
    }
    if !secret.is_object()
        || secret.to_string().len() > 256 * 1024
        || !public.is_object()
        || public.to_string().len() > 64 * 1024
    {
        return Err(super::wire::error(
            "插件公开视图或凭据格式无效、超过大小限制",
        ));
    }
    let mut secrets = Vec::new();
    collect(secret, false, &mut secrets, 0);
    if !check(public, &secrets, 0) {
        return Err(super::wire::error(
            "插件公开视图包含受保护内容或字段类型无效",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rejects_leaked_secret() {
        let secret = json!({ "accessToken": "supersecrettoken" });
        let public = json!({ "note": "prefix-supersecrettoken-suffix" });
        assert!(validate(&public, &secret).is_err());
    }

    #[test]
    fn accepts_safe_flags() {
        let secret = json!({ "accessToken": "supersecrettoken" });
        let public = json!({
            "hasAccessToken": true,
            "tokenTail": "123456",
            "tokenExpiresAt": 1700000000000i64,
            "displayName": "demo"
        });
        assert!(validate(&public, &secret).is_ok());
    }

    #[test]
    fn rejects_bad_token_tail_and_types() {
        let secret = json!({ "accessToken": "supersecrettoken" });
        assert!(validate(&json!({ "tokenTail": "1234567890" }), &secret).is_err());
        assert!(validate(&json!({ "hasToken": "yes" }), &secret).is_err());
        assert!(validate(&json!({ "tokenExpiresAt": -1 }), &secret).is_err());
    }

    #[test]
    fn rejects_non_object_shapes() {
        assert!(validate(&json!([]), &json!({})).is_err());
        assert!(validate(&json!({}), &json!("x")).is_err());
    }

    #[test]
    fn short_secrets_not_collected() {
        // 短于 8 字符的 secret 不参与包含检查（避免误伤常见短串）
        let secret = json!({ "apiKey": "abc" });
        assert!(validate(&json!({ "label": "abc" }), &secret).is_ok());
    }
}
