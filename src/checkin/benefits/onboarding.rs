//! 新手任务 / 一次性登录积分（loomy / xiaohuanxiong / officeace）。
//!
//! 来源：Agent2API `providers/{loomy,raccoon,officeace}/onboarding.rs`。
//! 共同点是**一次性**：领完即结清，之后再查应该是零上游请求 —— 所以这里
//! 用 KV 记忆（`benefit:onboarding:{account_id}`）记住「已结清」，避免每次
//! 点开页面都去问上游。
//!
//! 各家差异：
//! - **loomy**：上游有只读口（`GET /onboarding/tasks`）与逐任务完成口
//!   （`POST /onboarding/tasks/complete`），能精确知道还剩几个任务；
//! - **小浣熊**：上游**没有**只读口，只有「桌面端 / 移动端各领一次」两个
//!   grant 接口 —— 「已领」只能靠本地记忆推导；
//! - **OfficeAce**：新人礼与每日签到是**同一个上游动作**（领当天全部 bonus），
//!   不在这里重复实现，直接指向签到中心，避免两处打架。

use std::sync::Arc;

use serde_json::{json, Value};

use super::{load_memory, memory_key, save_memory, send_json, BenefitResult};
use crate::account_session::model::AccountSession;
use crate::storage::FileStore;

/// 执行一次新手通道动作。
///
/// `action` 取 `onboarding`（只读盘点）或 `onboarding_claim`（领取未领的）。
pub async fn run(
    store: &Arc<FileStore>,
    client: &reqwest::Client,
    session: &AccountSession,
    action: &str,
) -> BenefitResult {
    let claim = action == "onboarding_claim";
    match session.provider.as_str() {
        "loomy" => loomy(store, client, session, claim).await,
        "xiaohuanxiong" => xiaohuanxiong(store, client, session).await,
        "officeace" => {
            BenefitResult::already("OfficeAce 新人礼与每日签到是同一个上游动作，请在签到中心执行")
        }
        other => BenefitResult::skipped(format!("{other} 没有新手任务通道")),
    }
}

/// Loomy：读任务列表，`claim` 时逐个完成未完成项。
async fn loomy(
    store: &Arc<FileStore>,
    client: &reqwest::Client,
    session: &AccountSession,
    claim: bool,
) -> BenefitResult {
    let base = crate::account_session::adapter::effective_base_url(session)
        .unwrap_or_else(|_| "https://loomyad.xunfei.cn/api/v1".to_string());
    let base = base.trim_end_matches('/');
    let secret = super::secret_of(session);
    if secret.trim().is_empty() {
        return BenefitResult::failed("Loomy 账号没有凭据，请重新登录");
    }
    let headers = vec![
        ("token".to_string(), secret.clone()),
        ("authorization".to_string(), format!("Bearer {secret}")),
        ("content-type".to_string(), "application/json".to_string()),
    ];
    let key = memory_key(&session.id, "onboarding");

    // 已结清且只是盘点：零上游请求直接返回
    if !claim {
        if let Some(memory) = load_memory(store, &key) {
            if memory.get("settled").and_then(Value::as_bool) == Some(true) {
                return BenefitResult::already("Loomy 新手任务已全部结清").with_data(memory);
            }
        }
    }

    let (status, value) = match send_json(
        client,
        reqwest::Method::GET,
        &format!("{base}/onboarding/tasks"),
        &headers,
        None,
    )
    .await
    {
        Ok(pair) => pair,
        Err(e) => return BenefitResult::failed(e),
    };
    if status == 401 || status == 403 {
        return BenefitResult::failed("Loomy 登录态已失效，请重新登录");
    }
    if !(200..300).contains(&status) {
        return BenefitResult::failed(format!("Loomy 新手任务读取失败（HTTP {status}）"));
    }
    if value.get("code").and_then(Value::as_str) == Some("100002") {
        return BenefitResult::failed("Loomy 登录态已失效，请重新登录");
    }
    let tasks = value
        .pointer("/data/tasks")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if tasks.is_empty() {
        return BenefitResult::already("Loomy 没有新手任务").with_data(json!({ "tasks": [] }));
    }
    let pending: Vec<Value> = tasks
        .iter()
        .filter(|task| task.get("done").and_then(Value::as_bool) != Some(true))
        .cloned()
        .collect();

    if !claim {
        let settled = pending.is_empty();
        let snapshot = json!({
            "settled": settled,
            "tasks": tasks,
            "unclaimed": pending.len(),
        });
        let _ = save_memory(store, &key, &snapshot);
        return if settled {
            BenefitResult::already("Loomy 新手任务已全部结清").with_data(snapshot)
        } else {
            BenefitResult::done(format!("Loomy 还有 {} 个新手任务可领", pending.len()))
                .with_data(snapshot)
        };
    }

    let mut done = 0usize;
    let mut failed = 0usize;
    for task in &pending {
        let Some(task_key) = task.get("key").and_then(Value::as_str) else {
            continue;
        };
        match send_json(
            client,
            reqwest::Method::POST,
            &format!("{base}/onboarding/tasks/complete"),
            &headers,
            Some(&json!({ "key": task_key })),
        )
        .await
        {
            Ok((code, body)) if (200..300).contains(&code) => {
                // 上游对重复上报返回 alreadyCompleted=true，同样算成功
                let repeated = body
                    .pointer("/data/alreadyCompleted")
                    .and_then(Value::as_bool)
                    == Some(true);
                if repeated || body.get("code").and_then(Value::as_str) == Some("000000") {
                    done += 1;
                } else {
                    failed += 1;
                }
            }
            _ => failed += 1,
        }
    }
    let settled = failed == 0;
    let snapshot = json!({
        "settled": settled,
        "claimed": done,
        "failed": failed,
    });
    let _ = save_memory(store, &key, &snapshot);
    if failed == 0 {
        BenefitResult::done(format!("Loomy 已领取 {done} 个新手任务奖励")).with_data(snapshot)
    } else {
        BenefitResult::pending(format!("Loomy 领取 {done} 个，失败 {failed} 个（可重试）"))
            .with_data(snapshot)
    }
}

/// 小浣熊：桌面端 / 移动端各领一次登录积分（无只读口，靠本地记忆去重）。
async fn xiaohuanxiong(
    store: &Arc<FileStore>,
    client: &reqwest::Client,
    session: &AccountSession,
) -> BenefitResult {
    let secret = super::secret_of(session);
    if secret.trim().is_empty() {
        return BenefitResult::failed("小浣熊账号没有凭据，请重新登录");
    }
    let headers = vec![
        ("Authorization".to_string(), format!("Bearer {secret}")),
        ("content-type".to_string(), "application/json".to_string()),
    ];
    let key = memory_key(&session.id, "onboarding");
    let mut memory = load_memory(store, &key).unwrap_or_else(|| json!({}));

    // 两个通道各只能领一次：桌面端 / 移动端
    let channels = [("desktop", "desktop"), ("mobile", "mobile")];
    let mut claimed = 0usize;
    let mut skipped = 0usize;
    let mut errors: Vec<String> = Vec::new();
    for (name, path) in channels {
        if memory.get(name).and_then(Value::as_bool) == Some(true) {
            skipped += 1;
            continue;
        }
        let url = format!("https://xiaohuanxiong.com/api/web/{path}/v1/login/points/grant");
        match send_json(
            client,
            reqwest::Method::POST,
            &url,
            &headers,
            Some(&json!({})),
        )
        .await
        {
            Ok((status, body)) if (200..300).contains(&status) => {
                let code = body.get("code").and_then(Value::as_i64).unwrap_or(-1);
                if code == 0 {
                    memory[name] = json!(true);
                    claimed += 1;
                } else {
                    errors.push(format!("{name} code={code}"));
                }
            }
            Ok((status, _)) => errors.push(format!("{name} HTTP {status}")),
            Err(e) => errors.push(format!("{name}: {e}")),
        }
    }
    let settled = channels
        .iter()
        .all(|(name, _)| memory.get(*name).and_then(Value::as_bool) == Some(true));
    memory["settled"] = json!(settled);
    let _ = save_memory(store, &key, &memory);

    if !errors.is_empty() {
        return BenefitResult::pending(format!(
            "小浣熊领取 {} 个，失败：{}",
            claimed,
            errors.join("；")
        ))
        .with_data(memory);
    }
    if claimed == 0 {
        return BenefitResult::already(format!("小浣熊登录积分已领取过（{skipped} 个通道）"))
            .with_data(memory);
    }
    BenefitResult::done(format!("小浣熊已领取 {claimed} 个登录积分礼包")).with_data(memory)
}
