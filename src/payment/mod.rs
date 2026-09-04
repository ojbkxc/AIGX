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
//! 4. 末尾直接追加商户密钥（无 `&` 分隔符，与易支付官方及 new-api go-epay 一致，
//!    见下方 `sign` 实现）
//! 5. 取 MD5 小写十六进制即 sign

use anyhow::{anyhow, Result};
use md5::compute;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::time::Duration;
use tracing::{debug, info, warn};

pub mod order_store;
pub mod stripe;

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
        !self.pay_address.is_empty() && !self.epay_id.is_empty() && !self.epay_key.is_empty()
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
    /// 客户端 IP（部分易支付网关必填）
    pub clientip: String,
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

/// mapi.php JSON 响应体
///
/// 易支付 mapi.php 返回 `{code, msg, trade_no, payurl, qrcode}`。
/// `code == 1` 表示成功，此时 `payurl`（或 `qrcode`）为真实支付网关地址。
/// 字段名在不同实现中可能有别名（pay_url / qr_code / qr），用 serde alias 兼容。
#[derive(Debug, Deserialize)]
struct EpayApiResponse {
    code: Option<i32>,
    #[serde(default)]
    msg: Option<String>,
    #[serde(default)]
    trade_no: Option<String>,
    #[serde(default, alias = "pay_url")]
    payurl: Option<String>,
    #[serde(default, alias = "qr_code", alias = "qr")]
    qrcode: Option<String>,
}

/// 易支付客户端
///
/// 采用双策略下单：优先 mapi.php（server-to-server，返回真实支付网关地址，
/// 用户浏览器不接触 EPay CDN，避免地区封锁），失败则回退 submit.php 重定向。
#[derive(Debug, Clone)]
pub struct EpayClient {
    config: EpayConfig,
    /// 内嵌 HTTP 客户端：禁用重定向、10s 超时，用于 mapi.php server-to-server 调用
    client: reqwest::Client,
}

impl EpayClient {
    pub fn new(config: EpayConfig) -> Self {
        // 禁用重定向：mapi.php 应直接返回 JSON，任何 3xx 都视为异常。
        // B23：进程级复用连接池——原先每次 new 都重建 reqwest::Client，
        // 连接池/线程池无法复用，高频下单场景徒增 TCP 握手与句柄开销；
        // reqwest::Client 内部为 Arc，clone 廉价，配置固定无 per-instance 差异。
        static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
        let client = CLIENT
            .get_or_init(|| {
                reqwest::Client::builder()
                    .redirect(reqwest::redirect::Policy::none())
                    .timeout(Duration::from_secs(10))
                    .build()
                    .unwrap_or_default()
            })
            .clone();
        Self { config, client }
    }

    pub fn config(&self) -> &EpayConfig {
        &self.config
    }

    /// 规范化网关地址：去除尾部斜杠与已知端点后缀（submit.php / mapi.php / api.php），
    /// 以便后续拼接 `{base}/mapi.php` 或 `{base}/submit.php`。
    /// 参照 VFaka `EpayProvider::normalize_base_url`。
    fn normalize_base_url(url: &str) -> String {
        let mut base = url.trim().trim_end_matches('/').to_string();
        for suffix in &["/submit.php", "/mapi.php", "/api.php"] {
            if base.ends_with(suffix) {
                base.truncate(base.len() - suffix.len());
                break;
            }
        }
        base
    }

    /// 构造签名 — 参照 VFaka 的易支付签名算法：
    /// 1. 按 key 字典序排序（BTreeMap 保证）
    /// 2. 拼接为 `k1=v1&k2=v2...` 形式
    /// 3. 末尾直接追加商户密钥（不加 `&` 分隔符）
    /// 4. MD5 取小写十六进制
    fn sign(&self, params: &BTreeMap<String, String>) -> String {
        let sign_str: String = params
            .iter()
            .filter(|(_, v)| !v.is_empty())
            .map(|(k, v)| format!("{}={}", k, v))
            .collect::<Vec<_>>()
            .join("&");
        let input = format!("{}{}", sign_str, self.config.epay_key);
        format!("{:x}", compute(input.as_bytes()))
    }

    /// 尝试 mapi.php server-to-server API 调用。
    ///
    /// GET `{base}/mapi.php` 带签名参数作为 query，解析 JSON 响应。
    /// `code == 1` 时返回真实支付网关地址（优先 `payurl`，回退 `qrcode`）。
    /// 参照 VFaka `EpayProvider::try_mapi`。
    async fn try_mapi(&self, mapi_url: &str, params: &BTreeMap<String, String>) -> Result<String> {
        let resp = self
            .client
            .get(mapi_url)
            .query(params)
            .send()
            .await
            .map_err(|e| anyhow!("mapi request failed: {}", e))?;

        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| anyhow!("mapi response read failed: {}", e))?;

        if body.trim().is_empty() {
            return Err(anyhow!("mapi returned empty body"));
        }
        if !status.is_success() {
            return Err(anyhow!(
                "mapi returned HTTP {}: {}",
                status,
                body.chars().take(200).collect::<String>()
            ));
        }

        let epay_resp: EpayApiResponse = serde_json::from_str(&body).map_err(|e| {
            anyhow!(
                "mapi parse failed: {} body={}",
                e,
                body.chars().take(200).collect::<String>()
            )
        })?;

        debug!(
            code = ?epay_resp.code,
            payurl = ?epay_resp.payurl,
            qrcode = ?epay_resp.qrcode,
            trade_no = ?epay_resp.trade_no,
            msg = ?epay_resp.msg,
            "EPay mapi.php raw response"
        );

        match epay_resp.code {
            Some(1) => {}
            Some(code) => {
                return Err(anyhow!(
                    "mapi error code={}: {}",
                    code,
                    epay_resp.msg.unwrap_or_default()
                ));
            }
            None => {
                return Err(anyhow!(
                    "mapi missing code field: {}",
                    body.chars().take(200).collect::<String>()
                ));
            }
        }

        let pay_url = epay_resp
            .payurl
            .filter(|u| !u.is_empty())
            .or_else(|| epay_resp.qrcode.clone().filter(|u| !u.is_empty()));

        pay_url.ok_or_else(|| {
            anyhow!(
                "mapi returned no payurl or qrcode: {}",
                body.chars().take(300).collect::<String>()
            )
        })
    }

    /// 下单：构造已签名的参数与跳转 URL（双策略）。
    ///
    /// 1. 先尝试 mapi.php（server-to-server）：成功则 `url` 为真实支付网关地址，
    ///    用户浏览器不接触 EPay CDN，避免地区封锁。
    /// 2. mapi.php 失败则回退 submit.php 重定向：`url` 为 `{base}/submit.php?{query}`。
    pub async fn purchase(&self, args: &PurchaseArgs) -> Result<PurchaseResult> {
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
        params.insert("clientip".into(), args.clientip.clone());
        params.insert("device".into(), args.device.as_str().to_string());

        let sign = self.sign(&params);
        params.insert("sign".into(), sign);
        params.insert("sign_type".into(), "MD5".into());

        let base = Self::normalize_base_url(&self.config.pay_address);

        // 策略 1：mapi.php server-to-server
        let mapi_url = format!("{}/mapi.php", base);
        info!(url = %mapi_url, order_no = %args.out_trade_no, "Trying EPay mapi.php API");
        match self.try_mapi(&mapi_url, &params).await {
            Ok(pay_url) => {
                info!(
                    order_no = %args.out_trade_no,
                    pay_url = %pay_url,
                    "EPay mapi.php succeeded"
                );
                return Ok(PurchaseResult {
                    url: pay_url,
                    params,
                });
            }
            Err(e) => {
                warn!(
                    order_no = %args.out_trade_no,
                    error = %e,
                    "EPay mapi.php failed, falling back to submit.php redirect"
                );
            }
        }

        // 策略 2：submit.php 重定向回退（带 query string，浏览器直接跳转）
        let qs: String = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(params.iter())
            .finish();
        let url = format!("{}/submit.php?{}", base, qs);

        info!(order_no = %args.out_trade_no, "EPay submit.php fallback URL generated");
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
        let got = params.get("sign").ok_or_else(|| anyhow!("missing sign"))?;
        let verify_status = got == &expected;

        let trade_status = params.get("trade_status").cloned().unwrap_or_default();
        let out_trade_no = params.get("out_trade_no").cloned().unwrap_or_default();
        let pay_type = params.get("type").cloned().unwrap_or_default();

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
    /// F02（契约2）：下单时锁定的入账配额（amount × price × discount）。
    /// 回调直接使用该值入账，不再用 money/price 反推——下单与入账之间
    /// 管理员调整倍率/折扣会导致反推结果偏离用户下单时的承诺。
    /// 旧订单缺失该字段时反序列化为 0，入账回退 money/price 公式。
    #[serde(default)]
    pub quota: i64,
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

    #[tokio::test]
    async fn sign_roundtrip() {
        let client = EpayClient::new(cfg());
        let args = PurchaseArgs {
            pay_type: "alipay".into(),
            out_trade_no: "T1".into(),
            name: "TopUp".into(),
            money: "1.00".into(),
            notify_url: "https://x/notify".into(),
            return_url: "https://x/return".into(),
            clientip: "127.0.0.1".into(),
            device: Device::PC,
        };
        let res = client.purchase(&args).await.unwrap();
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
        map.insert("clientip".into(), "127.0.0.1".into());
        map.insert("device".into(), "pc".into());
        map.insert("trade_status".into(), "TRADE_SUCCESS".into());
        map.insert("sign".into(), "badsign".into());
        map.insert("sign_type".into(), "MD5".into());
        let v = client.verify(&map).unwrap();
        assert!(!v.verify_status);
    }
}
