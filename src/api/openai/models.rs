use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    Json,
};

use super::*;
use serde_json::Value;

/// 模型清单的**唯一聚合实现**（`/v1/models` 与控制台两个模型下拉共用）。
///
/// 为什么必须共用：客户端通过 `/v1/models` 看到的清单，与控制台里「聊天」/
/// 「AI 运维」两个下拉能选的模型必须是同一份 —— 否则会出现「客户端调得通、
/// 下拉里选不到」（或反过来「下拉能选、调用报 model 不存在」）的错位。
/// 此前 `/api/models/available` 是第二份实现，缺了别名与自定义模型登记，
/// 这个函数就是把两份收敛成一份。
///
/// 聚合口径（顺序即去重优先级）：
/// 1. 启用且未冷却/未熔断的渠道：`models` 非空 = 白名单，空 = 用发现快照；
///    账号型渠道按账号的 `models`（空则回落到渠道声明）；
/// 2. 聚合为空时对「留空=全部」的渠道做一次懒发现（写回 `discovered_models`）；
/// 3. 全局别名映射的**对外名**（关掉的别名不算 —— 列出来会让客户端看得到调不通）；
/// 4. 管理员登记的自定义模型。
///
/// 返回 `(模型名, owned_by)`；去重已在内部完成，调用方可直接二次过滤
/// （例如按用户分组白名单收窄、附加价格字段）。
pub(crate) async fn collect_available_models(
    state: &AppState,
) -> Vec<(String, Option<&'static str>)> {
    let mut seen = std::collections::HashSet::new();
    let mut model_list: Vec<(String, Option<&'static str>)> = Vec::new();

    let channels = state.channel_store.list();
    for ch in &channels {
        if !ch.is_enabled() {
            continue;
        }
        // 与调度口径一致：冷却/断路器打开的故障渠道不参与模型列表聚合，
        // 避免上游故障把模型从列表里吞掉导致客户端误判模型下线
        if state.channel_store.is_in_cooldown(&ch.id)
            || !state
                .channel_store
                .circuit_breaker()
                .allows_read_only(&ch.id)
        {
            continue;
        }
        // 账号型渠道：模型来自账号（与统一模型管理页同口径）
        if ch.channel_type == crate::channel::ChannelType::Account {
            for session in state.account_sessions.for_channel(&ch.id) {
                if session.status != "enabled" {
                    continue;
                }
                let models: Vec<String> = if session.models.is_empty() {
                    ch.effective_models().to_vec()
                } else {
                    session.models.clone()
                };
                for m in models {
                    if m.is_empty()
                        || state.model_rules.is_disabled(&session.provider, &m)
                        || !seen.insert(m.clone())
                    {
                        continue;
                    }
                    model_list.push((m, None));
                }
            }
            continue;
        }
        let channel_owned_by =
            crate::model::metadata::owned_by_for_channel_type(ch.channel_type.as_str());
        // models 非空 = 白名单（只暴露声明部分）；空 = 全部（用发现快照兜底）
        for m in ch.effective_models() {
            if m.is_empty() {
                continue;
            }
            // 统一模型管理里按 (provider, model) 关掉的模型不对外暴露。
            // 判断放在 seen 之前：另一家渠道若仍开着，仍能正常进列表。
            if state.model_rules.is_disabled(ch.channel_type.as_str(), m) {
                continue;
            }
            if !seen.insert(m.clone()) {
                continue;
            }
            model_list.push((m.clone(), channel_owned_by));
        }
    }

    // 聚合为空时对"留空=全部"的启用且非冷却渠道做一次懒加载，
    // 调上游 /models 发现并写回 discovered_models。
    if model_list.is_empty() {
        let empty_channels: Vec<crate::channel::Channel> = channels
            .iter()
            .filter(|c| {
                c.is_enabled()
                    && c.models.is_empty()
                    && c.discovered_models.is_empty()
                    && !state.channel_store.is_in_cooldown(&c.id)
                    && state
                        .channel_store
                        .circuit_breaker()
                        .allows_read_only(&c.id)
            })
            .cloned()
            .collect();
        for ch in &empty_channels {
            // 账号型渠道无渠道级凭证，模型发现按账号走管理面账号测试
            if ch.channel_type == crate::channel::ChannelType::Account {
                continue;
            }
            let key = ch.decode_api_key();
            if ch.base_url.trim().is_empty()
                && ch.channel_type != crate::channel::ChannelType::Cloudflare
                && ch.channel_type != crate::channel::ChannelType::Gemini
                && ch.channel_type != crate::channel::ChannelType::Zai
            {
                continue;
            }
            // 5 秒内同渠道不重复拉上游，避免并发首启放大请求
            if !state.channel_store.try_mark_lazy_discover(&ch.id) {
                continue;
            }
            match crate::api::admin::fetch_upstream_models(
                state,
                ch.channel_type,
                &ch.base_url,
                &key,
                &ch.quirks,
            )
            .await
            {
                Ok((_url, discovered)) => {
                    if !discovered.is_empty() {
                        if let Err(e) = state
                            .channel_store
                            .save_discovered_models(&ch.id, discovered.clone())
                        {
                            tracing::error!("lazy discover save failed for {}: {e}", ch.id);
                        }
                        let channel_owned_by = crate::model::metadata::owned_by_for_channel_type(
                            ch.channel_type.as_str(),
                        );
                        for m in &discovered {
                            if m.is_empty() || !seen.insert(m.clone()) {
                                continue;
                            }
                            model_list.push((m.clone(), channel_owned_by));
                        }
                    }
                }
                Err(e) => tracing::warn!("lazy discover failed for channel {}: {:?}", ch.id, e),
            }
        }
    }
    // 别名映射的对外名也暴露（供客户端按别名调用；别名无渠道归属，纯名字推断）。
    // 用 active_mappings：关掉的别名不参与改名，列出来会让客户端「看得到调不通」。
    for name in state.model_mapper.active_mappings().keys() {
        if seen.insert(name.clone()) {
            model_list.push((name.clone(), None));
        }
    }
    for entry in state.model_mapper.custom_models() {
        if entry.id.is_empty() || !seen.insert(entry.id.clone()) {
            continue;
        }
        let owned_by = crate::model::metadata::owned_by_for_channel_type(&entry.provider);
        model_list.push((entry.id, owned_by));
    }
    model_list
}

/// GET /v1/models - 模型列表（客户端可见的对外清单）
pub(crate) async fn handle_list_models(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    // 完整校验（状态/过期/额度/IP/用户余额），但跳过模型白名单：
    // 列举请求没有具体 model。原来用 verify_api_key 只查存在性，
    // 过期/禁用的 Key 也能读到模型清单。
    verify_api_key_listing(&state, &headers)?;

    // 聚合走与控制台下拉同一个函数（见 collect_available_models 的说明）：
    // 客户端能调到的模型，控制台就能选到，反之亦然。
    let now = chrono::Utc::now().timestamp();
    let data: Vec<Value> = collect_available_models(&state)
        .await
        .into_iter()
        .map(|(name, channel_owned_by)| {
            let meta = state.model_metadata.get(&name, channel_owned_by);
            serde_json::json!({
                "id": name,
                "object": "model",
                "created": now,
                "owned_by": meta.owned_by,
                "context_length": meta.context_length,
                "capabilities": meta.capabilities,
            })
        })
        .collect();

    Ok(Json(serde_json::json!({
        "object": "list",
        "data": data
    })))
}

/// GET /v1/models/{model} - 模型详情
pub(crate) async fn handle_get_model(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(model): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    // 模型详情有具体 model：走完整校验（含模型白名单），与对话端点口径一致
    verify_api_key_full(&state, &headers, &model)?;

    // 通用语义：渠道声明或映射可解析即认为存在
    // （models 非空 = 白名单；空 = 全部时以发现快照为准）
    let in_channels = state
        .channel_store
        .list()
        .iter()
        .any(|ch| ch.is_enabled() && ch.effective_models().iter().any(|m| m == &model));
    // 关掉的别名不算「存在」：与 resolve/list 口径一致，否则详情说存在、
    // 调用时却按未配别名透传。
    let mapped = state.model_mapper.is_active(&model);
    if in_channels || mapped {
        let now = chrono::Utc::now().timestamp();
        let meta = state.model_metadata.get(&model, None);
        Ok(Json(serde_json::json!({
            "id": model,
            "object": "model",
            "created": now,
            "owned_by": meta.owned_by,
            "context_length": meta.context_length,
            "capabilities": meta.capabilities,
            "permission": []
        })))
    } else {
        Err(openai_error_response(
            "model_not_found",
            &format!("Model '{model}' not found"),
            StatusCode::NOT_FOUND,
        ))
    }
}

#[cfg(test)]
mod shared_catalog_tests {
    //! 钉住「客户 API 与控制台下拉同源」这个不变量。
    //!
    //! 两边历史上是**两份独立实现**，`/api/models/available` 少聚合了全局别名
    //! 与自定义模型登记，导致「客户端调得通、控制台下拉里选不到」。现在两边都
    //! 调 [`collect_available_models`]；本测试断言的是**结构不变量**——两个
    //! handler 的源文件里都必须出现对该函数的调用，且不再各自内联聚合逻辑。
    //!
    //! 纯文本断言（不构造 AppState）：构造完整 AppState 需要 DB/渠道/密钥等
    //! 一大票依赖，成本远高于它要防的那个回归。

    const MODELS_SRC: &str = include_str!("models.rs");
    const CHANNELS_SRC: &str = include_str!("../admin/channels.rs");

    /// `/v1/models` 走共享聚合函数（而不是自己内联一份）。
    #[test]
    fn list_models_uses_shared_collector() {
        assert!(
            MODELS_SRC.contains("collect_available_models(&state)"),
            "/v1/models 必须调用 collect_available_models，不得内联聚合逻辑"
        );
    }

    /// 控制台下拉走同一个共享聚合函数。
    #[test]
    fn available_models_uses_shared_collector() {
        assert!(
            CHANNELS_SRC.contains("collect_available_models(&state)"),
            "/api/models/available 必须调用 collect_available_models，不得内联聚合逻辑"
        );
    }

    /// 反向断言：`/api/models/available` 里不该再出现「自己遍历渠道聚合」的特征代码。
    /// 出现即说明有人把第二份实现又抄回来了。
    #[test]
    fn available_models_has_no_second_aggregation() {
        for marker in [
            "effective_models()",
            "for_channel(&c.id)",
            "try_mark_lazy_discover",
        ] {
            assert!(
                !CHANNELS_SRC.contains(marker),
                "channels.rs 出现了内联聚合特征 `{marker}`：模型聚合必须只有一份实现"
            );
        }
    }

    /// 共享聚合函数必须同时覆盖别名与自定义模型（这两项正是当年漏掉的）。
    #[test]
    fn collector_covers_aliases_and_custom_models() {
        assert!(MODELS_SRC.contains("active_mappings()"));
        assert!(MODELS_SRC.contains("custom_models()"));
    }
}
