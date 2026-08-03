//! 易支付 (Epay) 模块 — MD5 签名的支付下单与回调验签。
//!
//! 仿 new-api 的 go-epay 包与 subscription_payment_epay.go：
//! - `EpayClient::purchase(args)` 构造表单参数并签名，返回提交 URL + params。
//! - `EpayClient::verify(params)` 验证回调签名，并返回验签结果。
//!
//! 易支付签名规则：
//! 1. 过滤 sign / sign_type 与值为空的参数
//! 2. 按 key 字典序排序
//! 3. 拼接为 `k1=v1&k2=v2...` 形式
//! 4. 末尾追加 `&key=PARTNER_KEY`（注意前面是 `&`）
//! 5. 取 MD5 小写十六进制即 sign

use anyhow::{anyhow, Result};
use md5::compute;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::collections::HashMap;

pub mod order_store;

/// 易支付配置
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EpayConfig {
    /// 易支付网关地址，如 https://pay.example.com
    pub pay_address: String,
    /// 商户 ID (PID)
    pub epay_id: String,
    /// 商户密钥 (KEY)
    pub epay_key: String,
    /// 启用的支付方式: alipay / wxpay / qqpay / bank 等
    #[serde(default)]
    pub pay_methods: Vec<String>,
    /// 充值倍率：1 元可购买多少配额
    #[serde(default = "default_price")]
    pub price: f64,
    /// 充值折扣: 原始金额 → 折扣 (0~1)，可选
    #[serde(default)]
    pub amount_discount: HashMap<i64, f64>,
    /// 最低充值金额
    #[serde(default = "default_min_topup")]
    pub min_topup: i64,
    /// 自定义回调地址，留空则使用 ServerAddress
    #[serde(default)]
    pub custom_callback_address: String,
}

fn default_price() -> f64 {
    1.0
}

fn default_min_topup() -> i64 {
    1
}

impl EpayConfig {
    /// 是否已配置完整
    pub fn ready(&self) -> bool {
        !self.pay_address.is_empty()
            && !self.epay_id.is_empty()
            && !self.epay_key.is_empty()
    }

    /// 是否包含某支付方式
    pub fn contains_pay_method(&self, m: &str) -> bool {
        self.pay_methods.iter().any(|s| s == m)
    }
}

/// 支付设备类型
#[derive(Debug, Clone, Copy)]
pub enum Device {
    PC,
    Mobile,
}

impl Device {
    pub fn as_str(&self) -> &'static str {
        match self {
            Device::PC => "pc",
            Device::Mobile => "mobile",
        }
    }
}

/// 下单参数
#[derive(Debug, Clone)]
pub struct PurchaseArgs {
    /// 支付方式: alipay / wxpay / qqpay ...
    pub pay_type: String,
    /// 商户订单号
    pub out_trade_no: String,
    /// 商品名称
    pub name: String,
    /// 金额（元，2 位小数字符串）
    pub money: String,
    /// 异步通知地址
    pub notify_url: String,
    /// 同步跳转地址
    pub return_url: String,
    pub device: Device,
}

/// 下单返回：提交地址 + 已签名参数
#[derive(Debug, Clone, Serialize)]
pub struct PurchaseResult {
    /// 表单提交 URL（用户跳转/POST 到此地址完成支付）
    pub url: String,
    /// 已签名的参数（可拼成 query 或作为 form 字段）
    pub params: BTreeMap<String, String>,
}

/// 验签结果
#[derive(Debug, Clone)]
pub struct VerifyResult {
    pub verify_status: bool,
    pub trade_status: String,
    pub out_trade_no: String,
    /// 回传的支付方式
    pub pay_type: String,
}

/// 易支付客户端
#[derive(Debug, Clone)]
pub struct EpayClient {
    config: EpayConfig,
}

impl EpayClient {
    pub fn new(config: EpayConfig) -> Self {
        Self { config }
    }

    pub fn config(&self) -> &EpayConfig {
        &self.config
    }

    /// 构造签名
    fn sign(&self, params: &BTreeMap<String, String>) -> String {
        let mut buf = String::new();
        for (k, v) in params {
            if v.is_empty() {
                continue;
            }
            if !buf.is_empty() {
                buf.push('&');
            }
            buf.push_str(k);
            buf.push('=');
            buf.push_str(v);
        }
        buf.push_str(&format!("&{}", self.config.epay_key));
        format!("{:x}", compute(buf.as_bytes()))
    }

    /// 下单：构造已签名的参数与跳转 URL
    pub fn purchase(&self, args: &PurchaseArgs) -> Result<PurchaseResult> {
        if !self.config.ready() {
            return Err(anyhow!("epay not configured"));
        }
        let mut params = BTreeMap::new();
        params.insert("pid".into(), self.config.epay_id.clone());
        params.insert("type".into(), args.pay_type.clone());
        params.insert("out_trade_no".into(), args.out_trade_no.clone());
        params.insert("notify_url".into(), args.notify_url.clone());
        params.insert("return_url".into(), args.return_url.clone());
        params.insert("name".into(), args.name.clone());
        params.insert("money".into(), args.money.clone());
        params.insert("device".into(), args.device.as_str().to_string());

        let sign = self.sign(&params);
        params.insert("sign".into(), sign);
        params.insert("sign_type".into(), "MD5".into());

        // submit.php 接受 GET query 与 POST form
        let mut url = self.config.pay_address.clone();
        if !url.ends_with('/') {
            url.push('/');
        }
        url.push_str("submit.php");

        Ok(PurchaseResult { url, params })
    }

    /// 验证回调签名
    pub fn verify(&self, params: &HashMap<String, String>) -> Result<VerifyResult> {
        if !self.config.ready() {
            return Err(anyhow!("epay not configured"));
        }
        let mut filtered = BTreeMap::new();
        for (k, v) in params {
            if k == "sign" || k == "sign_type" {
                continue;
            }
            if v.is_empty() {
                continue;
            }
            filtered.insert(k.clone(), v.clone());
        }

        let expected = self.sign(&filtered);
        let got = params
            .get("sign")
            .ok_or_else(|| anyhow!("missing sign"))?;
        let verify_status = got == &expected;

        let trade_status = params
            .get("trade_status")
            .cloned()
            .unwrap_or_default();
        let out_trade_no = params
            .get("out_trade_no")
            .cloned()
            .unwrap_or_default();
        let pay_type = params
            .get("type")
            .cloned()
            .unwrap_or_default();

        Ok(VerifyResult {
            verify_status,
            trade_status,
            out_trade_no,
            pay_type,
        })
    }
}

/// 充值订单
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopUpOrder {
    pub trade_no: String,
    pub user_id: String,
    /// 原始充值数量（用户选择的 amount，单位随展示类型）
    pub amount: i64,
    /// 实际支付金额（元）
    pub money: f64,
    /// 支付方式
    pub payment_method: String,
    /// 状态: pending / paid / expired
    #[serde(default = "default_pending")]
    pub status: String,
    pub create_time: i64,
    #[serde(default)]
    pub paid_time: Option<i64>,
}

fn default_pending() -> String {
    "pending".into()
}

impl TopUpOrder {
    pub fn is_pending(&self) -> bool {
        self.status == "pending"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> EpayConfig {
        EpayConfig {
            pay_address: "https://pay.example.com".into(),
            epay_id: "1001".into(),
            epay_key: "secretkey".into(),
            pay_methods: vec!["alipay".into(), "wxpay".into()],
            price: 1.0,
            amount_discount: HashMap::new(),
            min_topup: 1,
            custom_callback_address: String::new(),
        }
    }

    #[test]
    fn sign_roundtrip() {
        let client = EpayClient::new(cfg());
        let args = PurchaseArgs {
            pay_type: "alipay".into(),
            out_trade_no: "T1".into(),
            name: "TopUp".into(),
            money: "1.00".into(),
            notify_url: "https://x/notify".into(),
            return_url: "https://x/return".into(),
            device: Device::PC,
        };
        let res = client.purchase(&args).unwrap();
        assert!(res.params.contains_key("sign"));
        assert_eq!(res.params.get("sign_type").unwrap(), "MD5");
        // 模拟网关回传：原参数 + trade_status，并按相同规则重新签名
        let mut map: HashMap<String, String> = res
            .params
            .iter()
            .filter(|(k, _)| *k != "sign" && *k != "sign_type")
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        map.insert("trade_status".into(), "TRADE_SUCCESS".into());
        // 用客户端同样的签名规则生成回传 sign
        let mut filtered = BTreeMap::new();
        for (k, v) in &map {
            if !v.is_empty() {
                filtered.insert(k.clone(), v.clone());
            }
        }
        map.insert("sign".into(), client.sign(&filtered));
        map.insert("sign_type".into(), "MD5".into());
        let v = client.verify(&map).unwrap();
        assert!(v.verify_status);
        assert_eq!(v.trade_status, "TRADE_SUCCESS");
    }

    #[test]
    fn sign_tampered() {
        let client = EpayClient::new(cfg());
        let mut map = HashMap::new();
        map.insert("pid".into(), "1001".into());
        map.insert("type".into(), "alipay".into());
        map.insert("out_trade_no".into(), "T1".into());
        map.insert("notify_url".into(), "https://x/notify".into());
        map.insert("return_url".into(), "https://x/return".into());
        map.insert("name".into(), "TopUp".into());
        map.insert("money".into(), "1.00".into());
        map.insert("device".into(), "pc".into());
        map.insert("trade_status".into(), "TRADE_SUCCESS".into());
        map.insert("sign".into(), "badsign".into());
        map.insert("sign_type".into(), "MD5".into());
        let v = client.verify(&map).unwrap();
        assert!(!v.verify_status);
    }
}
