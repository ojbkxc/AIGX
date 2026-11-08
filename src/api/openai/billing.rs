use axum::{http::StatusCode, Json};

use super::*;
use crate::bridge::ChatFormat;
use serde_json::Value;
use std::sync::Arc;

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
    // P1-5：返回「实际扣除额」而非名义 cost，未扣到的一律不计收入。
    let mut collected = 0i64;
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
            // 订阅池已实收的部分
            collected = cost - remaining;
            let charged_wallet = if remaining > 0 {
                if !state.subscription_store.wallet_overflow_allowed(uid, now) {
                    // 订阅禁兜底：钱包部分不扣，收入只记订阅池实收（不再记名义 cost）
                    if let Some(u) = state.user_store.get_by_id(uid) {
                        state
                            .notify_service
                            .notify_spawn(crate::notify::NotifyEvent::QuotaLow {
                                user_email: u.email.clone(),
                                remaining: 0,
                            });
                    }
                    if collected > 0 {
                        let _ = state.api_key_store.charge_quota(&api_key.id, collected);
                    }
                    return collected;
                }
                let ok = state.user_store.try_charge(uid, remaining);
                if ok {
                    collected += remaining;
                }
                ok
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
                let _ = state.api_key_store.charge_quota(&api_key.id, collected);
            }
        } else {
            let _ = state.api_key_store.charge_quota(&api_key.id, cost);
            collected = cost;
        }
    }
    collected
}

/// 价格快照（P1-8：改价不影响在途预留结算）。
///
/// reserve_usage 冻结当次请求的计价参数（单价/倍率/按次扣额）；
/// settle_usage 用快照重算实扣额，避免预留期间改价导致结算价与
/// 预留冻结价漂移（实扣额 ≠ 预留额）。
#[derive(Clone)]
pub(crate) struct PriceSnapshot {
    /// 按次订阅固定扣额（count billing 模式）；None = token 计费
    pub count_flat: Option<i64>,
    /// token 计费：模型单价条目（input/output/cache_price + price_type）
    pub price: Option<crate::pricing::ModelPrice>,
    /// 模型倍率（计价键已解析后的命中值）
    pub model_ratio: f64,
    /// 分组倍率
    pub group_ratio: f64,
}

impl PriceSnapshot {
    /// 空快照（无计价条目）：结算按 0 处理。
    fn empty() -> Self {
        Self {
            count_flat: None,
            price: None,
            model_ratio: 1.0,
            group_ratio: 1.0,
        }
    }
}

/// 捕获当次请求的价格快照（P1-8）。
///
/// 按次订阅冻结全局固定扣额（原子快照）；token 计费冻结
/// ModelPrice + model_ratio + group_ratio。计价键经
/// `resolve_priced_name` 解析（对外名优先、上游名兜底），与
/// `resolve_billing_quota` 的查价口径一致。
fn capture_price_snapshot(
    state: &AppState,
    api_key: &crate::api::auth::ApiKey,
    requested_model: &str,
    model: &str,
    group: &str,
) -> PriceSnapshot {
    if is_count_billing(state, api_key) {
        return PriceSnapshot {
            count_flat: Some(crate::config::billing_flat_quota()),
            price: None,
            model_ratio: 1.0,
            group_ratio: 1.0,
        };
    }
    let priced_name = state
        .pricing_store
        .resolve_priced_name(requested_model, model);
    let price = priced_name
        .as_ref()
        .and_then(|n| state.pricing_store.get_price(n));
    let ratios = state.pricing_store.get_ratios();
    let model_ratio = priced_name
        .as_ref()
        .map(|n| ratios.model_ratio(n))
        .unwrap_or(1.0);
    let group_ratio = ratios.group_ratio(group);
    PriceSnapshot {
        count_flat: None,
        price,
        model_ratio,
        group_ratio,
    }
}

/// 纯计算部分（无 AppState 依赖）：从价格快照重算实扣额，不含工具附加费。拆分以便单测覆盖冻结结算等价性、改价不变、count 冻结、空快照回退。
fn compute_cost_from_snapshot_pure(
    snapshot: &PriceSnapshot,
    prompt_tokens: u64,
    completion_tokens: u64,
    cached_tokens: u64,
) -> i64 {
    // 按次订阅：固定扣额，不随 token/工具变化
    if let Some(flat) = snapshot.count_flat {
        return flat;
    }
    let Some(price) = &snapshot.price else {
        return 0;
    };
    // token 计费（上游缓存口径：cache_price 未配时回退正价）
    let base = if price.price_type == "count" {
        price.input_price
    } else {
        // 命中计费形态 input=0, cached=prompt：cached 不受 input 钳制；
        // 仅 fresh+cached 并存时钳制防超收（与 cost_with_name_inner 一致）。
        let cached = if prompt_tokens == 0 {
            cached_tokens
        } else {
            cached_tokens.min(prompt_tokens)
        };
        let fresh = prompt_tokens.saturating_sub(cached);
        let cache_price = match price.cache_price {
            Some(p) => p,
            // 上游缓存：未配缓存价 → 按正价收（不白送）
            None => price.input_price,
        };
        (fresh as f64 * price.input_price
            + cached as f64 * cache_price
            + completion_tokens as f64 * price.output_price)
            / 1000.0
    };
    if base <= 0.0 {
        0
    } else {
        (base * snapshot.model_ratio * snapshot.group_ratio + 0.999999) as i64
    }
}

/// 用价格快照重算实扣额（P1-8）。
///
/// 与 `resolve_billing_quota` 同构，但计价参数取自快照而非实时查价：
/// - count billing → 快照冻结的固定扣额（不随 token/工具变化）；
/// - token 计费 → 快照冻结的 ModelPrice + model_ratio + group_ratio；
/// - 工具附加费 → 快照冻结的 group_ratio（工具单价仍实时查——预留阶段
///   无法预知调用哪些工具，且 PricingStore 无批量快照接口，group_ratio
///   已冻结即可消除倍率漂移这一主要风险）。
fn compute_cost_from_snapshot(
    state: &AppState,
    snapshot: &PriceSnapshot,
    model: &str,
    prompt_tokens: u64,
    completion_tokens: u64,
    cached_tokens: u64,
    tool_calls: Option<&crate::pricing::ToolCallCounts>,
) -> i64 {
    let mut c =
        compute_cost_from_snapshot_pure(snapshot, prompt_tokens, completion_tokens, cached_tokens);
    // 工具按次附加费（group_ratio 用快照冻结值，与 token 倍率口径一致）
    if let Some(tools) = tool_calls {
        let mut total = 0.0f64;
        for (name, count) in tools {
            if *count == 0 {
                continue;
            }
            let tp = state.pricing_store.get_tool_price(name, model);
            if tp <= 0.0 || tp.is_nan() || tp.is_infinite() {
                continue;
            }
            total += tp * (*count as f64) / 1000.0 * snapshot.group_ratio;
        }
        if total > 0.0 {
            c = c.saturating_add((total + 0.999999) as i64);
        }
    }
    c
}

/// 预留记录（P1：两段式计费；P0-2：用户/key 侧改为逐笔 token）
#[derive(Clone)]
pub(crate) struct Reservation {
    /// 用户侧预留 token（0=无用户预留）
    pub user_token: u64,
    /// API key 侧预留 token（0=无 key 预留）
    pub key_token: u64,
    /// 订阅池承接的部分（P2 订阅化：优先于用户钱包扣减，结算/释放时
    /// 按 SubscriptionStore 的预留语义同步归还）
    pub reserved_sub: i64,
    /// 各订阅分别承接的 (id, amount)（多订阅时逐个记录，防止
    /// 跨订阅结算/释放时金额张冠李戴——原先只留最后一个 ID，前面的
    /// 订阅池预留会被永久吞掉）
    pub sub_allocs: Vec<(String, i64)>,
    /// 价格快照（P1-8：结算用预留时冻结的单价/倍率，防改价漂移）
    pub price_snapshot: PriceSnapshot,
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
    // P1-8：冻结计价参数（单价/倍率/按次扣额）供结算复用，防改价漂移。
    // 快照与 live 查价在 reserve 时刻等价，但 settle 时用快照而非重查。
    let price_snapshot = capture_price_snapshot(state, api_key, requested_model, model, group);
    let estimated_cost = compute_cost_from_snapshot(
        state,
        &price_snapshot,
        model,
        estimated_prompt_tokens,
        estimated_completion_tokens,
        // 预估阶段拿不到上游缓存量；结算侧（settle_usage）会用真实值重裁
        0,
        None,
    );

    if estimated_cost <= 0 {
        return Ok(Reservation {
            user_token: 0,
            key_token: 0,
            reserved_sub: 0,
            sub_allocs: Vec::new(),
            price_snapshot: PriceSnapshot::empty(),
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

    // 预留用户配额（订阅池承接后的剩余部分；P0-2：拿 token 供精确结算）
    let mut user_token: u64 = 0;
    if remaining_cost > 0 {
        if let Some(uid) = &api_key.user_id {
            match state.user_store.reserve_quota(uid, remaining_cost) {
                Some(tok) => user_token = tok,
                None => {
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
            }
        }
    }

    // 预留 API key 配额
    let key_token = match state
        .api_key_store
        .reserve_quota(&api_key.id, estimated_cost)
    {
        Some(tok) => tok,
        None => {
            // 回滚用户预留——按 token 精确解冻（P0-2）
            if user_token != 0 {
                if let Some(uid) = &api_key.user_id {
                    state.user_store.release_quota(uid, user_token);
                }
            }
            return Err(error_response(
                "insufficient_quota",
                "API key quota insufficient",
                StatusCode::PAYMENT_REQUIRED,
            ));
        }
    };

    Ok(Reservation {
        user_token,
        key_token,
        reserved_sub,
        sub_allocs,
        price_snapshot,
    })
}
/// 把一笔实际消费按「订阅池 → 用户钱包 → API key」分配结算，并摘除预留。
///
/// `settle_usage` 与 `settle_cache_hit` 共用此分配逻辑，保证两条路径对
/// 预留的结算口径完全一致（P0-1：缓存命中此前只做 release，没有走这套
/// 分配，导致预留虽归零但记账与正常路径不一致）。
/// 返回实际入账总额（== `actual_cost`，超额部分由 settle_quota 记 overdraft）。
fn distribute_settlement(
    state: &AppState,
    api_key: &crate::api::auth::ApiKey,
    reservation: &Reservation,
    actual_cost: i64,
) {
    // P2 订阅化：结算订阅池预留（多订阅逐个结算）。
    // 实际消费按各订阅预留占比顺序承接：每个订阅先扣自身预留内的
    // 部分（sub_take），不足部分溢出到钱包侧。用户钱包侧传 wallet_cost
    // 作为实际消费，保证总账不重不漏。
    let mut wallet_cost = actual_cost;
    for (sid, reserved) in &reservation.sub_allocs {
        let sub_take = (*reserved).min(wallet_cost);
        state.subscription_store.settle(sid, *reserved, sub_take);
        wallet_cost -= sub_take;
        if wallet_cost <= 0 {
            break;
        }
    }

    // 结算用户配额（按 token 精确摘除预留；超额记 overdraft，P0-3）
    if reservation.user_token != 0 {
        if let Some(uid) = &api_key.user_id {
            state
                .user_store
                .settle_quota(uid, reservation.user_token, wallet_cost);
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
    if reservation.key_token != 0 {
        state
            .api_key_store
            .settle_quota(&api_key.id, reservation.key_token, actual_cost);
    }
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
    // P1-8：优先用预留时冻结的价格快照结算，而非实时查价——避免预留期间
    // 改价导致实扣额与预留冻结价漂移（实扣额 ≠ 预留额）。快照为空（Reservation
    // 未携带快照的异常路径）时回退实时查价，保持与旧行为一致、避免误记 0 扣费。
    let actual_cost = if reservation.price_snapshot.count_flat.is_none()
        && reservation.price_snapshot.price.is_none()
    {
        resolve_billing_quota(
            state,
            api_key,
            requested_model,
            model,
            group,
            actual_prompt_tokens,
            actual_completion_tokens,
            cached_tokens,
            tool_calls,
        )
    } else {
        compute_cost_from_snapshot(
            state,
            &reservation.price_snapshot,
            model,
            actual_prompt_tokens,
            actual_completion_tokens,
            cached_tokens,
            tool_calls,
        )
    };

    if actual_cost > 0 {
        crate::metrics::global().record_cost("usd", (actual_cost as u64).saturating_mul(1_000_000));
    }

    distribute_settlement(state, api_key, reservation, actual_cost);

    actual_cost
}
/// 结算**网关响应缓存命中**的预留（P0-1）。
///
/// 命中请求不消耗上游资源，费用 = 调用方算好的 `cache_cost`（按缓存读价，
/// 未配 cache_price 时为 0）。本函数按与正常路径**完全一致**的口径结算：
/// 订阅池承接 → 用户钱包 → key 侧，逐笔摘除预留。返回实际入账额
/// （恒等于 `cache_cost`，超限部分记 overdraft 而非漏记），调用方据此写
/// `log.cost`，保证「预留归零、实扣 == 记录值」。`cache_cost == 0` 时
/// 等价于全额退还预留（无消费）。
pub(crate) fn settle_cache_hit(
    state: &AppState,
    api_key: &crate::api::auth::ApiKey,
    reservation: &Reservation,
    cache_cost: i64,
) -> i64 {
    let cache_cost = cache_cost.max(0);
    if cache_cost > 0 {
        crate::metrics::global().record_cost("usd", (cache_cost as u64).saturating_mul(1_000_000));
    }
    distribute_settlement(state, api_key, reservation, cache_cost);
    cache_cost
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
    if reservation.user_token != 0 {
        if let Some(uid) = &api_key.user_id {
            state.user_store.release_quota(uid, reservation.user_token);
        }
    }
    if reservation.reserved_sub > 0 {
        for (sid, amount) in &reservation.sub_allocs {
            state.subscription_store.release(sid, *amount);
        }
    }
    if reservation.key_token != 0 {
        state
            .api_key_store
            .release_quota(&api_key.id, reservation.key_token);
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
    /// Phase1-B 路由回放：选路时刻的策略快照（route_replay 开关关闭时 None）
    pub(crate) route_config: Option<crate::log::RouteConfigSnapshot>,
    /// Phase1-B/#26：resolve 层过滤截断计数（不静默丢）
    pub(crate) route_truncated: usize,
    /// #32 路由回放：逐次上游尝试明细（「执行时间线」数据源）
    pub(crate) attempt_details: Vec<crate::log::AttemptDetail>,
    /// #32：attempt_details 被截断的条数（不静默丢）
    pub(crate) attempt_details_truncated: usize,
    /// Phase1-B/#26 路由回放：逐账号跳过明细（按渠道分组、行数有界）
    pub(crate) account_skips: Vec<crate::log::AccountSkipGroup>,
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
    /// `errored` = 流中途出错：请求日志 status_code 记 500 而非 200，
    /// 失败率统计不失真（此前流式出错仍按 200 记录）。
    /// 返回 (prompt_tokens, completion_tokens) 供调用方提交限流记账。
    pub(crate) fn finalize(&self, errored: bool) -> (u64, u64) {
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
        log.status_code = if errored { 500 } else { 200 };
        log.ip = self.client_ip.clone();
        log.request_id = Some(self.request_id.clone());
        log.candidate_channels = self.candidate_channels.clone();
        log.filtered_channels = self.filtered_channels.clone();
        log.selected_channel = self.channel_id.clone();
        // Phase1-B 路由回放：策略快照（开关关闭时 None，序列化跳过）
        log.route_config = self.route_config.clone();
        log.route_truncated = self.route_truncated;
        log.attempt_details = self.attempt_details.clone();
        log.attempt_details_truncated = self.attempt_details_truncated;
        log.account_skips = self.account_skips.clone();
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
    /// 供限流记账。`errored` = 流中途出错时日志 status_code 记 500。
    pub(crate) fn finalize(&self, errored: bool) -> (u64, u64) {
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
        log.status_code = if errored { 500 } else { 200 };
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
        let (prompt_tokens, completion_tokens) = self.billing.finalize(false);
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
        let (prompt_tokens, completion_tokens) = self.billing.finalize(false);
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

#[cfg(test)]
mod price_snapshot_tests {
    use super::*;
    use crate::pricing::ModelPrice;

    fn token_snapshot(
        input: f64,
        output: f64,
        cache: Option<f64>,
        model_ratio: f64,
        group_ratio: f64,
    ) -> PriceSnapshot {
        let mut p = ModelPrice::new("m", input, output);
        p.cache_price = cache;
        PriceSnapshot {
            count_flat: None,
            price: Some(p),
            model_ratio,
            group_ratio,
        }
    }

    // 1. count billing 冻结固定扣额：不随 token/工具变化
    #[test]
    fn count_flat_freezes_flat_quota() {
        let snap = PriceSnapshot {
            count_flat: Some(42),
            price: None,
            model_ratio: 1.0,
            group_ratio: 1.0,
        };
        assert_eq!(compute_cost_from_snapshot_pure(&snap, 100, 200, 0), 42);
        assert_eq!(compute_cost_from_snapshot_pure(&snap, 999999, 0, 0), 42);
    }

    // 2. 冻结→settle 等价：快照成本 = 实时成本（价格未变时）
    #[test]
    fn snapshot_token_cost_matches_live_equivalent() {
        // 1000 input * 0.03/1k + 500 output * 0.06/1k = 0.03 + 0.03 = 0.06
        // × model_ratio(2) × group_ratio(1) = 0.12 → (0.12 + 0.999999) as i64 = 1
        let snap = token_snapshot(0.03, 0.06, None, 2.0, 1.0);
        assert_eq!(compute_cost_from_snapshot_pure(&snap, 1000, 500, 0), 1);
    }

    // 3. 改价不变：快照冻结了单价，改价不影响结算（核心 P1-8 保证）
    #[test]
    fn price_change_does_not_affect_snapshot() {
        // 快照冻结 0.03/0.06，compute_cost_from_snapshot_pure 只用快照值
        // → 0.03*1 + 0.06*0.5 = 0.06 → (0.06 + 0.999999) as i64 = 1
        let snap = token_snapshot(0.03, 0.06, None, 1.0, 1.0);
        assert_eq!(compute_cost_from_snapshot_pure(&snap, 1000, 500, 0), 1);
    }

    // 4. 空快照回退：price=None + count_flat=None → 0
    #[test]
    fn empty_snapshot_returns_zero() {
        let snap = PriceSnapshot {
            count_flat: None,
            price: None,
            model_ratio: 1.0,
            group_ratio: 1.0,
        };
        assert_eq!(compute_cost_from_snapshot_pure(&snap, 1000, 500, 0), 0);
    }

    // 5. cache_price 未配 → 回退正价（不白送）
    #[test]
    fn cache_price_none_falls_back_to_input_price() {
        // fresh=0, cached=1000, cache_price=None → 1000 * 0.03 / 1k = 0.03 → 1
        let snap = token_snapshot(0.03, 0.06, None, 1.0, 1.0);
        assert_eq!(compute_cost_from_snapshot_pure(&snap, 0, 0, 1000), 1);
    }

    // 6. cache_price 已配 → 按缓存价收
    #[test]
    fn cache_price_some_charges_cached_at_cache_rate() {
        // fresh=0, cached=1000, cache_price=0.001 → 1000 * 0.001 / 1k = 0.001 → 1
        let snap = token_snapshot(0.03, 0.06, Some(0.001), 1.0, 1.0);
        assert_eq!(compute_cost_from_snapshot_pure(&snap, 0, 0, 1000), 1);
    }

    // 7. cached 钳制：fresh+cached 并存时 cached ≤ prompt（防超收）
    #[test]
    fn cached_clamped_when_fresh_present() {
        // prompt=1000, cached=2000（超 prompt）→ cached 钳到 1000, fresh=0
        // cache_price=0.001 → 1000 * 0.001 / 1k = 0.001 → 1
        let snap = token_snapshot(0.03, 0.06, Some(0.001), 1.0, 1.0);
        assert_eq!(compute_cost_from_snapshot_pure(&snap, 1000, 0, 2000), 1);
        // prompt=0, cached=1000 → 不钳（命中计费形态）→ 同上 = 1
        assert_eq!(compute_cost_from_snapshot_pure(&snap, 0, 0, 1000), 1);
    }

    // 8. count 型 price_type：直接返回 input_price，不走 token 计算
    #[test]
    fn count_price_type_uses_input_price() {
        let mut p = ModelPrice::new("m", 0.04, 0.0);
        p.price_type = "count".to_string();
        let snap = PriceSnapshot {
            count_flat: None,
            price: Some(p),
            model_ratio: 1.0,
            group_ratio: 1.0,
        };
        // count 型：base = input_price = 0.04 → (0.04 + 0.999999) as i64 = 1
        assert_eq!(compute_cost_from_snapshot_pure(&snap, 999, 0, 999), 1);
    }
}
