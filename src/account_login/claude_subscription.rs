//! Claude 订阅登录：**关联本机已登录的官方 Claude Code**。
//!
//! 对齐 Agent2API `claude_subscription/accounts.rs`：
//! - 只读取官方 profile（`~/.claude.json` 的 `oauthAccount`），**不读**
//!   `.credentials.json` / 系统钥匙串、不复制、不回传订阅令牌；
//! - 落库的是「本机引用」（kind / 可执行文件 / profilePath / accountKey …），
//!   续期仍归官方 CLI 所有。
//!
//! ## 与上游的差异
//! - 上游会在关联时跑 `claude auth status --json` 复核登录态与套餐；AIGX
//!   当前没有受控 CLI 子进程层，这里只做 profile 归属校验（读 `oauthAccount`），
//!   `plan` / `cliVersion` 留空 —— 不伪造。CLI 复核与真正的对话转发见报告
//!   待接线清单（`claude_cli_required`）。
//! - AIGX 的登录引擎没有「表单参数」入口，这里固定读取当前系统用户的默认
//!   profile（`accountSource = current`）；指定其它配置目录需前端/路由接线。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::{Credentials, LoginFlow, Started};

/// 账号引用类型（与上游 `Link.kind` 逐字一致）。
const LINK_KIND: &str = "claude-code-link-v1";

/// 一次「关联官方登录态」流程的结果缓存。
pub struct ClaudeLinkFlow {
    result: Mutex<Option<Result<Credentials, String>>>,
    fields: Mutex<HashMap<String, String>>,
}

/// 当前用户 home 目录（`HOME` / Windows `USERPROFILE`）。
fn home_dir() -> Result<std::path::PathBuf, String> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(std::path::PathBuf::from)
        .ok_or_else(|| "无法定位当前用户 home 目录".to_string())
}

/// 官方 CLI 可执行文件：优先环境变量，否则退回 PATH 中的 `claude`。
fn executable() -> String {
    std::env::var("CLAUDE_CODE_EXECUTABLE")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "claude".to_string())
}

fn read_profile(path: &std::path::Path) -> Result<Value, String> {
    let content = std::fs::read_to_string(path).map_err(|_| {
        format!(
            "未找到官方 Claude 账号资料（{}）；请先在官方 CLI 执行 claude auth login",
            path.display()
        )
    })?;
    serde_json::from_str::<Value>(&content)
        .map_err(|_| "官方 Claude 账号资料格式不正确（未回显文件内容）".to_string())
}

fn field_str(account: &Value, key: &str) -> String {
    account
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string()
}

/// 关联一次：读 profile → 组装本机引用 → 产出凭证载荷。
fn link() -> Result<(Credentials, HashMap<String, String>), String> {
    let home = home_dir()?;
    let profile_path = home.join(".claude.json");
    let profile = read_profile(&profile_path)?;
    let account = profile
        .get("oauthAccount")
        .filter(|value| value.is_object())
        .cloned()
        .unwrap_or(Value::Null);
    let account_uuid = field_str(&account, "accountUuid");
    let organization_uuid = field_str(&account, "organizationUuid");
    let email = field_str(&account, "emailAddress");
    let organization = field_str(&account, "organizationName");
    let who = if account_uuid.is_empty() {
        email.to_ascii_lowercase()
    } else {
        account_uuid.clone()
    };
    if who.is_empty() {
        return Err("所选目录没有官方 Claude 订阅账号，请先在官方 CLI 完成登录".to_string());
    }
    let account_key = format!(
        "{:x}",
        Sha256::digest(format!("{who}\0{organization_uuid}").as_bytes())
    );
    let profile_path_str = profile_path.to_string_lossy().to_string();
    let executable = executable();

    let mut fields = HashMap::new();
    fields.insert("kind".to_string(), LINK_KIND.to_string());
    fields.insert("executable".to_string(), executable.clone());
    // 默认 profile：configDir 留空（与上游 `accountSource=current` 同义）。
    fields.insert("configDir".to_string(), String::new());
    fields.insert("profilePath".to_string(), profile_path_str.clone());
    fields.insert("accountKey".to_string(), account_key.clone());
    fields.insert("email".to_string(), email.clone());
    fields.insert("organization".to_string(), organization.clone());
    fields.insert("plan".to_string(), String::new());
    fields.insert("cliVersion".to_string(), String::new());

    let secret = json!({
        "kind": LINK_KIND,
        "executable": executable.clone(),
        "configDir": Value::Null,
        "profilePath": profile_path_str.clone(),
        "accountKey": account_key.clone(),
        "email": email.clone(),
        "organization": organization.clone(),
        "plan": "",
        "cliVersion": "",
    })
    .to_string();
    let name = if email.is_empty() {
        "Claude Code".to_string()
    } else {
        email
    };
    let credentials = Credentials {
        name,
        secret,
        base_url: None,
        extra_headers: HashMap::new(),
        expires_at: None,
    };
    Ok((credentials, fields))
}

#[async_trait]
impl LoginFlow for ClaudeLinkFlow {
    async fn poll(&self) -> Result<Option<Credentials>, String> {
        {
            let cached = self.result.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(result) = &*cached {
                return result.clone().map(Some);
            }
        }
        match link() {
            Ok((credentials, fields)) => {
                *self.fields.lock().unwrap_or_else(|e| e.into_inner()) = fields;
                *self.result.lock().unwrap_or_else(|e| e.into_inner()) =
                    Some(Ok(credentials.clone()));
                Ok(Some(credentials))
            }
            Err(message) => {
                *self.result.lock().unwrap_or_else(|e| e.into_inner()) = Some(Err(message.clone()));
                Err(message)
            }
        }
    }

    fn poll_interval(&self) -> Duration {
        // 一次读取即可；首轮 poll 就返回结果。
        Duration::from_millis(300)
    }

    fn fatal(&self, _error: &str) -> bool {
        // 「找不到官方 profile / 未登录」不是瞬时错误，重试 5 分钟没有意义。
        true
    }

    fn extra_fields(&self) -> HashMap<String, String> {
        self.fields
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

/// 发起 Claude 订阅关联。
pub async fn start(_client: &reqwest::Client) -> Result<Started, String> {
    Ok(Started {
        state: uuid::Uuid::new_v4().simple().to_string(),
        // 本机关联不需要浏览器跳转。
        auth_url: String::new(),
        flow: Arc::new(ClaudeLinkFlow {
            result: Mutex::new(None),
            fields: Mutex::new(HashMap::new()),
        }),
    })
}
