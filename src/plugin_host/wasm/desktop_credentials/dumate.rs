// Ported from Agent2API v3.5.0-beta.7 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//! 百度搭子（DuMate / qianfan-desktop-app）本机登录态读取器 —— 单次只读快照。
//!
//! ── 来源 ────────────────────────────────────────────────────────
//! 移植自上游 `plugin_host/wasm/desktop_credentials/dumate.rs`（v3.5.0-beta.7，
//! AGPL-3.0）。上游职责：读客户端 `auth.json`，用同目录 `.cookie-key` 做
//! AES-256-GCM 解封，把 Cookie 投影给插件。行为逐条对齐：
//!
//! 1. **固定路径**（常量，非参数）：`app_data()/qianfan-desktop-app/auth.json`
//!    与同目录 `.cookie-key`（必须正好 32 字节）。
//! 2. **账号档选择**：`accountProfiles` 至多 128 个；有 `activeProfileId`
//!    （非空、≤ 256 字节）时必须在列表里找到对应 `profileId`；没有 active 且
//!    只有一个 profile 时取它；多个 profile 又没有 active ⇒ 失败。
//! 3. **企业域拒收**：`activeProviderType == "enterprise"`、顶层或档位里出现非空
//!    `enterpriseApiBaseUrl`、或档位 `loginType == "enterprise"` ⇒ 明确报
//!    `desktop_credentials_unavailable`（企业登录域未接入），不静默降级。
//! 4. **密文布局**：base64(IV 12B ‖ Tag 16B ‖ Ciphertext)，明文是 Cookie 数组
//!    的 JSON；解封后按 `{"cookies": …}` 再包一次做严格解析。
//! 5. **Cookie 投影**：只保留 `baidu.com` / `dumate.cn` 及其子域的
//!    `BDUSS/BDUSS_BFESS/STOKEN/STOKEN_BFESS/PTOKEN/BAIDUID/BAIDUID_BFESS/
//!    bce-long-term-sessionid/bce-sessionid/bce-user-info/sec-token`；值必须非空、
//!    ≤ 16 KiB、不含控制字符，否则整体失败；过滤后为空 ⇒ 失败。
//! 6. **输出**：`{cookies, webOrigin:"https://console.bce.baidu.com"}`，有档位时
//!    附 `profile{bceUserId, bceAccountId, loginType, displayName}`，最后经
//!    [`super::content`] 包成 `{sourceId, sourceRevision, content}`。
//!
//! ── 只读边界 ────────────────────────────────────────────────────
//! - 路径全部是常量，**不接受任何调用参数**；只经父模块 [`super::file`] 做
//!   `stat` + `read`（符号链接 / reparse 点 / 超限一律拒收），不写、不删、不加锁。
//! - 不回传未筛选的原始 JSON，也不回传密钥或密文。
//!
//! ── 与上游的等价改写（AIGX 无对应 crate / 模块）───────────────────
//! - `zeroize::Zeroizing` → 普通 `Vec<u8>`：只在作用域内短命持有，行为不变
//!   （AIGX 不引入 zeroize 依赖，也不改 Cargo.toml）。
//! - `managed_task::declaration::parse_object` → 本文件内的 [`parse_object`]：
//!   同口径（字节上限、UTF-8、逐层拒重复键、容器深度 ≤ 32、必须顶层对象）。
//! - `aes_gcm::{AeadInOut, Nonce::try_from}` → `aes_gcm 0.10` 的 `Aead::decrypt`：
//!   同样把 `Ciphertext‖Tag` 作为一个缓冲、空 AAD 解封，密文/标签切分点不变。

use aes_gcm::aead::Aead as _;
use aes_gcm::{Aes256Gcm, KeyInit as _, Nonce};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde_json::{json, Value};

use crate::plugin_host::wasm::wire::WireError;

use super::{app_data, content, file, invalid, project};

/// 上游 `types::MAX_JSON_DEPTH`：JSON 容器（对象 / 数组）最大嵌套层数。
const MAX_JSON_DEPTH: usize = 32;

/// `desktop.credentials.read` 的搭子分支：读 auth.json → 选中账号档 → 解封 → 投影。
pub(super) fn read() -> Result<Value, WireError> {
    let root = app_data()?.join("qianfan-desktop-app");
    let path = root.join("auth.json");
    // auth.json 与 .cookie-key 都必须走父模块的常规文件校验（符号链接/上限）。
    let raw = file(&path, 512 * 1024)?;
    let auth = parse_object(&raw, 512 * 1024)?;
    let profile = match auth.get("accountProfiles") {
        Some(Value::Array(profiles)) if profiles.len() <= 128 => {
            let active = auth.get("activeProfileId").filter(|id| {
                id.as_str()
                    .is_some_and(|id| !id.is_empty() && id.len() <= 256)
            });
            match active {
                Some(active) => Some(
                    profiles
                        .iter()
                        .find(|profile| profile.get("profileId") == Some(active))
                        .ok_or_else(invalid)?,
                ),
                None if profiles.len() == 1 => profiles.first(),
                None => return Err(invalid()),
            }
        }
        None | Some(Value::Null) => None,
        _ => return Err(invalid()),
    };
    // 企业登录域（BCE 企业版）尚未接入：涉及就明确不可用，不拿个人域硬顶。
    if auth["activeProviderType"] == "enterprise"
        || auth
            .get("enterpriseApiBaseUrl")
            .is_some_and(|value| !value.is_null() && value != "")
        || profile.is_some_and(|profile| {
            profile["loginType"] == "enterprise"
                || profile
                    .get("enterpriseApiBaseUrl")
                    .is_some_and(|value| !value.is_null() && value != "")
        })
    {
        return Err(WireError::with_code(
            "百度搭子企业登录域尚未接入，请使用官方个人账号的网页登录或 Cookie",
            "desktop_credentials_unavailable",
        ));
    }
    let encoded = auth["cookies"]
        .as_str()
        .or_else(|| profile.and_then(|profile| profile["encryptedCookies"].as_str()))
        .filter(|encoded| !encoded.is_empty() && encoded.len() <= 384 * 1024)
        .ok_or_else(invalid)?;
    let key = file(&root.join(".cookie-key"), 32)?;
    if key.len() != 32 {
        return Err(invalid());
    }
    let encrypted = STANDARD.decode(encoded).map_err(|_| invalid())?;
    if encrypted.len() <= 28 || encrypted.len() > 256 * 1024 {
        return Err(invalid());
    }
    // IV 12B ‖ Tag 16B ‖ Ciphertext → 还原成 AEAD 需要的 Ciphertext ‖ Tag。
    let mut sealed = Vec::with_capacity(encrypted.len() - 12);
    sealed.extend_from_slice(&encrypted[28..]);
    sealed.extend_from_slice(&encrypted[12..28]);
    let plain = Aes256Gcm::new_from_slice(&key)
        .map_err(|_| invalid())?
        .decrypt(Nonce::from_slice(&encrypted[..12]), sealed.as_slice())
        .map_err(|_| invalid())?;
    // 复用严格对象解析器检查嵌套数组中的重复字段，不能在 Value 化后才核对。
    let mut wrapped = Vec::with_capacity(plain.len() + 16);
    wrapped.extend_from_slice(b"{\"cookies\":");
    wrapped.extend_from_slice(&plain);
    wrapped.push(b'}');
    let decoded = parse_object(&wrapped, 256 * 1024 + 16)?;
    let rows = decoded["cookies"]
        .as_array()
        .filter(|rows| !rows.is_empty() && rows.len() <= 256)
        .ok_or_else(invalid)?;
    let mut cookies = Vec::new();
    for row in rows {
        let domain = row["domain"]
            .as_str()
            .unwrap_or("")
            .trim_start_matches('.')
            .to_ascii_lowercase();
        if !(domain == "baidu.com"
            || domain.ends_with(".baidu.com")
            || domain == "dumate.cn"
            || domain.ends_with(".dumate.cn"))
        {
            continue;
        }
        let name = row["name"].as_str().unwrap_or("");
        if !matches!(
            name,
            "BDUSS"
                | "BDUSS_BFESS"
                | "STOKEN"
                | "STOKEN_BFESS"
                | "PTOKEN"
                | "BAIDUID"
                | "BAIDUID_BFESS"
                | "bce-long-term-sessionid"
                | "bce-sessionid"
                | "bce-user-info"
                | "sec-token"
        ) {
            continue;
        }
        // 命中白名单却值不合法：说明客户端状态被改坏，整体失败而不是丢掉这一条。
        if row["value"].as_str().is_none_or(|value| {
            value.is_empty() || value.len() > 16 * 1024 || value.chars().any(char::is_control)
        }) {
            return Err(invalid());
        }
        cookies.push(project(
            row,
            &[
                "name",
                "value",
                "domain",
                "path",
                "hostOnly",
                "secure",
                "httpOnly",
                "expirationDate",
                "expires",
            ],
        ));
    }
    if cookies.is_empty() {
        return Err(invalid());
    }
    let mut result = json!({"cookies":cookies, "webOrigin":"https://console.bce.baidu.com"});
    if let Some(profile) = profile {
        result["profile"] = project(
            profile,
            &["bceUserId", "bceAccountId", "loginType", "displayName"],
        );
    }
    content(&path, result)
}

/// 严格 JSON 形状检查（对标上游 `managed_task::strict_json::validate`）：
/// 只检查结构、不执行表达式；每一层对象都拒绝重复键（含转义后相同的键）。
struct Shape {
    // 标量不增加深度；根对象/数组算第一层，最多 32 层容器。
    containers: usize,
}

impl<'de> DeserializeSeed<'de> for Shape {
    type Value = ();

    fn deserialize<D: de::Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Shape {
    type Value = ();

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("bounded JSON with unique object keys")
    }

    fn visit_bool<E: de::Error>(self, _: bool) -> Result<(), E> {
        Ok(())
    }
    fn visit_i64<E: de::Error>(self, _: i64) -> Result<(), E> {
        Ok(())
    }
    fn visit_u64<E: de::Error>(self, _: u64) -> Result<(), E> {
        Ok(())
    }
    fn visit_f64<E: de::Error>(self, _: f64) -> Result<(), E> {
        Ok(())
    }
    fn visit_str<E: de::Error>(self, _: &str) -> Result<(), E> {
        Ok(())
    }
    fn visit_string<E: de::Error>(self, _: String) -> Result<(), E> {
        Ok(())
    }
    fn visit_unit<E: de::Error>(self) -> Result<(), E> {
        Ok(())
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<(), A::Error> {
        if self.containers >= MAX_JSON_DEPTH {
            return Err(de::Error::custom("JSON depth exceeded"));
        }
        while sequence
            .next_element_seed(Shape {
                containers: self.containers + 1,
            })?
            .is_some()
        {}
        Ok(())
    }

    fn visit_map<A: MapAccess<'de>>(self, mut object: A) -> Result<(), A::Error> {
        if self.containers >= MAX_JSON_DEPTH {
            return Err(de::Error::custom("JSON depth exceeded"));
        }
        let mut keys = std::collections::BTreeSet::new();
        while let Some(key) = object.next_key::<String>()? {
            if !keys.insert(key) {
                return Err(de::Error::custom("duplicate JSON field"));
            }
            object.next_value_seed(Shape {
                containers: self.containers + 1,
            })?;
        }
        Ok(())
    }
}

/// 只要顶层对象的严格 JSON 解析：上游
/// `managed_task::declaration::parse_object::<Value>(bytes, limit)` 的同口径替换
/// （AIGX 的 `managed_task` 没有 declaration 子模块）。
fn parse_object(bytes: &[u8], limit: usize) -> Result<Value, WireError> {
    if bytes.len() > limit || std::str::from_utf8(bytes).is_err() {
        return Err(invalid());
    }
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    Shape { containers: 0 }
        .deserialize(&mut deserializer)
        .map_err(|_| invalid())?;
    deserializer.end().map_err(|_| invalid())?;
    let value: Value = serde_json::from_slice(bytes).map_err(|_| invalid())?;
    if !value.is_object() {
        return Err(invalid());
    }
    Ok(value)
}
