use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Webhook 签名时间戳的容忍窗口（秒）。与 Stripe 官方库默认值一致：300s。
/// 超出（无论过去还是未来）一律视为重放/伪造。
const WEBHOOK_TOLERANCE_SECS: u64 = 300;

/// Stripe 配置
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StripeConfig {
    /// Stripe Secret Key (sk_live_... or sk_test_...)
    #[serde(default)]
    pub secret_key: String,
    /// Stripe Webhook Signing Secret (whsec_...)
    #[serde(default)]
    pub webhook_secret: String,
    /// 成功支付后的返回 URL
    #[serde(default)]
    pub success_url: String,
    /// 取消支付后的返回 URL
    #[serde(default)]
    pub cancel_url: String,
}

impl StripeConfig {
    /// `trim()` 判空：手写配置里只有空格的占位值不应被当作已配置。
    pub fn ready(&self) -> bool {
        !self.secret_key.trim().is_empty()
    }

    /// Webhook 回调是否可安全入账。
    ///
    /// P0 修复：`ready()` 只校验 secret_key——漏配 webhook_secret 时
    /// `verify_webhook` 用空 key 计算 HMAC，攻击者可自签
    /// `checkout.session.completed` 回调免费入账配额。
    /// 入账路径必须同时要求 whsec 非空。
    pub fn webhook_ready(&self) -> bool {
        !self.secret_key.trim().is_empty() && !self.webhook_secret.trim().is_empty()
    }
}

/// Stripe 支付客户端
pub struct StripeClient {
    config: StripeConfig,
    client: reqwest::Client,
}

/// 创建 Checkout Session 的请求参数
pub struct CheckoutParams {
    pub trade_no: String,
    pub user_id: String,
    pub amount_cents: i64,
    pub quota: i64,
    pub success_url: String,
    pub cancel_url: String,
}

/// Stripe Checkout Session 创建响应
#[derive(Debug, Deserialize)]
pub struct CheckoutSession {
    pub id: String,
    pub url: String,
}

impl StripeClient {
    pub fn new(config: StripeConfig) -> Self {
        static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
        let client = CLIENT
            .get_or_init(|| {
                reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(15))
                    .build()
                    .unwrap_or_default()
            })
            .clone();
        Self { config, client }
    }

    pub fn config(&self) -> &StripeConfig {
        &self.config
    }

    /// 创建 Stripe Checkout Session
    ///
    /// POST https://api.stripe.com/v1/checkout/sessions
    /// 参考 new-api controller/topup_stripe.go
    pub async fn create_checkout_session(&self, params: CheckoutParams) -> Result<CheckoutSession> {
        let url = "https://api.stripe.com/v1/checkout/sessions";
        let mut form = BTreeMap::new();
        form.insert("mode", "payment".to_string());
        form.insert("success_url", params.success_url);
        form.insert("cancel_url", params.cancel_url);
        form.insert("line_items[0][quantity]", "1".to_string());
        form.insert("line_items[0][price_data][currency]", "usd".to_string());
        form.insert(
            "line_items[0][price_data][unit_amount]",
            params.amount_cents.to_string(),
        );
        form.insert(
            "line_items[0][price_data][product_data][name]",
            format!("AIGX Quota Top-up (Order {})", params.trade_no),
        );
        form.insert("client_reference_id", params.trade_no.clone());
        form.insert("metadata[trade_no]", params.trade_no.clone());
        form.insert("metadata[user_id]", params.user_id);
        form.insert("metadata[quota]", params.quota.to_string());

        let resp = self
            .client
            .post(url)
            .bearer_auth(&self.config.secret_key)
            .form(&form)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Stripe request failed: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!("Stripe API error {status}: {body}"));
        }

        let session: CheckoutSession = resp
            .json()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to parse Stripe response: {e}"))?;

        Ok(session)
    }

    /// 验证 Stripe Webhook 签名并提取事件
    ///
    /// Stripe 使用 HMAC-SHA256 签名，签名头格式: t=...,v1=...
    /// 参考 new-api controller/topup_stripe.go 的 VerifySignature
    pub fn verify_webhook(&self, payload: &[u8], sig_header: &str) -> Result<StripeEvent> {
        let mut timestamp: Option<&str> = None;
        let mut signatures: Vec<&str> = Vec::new();

        for part in sig_header.split(',') {
            let part = part.trim();
            if let Some(rest) = part.strip_prefix("t=") {
                timestamp = Some(rest);
            } else if let Some(rest) = part.strip_prefix("v1=") {
                signatures.push(rest);
            }
        }

        let ts = timestamp.ok_or_else(|| anyhow::anyhow!("missing t= in stripe signature"))?;
        // 防重放：签名头里的 `t=` 是签名时间。没有这道闸，任何被抓到的合法回调
        // 都能无限重放（幂等键只挡重复入账，挡不住反复打进无效请求）。
        // 容忍窗口与 Stripe 官方库一致：300s，过去/未来两个方向都算。
        let ts_secs: i64 = ts
            .parse()
            .map_err(|_| anyhow::anyhow!("invalid t= in stripe signature"))?;
        let skew = chrono::Utc::now().timestamp().saturating_sub(ts_secs);
        if skew.unsigned_abs() > WEBHOOK_TOLERANCE_SECS as u64 {
            return Err(anyhow::anyhow!(
                "stripe webhook timestamp outside tolerance ({ts_secs})"
            ));
        }
        let expected_input = format!("{}.{}", ts, String::from_utf8_lossy(payload));
        let expected = hmac_sha256_hex(
            self.config.webhook_secret.as_bytes(),
            expected_input.as_bytes(),
        );

        let valid = signatures.iter().any(|s| {
            // timing-safe comparison
            if s.len() != expected.len() {
                return false;
            }
            s.as_bytes()
                .iter()
                .zip(expected.as_bytes().iter())
                .fold(0u8, |acc, (a, b)| acc | (a ^ b))
                == 0
        });

        if !valid {
            return Err(anyhow::anyhow!("invalid stripe webhook signature"));
        }

        let event: StripeEvent = serde_json::from_slice(payload)
            .map_err(|e| anyhow::anyhow!("failed to parse stripe event: {e}"))?;

        Ok(event)
    }
}

/// Stripe Webhook 事件
#[derive(Debug, Deserialize)]
pub struct StripeEvent {
    #[serde(rename = "type")]
    pub event_type: String,
    pub data: StripeEventData,
}

#[derive(Debug, Deserialize)]
pub struct StripeEventData {
    pub object: StripeObject,
}

#[derive(Debug, Deserialize)]
pub struct StripeObject {
    /// checkout.session 类型有 client_reference_id
    #[serde(default)]
    pub client_reference_id: Option<String>,
    /// 支付状态
    #[serde(default)]
    pub payment_status: Option<String>,
    /// metadata
    #[serde(default)]
    pub metadata: serde_json::Value,
}

impl StripeObject {
    /// 从 metadata 或 client_reference_id 提取 trade_no
    pub fn trade_no(&self) -> Option<String> {
        if let Some(ref id) = self.client_reference_id {
            return Some(id.clone());
        }
        self.metadata
            .get("trade_no")
            .and_then(|v| v.as_str())
            .map(String::from)
    }

    #[allow(dead_code)] // 预留/线表符号（见 main.rs 债务清单）
    pub fn user_id(&self) -> Option<String> {
        self.metadata
            .get("user_id")
            .and_then(|v| v.as_str())
            .map(String::from)
    }

    #[allow(dead_code)] // 预留/未接线（见 main.rs 债务清单）
    pub fn quota(&self) -> Option<i64> {
        self.metadata
            .get("quota")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok())
    }

    pub fn is_paid(&self) -> bool {
        self.payment_status.as_deref() == Some("paid")
    }
}

fn hmac_sha256_hex(key: &[u8], msg: &[u8]) -> String {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC key");
    mac.update(msg);
    let bytes = mac.finalize().into_bytes();
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "whsec_test";
    const EVENT: &str = r#"{"type":"checkout.session.completed","data":{"object":{"client_reference_id":"u1","payment_status":"paid","metadata":{}}}}"#;

    fn client() -> StripeClient {
        StripeClient {
            config: StripeConfig {
                secret_key: "sk_test".to_string(),
                webhook_secret: SECRET.to_string(),
                ..Default::default()
            },
            client: reqwest::Client::new(),
        }
    }

    fn signed_header(payload: &str, ts: i64) -> String {
        let sig = hmac_sha256_hex(SECRET.as_bytes(), format!("{ts}.{payload}").as_bytes());
        format!("t={ts},v1={sig}")
    }

    /// 配置就绪判定：漏配 webhook_secret 时不能入账（否则空 key 可被自签）。
    #[test]
    fn webhook_ready_requires_signing_secret() {
        let mut c = StripeConfig::default();
        assert!(!c.webhook_ready());
        c.secret_key = "sk".into();
        assert!(!c.webhook_ready());
        c.webhook_secret = "whsec".into();
        assert!(c.webhook_ready());
        // 只有空格的占位值不算配置
        c.webhook_secret = "   ".into();
        assert!(!c.webhook_ready());
    }

    #[test]
    fn accepts_freshly_signed_event() {
        let now = chrono::Utc::now().timestamp();
        let ev = client()
            .verify_webhook(EVENT.as_bytes(), &signed_header(EVENT, now))
            .expect("fresh signature must pass");
        assert_eq!(ev.event_type, "checkout.session.completed");
    }

    /// 防重放：过期或超前的时间戳一律拒绝（300s 窗口）。
    #[test]
    fn rejects_timestamp_outside_tolerance() {
        let now = chrono::Utc::now().timestamp();
        let old = now - WEBHOOK_TOLERANCE_SECS as i64 - 60;
        assert!(client()
            .verify_webhook(EVENT.as_bytes(), &signed_header(EVENT, old))
            .is_err());
        let future = now + WEBHOOK_TOLERANCE_SECS as i64 + 60;
        assert!(client()
            .verify_webhook(EVENT.as_bytes(), &signed_header(EVENT, future))
            .is_err());
        // 窗口内仍然接受（取 60s，留足执行抖动余量，避免边界秒导致偶发失败）
        let recent = now - 60;
        assert!(client()
            .verify_webhook(EVENT.as_bytes(), &signed_header(EVENT, recent))
            .is_ok());
    }

    #[test]
    fn rejects_tampered_payload_and_bad_signature() {
        let now = chrono::Utc::now().timestamp();
        let header = signed_header(EVENT, now);
        let c = client();
        // 改了 body，签名不再匹配
        let tampered = EVENT.replace("paid", "unpaid");
        assert!(c.verify_webhook(tampered.as_bytes(), &header).is_err());
        // 签名本身不对
        assert!(c
            .verify_webhook(EVENT.as_bytes(), &format!("t={now},v1=deadbeef"))
            .is_err());
        // 缺 t=（无法校验时效，直接拒绝）
        assert!(c.verify_webhook(EVENT.as_bytes(), "v1=deadbeef").is_err());
        // t= 不是数字
        assert!(c
            .verify_webhook(EVENT.as_bytes(), "t=abc,v1=deadbeef")
            .is_err());
    }

    #[test]
    fn accepts_multiple_v1_signatures() {
        let now = chrono::Utc::now().timestamp();
        // Stripe 在轮换密钥时会同时给多个 v1=，命中任一即通过
        let good = signed_header(EVENT, now);
        let sig = good.split(',').nth(1).unwrap();
        let header = format!("t={now},v1=deadbeef,{sig}");
        assert!(client().verify_webhook(EVENT.as_bytes(), &header).is_ok());
    }
}
