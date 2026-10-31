//! 登录态供应商的「网页登录」引擎（服务端无头登录 + 任务表）。
//!
//! 移植自 Agent2API 的 `core/login`：发起登录 → 返回授权地址 → 用户在浏览器里
//! 完成登录 → 网关在后台轮询/收回调拿到凭证 → 自动落成账号池里的一条账号。
//!
//! ── 与 Agent2API 的差异 ──────────────────────────────────────
//! AIGX 的账号必须挂在 `channel_type = account` 的渠道下，因此每个登录任务
//! 都带一个 `channel_id`（没传时按 provider 找同名渠道，找不到就新建一个）。
//! 登录成功后的落库走 `AccountSessionStore::upsert`，凭证统一进
//! `Credential.secret`（登录态型 = `CREDENTIAL_KIND_SESSION`）。
//!
//! ── 任务表 ──────────────────────────────────────────────────
//! `state` 是每次发起的不可预测随机串（uuid v4）。前端拿它查 `/wait`；
//! 取消/超时/完成后保留 10 分钟，取任务时顺手清过期项。

mod accio;
mod antigravity;
mod catpaw;
mod cline;
pub mod loomy;
mod qoder;
mod raccoon;
mod trae;
mod workbuddy;
mod zcode;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::account_session::model::{AccountSession, Credential, CREDENTIAL_KIND_SESSION};
use crate::account_session::registry;
use crate::account_session::AccountSessionStore;
use crate::api::openai::AppState;
use crate::channel::{Channel, ChannelStore, ChannelType};

/// 轮询间隔（多数家的折中值；各家可在 `LoginFlow::poll_interval` 覆盖）。
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(2);
/// 单次登录总超时。
pub const LOGIN_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// 任务完成后保留时长。
const TASK_RETENTION: Duration = Duration::from_secs(10 * 60);

/// 登录成功后的凭证载荷（与具体供应商无关）。
#[derive(Debug, Clone, Default)]
pub struct Credentials {
    pub name: String,
    /// 落库的凭证（登录态型：Cookie / JWT / AK:SK:STS 等）
    pub secret: String,
    /// 账号级上游地址（空 = 用 provider 默认）
    pub base_url: Option<String>,
    pub extra_headers: HashMap<String, String>,
    pub expires_at: Option<i64>,
}

/// 一个进行中的登录流程。
///
/// `double_must_use`：`async_trait` 宏给每个方法的返回值包一层 pinned future
/// （本身已是 must_use），再叠加一层 `#[must_use]` 会被 clippy 判为重复。
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait LoginFlow: Send + Sync {
    /// 轮询一次：`Ok(None)` = 还没完成，继续等；`Ok(Some(_))` = 拿到凭证。
    async fn poll(&self) -> Result<Option<Credentials>, String>;

    /// 网页端手工粘贴回调地址（浏览器到不了网关 loopback 时的兜底）。
    async fn submit_callback(&self, _url: &str) -> Result<Option<Credentials>, String> {
        Err("该供应商的网页登录不支持回调地址回填".to_string())
    }

    fn poll_interval(&self) -> Duration {
        DEFAULT_POLL_INTERVAL
    }
}

/// 发起结果：授权地址 + 后续轮询句柄。
pub struct Started {
    pub state: String,
    pub auth_url: String,
    pub flow: Arc<dyn LoginFlow>,
}

/// 一次登录任务的状态。
#[derive(Clone)]
pub struct LoginTask {
    pub state: String,
    pub auth_url: String,
    pub provider: String,
    pub channel_id: String,
    pub done: bool,
    pub error: Option<String>,
    pub account_id: Option<String>,
    pub canceled: bool,
    finished_at: Option<Instant>,
    /// 本任务的登录流程（回调型流程要在提交回调地址时复用发起时的上下文）。
    flow: Option<Arc<dyn LoginFlow>>,
}

type Handle = Arc<Mutex<LoginTask>>;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn table() -> &'static Mutex<HashMap<String, Handle>> {
    static TASKS: OnceLock<Mutex<HashMap<String, Handle>>> = OnceLock::new();
    TASKS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn sweep(tasks: &mut HashMap<String, Handle>) {
    let now = Instant::now();
    tasks.retain(|_, handle| {
        let task = lock(handle);
        match task.finished_at {
            Some(finished) => now.duration_since(finished) < TASK_RETENTION,
            None => true,
        }
    });
}

fn register(task: Handle) -> Handle {
    let state = lock(&task).state.clone();
    let mut tasks = lock(table());
    sweep(&mut tasks);
    tasks.insert(state, task.clone());
    task
}

/// 取任务快照（含过期清理）。
pub fn snapshot(state: &str) -> Option<LoginTask> {
    if state.trim().is_empty() {
        return None;
    }
    let mut tasks = lock(table());
    sweep(&mut tasks);
    tasks.get(state).map(|handle| lock(handle).clone())
}

fn finish(handle: &Handle, account_id: Option<String>, error: Option<String>) {
    let mut task = lock(handle);
    task.done = true;
    task.account_id = account_id;
    task.error = error;
    task.finished_at = Some(Instant::now());
}

/// 取消一个任务（立即从表里移除，后续 `/wait` 会拿到 404）。
pub fn cancel(state: &str) -> bool {
    let handle = {
        let tasks = lock(table());
        tasks.get(state).cloned()
    };
    let Some(handle) = handle else {
        return false;
    };
    {
        let mut task = lock(&handle);
        if task.done {
            return false;
        }
        task.canceled = true;
        task.done = true;
        task.error = Some("登录已取消".to_string());
        task.finished_at = Some(Instant::now());
    }
    let mut tasks = lock(table());
    tasks.retain(|_, item| !Arc::ptr_eq(item, &handle));
    true
}

/// `/wait` 的三分支响应：pending / done+error / done+account。
pub fn wait_response(task: &LoginTask) -> Value {
    if !task.done {
        return json!({ "pending": true });
    }
    if let Some(error) = &task.error {
        return json!({ "done": true, "error": error });
    }
    json!({
        "done": true,
        "accountId": task.account_id.clone().unwrap_or_default(),
        "session": {
            "accountUid": task.account_id.clone().unwrap_or_default(),
            "provider": task.provider.clone(),
        },
    })
}

/// 发起网页登录：返回 `{state, authUrl, provider}`，后台任务异步等待完成。
pub async fn start(state: &AppState, provider: &str, channel_id: &str) -> Result<Value, String> {
    let provider = registry::normalize_kind(provider);
    if registry::session_meta(&provider).is_none() {
        return Err(format!("「{provider}」不是登录态型供应商，不需要网页登录"));
    }
    let client = reqwest::Client::new();
    let started = match provider.as_str() {
        "workbuddy" => workbuddy::start(&client).await?,
        "qoder" => qoder::start(&client).await?,
        "zcode" => zcode::start(&client).await?,
        "cline" => cline::start(&client).await?,
        "catpaw" => catpaw::start(&client).await?,
        "xiaohuanxiong" => raccoon::start(&client).await?,
        "accio" => accio::start(&client).await?,
        "antigravity" => antigravity::start(&client).await?,
        "trae" => trae::start(&client).await?,
        other => {
            return Err(format!(
                "「{other}」的网页登录引擎尚未接入网关：请改用「填写凭证」粘贴登录态"
            ))
        }
    };
    let handle = register(Arc::new(Mutex::new(LoginTask {
        state: started.state.clone(),
        auth_url: started.auth_url.clone(),
        provider: provider.clone(),
        channel_id: channel_id.trim().to_string(),
        done: false,
        error: None,
        account_id: None,
        canceled: false,
        finished_at: None,
        flow: Some(started.flow.clone()),
    })));
    let flow = started.flow.clone();
    let sessions = state.account_sessions.clone();
    let channels = state.channel_store.clone();
    let runner = handle.clone();
    tokio::spawn(async move {
        run_loop(runner, flow, sessions, channels).await;
    });
    Ok(json!({
        "state": started.state,
        "authUrl": started.auth_url,
        "provider": provider,
    }))
}

async fn run_loop(
    handle: Handle,
    flow: Arc<dyn LoginFlow>,
    sessions: AccountSessionStore,
    channels: Arc<ChannelStore>,
) {
    let deadline = Instant::now() + LOGIN_TIMEOUT;
    let interval = flow.poll_interval();
    let mut last_error = String::new();
    loop {
        {
            let task = lock(&handle);
            if task.canceled || task.done {
                return;
            }
        }
        if Instant::now() >= deadline {
            let message = if last_error.is_empty() {
                "网页登录超时，请重新发起".to_string()
            } else {
                format!("网页登录超时（最后一次错误：{last_error}）")
            };
            finish(&handle, None, Some(message));
            return;
        }
        tokio::time::sleep(interval).await;
        {
            let task = lock(&handle);
            if task.canceled || task.done {
                return;
            }
        }
        match flow.poll().await {
            Ok(Some(creds)) => {
                let task = lock(&handle);
                if task.canceled || task.done {
                    return;
                }
                let provider = task.provider.clone();
                let channel_id = task.channel_id.clone();
                drop(task);
                match persist_account(&sessions, &channels, &provider, &channel_id, creds) {
                    Ok(account_id) => finish(&handle, Some(account_id), None),
                    Err(error) => finish(&handle, None, Some(error)),
                }
                return;
            }
            Ok(None) => continue,
            Err(error) => last_error = error,
        }
    }
}

/// 手工提交回调地址：交给对应流程完成换证，成功后落账号。
pub async fn submit_callback(
    state: &AppState,
    task_state: &str,
    url: &str,
) -> Result<String, String> {
    let task = snapshot(task_state).ok_or_else(|| "登录任务不存在或已过期".to_string())?;
    if task.done {
        return task
            .account_id
            .clone()
            .ok_or_else(|| "该登录任务已结束".to_string());
    }
    let provider = task.provider.clone();
    let flow = task
        .flow
        .clone()
        .ok_or_else(|| "登录任务的流程上下文已丢失，请重新发起".to_string())?;
    let creds = flow
        .submit_callback(url)
        .await?
        .ok_or_else(|| "回调未产生凭证".to_string())?;
    let account_id = persist_account(
        &state.account_sessions,
        &state.channel_store,
        &provider,
        &task.channel_id,
        creds,
    )?;
    if let Some(handle) = {
        let tasks = lock(table());
        tasks.get(task_state).cloned()
    } {
        finish(&handle, Some(account_id.clone()), None);
    }
    Ok(account_id)
}

/// 短信登录成功后落账号（供管理面 `sms/verify` 调用）。
pub fn save_credentials(
    state: &AppState,
    provider: &str,
    channel_id: &str,
    creds: Credentials,
) -> Result<String, String> {
    persist_account(
        &state.account_sessions,
        &state.channel_store,
        provider,
        channel_id,
        creds,
    )
}

/// 登录凭证 → 账号池记录（自动建渠道、补齐默认名称）。
fn persist_account(
    sessions: &AccountSessionStore,
    channels: &ChannelStore,
    provider: &str,
    channel_id: &str,
    creds: Credentials,
) -> Result<String, String> {
    let channel = ensure_channel(channels, channel_id, provider)?;
    let mut session = AccountSession::new(channel.id.clone(), creds.name.clone());
    session.provider = provider.to_string();
    session.base_url = creds.base_url.unwrap_or_default();
    session.credential = Credential {
        kind: CREDENTIAL_KIND_SESSION.to_string(),
        secret: creds.secret,
    };
    session.extra_headers = creds.extra_headers;
    session.expires_at = creds.expires_at;
    if session.name.trim().is_empty() {
        let count = sessions.for_channel(&channel.id).len();
        session.name = format!("{}#{}", label_of(provider), count + 1);
    }
    let saved = sessions
        .upsert(session)
        .map_err(|error| format!("保存账号失败：{error}"))?;
    Ok(saved.id)
}

fn ensure_channel(
    channels: &ChannelStore,
    channel_id: &str,
    provider: &str,
) -> Result<Channel, String> {
    if !channel_id.trim().is_empty() {
        let channel = channels
            .get(channel_id.trim())
            .ok_or_else(|| "渠道不存在".to_string())?;
        if channel.channel_type != ChannelType::Account {
            return Err("目标渠道不是账号型渠道".to_string());
        }
        return Ok(channel);
    }
    let label = label_of(provider);
    if let Some(found) = channels.list().into_iter().find(|channel| {
        channel.channel_type == ChannelType::Account && channel.name.starts_with(&label)
    }) {
        return Ok(found);
    }
    let channel = Channel {
        id: String::new(),
        name: format!("{label} 账号池"),
        channel_type: ChannelType::Account,
        base_url: String::new(),
        api_key: String::new(),
        priority: 0,
        weight: 1,
        status: "enabled".to_string(),
        models: Vec::new(),
        model_mapping: HashMap::new(),
        cost_pricing: HashMap::new(),
        account_id: String::new(),
        last_error: None,
        last_used_at: None,
        discovered_models: Vec::new(),
        response_time: None,
        test_time: None,
        balance: None,
        credit: None,
        created_at: 0,
        updated_at: 0,
    };
    channels
        .add(channel)
        .map_err(|error| format!("创建账号型渠道失败：{error}"))
}

fn label_of(provider: &str) -> String {
    registry::session_meta(provider)
        .map(|_| {
            registry::find(provider)
                .map(|spec| spec.label.to_string())
                .unwrap_or_else(|| provider.to_string())
        })
        .unwrap_or_else(|| provider.to_string())
}
