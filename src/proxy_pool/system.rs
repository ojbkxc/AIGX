//! 系统代理读取（`GET /api/proxies/system`，对照 Agent2API v3.5.0-beta.3
//! `core/proxies/system.rs`）。
//!
//! 只读取通用 HTTPS 出口，不访问探测地址；结果供「账号代理」表单的
//! 「从系统代理导入」按钮回填。协议只支持 HTTP / SOCKS5（与上游一致的
//! 取舍：其它代理形态没法用 hyper 的 matcher 表达时如实报错，让用户手填）。

use base64::Engine;
use hyper_util::client::proxy::matcher::{Intercept, Matcher};
use serde_json::{json, Value};

/// 读系统代理快照：`{proxy: {source, protocol, host, port, username, password} | null}`。
///
/// 认证信息只在内存里传递（返回值仅用于本次管理接口响应，不落库）。
pub fn snapshot() -> Result<Value, String> {
    // 目标只用「协议 + 主机」触发匹配，不会真正出网。
    let target = axum::http::Uri::from_static("https://proxy-detection.invalid/");
    let Some(proxy) = Matcher::from_system().intercept(&target) else {
        return Ok(json!({ "proxy": null }));
    };
    let uri = proxy.uri();
    let (protocol, default_port) = match uri.scheme_str() {
        Some("http") => ("http", 80),
        Some("socks5" | "socks5h") => ("socks5", 1080),
        _ => {
            return Err("系统代理协议暂不支持自动填入，请手动配置 HTTP 或 SOCKS5 代理".to_string())
        }
    };
    let host = uri
        .host()
        .filter(|host| !host.is_empty())
        .ok_or("系统代理地址无效，请手动填写")?;
    let port = uri.port_u16().unwrap_or(default_port);
    if port == 0 {
        return Err("系统代理地址无效，请手动填写".to_string());
    }
    let (username, password) = credentials(&proxy)?;
    Ok(json!({
        "proxy": {
            "source": "custom",
            "protocol": protocol,
            "host": host,
            "port": port,
            "username": username,
            "password": password,
        }
    }))
}

fn credentials(proxy: &Intercept) -> Result<(String, String), String> {
    if let Some((username, password)) = proxy.raw_auth() {
        return Ok((username.to_string(), password.to_string()));
    }
    let Some(header) = proxy.basic_auth() else {
        return Ok((String::new(), String::new()));
    };
    let invalid = || "系统代理认证信息无法读取，请手动填写".to_string();
    let encoded = header
        .to_str()
        .ok()
        .and_then(|value| value.strip_prefix("Basic "))
        .ok_or_else(invalid)?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| invalid())?;
    let text = String::from_utf8(bytes).map_err(|_| invalid())?;
    let (username, password) = text.split_once(':').ok_or_else(invalid)?;
    Ok((username.to_string(), password.to_string()))
}

#[cfg(test)]
mod tests {
    //! 系统代理读取依赖真实 OS 配置，无法在单测里稳定断言；
    //! 这里只锚定「无系统代理时返回 `{proxy:null}`」这一兜底形态的结构约束
    //! 由 handler 测试覆盖（见 proxy_pool_api tests）。解析逻辑与上游逐行
    //! 对齐，保留函数级说明即可。
}
