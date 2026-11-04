//! 一次性 / 限时福利领取通道 —— 每日签到之外的第二类「替上游账号领额度」。
//!
//! ## 为什么单独一套，不复用 [`crate::checkin::claim`]
//!
//! `claim` 的契约是「单账号、单动作、**按日**」：一个 status 路径 + 一个
//! claim 路径 + 一个「今天是否已领」判据。而本模块这些动作都不符合：
//!
//! - **Onboarding / CodeArts 新手礼**：一次性（领完即结清，之后再查应是零上游请求）；
//! - **WB-intl 保活**：本质是「配置 + 每日活跃执行」，不是「领一次」；
//! - **Qoder UMID**：机器级组件（一次安装服务所有国际版账号），与账号无关；
//! - **ZCode 限时套餐**：按份（plan_id）判定、跨日按 plan 去重，还要过验证码；
//! - **WorkBuddy growth**：一棵任务树，多步 accept → 上报 → claim。
//!
//! 硬塞进按日台账会把「已结清」表达成「今天已领」，明天又白打一次上游。
//! 所以本模块另立契约：一次性 / 限时动作**不进入**每日 scheduler（只能手动
//! 触发）。唯一例外是 **WB-intl 保活** —— 它是每日活跃执行，由
//! `scheduler::run_schedule` 对 `workbuddy-intl` 账号直接调
//! `keepalive::run("full")` 跑每日链（对齐 Agent2API「定时与批量签到恒走
//! `WorkbuddyActivity::Full`」），保活计数与 `kept_active` 台账标记一次产出。
//!
//! ## 统一契约
//!
//! 所有动作返回 [`BenefitResult`]：`outcome` 走 [`BenefitOutcome`] 五分类，
//! `data` 放各动作自己的结构化结果（任务列表 / 套餐列表 / 汇总数字）。
//! 一次性状态另开 KV 前缀（`benefit:*`），**不混入** `checkin:rec:*` 的按日台账。

pub mod codearts;
pub mod growth;
pub mod keepalive;
pub mod onboarding;
pub mod umid;
pub mod zcode_claim;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::account_session::model::AccountSession;

/// 一次性/限时动作的结果分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BenefitOutcome {
    /// 本次真的领到（或执行成功）
    Done,
    /// 之前已领过 / 已结清（不算失败）
    Already,
    /// 提交了但上游还没确认到账（可重试）
    Pending,
    /// 上游明确拒绝
    Failed,
    /// 该账号/该家没有这个通道
    Skipped,
}

/// 一次福利动作的结果。
#[derive(Debug, Clone, Serialize)]
pub struct BenefitResult {
    pub ok: bool,
    pub outcome: BenefitOutcome,
    /// 给管理员看的一句话
    pub message: String,
    /// 结构化明细（任务列表 / 套餐列表 / 汇总）
    #[serde(skip_serializing_if = "Value::is_null")]
    pub data: Value,
}

impl BenefitResult {
    pub fn done(message: impl Into<String>) -> Self {
        Self {
            ok: true,
            outcome: BenefitOutcome::Done,
            message: message.into(),
            data: Value::Null,
        }
    }

    pub fn already(message: impl Into<String>) -> Self {
        Self {
            ok: true,
            outcome: BenefitOutcome::Already,
            message: message.into(),
            data: Value::Null,
        }
    }

    pub fn pending(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            outcome: BenefitOutcome::Pending,
            message: message.into(),
            data: Value::Null,
        }
    }

    pub fn failed(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            outcome: BenefitOutcome::Failed,
            message: message.into(),
            data: Value::Null,
        }
    }

    pub fn skipped(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            outcome: BenefitOutcome::Skipped,
            message: message.into(),
            data: Value::Null,
        }
    }

    pub fn with_data(mut self, data: Value) -> Self {
        self.data = data;
        self
    }
}

/// 通道契约表（端点与判据集中一处）。
///
/// 与 `checkin::claim::CHECKIN_CONTRACTS` 同样只起两个作用：契约测试断言
/// 「表里每家都有实现分派」，以及上游改路径时有**一处**会对红。真正的请求
/// 构造在各自实现里，不从这里取。
pub struct BenefitContract {
    /// provider id（与账号池 registry 一致）
    pub provider: &'static str,
    /// 动作 id（API 路径里的 `action`）
    pub action: &'static str,
    /// 人类可读的通道名
    pub label: &'static str,
    /// 上游端点（只读侧；写侧见实现）
    pub endpoint: &'static str,
}

/// 全部福利通道。
pub const BENEFIT_CONTRACTS: &[BenefitContract] = &[
    BenefitContract {
        provider: "codearts",
        action: "newbie",
        label: "CodeArts 新人礼",
        endpoint: "GET /v1/ops/delivery?channel=IDE",
    },
    BenefitContract {
        provider: "loomy",
        action: "onboarding",
        label: "Loomy 新手任务",
        endpoint: "GET /onboarding/tasks",
    },
    BenefitContract {
        provider: "loomy",
        action: "onboarding_claim",
        label: "Loomy 新手任务领取",
        endpoint: "POST /onboarding/tasks/complete",
    },
    BenefitContract {
        provider: "xiaohuanxiong",
        action: "onboarding",
        label: "小浣熊登录积分",
        endpoint: "POST /api/web/desktop/v1/login/points/grant",
    },
    BenefitContract {
        provider: "officeace",
        action: "onboarding",
        label: "OfficeAce 新人礼",
        endpoint: "POST /v1/subscription/bonus/claim",
    },
    BenefitContract {
        provider: "workbuddy-intl",
        action: "keepalive",
        label: "WorkBuddy 国际版保活（只打最小对话）",
        endpoint: "POST {base}/v1/chat/completions",
    },
    BenefitContract {
        provider: "workbuddy-intl",
        action: "claim",
        label: "WorkBuddy 国际版日活领取",
        endpoint: "POST {base}/v2/billing/meter/daily-checkin",
    },
    BenefitContract {
        provider: "workbuddy-intl",
        action: "keepalive_claim",
        label: "WorkBuddy 国际版保活+领取",
        endpoint: "POST {base}/console/as/conversations/",
    },
    BenefitContract {
        provider: "workbuddy",
        action: "growth",
        label: "WorkBuddy 成长任务",
        endpoint: "GET /v2/activity/growth/tasks",
    },
    BenefitContract {
        provider: "zcode",
        action: "plan",
        label: "ZCode 限时套餐",
        endpoint: "GET /api/v1/zcode-plan/billing/preview",
    },
    BenefitContract {
        provider: "zcode-intl",
        action: "plan",
        label: "ZCode 国际版限时套餐",
        endpoint: "GET /api/v1/zcode-plan/billing/preview",
    },
    BenefitContract {
        provider: "qoder-intl",
        action: "umid",
        label: "Qoder 国际版 UMID 组件",
        endpoint: "GET /api/risk/umid/status",
    },
];

/// 该 provider 支持的福利动作（供界面决定渲染哪些行）。
pub fn actions_for(provider: &str) -> Vec<&'static str> {
    BENEFIT_CONTRACTS
        .iter()
        .filter(|contract| contract.provider == provider)
        .map(|contract| contract.action)
        .collect()
}

/// 分派一次福利动作。
///
/// `action` 取值见 [`BENEFIT_CONTRACTS`]（`onboarding` / `onboarding_claim` /
/// `newbie` / `keepalive` / `growth` / `plan` / `umid`）；`params` 给需要
/// 额外输入的动作（growth 的任务 code、zcode 的 mode/plan_id/验证码参数）。
///
/// `pool` 目前只做参数占位：各动作都只读单个账号，不需要池级调度；保留它
/// 是为了后续「批量领取」入口复用同一分派。
pub async fn run(
    pool: &crate::account_session::AccountSessionStore,
    store: &std::sync::Arc<crate::storage::FileStore>,
    client: &reqwest::Client,
    session: &AccountSession,
    action: &str,
    params: &Value,
) -> BenefitResult {
    let _ = pool;
    match (session.provider.as_str(), action) {
        ("codearts", "newbie") => codearts::claim_newbie(client, session).await,
        ("loomy" | "xiaohuanxiong" | "officeace", "onboarding" | "onboarding_claim") => {
            onboarding::run(store, client, session, action).await
        }
        ("workbuddy-intl", "keepalive") => {
            keepalive::run(store, client, session, "keepalive").await
        }
        ("workbuddy-intl", "claim") => keepalive::run(store, client, session, "claim").await,
        ("workbuddy-intl", "keepalive_claim") => {
            keepalive::run(store, client, session, "full").await
        }
        ("workbuddy", "growth") => growth::run(client, session, params).await,
        ("zcode" | "zcode-intl", "plan") => zcode_claim::run(store, client, session, params).await,
        ("qoder-intl", "umid") => umid::status(),
        (provider, action) => BenefitResult::skipped(format!("{provider} 没有 {action} 通道")),
    }
}

/// 写一次性状态（结清标记 / 已领记录）。
pub(crate) fn save_memory(
    store: &crate::storage::FileStore,
    key: &str,
    value: &Value,
) -> anyhow::Result<()> {
    store.put(key, value)
}

/// 读一次性状态。
pub(crate) fn load_memory(store: &crate::storage::FileStore, key: &str) -> Option<Value> {
    store.get::<Value>(key).ok().flatten()
}

/// 一次性状态键（按账号 + 通道分格）。
pub(crate) fn memory_key(account_id: &str, channel: &str) -> String {
    format!("benefit:{channel}:{account_id}")
}

/// 账号的凭证明文（各家实现都要用）。
pub(crate) fn secret_of(session: &AccountSession) -> String {
    session.credential.secret_plain()
}

/// 发一次 JSON 请求，返回 (status, body)。
pub(crate) async fn send_json(
    client: &reqwest::Client,
    method: reqwest::Method,
    url: &str,
    headers: &[(String, String)],
    body: Option<&Value>,
) -> Result<(u16, Value), String> {
    let mut request = client
        .request(method, url)
        .timeout(std::time::Duration::from_secs(30));
    for (name, value) in headers {
        request = request.header(name, value);
    }
    if let Some(payload) = body {
        request = request.json(payload);
    }
    let response = request.send().await.map_err(|e| format!("请求失败：{e}"))?;
    let status = response.status().as_u16();
    let text = response.text().await.unwrap_or_default();
    let value = serde_json::from_str::<Value>(&text).unwrap_or(Value::Null);
    Ok((status, value))
}

/// JWT 声明取值（不验签）—— 各家拿 uid / 账号 id 用。
pub(crate) fn jwt_claim_str(token: &str, keys: &[&str]) -> Option<String> {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    let payload = token.split('.').nth(1)?;
    let raw = URL_SAFE_NO_PAD
        .decode(payload)
        .or_else(|_| URL_SAFE_NO_PAD.decode(format!("{payload}==")))
        .ok()?;
    let value: Value = serde_json::from_slice(&raw).ok()?;
    keys.iter().find_map(|key| {
        value.get(*key).and_then(|v| match v {
            Value::String(s) if !s.trim().is_empty() => Some(s.clone()),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actions_cover_contract_table() {
        assert!(actions_for("codearts").contains(&"newbie"));
        assert!(actions_for("loomy").contains(&"onboarding"));
        assert!(actions_for("workbuddy-intl").contains(&"keepalive"));
        assert!(actions_for("workbuddy-intl").contains(&"claim"));
        assert!(actions_for("workbuddy-intl").contains(&"keepalive_claim"));
        assert!(actions_for("zcode").contains(&"plan"));
        assert!(actions_for("zcode-intl").contains(&"plan"));
        assert!(actions_for("openai").is_empty());
        // 契约表里每条都必须是「有分派」的 —— 防止加表忘接线
        for contract in BENEFIT_CONTRACTS {
            assert!(
                !contract.endpoint.is_empty(),
                "{} 的端点不该为空",
                contract.label
            );
        }
    }

    #[test]
    fn memory_key_is_namespaced() {
        assert_eq!(memory_key("abc", "onboarding"), "benefit:onboarding:abc");
    }
}
