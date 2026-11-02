//! Cline 伪装头管理 API — 对照 Agent2API `api/cline_headers.rs`（GET/PUT）。
//!
//! ── 这个端点管什么 ──────────────────────────────────────────
//! Cline 上游请求的伪装头（`x-client-type: cline-sdk`、平台标识、版本号等）
//! 的**逐键覆盖**。默认值取自 `account_session::registry::SESSION_PROVIDERS`
//! 里 cline 渠道的请求头模板（与官方客户端形态对齐的那一套，上游收紧时跟着
//! 版本走），配置里只存用户改过的键（`usage.cline_headers.overrides`，落
//! config.toml），转发侧逐请求读快照——改完下一个请求立即生效，不重启进程。
//!
//! ── 接口形状 ────────────────────────────────────────────────
//! GET 返回 `{defaults, overrides, effective}`：第一份是代码里的默认值（界面
//! 用它区分「改回了默认」与「本来就没改」），第二份是用户改过的键（原样回
//! 显，界面据此标出哪些行被改过），第三份是合并后的生效值。
//! PUT 收 `{overrides: {...}}` **整体替换**覆盖表：非空值按键覆盖/新增，
//! 空串 = 这个头不发；把某个键从覆盖表里删掉 = 回落默认值。凭证头
//! （Authorization）与非法头名/头值一律 400 拒绝，不落库。

use std::collections::BTreeMap;

use axum::extract::State;
use axum::http::HeaderMap;
use axum::http::{HeaderName, HeaderValue};
use axum::Json;
use serde_json::{json, Map, Value};

use super::super::openai::AppState;
use super::common::verify_admin;
use crate::account_session::registry::SESSION_PROVIDERS;

/// 覆盖表规模上限：伪装头就十来个键，64 是给自定义头留的余量
const MAX_ENTRIES: usize = 64;
/// 单个头名的长度上限（与 `http::HeaderName` 的名字长度上界同值）
const MAX_KEY_LEN: usize = 128;
/// 单个头值的长度上限
const MAX_VALUE_LEN: usize = 4096;

/// **固定头**：每请求动态生成（Authorization 携带账号 token，由桥接层按
/// 凭证构造）或协议必需（Content-Type）。覆盖表里出现它们一律 400。
///
/// 必须拦在配置侧：覆盖表里写一个 Authorization 会在合并时顶掉真正的
/// 凭证，把请求发成别人身份。
const FIXED_HEADERS: [&str; 2] = ["Authorization", "Content-Type"];

/// cline 渠道（SESSION_PROVIDERS 键名）的默认请求头模板。
fn default_headers() -> BTreeMap<String, String> {
    let mut defaults = BTreeMap::new();
    // SESSION_PROVIDERS 里 cline / cline-free / cline-pass 三个条目的 headers
    // 完全一致（见 registry.rs 注释），取第一个即可。
    if let Some((_, meta)) = SESSION_PROVIDERS.iter().find(|(kind, _)| *kind == "cline") {
        for (key, value) in meta.headers {
            defaults.insert((*key).to_string(), (*value).to_string());
        }
    }
    defaults
}

/// GET /api/cline-headers — 默认值 + 覆盖表 + 合并后的生效值。
pub async fn handle_get_cline_headers(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (axum::http::StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    Ok(Json(headers_json(&default_headers())))
}

/// PUT /api/cline-headers — body `{overrides: {"User-Agent": "...", ...}}`
///
/// 整体替换覆盖表（不是增量 merge：界面按「编辑后的整张表」提交，删除某行
/// 就是把它从表里去掉）。校验见 [`validated_overrides`]；非法项直接 400，
/// 不做静默修正。保存后立即生效（转发侧逐请求读快照）。
pub async fn handle_update_cline_headers(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> Result<Json<Value>, (axum::http::StatusCode, Json<Value>)> {
    let mut config = verify_admin(&state, &headers).await?;
    let Some(object) = payload.as_object() else {
        return Err(super::common::error_response(
            "请求体必须是 JSON 对象",
            axum::http::StatusCode::BAD_REQUEST,
        ));
    };
    let Some(raw_overrides) = object.get("overrides") else {
        return Err(super::common::error_response(
            "缺少 overrides",
            axum::http::StatusCode::BAD_REQUEST,
        ));
    };
    let Some(entries) = raw_overrides.as_object() else {
        return Err(super::common::error_response(
            "overrides 必须是对象（头名 → 头值）",
            axum::http::StatusCode::BAD_REQUEST,
        ));
    };
    let overrides = match validated_overrides(entries) {
        Ok(overrides) => overrides,
        Err(message) => {
            return Err(super::common::error_response(
                &message,
                axum::http::StatusCode::BAD_REQUEST,
            ))
        }
    };

    // 持久化（config.toml）+ 进程级快照热更（下一请求生效）
    config.cline_headers.overrides = overrides.clone();
    state.config_manager.update(config).await.map_err(|e| {
        super::common::error_response(
            &format!("Cline 伪装头持久化失败: {e}"),
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        )
    })?;
    crate::config::set_cline_header_overrides(overrides.into_iter().collect());

    tracing::info!("Cline 伪装头覆盖表已更新（下一个 Cline 请求生效）");
    Ok(Json(json!({
        "success": true,
        "data": headers_json(&default_headers())
    })))
}

/// 覆盖表的校验与归一（PUT 的本体）。
///
/// 非法项一律 `Err`（400 文案），不做静默修正。**字符合法性必须在这里拦**：
/// 头名不是 HTTP token（含空格、冒号、非 ASCII）或头值带控制字符时，桥接层
/// 构造请求的阶段就会失败——于是每一条 Cline 请求都发不出去，而客户端看到
/// 的是「上游请求失败」，与真实原因（配置里有个畸形头名）毫无关系。校验用
/// `HeaderName`/`HeaderValue` 的 `try_from`：与 reqwest 构造请求时用的是
/// **同一套规则**，不需要另维护一张字符白名单。
fn validated_overrides(entries: &Map<String, Value>) -> Result<BTreeMap<String, String>, String> {
    if entries.len() > MAX_ENTRIES {
        return Err(format!("overrides 最多 {MAX_ENTRIES} 项"));
    }
    let mut overrides = BTreeMap::new();
    for (key, value) in entries {
        let key = key.trim();
        if key.is_empty() || key.len() > MAX_KEY_LEN {
            return Err(format!("头名非法或过长: {key:?}"));
        }
        if HeaderName::try_from(key).is_err() {
            return Err(format!(
                "头名含非法字符（只能是字母、数字与 !#$%&'*+-.^_`|~）: {key:?}"
            ));
        }
        if FIXED_HEADERS
            .iter()
            .any(|name| name.eq_ignore_ascii_case(key))
        {
            return Err(format!("{key} 是网关固定头，不能覆盖"));
        }
        let Some(text) = value.as_str() else {
            return Err(format!("头 {key} 的值必须是字符串"));
        };
        if text.len() > MAX_VALUE_LEN {
            return Err(format!("头 {key} 的值过长"));
        }
        if HeaderValue::try_from(text).is_err() {
            return Err(format!("头 {key} 的值含非法字符（不能有换行等控制字符）"));
        }
        overrides.insert(key.to_string(), text.to_string());
    }
    Ok(overrides)
}

/// 响应形状（GET 与 PUT 共用，前端直接用响应刷新界面）。
///
/// `defaults` 一并给出：界面要把「用户把值改回了默认」与「本来就没改」
/// 区分开（前者是一份覆盖、后者不该出现在覆盖表里），没有默认值就做不到
/// 这个判断。合并规则：overrides 的非空值按键覆盖/新增，空串 = 不发这个头
/// （从 effective 里去掉）。
fn headers_json(defaults: &BTreeMap<String, String>) -> Value {
    let overrides: BTreeMap<String, String> = crate::config::cline_header_overrides()
        .into_iter()
        .collect();
    let mut effective = defaults.clone();
    for (key, value) in &overrides {
        if value.is_empty() {
            effective.remove(key);
        } else {
            effective.insert(key.clone(), value.clone());
        }
    }
    json!({
        "defaults": defaults,
        "overrides": overrides,
        "effective": effective,
    })
}
