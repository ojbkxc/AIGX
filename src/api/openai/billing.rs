use axum::{
    http::StatusCode,
    Json,
};

use serde_json::Value;
use std::sync::Arc;
use crate::bridge::ChatFormat;
use super::*;

/// 执行计费扣减（用户 quota + key used_quota）。
///
/// 参照 new-api：优先扣 key 绑定用户 quota，再扣 key 自身额度；
/// 用户余额不足时跳过 key 扣费以保持计费一致性。
///
/// 当 `try_charge` 失败或扣费后余额低于阈值时，发送 `QuotaLow` 通知
/// （参照非流式分支阈值计算，与 handle_chat_completions 非流式分支一致）。
// 与同族 charge_usage_with_tools / settle_usage 参数对齐（含缓存量透传），
// 不为规避参数计数拆散计费语义。
#[allow(clippy::too_many_arguments)]
pub(crate) fn charge_usage(
    state: &AppState,
    api_key: &crate::api::auth::ApiKey,
    requested_model: &str,
    model: &str,
    group: &str,
    prompt_tokens: u64,
    completion_tokens: u64,
    cached_tokens: u64,
) -> i64 {
    charge_usage_with_tools(
        state,
        api_key,
        requested_model,
        model,
        group,
        prompt_tokens,
        completion_tokens,
        cached_tokens,
        None,
    )
}
/// 订阅按次计费：用户任一活跃订阅为 `billing_mode=count` 模式即整体按次。
pub(crate) fn is_count_billing(state: &AppState, api_key: &crate::api::auth::ApiKey) -> bool {
    api_key
        .user_id
        .as_deref()
        .map(|uid| {
            let now = chrono::Utc::now().timestamp();
            state
                .subscription_store
                .find_active(uid, now)
                .iter()
                .any(|s| s.is_count_billing())
        })
        .unwrap_or(false)
}
/// 计费额度裁决（销售侧，与渠道成本无关）。
///
/// 活跃订阅为 `billing_mode=count` 时，每次请求固定扣全局
/// `config::billing_flat_quota`（忽略 token 量）；否则按 token 量 ×
/// 定价 × 倍率，附加工具按次费。
// 参数是计费语义的完整集合（调用上下文 + 请求/上游模型名 + 分组 + token 量
// + 工具附加费），与同族的 charge_usage_with_tools / settle_usage 一致，
// 不为规避参数计数拆散语义。
#[allow(clippy::too_many_arguments)]
pub(crate) fn resolve_billing_quota(
    state: &AppState,
    api_key: &crate::api::auth::ApiKey,
    requested_model: &str,
    model: &str,
    group: &str,
    prompt_tokens: u64,
    completion_tokens: u64,
    cached_tokens: u64,
    tool_calls: Option<&crate::pricing::ToolCallCounts>,
) -> i64 {
    // 按次订阅：固定扣额度，不再叠加缓存读/token 差异化计费
    if is_count_billing(state, api_key) {
        return crate::config::billing_flat_quota();
    }

    // 计价键双名感知：定价目录按对外名配置，model 为映射后上游名——
    // 对外名优先、上游名兜底，修复映射场景定价失效导致的按 0 免费放行。
    //
    // 走**上游缓存**口径（`calculate_cost_quoted_upstream_cache`）：
    // 这里的缓存量来自上游 usage（prompt caching），上游确实处理了这些
    // token、我方要照付，所以 cache_price 未配置时必须回退正价而不是免费。
    // 网关内部响应缓存命中是另一条路径（另一个函数），那里的免费语义不变。
    match state.pricing_store.calculate_cost_quoted_upstream_cache(
        requested_model,
        model,
        prompt_tokens,
        completion_tokens,
        cached_tokens,
        group,
    ) {
        Ok(mut c) => {
            if let Some(tools) = tool_calls {
                c = c.saturating_add(
                    state
                        .pricing_store
                        .calculate_tool_surcharge(tools, model, group),
                );
            }
            c
        }
        Err(e) => {
            tracing::warn!("billing: {e}, billing as 0");
            0
        }
    }
}
/// 计算渠道成本（配额单位）。
///
/// 该渠道配了该模型成本价 → 按成本价单价 * 全局倍率表算；
/// 未配成本价 → 直接回退为销售价 `cost`（日志利润列显示 "—"）。
/// 无渠道/渠道不存在 → 回退 `cost`。
#[allow(clippy::too_many_arguments)]
pub(crate) fn compute_channel_cost(
    state: &AppState,
    channel_id: Option<&str>,
    requested_model: &str,
    billing_model: &str,
    group: &str,
    prompt_tokens: u64,
    completion_tokens: u64,
    cost: i64,
) -> i64 {
    let Some(cid) = channel_id else {
        return cost;
    };
    let Some(ch) = state.channel_store.get(cid) else {
        return cost;
    };
    // 成本价 key = 映射后的上游模型名
    let Some(cp) = ch.cost_price_for(billing_model) else {
        return cost;
    };
    // 成本价与销售价共用同一倍率表（model_ratio/group_ratio），只换单价来源；
    // 模型倍率双名查找（对外名优先、上游名兜底），与销售侧计费口径一致
    let ratios = state.pricing_store.get_ratios();
    let base = if cp.price_type == "count" {
        cp.input_price
    } else {
        (prompt_tokens as f64 * cp.input_price + completion_tokens as f64 * cp.output_price)
            / 1000.0
    };
    let channel_cost = (base
        * ratios.model_ratio_aliased(requested_model, billing_model)
        * ratios.group_ratio(group)
        + 0.999999) as i64;
    // 有成本价但该模型成本算出来为 0（配置异常等）：回退到销售价，避免误显示利润
    if channel_cost == 0 {
        cost
    } else {
        channel_cost
    }
}
/// 计费扣减（带工具按次调用附加费）。
///
/// 与 `charge_usage` 相同，额外将 `tool_calls`（工具名 → 调用次数）按
/// `PricingStore::calculate_tool_surcharge` 折算为附加费并入总 cost。
#[allow(clippy::too_many_arguments)]
pub(crate) fn charge_usage_with_tools(
    state: &AppState,
    api_key: &crate::api::auth::ApiKey,
    requested_model: &str,
    model: &str,
    group: &str,
    prompt_tokens: u64,
    completion_tokens: u64,
    cached_tokens: u64,
    tool_calls: Option<&crate::pricing::ToolCallCounts>,
) -> i64 {
    // 计费额度裁决：按次订阅固定扣额度 / 按 token 量计费
    let cost = resolve_billing_quota(
        state,
        api_key,
        requested_model,
        model,
        group,
        prompt_tokens,
        completion_tokens,
        cached_tokens,
        tool_calls,
    );
    // Prometheus 成本指标：pricing 以 USD 计价，cost 即向上取整后的美元配额
    // （单位 1 美元），统一折算为微美元累加，供 /metrics 展示 aigx_cost_usd_total。
    if cost > 0 {
        crate::metrics::global().record_cost("usd", (cost as u64).saturating_mul(1_000_000));
        if let Some(uid) = &api_key.user_id {
            // P2 订阅化：直接扣路径同样优先扣活跃订阅池（最早到期的先用），
            // 池不足部分回退钱包。订阅禁 overflow 且池不足时整体拒绝。
            let now = chrono::Utc::now().timestamp();
            let mut remaining = cost;
            for sub in state.subscription_store.find_active(uid, now) {
                if remaining <= 0 {
                    break;
                }
                let pool = if sub.amount_total > 0 {
                    sub.amount_total - sub.amount_used
                } else {
                    remaining
                };
                let take = pool.min(remaining);
                if take > 0 && state.subscription_store.try_charge(&sub.id, take) {
                    remaining -= take;
                }
            }
            let charged_wallet = if remaining > 0 {
                if !state.subscription_store.wallet_overflow_allowed(uid, now) {
                    // 订阅禁兜底：直接拒绝（key 侧不扣，保持一致性）
                    if let Some(u) = state.user_store.get_by_id(uid) {
                        state
                            .notify_service
                            .notify_spawn(crate::notify::NotifyEvent::QuotaLow {
                                user_email: u.email.clone(),
                                remaining: 0,
                            });
                    }
                    return cost;
                }
                state.user_store.try_charge(uid, remaining)
            } else {
                true // 订阅池全额承接
            };
            // 额度不足或剩余过低通知（与非流式分支一致）
            if let Some(u) = state.user_store.get_by_id(uid) {
                let user_remaining = u.remaining();
                // 阈值：固定 1000 或 quota 的 10%，取较小者；扣费失败必通知
                let threshold = (u.quota / 10).clamp(1000, 10000);
                if !charged_wallet || user_remaining < threshold {
                    state
                        .notify_service
                        .notify_spawn(crate::notify::NotifyEvent::QuotaLow {
                            user_email: u.email.clone(),
                            remaining: user_remaining,
                        });
                }
            }
            // 问题 6：try_charge 失败时跳过 charge_quota，保持计费一致性
            if charged_wallet {
                let _ = state.api_key_store.charge_quota(&api_key.id, cost);
            }
        } else {
            let _ = state.api_key_store.charge_quota(&api_key.id, cost);
        }
    }
    cost
}
/// 预留记录（P1：两段式计费）
pub(crate) struct Reservation {
    pub reserved_user: i64,
    pub reserved_key: i64,
    /// 订阅池承接的部分（P2 订阅化：优先于用户钱包扣减，结算/释放时
    /// 按 SubscriptionStore 的预留语义同步归还）
    pub reserved_sub: i64,
    /// 各订阅分别承接的 (id, amount)（多订阅时逐个记录，防止
    /// 跨订阅结算/释放时金额张冠李戴——原先只留最后一个 ID，前面的
    /// 订阅池预留会被永久吞掉）
    pub sub_allocs: Vec<(String, i64)>,
}
/// 预留配额（P1：两段式计费第一步）。
///
/// 在请求发起前调用，估算 token 并预留相应配额。返回预留记录供后续结算使用。
/// 若预留失败（余额不足），返回错误。
pub(crate) fn reserve_usage(
    state: &AppState,
    api_key: &crate::api::auth::ApiKey,
    requested_model: &str,
    model: &str,
    group: &str,
    estimated_prompt_tokens: u64,
    estimated_completion_tokens: u64,
) -> Result<Reservation, (StatusCode, Json<Value>)> {
    let estimated_cost = resolve_billing_quota(
        state,
        api_key,
        requested_model,
        model,
        group,
        estimated_prompt_tokens,
        estimated_completion_tokens,
        // 预估阶段拿不到上游缓存量；结算侧（settle_usage）会用真实值重裁
        0,
        None,
    );

    if estimated_cost <= 0 {
        return Ok(Reservation {
            reserved_user: 0,
            reserved_key: 0,
            reserved_sub: 0,
            sub_allocs: Vec::new(),
        });
    }

    // P2 订阅化：优先尝试用活跃订阅池承接（最早到期的先用）。
    // 池不足的部分回退用户钱包（须订阅允许 overflow），零订阅时全走钱包。
    let mut remaining_cost = estimated_cost;
    let mut reserved_sub: i64 = 0;
    let mut sub_allocs: Vec<(String, i64)> = Vec::new();
    if let Some(uid) = &api_key.user_id {
        let now = chrono::Utc::now().timestamp();
        let subs = state.subscription_store.find_active(uid, now);
        for sub in &subs {
            let pool = if sub.amount_total > 0 {
                sub.amount_total - sub.amount_used
            } else {
                remaining_cost // 不限池：全部承接
            };
            if pool <= 0 {
                continue;
            }
            let take = pool.min(remaining_cost);
            if state.subscription_store.try_reserve(&sub.id, take) {
                reserved_sub += take;
                remaining_cost -= take;
                sub_allocs.push((sub.id.clone(), take));
                if remaining_cost <= 0 {
                    break;
                }
            }
        }
        // 池不够且订阅禁止钱包兜底 → 回滚已预留部分并拒绝
        if remaining_cost > 0 && !state.subscription_store.wallet_overflow_allowed(uid, now) {
            for (sid, amount) in &sub_allocs {
                state.subscription_store.release(sid, *amount);
            }
            return Err(error_response(
                "insufficient_quota",
                "Subscription quota insufficient",
                StatusCode::PAYMENT_REQUIRED,
            ));
        }
    }

    // 预留用户配额（订阅池承接后的剩余部分）
    let reserved_user = if remaining_cost > 0 {
        if let Some(uid) = &api_key.user_id {
            if !state.user_store.reserve_quota(uid, remaining_cost) {
                // 回滚订阅池预留
                for (sid, amount) in &sub_allocs {
                    state.subscription_store.release(sid, *amount);
                }
                return Err(error_response(
                    "insufficient_quota",
                    "User quota insufficient",
                    StatusCode::PAYMENT_REQUIRED,
                ));
            }
            remaining_cost
        } else {
            0
        }
    } else {
        0
    };

    // 预留 API key 配额
    let reserved_key = if !state
        .api_key_store
        .reserve_quota(&api_key.id, estimated_cost)
    {
        // 回滚用户预留——必须用 release 语义（G1：settle 的并发钳制
        // 可能归还到他人刚建立的预留；release 只解冻不产生消费）
        if reserved_user > 0 {
            if let Some(uid) = &api_key.user_id {
                state.user_store.release_quota(uid, reserved_user);
            }
        }
        return Err(error_response(
            "insufficient_quota",
            "API key quota insufficient",
            StatusCode::PAYMENT_REQUIRED,
        ));
    } else {
        estimated_cost
    };

    Ok(Reservation {
        reserved_user,
        reserved_key,
        reserved_sub,
        sub_allocs,
    })
}
/// 结算预留配额（P1：两段式计费第二步）。
///
/// 请求完成后调用，根据实际 token 消耗结算费用，释放多预留的部分。
// 参数是计费语义的完整集合（调用上下文 + 预留记录 + 实际消耗 + 工具附加费），
// 与 charge_usage_with_tools 同构，不为规避参数计数拆散语义
#[allow(clippy::too_many_arguments)]
pub(crate) fn settle_usage(
    state: &AppState,
    api_key: &crate::api::auth::ApiKey,
    requested_model: &str,
    model: &str,
    group: &str,
    reservation: &Reservation,
    actual_prompt_tokens: u64,
    actual_completion_tokens: u64,
    cached_tokens: u64,
    tool_calls: Option<&crate::pricing::ToolCallCounts>,
) -> i64 {
    let actual_cost = resolve_billing_quota(
        state,
        api_key,
        requested_model,
        model,
        group,
        actual_prompt_tokens,
        actual_completion_tokens,
        cached_tokens,
        tool_calls,
    );

    if actual_cost > 0 {
        crate::metrics::global().record_cost("usd", (actual_cost as u64).saturating_mul(1_000_000));
    }

    // P2 订阅化：结算订阅池预留（多订阅逐个结算）。
    // 实际消费按各订阅预留占比顺序承接：每个订阅先扣自身预留内的
    // 部分（sub_take），不足部分溢出到钱包侧。reserved_user 的 settle
    // 会把 reserved 释放并把差值入账——这里传 wallet_cost 作为钱包侧
    // 实际消费，保证总账不重不漏。
    let mut wallet_cost = actual_cost;
    for (sid, reserved) in &reservation.sub_allocs {
        let sub_take = (*reserved).min(wallet_cost);
        state.subscription_store.settle(sid, *reserved, sub_take);
        wallet_cost -= sub_take;
        if wallet_cost <= 0 {
            break;
        }
    }

    // 结算用户配额
    if reservation.reserved_user > 0 {
        if let Some(uid) = &api_key.user_id {
            state
                .user_store
                .settle_quota(uid, reservation.reserved_user, wallet_cost);
            // 余额通知
            if let Some(u) = state.user_store.get_by_id(uid) {
                let remaining = u.remaining();
                let threshold = (u.quota / 10).clamp(1000, 10000);
                if remaining < threshold {
                    state
                        .notify_service
                        .notify_spawn(crate::notify::NotifyEvent::QuotaLow {
                            user_email: u.email.clone(),
                            remaining,
                        });
                }
            }
        }
    }

    // 结算 API key 配额
    if reservation.reserved_key > 0 {
        state
            .api_key_store
            .settle_quota(&api_key.id, reservation.reserved_key, actual_cost);
    }

    actual_cost
}
/// 释放预留（P1：两段式计费的取消路径）。
///
/// 请求失败（渠道全败/客户端错误）时调用：按 `Reservation` 记录把
/// user 与 key 两侧的预留全部解冻归还。与 `settle_usage` 对称——
/// settle 是「结算消费」，release 是「无消费归还」。
/// 2026-09-08 二轮审查 G5：失败路径原先不释放，预留永久冻结。
pub(crate) fn release_reservation(
    state: &AppState,
    api_key: &crate::api::auth::ApiKey,
    reservation: &Reservation,
) {
    if reservation.reserved_user > 0 {
        if let Some(uid) = &api_key.user_id {
            state
                .user_store
                .release_quota(uid, reservation.reserved_user);
        }
    }
    if reservation.reserved_sub > 0 {
        for (sid, amount) in &reservation.sub_allocs {
            state.subscription_store.release(sid, *amount);
        }
    }
    if reservation.reserved_key > 0 {
        state
            .api_key_store
            .release_quota(&api_key.id, reservation.reserved_key);
    }
}
/// 流式计费共享状态：后缀事件（正常结束）与 Drop 守卫（断连兜底）共用。
///
/// 通过 `charged` 原子标志保证计费恰好执行一次：
/// - 流正常结束：后缀事件先 `swap` 抢到计费权并完成计费，守卫 drop 时直接跳过；
/// - 客户端断连：SSE 流被中途 drop，后缀事件不会执行，守卫在 Drop 中兜底计费。
///
/// 两条路径互斥，同时杜绝"双计费"与"断连零计费"。
///
/// 若客户端在计费完成前断连，守卫的 Drop 实现会负责兜底计费，确保请求最终被正确记账。
pub(crate) struct StreamBillingState {
    pub(crate) state: AppState,
    pub(crate) api_key: crate::api::auth::ApiKey,
    /// 映射后的上游模型名（= 价格表名），用户可见
    pub(crate) model: String,
    /// 用户原始请求模型名（admin 排查用，无映射时 None）
    pub(crate) origin_model: Option<String>,
    pub(crate) group: String,
    /// 请求快照（用于估算 prompt tokens）
    pub(crate) chat_req: ChatFormat,
    /// 工具按次调用计数表（流式过程中统计 tool_use 起始块）
    pub(crate) tool_calls: Arc<parking_lot::Mutex<crate::pricing::ToolCallCounts>>,
    /// 输出文本累积器（用于估算 completion tokens）
    pub(crate) acc: Arc<parking_lot::Mutex<String>>,
    /// 计费权标志：false=未计费，true=已计费
    pub(crate) charged: Arc<std::sync::atomic::AtomicBool>,
    /// 事后限流记账句柄（断连兜底路径使用 clone）
    pub(crate) rate_bundle: Option<crate::ratelimit::ReservationBundle>,
    pub(crate) request_start: std::time::Instant,
    pub(crate) client_ip: Option<String>,
    pub(crate) request_id: String,
    pub(crate) channel_id: Option<String>,
    /// 渠道名（admin 排查用）
    pub(crate) channel_name: Option<String>,
    /// P1-7：调度决策回放（流式路径补全）
    pub(crate) candidate_channels: Vec<String>,
    pub(crate) filtered_channels: Vec<crate::log::FilteredChannel>,
    /// P1：预留记录（两段式计费）
    pub(crate) reservation: Option<Reservation>,
    /// 日志快照：客户端请求体（log_body 开关开启时填充，None 时零开销）
    pub(crate) debug_request_body: Option<String>,
}
impl StreamBillingState {
    /// 估算 token 并执行计费三件套：usage 累计 + charge_usage + 请求日志。
    ///
    /// 正常结束与断连兜底两路径共用的收尾逻辑；
    /// `latency_ms` 记录真实耗时（B20：修复流式请求硬编码 0 的问题）。
    /// 返回 (prompt_tokens, completion_tokens) 供调用方提交限流记账。
    pub(crate) fn finalize(&self) -> (u64, u64) {
        let output_text = self.acc.lock().clone();
        let completion_tokens = crate::token_estimate::count_text(&self.model, &output_text) as u64;
        let prompt_tokens =
            crate::token_estimate::count_chat_prompt(&self.model, &self.chat_req) as u64;

        // 累计用量
        self.state
            .usage_tracker
            .accumulate(prompt_tokens, completion_tokens, 0, 0, 0, 0.0);

        // 扣费：P1 两段式——若存在预留记录则走 settle_usage，否则回退到旧路径
        let tool_calls = self.tool_calls.lock().clone();
        let tool_calls_opt = if tool_calls.is_empty() {
            None
        } else {
            Some(&tool_calls)
        };
        // 计价键：对外请求名（origin_model）优先，上游名兜底
        let requested_model = self.origin_model.as_deref().unwrap_or(&self.model);
        let cost = if let Some(reservation) = &self.reservation {
            settle_usage(
                &self.state,
                &self.api_key,
                requested_model,
                &self.model,
                &self.group,
                reservation,
                prompt_tokens,
                completion_tokens,
                // 本地估算路径没有上游 usage，无缓存量可言
                0,
                tool_calls_opt,
            )
        } else {
            charge_usage_with_tools(
                &self.state,
                &self.api_key,
                requested_model,
                &self.model,
                &self.group,
                prompt_tokens,
                completion_tokens,
                0,
                tool_calls_opt,
            )
        };

        // 记录请求日志（B20：真实耗时）
        let mut log = crate::log::RequestLog::new();
        log.user_id = self.api_key.user_id.clone();
        log.key_id = Some(self.api_key.id.clone());
        log.channel_id = self.channel_id.clone();
        log.channel_name = self.channel_name.clone();
        log.model = self.model.clone();
        log.origin_model = self.origin_model.clone();
        log.channel_cost = self.compute_channel_cost(prompt_tokens, completion_tokens, cost);
        log.input_tokens = prompt_tokens;
        log.output_tokens = completion_tokens;
        log.cost = cost;
        log.latency_ms = self.request_start.elapsed().as_millis() as u64;
        log.status_code = 200;
        log.ip = self.client_ip.clone();
        log.request_id = Some(self.request_id.clone());
        log.candidate_channels = self.candidate_channels.clone();
        log.filtered_channels = self.filtered_channels.clone();
        log.selected_channel = self.channel_id.clone();
        // 对话快照（**默认记录**，不依赖 log_body 开关）：管理员点开日志就能看到
        // 这轮对话说了什么，无需先去系统设置打开调试模式。
        {
            let mut conv = crate::log::LogConversation::from_chat_messages(&self.chat_req.messages);
            conv.push_assistant(&self.acc.lock());
            if !conv.is_empty() {
                log.conversation = Some(conv);
            }
        }
        // log_body 开关：快照 = 请求体 + 流式拼接文本（关闭时 None 零开销）
        if self.debug_request_body.is_some() {
            log.debug = crate::log::LogDebugSnapshot::new(
                self.debug_request_body.clone(),
                Some(self.acc.lock().clone()),
                None,
            );
        }
        self.state.log_store.record_request(log);

        (prompt_tokens, completion_tokens)
    }

    /// 计算渠道成本（配额单位）。配了成本价则按成本价算；未配则 = 销售价 cost
    /// （日志利润列显示 "—"）。
    fn compute_channel_cost(&self, prompt_tokens: u64, completion_tokens: u64, cost: i64) -> i64 {
        compute_channel_cost(
            &self.state,
            self.channel_id.as_deref(),
            self.origin_model.as_deref().unwrap_or(&self.model),
            &self.model,
            &self.group,
            prompt_tokens,
            completion_tokens,
            cost,
        )
    }
}
/// 流式计费 Drop 守卫（B05）。
///
/// 客户端断连时 SSE 流被 hyper 中途 drop，原实现的计费逻辑位于
/// `stream::once(final_event)` 后缀事件中，断连时后缀不执行 → 流式请求零计费。
/// 守卫随流存活（见 [`GuardedStream`]）：流被 drop 时守卫一并 drop，
/// 在 Drop 中原子抢占计费权——若后缀事件尚未计费则在此兜底执行。
pub(crate) struct StreamUsageGuard {
    billing: Arc<StreamBillingState>,
}
impl StreamUsageGuard {
    pub(crate) fn new(billing: Arc<StreamBillingState>) -> Self {
        Self { billing }
    }
}
/// 包装流并附加计费守卫（B05）。
///
/// 守卫作为字段随流存活：无论流被完整消费（正常结束，后缀事件已计费）
/// 还是被 hyper 中途 drop（客户端断连），守卫都会随之 drop 并触发兜底计费。
/// 守卫与后缀事件通过原子标志互斥，计费恰好执行一次。
///
/// 泛型 `G` 允许不同端点挂接各自的守卫类型（chat 的
/// StreamUsageGuard / responses 的 ResponsesStreamGuard）。
pub(crate) struct GuardedStream<S, G> {
    pub(crate) inner: std::pin::Pin<Box<S>>,
    pub(crate) _guard: G,
    /// 管理员「终止在途请求」的取消信号（None = 不参与取消）。
    ///
    /// 一旦取消，poll_next 直接返回 `Ready(None)` 结束下游 SSE 流 ——
    /// 上游连接随 `inner` 被 drop 而关闭（reqwest 的 body 流 drop 即断开），
    /// 客户端会看到 SSE 流提前结束，而不是一直挂着。
    ///
    /// 这里选「结束流」而不是「返回 error 帧」：对已开始输出的流，追加
    /// error 事件容易让客户端把它当业务错误重试；直接收尾更接近用户点
    /// 「停止生成」的语义。
    pub(crate) cancel: Option<crate::log::inflight::CancelSignal>,
    /// 在途请求守卫：随流存活，流结束时 Drop 自动从在途列表注销。
    /// 放在流里（而不是函数局部变量）是因为：函数返回响应时请求仍在跑，
    /// guard 若随函数 drop，管理员列表里就会立刻看不到这条请求。
    pub(crate) _inflight: Option<crate::log::inflight::InflightGuard>,
}
/// Responses 流式计费共享状态（透传方案中 StreamBillingState 的对应物）。
///
/// 与 chat 的差异：透传流不能往客户端 SSE 里塞自定义后缀事件，计费
/// 全部由 [`ResponsesStreamGuard`] 在流 drop 时执行——正常结束（流被
/// hyper 完整消费后 drop）与客户端断连（流被中途 drop）都经过 Drop，
/// 天然恰好一次，无需原子标志互斥。
pub(crate) struct ResponsesBillingState {
    pub(crate) state: AppState,
    pub(crate) api_key: crate::api::auth::ApiKey,
    /// 映射后的上游模型名（= 价格表名），用户可见
    pub(crate) model: String,
    /// 用户原始请求模型名（admin 排查用，无映射时 None）
    pub(crate) origin_model: Option<String>,
    pub(crate) group: String,
    /// 请求侧 input 文本快照（上游未上报 usage 时估算 prompt tokens 用）
    pub(crate) input_text: String,
    /// 上游 usage（input_tokens, output_tokens）——旁路解析
    /// `response.completed` 等终止事件的 `.response.usage` 填充
    pub(crate) usage: Arc<parking_lot::Mutex<Option<(u64, u64)>>>,
    /// 输出文本累积器（估算 completion tokens 兜底）
    pub(crate) acc: Arc<parking_lot::Mutex<String>>,
    /// 事后限流记账句柄
    pub(crate) rate_bundle: Option<crate::ratelimit::ReservationBundle>,
    pub(crate) request_start: std::time::Instant,
    pub(crate) client_ip: Option<String>,
    pub(crate) request_id: String,
    pub(crate) channel_id: Option<String>,
    /// 渠道名（admin 排查用）
    pub(crate) channel_name: Option<String>,
    /// 日志快照：客户端请求体（log_body 开关开启时填充，None 时零开销）
    pub(crate) debug_request_body: Option<String>,
}
impl ResponsesBillingState {
    /// 计费三件套：usage 累计 + charge_usage + 请求日志。
    ///
    /// 上游 usage（权威值）优先；缺失的侧逐项回退 token 估算
    /// （能估就估，拿不到就 0）。返回 (prompt_tokens, completion_tokens)
    /// 供限流记账。
    pub(crate) fn finalize(&self) -> (u64, u64) {
        let (mut prompt_tokens, mut completion_tokens) = self.usage.lock().unwrap_or((0, 0));
        let output_text = self.acc.lock().clone();
        // 上游未上报 usage 的侧逐项估算
        if prompt_tokens == 0 {
            prompt_tokens = crate::token_estimate::count_text(&self.model, &self.input_text) as u64;
        }
        if completion_tokens == 0 {
            completion_tokens = crate::token_estimate::count_text(&self.model, &output_text) as u64;
        }

        // 累计用量
        self.state
            .usage_tracker
            .accumulate(prompt_tokens, completion_tokens, 0, 0, 0, 0.0);

        // 扣费（try_charge + QuotaLow 通知 + key 扣减）
        let cost = charge_usage(
            &self.state,
            &self.api_key,
            self.origin_model.as_deref().unwrap_or(&self.model),
            &self.model,
            &self.group,
            prompt_tokens,
            completion_tokens,
            // usage 元组只带 (prompt, completion)，无缓存量
            0,
        );

        // 记录请求日志（真实耗时）
        let mut log = crate::log::RequestLog::new();
        log.user_id = self.api_key.user_id.clone();
        log.key_id = Some(self.api_key.id.clone());
        log.channel_id = self.channel_id.clone();
        log.channel_name = self.channel_name.clone();
        log.model = self.model.clone();
        log.origin_model = self.origin_model.clone();
        log.channel_cost = compute_channel_cost(
            &self.state,
            self.channel_id.as_deref(),
            self.origin_model.as_deref().unwrap_or(&self.model),
            &self.model,
            &self.group,
            prompt_tokens,
            completion_tokens,
            cost,
        );
        log.input_tokens = prompt_tokens;
        log.output_tokens = completion_tokens;
        log.cost = cost;
        log.latency_ms = self.request_start.elapsed().as_millis() as u64;
        log.status_code = 200;
        log.ip = self.client_ip.clone();
        log.request_id = Some(self.request_id.clone());
        // log_body：快照 = 请求体 + 流式拼接文本（关闭时 None 零开销）
        if self.debug_request_body.is_some() {
            log.debug = crate::log::LogDebugSnapshot::new(
                self.debug_request_body.clone(),
                Some(self.acc.lock().clone()),
                None,
            );
        }
        self.state.log_store.record_request(log);

        (prompt_tokens, completion_tokens)
    }
}
/// Responses 流式计费 Drop 守卫（透传方案）。
///
/// 随透传字节流存活（见泛型化的 [`GuardedStream`]）：流被 drop 时
/// （正常结束或客户端断连）在 Drop 中执行计费。Drop 中不能 await：
/// 计费本身（usage 提取/token 估算/扣费/日志）全为同步操作，异步的
/// 事后限流记账交由后台任务补交（同 StreamUsageGuard）。
pub(crate) struct ResponsesStreamGuard {
    pub(crate) billing: Arc<ResponsesBillingState>,
}


impl Drop for StreamUsageGuard {
    fn drop(&mut self) {
        // 原子抢占计费权：仅当后缀事件未执行计费时（swap 返回 false）才兜底
        if self
            .billing
            .charged
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            return; // 正常路径已计费，无需重复
        }
        // Drop 中不能 await：计费本身（token 估算/扣费/日志）全为同步操作，
        // 直接执行；异步的事后限流记账交由后台任务补交。
        let (prompt_tokens, completion_tokens) = self.billing.finalize();
        // Prometheus 指标（断连兜底路径上报；正常路径由后缀事件上报）
        crate::metrics::global().record_request(
            &self.billing.model,
            self.billing.channel_id.as_deref().unwrap_or("unknown"),
            "ok",
            self.billing.request_start.elapsed().as_millis() as u64,
        );
        crate::metrics::global().record_tokens(&self.billing.model, "prompt", prompt_tokens);
        crate::metrics::global().record_tokens(
            &self.billing.model,
            "completion",
            completion_tokens,
        );
        if let Some(bundle) = self.billing.rate_bundle.clone() {
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    bundle
                        .commit_tokens(prompt_tokens + completion_tokens)
                        .await;
                });
            }
        }
    }
}


// G 仅需 Unpin：poll_next 通过 get_mut 访问 inner，要求整个结构体
// Unpin；两个守卫（StreamUsageGuard/ResponsesStreamGuard）只含 Arc，
// 天然满足。
impl<S: futures::Stream, G: std::marker::Unpin> futures::Stream for GuardedStream<S, G> {
    type Item = S::Item;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        // GuardedStream 所有字段均为 Unpin（Pin<Box<S>> 与普通结构体），可安全 get_mut
        let this = self.get_mut();
        if this.cancel.as_ref().is_some_and(|c| c.is_cancelled()) {
            return std::task::Poll::Ready(None);
        }
        this.inner.as_mut().poll_next(cx)
    }
}

impl Drop for ResponsesStreamGuard {
    fn drop(&mut self) {
        let (prompt_tokens, completion_tokens) = self.billing.finalize();
        // Prometheus 指标（透传流没有后缀事件，Drop 即唯一上报点）
        crate::metrics::global().record_request(
            &self.billing.model,
            self.billing.channel_id.as_deref().unwrap_or("unknown"),
            "ok",
            self.billing.request_start.elapsed().as_millis() as u64,
        );
        crate::metrics::global().record_tokens(&self.billing.model, "prompt", prompt_tokens);
        crate::metrics::global().record_tokens(
            &self.billing.model,
            "completion",
            completion_tokens,
        );
        if let Some(bundle) = self.billing.rate_bundle.clone() {
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    bundle
                        .commit_tokens(prompt_tokens + completion_tokens)
                        .await;
                });
            }
        }
    }
}
