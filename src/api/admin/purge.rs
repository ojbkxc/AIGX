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

/// 模型管理视图的**进程内缓存**（候选推荐 / 体检审计共用）。
///
/// ── 为什么必须有 ──────────────────────────────────────────
/// `model::manage::build_view` 要把「所有启用渠道 × 渠道模型 × 账号池 × 账号
/// 白名单 × 模型规则 × 定价表 × 映射表」全量聚合一遍（线上 196 个模型、几十条
/// 渠道、上百个账号），是管理面最贵的一次计算。破甲页每次打开都要它（体检要判
/// 「未定价 / 同厂商 / 无承载」，推荐要排候选）——不缓存就等于每次开页面扫全表。
///
/// ── 缓存键与失效 ─────────────────────────────────────────
/// 键是「数据世代」：`(渠道集合指纹, 定价条目数, 模型规则世代)`。这三样任何一样
/// 变了都意味着视图可能变；**没变就直接复用**（TTL 之外再看指纹，避免「刚好在
/// 30 秒内改了渠道，页面还在给旧推荐」）。
///
/// 为什么不用「时间到就重算」：模型管理页本身不缓存（写接口回读最新视图），
/// 破甲页只是**读**。真正的写操作只有渠道/定价/规则三类，它们都会改指纹，
/// 所以按指纹失效既省算力又不会给出过期推荐。
struct ModelViewCache {
    at: std::time::Instant,
    fingerprint: ViewFingerprint,
    view: std::sync::Arc<crate::model::manage::ManageView>,
}

/// 视图指纹：渠道（id / 状态 / 声明规模 / 更新时间）+ 定价条目数 + 模型规则数。
///
/// 只取**长度**不取内容，是刻意的：
/// - 定价对推荐的影响只有「有没有这条价」（`ensure_model_priced` 与 `price.is_some()`），
///   改价的数值不影响候选资格 —— 所以条目数变了才需要重算；
/// - 模型规则同理，只有「关掉/打开某一行」会影响 `enabled` 与候选资格。
///
/// 代价是一次 `list_prices()` 的浅拷贝（200 条左右），远低于 `build_view` 的全量聚合。
#[derive(Debug, Clone, PartialEq, Eq)]
struct ViewFingerprint {
    channels: Vec<(String, String, usize, i64)>,
    prices: usize,
    rules: usize,
}

/// 缓存 TTL：30 秒与技能扫描同档（够把「连续开几次页面」挡掉，又不至于长期陈旧）。
const VIEW_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(30);

static VIEW_CACHE: std::sync::OnceLock<parking_lot::RwLock<Option<ModelViewCache>>> =
    std::sync::OnceLock::new();

fn view_cache() -> &'static parking_lot::RwLock<Option<ModelViewCache>> {
    VIEW_CACHE.get_or_init(|| parking_lot::RwLock::new(None))
}

/// 取视图（`fresh = true` 时跳过缓存）。
///
/// 指纹只 stat 内存里的存储（渠道快照是 Arc、定价是 RwLock 里的 HashMap、
/// 规则集合同样在内存），没有磁盘 I/O。
fn manage_view(state: &AppState, fresh: bool) -> std::sync::Arc<crate::model::manage::ManageView> {
    let fingerprint = view_fingerprint(state);
    if !fresh {
        let guard = view_cache().read();
        if let Some(cached) = guard.as_ref() {
            if cached.fingerprint == fingerprint && cached.at.elapsed() < VIEW_CACHE_TTL {
                return cached.view.clone();
            }
        }
    }
    let view = std::sync::Arc::new(super::model_manage::current_view(state));
    let mut guard = view_cache().write();
    *guard = Some(ModelViewCache {
        at: std::time::Instant::now(),
        fingerprint,
        view: view.clone(),
    });
    view
}

fn view_fingerprint(state: &AppState) -> ViewFingerprint {
    let channels = state
        .channel_store
        .list()
        .iter()
        .map(|channel| {
            (
                channel.id.clone(),
                channel.status.clone(),
                channel.models.len() + channel.discovered_models.len(),
                channel.updated_at,
            )
        })
        .collect();
    ViewFingerprint {
        channels,
        prices: state.pricing_store.list_prices().len(),
        rules: state.model_rules.all_disabled().len(),
    }
}

/// 当前「主模型」——推荐与「同厂商」判定的基准。
///
/// 取设置页的 `default_model`（运维在「系统设置」里指定的默认模型）；留空时
/// 返回空串，推荐会退化成「不做同厂商排除」的通用池（界面会提示这一条）。
fn main_model_of(state: &AppState) -> String {
    let pinned = crate::config::default_model();
    if !pinned.trim().is_empty() {
        return pinned.trim().to_string();
    }
    // 没设默认模型：用模型管理视角的「首选推理模型」兜底（与 agent 同一套排序），
    // 比空着更有信息量，且不会与模型管理页的判断打架。
    crate::model::manage::pick_default_model(&manage_view(state, false)).unwrap_or_default()
}

/// GET /api/purge —— 读取破甲配置、统计快照与进程运行信息。
pub async fn handle_get_purge(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<PurgeQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    // `?skills=fresh` 跳过技能扫描的 30 秒缓存。
    // 「重新扫描」按钮若仍走缓存，用户放好 SKILL.md 点一下界面还是空的，
    // 按钮看起来就是坏的——显式刷新必须真的刷新。
    let fresh = query.skills.as_deref() == Some("fresh");
    // `?models=fresh` 跳过模型视图缓存（改了渠道/定价后立刻看推荐）
    let fresh_models = query.models.as_deref() == Some("fresh");
    Ok(Json(payload_full(&state, fresh, fresh_models)))
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

/// POST /api/purge/stats/reset -- 清零统计，把此刻作为新的观察基线。
///
/// 只清计数（含事件环），不动配置、不写 `last_run` 快照。
pub async fn handle_reset_purge_stats(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    crate::purge::reset_stats();
    Ok(Json(
        json!({ "success": true, "stats": crate::purge::snapshot() }),
    ))
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

    let next = normalize(parsed);
    state.purge.update(next).map_err(|e| {
        error_response(
            &format!("Failed to save purge settings: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )
    })?;

    // 保存后把技能目录建出来（若用户把 skill_dir 改成一个新路径）。
    // 只建目录不建技能：用户在这里只是「指向一个新位置」，没有要求放内容。
    // 建不出来不阻塞保存（配置本身是合法的），体检会报 `skill_dir_missing`。
    if let Err(error) = crate::purge::ensure_skill_dir(&state.purge.snapshot().skill_dir) {
        tracing::warn!("[Purge] 保存后建技能目录失败（体检会报 skill_dir_missing）：{error}");
    }

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
    //
    // `data.auto_preset`：**写路径**的口径。列表里的 `model_match` 是逐条展示用的
    // （`preset_match_for_model`），而真正决定「这个模型的请求会不会被前插预设正文」
    // 的是 `preset_for_model`——两者此前只有展示口径，管理员看着列表上的一排
    // `vendor` 无法判断实际会套哪一条、会不会一条都不套。这里把写路径的结论
    // 直接透出来（同一函数，不另写判定），列表本身仍是展示。
    let write_path = query.model.as_deref().map(|model| {
        let settings = state.purge.snapshot();
        let pinned = settings.auto_preset_id.trim();
        // 通用兜底开关与数据面同参：关掉时这里的 `match`/`applied` 必须一起变成
        // `None`/`false`，否则列表会承诺一条实际不会前插的正文。
        let hit = presets::preset_for_model(model, pinned, settings.universal_fallback);
        let match_kind = hit.as_ref().map(|(_, kind)| match kind {
            presets::PresetMatch::Exact => "exact",
            presets::PresetMatch::Family => "family",
            presets::PresetMatch::Universal => "universal",
            presets::PresetMatch::Pinned => "pinned",
        });
        let preset_id = hit.as_ref().map(|(detail, _)| detail.id.clone());
        // 与 `purge::apply_auto_preset` 的前置条件逐条对齐：总开关 ∧ 自动套用 ∧
        // 命中 ∧ 正文非空。这里不改写任何东西，只回答「会不会套」。
        let body_not_empty = hit
            .as_ref()
            .is_some_and(|(detail, _)| !detail.body.trim().is_empty());
        let applied = settings.enabled && settings.auto_preset && body_not_empty;
        json!({
            "model": model,
            "preset_id": preset_id,
            "match": match_kind,
            "applied": applied,
        })
    });

    let mut data = json!({ "items": items, "vendors": vendors, "total": total });
    if let Some(write_path) = write_path {
        if let Some(obj) = data.as_object_mut() {
            obj.insert("auto_preset".to_string(), write_path);
        }
    }
    Ok(Json(json!({
        "success": true,
        "data": data,
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

/// GET /api/purge/skills/skeleton —— 新建技能的骨架（**格式即解析器契约**）。
///
/// 查询参数：
/// - `name`：技能名（必填，同时决定默认目录名）；
/// - `description`：一句话说明（可选；它的词会被解析器派生成关键词）；
/// - `keywords`：显式关键词（逗号分隔，可选）；
/// - `body`：正文（可选，缺省给占位说明）。
///
/// 只生成文本、不写盘：前端把结果填进编辑器让用户先改再保存（用户改一个字就
/// 被后端覆盖的「自动生成」体验是最差的）。写盘走 `POST /api/purge/skills`。
pub async fn handle_get_skill_skeleton(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<SkeletonQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let name = query.name.as_deref().unwrap_or("").trim();
    if name.is_empty() {
        return Err(error_response(
            "缺少 name：新建技能必须有一个名字（它同时是默认关键词与目录名来源）",
            StatusCode::BAD_REQUEST,
        ));
    }
    let keywords = query
        .keywords
        .as_deref()
        .map(crate::purge::skill_doc::parse_keywords_value)
        .unwrap_or_default();
    let content = crate::purge::skill_doc::skeleton(
        name,
        query.description.as_deref().unwrap_or(""),
        &keywords,
        query.body.as_deref().unwrap_or(""),
    );
    let settings = state.purge.snapshot();
    // 回读校验：把「模板生成的技能会不会生效」当场算出来（不写盘也能知道）
    let validated = crate::purge::skill_doc::validate(&content, settings.skill_inject_limit);
    let (validate_json, validate_error) = match validated {
        Ok(v) => (
            json!({
                "name": v.name,
                "keywords": v.keywords,
                "body_chars": v.body_chars,
                "has_keywords_key": v.has_keywords_key,
                "warnings": v.warnings,
            }),
            Value::Null,
        ),
        Err(message) => (Value::Null, Value::String(message)),
    };
    let dir_name = crate::purge::skill_import::slugify(name).unwrap_or_default();
    let path = crate::purge::skill_doc::target_path(
        &crate::purge::expand_home(&settings.skill_dir),
        &dir_name,
    )
    .map(|p| p.to_string_lossy().to_string())
    .unwrap_or_default();
    Ok(Json(json!({
        "success": true,
        "data": {
            "content": content,
            "dir": dir_name,
            "path": path,
            "skill_dir_resolved": crate::purge::expanded_skill_dir(&settings.skill_dir),
            "validate": validate_json,
            "validate_error": validate_error,
        }
    })))
}

/// `GET /api/purge/skills/skeleton` 的查询参数。
#[derive(Debug, Default, Deserialize)]
pub struct SkeletonQuery {
    name: Option<String>,
    description: Option<String>,
    keywords: Option<String>,
    body: Option<String>,
}

/// `POST /api/purge/skills` 的请求体（JSON；纯文本技能，不走归档）。
#[derive(Debug, Deserialize)]
pub struct SkillWriteRequest {
    /// 技能正文（完整 `SKILL.md`，含 frontmatter）。必填。
    pub content: String,
    /// 目标目录名（可选；缺省取 frontmatter 的 `name`）。
    pub dir: Option<String>,
    /// 覆盖同名目录（默认 false）。
    #[serde(default)]
    pub overwrite: bool,
}

/// POST /api/purge/skills —— 写一份纯文本 `SKILL.md`（新建 / 粘贴 / 上传 .md）。
///
/// 与归档导入（`/api/purge/skills/import`）的分工：
/// - 归档导入：ZIP/TAR/TGZ 解压（内部再有 `SKILL.md` 的**结构**校验）；
/// - 本端点：**一份** `SKILL.md` 的**内容**校验（解析器读不出正文就拒绝）。
///
/// 两者都只写盘，不改配置，不自动启用任何开关。目录不存在会建出来
/// （`skill_doc::write_skill` 内部 `create_dir_all`）。
pub async fn handle_write_skill(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<SkillWriteRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let settings = state.purge.snapshot();
    if body.content.trim().is_empty() {
        return Err(error_response(
            "content 为空：技能正文不能为空",
            StatusCode::BAD_REQUEST,
        ));
    }
    // 大小上限与归档单文件同量级（技能正文是注进 system 的文本，几 MB 已离谱）
    const MAX_SKILL_TEXT_BYTES: usize = 2 * 1024 * 1024;
    if body.content.len() > MAX_SKILL_TEXT_BYTES {
        return Err(error_response(
            &format!(
                "技能正文超过 {} MiB 上限",
                MAX_SKILL_TEXT_BYTES / 1024 / 1024
            ),
            StatusCode::PAYLOAD_TOO_LARGE,
        ));
    }

    // 目录名：显式给了就用，否则取解析出的 name（最终都会经 slugify 规整）
    let parsed = crate::purge::parse_skill_parts(&body.content);
    let dir_name = body
        .dir
        .as_deref()
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .map(str::to_string)
        .or_else(|| parsed.as_ref().map(|(name, _, _, _)| name.clone()))
        .unwrap_or_default();
    if dir_name.trim().is_empty() {
        return Err(error_response(
            "无法确定目录名：frontmatter 里没有 name，也没传 dir",
            StatusCode::BAD_REQUEST,
        ));
    }

    let skill_root = crate::purge::expand_home(&settings.skill_dir);
    let content = body.content.clone();
    let overwrite = body.overwrite;
    let limit = settings.skill_inject_limit;
    let dir_name_owned = dir_name.clone();

    let report = tokio::task::spawn_blocking(move || {
        crate::purge::skill_doc::write_skill(
            &skill_root,
            &dir_name_owned,
            &content,
            overwrite,
            limit,
            "text",
        )
    })
    .await
    .map_err(|e| {
        error_response(
            &format!("写入任务失败：{e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )
    })?
    .map_err(|message| {
        // 目录已存在且未要求覆盖是**冲突**（409），其余是内容/环境问题
        // 两个分支都返回 CONFLICT，合成一个条件（clippy `if_same_then_else`）。
        let status = if message.contains("已存在") || message.contains("非普通目录") {
            StatusCode::CONFLICT
        } else if message.contains("失败") {
            StatusCode::INTERNAL_SERVER_ERROR
        } else {
            StatusCode::BAD_REQUEST
        };
        error_response(&message, status)
    })?;

    // 写完立刻让技能缓存失效，界面「重新扫描」能马上看到（写盘即生效）
    crate::purge::skill_doc_briefs_with(&settings.skill_dir, true);

    Ok(Json(json!({
        "success": true,
        "data": report,
        "skills": crate::purge::skill_doc_briefs_with(&settings.skill_dir, false),
    })))
}

/// GET /api/purge/fallback/suggest —— 异源递进的候选推荐。
///
/// 查询参数：`limit`（缺省 3，夹紧 1..=8）、`models=fresh`（跳过视图缓存）、
/// `model`（覆盖主模型；缺省用设置页的默认模型）。
///
/// 与 `GET /api/purge` 里的 `fallback_suggestion` 同一份实现——单开一个端点是为了
/// 「模型管理里刚改了渠道/定价 → 回破甲页点一下重算」不必重拉整页快照。
pub async fn handle_suggest_fallback(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<SuggestQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let fresh = query.models.as_deref() == Some("fresh");
    let view = manage_view(&state, fresh);
    let main = query
        .model
        .as_deref()
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| main_model_of(&state));
    let limit = query
        .limit
        .unwrap_or(crate::purge::fallback_plan::DEFAULT_RECOMMEND);
    Ok(Json(json!({
        "success": true,
        "data": crate::purge::fallback_plan::recommend(view.as_ref(), &main, limit),
        "main_model": main,
    })))
}

/// `/api/purge/fallback/suggest` 的查询参数。
#[derive(Debug, Default, Deserialize)]
pub struct SuggestQuery {
    limit: Option<usize>,
    models: Option<String>,
    model: Option<String>,
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

/// `GET /api/purge` 的查询参数。
///
/// - `?skills=fresh`：跳过技能目录扫描的 30 秒缓存（控制台「重新扫描」用）；
/// - `?models=fresh`：跳过模型管理视图缓存（改了渠道/定价后立刻重算推荐）。
#[derive(Debug, Default, Deserialize)]
pub struct PurgeQuery {
    skills: Option<String>,
    models: Option<String>,
}

fn payload(state: &AppState) -> Value {
    payload_with(state, false)
}

fn payload_with(state: &AppState, fresh_skills: bool) -> Value {
    payload_full(state, fresh_skills, false)
}

fn payload_full(state: &AppState, fresh_skills: bool, fresh_models: bool) -> Value {
    let settings = state.purge.snapshot();
    let view = manage_view(state, fresh_models);
    let main_model = main_model_of(state);
    // 推荐与体检审计共用同一份视图（同一次请求里只算一遍视图）
    let recommendation = crate::purge::fallback_plan::recommend(
        view.as_ref(),
        &main_model,
        crate::purge::fallback_plan::DEFAULT_RECOMMEND,
    );
    json!({
        "success": true,
        "data": {
            "settings": settings.as_ref(),
            "stats": crate::purge::snapshot(),
            // 统计基线（unix 秒）：清零过就是清零时刻，否则是进程启动时刻
            "stats_since": crate::purge::stats_baseline_at(),
            // 时间窗计数（beta 迭代：1h / 24h；前端切换统计口径用）
            "stats_1h": crate::purge::windowed_snapshot(3_600_000),
            "stats_24h": crate::purge::windowed_snapshot(86_400_000),
            // 上次运行（关机时落盘）的统计快照；无记录 → null
            "stats_last_run": state.purge.last_run(),
            // 配置体检：「开了但不生效 / 自相矛盾」的静态检查结果。
            // `config_warning_items` 带 code（前端跳修复入口）；`config_warnings`
            // 旧字符串形态保留（旧前端兼容）。
            //
            // 带视图的版本额外判「异源模型未定价 / 同厂商 / 无承载 / 不在清单」——
            // 这四条光看配置判不出来，必须有模型清单（见 `config_warning_items_with`）。
            "config_warnings": crate::purge::config_warnings_with(&settings, Some(view.as_ref()), &main_model),
            // 体检项补 `severity`：由 code 纯函数推导（不存进结构体，避免两处
            // 来源不一致）；前端按级排序着色，Error 优先。
            "config_warning_items": crate::purge::config_warning_items_with(&settings, Some(view.as_ref()), &main_model)
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
            // 技能目录扫描快照（名称+关键词+正文字符数 + 有没有 keywords 行 + 文件路径；
            // skill_inject 配置的可验证性。走 `SkillDocBrief`——`SkillBrief` 的
            // 形状保持不变，agent 面 consume 它，加字段会打穿那边）
            "skills": crate::purge::skill_doc_briefs_with(&settings.skill_dir, fresh_skills),
            // 技能目录的展开后绝对路径：`~` 的展开依赖服务进程的 HOME，
            // 与用户交互式 shell 可能不同——放错目录时靠这个才能定位
            "skill_dir_resolved": crate::purge::expanded_skill_dir(&settings.skill_dir),
            // 技能目录是否真的存在（前端「新建技能」前的提示；服务启动会尝试建）
            "skill_dir_exists": std::fs::metadata(crate::purge::expand_home(&settings.skill_dir))
                .map(|m| m.is_dir())
                .unwrap_or(false),
            // 当前注入上限（技能快照里每条正文是否会被截断，前端据此高亮）
            "skill_inject_limit": settings.skill_inject_limit,
            // 异源递进的候选推荐：从「模型管理」的启用模型里按真异源挑，
            // 一键填入用。`main_model` 与体检用的是同一个值（口径统一）。
            "fallback_suggestion": recommendation,
            "main_model": main_model,
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
