//! 目录刷新的统一网络层：超时、大小上限、限流重试语义。
//!
//! ## 为什么收敛到一个模块
//!
//! 目录刷新要访问三类源：raw.githubusercontent.com（registry / dist /
//! 仓内文件）、api.github.com（搜索 / repo 星数）、registry.npmjs.org
//! （版本回退）。三类的失败语义不同：
//!
//! - GitHub API 的 403/429 带 `x-ratelimit-reset` / `retry-after` 头——
//!   **这是权威的下次可重试时刻**，自己拍脑袋退避只会越打越死；
//! - npm registry 基本不限流，失败按普通退避；
//! - 5xx / 网络错误退避后原路重试一次即可（目录刷新是后台任务，不值得
//!   为它做复杂重试策略）。
//!
//! 所有失败统一折叠成 [`Failure`]：`retry_at`（unix 秒）由 refresh 层
//! 落进缓存的重试闸门，到期前该阶段不再发请求。

use std::time::Duration;

use serde::Deserialize;

/// 一次失败（message 给管理面 / 日志；retry_at 是下次允许重试的 unix 秒）。
#[derive(Clone, Debug)]
pub struct Failure {
    pub message: String,
    pub retry_at: i64,
}

impl Failure {
    /// 立即失败（retry_at = now + delay）。
    pub fn delayed(message: impl Into<String>, delay_secs: i64) -> Self {
        Failure {
            message: message.into(),
            retry_at: now_unix() + delay_secs,
        }
    }

    /// 404：内容短期内不会出现，长退避（1h）。调用方可对「预期内的 404」
    /// （如仓里没有 package.json）自行降级处理。
    pub fn not_found(what: &str) -> Self {
        Self::delayed(format!("{what}: HTTP 404"), 3600)
    }
}

/// 当前 unix 秒。全目录模块统一从这里取时间，保证 TTL / retry_at 可比较。
pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// RFC3339 / ISO8601（如 dist 的 `2026-11-11T08:19:01.838Z`、GitHub 的
/// `pushed_at`）→ unix 秒。目录时间字段统一存 unix 秒，时区无关。
pub fn iso_to_unix(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.timestamp())
}

/// 目录刷新用的 HTTP 客户端：连接超时 + UA（GitHub API 强制要求 UA）。
///
/// 请求级超时由调用方用 [`fetch_bytes`] 的 timeout 参数控制——目录各阶段
/// 体量差异大（registry 79KB vs dist 3.5MB），统一总超时要么浪费要么误杀。
pub fn build_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .user_agent(format!("AIGX-Gateway/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .unwrap_or_default()
}

/// GitHub 个人令牌：环境变量可配（可选）。未配置走匿名配额（搜索 10 次/分
/// 与 core 60 次/时）——对 6h/10min 的刷新节奏够用；限流时 Failure.retry_at
/// 会把下次刷新推到配额重置之后。
pub fn github_token() -> Option<String> {
    std::env::var("GITHUB_TOKEN")
        .or_else(|_| std::env::var("MAGPIE_GITHUB_TOKEN"))
        .ok()
        .filter(|t| !t.trim().is_empty())
}

/// GitHub 请求附加头。
fn apply_github_headers(
    rb: reqwest::RequestBuilder,
    token: Option<&str>,
) -> reqwest::RequestBuilder {
    let rb = rb
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28");
    match token {
        Some(t) => rb.header(reqwest::header::AUTHORIZATION, format!("Bearer {t}")),
        None => rb,
    }
}

/// 从限流响应头算「下次可重试」时刻。
///
/// `x-ratelimit-reset` 是 epoch 秒（GitHub 权威）；`retry-after` 是相对秒数。
/// 下限 30s（防止 reset 值异常导致热循环），上限 2h（防头部异常把阶段挂死）。
fn rate_limit_retry_at(resp: &reqwest::Response) -> i64 {
    let now = now_unix();
    let mut at = now + 30;
    if let Some(v) = resp
        .headers()
        .get("x-ratelimit-reset")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<i64>().ok())
    {
        at = at.max(v + 5);
    }
    if let Some(v) = resp
        .headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<i64>().ok())
    {
        at = at.max(now + v + 5);
    }
    at.min(now + 7200)
}

/// GET 一个 URL，带请求级超时与响应大小上限。成功返回原始字节。
pub async fn fetch_bytes(
    client: &reqwest::Client,
    url: &str,
    max_bytes: usize,
    timeout: Duration,
    github_api: bool,
) -> Result<Vec<u8>, Failure> {
    let mut rb = client.get(url).timeout(timeout);
    if github_api {
        rb = apply_github_headers(rb, github_token().as_deref());
    }
    let resp = rb.send().await.map_err(|e| {
        // 连接/超时类错误：短退避后可重试
        Failure::delayed(format!("GET {url}: {e}"), 120)
    })?;

    let status = resp.status();
    if status == reqwest::StatusCode::FORBIDDEN || status == reqwest::StatusCode::TOO_MANY_REQUESTS
    {
        return Err(Failure {
            message: format!("GET {url}: HTTP {status}（限流）"),
            retry_at: rate_limit_retry_at(&resp),
        });
    }
    if status == reqwest::StatusCode::NOT_FOUND {
        return Err(Failure::not_found(url));
    }
    if !status.is_success() {
        return Err(Failure::delayed(format!("GET {url}: HTTP {status}"), 300));
    }
    // 双重上限：先看声明的 Content-Length（快速拒绝），读完后再看实际长度
    //（有些源不带 Content-Length，不能只信声明）。
    if let Some(len) = resp.content_length() {
        if len as usize > max_bytes {
            return Err(Failure::delayed(
                format!("GET {url}: 响应 {len} 字节超上限 {max_bytes}"),
                600,
            ));
        }
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| Failure::delayed(format!("GET {url}: 读体失败 {e}"), 120))?;
    if bytes.len() > max_bytes {
        return Err(Failure::delayed(
            format!("GET {url}: 响应 {} 字节超上限 {max_bytes}", bytes.len()),
            600,
        ));
    }
    Ok(bytes.to_vec())
}

/// GET 并解析 JSON。大小上限在字节层已把守（3.5MB 的 dist / 79KB 的
/// registry 都远小于 16MB 上限，反序列化内存占用有界）。
pub async fn fetch_json<T: serde::de::DeserializeOwned>(
    client: &reqwest::Client,
    url: &str,
    max_bytes: usize,
    timeout: Duration,
    github_api: bool,
) -> Result<T, Failure> {
    let bytes = fetch_bytes(client, url, max_bytes, timeout, github_api).await?;
    serde_json::from_slice(&bytes)
        .map_err(|e| Failure::delayed(format!("GET {url}: JSON 解析失败 {e}"), 600))
}

/// npm registry 包 URL。scoped 包的 `/` 必须转义成 %2F（npm 文档口径，
/// 未转义的路径对部分代理/缓存层有歧义）。
pub fn npm_registry_url(name: &str) -> String {
    format!("https://registry.npmjs.org/{}", name.replace('/', "%2f"))
}

/// npm registry 文档的裁剪形状：只取 dist-tags / time / 各版本两个字段。
///
/// 完整文档对热门包可达数 MB；typed struct 让 serde 只保留需要的键，
/// versions 多也不会把内存撑大。
#[derive(Deserialize, Debug, Default)]
pub struct NpmDoc {
    #[serde(default, rename = "dist-tags")]
    pub dist_tags: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub time: std::collections::BTreeMap<String, String>,
    /// 顶层 repository（npm 多数包在这里声明；旧版本文档也常在此处）。
    #[serde(default)]
    pub repository: Option<serde_json::Value>,
    #[serde(default)]
    pub versions: std::collections::BTreeMap<String, NpmVersion>,
}

/// npm 版本清单的裁剪形状。
#[derive(Deserialize, Debug, Default)]
pub struct NpmVersion {
    #[serde(default)]
    pub repository: Option<serde_json::Value>,
    #[serde(default)]
    pub description: Option<String>,
}

impl NpmDoc {
    /// latest 版本号；dist-tags 缺失时回退字典序最大版本。
    pub fn latest_version(&self) -> Option<&str> {
        if let Some(v) = self.dist_tags.get("latest") {
            if !v.is_empty() {
                return Some(v);
            }
        }
        self.versions.keys().next_back().map(|s| s.as_str())
    }

    /// latest 版本的发布时刻（unix 秒）。
    pub fn latest_time(&self) -> Option<i64> {
        let v = self.latest_version()?;
        self.time.get(v).and_then(|t| iso_to_unix(t))
    }

    /// latest 版本的仓库声明（string 或 {url} 两种形态都收）。
    /// latest 版本的仓库声明（string 或 {url} 两种形态都收）。
    ///
    /// 优先版本级 `versions[latest].repository`（更精确），缺失时回退
    /// 顶层 `repository`（npm 常见形态：repository 只在包级声明一次）。
    pub fn latest_repository(&self) -> Option<serde_json::Value> {
        self.versions
            .get(self.latest_version()?)
            .and_then(|v| v.repository.clone())
            .or_else(|| self.repository.clone())
    }

    /// latest 版本的 description。
    pub fn latest_description(&self) -> Option<&str> {
        self.versions
            .get(self.latest_version()?)
            .and_then(|v| v.description.as_deref())
    }
}

/// 有界并发 map：`limit` 个并发，返回**完成顺序**的结果表（None = 任务
/// 失败/panic）。
///
/// **结果顺序不等于输入顺序**（`JoinSet::join_next` 按完成先后返回）。
/// 调用方必须让闭包自己带上输入下标（`(i, item)` → `(i, result)`），再按
/// 下标回填；本模块三个调用点（`market` 的 npm 版本回填、`github` 的取证与
/// stars）都是这个形态。旧注释写作「结果按输入顺序回填」，与实现不符——按
/// 那句话写下标无关的调用会静默把 A 仓的结果写给 B 条。
///
/// 用 Semaphore 排队而不是无限 spawn：GitHub 深扫（每仓最多 17 个请求）×
/// 一次搜索可达 1000 仓，无界并发会瞬间把自己打进限流。
pub async fn map_limited<T, R, F, Fut>(limit: usize, items: Vec<T>, f: F) -> Vec<Option<R>>
where
    T: Send + 'static,
    R: Send + 'static,
    F: Fn(T) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = R> + Send + 'static,
{
    use tokio::sync::Semaphore;
    use tokio::task::JoinSet;

    let sem = std::sync::Arc::new(Semaphore::new(limit));
    let f = std::sync::Arc::new(f);
    let mut set = JoinSet::new();
    for item in items {
        let sem = sem.clone();
        let f = f.clone();
        set.spawn(async move {
            let _permit = sem.acquire_owned().await;
            f(item).await
        });
    }
    let mut out: Vec<Option<R>> = Vec::new();
    while let Some(res) = set.join_next().await {
        // JoinError（panic/取消）→ None；R 本身可为 Option<X>（调用方自行 flatten）
        out.push(res.ok());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn npm_url_encodes_scoped_names() {
        assert_eq!(
            npm_registry_url("@magpie-community/kiro"),
            "https://registry.npmjs.org/@magpie-community%2fkiro"
        );
        assert_eq!(
            npm_registry_url("plain-pkg"),
            "https://registry.npmjs.org/plain-pkg"
        );
    }

    #[test]
    fn iso_time_to_unix() {
        assert_eq!(iso_to_unix("2026-11-11T08:19:01.838Z"), Some(1_794_385_141));
        assert_eq!(iso_to_unix("not-a-time"), None);
    }

    #[test]
    fn failure_delayed_uses_now() {
        let f = Failure::delayed("boom", 60);
        assert!(f.retry_at > now_unix());
        assert_eq!(f.message, "boom");
    }

    #[test]
    fn npm_doc_latest_helpers() {
        // 真实 npm 文档形态：repository 在**顶层**声明一次（实测多数包如此），
        // 版本级只带 description；另有一条只有旧版本带 repository 的变体。
        let doc: NpmDoc = serde_json::from_str(
            r#"{
                "dist-tags": {"latest": "0.2.0"},
                "time": {"0.1.0": "2026-11-10T00:00:00.000Z", "0.2.0": "2026-11-11T08:19:01.838Z", "created": "2026-11-10T00:00:00.000Z"},
                "repository": {"url": "git+https://github.com/owner/repo.git"},
                "versions": {
                    "0.1.0": {"repository": {"url": "git+https://github.com/owner/repo.git"}},
                    "0.2.0": {"description": "hello"}
                }
            }"#,
        )
        .unwrap();
        assert_eq!(doc.latest_version(), Some("0.2.0"));
        assert_eq!(doc.latest_time(), Some(1_794_385_141));
        assert_eq!(doc.latest_description(), Some("hello"));
        assert!(
            doc.latest_repository().is_some(),
            "顶层 repository 应作为回退"
        );

        // 只有版本级声明时也能取到
        let doc2: NpmDoc = serde_json::from_str(
            r#"{
                "dist-tags": {"latest": "1.0.0"},
                "versions": {"1.0.0": {"repository": "github:owner/repo"}}
            }"#,
        )
        .unwrap();
        assert_eq!(
            doc2.latest_repository(),
            Some(serde_json::json!("github:owner/repo"))
        );
    }
}
