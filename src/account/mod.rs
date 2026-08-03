use std::sync::Arc;
use parking_lot::RwLock;
use rand::seq::SliceRandom;
use serde::{Deserialize, Serialize};

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
}

impl AccountPool {
    pub fn new(store: Arc<FileStore>) -> Self {
        let pool = Self {
            accounts: RwLock::new(Vec::new()),
            store,
        };
        let _ = pool.load();
        pool
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
        accounts.sort_by(|a, b| b.created_at.cmp(&a.created_at));
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

    /// 获取一个可用账号（随机负载均衡）
    #[allow(dead_code)]
    pub fn acquire(&self) -> Option<CfAccount> {
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

    /// 测试账号连接。向 Cloudflare API 发送 3 个测试请求：
    ///
    /// 1. `GET /accounts/{id}/ai/models/search?per_page=1` — 验证 Workers AI Read 权限
    /// 2. `POST /accounts/{id}/ai/run/@cf/baai/bge-base-en-v1.5` — 验证 Workers AI Edit 权限
    /// 3. GraphQL 查询 — 验证 Account Analytics Read 权限
    pub async fn test(&self, account: &CfAccount) -> Result<TestResult, String> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| format!("Failed to create HTTP client: {}", e))?;

        let base_url = format!(
            "https://api.cloudflare.com/client/v4/accounts/{}",
            account.account_id
        );
        let auth_header = format!("Bearer {}", account.api_token);

        // ── Test 1: Workers AI — 搜索模型 ──────────────────────────────
        let models_ok = match client
            .get(format!("{}/ai/models/search?per_page=1", base_url))
            .header("Authorization", &auth_header)
            .send()
            .await
        {
            Ok(resp) => resp.status().is_success(),
            Err(e) => {
                return Err(format!("Models API test failed: {}", e));
            }
        };

        // ── Test 2: Workers AI — 运行推理 ──────────────────────────────
        let inference_ok = match client
            .post(format!(
                "{}/ai/run/@cf/baai/bge-base-en-v1.5",
                base_url
            ))
            .header("Authorization", &auth_header)
            .json(&serde_json::json!({ "text": "test"}))
            .send()
            .await
        {
            Ok(resp) => {
                let status = resp.status();
                // 429 (rate limited) 也算通过 — 说明 API 可达
                status.is_success() || status == 429
            }
            Err(e) => {
                return Err(format!("Inference API test failed: {}", e));
            }
        };

        // ── Test 3: GraphQL — 查询用量分析 ─────────────────────────────
        let analytics_query = format!(
            "{{ viewer {{ accounts(filter: {{ accountTag: \"{}\" }}) {{ aiWorkersUsageAdaptiveGroups(limit: 1) {{ dimensions {{ modelId }} }} }} }} }}",
            account.account_id
        );
        let analytics_ok = match client
            .post("https://api.cloudflare.com/client/v4/graphql")
            .header("Authorization", &auth_header)
            .json(&serde_json::json!({ "query": analytics_query }))
            .send()
            .await
        {
            Ok(resp) => resp.status().is_success(),
            Err(e) => {
                return Err(format!("Analytics GraphQL test failed: {}", e));
            }
        };

        let success = models_ok && inference_ok && analytics_ok;
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
}