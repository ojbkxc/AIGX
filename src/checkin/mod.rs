//! 签到中心 —— 替账号池里的上游账号每日领额度。
//!
//! ## 与「用户签到」的区别（别混淆）
//!
//! - `user::checkin`：**用户**每日签到，给本站配额奖励（`/api/checkin`，钱包页）。
//! - 本模块：**上游账号**每日签到，替账号去各家客户端领它们自带的额度/积分。
//!   产出物落在上游账号上，不是本站余额。
//!
//! ## 结构
//!
//! - [`store`]：签到记录台账（KV）+ 定时计划（KV）+ 设置
//! - [`claim`]：各 provider 的执行体，统一 `{success, msg}` 契约
//! - [`scheduler`]：30 秒轮询，按计划时刻触发，当天去重
//!
//! ## 统一契约
//!
//! 所有 provider 的 claim 都返回 [`ClaimResult`]，只有两个字段决定台账与
//! 界面：`success` 与 `msg`。各家的成功判据（HTTP 码 / 业务码 / 回查翻转）
//! 由各自的实现负责翻译成这两个字段 —— 判据只此一份，调度与落账不分叉。
//!
//! ## 运行时验证
//!
//! [`runtime_tests`] 用**上游替身**（本地 mock server）把 12 家的完整签到
//! 流程跑通 —— 成功领取 / 今天已领 / 登录态失效 / 活动未下发，并断言请求
//! 构造（路径、头）。真上游要真实登录态账号（跑不了也测不了），替身是
//! 唯一能在 CI 里跑的验证方式。
//!
//! ## 不支持的 provider
//!
//! 没有 claim 实现的 provider 不出现在签到中心（见 [`claim::supports_checkin`]），
//! 而不是列出来每次都报「不支持」。这与 Agent2API 的 `CHECKIN_PROVIDERS`
//! 白名单同一取向 —— 用白名单而非黑名单，避免新增 provider 时静默漏签。

pub mod benefits;
pub mod claim;
pub mod scheduler;
pub mod store;

#[cfg(test)]
mod runtime_tests;

pub use store::{CheckinOutcome, CheckinStore};

/// 一次签到的结果（各家 claim 的统一返回）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ClaimResult {
    /// 是否真的领到（或今天已经领过 —— 视 `outcome` 区分）
    pub success: bool,
    /// 给管理员看的一句话
    pub msg: String,
    /// 结果分类：success / already / pending / failed
    pub outcome: CheckinOutcome,
}
