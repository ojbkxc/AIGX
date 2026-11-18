use parking_lot::RwLock;
use rand::seq::SliceRandom;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::storage::FileStore;

// ── Data structures ──────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CfAccount {
    pub id: String,
    pub name: String,
    pub account_id: String,
    pub api_token: String,
    #[serde(default = "default_status")]
    pub status: String, // "active", "error", "pending"
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub last_used_at: Option<i64>,
    #[serde(default)]
    pub created_at: i64,
}

fn default_status() -> String {
    "active".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestResult {
    pub success: bool,
    pub models: bool,
    pub inference: bool,
    pub analytics: bool,
    pub message: String,
}

// ── AccountPool ──────────────────────────────────────────────────────

/// 账号池 - 管理多个 CF 账号，支持负载均衡
pub struct AccountPool {
    accounts: RwLock<Vec<CfAccount>>,
    store: Arc<FileStore>,
    /// SSRF 逃生阀快照（`config.usage.allow_internal_upstream`，启动时由
    /// main 注入）。false（默认）时账号 Worker 地址不得指向内网/环回/
    /// 云元数据地址——账号 `account_id` 由管理员填写、网关会主动外发请求。
    allow_internal_upstream: AtomicBool,
}

/// 解析账号 Worker 地址（补 scheme）并做 SSRF 出站校验。
///
/// `account_id` 即 cf-ai-gw Worker 地址；兼容裸域名/本地地址：
/// 127.0.0.1 / localhost / ::1 视为 http://，其余裸域名默认补 https://。
///
/// 校验策略与渠道 `base_url` 同口径（`net_guard::validate_outbound_url`）：
/// 拒绝内网/环回/链路本地/云元数据等保留地址；`allow_internal=true`
/// （逃生阀）时跳过内网检查但仍校验协议与可解析性。
pub(crate) fn resolve_worker_url(
    account_id: &str,
    allow_internal: bool,
) -> Result<String, crate::net_guard::UrlRejection> {
    let mut base_url = account_id.trim().trim_end_matches('/').to_string();
    if !base_url.starts_with("http://") && !base_url.starts_with("https://") {
        let is_localhost = base_url.starts_with("127.0.0.1")
            || base_url.starts_with("localhost")
            || base_url.starts_with("::1");
        base_url = if is_localhost {
            format!("http://{base_url}")
        } else {
            format!("https://{base_url}")
        };
    }
    crate::net_guard::validate_outbound_url(&base_url, allow_internal)?;
    Ok(base_url)
}

impl AccountPool {
    pub fn new(store: Arc<FileStore>) -> Self {
        let pool = Self {
            accounts: RwLock::new(Vec::new()),
            store,
            allow_internal_upstream: AtomicBool::new(false),
        };
        let _ = pool.load();
        pool
    }

    /// 注入 SSRF 逃生阀快照（启动时由 main 按
    /// `config.usage.allow_internal_upstream` 调用一次）。
    pub fn set_allow_internal_upstream(&self, allow: bool) {
        self.allow_internal_upstream.store(allow, Ordering::Relaxed);
    }

    /// 当前 SSRF 逃生阀状态（false = 拒绝内网/环回/元数据地址）。
    pub fn allow_internal_upstream(&self) -> bool {
        self.allow_internal_upstream.load(Ordering::Relaxed)
    }

    /// 从存储加载账号
    pub fn load(&self) -> anyhow::Result<()> {
        let keys = self.store.list("account:")?;
        let mut accounts = Vec::with_capacity(keys.len());
        for key in &keys {
            if let Some(account) = self.store.get::<CfAccount>(key)? {
                accounts.push(account);
            }
        }
        // 按创建时间排序
        accounts.sort_by_key(|b| std::cmp::Reverse(b.created_at));
        *self.accounts.write() = accounts;
        Ok(())
    }

    /// 保存账号到存储
    #[allow(dead_code)]
    pub fn save(&self) -> anyhow::Result<()> {
        let accounts = self.accounts.read();
        for account in accounts.iter() {
            let key = format!("account:{}", account.id);
            self.store.put(&key, account)?;
        }
        Ok(())
    }

    /// 获取所有账号
    pub fn list(&self) -> Vec<CfAccount> {
        self.accounts.read().clone()
    }

    /// 获取活跃账号列表
    pub fn active_accounts(&self) -> Vec<CfAccount> {
        self.accounts
            .read()
            .iter()
            .filter(|a| a.status == "active")
            .cloned()
            .collect()
    }

    /// 获取一个可用账号（空闲最久优先，最大化每次使用间隔，绕免费 API 会话级限速）
    #[allow(dead_code)] // 预留/线表符号（见 main.rs 债务清单）
    pub fn acquire(&self) -> Option<CfAccount> {
        let now = chrono::Utc::now().timestamp();
        self.accounts
            .read()
            .iter()
            .filter(|a| a.status == "active")
            .max_by_key(|a| {
                // 从未使用过（last_used_at=None）视为最早，优先选择；
                // 使用时间戳越小（越久未用）优先级越高。
                a.last_used_at.map(|t| now - t).unwrap_or(i64::MAX)
            })
            .cloned()
    }

    /// 获取一个可用账号（随机负载均衡——遗留方法，新增调用方请用 `acquire` 空闲优先）
    #[allow(dead_code)]
    pub fn acquire_random(&self) -> Option<CfAccount> {
        let active: Vec<CfAccount> = self
            .accounts
            .read()
            .iter()
            .filter(|a| a.status == "active")
            .cloned()
            .collect();
        active.choose(&mut rand::thread_rng()).cloned()
    }

    /// 添加账号
    pub fn add(&self, account: CfAccount) -> anyhow::Result<()> {
        let key = format!("account:{}", account.id);
        self.store.put(&key, &account)?;
        self.accounts.write().push(account);
        Ok(())
    }

    /// 更新账号
    pub fn update(&self, id: &str, account: CfAccount) -> anyhow::Result<()> {
        let key = format!("account:{}", id);
        self.store.put(&key, &account)?;
        let mut accounts = self.accounts.write();
        if let Some(pos) = accounts.iter().position(|a| a.id == id) {
            accounts[pos] = account;
        }
        Ok(())
    }

    /// 获取底层存储引用（用于 ModelMapper 等组件）
    #[allow(dead_code)]
    pub fn store(&self) -> &Arc<FileStore> {
        &self.store
    }

    /// 删除账号
    pub fn remove(&self, id: &str) -> anyhow::Result<()> {
        let key = format!("account:{}", id);
        self.store.delete(&key)?;
        self.accounts.write().retain(|a| a.id != id);
        Ok(())
    }

    /// 更新账号状态
    #[allow(dead_code)]
    pub fn set_status(&self, id: &str, status: &str, error: Option<String>) {
        let mut accounts = self.accounts.write();
        if let Some(account) = accounts.iter_mut().find(|a| a.id == id) {
            account.status = status.to_string();
            account.last_error = error;
        }
    }

    /// 标记账号已使用（更新 last_used_at）
    pub fn mark_used(&self, id: &str) {
        let mut accounts = self.accounts.write();
        if let Some(account) = accounts.iter_mut().find(|a| a.id == id) {
            account.last_used_at = Some(chrono::Utc::now().timestamp());
        }
    }

    /// 测试账号连接。账号在 Binding 架构下即 cf-ai-gw Worker 部署：
    /// `account_id` 为 Worker 地址，`api_token` 为 Worker API Key（可空）。
    /// 向 Worker 发送 2 个测试请求（OpenAI 兼容端点）：
    ///
    /// 1. `GET /v1/models` — 验证 Worker 可达、代理鉴权通过（如启用）
    /// 2. `POST /v1/chat/completions` — 验证 AI Binding 推理可用
    pub async fn test(&self, account: &CfAccount) -> Result<TestResult, String> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| format!("Failed to create HTTP client: {}", e))?;

        // account_id 即 cf-ai-gw Worker 地址；兼容裸域名/本地地址。
        // P0 SSRF：发送前经 net_guard 校验（逃生阀随账号池快照）。
        let base_url = resolve_worker_url(&account.account_id, self.allow_internal_upstream())
            .map_err(|rej| rej.message())?;

        let build_req = |client: &reqwest::Client, url: &str| {
            let mut req = client.get(url);
            if !account.api_token.trim().is_empty() {
                req = req.bearer_auth(account.api_token.trim());
            }
            req
        };

        // ── Test 1: GET /v1/models — Worker 可达性 + 代理鉴权 ──────────
        let models_ok = match build_req(&client, &format!("{base_url}/v1/models"))
            .send()
            .await
        {
            Ok(resp) => {
                let status = resp.status();
                // 401/403 表示鉴权未通过；其余可达性（含 404 端点差异）判失败
                if status == reqwest::StatusCode::UNAUTHORIZED
                    || status == reqwest::StatusCode::FORBIDDEN
                {
                    return Err(format!(
                        "Proxy auth failed (HTTP {status}) — check API Key / cf-ai-gw proxy auth"
                    ));
                }
                status.is_success()
            }
            Err(e) => {
                return Err(format!("Worker /v1/models test failed: {e}"));
            }
        };

        // ── Test 2: POST /v1/chat/completions — AI Binding 推理可用 ────
        let inference_ok = match client
            .post(format!("{base_url}/v1/chat/completions"))
            .bearer_auth(account.api_token.trim())
            .json(&serde_json::json!({
                "model": "@cf/baai/bge-base-en-v1.5",
                "messages": [{"role": "user", "content": "test"}],
                "max_tokens": 8,
            }))
            .send()
            .await
        {
            Ok(resp) => {
                let status = resp.status();
                // 429 (rate limited) 也算通过 — 说明 Worker 可达且已受理
                status.is_success() || status == 429
            }
            Err(e) => {
                return Err(format!("Inference API test failed: {e}"));
            }
        };

        // Binding 架构下无独立 GraphQL 用量分析端点；analytics 字段保留
        // 为 true 以兼容前端，实际用量由 AIGX 本地 usage_tracker 统计。
        let analytics_ok = true;

        let success = models_ok && inference_ok;
        Ok(TestResult {
            success,
            models: models_ok,
            inference: inference_ok,
            analytics: analytics_ok,
            message: format!(
                "Models: {}, Inference: {}, Analytics: {}",
                if models_ok { "OK" } else { "FAIL" },
                if inference_ok { "OK" } else { "FAIL" },
                if analytics_ok { "OK" } else { "FAIL" },
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_test_account() -> CfAccount {
        CfAccount {
            id: "test-1".to_string(),
            name: "Test Account".to_string(),
            account_id: "test-account-id".to_string(),
            api_token: "test-token".to_string(),
            status: "active".to_string(),
            last_error: None,
            last_used_at: None,
            created_at: 1_000_000,
        }
    }

    #[test]
    fn test_add_and_list() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(FileStore::new(dir.path().to_path_buf()));
        let pool = AccountPool::new(store);

        let account = create_test_account();
        pool.add(account.clone()).unwrap();

        let accounts = pool.list();
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].id, "test-1");
    }

    #[test]
    fn test_active_accounts() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(FileStore::new(dir.path().to_path_buf()));
        let pool = AccountPool::new(store);

        let mut active = create_test_account();
        active.id = "active-1".to_string();
        active.status = "active".to_string();

        let mut error = create_test_account();
        error.id = "error-1".to_string();
        error.status = "error".to_string();

        pool.add(active.clone()).unwrap();
        pool.add(error.clone()).unwrap();

        let active_list = pool.active_accounts();
        assert_eq!(active_list.len(), 1);
        assert_eq!(active_list[0].id, "active-1");
    }

    #[test]
    fn test_acquire() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(FileStore::new(dir.path().to_path_buf()));
        let pool = AccountPool::new(store);

        let account = create_test_account();
        pool.add(account.clone()).unwrap();

        let acquired = pool.acquire();
        assert!(acquired.is_some());
        assert_eq!(acquired.unwrap().id, "test-1");
    }

    #[test]
    fn test_acquire_idle_longest_first() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(FileStore::new(dir.path().to_path_buf()));
        let pool = AccountPool::new(store);

        let mut a1 = create_test_account();
        a1.id = "idle-1".to_string();
        a1.last_used_at = Some(1_000_000); // 很久前使用

        let mut a2 = create_test_account();
        a2.id = "idle-2".to_string();
        a2.last_used_at = Some(2_000_000); // 较近使用

        let mut a3 = create_test_account();
        a3.id = "idle-3".to_string();
        a3.last_used_at = None; // 从未使用，应最先被选中

        pool.add(a1).unwrap();
        pool.add(a2).unwrap();
        pool.add(a3).unwrap();

        // 从未使用过的最优先
        let first = pool.acquire().unwrap();
        assert_eq!(first.id, "idle-3");
        pool.mark_used("idle-3");

        // 空闲最久的 idle-1 其次
        let second = pool.acquire().unwrap();
        assert_eq!(second.id, "idle-1");
    }

    #[test]
    fn test_acquire_no_active() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(FileStore::new(dir.path().to_path_buf()));
        let pool = AccountPool::new(store);

        let acquired = pool.acquire();
        assert!(acquired.is_none());
    }

    #[test]
    fn test_remove() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(FileStore::new(dir.path().to_path_buf()));
        let pool = AccountPool::new(store);

        let account = create_test_account();
        pool.add(account).unwrap();
        assert_eq!(pool.list().len(), 1);

        pool.remove("test-1").unwrap();
        assert_eq!(pool.list().len(), 0);
    }

    #[test]
    fn test_set_status() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(FileStore::new(dir.path().to_path_buf()));
        let pool = AccountPool::new(store);

        let account = create_test_account();
        pool.add(account).unwrap();

        pool.set_status("test-1", "error", Some("test error".to_string()));
        let accounts = pool.list();
        assert_eq!(accounts[0].status, "error");
        assert_eq!(accounts[0].last_error, Some("test error".to_string()));
    }

    #[test]
    fn test_update() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(FileStore::new(dir.path().to_path_buf()));
        let pool = AccountPool::new(store);

        let account = create_test_account();
        pool.add(account).unwrap();

        let mut updated = create_test_account();
        updated.name = "Updated Name".to_string();
        pool.update("test-1", updated).unwrap();

        let accounts = pool.list();
        assert_eq!(accounts[0].name, "Updated Name");
    }

    // ── P0 SSRF 接线（resolve_worker_url）────────────────────────────

    /// 默认（逃生阀关闭）：内网/环回/元数据地址一律拒绝。
    #[test]
    fn resolve_worker_url_rejects_internal_by_default() {
        for account_id in [
            "127.0.0.1:11434/v1",
            "localhost:3000",
            "169.254.169.254/latest/meta-data/",
            "10.0.0.5",
            "192.168.1.10:8000",
        ] {
            assert!(
                resolve_worker_url(account_id, false).is_err(),
                "{account_id} 应被 SSRF 校验拒绝"
            );
        }
    }

    /// 逃生阀打开（allow_internal_upstream=true）：内网地址放行。
    #[test]
    fn resolve_worker_url_escape_hatch() {
        assert_eq!(
            resolve_worker_url("127.0.0.1:11434/v1", true).unwrap(),
            "http://127.0.0.1:11434/v1"
        );
        assert_eq!(
            resolve_worker_url("localhost:3000", true).unwrap(),
            "http://localhost:3000"
        );
    }

    /// 裸域名/公网字面量：补 scheme 且通过校验（字面量 IP 不做 DNS 查询）。
    #[test]
    fn resolve_worker_url_bare_domain_gets_https() {
        assert_eq!(
            resolve_worker_url("1.1.1.1/v1", false).unwrap(),
            "https://1.1.1.1/v1"
        );
        // 已带 scheme 的原样保留（仅去尾斜杠）
        assert_eq!(
            resolve_worker_url("https://8.8.8.8:8080/", false).unwrap(),
            "https://8.8.8.8:8080"
        );
    }

    /// 账号池逃生阀快照：setter/getter 往返。
    #[test]
    fn pool_allow_internal_flag_roundtrip() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(FileStore::new(dir.path().to_path_buf()));
        let pool = AccountPool::new(store);
        assert!(!pool.allow_internal_upstream(), "默认必须拒绝内网");
        pool.set_allow_internal_upstream(true);
        assert!(pool.allow_internal_upstream());
    }
}
