//! ZCode 人机验证令牌池（阿里云无痕验证 proof：**界面铸造、网关消费**）。
//!
//! ── 它解决什么（一句话）──────────────────────────────────────
//! ZCode 的「限时套餐 / 领取」那条链上有一道人机验证门禁：
//! 少 `X-Aliyun-Captcha-Verify-Param` 一律 `400 {"code":3007,"msg":"captcha
//! verify failed"}`（对照 Agent2API `providers/zcode/captcha.rs` 的实测结论）。
//! 令牌是阿里云验证码 V3「无痕验证」的产物（`certifyId` + `securityToken` 的
//! base64 JWT 形态，约 280 字符），**只有浏览器环境能铸**：要跑阿里云 SDK。
//! Rust 侧没有 JS/DOM 引擎，所以分工是：
//!
//! ```text
//!   前端（frontend/src/lib/zcodeCaptchaPool.ts）
//!       └─ SDK.instance.startTracelessVerification()   ← 静默、无需用户操作
//!              └─ POST /api/zcode/captcha {tokens:[…]}  ← 铸一个推一个（本模块收口）
//!   Rust 领取链路（checkin/benefits/zcode_claim.rs）
//!       └─ take() → X-Aliyun-Captcha-Verify-Param / -Region
//! ```
//!
//! ── 令牌的几条硬性质（都来自实测，别当优化去掉）────────────────
//!   · **一次一用**：同一个令牌发第二次必回 3007；失败的请求同样消耗掉它
//!     （上游在鉴权前先验验证码），所以「重试」必须再取一个；
//!   · **有寿命**：见 [`TOKEN_TTL_MS`]，过期的直接丢，不试 —— 试了也是 3007；
//!   · **与账号无关**：令牌是「这台设备 + 这个场景」的产物，不绑账号，全池共用；
//!   · **有风控上限**：阿里云对铸造频率/IP 有风控，所以本模块**只做收口与消费**，
//!     绝不主动去要令牌；铸造节奏由界面按「库存低于目标才补」控制
//!     （节奏设置见 `config.usage.zcode_captcha_pool_*`）。
//!
//! ── 没有令牌时怎么办（这是本模块的边界）──────────────────────
//! 如实失败，给出可执行的提示：headless / Docker 部署没有 WebView，铸不出令牌，
//! 这条通道在那里就是不可用 —— 前端面板会直说这句话。不要伪造令牌、也不要静默
//! 降级：上游会用 3007 把请求挡回来，用户只看到一句看不懂的英文。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件（业务代码）零 unwrap / expect / panic ——
//! 锁中毒一律退化成空池，而不是把网关带崩。

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::OnceLock;

use serde_json::{json, Value};

/// 请求头：阿里云验证码 proof（与 Agent2API / 上游实测口径逐字一致）。
pub const VERIFY_PARAM_HEADER: &str = "x-aliyun-captcha-verify-param";

/// 请求头：这个 proof 属于哪个阿里云站点（`cn` / `sgp` 等）。
pub const VERIFY_REGION_HEADER: &str = "x-aliyun-captcha-verify-region";

/// 令牌寿命（毫秒）。上游按 ~95 秒失效，这里留一点余量给「铸好到发出」的窗口：
/// 超过它的一律丢弃（过期令牌上游一定拒，试它只是多一次 3007）。
pub const TOKEN_TTL_MS: i64 = 120_000;

/// 池子容量上限：超过就丢**最旧**的。
///
/// 界面按「库存低于目标才补」的节奏铸造，正常不会逼近这个上限；它是防御性的
/// （界面被改坏 / 重复推送时不至于把内存撑爆）。
const POOL_MAX: usize = 500;

/// 一条令牌。
struct Entry {
    /// 上游认的那个 base64 验证串（原样进 `X-Aliyun-Captcha-Verify-Param`）
    param: String,
    /// 阿里云站点（原样进 `X-Aliyun-Captcha-Verify-Region`；空 = 不发这个头）
    region: String,
    /// 铸出时刻（毫秒时间戳，本地时钟）
    at: i64,
}

/// 池子 + 计数（计数只为界面/排障展示，不参与任何判定）。
#[derive(Default)]
struct Pool {
    entries: VecDeque<Entry>,
    /// 累计入库 / 消费 / 上游拒收（3007）/ 过期丢弃
    minted: u64,
    consumed: u64,
    rejected: u64,
    stale: u64,
    /// 最近一次上游回 3007 的时刻（0 = 没有过）—— 界面据此提高补货优先级
    last_challenge_at: i64,
    /// 最近一次入库时刻
    last_mint_at: i64,
}

fn pool() -> &'static Mutex<Pool> {
    static POOL: OnceLock<Mutex<Pool>> = OnceLock::new();
    POOL.get_or_init(|| Mutex::new(Pool::default()))
}

/// 毫秒时间戳（与 `scheduled_tasks::task_state` 同口径）。
fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// 入池（界面铸好一个就推一个）。返回入库后的库存数。
///
/// 空串一律拒绝：`param` 是必填的验证串，空值入池只会让下一次请求带着一个空头
/// 出去（上游当缺失处理，回 3007，白费一次往返）。
pub fn push(param: &str, region: &str) -> usize {
    let param = param.trim();
    if param.is_empty() {
        return ready();
    }
    let now = now_ms();
    let Ok(mut pool) = pool().lock() else {
        // 锁中毒：退化成空池，下一次取必然想要新令牌（不 panic 是本文件的前提）
        return 0;
    };
    pool.entries.push_back(Entry {
        param: param.to_string(),
        region: region.trim().to_string(),
        at: now,
    });
    while pool.entries.len() > POOL_MAX {
        pool.entries.pop_front();
    }
    pool.minted = pool.minted.saturating_add(1);
    pool.last_mint_at = now;
    pool.entries.len()
}

/// 取一个令牌（FIFO：先铸先用，避免新令牌被旧令牌挤到过期）。
///
/// 顺手把过期条目丢掉并计数 —— 池子里躺着几十条过期令牌时库存数会骗人
/// （界面看「还有 8 个」却连续 3007）。
pub fn take() -> Option<(String, String)> {
    let now = now_ms();
    let mut pool = pool().lock().ok()?;
    while let Some(entry) = pool.entries.pop_front() {
        if now.saturating_sub(entry.at) > TOKEN_TTL_MS {
            pool.stale = pool.stale.saturating_add(1);
            continue;
        }
        pool.consumed = pool.consumed.saturating_add(1);
        return Some((entry.param, entry.region));
    }
    None
}

/// 上游回了一次 3007（令牌被拒/过期/没用上）。
///
/// 同时是「池子里的令牌不可信了」的信号：界面看到这个计数变大就会立刻补货
/// （连库存都暂时不信，见 `zcodeCaptchaPool.ts` 的 `challenged` 分支）。
pub fn note_challenge() {
    let now = now_ms();
    if let Ok(mut pool) = pool().lock() {
        pool.rejected = pool.rejected.saturating_add(1);
        pool.last_challenge_at = now;
    }
}

/// 当前可用库存（不含过期条目）。
pub fn ready() -> usize {
    let now = now_ms();
    let Ok(mut pool) = pool().lock() else {
        return 0;
    };
    while pool
        .entries
        .front()
        .is_some_and(|entry| now.saturating_sub(entry.at) > TOKEN_TTL_MS)
    {
        pool.entries.pop_front();
        pool.stale = pool.stale.saturating_add(1);
    }
    pool.entries.len()
}

/// 池子概况（管理接口 `GET /api/zcode/captcha` 的响应体）。
///
/// 不含「该不该铸」的判据 —— 那取决于**有没有账号在用这条通道**，属于调用方
/// 的业务判断（见 `api/admin/zcode_captcha.rs`）。
pub fn stats() -> Value {
    let ready = ready();
    let now = now_ms();
    let Ok(pool) = pool().lock() else {
        return json!({ "ready": ready, "ttlMs": TOKEN_TTL_MS });
    };
    let oldest_age_ms = pool
        .entries
        .front()
        .map(|entry| now.saturating_sub(entry.at))
        .unwrap_or(0);
    json!({
        "ready": ready,
        "ttlMs": TOKEN_TTL_MS,
        "oldestAgeMs": oldest_age_ms,
        "minted": pool.minted,
        "consumed": pool.consumed,
        "rejected": pool.rejected,
        "stale": pool.stale,
        "lastChallengeAt": pool.last_challenge_at,
        "lastMintAt": pool.last_mint_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一个用例跑完整个池子生命周期。
    ///
    /// 池是**进程级单例**，拆成多个 `#[test]` 会并行跑并互相踩（一个清了池，
    /// 另一个的断言就红）。所以生命周期断言全部合在这一个用例里 —— 宁可长一点，
    /// 也不要引入「单跑绿、全跑红」的顺序依赖。
    #[test]
    fn pool_keeps_fifo_ttl_and_counters() {
        // 空串不入池（否则下一次请求会带一个空头出去）
        let before = ready();
        assert_eq!(push("   ", "cn"), before);
        assert_eq!(push("", ""), before);

        // 入池后可取，FIFO 顺序
        let first = format!("param-first-{}", now_ms());
        let second = format!("param-second-{}", now_ms());
        assert!(push(&first, "cn") >= 1);
        assert!(push(&second, "sgp") >= 1);
        assert!(ready() >= 2);

        let (head, head_region) = take().expect("刚入池的令牌必须能取到");
        assert_eq!(head, first, "FIFO：先铸先出");
        assert_eq!(head_region, "cn");
        let (tail, tail_region) = take().expect("第二个令牌也必须能取到");
        assert_eq!(tail, second);
        assert_eq!(tail_region, "sgp");
        // 取完就没了（一次一用）
        assert!(take().is_none());

        // 3007 计数递增（界面据此判断「上游刚拒过令牌」）
        let rejected_before = stats()
            .get("rejected")
            .and_then(Value::as_u64)
            .unwrap_or_default();
        note_challenge();
        let rejected_after = stats()
            .get("rejected")
            .and_then(Value::as_u64)
            .unwrap_or_default();
        assert_eq!(rejected_after, rejected_before + 1);

        // 过期令牌：直接构造一条「早已过期」的条目（TTL 是常量，没法等它自然过期）。
        // 过期条目不算库存，也不会被当令牌发出去。
        {
            let mut guard = pool().lock().expect("测试里锁不应中毒");
            guard.entries.clear();
            let at = now_ms() - TOKEN_TTL_MS - 1_000;
            guard.entries.push_back(Entry {
                param: "stale-token".to_string(),
                region: "cn".to_string(),
                at,
            });
        }
        assert_eq!(ready(), 0);
        assert!(take().is_none());
    }
}
