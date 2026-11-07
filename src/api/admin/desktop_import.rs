//! 桌面端登录态导入（Phase1-#23 desktop 段）——**安全红线最高的一段**。
//!
//! ── 语义（对齐 A2A 添加账号的 desktop 段）──────────────────────
//! 从本机桌面客户端的登录态文件里读凭证、导入账号池。**只读**：本模块
//! 对文件系统只做 read，不写不删。
//!
//! ── 安全红线（任务书原文，逐条落实）────────────────────────────
//! 1. **固定路径白名单**：文件路径写死在 [`DESKTOP_SOURCES`]，HTTP 请求体
//!    **不接受任何路径参数**——接受用户路径 = 任意文件读取漏洞。
//! 2. **仅管理员**：路由挂 verify_admin（见调用点）。
//! 3. **平台门**：DPAPI 解密仅 Windows；非 Windows 平台端点如实返回
//!    「不支持」（`#[cfg(windows)]` 分支不参与其他平台编译，6 平台矩阵
//!    照常出包）。
//! 4. **大小上限**：[`MAX_SOURCE_FILE_BYTES`] 兜底，防读到大文件打爆内存。
//! 5. **明文不留痕**：解出的凭证只经 `seal_or_keep` 进账号存储；日志里
//!    只出现来源名与字节数，绝不打印凭证内容。
//!
//! ── 与上游的差距（如实声明，不装完成）────────────────────────
//! 上游 A2A 的桌面导入实现在**闭源 provider 插件**里（本仓只剩
//! `account_bootstrap.rs:15-17` 的注释——「AutoClaw 读的都是客户端私有
//! 文件布局与 safeStorage 解密链，属于上游知识」；chatglm 的 Chromium
//! os_crypt Cookies 同理）。按「不自创上游知识」纪律：
//! - 白名单里的**明文形态**来源（auth.json 直接是令牌 JSON 的）本模块
//!   真实实现，读出即导入；
//! - **DPAPI/safeStorage 形态**的来源在 `SUPPORTED` 列表中如实标
//!   `encrypted: true` 并回「该来源的解密链是上游插件私有知识，本仓库
//!   不具备」——等拿到解密知识再补实现，端点形状已定（discover/import
//!   两个动作 + 来源白名单），补实现只动一个 match 分支。
//!
//! 性能影响：冷路径（管理面手动触发，单文件读取，大小有界）；数据面零改动。

use serde::Deserialize;
use serde_json::{json, Value};

use super::super::openai::AppState;
use super::common::error_response;
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::Json,
};

use crate::account_login::paste::normalize_token;

/// 白名单来源 → provider kind 的映射（cline 系客户端 → cline 家）。
fn provider_of_source(src: &DesktopSource) -> &'static str {
    match src.id {
        "cline" => "cline",
        // 其余来源当前都是 encrypted（不可达），占位防未来加明文来源漏配
        _ => "cline",
    }
}

/// 单个来源文件的读取上限（#23 任务书：防大文件打爆内存）。
///
/// 桌面客户端登录态文件实际都在几十 KB 量级；4 MiB 已是极宽裕的上限。
pub const MAX_SOURCE_FILE_BYTES: u64 = 4 * 1024 * 1024;

/// 一个桌面登录态来源（白名单条目）。
pub struct DesktopSource {
    /// 来源 id（HTTP 请求里用这个，不是路径）
    pub id: &'static str,
    /// 展示名（前端列表直接渲染）
    pub label: &'static str,
    /// 固定绝对路径（唯一合法的读取目标；`~` = 用户 home）
    pub path: &'static str,
    /// true = 需要 DPAPI/safeStorage 解密（上游插件私有知识，暂不支持）
    pub encrypted: bool,
    /// 令牌 JSON 的取值路径（`{"auth":{"token":...}}` → `auth.token`）；
    /// None = 整个文件内容就是令牌
    pub token_pointer: Option<&'static str>,
}

/// 白名单（写死；来源与上游 add-account-configs 的 desktop 段对齐，
/// 但**解密知识**除外——见模块头「与上游的差距」）。
pub static DESKTOP_SOURCES: &[DesktopSource] = &[
    DesktopSource {
        id: "autoclaw",
        label: "AutoClaw 客户端登录态",
        path: "%APPDATA%/AutoClaw/auth.json",
        encrypted: true, // safeStorage（DPAPI + AES-GCM）= 上游插件私有知识
        token_pointer: None,
    },
    DesktopSource {
        id: "box-agent",
        label: "BoxAgent 客户端登录态",
        path: "~/.box-agent/config/auth.json",
        encrypted: true,
        token_pointer: None,
    },
    DesktopSource {
        id: "meituan-catpaw",
        label: "美团猫爪客户端登录态",
        path: "~/.meituan-catpaw/auth.json",
        encrypted: true,
        token_pointer: None,
    },
    DesktopSource {
        id: "cline",
        label: "Cline 客户端登录态",
        path: "~/.cline/config/auth.json",
        encrypted: false,
        token_pointer: Some("auth.token"),
    },
];

/// 把白名单路径展开成真实路径（`%APPDATA%` / `~` 两个约定；**不接受外部
/// 传入的路径**——本函数只吃白名单里的字面量）。
fn resolve_path(pattern: &str) -> Option<std::path::PathBuf> {
    if let Some(rest) = pattern.strip_prefix("%APPDATA%/") {
        // Windows 环境变量；非 Windows 上这个来源本就标记为不支持
        let appdata = std::env::var("APPDATA").ok()?;
        return Some(std::path::Path::new(&appdata).join(rest));
    }
    if let Some(rest) = pattern.strip_prefix("~/") {
        let home = std::env::var(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).ok()?;
        return Some(std::path::Path::new(&home).join(rest));
    }
    None
}

/// 来源清单（GET /api/account-sessions/desktop/sources 的投影）。
pub fn list_sources() -> Vec<Value> {
    DESKTOP_SOURCES
        .iter()
        .map(|src| {
            json!({
                "id": src.id,
                "label": src.label,
                "path": src.path,
                "encrypted": src.encrypted,
                // 平台可用性：非 Windows 一律不支持（DPAPI 是 Windows 技术；
                // 明文来源在 mac/linux 理论可读，但白名单按上游口径桌面客户端
                // 都是 Windows 形态，统一收口到 Windows-only，避免各端行为漂移）
                "supported": cfg!(windows) && !src.encrypted,
                "unsupportedReason": if !cfg!(windows) {
                    Some("仅 Windows 可用（来源文件是 Windows 客户端的登录态）".to_string())
                } else if src.encrypted {
                    Some("该来源的 DPAPI/safeStorage 解密链是上游插件私有知识，暂不支持".to_string())
                } else {
                    None
                },
            })
        })
        .collect()
}

/// 读一个明文来源的令牌（大小上限内读文件 → 取值路径 → 归一化）。
///
/// 凭证内容**不进日志**——返回值之外只留来源 id 供审计。
fn read_plaintext_token(src: &DesktopSource) -> Result<String, String> {
    let path =
        resolve_path(src.path).ok_or_else(|| "来源文件路径无法解析（环境变量缺失）".to_string())?;
    let meta = std::fs::metadata(&path).map_err(|_| format!("来源文件不存在: {}", src.path))?;
    if meta.len() > MAX_SOURCE_FILE_BYTES {
        return Err(format!(
            "来源文件过大（{} 字节 > 上限 {MAX_SOURCE_FILE_BYTES}），疑似不是登录态文件",
            meta.len()
        ));
    }
    let content = std::fs::read_to_string(&path).map_err(|e| format!("读取失败: {e}"))?;
    let raw = match src.token_pointer {
        // E0515：先落成 owned Value 再取路径字段（不能从局部 &Value 借引用返回）
        Some(pointer) => {
            let parsed: Value = serde_json::from_str(&content)
                .map_err(|e| format!("来源文件不是合法 JSON: {e}"))?;
            parsed
                .pointer(pointer)
                .and_then(Value::as_str)
                .map(|s| s.to_string())
                .ok_or_else(|| "文件里找不到令牌字段（结构不符合预期）".to_string())?
        }
        None => content,
    };
    Ok(normalize_token(&raw))
}

/// POST /api/account-sessions/desktop-import 的请求体。
///
/// **刻意只有 `source_id` 一个字段**——没有路径、没有文件名、没有内容。
#[derive(Debug, Deserialize)]
pub struct DesktopImportRequest {
    /// 白名单来源 id（见 [`DESKTOP_SOURCES`]）
    pub source_id: String,
    /// 可选备注名（缺省 = 来源展示名）
    #[serde(default)]
    pub name: Option<String>,
}

/// 处理 desktop 导入：白名单分发 + 平台门 + 凭证经既有创建链落库。
///
/// 处理 desktop 导入：白名单分发 + 平台门，落库直接复用
/// [`super::account_sessions::handle_create_account_session`]（与前端创建
/// 账号同一入口，全部运维链路生效）。
pub async fn handle_desktop_import(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<DesktopImportRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    super::common::verify_admin(&state, &headers).await?;
    let src = DESKTOP_SOURCES
        .iter()
        .find(|s| s.id == body.source_id)
        .ok_or_else(|| {
            error_response(
                "未知的来源 id（来源清单见 GET /api/account-sessions/desktop/sources）",
                StatusCode::BAD_REQUEST,
            )
        })?;
    if !cfg!(windows) {
        return Err(error_response(
            "仅 Windows 可用（来源文件是 Windows 客户端的登录态）",
            StatusCode::NOT_IMPLEMENTED,
        ));
    }
    if src.encrypted {
        // 上游插件私有知识（safeStorage/DPAPI 链），如实不支持——形状已定，
        // 拿到解密知识后在此补解密分支即可
        return Err(error_response(
            "该来源的 DPAPI/safeStorage 解密链是上游插件私有知识，暂不支持；请用「填写凭证」粘贴令牌",
            StatusCode::NOT_IMPLEMENTED,
        ));
    }
    // 明文来源：读取 + 既有校验链（与粘贴式登录同一路径）
    let token =
        read_plaintext_token(src).map_err(|e| error_response(&e, StatusCode::BAD_REQUEST))?;
    let name = body
        .name
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| src.label.to_string());
    let provider = provider_of_source(src);
    // 落库走与粘贴式完全相同的链：AccountSessionRequest → apply_request
    //（credential 编码 seal_or_keep 在 upsert 内）——手工粘贴账号的全部
    // 运维链路（单删/批量/导出/限制器）对桌面导入账号同样生效。
    // 明文只在本函数栈上存活，不进日志、不进审计载荷。
    let req = super::account_sessions::AccountSessionRequest {
        channel_id: String::new(),
        name: name.clone(),
        provider: provider.to_string(),
        base_url: String::new(),
        chat_path: None,
        api_key: Some(token),
        proxy: None,
        status: "enabled".to_string(),
        priority: None,
        weight: None,
        max_concurrency: None,
        models: Vec::new(),
        extra_headers: None,
        refresh_token: None,
        credential_kind: None,
        expires_at: None,
        usage_query_interval_secs: None,
        limiters: None,
        limiter_groups: None,
        auto_channel: Some(super::account_sessions::AutoChannelSpec {
            name: Some(name.clone()),
            base_url: None,
            quirks: None,
            balance_scheme: None,
            model_mapping: Default::default(),
            cost_pricing: Default::default(),
        }),
    };
    // 走与前端创建账号**完全相同的入口**（SSRF 校验/审计/自动建渠道/
    // 目录回填同链）——直接调 handle_create_account_session（axum 提取器
    // 就是普通参数，与运维 Agent 复用 legacy keys 处理函数同一模式），
    // 桌面导入账号天然获得全部运维链路（单删/批量/导出/限制器）。
    let resp = super::account_sessions::handle_create_account_session(
        State(state),
        headers,
        axum::Json(req),
    )
    .await;
    tracing::info!("[DesktopImport] source={} 已导入（凭证不落日志）", src.id);
    resp
}

/// GET /api/account-sessions/desktop/sources —— 白名单清单（前端渲染）。
pub async fn handle_desktop_sources(
    State(_state): State<AppState>,
    _headers: HeaderMap,
) -> Json<Value> {
    Json(json!({ "success": true, "data": list_sources() }))
}
