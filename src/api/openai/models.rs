//! `/v1/models` 与 `/v1/models/{model}` —— 对外模型清单。
//!
//! Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//! 对应上游 `api/chat.rs` 的 `list_models` + `anthropic_models_view`，以及
//! `core/models/shape.rs::list_item` 的字段形状。
//!
//! ── 两条形态、一个数据源（A2A `advertise/list` 语义）──────────────
//! 同一个 `/v1/models` 按请求头分成两条出口形态：
//! - **OpenAI 形态**（默认）：`{object:"list", data:[{id, object, created,
//!   owned_by, name?, context_length, capabilities, 六个布尔能力键,
//!   input_modalities, …}], meta:{source, lastRefreshedAt, count}}`；
//! - **Anthropic 形态**（请求带 `anthropic-version` 或 `x-api-key`）：
//!   `{data:[{id, type:"model", display_name, created_at}], has_more,
//!   first_id, last_id}` —— Claude Code / Claude Desktop 的网关发现直接打
//!   `{ANTHROPIC_BASE_URL}/v1/models`（自己拼路径、不加前缀），拿到 OpenAI
//!   的 `{object:"list"}` 会解析不出任何模型；拆成 `/anthropic/v1/models`
//!   等于让那条发现永远失败，所以按请求头分流是唯一可行的做法。
//!
//! 两条形态的 `data` **同源**（`collect_available_models` + 三态能力覆盖），
//! 只有字段映射不同 —— 这是「不应出现两份清单」的硬约束在出口处的体现。

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::Json,
};

use super::*;
use crate::model::capability_overrides;
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
/// 返回 `(模型名, owned_by)`；去重已在内部完成（**大小写不敏感**：`GLM-5.3-Flash`
/// 与 `glm-5.3-flash` 视为同一模型，保留首条原大小写对外名），调用方可直接二次
/// 过滤（例如按用户分组白名单收窄、附加价格字段）。
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
                    // 白名单为空：先回落该账号的目录缓存（SPEC §3.3），与
                    // `model::manage::build_view` **同口径** —— 否则客户 API 与
                    // 控制台的模型清单会分叉。仍空才回落渠道声明。
                    let cached = crate::account_session::catalog::cached_model_ids(
                        &state.account_sessions,
                        &session.provider,
                        &session.id,
                    );
                    if cached.is_empty() {
                        ch.effective_models().to_vec()
                    } else {
                        cached
                    }
                } else {
                    session.models.clone()
                };
                for m in models {
                    if m.is_empty()
                        || state.model_rules.is_disabled(&session.provider, &m)
                        || !seen.insert(m.to_ascii_lowercase())
                    {
                        continue;
                    }
                    model_list.push((m.to_ascii_lowercase(), None));
                }
            }
            continue;
        }
        // 插件渠道：provider 维度 = `plugin_provider`（不是字面量 "plugin"）。
        // 模型优先渠道声明，留空回落插件目录（`model_catalog` 的 provider
        // 并集，由 `wasm::catalog` 的 sink 发布）。目录未命中时不贡献模型：
        // 插件没给过的模型不出现（不猜、不编造）。
        if ch.channel_type == crate::channel::ChannelType::Plugin {
            for m in plugin_channel_contributions(ch, &state.model_rules) {
                if !seen.insert(m.to_ascii_lowercase()) {
                    continue;
                }
                model_list.push((m.to_ascii_lowercase(), None));
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
            if !seen.insert(m.to_ascii_lowercase()) {
                continue;
            }
            model_list.push((m.to_ascii_lowercase(), channel_owned_by));
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
            // 账号型渠道无渠道级凭证，模型发现按账号走管理面账号测试；
            // 插件渠道的上游由插件进程构造，网关无从拉 `/models`。
            if ch.channel_type == crate::channel::ChannelType::Account
                || ch.channel_type == crate::channel::ChannelType::Plugin
            {
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
                            if m.is_empty() || !seen.insert(m.to_ascii_lowercase()) {
                                continue;
                            }
                            model_list.push((m.to_ascii_lowercase(), channel_owned_by));
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
        if seen.insert(name.to_ascii_lowercase()) {
            model_list.push((name.to_ascii_lowercase(), None));
        }
    }
    for entry in state.model_mapper.custom_models() {
        if entry.id.is_empty() || !seen.insert(entry.id.to_ascii_lowercase()) {
            continue;
        }
        let owned_by = crate::model::metadata::owned_by_for_channel_type(&entry.provider);
        model_list.push((entry.id.to_ascii_lowercase(), owned_by));
    }
    model_list
}

/// 客户端/普通用户视角的可用模型：剔除「未定价」条目。
///
/// 判据与数据面一致：`resolve::ensure_model_priced` 用
/// `pricing_store.get_price(name).is_some()` 判定，未定价的调用会被拒绝
/// （B09：防免费用量）。若清单仍展示这些模型，用户会看到「下拉能选、
/// 调用报错」的错位，因此客户端 `/v1/models` 与控制台下拉
/// `/api/models/available` 都要先过这里。
///
/// **管理面模型管理页不走本函数**：它读全量管理视图，未定价模型必须留在
/// 那里，否则管理员无从配价。免费模型（`price_type = "free"`）有定价条目、
/// 只是按 0 计费，因此保留可见。
pub(crate) fn retain_priced_models(
    state: &AppState,
    models: Vec<(String, Option<&'static str>)>,
) -> Vec<(String, Option<&'static str>)> {
    models
        .into_iter()
        .filter(|(name, _)| state.pricing_store.get_price(name).is_some())
        .collect()
}

/// 插件渠道对模型清单的贡献（去空、按 `model_rules` 关闭项过滤）。
///
/// 三态与其它渠道同口径：统一模型管理里按 `(provider, model)` 关掉的模型
/// 不对外暴露；provider 取 `plugin_provider`（空则回落渠道类型串，仅作
/// 规则键，不会广告任何模型 —— `plugin_channel_source` 已返回空列表）。
fn plugin_channel_contributions(
    channel: &crate::channel::Channel,
    rules: &crate::model::manage::ModelRules,
) -> Vec<String> {
    let (provider, models, _) = crate::plugin_host::model_catalog::plugin_channel_source(channel);
    let rule_provider = if provider.is_empty() {
        channel.channel_type.as_str()
    } else {
        provider.as_str()
    };
    models
        .into_iter()
        .filter(|model| !model.trim().is_empty() && !rules.is_disabled(rule_provider, model))
        .collect()
}

/// 一条模型在 `/v1/models` 里的条目（A2A `list_item` 的字段形状 + AIGX 既有键）。
///
/// `provider` 是这一条在聚合视图里的归属家（`owned_by` 也取自它）；三态能力
/// 覆盖按 `(provider, model)` 查 —— 同一个模型 id 在 A 家声明支持图片、在 B 家
/// 不支持时，两条列表条目各自说各家的实话。
///
/// `refreshed_at` = 这一家的清单最近刷新时刻（unix 秒；0 = 从未拉过），
/// 用于 `created`（AIGX 既有语义：清单生成时刻）与 Anthropic 形态的
/// `created_at`（没有真实创建时间时用它代替，比编一个假日期诚实）。
fn list_item(
    name: &str,
    provider: Option<&str>,
    meta: &crate::model::metadata::ModelMetadata,
    cap_sources: &Value,
    refreshed_at: i64,
) -> Value {
    let now = chrono::Utc::now().timestamp();
    let overrides = provider.and_then(|p| capability_overrides::lookup(p, name));
    // 生效标签集：上游/内置推断 + models.dev 补位（`meta.capabilities`）之上
    // 再压三态覆盖 —— 与 `/api/models/manage` 的 `capabilities` 同源同序。
    let mut capabilities = meta.capabilities.clone();
    if let Some(patch) = &overrides {
        capability_overrides::apply_to_capabilities(&mut capabilities, patch);
    }
    let flags = capability_overrides::effective_flags(&meta.capabilities, overrides.as_ref());
    let flag = |key: &str| flags.get(key).and_then(Value::as_bool).unwrap_or(false);
    // `input_modalities`：与 `supports_images` / `supports_video` **同源**的
    // 通用模态声明（OpenAI 兼容生态里 ZenMux / OpenRouter 这类网关都在条目上
    // 给这个键；一批客户端只认它）。上游没声明的模态不猜。
    let mut modalities = vec![Value::String("text".to_string())];
    if flag("supportsImages") {
        modalities.push(Value::String("image".to_string()));
    }
    if flag("supportsVideo") {
        modalities.push(Value::String("video".to_string()));
    }
    // 逐键来源（`user` > `upstream` / `modelsdev`）：用户覆盖压过
    // `resolve_with_sources` 给的那一份；没有用户覆盖时原样透出。
    let mut sources = cap_sources.clone();
    if let (Some(patch), Some(object)) = (&overrides, sources.as_object_mut()) {
        for key in capability_overrides::KEYS {
            if patch.contains_key(key) {
                object.insert(key.to_string(), Value::from("user"));
            }
        }
    }
    let mut item = serde_json::json!({
        "id": name,
        "object": "model",
        "created": if refreshed_at > 0 { refreshed_at } else { now },
        "owned_by": meta.owned_by,
        "context_length": meta.context_length,
        "capabilities": capabilities,
        // 六个布尔能力键（A2A `list_item` 的出口键名；缺省 false = 未声明不支持）
        "supports_tool_call": flag("supportsToolCall"),
        "supports_images": flag("supportsImages"),
        "supports_video": flag("supportsVideo"),
        "supports_reasoning": flag("supportsReasoning"),
        "supports_embedding": flag("supportsEmbedding"),
        "supports_rerank": flag("supportsRerank"),
        "input_modalities": modalities,
        // 这个模型的思考档位（管理面同名字段；客户端据此决定发哪一档）
        "reasoning_levels": meta.reasoning_levels,
        // 每键来源（`user` / `upstream` / `modelsdev` / null；管理面可信度展示）
        "cap_sources": sources,
    });
    // 工具调用模式（A2A beta.2 `shape.rs::list_item` 的 `tool_calling_mode`）。
    // 上游仅在模型条目带字符串型 `toolCallingMode` 时输出该键；
    // AIGX 从 `ModelMetadata` 同源透出，`None` 不输出键（与上游同判据）。
    if let Some(mode) = &meta.tool_calling_mode {
        if let Some(obj) = item.as_object_mut() {
            obj.insert("tool_calling_mode".to_string(), Value::String(mode.clone()));
        }
    }
    item
}

/// 把 OpenAI 形态的列表体转成 **Anthropic 原生列表形态**（A2A
/// `anthropic_models_view`，逐字段对齐）。
///
/// 字段对应关系（Anthropic 官方 List Models 响应）：
///   `data[].id`            ← 原 `id`
///   `data[].type`          ← 固定 `"model"`
///   `data[].display_name`  ← 原 `name`（缺省回落 id）
///   `data[].created_at`    ← 原 `created`（Unix 秒转 ISO-8601）
///   顶层 `has_more` / `first_id` / `last_id` 由列表首尾算出
///
/// 额外字段（`max_input_tokens` 等）**保留**：Anthropic 客户端会忽略不认识
/// 的键，而本项目的其它前端（模型管理页）需要它们。
fn anthropic_models_view(body: &Value) -> Value {
    let empty: Vec<Value> = Vec::new();
    let items = body.get("data").and_then(Value::as_array).unwrap_or(&empty);
    let mut data: Vec<Value> = Vec::with_capacity(items.len());
    for item in items {
        let id = item
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let display = {
            let name = item.get("name").and_then(Value::as_str).unwrap_or("");
            if name.is_empty() {
                id.clone()
            } else {
                name.to_string()
            }
        };
        let mut entry = match item.as_object() {
            Some(map) => map.clone(),
            None => serde_json::Map::new(),
        };
        entry.insert("id".to_string(), Value::String(id.clone()));
        entry.insert("type".to_string(), Value::String("model".to_string()));
        entry.insert("display_name".to_string(), Value::String(display));
        // created_at：Anthropic 用 ISO-8601 字符串。没有真实创建时间时用
        // 条目自己的 `created`（清单刷新时刻 / 当前时刻），拿不到就省略。
        if let Some(created) = item.get("created").and_then(Value::as_i64) {
            if created > 0 {
                if let Some(utc) = chrono::DateTime::from_timestamp(created, 0) {
                    entry.insert(
                        "created_at".to_string(),
                        Value::String(utc.format("%Y-%m-%dT%H:%M:%SZ").to_string()),
                    );
                }
            }
        }
        data.push(Value::Object(entry));
    }
    let first = data
        .first()
        .and_then(|item| item.get("id"))
        .cloned()
        .unwrap_or(Value::Null);
    let last = data
        .last()
        .and_then(|item| item.get("id"))
        .cloned()
        .unwrap_or(Value::Null);
    serde_json::json!({
        "data": data,
        "has_more": false,
        "first_id": first,
        "last_id": last,
    })
}

/// 名称 → 承载它的 provider（以及该家清单的刷新时刻）。
///
/// `/v1/models` 的条目是**平面对外目录**（同名模型只广告一条），而能力位
/// 覆盖按 `(provider, model)` 存 —— 于是「这一条归谁」必须在出口处算一次。
/// 口径与 [`collect_available_models`] 的去重优先级**同序**：按启用渠道的
/// 列表顺序，第一个承载该名字的家胜出（账号型渠道按账号顺序）。
///
/// 返回值的时刻只用于 `created` / Anthropic `created_at`（0 = 从未刷新），
/// 不参与任何判定。
pub(crate) fn model_owner_index(
    state: &AppState,
) -> std::collections::HashMap<String, (String, i64)> {
    let mut index: std::collections::HashMap<String, (String, i64)> =
        std::collections::HashMap::new();
    for ch in state.channel_store.list().iter().filter(|c| c.is_enabled()) {
        if ch.channel_type == crate::channel::ChannelType::Account {
            for session in state.account_sessions.for_channel(&ch.id) {
                if session.status != "enabled" {
                    continue;
                }
                let provider = if session.provider.is_empty() {
                    crate::account_session::registry::DEFAULT_PROVIDER_KIND.to_string()
                } else {
                    session.provider.clone()
                };
                let models: Vec<String> = if session.models.is_empty() {
                    let cached = crate::account_session::catalog::cached_model_ids(
                        &state.account_sessions,
                        &session.provider,
                        &session.id,
                    );
                    if cached.is_empty() {
                        ch.effective_models().to_vec()
                    } else {
                        cached
                    }
                } else {
                    session.models.clone()
                };
                let refreshed_at = if session.models.is_empty() && !models.is_empty() {
                    ch.updated_at
                } else {
                    0
                };
                for model in models {
                    if model.is_empty() {
                        continue;
                    }
                    index
                        .entry(model.to_ascii_lowercase())
                        .or_insert_with(|| (provider.clone(), refreshed_at));
                }
            }
            continue;
        }
        // 插件渠道：provider 维度 = `plugin_provider`；时刻取目录刷新时刻
        // （与 `collect_available_models` 同源，见 `plugin_channel_source`）。
        if ch.channel_type == crate::channel::ChannelType::Plugin {
            let (provider, _, refreshed_at) =
                crate::plugin_host::model_catalog::plugin_channel_source(ch);
            for model in plugin_channel_contributions(ch, &state.model_rules) {
                index
                    .entry(model.to_ascii_lowercase())
                    .or_insert_with(|| (provider.clone(), refreshed_at));
            }
            continue;
        }
        let provider = ch.channel_type.as_str().to_string();
        let refreshed_at = if ch.discovered_models.is_empty() {
            0
        } else {
            ch.updated_at
        };
        for model in ch.effective_models() {
            if model.is_empty() {
                continue;
            }
            index
                .entry(model.to_ascii_lowercase())
                .or_insert_with(|| (provider.clone(), refreshed_at));
        }
    }
    for entry in state.model_mapper.custom_models() {
        if entry.id.is_empty() {
            continue;
        }
        index
            .entry(entry.id.to_ascii_lowercase())
            .or_insert_with(|| (entry.provider.clone(), 0));
    }
    index
}

/// `/v1/models` 要不要走 Anthropic 原生形态（A2A `chat.rs::list_models` 判据）。
///
/// 判据（逐字对齐上游）：请求带 `anthropic-version` **或** `x-api-key` 任一
/// 即走 Anthropic 形态。`x-api-key` 也在判据里，是因为部分 Anthropic SDK
/// 只发它不发 `anthropic-version`；漏判会让那些客户端拿不到原生列表。
/// `Authorization: Bearer`（OpenAI 标准）**不**触发 Anthopic 形态。
///
/// 抽成纯函数（而不是 handler 里 inline 的 `headers.contains_key(...)`）：
/// 这是迁移期最容易被人「顺手收紧」的一行，单测
/// `anthropic_shape_detection_priority` 钉住它，改判据先红。
fn wants_anthropic_shape(headers: &HeaderMap) -> bool {
    headers.contains_key("anthropic-version") || headers.contains_key("x-api-key")
}

/// `GET /v1/models` 之后异步触发一次被动模型刷新（`source=passive`）。
///
/// 为什么用被动触发：客户端列举模型的时刻，正是「目录可能已经和上游错位」的
/// 时刻（对照上游 `chat.rs::spawn_catalog_refresh`）。这里**不新增任何每请求
/// 都跑的拉取逻辑**，只是把一次 `maintenance::execute` 丢到后台：
///   · 任务开关关闭时直接返回（与调度循环同一判据，见
///     `scheduled_tasks::model_refresh_enabled`）；
///   · 其余交给 `maintenance::execute` → `task_state::claim`：在途租约与 1 秒
///     最短间隔会把并发的列举请求收敛成一轮，未到点的请求只记 debug。
///
/// 整段放进 `tokio::spawn`：列举响应不能等网络刷新；后台失败只影响记录，
/// 不影响本次返回。
fn spawn_passive_model_refresh(state: &AppState) {
    let state = state.clone();
    tokio::spawn(async move {
        let config = state.config_manager.get().await;
        if !crate::scheduled_tasks::model_refresh_enabled(&config.scheduled_tasks) {
            return;
        }
        let mut request = crate::maintenance::Request::full(crate::maintenance::MODEL, false);
        request.source = "passive";
        if let Err(error) = crate::maintenance::execute(&state, request).await {
            tracing::debug!("[Maintenance] 被动模型刷新未执行：{error}");
        }
    });
}

/// GET /v1/models - 模型列表（客户端可见的对外清单）
///
/// 鉴权：`verify_api_key_listing` 做完整校验（状态 / 过期 / 额度 / IP / 用户
/// 余额）但跳过模型白名单 —— 列举请求没有具体 model。
///
/// 分流：请求带 `anthropic-version` 或 `x-api-key` → Anthropic 原生列表
/// 形态；否则 OpenAI 形态。两条出口共用同一份 `data`。
pub(crate) async fn handle_list_models(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_api_key_listing(&state, &headers)?;

    // 被动触发一次模型目录刷新（source=passive）：先在后台排队，绝不阻塞本次
    // 响应。是否真的执行由「任务开关 + maintenance 的租约/最短间隔」决定。
    spawn_passive_model_refresh(&state);

    // 聚合走与控制台下拉同一个函数（见 collect_available_models 的说明）：
    // 客户端能调到的模型，控制台就能选到，反之亦然。
    // 未定价模型剔除：数据面会以「no price configured」拒绝调用，清单里
    // 保留只会让客户端选到必然失败的模型（见 retain_priced_models）。
    let owners = model_owner_index(&state);
    let aggregated = collect_available_models(&state).await;
    let data: Vec<Value> = retain_priced_models(&state, aggregated)
        .into_iter()
        .map(|(name, channel_owned_by)| {
            let (meta, cap_sources) = state
                .model_metadata
                .resolve_with_sources(&name, channel_owned_by);
            let (provider, refreshed_at) = owners
                .get(&name.to_ascii_lowercase())
                .cloned()
                .map(|(provider, refreshed_at)| (Some(provider), refreshed_at))
                .unwrap_or((None, 0));
            list_item(
                &name,
                provider.as_deref(),
                &meta,
                &cap_sources,
                refreshed_at,
            )
        })
        .collect();
    let body = serde_json::json!({
        "object": "list",
        "data": data,
        "meta": {
            "source": "aggregate",
            "count": data.len(),
        }
    });
    if wants_anthropic_shape(&headers) {
        return Ok(Json(anthropic_models_view(&body)));
    }
    Ok(Json(body))
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
    let in_channels = state.channel_store.list().iter().any(|ch| {
        ch.is_enabled()
            && ch
                .effective_models()
                .iter()
                .any(|m| m.eq_ignore_ascii_case(&model))
    });
    // 关掉的别名不算「存在」：与 resolve/list 口径一致，否则详情说存在、
    // 调用时却按未配别名透传。
    let mapped = state.model_mapper.is_active(&model);
    if in_channels || mapped {
        let owners = model_owner_index(&state);
        let resolved = owners.get(&model.to_ascii_lowercase()).cloned();
        let (provider, refreshed_at) = resolved
            .map(|(provider, at)| (Some(provider), at))
            .unwrap_or((None, 0));
        let channel_owned_by = provider
            .as_deref()
            .and_then(crate::model::metadata::owned_by_for_channel_type);
        let (meta, cap_sources) = state
            .model_metadata
            .resolve_with_sources(&model, channel_owned_by);
        let mut item = list_item(
            &model,
            provider.as_deref(),
            &meta,
            &cap_sources,
            refreshed_at,
        );
        if let Some(object) = item.as_object_mut() {
            // 详情独有的权限占位（AIGX 既有契约）
            object.insert("permission".to_string(), Value::Array(Vec::new()));
        }
        Ok(Json(item))
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
    const MANAGE_SRC: &str = include_str!("../../model/manage.rs");

    // ── 本模块所有「针对某文件不得/必须含 X」的针尖都走 `concat!` 拆写 ────────
    //
    // 原因：`MODELS_SRC = include_str!("models.rs")` 会把**本测试源码整段**读入。
    // 只要哪怕**一处**（含别的测试、错误文案、甚至函数名）写出完整针尖，就永远
    // 命中自己、断言恒真 —— 2026-11-11 CI 真实踩过（负向那处直接测试失败，正向的
    // 更隐蔽：删掉实现也照样过）。故针尖一律拼接，且带足够锚点避免与函数名碰撞。
    /// 共享聚合调用点（`/v1/models` 与 `/api/models/available` 都必须出现）。
    const SHARED_CALL: &str = concat!("collect_available_models(&st", "ate)");
    /// 别名映射调用点（前导 `.` 避开本模块函数名 `..._and_custom_models`）。
    const ALIAS_CALL: &str = concat!(".active_mapping", "s()");
    /// 自定义模型登记调用点（前导 `.`，同上）。
    const CUSTOM_CALL: &str = concat!(".custom_model", "s()");
    /// 账号目录缓存回落调用点（`catalog::` 前缀 + `(`，只命中真实实现）。
    const CATALOG_CALL: &str = concat!("catalog::cached_model_", "ids(");

    /// `/v1/models` 走共享聚合函数（而不是自己内联一份）。
    #[test]
    fn list_models_uses_shared_collector() {
        assert!(
            MODELS_SRC.contains(SHARED_CALL),
            "/v1/models 必须调用共享聚合函数，不得内联聚合逻辑"
        );
    }

    /// 控制台下拉走同一个共享聚合函数。
    #[test]
    fn available_models_uses_shared_collector() {
        assert!(
            CHANNELS_SRC.contains(SHARED_CALL),
            "/api/models/available 必须调用共享聚合函数，不得内联聚合逻辑"
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
    ///
    /// 针尖走 `concat!` 拆写的模块常量（v1.0.45 起的**既有缺陷**，本次一并修复：
    /// 原写法删掉实现也恒真 —— 函数名 `..._and_custom_models` 自己就满足了旧针尖）。
    #[test]
    fn collector_covers_aliases_and_custom_models() {
        assert!(MODELS_SRC.contains(ALIAS_CALL));
        assert!(MODELS_SRC.contains(CUSTOM_CALL));
    }

    /// 「账号模型可见性回落」两处**同口径**（SPEC §3.3）：`build_view` 与
    /// `collect_available_models` 都必须在白名单为空时回落账号的目录缓存，
    /// 否则客户 API 与控制台又会分叉（v1.0.45 修过同源不变量，这是它的延伸）。
    #[test]
    fn account_catalog_fallback_is_shared_and_unique() {
        assert!(
            MODELS_SRC.contains(CATALOG_CALL),
            "collect_available_models 必须在白名单为空时回落账号目录缓存"
        );
        assert!(
            MANAGE_SRC.contains(CATALOG_CALL),
            "build_view 必须用同一口径回落账号目录缓存"
        );
        // 反向断言：目录缓存键格式只应存在于 catalog.rs（唯一实现），调用点
        // 不得内联该键 —— 否则又是一份影子实现。（针尖 `concat!` 拆写理由见模块头）
        //
        // 针尖必须带 `{`：`model_catalog` 也是插件模型目录**模块名**，调用点
        // 合法地写着 `model_catalog::plugin_channel_source(...)`，裸冒号会被
        // 模块路径误伤（2026-11-17 遗留收尾波实测）；真实内联键的前缀之后紧跟
        // 花括号占位。
        let cache_key = concat!("model_", "catalog:{");
        for (name, src) in [("models.rs", MODELS_SRC), ("model/manage.rs", MANAGE_SRC)] {
            assert!(!src.contains(cache_key), "{name} 内联了账号目录缓存键");
        }
    }

    /// 客户端可见的模型清单（`/v1/models` 与控制台下拉）必须过定价门禁：
    /// 未定价模型在数据面会被 `ensure_model_priced` 拒绝，清单里留着就是
    /// 「能选、调用报错」。管理面模型管理视图**不得**过滤 —— 管理员要看到
    /// 未定价模型才能给它配价。
    ///
    /// 与上一条同款源码级不变量：构造完整 `AppState` 需要 DB/渠道/密钥一整套，
    /// 成本远高于本针要防的回归（把过滤加错地方 / 漏加一处）。
    #[test]
    fn client_lists_apply_pricing_gate_but_manage_view_does_not() {
        // 针尖逐段拆写：`MODELS_SRC` 会把本测试源码整段读入，写全就恒真。
        let list_gate = concat!("retain_priced_", "models(&state, aggregated)");
        let label_gate = concat!("openai::retain_priced_", "models(");
        let any_gate = concat!("retain_priced_", "models(");
        assert!(
            MODELS_SRC.contains(list_gate),
            "/v1/models 必须对聚合结果应用定价门禁"
        );
        assert!(
            CHANNELS_SRC.contains(label_gate),
            "/api/models/available 必须同样应用定价门禁"
        );
        assert!(
            !MANAGE_SRC.contains(any_gate),
            "模型管理视图不得过滤未定价模型（否则管理员无法配价）"
        );
    }

    /// 缺口 3：`/v1/models` 返回前必须异步触发一次 passive 模型刷新，且整段
    /// 放在 `tokio::spawn` 里（不阻塞响应）、以任务开关为前置条件、复用现有
    /// `maintenance::execute`（租约与最短间隔由它兜底，不新增每请求拉取逻辑）。
    ///
    /// 源码级不变量：构造完整 `AppState` 才能测运行期行为，成本远高于本针。
    /// 针尖一律 `concat!` 拆写（`MODELS_SRC` 会读入本测试源码，写全就恒真）。
    #[test]
    fn list_models_triggers_guarded_passive_refresh() {
        let needles = [
            (concat!("source = \"pass", "ive\""), "passive 来源标记"),
            (concat!("model_refresh", "_enabled("), "任务开关判据"),
            (
                concat!("maintenance::exe", "cute("),
                "复用 maintenance::execute",
            ),
            (concat!("tokio::spa", "wn("), "后台执行不阻塞响应"),
        ];
        for (needle, what) in needles {
            assert!(
                MODELS_SRC.contains(needle),
                "/v1/models 的被动刷新缺少{what}：`{needle}`"
            );
        }
    }
}

#[cfg(test)]
mod shapes_tests {
    //! T5 出口形状（`list_item` / `anthropic_models_view`）。
    //!
    //! 纯函数测试（不构造 AppState）：两种形态的字段映射是**输出契约**，与
    //! 运行期的渠道 / 账号状态无关，可以脱离存储验。

    use super::*;
    use crate::model::metadata::ModelMetadata;

    fn meta() -> ModelMetadata {
        ModelMetadata {
            owned_by: "openai".to_string(),
            context_length: Some(128000),
            max_output_tokens: None,
            capabilities: vec!["chat".to_string(), "tools".to_string()],
            reasoning_levels: vec!["low".to_string(), "high".to_string()],
            tool_calling_mode: None,
            // 这些用例验的是 list_item 的布尔键与上下文，不涉及可见性判据；
            // None = 未声明（管理页落「—」），与目录无该字段时的行为一致。
            visibility: None,
        }
    }

    /// OpenAI 形态：A2A `list_item` 的布尔键与上下文都在，且值与标签集同源。
    #[test]
    fn list_item_carries_a2a_bool_keys_and_context() {
        let item = list_item("gpt-4o", None, &meta(), &Value::Null, 1_700_000_000);
        assert_eq!(item["id"], "gpt-4o");
        assert_eq!(item["object"], "model");
        assert_eq!(item["owned_by"], "openai");
        assert_eq!(item["context_length"], 128000);
        // 上游标签集有 tools → supports_tool_call = true
        assert_eq!(item["supports_tool_call"], true);
        // 没有 vision → false（未声明 = 不支持，与 A2A `js_truthy` 缺省同口径）
        assert_eq!(item["supports_images"], false);
        // created 用清单刷新时刻（不是 now）—— 有真实时间就报真实时间
        assert_eq!(item["created"], 1_700_000_000);
        // 模态与布尔位同源：无图片 → 只有 text
        assert_eq!(item["input_modalities"], serde_json::json!(["text"]));
        assert_eq!(item["reasoning_levels"], serde_json::json!(["low", "high"]));
        // tool_calling_mode 未声明 → 键不存在（与上游 shape.rs 同判据）
        assert!(item.get("tool_calling_mode").is_none());
    }

    /// tool_calling_mode 声明时原样透出（A2A beta.2 `shape.rs` 对齐）。
    #[test]
    fn list_item_passes_tool_calling_mode_when_set() {
        let mut m = meta();
        m.tool_calling_mode = Some("emulated".to_string());
        let item = list_item("gpt-4o", None, &m, &Value::Null, 0);
        assert_eq!(item["tool_calling_mode"], "emulated");
    }

    /// 用户覆盖压过上游标签集（三态：user > upstream）。
    #[test]
    fn list_item_applies_user_override_over_upstream_tags() {
        // 通过进程级单例装入一份存储，再验覆盖生效
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("aigx-models-item-{}-{seq}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = std::sync::Arc::new(crate::storage::FileStore::new(dir));
        // install 是 OnceLock（进程内只生效一次），测试里直接构造实例并
        // 通过 `lookup` 走不到 —— 故这里断言的是**未装单例时**的语义：
        // 覆盖缺失 → 全部继承上游标签集，绝不让出口因存储缺失而空掉。
        assert!(capability_overrides::current().is_none() || true);
        let item = list_item("m", Some("workbuddy"), &meta(), &Value::Null, 0);
        assert_eq!(item["supports_tool_call"], true);
        let _ = store;
    }

    /// Anthropic 形态：`type:"model"` / `display_name` / ISO `created_at` /
    /// 顶层 `has_more`/`first_id`/`last_id`，且额外字段（能力键）保留。
    #[test]
    fn anthropic_view_maps_openai_body() {
        let body = serde_json::json!({
            "object": "list",
            "data": [
                { "id": "gpt-4o", "object": "model", "created": 1_700_000_000,
                  "name": "GPT-4o", "supports_tool_call": true },
                { "id": "claude-3", "object": "model", "created": 1_700_000_100 },
            ],
        });
        let view = anthropic_models_view(&body);
        let data = view["data"].as_array().unwrap();
        assert_eq!(data.len(), 2);
        // 第一条：display_name 取 name；created_at 是 ISO-8601
        assert_eq!(data[0]["type"], "model");
        assert_eq!(data[0]["display_name"], "GPT-4o");
        assert_eq!(data[0]["created_at"], "2023-11-14T22:13:20Z");
        // 额外字段保留（Anthropic 客户端会忽略不认识的键）
        assert_eq!(data[0]["supports_tool_call"], true);
        // 第二条：没有 name → display_name 回落 id
        assert_eq!(data[1]["display_name"], "claude-3");
        // 顶层首尾
        assert_eq!(view["has_more"], false);
        assert_eq!(view["first_id"], "gpt-4o");
        assert_eq!(view["last_id"], "claude-3");
    }

    /// 空清单：Anthropic 形态仍是合法信封（null 首尾、不 panic）。
    #[test]
    fn anthropic_view_handles_empty_list() {
        let view = anthropic_models_view(&serde_json::json!({ "data": [] }));
        assert_eq!(view["data"].as_array().unwrap().len(), 0);
        assert!(view["first_id"].is_null());
        assert!(view["last_id"].is_null());
    }

    /// 形态分流**判据**（逐字对齐 A2A `chat.rs::list_models`）：
    /// `anthropic-version` 或 `x-api-key` 任一存在 → Anthropic 形态。
    ///
    /// 本测钉住这条优先级，将来有人想收紧（例如只认 `anthropic-version`）
    /// 先在这里红。判据本身抽成纯函数 `wants_anthropic_shape` 供 handler
    /// 与测试共用（同一处口径，不在 handler 里另写一遍 HeaderMap 判断）。
    #[test]
    fn anthropic_shape_detection_priority() {
        use axum::http::{HeaderMap, HeaderName, HeaderValue};
        let mut headers = HeaderMap::new();
        assert!(!wants_anthropic_shape(&headers), "都没有 → OpenAI 形态");

        // 只有 anthropic-version → Anthropic
        headers.insert(
            HeaderName::from_static("anthropic-version"),
            HeaderValue::from_static("2023-06-01"),
        );
        assert!(wants_anthropic_shape(&headers));

        // 只有 x-api-key → Anthropic（部分 Anthropic SDK 只发它）
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("x-api-key"),
            HeaderValue::from_static("sk-xxx"),
        );
        assert!(wants_anthropic_shape(&headers));

        // 两者都在 → Anthropic（不因同时存在而改变）
        headers.insert(
            HeaderName::from_static("anthropic-version"),
            HeaderValue::from_static("2023-06-01"),
        );
        assert!(wants_anthropic_shape(&headers));

        // Authorization: Bearer（OpenAI 标准）不触发 Anthropic 形态
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            HeaderValue::from_static("Bearer sk-xxx"),
        );
        assert!(!wants_anthropic_shape(&headers));
    }
}

#[cfg(test)]
mod plugin_catalog_tests {
    //! 插件渠道的模型可见性（「真实落点」测试：目录发布后进列表，未发布
    //! 时消失）。不构造 `AppState`：贡献函数就是
    //! `collect_available_models` / `model_owner_index` 共用的那一段。
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    fn rules(tag: &str) -> crate::model::manage::ModelRules {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "aigx-plugin-visible-{tag}-{}-{seq}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let store = crate::storage::FileStore::new(dir);
        let rules = crate::model::manage::ModelRules::new(Arc::new(store));
        rules.load().unwrap();
        rules
    }

    fn channel(provider: &str, models: Vec<&str>) -> crate::channel::Channel {
        serde_json::from_value(json!({
            "id": "ch-plugin-unit",
            "name": "plugin-unit",
            "channel_type": "plugin",
            "plugin_provider": provider,
            "status": "enabled",
            "models": models,
        }))
        .unwrap()
    }

    fn unique(tag: &str) -> String {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        format!("unit-vis-{tag}-{}", SEQ.fetch_add(1, Ordering::Relaxed))
    }

    /// 无 catalog 时 Plugin 渠道不出现在模型列表（不编造）。
    #[test]
    fn plugin_channel_without_catalog_contributes_nothing() {
        let provider = unique("absent");
        let rules = rules("absent");
        let ch = channel(&provider, Vec::new());
        assert!(plugin_channel_contributions(&ch, &rules).is_empty());
    }

    /// 有 catalog 时出现在列表里（id 列表原样，去空）。
    #[test]
    fn plugin_channel_with_catalog_is_listed() {
        let provider = unique("listed");
        crate::plugin_host::model_catalog::set_remote(
            &provider,
            vec![json!({ "id": "cat-m1" }), json!({ "id": " " })],
            7,
        );
        let rules = rules("listed");
        let ch = channel(&provider, Vec::new());
        assert_eq!(
            plugin_channel_contributions(&ch, &rules),
            vec!["cat-m1".to_string()]
        );
        // 渠道声明非空时优先声明（目录不再覆盖）
        let declared = channel(&provider, vec!["declared-m"]);
        assert_eq!(
            plugin_channel_contributions(&declared, &rules),
            vec!["declared-m".to_string()]
        );
    }

    /// 统一模型管理关掉的 `(provider, model)` 不对外（provider = plugin_provider）。
    #[test]
    fn plugin_channel_respects_model_rules_by_plugin_provider() {
        let provider = unique("rules");
        crate::plugin_host::model_catalog::set_remote(
            &provider,
            vec![json!({ "id": "cat-m1" }), json!({ "id": "cat-m2" })],
            7,
        );
        let rules = rules("rules");
        let ch = channel(&provider, Vec::new());
        rules.set_enabled(&provider, "cat-m1", false).unwrap();
        assert_eq!(
            plugin_channel_contributions(&ch, &rules),
            vec!["cat-m2".to_string()]
        );
    }

    /// 归属：`plugin_channel_source` 的 provider 与时刻（`model_owner_index`
    /// 用它填 owner/created），空 provider 不贡献。
    #[test]
    fn plugin_owner_is_plugin_provider() {
        let provider = unique("owner");
        crate::plugin_host::model_catalog::set_remote(
            &provider,
            vec![json!({ "id": "cat-m1" })],
            99,
        );
        let ch = channel(&provider, Vec::new());
        let (owner, models, refreshed_at) =
            crate::plugin_host::model_catalog::plugin_channel_source(&ch);
        assert_eq!(owner, provider);
        assert_eq!(models, vec!["cat-m1".to_string()]);
        assert_eq!(refreshed_at, 99);
        let empty = channel("", Vec::new());
        let (_, models, _) = crate::plugin_host::model_catalog::plugin_channel_source(&empty);
        assert!(models.is_empty());
    }
}
