//! 代理连通性测试设置（`GET/POST /api/proxies/settings`，对照 Agent2API
//! v3.5.0-beta.3 `core/proxy_settings.rs`）。
//!
//! 与上游同语义：整份设置存 KV 的 [`KV_KEY`] 保留键，目前只有一项
//! `testUrl`（测试代理时访问的目标网址；空 = 沿用网关默认值）。旧内核端口
//! 配置不再消费。
//!
//! 校验口径照抄上游：必须是对象；`testUrl` 必须是字符串，非空时须为
//! `http(s)://` 带主机，长度 ≤ [`MAX_URL_LENGTH`]；其它字段原样保留（前向
//! 兼容未知键）。存储不可用时报「设置数据库不可用」，不静默吞掉。

use serde_json::{json, Value};

/// KV 键名（整份 JSON 对象读写）。
pub const KV_KEY: &str = "proxy_settings";

/// 测试网址长度上限（上游同值）。
const MAX_URL_LENGTH: usize = 2048;

/// 缺省测试目标（与 `crate::proxy_pool::DEFAULT_TEST_URL` 同源）。
pub fn default_test_url() -> &'static str {
    super::DEFAULT_TEST_URL
}

fn read_raw() -> Value {
    super::store()
        .ok()
        .and_then(|store| store.get::<Value>(KV_KEY).ok())
        .flatten()
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}))
}

/// 当前配置的测试目标（空/缺省 → None，调用方回落默认值）。
///
/// 存储里是已归一化的值（保存时校验过），这里仍做一次轻量兜底：
/// 外部工具改脏数据时宁可回落默认，也不把非法 URL 交给测试客户端。
pub fn test_url() -> Option<String> {
    let text = read_raw()
        .get("testUrl")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if text.is_empty() || text.chars().count() > MAX_URL_LENGTH {
        return None;
    }
    let parsed = url::Url::parse(&text).ok()?;
    matches!(parsed.scheme(), "http" | "https").then_some(text)
}

/// 公开形态（`{testUrl, defaults:{testUrl}}`，与上游一致）。
pub fn get() -> Value {
    json!({
        "testUrl": read_raw()
            .get("testUrl")
            .and_then(Value::as_str)
            .unwrap_or(""),
        "defaults": { "testUrl": default_test_url() },
    })
}

/// 校验并归一化 `testUrl` 字段（纯函数，便于单测）：返回 trim 后的网址。
fn normalize_test_url(value: &Value) -> Result<String, String> {
    let text = value
        .as_str()
        .ok_or_else(|| "测试网址必须是字符串".to_string())?
        .trim();
    if !text.is_empty() {
        let url = url::Url::parse(text).map_err(|_| "测试网址无效".to_string())?;
        if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
            return Err("测试网址要以 http:// 或 https:// 开头".to_string());
        }
        if text.chars().count() > MAX_URL_LENGTH {
            return Err("测试网址过长".to_string());
        }
    }
    Ok(text.to_string())
}

/// 保存（body 只认 `testUrl`；未知键原样保留，与上游相同）。
pub fn save(input: &Value) -> Result<Value, String> {
    if !input.is_object() {
        return Err("代理设置必须是对象".to_string());
    }
    let mut next = read_raw();
    if let Some(value) = input.get("testUrl") {
        let text = normalize_test_url(value)?;
        next["testUrl"] = json!(text);
    }
    let store = super::store().map_err(|_| "设置数据库不可用".to_string())?;
    store
        .put(KV_KEY, &next)
        .map_err(|error| format!("保存代理设置失败: {error}"))?;
    tracing::info!("[Proxies] ✅ 已更新连通性测试设置");
    Ok(get())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_accepts_http_and_https_and_empty() {
        assert_eq!(
            normalize_test_url(&json!(" https://example.com/ping ")).unwrap(),
            "https://example.com/ping"
        );
        assert_eq!(
            normalize_test_url(&json!("http://10.0.0.1:8080/health")).unwrap(),
            "http://10.0.0.1:8080/health"
        );
        // 空串 = 清除（沿用默认目标）
        assert_eq!(normalize_test_url(&json!("  ")).unwrap(), "");
    }

    #[test]
    fn normalize_rejects_bad_urls() {
        // 负向断言 + 正向对照：非法一律拒绝，合法形态在上一条用例里通过。
        for bad in [
            json!(123),
            json!("ftp://example.com"),
            json!("example.com/ping"),
            json!("https://"),
        ] {
            let result = normalize_test_url(&bad);
            assert!(result.is_err(), "应拒绝 {bad:?}，实际 {result:?}");
        }
        let long = format!("https://example.com/{}", "a".repeat(MAX_URL_LENGTH + 1));
        assert!(normalize_test_url(&json!(long)).is_err());
    }
}
