//! 破甲控制台管理面（`/api/purge`）。
//!
//! 与 `prompt` / `sanitize` 同级：GET 读配置 + 进程统计，PUT 全量写配置并热生效。
//! 统计是进程内原子计数（重启归零），不做持久化——它反映的是当前进程的
//! 真实请求行为，不是历史报表。

use axum::{
    extract::{Multipart, Path as AxumPath, Query, State},
    http::{HeaderMap, StatusCode},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};

use super::super::openai::AppState;
use super::common::{error_response, verify_admin};

use crate::bridge::{ChatMessage, Role};
use crate::purge::presets::{self, ApplyTarget};
use crate::purge::{PurgeSettings, RewriteRule};

/// GET /api/purge —— 读取破甲配置、统计快照与进程运行信息。
pub async fn handle_get_purge(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    Ok(Json(payload(&state)))
}

/// GET /api/purge/stats.csv —— 四档统计导出（运维报表）。
///
/// 单独一个端点而不是塞进 GET /api/purge：导出的消费方是「下载成文件」，
/// 与 JSON 快照的生命周期不同（前者进 Excel/BI，后者是页面渲染数据）。
/// Content-Disposition 带文件名，浏览器直接落盘为 .csv。
/// GET /api/purge/defaults -- 出厂默认配置（只读，不碰存储）。
///
/// 用途：控制台「重置为默认」把默认值填进**草稿**（用户确认后再点保存），
/// 所以这里不返回当前配置、也不写库。默认值经 `normalize` 归一，与「全新
/// 安装首次保存」的结果一致——否则重置出来的配置与出厂态会有微妙差异。
pub async fn handle_get_purge_defaults(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let defaults = normalize(PurgeSettings::default());
    Ok(Json(json!({ "success": true, "settings": defaults })))
}

pub async fn handle_export_purge_stats(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<([(axum::http::HeaderName, String); 2], String), (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let csv = crate::purge::stats_csv(state.purge.last_run().as_ref());
    Ok((
        [
            (
                axum::http::header::CONTENT_TYPE,
                "text/csv; charset=utf-8".to_string(),
            ),
            (
                axum::http::header::CONTENT_DISPOSITION,
                "attachment; filename=\"aigx-purge-stats.csv\"".to_string(),
            ),
        ],
        csv,
    ))
}

/// PUT /api/purge —— 全量保存配置（热生效）。
///
/// 请求体既可以是配置对象本身，也可以是 `{ "settings": {...} }`；
/// 数值字段统一 clamp，空改写规则（from 为空）会被丢弃，避免页面误存无效词。
pub async fn handle_update_purge(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;

    let raw = body
        .get("settings")
        .filter(|v| !v.is_null())
        .cloned()
        .unwrap_or(body);
    let parsed: PurgeSettings = serde_json::from_value(raw).map_err(|e| {
        error_response(
            &format!("Invalid purge settings: {e}"),
            StatusCode::BAD_REQUEST,
        )
    })?;

    state.purge.update(normalize(parsed)).map_err(|e| {
        error_response(
            &format!("Failed to save purge settings: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )
    })?;

    Ok(Json(payload(&state)))
}

/// GET /api/purge/presets —— 内置预设检索（不带正文，只带预览）。
///
/// 查询参数：`q` 关键词（匹配 id/vendor/model/title/tags）、`vendor` 厂商精确
/// 匹配（忽略大小写）、`model` 模型名匹配（可选，口径同 q/vendor：
/// trim 后为空按未传处理）、`limit` 条数（默认 50，上限 500）。
/// `vendors` 的计数按**过滤后**结果统计，前端 chip 数量才会跟着搜索词变。
///
/// 依赖：`presets::list` 的 `model: Option<&str>` 参数由 rust-core 提供；
/// 在该签名落地前，本调用按约定签名（`q, vendor, model, limit`）接线，
/// 合入顺序上必须后于 rust-core 的 presets.rs 变更，否则编译不过。
pub async fn handle_list_purge_presets(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<PresetQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;

    let limit = query.limit.unwrap_or(presets::DEFAULT_LIMIT);
    let (items, vendors, total) = presets::list_page(
        query.q.as_deref(),
        query.vendor.as_deref(),
        query.model.as_deref(),
        query.offset.unwrap_or(0),
        limit,
    );

    // 统一 API 信封：前端 `request()` 原样返回 JSON，页面按 `{success,data}`
    // 解包（与 /api/purge 一致）。此前直接返回裸对象，`!res.success` 恒真
    // → 破甲控制台「预设列表加载失败」。
    Ok(Json(json!({
        "success": true,
        "data": { "items": items, "vendors": vendors, "total": total },
    })))
}

/// GET /api/purge/presets/{id} —— 单条预设详情（含完整正文）。
pub async fn handle_get_purge_preset(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;

    match presets::get(&id) {
        Some(detail) => Ok(Json(json!({ "success": true, "data": detail }))),
        None => Err(error_response(
            &format!("Preset not found: {id}"),
            StatusCode::NOT_FOUND,
        )),
    }
}

/// POST /api/purge/presets/{id}/apply —— 把预设落到生效位置。
///
/// 请求体：`{ "target": "structured_template" | "skill_file", "activate": bool }`
/// - `structured_template`：写入配置并**立即落库热生效**，返回新的完整配置；
/// - `skill_file`：只写盘（不改配置），由后续 `skill_inject` 扫描命中。
///
/// 两种目标都不改数据面转发逻辑，也不自动启用任何治理开关。
pub async fn handle_apply_purge_preset(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;

    let target: ApplyTarget = match body.get("target").and_then(|v| v.as_str()) {
        Some("structured_template") => ApplyTarget::StructuredTemplate,
        Some("skill_file") => ApplyTarget::SkillFile,
        Some(other) => {
            return Err(error_response(
                &format!("Invalid target: {other} (expected structured_template | skill_file)"),
                StatusCode::BAD_REQUEST,
            ))
        }
        None => {
            return Err(error_response(
                "Missing target: expected structured_template | skill_file",
                StatusCode::BAD_REQUEST,
            ))
        }
    };
    let activate = body
        .get("activate")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let mut cfg = state.purge.snapshot().as_ref().clone();
    let applied = presets::apply(&mut cfg, &id, target, activate).map_err(|e| {
        // 预设不存在 / id 不可用作路径段 / 写盘失败都归到这里：
        // 前两者是调用方输入问题（400），写盘失败是环境问题（500）。
        let code = if presets::get(&id).is_some() {
            StatusCode::INTERNAL_SERVER_ERROR
        } else {
            StatusCode::NOT_FOUND
        };
        error_response(&format!("Failed to apply preset: {e}"), code)
    })?;

    // 只有结构化模板需要落库；技能文件只写盘，配置保持不变。
    let settings = if target == ApplyTarget::StructuredTemplate {
        state.purge.update(normalize(cfg)).map_err(|e| {
            error_response(
                &format!("Failed to save purge settings: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            )
        })?
    } else {
        state.purge.snapshot().as_ref().clone()
    };

    Ok(Json(json!({
        "success": true,
        "data": { "applied": applied, "settings": settings },
    })))
}

/// POST /api/purge/skills/import —— 导入技能包（ZIP/TAR/TGZ）到破甲技能目录。
///
/// multipart 表单字段：
/// - `file`：归档文件（必填，ZIP / TAR / TGZ，按魔数识别）；
/// - `name`：目标目录名（可选，缺省按上传文件名推导）；
/// - `overwrite`：同名目录是否覆盖（"true"/"1"/"yes"/"on"，默认 false）。
///
/// 安全校验（见 [`crate::purge::skill_import`]）：拒绝符号链接/绝对路径/`..`/
/// 保留名/重复路径，条目数与解压后总大小双上限；归档必须含 `SKILL.md`。只写盘，
/// 不改配置，不自动启用任何治理开关。
pub async fn handle_import_purge_skills(
    State(state): State<AppState>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;

    const MAX_FIELDS: usize = 16;
    let mut file_bytes: Option<Vec<u8>> = None;
    let mut filename = String::new();
    let mut name = String::new();
    let mut overwrite = false;
    let mut fields = 0usize;

    while let Some(mut field) = multipart.next_field().await.map_err(|e| {
        error_response(
            &format!("读取 multipart 失败：{e}"),
            StatusCode::BAD_REQUEST,
        )
    })? {
        fields += 1;
        if fields > MAX_FIELDS {
            return Err(error_response(
                "multipart 字段过多",
                StatusCode::BAD_REQUEST,
            ));
        }
        let field_name = field.name().unwrap_or_default().to_string();
        let file_name = field.file_name().map(|s| s.to_string());
        let mut buf: Vec<u8> = Vec::new();
        while let Some(chunk) = field
            .chunk()
            .await
            .map_err(|e| error_response(&format!("读取字段失败：{e}"), StatusCode::BAD_REQUEST))?
        {
            if buf.len() + chunk.len() > crate::purge::skill_import::MAX_ARCHIVE_BYTES {
                return Err(error_response(
                    &format!(
                        "归档超过 {} MiB 上限",
                        crate::purge::skill_import::MAX_ARCHIVE_BYTES / 1024 / 1024
                    ),
                    StatusCode::PAYLOAD_TOO_LARGE,
                ));
            }
            buf.extend_from_slice(&chunk);
        }
        match field_name.as_str() {
            "file" => {
                filename = file_name.unwrap_or_default();
                file_bytes = Some(buf);
            }
            "name" => name = String::from_utf8_lossy(&buf).trim().to_string(),
            "overwrite" => {
                let value = String::from_utf8_lossy(&buf).to_ascii_lowercase();
                overwrite = matches!(value.trim(), "true" | "1" | "yes" | "on");
            }
            _ => {}
        }
    }

    let bytes = file_bytes
        .ok_or_else(|| error_response("缺少 file 字段（归档文件）", StatusCode::BAD_REQUEST))?;
    let format = crate::purge::skill_import::ArchiveFormat::detect(&bytes)
        .map_err(|e| error_response(&e.to_string(), StatusCode::BAD_REQUEST))?;

    let dir_name = if name.is_empty() {
        crate::purge::skill_import::derive_dir_name(&filename).ok_or_else(|| {
            error_response(
                "无法从文件名推导目录名，请显式提供 name",
                StatusCode::BAD_REQUEST,
            )
        })?
    } else {
        name
    };

    let skill_root = crate::purge::expand_home(&state.purge.snapshot().skill_dir);

    let report = tokio::task::spawn_blocking(move || {
        crate::purge::skill_import::import_archive(
            &bytes,
            format,
            &skill_root,
            &dir_name,
            overwrite,
        )
    })
    .await
    .map_err(|e| {
        error_response(
            &format!("导入任务失败：{e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )
    })?
    .map_err(|e| {
        let message = e.message().to_string();
        let status = match e {
            crate::purge::skill_import::ImportError::Exists(_) => StatusCode::CONFLICT,
            crate::purge::skill_import::ImportError::TooLarge(_) => StatusCode::PAYLOAD_TOO_LARGE,
            crate::purge::skill_import::ImportError::Invalid(_) => StatusCode::BAD_REQUEST,
        };
        error_response(&message, status)
    })?;

    Ok(Json(json!({ "success": true, "data": report })))
}

/// `/api/purge/presets` 的查询参数。
///
/// `model` 为可选过滤：空白（trim 后为空）按未传处理，空白归一在
/// `presets::list` 内完成，与 q/vendor 同口径。
#[derive(Debug, Deserialize)]
pub struct PresetQuery {
    q: Option<String>,
    vendor: Option<String>,
    model: Option<String>,
    /// 跳过前 N 条（「加载更多」用；缺省 0 = 第一页）。
    offset: Option<usize>,
    limit: Option<usize>,
}

/// POST /api/purge/preview —— 改写试跑（dry-run）。
///
/// 请求体：`{ "messages": [{"role":"user","content":"..."}], "settings"?: {...} }`
/// - `settings` 缺省时用当前生效配置；
/// - 返回应用全部已启用治理项后的消息列表，**不写统计、不落库**。
///
/// 供运维在保存前验证改写规则 / 模板 / 技能注入的实际效果。
pub async fn handle_preview_purge(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;

    let cfg = match body.get("settings").filter(|v| !v.is_null()) {
        Some(raw) => {
            let parsed: PurgeSettings = serde_json::from_value(raw.clone()).map_err(|e| {
                error_response(
                    &format!("Invalid purge settings: {e}"),
                    StatusCode::BAD_REQUEST,
                )
            })?;
            normalize(parsed)
        }
        None => state.purge.snapshot().as_ref().clone(),
    };

    let raw_messages = body
        .get("messages")
        .and_then(|v| v.as_array())
        .ok_or_else(|| {
            error_response(
                "Missing messages array: expected [{role, content}]",
                StatusCode::BAD_REQUEST,
            )
        })?;
    if raw_messages.is_empty() {
        return Err(error_response(
            "messages array must not be empty",
            StatusCode::BAD_REQUEST,
        ));
    }
    let mut messages: Vec<ChatMessage> = raw_messages
        .iter()
        .map(|m| {
            let role = match m.get("role").and_then(|v| v.as_str()).unwrap_or("user") {
                "system" => Role::System,
                "assistant" => Role::Assistant,
                "tool" => Role::Tool,
                _ => Role::User,
            };
            let content = m
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            ChatMessage {
                role,
                content: Some(content),
                content_blocks: None,
                name: None,
                tool_call_id: None,
                tool_calls: None,
                reasoning: None,
            }
        })
        .collect();

    // `model` 可选：给了就一并预演「按模型自动套用」的预设前插（与数据面同链），
    // 不给则只跑治理（旧行为逐字节不变）。
    let model = body
        .get("model")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let applied = crate::purge::preview_request(&mut messages, &cfg, &model);

    let out: Vec<Value> = messages
        .iter()
        .map(|m| {
            let role = match m.role {
                Role::System => "system",
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::Tool => "tool",
            };
            json!({ "role": role, "content": m.content })
        })
        .collect();

    Ok(Json(json!({
        "success": true,
        "data": {
            "applied": { "changed": applied.changed, "skill_hit": applied.skill_hit },
            "detail": applied.detail,
            "messages": out,
        }
    })))
}

fn payload(state: &AppState) -> Value {
    let settings = state.purge.snapshot();
    json!({
        "success": true,
        "data": {
            "settings": settings.as_ref(),
            "stats": crate::purge::snapshot(),
            // 时间窗计数（beta 迭代：1h / 24h；前端切换统计口径用）
            "stats_1h": crate::purge::windowed_snapshot(3_600_000),
            "stats_24h": crate::purge::windowed_snapshot(86_400_000),
            // 上次运行（关机时落盘）的统计快照；无记录 → null
            "stats_last_run": state.purge.last_run(),
            // 配置体检：「开了但不生效 / 自相矛盾」的静态检查结果。
            // `config_warning_items` 带 code（前端跳修复入口）；`config_warnings`
            // 旧字符串形态保留（旧前端兼容）。
            "config_warnings": crate::purge::config_warnings(&settings),
            // 体检项补 `severity`：由 code 纯函数推导（不存进结构体，避免两处
            // 来源不一致）；前端按级排序着色，Error 优先。
            "config_warning_items": crate::purge::config_warning_items(&settings)
                .into_iter()
                .map(|item| {
                    let severity = crate::purge::severity_of(item.code);
                    json!({
                        "code": item.code,
                        "message": item.message,
                        "severity": severity,
                    })
                })
                .collect::<Vec<_>>(),
            // 技能目录扫描快照（名称+关键词+正文字符数；skill_inject 配置的可验证性）
            "skills": crate::purge::skill_briefs(&settings.skill_dir),
            "runtime": {
                "pid": crate::purge::pid(),
                "uptime_secs": crate::purge::uptime_secs(),
                "upstream": settings.upstream_label,
                // 当前在途请求数（真实并发，进程内即时值；非累计计数）
                "inflight": state.inflight.len(),
            }
        }
    })
}

fn normalize(mut cfg: PurgeSettings) -> PurgeSettings {
    cfg.retry_limit = cfg.retry_limit.clamp(1, 32);
    cfg.fallback_trigger_attempt = cfg.fallback_trigger_attempt.clamp(1, 32);
    cfg.skill_inject_limit = cfg.skill_inject_limit.clamp(16, 20_000);

    cfg.reasoning_floor = trim_or(&cfg.reasoning_floor, "low");
    cfg.auto_preset_id = cfg.auto_preset_id.trim().to_string();
    cfg.skill_dir = cfg.skill_dir.trim().to_string();
    if cfg.skill_dir.is_empty() {
        cfg.skill_dir = crate::purge::DEFAULT_SKILL_DIR.to_string();
    }
    cfg.upstream_label = cfg.upstream_label.trim().to_string();

    cfg.fallback_models = cfg
        .fallback_models
        .into_iter()
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty())
        .take(16)
        .collect();

    cfg.rewrite_rules = cfg
        .rewrite_rules
        .into_iter()
        .filter_map(|rule| {
            let from = rule.from.trim().to_string();
            if from.is_empty() {
                return None;
            }
            Some(RewriteRule {
                from,
                to: rule.to.trim().to_string(),
                enabled: rule.enabled,
            })
        })
        .take(200)
        .collect();

    cfg.synonym_pairs = cfg
        .synonym_pairs
        .into_iter()
        .filter_map(|rule| {
            let from = rule.from.trim().to_string();
            if from.is_empty() {
                return None;
            }
            Some(RewriteRule {
                from,
                to: rule.to.trim().to_string(),
                enabled: rule.enabled,
            })
        })
        .take(200)
        .collect();

    cfg.normalize_strip_prefixes = cfg
        .normalize_strip_prefixes
        .into_iter()
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .take(64)
        .collect();

    // 拒绝话术词表：trim、去空、截断 64（空表语义 = 关闭字面拒绝检测，保留空表）
    cfg.refusal_patterns = cfg
        .refusal_patterns
        .into_iter()
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .take(64)
        .collect();

    // 失败分类词表：同上（空表 = 该类不参与字面判定）
    cfg.failure_filter_keywords = cfg
        .failure_filter_keywords
        .into_iter()
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .take(64)
        .collect();
    cfg.failure_empty_keywords = cfg
        .failure_empty_keywords
        .into_iter()
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .take(64)
        .collect();

    // 结构化模板：空名丢弃、截断 32 条、文本保留原样（仅去尾随空白）
    cfg.structured_templates = cfg
        .structured_templates
        .into_iter()
        .filter_map(|tpl| {
            let name = tpl.name.trim().to_string();
            if name.is_empty() {
                return None;
            }
            Some(crate::purge::PurgeTemplate {
                name,
                text: tpl.text.trim_end().to_string(),
            })
        })
        .take(32)
        .collect();
    cfg.active_template = cfg.active_template.trim().to_string();
    // 选中的模板不存在 → 清空（回退 structured_suffix），避免静默悬空引用
    if !cfg.active_template.is_empty()
        && !cfg
            .structured_templates
            .iter()
            .any(|tpl| tpl.name == cfg.active_template)
    {
        cfg.active_template = String::new();
    }

    cfg
}

fn trim_or(value: &str, fallback: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        fallback.to_string()
    } else {
        trimmed.to_string()
    }
}
