use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;


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
    pub fn ready(&self) -> bool {
        !self.secret_key.is_empty()
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
    pub async fn create_checkout_session(
        &self,
        params: CheckoutParams,
    ) -> Result<CheckoutSession> {
        let url = "https://api.stripe.com/v1/checkout/sessions";
        let mut form = BTreeMap::new();
        form.insert("mode", "payment".to_string());
        form.insert("success_url", params.success_url);
        form.insert("cancel_url", params.cancel_url);
        form.insert(
            "line_items[0][quantity]",
            "1".to_string(),
        );
        form.insert(
            "line_items[0][price_data][currency]",
            "usd".to_string(),
        );
        form.insert(
            "line_items[0][price_data][unit_amount]",
            params.amount_cents.to_string(),
        );
        form.insert(
            "line_items[0][price_data][product_data][name]",
            format!("AIGX Quota Top-up (Order {})", params.trade_no),
        );
        form.insert(
            "client_reference_id",
            params.trade_no.clone(),
        );
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
        let expected_input = format!("{}.{}", ts, String::from_utf8_lossy(payload));
        let expected = hmac_sha256_hex(self.config.webhook_secret.as_bytes(), expected_input.as_bytes());

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

    pub fn user_id(&self) -> Option<String> {
        self.metadata
            .get("user_id")
            .and_then(|v| v.as_str())
            .map(String::from)
    }

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
    use sha2::Sha256;
use hmac::{Hmac, Mac};
    
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC key");
    mac.update(msg);
    let bytes = mac.finalize().into_bytes();
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}
