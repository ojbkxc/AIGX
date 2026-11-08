use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    Json,
};

use super::*;
use serde_json::Value;

/// GET /v1/models - 模型列表
pub(crate) async fn handle_list_models(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    // 完整校验（状态/过期/额度/IP/用户余额），但跳过模型白名单：
    // 列举请求没有具体 model。原来用 verify_api_key 只查存在性，
    // 过期/禁用的 Key 也能读到模型清单。
    verify_api_key_listing(&state, &headers)?;

    // 通用网关语义：模型列表 = 所有启用渠道声明/发现的模型 ∪ 别名映射的 key。
    // 渠道 models 是主数据源（管理员在渠道上配置），映射只是可选别名。
    // P1 模型元信息：owned_by 由渠道类型/名字推断 + 管理员覆盖，
    // 附带 context_length 与能力标签（非标准扩展字段，客户端可忽略）。
    let now = chrono::Utc::now().timestamp();
    let mut seen = std::collections::HashSet::new();
    let mut model_list: Vec<Value> = Vec::new();

    let build_entry = |name: &str, channel_owned_by: Option<&str>| {
        let meta = state.model_metadata.get(name, channel_owned_by);
        serde_json::json!({
            "id": name,
            "object": "model",
            "created": now,
            "owned_by": meta.owned_by,
            "context_length": meta.context_length,
            "capabilities": meta.capabilities,
        })
    };

    let channels = state.channel_store.list();
    for ch in &channels {
        if !ch.is_enabled() {
            continue;
        }
        // 与调度口径一致：冷却/断路器打开的故障渠道不参与模型列表聚合，
        // 避免上游故障把模型从 /v1/models 吞掉导致客户端误判模型下线
        if state.channel_store.is_in_cooldown(&ch.id)
            || !state
                .channel_store
                .circuit_breaker()
                .allows_read_only(&ch.id)
        {
            continue;
        }
        // 账号型渠道：模型来自账号（与统一模型管理页 / /api/models/available 同口径）
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
                    model_list.push(build_entry(&m, None));
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
            model_list.push(build_entry(m, channel_owned_by));
        }
    }

    // 与 /api/models/available 口径一致：聚合为空时对"留空=全部"的启用且
    // 非冷却渠道做一次懒加载，调上游 /models 发现并写回 discovered_models。
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
                &state,
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
                            model_list.push(build_entry(m, channel_owned_by));
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
            model_list.push(build_entry(name, None));
        }
    }
    for entry in state.model_mapper.custom_models() {
        if entry.id.is_empty() || !seen.insert(entry.id.clone()) {
            continue;
        }
        let owned_by = crate::model::metadata::owned_by_for_channel_type(&entry.provider);
        model_list.push(build_entry(&entry.id, owned_by));
    }

    Ok(Json(serde_json::json!({
        "object": "list",
        "data": model_list
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
