//! ALTCHA 机器人校验（proof-of-work，协议见 <https://altcha.org>；
//! 对齐 Agent2API `server::altcha`：登录 / 注册前必须先解一道题，服务端验签
//! + 防重放）。
//!
//! ── 它防什么 ────────────────────────────────────────────────
//! `/api/auth/login` 与 `/api/auth/register` 是**公开的认证边界**，脚本可以
//! 无限打：暴破密码、抢注管理员。失败锁定（`SecurityPolicyConfig.fail_threshold`）
//! 拦的是「同一来源高频失败」，换 IP 就绕开了。ALTCHA 补的是另一维：**每个
//! 请求必须先花一次算力** —— 登录页向网关领题、浏览器后台把答案算出来一起
//! 提交；脚本每试一个密码都要先领题再解一道哈希枚举，批量攻击从零成本变成
//! 逐请求成本。对真人完全无感：题目在页面加载时就算完了。
//!
//! ── 协议（SHA-256 模式）─────────────────────────────────────
//! 网关签发 challenge（JSON）：
//!   `algorithm = "SHA-256"`、`salt`（随机 hex）、
//!   `challenge = sha256(salt + number)` 的 hex（number 不下发，客户端枚举）、
//!   `maxnumber`（枚举上限）、
//!   `signature = HMAC-SHA256(secret, challenge字段)` —— 防伪造 / 篡改。
//! 客户端提交 payload（base64 的 JSON）：`{algorithm, challenge, number, salt,
//! signature}`。
//! 验证 = 签名一致 + `sha256(salt+number) == challenge` + **台账在案**
//! （这台服务器签发过的才有效，伪造的题直接拒）+ 未过期 + **用后即焚**
//! （同一个 payload 不能换第二个密码，防重放）。
//!
//! ── 密钥与状态 ──────────────────────────────────────────────
//! secret 首次使用时生成（32 字节 hex）落 `auth.altcha_secret`，此后常驻；
//! 只经 HMAC 使用，从不外发。台账（challenge → 过期时刻）在内存：进程重启
//! 清空，重启窗口内已下发未用的 challenge 重放一次的风险由 10 分钟过期兜底
//! —— 登录页拿到题到提交之间通常只有几秒。

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::api::openai::AppState;

/// 密钥在 KV 里的存储键。
const KV_SECRET_KEY: &str = "auth.altcha_secret";
/// Turnstile secret 在 KV 里的存储键（与 ALTCHA 密钥同库、不落 config.toml）。
const KV_TURNSTILE_SECRET: &str = "auth.turnstile_secret";
/// Cloudflare Turnstile 校验地址。
const TURNSTILE_SITEVERIFY: &str = "https://challenges.cloudflare.com/turnstile/v0/siteverify";
/// challenge 有效期：登录页加载到提交的合理上限（对齐上游同款 10 分钟）。
const CHALLENGE_TTL_MS: i64 = 10 * 60 * 1000;
/// 枚举上限量级：浏览器分批枚举 5–10 万次 SHA-256 在百毫秒到一秒级，对
/// 「每请求都要解一道」的脚本已是持续成本；再大只拖慢真人。
const MAXNUMBER_MIN: u32 = 50_000;
const MAXNUMBER_MAX: u32 = 100_000;
/// 台账容量上限：忘清理也只占这点内存（每条 ~几十字节）。
const STORE_CAP: usize = 4096;

/// challenge → 过期时刻（毫秒）。键就是 challenge 的 hex —— 台账只认
/// 「本服务器签发过的题」，客户端自造的题不在案，直接拒。
static STORE: OnceLock<Mutex<HashMap<String, i64>>> = OnceLock::new();

fn store() -> &'static Mutex<HashMap<String, i64>> {
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn sha256_hex(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn hmac_hex(secret: &str, message: &str) -> String {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC 接受任意长度的密钥");
    mac.update(message.as_bytes());
    format!("{:x}", mac.finalize().into_bytes())
}

/// 随机 hex 串（`bytes` 个字节 → 2×bytes 个 hex 字符）。
fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    getrandom::getrandom(&mut buf).expect("系统随机源不可用");
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// 随机一个 `[min, max]` 区间的 u32。
fn random_range(min: u32, max: u32) -> u32 {
    let span = max - min + 1;
    let mut buf = [0u8; 4];
    getrandom::getrandom(&mut buf).expect("系统随机源不可用");
    min + (u32::from_le_bytes(buf) % span)
}

/// HMAC 签名密钥：KV 里没有就现场生成并落盘（幂等；KV 不可用时退化为
/// 进程内随机 —— 校验仍可用，只是重启后旧题全部失效，可接受）。
fn secret(state: &AppState) -> String {
    let existing = state
        .alert_store
        .get::<String>(KV_SECRET_KEY)
        .ok()
        .flatten()
        .filter(|value| !value.is_empty());
    if let Some(existing) = existing {
        return existing;
    }
    let generated = random_hex(32);
    let _ = state.alert_store.put(KV_SECRET_KEY, &generated);
    generated
}

/// 机器人校验是否启用（设置页开关，默认开）。
pub fn enabled() -> bool {
    crate::config::security_captcha_enabled()
}

/// 当前**生效**的校验提供方。
///
/// `turnstile` 只有在 site key 与 secret 都已配置时才真正启用；缺任一项一律
/// 回落 `altcha`（自托管），避免「开关打开但配置不全」把登录锁死 —— 前端
/// challenge 形状与后端 verify 分派必须看同一个生效值。
pub fn effective_provider(state: &AppState) -> &'static str {
    if crate::config::security_captcha_provider() == "turnstile"
        && !crate::config::security_turnstile_site_key()
            .trim()
            .is_empty()
        && turnstile_secret_set(state)
    {
        "turnstile"
    } else {
        "altcha"
    }
}

/// 签发一道 challenge（登录页的 `GET /api/auth/captcha` 用）。
/// 开关关闭时返回 `None`（端点回 400，前端跳过校验）。
///
/// 按生效提供方返回不同形状：
/// - `altcha`：proof-of-work 题面（`algorithm/challenge/maxnumber/salt/signature`）；
/// - `turnstile`：`{provider, siteKey}`（前端据此加载 Cloudflare 组件）。
///
/// 两种都带 `provider` 字段，前端只认它，不猜形状。
pub fn challenge(state: &AppState) -> Option<Value> {
    if !enabled() {
        return None;
    }
    if effective_provider(state) == "turnstile" {
        let site_key = crate::config::security_turnstile_site_key();
        return Some(json!({
            "provider": "turnstile",
            "siteKey": site_key,
        }));
    }
    let salt = random_hex(12);
    let maxnumber = random_range(MAXNUMBER_MIN, MAXNUMBER_MAX);
    let number = random_range(0, maxnumber);
    let challenge_hex = sha256_hex(&format!("{salt}{number}"));
    let signature = hmac_hex(&secret(state), &challenge_hex);

    {
        let mut table = match store().lock() {
            Ok(table) => table,
            Err(poisoned) => poisoned.into_inner(),
        };
        let now = now_ms();
        // 顺手清过期；超容量时整体清空（容量只是防御性兜底，非常规淘汰路径）
        table.retain(|_, expires| *expires > now);
        if table.len() >= STORE_CAP {
            table.clear();
        }
        table.insert(challenge_hex.clone(), now + CHALLENGE_TTL_MS);
    }

    Some(json!({
        "provider": "altcha",
        "algorithm": "SHA-256",
        "challenge": challenge_hex,
        "maxnumber": maxnumber,
        "salt": salt,
        "signature": signature,
    }))
}

/// Turnstile secret 是否已配置（设置页只回「是否已设置」，不回明文）。
pub fn turnstile_secret_set(state: &AppState) -> bool {
    state
        .alert_store
        .get::<String>(KV_TURNSTILE_SECRET)
        .ok()
        .flatten()
        .is_some_and(|secret| !secret.trim().is_empty())
}

/// 设置 / 清除 Turnstile secret（空串 = 清除；存 KV，不写 config.toml）。
pub fn set_turnstile_secret(state: &AppState, secret: &str) {
    let secret = secret.trim();
    if secret.is_empty() {
        if let Err(error) = state.alert_store.delete(KV_TURNSTILE_SECRET) {
            tracing::warn!("clear turnstile secret failed: {error}");
        }
    } else if let Err(error) = state
        .alert_store
        .put(KV_TURNSTILE_SECRET, &secret.to_string())
    {
        tracing::warn!("persist turnstile secret failed: {error}");
    }
}

/// 验证登录 / 注册请求携带的机器人校验 payload。
/// 失败返回给用户看的原因（端点原样转 400）。开关关闭时直接放行。
///
/// 按生效提供方分派：`altcha` 本地验签（同步），`turnstile` 调 Cloudflare
/// siteverify（异步，5s 超时）。配置不全时 `effective_provider` 已回落
/// altcha，前后端不会各走一半。
pub async fn verify(state: &AppState, token: &str) -> Result<(), String> {
    if !enabled() {
        return Ok(());
    }
    if effective_provider(state) == "turnstile" {
        return verify_turnstile(state, token).await;
    }
    verify_altcha(state, token)
}

/// Cloudflare Turnstile：token 由前端组件产出，服务端调 siteverify 校验。
async fn verify_turnstile(state: &AppState, token: &str) -> Result<(), String> {
    let token = token.trim();
    if token.is_empty() {
        return Err("请先完成机器人校验".to_string());
    }
    let secret = state
        .alert_store
        .get::<String>(KV_TURNSTILE_SECRET)
        .ok()
        .flatten()
        .unwrap_or_default();
    if secret.trim().is_empty() {
        return Err("Turnstile 未配置 secret key，请管理员先完成配置".to_string());
    }
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    let client = CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new())
    });
    let response = client
        .post(TURNSTILE_SITEVERIFY)
        .form(&[("secret", secret.trim()), ("response", token)])
        .send()
        .await
        .map_err(|error| {
            tracing::warn!("turnstile siteverify request failed: {error}");
            "机器人校验服务不可用，请稍后重试".to_string()
        })?;
    let body: Value = response.json().await.map_err(|error| {
        tracing::warn!("turnstile siteverify bad response: {error}");
        "机器人校验服务返回异常，请稍后重试".to_string()
    })?;
    if body.get("success").and_then(Value::as_bool) == Some(true) {
        return Ok(());
    }
    tracing::warn!(
        "turnstile rejected token: {}",
        body.get("error-codes")
            .cloned()
            .unwrap_or(serde_json::Value::Null)
    );
    Err("机器人校验未通过，请刷新页面重试".to_string())
}

/// ALTCHA 本地验签 + 台账校验（原 `verify` 主体）。
fn verify_altcha(state: &AppState, token: &str) -> Result<(), String> {
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(token.trim())
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(token.trim()))
        .map_err(|_| "机器人校验无效，请刷新页面重试".to_string())?;
    let parsed: Value = serde_json::from_slice(&decoded)
        .map_err(|_| "机器人校验无效，请刷新页面重试".to_string())?;

    let get = |field: &str| parsed.get(field).and_then(Value::as_str).unwrap_or("");
    let algorithm = get("algorithm");
    let challenge_field = get("challenge");
    let salt = get("salt");
    let signature = get("signature");
    let number = parsed.get("number").and_then(Value::as_u64);
    if algorithm != "SHA-256" || challenge_field.is_empty() || salt.is_empty() || number.is_none() {
        return Err("机器人校验无效，请刷新页面重试".to_string());
    }
    let number = number.unwrap();

    // 签名先验（防伪造 challenge），再验答案本身
    if !constant_time_eq(
        signature.as_bytes(),
        hmac_hex(&secret(state), challenge_field).as_bytes(),
    ) {
        return Err("机器人校验无效，请刷新页面重试".to_string());
    }
    if !constant_time_eq(
        challenge_field.as_bytes(),
        sha256_hex(&format!("{salt}{number}")).as_bytes(),
    ) {
        return Err("机器人校验无效，请刷新页面重试".to_string());
    }

    // 台账：在案（本服务器签发）+ 未过期 + 用后即焚（防重放）
    let mut table = match store().lock() {
        Ok(table) => table,
        Err(poisoned) => poisoned.into_inner(),
    };
    match table.remove(challenge_field) {
        Some(expires) if expires > now_ms() => Ok(()),
        _ => Err("机器人校验已过期，请刷新页面重试".to_string()),
    }
}

/// 常数时间比较（避免签名比对被计时侧信道推断）。
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_hex_matches_known_vector() {
        // 空串的 SHA-256 是公开测试向量
        assert_eq!(
            sha256_hex(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn hmac_hex_is_deterministic_and_keyed() {
        let a = hmac_hex("secret", "msg");
        assert_eq!(a, hmac_hex("secret", "msg"));
        assert_ne!(a, hmac_hex("secret2", "msg"));
    }

    #[test]
    fn constant_time_eq_behaves_like_eq() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
    }
}
