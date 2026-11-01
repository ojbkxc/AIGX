//! ZCode 的 CLI 轮询登录（国内版 bigmodel / 国际版 zai 两个地区）。
//!
//! ```text
//! ① POST {zcode}/api/v1/oauth/cli/init  body {"provider":"bigmodel"|"zai"}（Bearer <poll token>）
//!    → data.flow_id / data.authorize_url / data.poll_interval_sec
//! ② 给 authorize_url 补上中转页参数（国内版 redirect= / 国际版 redirect_uri=，
//!    参数名不同——写错会静默停在 pending 直到超时）
//! ③ GET {zcode}/api/v1/oauth/cli/poll/{flow_id}（Bearer <poll token>）
//!    status=ready → data.{bigmodel|zai}.access_token（OAuth 令牌）+ token（套餐 JWT）
//! ④ OAuth 令牌**不能直接转发**（打推理端点必 401）：要经 biz 链路换成
//!    编码套餐 API Key（见 Agent2API `zcode/coding_key.rs`，本文件的 coding_key 模块）
//! ```
//!
//! 两个地区共用同一个 zcode 平面（zcode.z.ai），只有 `provider` 取值、
//! 中转页参数名与业务域（biz_host）不同。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};

use super::{Credentials, LoginFlow, Started};

const ZCODE_ORIGIN: &str = "https://zcode.z.ai";
const INIT_PATH: &str = "/api/v1/oauth/cli/init";
const APP_VERSION: &str = "3.14.0";

/// 一次登录的地区上下文。
///
/// `upstream_provider` 同时是 init 请求体里的 `provider` 与 poll 响应里的
/// 分支键（`data.bigmodel.access_token` / `data.zai.access_token`）。
pub(crate) struct Region {
    /// OAuth 的 provider 名（bigmodel / zai，不是 provider id）
    upstream_provider: &'static str,
    /// 中转页参数名（国内版 redirect / 国际版 redirect_uri）
    interstitial_param: &'static str,
    /// 凭证换取的业务域（Agent2API region.rs：国内 bigmodel.cn / 国际 api.z.ai）
    biz_host: &'static str,
}

/// 国内版（bigmodel）
const REGION_CN: Region = Region {
    upstream_provider: "bigmodel",
    interstitial_param: "redirect",
    biz_host: "https://bigmodel.cn",
};

/// 国际版（zai）
const REGION_INTL: Region = Region {
    upstream_provider: "zai",
    interstitial_param: "redirect_uri",
    biz_host: "https://api.z.ai",
};

pub struct ZcodeFlow {
    client: reqwest::Client,
    region: &'static Region,
    flow_id: String,
    poll_token: String,
    interval: Duration,
}

#[async_trait]
impl LoginFlow for ZcodeFlow {
    fn poll_interval(&self) -> Duration {
        self.interval
    }

    async fn poll(&self) -> Result<Option<Credentials>, String> {
        let url = format!("{ZCODE_ORIGIN}/api/v1/oauth/cli/poll/{}", self.flow_id);
        let response = match self
            .client
            .get(&url)
            .header("Accept", "application/json")
            .header("Authorization", format!("Bearer {}", self.poll_token))
            .send()
            .await
        {
            Ok(response) => response,
            Err(_) => return Ok(None),
        };
        let status = response.status().as_u16();
        if status >= 500 || status == 408 || status == 429 {
            return Ok(None);
        }
        let text = response.text().await.unwrap_or_default();
        let payload = match serde_json::from_str::<Value>(&text) {
            Ok(value) => value,
            Err(_) => return Ok(None),
        };
        if let Some(code) = payload.get("code").and_then(Value::as_i64) {
            if code != 0 {
                let message = payload
                    .get("msg")
                    .or_else(|| payload.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("上游拒绝");
                return Err(format!("ZCode 登录失败（{code}）：{message}"));
            }
        }
        if status >= 400 {
            return Err(format!("ZCode 登录轮询被拒（HTTP {status}）"));
        }
        let data = match payload.get("data") {
            Some(data) => data,
            None => return Ok(None),
        };
        match data.get("status").and_then(Value::as_str).unwrap_or("") {
            "ready" => {}
            "failed" => return Err("ZCode 授权被拒绝或已失效，请重新发起登录".to_string()),
            _ => return Ok(None),
        }
        let access_token = data
            .get(self.region.upstream_provider)
            .and_then(|value| value.get("access_token"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        if access_token.is_empty() {
            return Err(format!(
                "ZCode 登录响应缺少 {}.access_token",
                self.region.upstream_provider
            ));
        }
        // OAuth 令牌不能直接用于推理（上游按 OAuth 那条路校验、必 401），
        // 先换成编码套餐 API Key。换取失败即登录失败：不落一条
        // 「能登录但永远发不出请求」的半成品账号（与 Agent2API 同一取舍）。
        let api_key = coding_key::resolve(self.region, &access_token).await?;
        let user_id = data
            .get("user")
            .and_then(|value| value.get("user_id"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        let name = if user_id.is_empty() {
            "ZCode".to_string()
        } else {
            format!("ZCode {user_id}")
        };
        Ok(Some(Credentials {
            name,
            secret: api_key,
            base_url: None,
            extra_headers: HashMap::new(),
            expires_at: None,
        }))
    }
}

pub async fn start(client: &reqwest::Client, intl: bool) -> Result<Started, String> {
    let region = if intl { &REGION_INTL } else { &REGION_CN };
    let poll_token = random_hex_32();
    let url = format!("{ZCODE_ORIGIN}{INIT_PATH}");
    let response = client
        .post(&url)
        .header("Authorization", format!("Bearer {poll_token}"))
        .header("Content-Type", "application/json")
        .json(&json!({ "provider": region.upstream_provider }))
        .send()
        .await
        .map_err(|error| format!("ZCode 登录发起失败：{error}"))?;
    let status = response.status().as_u16();
    let text = response.text().await.map_err(|error| error.to_string())?;
    let payload = serde_json::from_str::<Value>(&text)
        .map_err(|_| "ZCode 登录发起：上游响应不是 JSON".to_string())?;
    if let Some(code) = payload.get("code").and_then(Value::as_i64) {
        if code != 0 {
            let message = payload
                .get("msg")
                .or_else(|| payload.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("上游拒绝");
            return Err(format!("ZCode 登录发起失败（{code}）：{message}"));
        }
    }
    if status >= 400 {
        return Err(format!("ZCode 登录发起失败（HTTP {status}）"));
    }
    let data = payload
        .get("data")
        .ok_or_else(|| "ZCode 登录发起：响应缺少 data".to_string())?;
    let flow_id = data
        .get("flow_id")
        .and_then(Value::as_str)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "ZCode 登录响应缺少 flow_id".to_string())?;
    let authorize_url = data
        .get("authorize_url")
        .and_then(Value::as_str)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "ZCode 登录响应缺少 authorize_url".to_string())?;
    let auth_url = apply_interstitial(region, &authorize_url);
    let interval = Duration::from_secs(
        data.get("poll_interval_sec")
            .and_then(Value::as_i64)
            .filter(|value| *value > 0)
            .unwrap_or(3)
            .max(1) as u64,
    );
    Ok(Started {
        state: flow_id.clone(),
        auth_url,
        flow: Arc::new(ZcodeFlow {
            client: client.clone(),
            region,
            flow_id,
            poll_token,
            interval,
        }),
    })
}

/// 把中转页参数替换进 `authorize_url`（同名参数 set 语义：全部替换、只留一个）。
///
/// 中转页地址要**整体**编码（内层已编码的 `zcode%3A%2F%2F…` 再编一次成
/// `%253A%252F%252F…`），与 Agent2API 逐字节同形。
fn apply_interstitial(region: &Region, authorize_url: &str) -> String {
    let target = encode_component(&format!(
        "{ZCODE_ORIGIN}/app/oauth/login?redirect=zcode%3A%2F%2Foauth%2Fcallback&app_version={APP_VERSION}"
    ));
    let prefix = format!("{}=", region.interstitial_param);
    let replacement = format!("{prefix}{target}");
    let (head, fragment) = match authorize_url.split_once('#') {
        Some((head, fragment)) => (head, Some(fragment)),
        None => (authorize_url, None),
    };
    let (path, query) = match head.split_once('?') {
        Some((path, query)) => (path, query),
        None => (head, ""),
    };
    let mut replaced = false;
    let mut segments: Vec<String> = Vec::new();
    for segment in query.split('&').filter(|value| !value.is_empty()) {
        if segment.starts_with(prefix.as_str()) {
            if !replaced {
                segments.push(replacement.clone());
                replaced = true;
            }
            continue;
        }
        segments.push(segment.to_string());
    }
    if !replaced {
        segments.push(replacement);
    }
    let mut out = String::from(path);
    out.push('?');
    out.push_str(&segments.join("&"));
    if let Some(fragment) = fragment {
        out.push('#');
        out.push_str(fragment);
    }
    out
}

fn random_hex_32() -> String {
    let mut out = String::with_capacity(64);
    for _ in 0..2 {
        out.push_str(&uuid::Uuid::new_v4().simple().to_string());
    }
    out
}

fn encode_component(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        let ch = *byte as char;
        if ch.is_ascii_alphanumeric()
            || matches!(ch, '-' | '_' | '.' | '!' | '~' | '*' | '\'' | '(' | ')')
        {
            out.push(ch);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

// ── 编码套餐凭证换取（移植自 Agent2API `providers/zcode/coding_key.rs`）──
//
// OAuth 登录给的 access_token 不是推理凭证：直接打 /chat/completions 会被
// 上游按「OAuth 令牌」那条路校验并回 401。官方客户端与参考实现都先做一次
// 换取，拿到的才是编码套餐 API Key：
//   z/login（仅国际版）→ 查默认机构/项目 → 找或建 zcode-api-key → 取 secret
mod coding_key {
    use super::Region;

    /// 网关在用户账号里认的密钥名（同名复用，重复登录不堆密钥）
    const KEY_NAME: &str = "zcode-api-key";
    const DEFAULT_ORG_MARKER: &str = "默认机构";
    const DEFAULT_PROJECT_MARKER: &str = "默认项目";

    /// 换取编码套餐凭证：成功返回**直接可用于推理**的 API Key 串。
    ///
    /// 国际版恒为 `{apiKey}.{secret}`；国内版有 secret 时两段、否则单段。
    pub(super) async fn resolve(
        region: &'static Region,
        access_token: &str,
    ) -> Result<String, String> {
        let oauth_token = access_token.trim();
        if oauth_token.is_empty() {
            return Err("ZCode 登录响应里没有可换取推理凭证的登录态".to_string());
        }
        let host = region.biz_host;
        // 国际版：OAuth 令牌 → biz 令牌；国内版：OAuth 令牌直接当 biz 令牌用
        let (biz_token, bearer_prefix) = if host == "https://api.z.ai" {
            (z_login(host, oauth_token).await?, true)
        } else {
            (oauth_token.to_string(), false)
        };
        let authorization = if bearer_prefix {
            format!("Bearer {biz_token}")
        } else {
            biz_token
        };

        let (org_id, project_id) = customer_info(host, &authorization).await?;
        let api_key = find_or_create_api_key(host, &authorization, &org_id, &project_id).await?;
        let secret = copy_secret_key(host, &authorization, &org_id, &project_id, &api_key).await?;

        if secret.trim().is_empty() {
            // 国际版没有 secret 签不了名（发出去必 401），判登录失败；
            // 国内版单段 API Key 也能用
            if host == "https://api.z.ai" {
                return Err(
                    "ZCode 国际版未返回密钥 secret（无法用于推理），请在 z.ai 控制台确认编码套餐已开通后重试"
                        .to_string(),
                );
            }
            return Ok(api_key);
        }
        Ok(format!("{api_key}.{secret}"))
    }

    /// 国际版第一步：`POST {host}/api/auth/z/login`，用 OAuth 令牌换 biz 令牌。
    async fn z_login(host: &str, oauth_token: &str) -> Result<String, String> {
        let url = format!("{host}/api/auth/z/login");
        let body = serde_json::json!({ "token": oauth_token });
        let payload = biz_request("POST", &url, "", Some(&body)).await?;
        payload
            .get("access_token")
            .or_else(|| payload.get("accessToken"))
            .or_else(|| payload.get("data").and_then(|d| d.get("access_token")))
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
            .ok_or_else(|| "ZCode 登录换取失败：上游未返回访问令牌".to_string())
    }

    /// 查默认机构与默认项目（按名字片段找，找不到退回第一个）。
    async fn customer_info(host: &str, authorization: &str) -> Result<(String, String), String> {
        let url = format!("{host}/api/biz/customer/getCustomerInfo");
        let payload = biz_request("GET", &url, authorization, None).await?;
        let orgs = payload
            .get("organizations")
            .or_else(|| payload.get("orgs"))
            .and_then(serde_json::Value::as_array)
            .filter(|items| !items.is_empty())
            .ok_or_else(|| "ZCode 登录换取失败：该账号下没有可用机构".to_string())?;
        let org = orgs
            .iter()
            .find(|item| {
                org_name(item)
                    .map(|name| name.contains(DEFAULT_ORG_MARKER))
                    .unwrap_or(false)
            })
            .or_else(|| orgs.first())
            .ok_or_else(|| "ZCode 登录换取失败：该账号下没有可用机构".to_string())?;
        let org_id = pick_id(org, &["organizationId", "id", "orgId"])
            .ok_or_else(|| "ZCode 登录换取失败：机构缺少标识".to_string())?;
        let projects = org
            .get("projects")
            .and_then(serde_json::Value::as_array)
            .filter(|items| !items.is_empty())
            .ok_or_else(|| "ZCode 登录换取失败：默认机构下没有可用项目".to_string())?;
        let project = projects
            .iter()
            .find(|item| {
                project_name(item)
                    .map(|name| name.contains(DEFAULT_PROJECT_MARKER))
                    .unwrap_or(false)
            })
            .or_else(|| projects.first())
            .ok_or_else(|| "ZCode 登录换取失败：默认机构下没有可用项目".to_string())?;
        let project_id = pick_id(project, &["projectId", "id"])
            .ok_or_else(|| "ZCode 登录换取失败：项目缺少标识".to_string())?;
        Ok((org_id, project_id))
    }

    /// 找同名密钥，没有就建一个。
    async fn find_or_create_api_key(
        host: &str,
        authorization: &str,
        org_id: &str,
        project_id: &str,
    ) -> Result<String, String> {
        let url = format!("{host}/api/biz/v1/organization/{org_id}/projects/{project_id}/api_keys");
        // 列表失败不致命（没有列表权限但能建密钥的账号依然可用）
        if let Ok(payload) = biz_request("GET", &url, authorization, None).await {
            let found = payload
                .as_array()
                .into_iter()
                .flatten()
                .find(|item| {
                    item.get("name").and_then(serde_json::Value::as_str) == Some(KEY_NAME)
                        && item
                            .get("apiKey")
                            .and_then(serde_json::Value::as_str)
                            .map(|key| !key.trim().is_empty())
                            .unwrap_or(false)
                })
                .and_then(|item| item.get("apiKey").and_then(serde_json::Value::as_str))
                .map(|key| key.trim().to_string())
                .filter(|key| !key.is_empty());
            if let Some(key) = found {
                return Ok(key);
            }
        }
        let body = serde_json::json!({ "name": KEY_NAME });
        let created = biz_request("POST", &url, authorization, Some(&body)).await?;
        created
            .get("apiKey")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|key| !key.is_empty())
            .map(str::to_string)
            .ok_or_else(|| "ZCode 登录换取失败：上游未返回新建密钥".to_string())
    }

    /// 取密钥的 secret（失败与「没给」同义：返回空串，由调用方按地区判定）。
    async fn copy_secret_key(
        host: &str,
        authorization: &str,
        org_id: &str,
        project_id: &str,
        api_key: &str,
    ) -> Result<String, String> {
        let url = format!(
            "{host}/api/biz/v1/organization/{org_id}/projects/{project_id}/api_keys/copy/{}",
            encode_path_segment(api_key)
        );
        let Ok(payload) = biz_request("GET", &url, authorization, None).await else {
            return Ok(String::new());
        };
        Ok(payload
            .get("secretKey")
            .or_else(|| payload.get("secret_key"))
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .unwrap_or("")
            .to_string())
    }

    /// 一次业务接口调用（信封语义：code 0/200 算成功，缺失视为成功）。
    async fn biz_request(
        method: &str,
        url: &str,
        authorization: &str,
        body: Option<&serde_json::Value>,
    ) -> Result<serde_json::Value, String> {
        let client = reqwest::Client::new();
        let mut request = client.request(
            reqwest::Method::from_bytes(method.as_bytes()).unwrap_or(reqwest::Method::GET),
            url,
        );
        if !authorization.trim().is_empty() {
            request = request.header("Authorization", authorization);
        }
        if let Some(body) = body {
            request = request
                .header("Content-Type", "application/json")
                .json(body);
        }
        let response = request
            .send()
            .await
            .map_err(|error| format!("ZCode 登录换取失败：{error}"))?;
        let payload: serde_json::Value = response
            .json()
            .await
            .map_err(|_| "ZCode 登录换取失败：上游响应不是 JSON".to_string())?;
        if let Some(code) = payload.get("code").or_else(|| payload.get("status")) {
            if !biz_code_ok(code) {
                let message = payload
                    .get("msg")
                    .and_then(serde_json::Value::as_str)
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                    .unwrap_or("上游未说明原因");
                return Err(format!("ZCode 登录换取被拒：{message}"));
            }
        }
        Ok(payload.get("data").cloned().unwrap_or(payload))
    }

    fn biz_code_ok(code: &serde_json::Value) -> bool {
        if let Some(number) = code.as_i64() {
            return number == 0 || number == 200;
        }
        matches!(
            code.as_str().map(str::trim),
            Some("0") | Some("200") | Some("")
        )
    }

    fn org_name(item: &serde_json::Value) -> Option<&str> {
        item.get("organizationName")
            .or_else(|| item.get("name"))
            .and_then(serde_json::Value::as_str)
    }

    fn project_name(item: &serde_json::Value) -> Option<&str> {
        item.get("projectName")
            .or_else(|| item.get("name"))
            .and_then(serde_json::Value::as_str)
    }

    fn pick_id(item: &serde_json::Value, keys: &[&str]) -> Option<String> {
        keys.iter()
            .find_map(|key| item.get(*key).and_then(serde_json::Value::as_str))
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    }

    /// 路径段编码（密钥可能带 `.` 等保留字符，直接拼会让 copy/{key} 断掉）
    fn encode_path_segment(value: &str) -> String {
        let mut out = String::with_capacity(value.len());
        for byte in value.as_bytes() {
            let ch = *byte as char;
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '~') {
                out.push(ch);
            } else {
                out.push_str(&format!("%{byte:02X}"));
            }
        }
        out
    }
}
