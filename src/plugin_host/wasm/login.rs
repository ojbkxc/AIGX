// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//! 交互式登录的声明与校验 —— 字段表单 + 极验验证码。
//!
//! 与上游逐字一致：交互声明 `id`/`title`/`message`/`fields`/`captcha` 全部
//! 有长度上限；字段名限 `[A-Za-z0-9_-]` 且禁止原型污染名；`select` 必须
//! 有候选项；验证码只收 geetest 三件套，且不得与输入字段重名。

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::manifest::WasmManifest;
use super::wire::WireError;

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Prompt {
    pub key: String,
    pub label: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub secret: bool,
    #[serde(default)]
    pub optional: bool,
    #[serde(default)]
    pub options: Vec<OptionItem>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OptionItem {
    pub value: String,
    pub label: String,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Interaction {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub fields: Vec<Prompt>,
    #[serde(default)]
    pub captcha: Option<Geetest>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Geetest {
    pub gt: String,
    pub challenge: String,
    pub success: bool,
    pub new_captcha: bool,
}

#[derive(Default)]
pub(crate) struct Interactions {
    pending: Mutex<HashMap<String, Interaction>>,
}

impl Interactions {
    pub fn remove(&self, flow: &str) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.remove(flow);
        }
    }

    /// 记住插件声明的交互步骤（非法声明直接报错，不污染状态）。
    pub fn remember(
        &self,
        flow: &str,
        value: &Value,
        manifest: &WasmManifest,
    ) -> Result<(), WireError> {
        let Some(value) = value.get("interaction").filter(|value| !value.is_null()) else {
            if value.get("interaction").is_some()
                || value["state"] == "done"
                || value["state"] == "failed"
            {
                self.remove(flow);
            }
            return Ok(());
        };
        if !manifest.allows("login.interaction.v1") {
            return Err(super::wire::error("插件未声明交互式登录能力"));
        }
        let interaction: Interaction =
            serde_json::from_value(value.clone()).map_err(|_| super::errors::protocol())?;
        if interaction.id.is_empty()
            || interaction.id.len() > 128
            || interaction.id.contains('\0')
            || interaction.title.len() > 256
            || interaction.message.len() > 8192
            || interaction.fields.len() > 16
        {
            return Err(super::wire::error("登录交互声明超过限制"));
        }
        let mut keys = std::collections::HashSet::new();
        for field in &interaction.fields {
            if field.key.is_empty()
                || field.key.len() > 64
                || !field
                    .key
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
                || matches!(
                    field.key.as_str(),
                    "__proto__" | "constructor" | "prototype"
                )
                || !keys.insert(&field.key)
                || !matches!(field.kind.as_str(), "text" | "select")
                || field.label.len() > 256
                || field.options.len() > 64
                || field.kind == "select" && field.options.is_empty()
                || field
                    .options
                    .iter()
                    .any(|item| item.value.len() > 1024 || item.label.len() > 256)
            {
                return Err(super::wire::error("登录交互字段无效"));
            }
        }
        if let Some(captcha) = &interaction.captcha {
            if !manifest.allows("login.captcha.geetest.v1")
                || captcha.gt.is_empty()
                || captcha.gt.len() > 128
                || captcha.challenge.is_empty()
                || captcha.challenge.len() > 256
            {
                return Err(super::wire::error("登录验证码声明无效"));
            }
        }
        if interaction.captcha.is_some()
            && !interaction
                .fields
                .iter()
                .all(|field| field.key != "captcha")
        {
            return Err(super::wire::error("登录输入字段不能与验证码结果重名"));
        }
        let mut pending = self
            .pending
            .lock()
            .map_err(|_| super::wire::error("登录交互不可用"))?;
        if pending.len() >= 64 && !pending.contains_key(flow) {
            return Err(super::wire::error("登录交互数量超过上限"));
        }
        pending.insert(flow.into(), interaction);
        Ok(())
    }

    /// 用户提交后清除该步骤。
    pub fn accepted(&self, flow: &str, value: &Value) {
        if let Ok(mut pending) = self.pending.lock() {
            if pending.get(flow).is_some_and(|interaction| {
                value["interaction"].as_str() == Some(interaction.id.as_str())
            }) {
                pending.remove(flow);
            }
        }
    }

    pub fn snapshot(&self, flow: &str) -> Option<Value> {
        self.pending
            .lock()
            .ok()?
            .get(flow)
            .and_then(|interaction| serde_json::to_value(interaction).ok())
    }

    /// 校验用户提交的输入满足当前步骤声明。
    pub fn check(&self, flow: &str, params: &Value) -> Result<(), WireError> {
        let guard = self
            .pending
            .lock()
            .map_err(|_| super::wire::error("登录交互不可用"))?;
        let interaction = guard
            .get(flow)
            .ok_or_else(|| super::wire::error("登录交互已过期"))?;
        if params["interaction"].as_str() != Some(interaction.id.as_str()) {
            return Err(super::wire::error("登录步骤已变化，请重新读取"));
        }
        let inputs: BTreeMap<String, String> = serde_json::from_value(params["inputs"].clone())
            .map_err(|_| super::wire::error("登录输入必须是文本字段"))?;
        if inputs.iter().any(|(key, value)| {
            value.len() > 8192 || !interaction.fields.iter().any(|field| &field.key == key)
        }) {
            return Err(super::wire::error("登录输入包含未申报字段或超过限制"));
        }
        for field in &interaction.fields {
            let value = inputs.get(&field.key).map(String::as_str).unwrap_or("");
            if !field.optional && value.trim().is_empty()
                || field.kind == "select"
                    && !value.is_empty()
                    && !field.options.iter().any(|option| option.value == value)
            {
                return Err(super::wire::error("请完整填写当前登录步骤"));
            }
        }
        if let Some(captcha) = &interaction.captcha {
            let reply = &params["captcha"];
            if reply["geetest_challenge"].as_str().is_none_or(|value| {
                value.is_empty() || value.len() > 512 || !value.starts_with(&captcha.challenge)
            }) || ["geetest_validate", "geetest_seccode"].iter().any(|key| {
                reply[*key]
                    .as_str()
                    .is_none_or(|value| value.is_empty() || value.len() > 1024)
            }) {
                return Err(super::wire::error("请完成当前步骤的人机验证"));
            }
        } else if params.get("captcha").is_some_and(|value| !value.is_null()) {
            return Err(super::wire::error("当前步骤不接受验证码结果"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn manifest(requires: &[&str]) -> WasmManifest {
        WasmManifest {
            provider: "p".into(),
            abi: 1,
            requires: requires.iter().map(|s| s.to_string()).collect(),
            origins: vec![],
            helpers: vec![],
            memory_mib: 64,
            concurrency: 8,
            verified: false,
            account_origin: None,
            desktop_launch: None,
            managed_login: None,
            managed_task: None,
            concurrency_hard_limit: None,
        }
    }

    fn form() -> Value {
        json!({
            "id": "step1",
            "title": "登录",
            "fields": [
                { "key": "username", "label": "账号", "type": "text" },
                { "key": "mode", "label": "方式", "type": "select",
                  "options": [{ "value": "a", "label": "A" }] }
            ]
        })
    }

    #[test]
    fn remembers_valid_interaction() {
        let interactions = Interactions::default();
        let manifest = manifest(&["login.interaction.v1"]);
        interactions
            .remember("f1", &json!({ "interaction": form() }), &manifest)
            .unwrap();
        assert!(interactions.snapshot("f1").is_some());
    }

    #[test]
    fn requires_capability() {
        let interactions = Interactions::default();
        let manifest = manifest(&[]);
        assert!(interactions
            .remember("f1", &json!({ "interaction": form() }), &manifest)
            .is_err());
    }

    #[test]
    fn rejects_prototype_pollution_and_duplicate_keys() {
        let interactions = Interactions::default();
        let manifest = manifest(&["login.interaction.v1"]);
        let bad = json!({ "interaction": { "id": "s", "title": "t",
            "fields": [{ "key": "__proto__", "label": "x", "type": "text" }] } });
        assert!(interactions.remember("f1", &bad, &manifest).is_err());
        let dup = json!({ "interaction": { "id": "s", "title": "t",
            "fields": [
                { "key": "a", "label": "x", "type": "text" },
                { "key": "a", "label": "y", "type": "text" }
            ] } });
        assert!(interactions.remember("f1", &dup, &manifest).is_err());
    }

    #[test]
    fn checks_required_and_select_values() {
        let interactions = Interactions::default();
        let manifest = manifest(&["login.interaction.v1"]);
        interactions
            .remember("f1", &json!({ "interaction": form() }), &manifest)
            .unwrap();
        // 缺必填
        assert!(interactions
            .check("f1", &json!({ "interaction": "step1", "inputs": {} }))
            .is_err());
        // 未申报字段
        assert!(interactions
            .check(
                "f1",
                &json!({ "interaction": "step1", "inputs": { "username": "u", "x": "1" } })
            )
            .is_err());
        // 正常
        assert!(interactions
            .check(
                "f1",
                &json!({ "interaction": "step1", "inputs": { "username": "u", "mode": "a" } })
            )
            .is_ok());
        // select 非法值
        assert!(interactions
            .check(
                "f1",
                &json!({ "interaction": "step1", "inputs": { "username": "u", "mode": "b" } })
            )
            .is_err());
    }

    #[test]
    fn captcha_rules() {
        let interactions = Interactions::default();
        let manifest = manifest(&["login.interaction.v1", "login.captcha.geetest.v1"]);
        let with_captcha = json!({ "interaction": { "id": "s", "title": "t",
            "fields": [{ "key": "user", "label": "u", "type": "text" }],
            "captcha": { "gt": "g", "challenge": "chal-", "success": true, "new_captcha": true } } });
        interactions
            .remember(
                "f1",
                &json!({ "interaction": with_captcha["interaction"] }),
                &manifest,
            )
            .unwrap();
        // 缺验证码
        assert!(interactions
            .check(
                "f1",
                &json!({ "interaction": "s", "inputs": { "user": "u" } })
            )
            .is_err());
        // 验证码 challenge 前缀不符
        assert!(interactions
            .check(
                "f1",
                &json!({ "interaction": "s", "inputs": { "user": "u" },
                "captcha": { "geetest_challenge": "other", "geetest_validate": "v",
                             "geetest_seccode": "s" } })
            )
            .is_err());
        // 正常
        assert!(interactions
            .check(
                "f1",
                &json!({ "interaction": "s", "inputs": { "user": "u" },
                "captcha": { "geetest_challenge": "chal-abc", "geetest_validate": "v",
                             "geetest_seccode": "s" } })
            )
            .is_ok());
    }

    #[test]
    fn removes_on_done_state() {
        let interactions = Interactions::default();
        let manifest = manifest(&["login.interaction.v1"]);
        interactions
            .remember("f1", &json!({ "interaction": form() }), &manifest)
            .unwrap();
        interactions
            .remember("f1", &json!({ "state": "done" }), &manifest)
            .unwrap();
        assert!(interactions.snapshot("f1").is_none());
    }
}
