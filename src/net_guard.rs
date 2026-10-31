//! 出站 URL 安全校验（SSRF 防护）。
//!
//! 渠道的 `base_url`、账号的 `base_url` 都由管理员填写，网关会主动向它发请求
//! （拉模型目录、探活、余额查询、转发对话）。如果不做校验，填一个内网地址就能
//! 让网关替攻击者访问内网服务：
//!
//! - `http://127.0.0.1:6379` —— 打本机 Redis
//! - `http://169.254.169.254/latest/meta-data/` —— 云厂商元数据服务（常带临时凭证）
//! - `http://10.0.0.5:9200` —— 内网 Elasticsearch
//! - `http://[::1]:8080` / `http://0.0.0.0` —— IPv6 环回与通配地址
//!
//! ## 校验策略
//!
//! 1. **协议白名单**：只允许 `http` / `https`。
//! 2. **主机名解析**：把域名解析成 IP 后逐个检查 —— 只查字面量 IP 会被
//!    `evil.com → 127.0.0.1` 这类 DNS rebinding 绕过。
//! 3. **拒绝的地址段**：环回、私有、链路本地、唯一本地（IPv6 ULA）、组播、
//!    广播、以及未指定地址。
//!
//! ## 已知取舍
//!
//! - **这是「配置时校验」，不是「连接时校验」**：管理员保存渠道时检查一次。
//!   DNS 在保存与真正发请求之间仍可能改变（TOCTOU）。要彻底封死需要在
//!   reqwest 的 resolver 层做，代价是每次连接多一次解析检查；当前先把
//!   「明文填内网地址」这类最容易发生的误配/攻击挡住。
//! - **允许内网地址的逃生阀**：某些自托管场景（Ollama 在本机、内网 vLLM）
//!   确实需要指向内网。用配置项 `allow_internal_upstream` 显式放行，
//!   而不是静默放行 —— 让管理员知道自己在做什么。

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, ToSocketAddrs};

/// 校验结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UrlRejection {
    /// 协议不是 http/https
    BadScheme(String),
    /// URL 解析失败
    Malformed(String),
    /// 主机名为空
    MissingHost,
    /// 解析不到任何 IP
    Unresolvable(String),
    /// 指向内网/环回/元数据地址（附命中的 IP）
    InternalAddress { host: String, ip: IpAddr },
}

impl UrlRejection {
    /// 给管理员看的中文说明。
    pub fn message(&self) -> String {
        match self {
            Self::BadScheme(scheme) => format!("上游地址只支持 http / https，当前是 `{scheme}`"),
            Self::Malformed(e) => format!("上游地址格式不正确：{e}"),
            Self::MissingHost => "上游地址缺少主机名".to_string(),
            Self::Unresolvable(host) => {
                format!("无法解析上游主机 `{host}`（域名不存在或 DNS 不可用）")
            }
            Self::InternalAddress { host, ip } => format!(
                "上游地址 `{host}` 解析到内网/保留地址 {ip}，已拒绝。\
                 如确需指向内网服务（如本机 Ollama），请在系统设置里开启\
                 「允许内网上游地址」"
            ),
        }
    }
}

/// 该 IP 是否属于禁止出站访问的地址段。
pub fn is_forbidden_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_forbidden_v4(v4),
        IpAddr::V6(v6) => is_forbidden_v6(v6),
    }
}

fn is_forbidden_v4(v4: Ipv4Addr) -> bool {
    v4.is_loopback()          // 127.0.0.0/8
        || v4.is_private()    // 10/8, 172.16/12, 192.168/16
        || v4.is_link_local() // 169.254.0.0/16（含云元数据 169.254.169.254）
        || v4.is_broadcast()  // 255.255.255.255
        || v4.is_documentation()
        || v4.is_unspecified() // 0.0.0.0
        // 共享地址段 100.64.0.0/10（CGNAT，也常被内网使用）
        || (v4.octets()[0] == 100 && (64..128).contains(&v4.octets()[1]))
        // 组播 224.0.0.0/4
        || v4.is_multicast()
}

fn is_forbidden_v6(v6: Ipv6Addr) -> bool {
    v6.is_loopback()               // ::1
        || v6.is_unspecified()     // ::
        || v6.is_multicast()
        // 唯一本地地址 fc00::/7
        || (v6.segments()[0] & 0xfe00) == 0xfc00
        // 链路本地 fe80::/10
        || (v6.segments()[0] & 0xffc0) == 0xfe80
        // IPv4 映射地址 ::ffff:a.b.c.d —— 按内嵌的 v4 判定，
        // 否则 ::ffff:127.0.0.1 会绕过
        || match v6.to_ipv4_mapped() {
            Some(v4) => is_forbidden_v4(v4),
            None => false,
        }
}

/// 校验一个出站 URL 是否允许访问。
///
/// `allow_internal = true` 时跳过内网地址检查（仍校验协议与可解析性）。
pub fn validate_outbound_url(raw: &str, allow_internal: bool) -> Result<(), UrlRejection> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(UrlRejection::MissingHost);
    }
    let parsed =
        reqwest::Url::parse(trimmed).map_err(|e| UrlRejection::Malformed(e.to_string()))?;

    let scheme = parsed.scheme();
    if scheme != "http" && scheme != "https" {
        return Err(UrlRejection::BadScheme(scheme.to_string()));
    }

    let host = parsed
        .host_str()
        .filter(|h| !h.trim().is_empty())
        .ok_or(UrlRejection::MissingHost)?;

    if allow_internal {
        return Ok(());
    }

    // 字面量 IP：直接判定，不必解析
    if let Ok(ip) = host.parse::<IpAddr>() {
        return if is_forbidden_ip(ip) {
            Err(UrlRejection::InternalAddress {
                host: host.to_string(),
                ip,
            })
        } else {
            Ok(())
        };
    }

    // 域名：解析后逐个检查（防 DNS 指向内网）
    let port = parsed.port_or_known_default().unwrap_or(80);
    let addrs: Vec<_> = (host, port)
        .to_socket_addrs()
        .map_err(|_| UrlRejection::Unresolvable(host.to_string()))?
        .collect();
    if addrs.is_empty() {
        return Err(UrlRejection::Unresolvable(host.to_string()));
    }
    for addr in addrs {
        if is_forbidden_ip(addr.ip()) {
            return Err(UrlRejection::InternalAddress {
                host: host.to_string(),
                ip: addr.ip(),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_loopback_and_metadata() {
        for url in [
            "http://127.0.0.1:8080",
            "http://localhost:9200",
            "http://169.254.169.254/latest/meta-data/",
            "http://10.0.0.5",
            "http://192.168.1.1",
            "http://172.16.0.1",
            "http://0.0.0.0:80",
            "http://[::1]:8080",
            "http://[::ffff:127.0.0.1]/",
            "http://224.0.0.1",
        ] {
            assert!(validate_outbound_url(url, false).is_err(), "{url} 应被拒绝");
        }
    }

    #[test]
    fn allows_public_https() {
        // 只校验「不是内网地址」，不实际发请求；1.1.1.1 一定是公网
        assert!(validate_outbound_url("https://1.1.1.1/v1", false).is_ok());
        assert!(validate_outbound_url("http://8.8.8.8:8080/v1", false).is_ok());
    }

    #[test]
    fn rejects_bad_scheme() {
        assert!(matches!(
            validate_outbound_url("file:///etc/passwd", false),
            Err(UrlRejection::BadScheme(_))
        ));
        assert!(matches!(
            validate_outbound_url("gopher://127.0.0.1", false),
            Err(UrlRejection::BadScheme(_))
        ));
    }

    #[test]
    fn allow_internal_escape_hatch() {
        // 自托管 Ollama 等场景显式放行
        assert!(validate_outbound_url("http://127.0.0.1:11434/v1", true).is_ok());
        assert!(validate_outbound_url("http://192.168.1.10:8000", true).is_ok());
    }

    #[test]
    fn empty_and_malformed() {
        assert!(validate_outbound_url("", false).is_err());
        assert!(validate_outbound_url("   ", false).is_err());
        assert!(validate_outbound_url("not a url", false).is_err());
    }

    #[test]
    fn cgnat_range_rejected() {
        // 100.64.0.0/10 是 CGNAT，也常被当作内网使用
        assert!(validate_outbound_url("http://100.64.0.1", false).is_err());
        assert!(validate_outbound_url("http://100.127.255.254", false).is_err());
        // 边界外是公网
        assert!(validate_outbound_url("http://100.63.255.255", false).is_ok());
        assert!(validate_outbound_url("http://100.128.0.1", false).is_ok());
    }
}
