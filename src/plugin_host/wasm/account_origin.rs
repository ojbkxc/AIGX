// Ported from Agent2API v3.5.0-beta.2 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//! 账号来源（account origin）授权 —— 插件只能以「当前账号当前凭据」的
//! 来源地址访问其声明的固定路由，不能自报任意网络地址。
//!
//! ## 对齐来源
//!
//! 对应上游 `plugin_host::wasm::account_origin` 的声明校验与
//! `http.open_account` 主机方法。上游用一份持久化的 `pluginAccountOriginGrants`
//! 授权表（运维逐条确认）；AIGX 的 Wasm 宿主与配置存储解耦，这里以
//! **清单声明即授权**（`wasm.json` 的 `account_origin` 声明经运维安装即代表
//! 批准）落地，并在文档里注明与上游「显式逐条确认」的差异。校验规则与上游
//! 逐条一致：明确至少四级 ASCII 域名后缀、精确 HTTPS 基础路径、GET/POST
//! 精确路由、不接受查询串/编码路径/点段。
//!
//! ## 安全边界
//!
//! 插件**不能**指定目标主机：主机由当前账号的 secret 字段解析，且必须落在
//! 声明的 `host_suffix` 与 `base_path` 内；插件只能选声明里的 `route`。

use std::collections::HashSet;

use serde::Deserialize;
use serde_json::{json, Value};

use super::context::Context;
use super::wire::WireError;

/// 能力名（与上游同名）。
pub(crate) const CAPABILITY: &str = "http.account-origin.v1";

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Declaration {
    pub secret_field: String,
    pub base_path: String,
    pub host_suffix: String,
    pub routes: Vec<Route>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct Route {
    pub method: String,
    pub path: String,
}

fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        && !matches!(value, "__proto__" | "prototype" | "constructor")
}

fn path_ok(value: &str) -> bool {
    value.starts_with('/')
        && value.len() <= 512
        && (value == "/" || !value.ends_with('/'))
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"/-._~".contains(&byte))
        && (value == "/"
            || value[1..]
                .split('/')
                .all(|part| !matches!(part, "" | "." | "..")))
}

impl Declaration {
    pub(super) fn validate(&self) -> Result<(), String> {
        let suffix = self
            .host_suffix
            .strip_prefix('.')
            .ok_or("账号来源必须声明明确的域名后缀")?;
        let labels: Vec<_> = suffix.split('.').collect();
        if !identifier(&self.secret_field)
            || !path_ok(&self.base_path)
            || suffix.len() > 252
            || labels.len() < 4
            || labels.iter().any(|label| {
                label.is_empty()
                    || label.len() > 63
                    || label.starts_with('-')
                    || label.ends_with('-')
                    || !label.bytes().all(|byte| {
                        byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'
                    })
            })
            || labels.last().is_none_or(|label| {
                label.len() < 2 || !label.bytes().all(|byte| byte.is_ascii_lowercase())
            })
            || matches!(
                labels.last().copied(),
                Some("local" | "localhost" | "internal" | "invalid" | "test" | "example" | "onion")
            )
            || suffix.ends_with(".home.arpa")
            || suffix.parse::<std::net::IpAddr>().is_ok()
            || self.routes.is_empty()
            || self.routes.len() > 16
        {
            return Err(
                "账号来源字段、基础路径或委托域名范围无效，不能使用通配符或宽泛域名".into(),
            );
        }
        let mut routes = HashSet::new();
        if self.routes.iter().any(|route| {
            !matches!(route.method.as_str(), "GET" | "POST")
                || !path_ok(&route.path)
                || !routes.insert((&route.method, &route.path))
        }) {
            return Err(
                "账号来源只允许不重复的 GET/POST 精确路径，不允许查询串、编码路径或点段".into(),
            );
        }
        Ok(())
    }

    fn contains_host(&self, host: &str) -> bool {
        host.ends_with(&self.host_suffix) && host.len() > self.host_suffix.len()
    }

    /// 从账号 secret 里取出并规范化来源 URL。
    fn source(&self, secret: &Value) -> Result<url::Url, WireError> {
        let raw = secret
            .get(&self.secret_field)
            .and_then(Value::as_str)
            .filter(|value| {
                value.len() <= 2048
                    && !value.contains('\\')
                    && !value
                        .chars()
                        .any(|ch| ch.is_control() || ch.is_whitespace())
            })
            .ok_or_else(|| denied("当前账号没有规范的服务器来源，请重新登录或导入账号"))?;
        if !raw.starts_with("https://")
            || raw[8..]
                .split(['/', '?', '#'])
                .next()
                .is_none_or(|part| part.is_empty() || part.contains('@'))
        {
            return Err(denied("账号服务器来源必须是规范 HTTPS 地址"));
        }
        let url = url::Url::parse(raw).map_err(|_| denied("账号服务器来源格式无效"))?;
        if url.scheme() != "https"
            || url.port_or_known_default() != Some(443)
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.path() != self.base_path
            || !raw.ends_with(&self.base_path)
            || url.host_str().is_none_or(|host| !self.contains_host(host))
        {
            return Err(denied(
                "当前账号服务器来源不符合已授权的域名、端口或基础路径",
            ));
        }
        Ok(url)
    }
}

fn denied(message: &str) -> WireError {
    WireError::with_code(message, "forbidden")
}

fn bad_params(message: &str) -> WireError {
    WireError::with_code(message, "bad_params")
}

/// 地址是否是可公开路由的（拒绝私网 / 环回 / 链路本地 / 组播 / 文档保留段）。
fn public_ip(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !ip.is_private()
                && !ip.is_loopback()
                && !ip.is_link_local()
                && !ip.is_multicast()
                && !ip.is_broadcast()
                && !ip.is_documentation()
                && a > 0
                && a < 224
                && !(a == 100 && (64..=127).contains(&b))
                && !(a == 198 && matches!(b, 18 | 19))
                && !(a == 192 && b == 0 && c == 0)
                && !(a == 192 && b == 88 && c == 99)
        }
        std::net::IpAddr::V6(ip) => {
            if let Some(ip) = ip.to_ipv4_mapped() {
                return public_ip(std::net::IpAddr::V4(ip));
            }
            let parts = ip.segments();
            parts[0] & 0xe000 == 0x2000
                && parts[0] != 0x2002
                && !(parts[0] == 0x2001 && (parts[1] < 0x0200 || parts[1] == 0x0db8))
                && parts[0] != 0x3ffe
                && !(parts[0] == 0x3fff && parts[1] & 0xf000 == 0)
        }
    }
}

/// 解析并校验目标域名只落到可公开路由的地址（防私网/本机 SSRF）。
async fn public_addresses(url: &url::Url) -> Result<(), WireError> {
    let hostname = url
        .host_str()
        .ok_or_else(|| denied("账号来源缺少服务器域名"))?;
    let resolved = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        tokio::net::lookup_host((hostname, 443)),
    )
    .await
    .map_err(|_| WireError::with_code("账号服务器域名解析超时", "timeout"))?
    .map_err(|_| denied("账号服务器域名解析失败"))?;
    let addresses: Vec<_> = resolved.collect();
    if addresses.is_empty() || addresses.iter().any(|address| !public_ip(address.ip())) {
        return Err(denied("账号服务器域名包含本机、私网或保留地址，已停止连接"));
    }
    if addresses.len() > 64 {
        return Err(denied("账号服务器域名返回地址过多"));
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Open {
    method: String,
    path: String,
    #[serde(default)]
    body_b64: Option<String>,
    #[serde(default)]
    headers: Option<Value>,
}

impl Context {
    /// `http.open_account` —— 以当前账号来源地址访问其声明的固定路由。
    ///
    /// 参数 `{ method, path, body_b64?, headers? }`；`path` 必须精确命中
    /// 声明里的某条路由，主机由账号 secret 解析且必须落在声明的后缀与前缀内。
    /// 目标域名的全部解析地址必须是可公开路由的（拒绝私网/本机）。
    pub(super) async fn open_account(&mut self, params: &Value) -> Result<Value, WireError> {
        self.permission(CAPABILITY)?;
        // 与上游同口径：账号来源请求同时需要普通出站与账号读取能力。
        self.permission("http.v1")?;
        self.permission("accounts.cas.v1")?;
        let declaration = self
            .shared
            .manifest
            .account_origin
            .clone()
            .ok_or_else(|| denied("插件未声明账号来源权限"))?;
        let declaration: Declaration =
            serde_json::from_value(declaration).map_err(|_| denied("账号来源声明格式无效"))?;
        declaration
            .validate()
            .map_err(|_| denied("账号来源声明无效"))?;
        let input: Open = serde_json::from_value(params.clone())
            .map_err(|_| bad_params("账号来源请求参数无效"))?;
        let method = input.method.to_ascii_uppercase();
        if !matches!(method.as_str(), "GET" | "POST") {
            return Err(bad_params("账号来源只允许 GET / POST"));
        }
        if !path_ok(&input.path) || input.path != input.path.trim() {
            return Err(bad_params("账号来源请求路径不合法"));
        }
        if !declaration
            .routes
            .iter()
            .any(|route| route.method == method && route.path == input.path)
        {
            return Err(denied("账号来源请求的方法或路径不在已授权清单中"));
        }
        if let Some(headers) = &input.headers {
            let headers = headers
                .as_object()
                .filter(|headers| headers.len() <= 128)
                .ok_or_else(|| bad_params("账号来源请求头数量无效"))?;
            if headers.iter().any(|(name, value)| {
                reqwest::header::HeaderName::from_bytes(name.as_bytes()).is_err()
                    || value
                        .as_str()
                        .is_none_or(|value| reqwest::header::HeaderValue::from_str(value).is_err())
            }) {
                return Err(bad_params("账号来源请求头格式无效"));
            }
        }
        // 账号上下文（会校验账号归属当前 provider）。
        let account = self.account()?.to_string();
        let provider = self.shared.manifest.provider.clone();
        let record = self.shared.accounts.store_get(&provider, &account).await?;
        let origin = declaration.source(&record["secret"])?;
        public_addresses(&origin).await?;
        let url = format!("{}{}", origin.as_str().trim_end_matches('/'), input.path);
        // 复用出站面：与 `http.open` 同形参数。
        let id = format!("wasm-{}", super::net_id());
        let mut egress_params = json!({
            "id": id,
            "url": url,
            "method": method,
            "provider": provider,
            "proxy": self.proxy.clone(),
            "trace": self.trace.clone(),
            "max_chunk": 64 * 1024,
        });
        if let Some(headers) = input.headers {
            egress_params["headers"] = headers;
        }
        if let Some(body_b64) = input.body_b64 {
            if body_b64.len() > super::MAX_MESSAGE {
                return Err(WireError::with_code(
                    "账号来源请求体过大",
                    "plugin_resource_limit",
                ));
            }
            egress_params["body_b64"] = json!(body_b64);
        }
        self.resources.insert(id.clone(), "http");
        let result = self.shared.egress.request(egress_params).await;
        if result.is_err() {
            self.resources.remove(&id);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decl(suffix: &str) -> Declaration {
        Declaration {
            secret_field: "baseUrl".into(),
            base_path: "/v2".into(),
            host_suffix: suffix.into(),
            routes: vec![
                Route {
                    method: "GET".into(),
                    path: "/models".into(),
                },
                Route {
                    method: "POST".into(),
                    path: "/chat".into(),
                },
            ],
        }
    }

    #[test]
    fn valid_declaration_passes() {
        assert!(decl(".svc.region.example.com").validate().is_ok());
    }

    #[test]
    fn rejects_shallow_suffix() {
        assert!(decl(".example.com").validate().is_err());
        assert!(decl(".com").validate().is_err());
    }

    #[test]
    fn rejects_wildcard_and_bad_paths() {
        let mut d = decl(".svc.region.example.com");
        d.base_path = "/v2/".into(); // 尾斜线不允许
        assert!(d.validate().is_err());
        let mut d = decl(".svc.region.example.com");
        d.routes.push(Route {
            method: "GET".into(),
            path: "/../etc".into(),
        });
        assert!(d.validate().is_err());
    }

    #[test]
    fn source_requires_matching_host_and_base_path() {
        let d = decl(".svc.region.example.com");
        let ok = json!({ "baseUrl": "https://api.svc.region.example.com/v2" });
        assert!(d.source(&ok).is_ok());
        let wrong_host = json!({ "baseUrl": "https://evil.example.net/v2" });
        assert!(d.source(&wrong_host).is_err());
        let wrong_path = json!({ "baseUrl": "https://api.svc.region.example.com/v3" });
        assert!(d.source(&wrong_path).is_err());
        let http = json!({ "baseUrl": "http://api.svc.region.example.com/v2" });
        assert!(d.source(&http).is_err());
    }

    #[test]
    fn rejects_private_and_reserved_addresses() {
        use std::net::IpAddr;
        for text in [
            "127.0.0.1",
            "10.0.0.1",
            "192.168.1.1",
            "169.254.1.1",
            "100.64.0.1",
        ] {
            let ip: IpAddr = text.parse().unwrap();
            assert!(!public_ip(ip), "私网/保留地址必须被拒: {text}");
        }
        for text in ["8.8.8.8", "1.1.1.1", "2606:4700:4700::1111"] {
            let ip: IpAddr = text.parse().unwrap();
            assert!(public_ip(ip), "公网地址必须放行: {text}");
        }
    }
}
