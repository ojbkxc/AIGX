//! 本机 Agent 客户端接入（`/api/agents*`）—— 对标 Agent2API v3.5.1 `core/agents/*`。
//!
//! ── 这个面是什么 ────────────────────────────────────────────
//! 把「本机 CLI / 桌面客户端（Claude Code、Codex、OpenCode、ZCode、WorkBuddy…）」
//! 接到本网关上：读它们的配置文件、算出「接上网关需要改哪几个键」、预览差异、
//! 用户确认后写入，并保留可回滚的备份。
//!
//! ── 为什么是「预览 → 令牌 → 应用」三段 ──────────────────────
//! 写入的是**用户本机的真实配置文件**。段与段之间文件可能被别人改掉（另一个
//! 窗口、客户端自己、`git checkout`），所以应用时必须 CAS：令牌里含
//! 「应用时的磁盘内容哈希」，与预览时不一致就 409 要求重新预览。这与 A2A
//! `core/agents/apply.rs` 的确认令牌同语义。
//!
//! ── 边界（宁可少写，不写错）────────────────────────────────
//! 每家客户端的配置形状不同，只有**已验证形状**的客户端才 `writable`：
//! - `claude-code`：`settings.json` 的 `env.ANTHROPIC_BASE_URL` / `env.ANTHROPIC_AUTH_TOKEN`
//! - `codex`：`config.toml` 的 `model_providers.<id>` + `model_provider`
//! - `opencode` / `zcode`：JSON 的 `provider.<id>`（openai-compatible 条目）
//! - `workbuddy`：JSON 的模型清单数组（只追加，不改其它条目）
//!
//! 其余（`gemini-cli` / `cursor-cli` / `dsh` / `claude-desktop` / `kimi-code`）
//! 只做**检测 + 读取**，`writable=false` 并给出原因 —— 假装写入等于把用户的
//! 配置文件写坏。补齐它们需要逐家的写入器（A2A 用 `clients/*.rs` 一家一个），
//! 那是后续批次。
//!
//! ── 方案库落点（为什么借用账号会话的 FileStore）──────────────
//! AIGX 没有 `state.db`；`AppState` 上只有各域自己的 KV（`alert_store` 属告警、
//! `scheduled_task_store` 属排期），`upstream_checkin.file_store()` 又把键写进
//! 签到命名空间。`AccountSessionStore::store()` 持有的是同一份**通用** KV，且
//! 按键前缀分域，所以方案库 / 路径覆盖 / 已写入记录统一落它：
//! `agent:profiles:{clientId}`、`agent:path-override`、`agent:config-applied`。
//!
//! ── 资源（MCP / Skills）面的读写口径 ────────────────────────
//! 读取：真实文件扫描（`Target::mcp_path()` 的条目 + `Target::skill_roots()`
//! 下的目录/单文件），只读客户端也能读。
//! 写入：仍走「预览 → 令牌 → 应用」，目标路径只来自 `Target` 的路径表，
//! 请求里的任何路径都不参与写盘；只读客户端一律按 [`block_reason`] 拒绝。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap / expect / panic / 索引越界。

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::Json;
use axum::routing::{get, post};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::common::{error_response, verify_admin};
use crate::api::openai::AppState;
use crate::tool_services::config_files::{self, ConfigFormat, Target};

/// 客户端接入标识（写进配置里的 provider 名）。
const PROVIDER_ID: &str = "aigx";

/// 预览/应用请求体上限：只含几个字符串，64 KiB 足够。
const MAX_BODY_BYTES: usize = 64 * 1024;

/// 方案库键前缀（单客户端一份数组：`agent:profiles:{clientId}`）。
const PROFILE_KEY_PREFIX: &str = "agent:profiles:";

/// 配置路径覆盖表（`clientId → 路径`）：只影响界面展示，**不参与写盘**。
const PATH_OVERRIDE_KEY: &str = "agent:path-override";

/// 「已写入」记录表（`clientId → [{profileId, path, appliedAt}]`）。
const CONFIG_APPLIED_KEY: &str = "agent:config-applied";

/// 方案库上限（与 A2A `save_profiles` 的 200 套一致）。
const MAX_PROFILES: usize = 200;

/// 单次资源扫描的 Skill 上限：塞满的目录不该把管理面响应撑爆。
const MAX_SKILLS: usize = 1000;

/// 管理面路由。
pub fn router() -> axum::Router<AppState> {
    axum::Router::new()
        .route("/api/agents", get(list_agents))
        .route("/api/agents/models", get(list_models))
        .route("/api/agents/preview", post(preview))
        .route("/api/agents/apply", post(apply))
        .route("/api/agents/restore", post(restore))
        // 后半段：方案库 / 导入检测 / 客户端资源（MCP + Skills）。
        // 静态段（profiles、import）先于 `:id` 注册；axum 0.7 也是静态优先。
        .route("/api/agents/profiles", post(save_profiles))
        .route("/api/agents/import", post(import_client))
        .route("/api/agents/:id/resources", get(list_resources))
        .route("/api/agents/:id/resources/preview", post(preview_resources))
        .route("/api/agents/:id/resources/apply", post(apply_resources))
}

/// 客户端展示/检测元数据（形状对齐 Agent2API `ClientDef`，字段名用前端已有的
/// `A2aClient` 契约：vendor/hue/letter/icon/configMode/protocols/binaries）。
struct ClientMeta {
    target: Target,
    vendor: &'static str,
    hue: u16,
    letter: &'static str,
    icon: &'static str,
    icon_full: bool,
    /// `single` = 一份配置接一个网关；`providers` = provider 表可并存；
    /// `models` = 只往模型清单里加条目；`readonly` = 只检测
    config_mode: &'static str,
    protocols: &'static [&'static str],
    binaries: &'static [&'static str],
}

/// 全部受支持客户端（顺序 = 界面展示顺序）。
const CLIENTS: &[ClientMeta] = &[
    ClientMeta {
        target: Target::ClaudeCode,
        vendor: "Anthropic",
        hue: 28,
        letter: "CC",
        icon: "claude-code.svg",
        icon_full: false,
        config_mode: "single",
        protocols: &["anthropic"],
        binaries: &["claude"],
    },
    ClientMeta {
        target: Target::Codex,
        vendor: "OpenAI",
        hue: 160,
        letter: "CX",
        icon: "codex.png",
        icon_full: true,
        config_mode: "single",
        protocols: &["responses"],
        binaries: &["codex"],
    },
    ClientMeta {
        target: Target::OpenCode,
        vendor: "OpenCode",
        hue: 300,
        letter: "OC",
        icon: "opencode.svg",
        icon_full: true,
        config_mode: "providers",
        protocols: &["openai_chat", "responses", "anthropic"],
        binaries: &["opencode"],
    },
    ClientMeta {
        target: Target::Workbuddy,
        vendor: "Tencent",
        hue: 250,
        letter: "WB",
        icon: "workbuddy.svg",
        icon_full: true,
        config_mode: "models",
        protocols: &["openai_chat"],
        binaries: &["workbuddy", "codebuddy"],
    },
    ClientMeta {
        target: Target::Zcode,
        vendor: "Z.ai",
        hue: 265,
        letter: "ZC",
        icon: "zcode.png",
        icon_full: true,
        config_mode: "providers",
        protocols: &["openai_chat", "responses", "anthropic"],
        binaries: &["zcode"],
    },
    ClientMeta {
        target: Target::KimiCode,
        vendor: "Moonshot",
        hue: 30,
        letter: "KM",
        icon: "kimi-code.png",
        icon_full: true,
        config_mode: "readonly",
        protocols: &["openai_chat", "responses", "anthropic"],
        binaries: &["kimi"],
    },
    ClientMeta {
        target: Target::GeminiCli,
        vendor: "Google",
        hue: 220,
        letter: "GM",
        icon: "gemini-cli.svg",
        icon_full: true,
        config_mode: "readonly",
        protocols: &["gemini"],
        binaries: &["gemini"],
    },
    ClientMeta {
        target: Target::CursorCli,
        vendor: "Anysphere",
        hue: 200,
        letter: "CU",
        icon: "cursor-cli.png",
        icon_full: true,
        config_mode: "readonly",
        protocols: &["openai_chat"],
        binaries: &["cursor-agent", "cursor"],
    },
    ClientMeta {
        target: Target::Dsh,
        vendor: "DeepSeek",
        hue: 210,
        letter: "DS",
        icon: "dsh.svg",
        icon_full: false,
        config_mode: "readonly",
        protocols: &["openai_chat", "anthropic"],
        binaries: &["dsh"],
    },
    ClientMeta {
        target: Target::ClaudeDesktop,
        vendor: "Anthropic",
        hue: 28,
        letter: "CD",
        icon: "claude-desktop.png",
        icon_full: true,
        config_mode: "readonly",
        protocols: &["mcp"],
        binaries: &["claude-desktop"],
    },
];

/// 二进制是否在 PATH 上（Windows 额外试 `.exe` / `.cmd`）。
fn binary_on_path(name: &str) -> bool {
    let path = match std::env::var_os("PATH") {
        Some(path) => path,
        None => return false,
    };
    let exts: &[&str] = if cfg!(windows) {
        &["", ".exe", ".cmd", ".bat"]
    } else {
        &[""]
    };
    for dir in std::env::split_paths(&path) {
        for ext in exts {
            if dir.join(format!("{name}{ext}")).is_file() {
                return true;
            }
        }
    }
    false
}

/// 一家客户端能不能写、为什么（顺序与 [`CLIENTS`] 一致）。
fn writable_of(target: Target) -> Option<&'static str> {
    match target {
        Target::ClaudeCode => Some("env.ANTHROPIC_BASE_URL + env.ANTHROPIC_AUTH_TOKEN"),
        Target::Codex => Some("model_providers.<id> + model_provider"),
        Target::OpenCode | Target::Zcode => Some("provider.<id>（openai-compatible）"),
        Target::Workbuddy => Some("模型清单追加"),
        Target::GeminiCli => None,
        Target::CursorCli => None,
        Target::Dsh => None,
        Target::ClaudeDesktop => None,
        Target::KimiCode => None,
    }
}

/// 不能写入时的原因（界面直接展示，必须说清「缺什么」而不是「不支持」）。
fn block_reason(target: Target) -> &'static str {
    match target {
        Target::GeminiCli => {
            "Gemini CLI 走原生 Gemini 协议，本网关没有对应的原生入口，写进去也连不通"
        }
        Target::KimiCode => {
            "Kimi Code 的 providers 表按新版/旧版两套字段分流，写入器尚未实现（只读检测可用）"
        }
        Target::CursorCli => {
            "Cursor CLI 的模型清单写在应用私有库里，不在配置文件路径上（只读检测可用）"
        }
        Target::Dsh => "DSH 的 provider 段是 YAML 且带环境变量引用，写入器尚未实现（只读检测可用）",
        Target::ClaudeDesktop => {
            "Claude Desktop 只认 MCP 段，不接网关 base_url（用工具服务的 MCP 安装入口）"
        }
        _ => "",
    }
}

/// 网关对外的接入地址（客户端要填的 base URL）。
///
/// 单端口部署时数据面与管理面同端口；`admin_bind` 设置后数据面仍在主端口，
/// 所以这里一律用主端口的 `host:port`。`0.0.0.0` / `::` 对客户端不可用，
/// 换成 `127.0.0.1`（本机客户端访问本机网关是最常见场景）。
fn gateway_base(config: &crate::config::AppConfig) -> String {
    let host = config.server.host.trim();
    let host = if host.is_empty() || host == "0.0.0.0" || host == "::" || host == "[::]" {
        "127.0.0.1"
    } else {
        host
    };
    format!("http://{host}:{}", config.server.port)
}

/// 备份目录：`{data_dir}/agents/backups`。
fn backup_dir(config: &crate::config::AppConfig) -> PathBuf {
    crate::config::expand_path(&config.server.data_dir)
        .join("agents")
        .join("backups")
}

/// 读文件（不存在 = 空串），返回 `(原文, 是否存在)`。
fn read_text(path: &Path) -> (String, bool) {
    match std::fs::read_to_string(path) {
        Ok(text) => (text, true),
        Err(_) => (String::new(), false),
    }
}

/// 原子写：同目录临时文件 + rename（不留下半截文件）。
fn write_atomic(path: &Path, text: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("创建目录失败：{e}"))?;
    }
    let tmp = path.with_extension(format!(
        "{}.tmp{}",
        path.extension().and_then(|ext| ext.to_str()).unwrap_or(""),
        std::process::id()
    ));
    std::fs::write(&tmp, text).map_err(|e| format!("写临时文件失败：{e}"))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("替换目标文件失败：{e}")
    })
}

/// 内容指纹（令牌用）。
fn fingerprint(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part.as_bytes());
        hasher.update([0u8]);
    }
    format!("{:x}", hasher.finalize())
}

/// 递归比较两个 JSON，产出 `[{"path": "/a/b", "before": …, "after": …}]`。
///
/// 比整段 unified diff 更适合界面：用户要看的是「哪几个键被改了」，而不是
/// 几百行上下文。`null` 表示该侧没有这个键（新增/删除）。
fn diff_values(before: &Value, after: &Value, prefix: &str, out: &mut Vec<Value>) {
    match (before, after) {
        (Value::Object(left), Value::Object(right)) => {
            for (key, value) in left {
                let pointer = format!("{prefix}/{}", escape_pointer(key));
                match right.get(key) {
                    Some(other) => diff_values(value, other, &pointer, out),
                    None => out.push(json!({
                        "path": pointer,
                        "before": value,
                        "after": Value::Null,
                    })),
                }
            }
            for (key, value) in right {
                if !left.contains_key(key) {
                    out.push(json!({
                        "path": format!("{prefix}/{}", escape_pointer(key)),
                        "before": Value::Null,
                        "after": value,
                    }));
                }
            }
        }
        (left, right) if left == right => {}
        (left, right) => out.push(json!({
            "path": prefix,
            "before": left,
            "after": right,
        })),
    }
}

/// JSON Pointer 转义（`~` → `~0`、`/` → `~1`）。
fn escape_pointer(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}

/// 具体较深层的对象（缺就建）。
fn object_at<'a>(root: &'a mut Value, path: &[&str]) -> Option<&'a mut Map<String, Value>> {
    let mut current = root;
    for key in path {
        if !current.is_object() {
            *current = Value::Object(Map::new());
        }
        let object = current.as_object_mut()?;
        current = object
            .entry((*key).to_string())
            .or_insert_with(|| Value::Object(Map::new()));
    }
    current.as_object_mut()
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct WireRequest {
    /// 客户端 id（[`Target::parse`] 可识别的值）
    #[serde(default)]
    pub client_id: String,
    /// 网关地址；空 = 用本机默认 `http://127.0.0.1:<port>`
    #[serde(default)]
    pub base_url: String,
    /// 网关密钥（sk-…）。空 = 只写地址不写密钥（部分客户端靠环境变量）
    #[serde(default)]
    pub api_key: String,
    /// 默认模型（可选）
    #[serde(default)]
    pub model: String,
    /// 预览令牌（应用/恢复时必填）
    #[serde(default)]
    pub token: String,
}

/// 解析请求 → 目标客户端。
fn target_of(request: &WireRequest) -> Result<Target, (StatusCode, Json<Value>)> {
    Target::parse(&request.client_id).ok_or_else(|| {
        error_response(
            &format!("未知客户端：{}", request.client_id.trim()),
            StatusCode::BAD_REQUEST,
        )
    })
}

/// 按客户端形状把「接上网关」的改动写进 `desired`。
fn shape_patch(
    target: Target,
    desired: &mut Value,
    base_url: &str,
    api_key: &str,
    model: &str,
) -> Result<(), String> {
    match target {
        Target::ClaudeCode => {
            let env = object_at(desired, &["env"]).ok_or("settings.json 的 env 不是对象")?;
            env.insert("ANTHROPIC_BASE_URL".into(), json!(base_url));
            if !api_key.is_empty() {
                env.insert("ANTHROPIC_AUTH_TOKEN".into(), json!(api_key));
            }
            if !model.is_empty() {
                if let Some(root) = desired.as_object_mut() {
                    root.insert("model".into(), json!(model));
                }
            }
        }
        Target::Codex => {
            let provider = object_at(desired, &["model_providers", PROVIDER_ID])
                .ok_or("config.toml 的 model_providers 不是表")?;
            provider.insert("name".into(), json!("AIGX"));
            provider.insert("base_url".into(), json!(base_url));
            provider.insert("env_key".into(), json!("AIGX_API_KEY"));
            provider.insert("wire_api".into(), json!("responses"));
            if let Some(root) = desired.as_object_mut() {
                root.insert("model_provider".into(), json!(PROVIDER_ID));
                if !model.is_empty() {
                    root.insert("model".into(), json!(model));
                }
            }
        }
        Target::OpenCode | Target::Zcode => {
            let provider =
                object_at(desired, &["provider", PROVIDER_ID]).ok_or("provider 段不是对象")?;
            provider.insert("npm".into(), json!("@ai-sdk/openai-compatible"));
            provider.insert("name".into(), json!("AIGX"));
            let options = provider
                .entry("options".to_string())
                .or_insert_with(|| Value::Object(Map::new()));
            let options = options.as_object_mut().ok_or("provider.options 不是对象")?;
            options.insert("baseURL".into(), json!(base_url));
            if !api_key.is_empty() {
                options.insert("apiKey".into(), json!(api_key));
            }
        }
        Target::Workbuddy => {
            // 模型清单是数组；缺就新建。只追加/更新本网关的条目。
            if !desired.is_array() {
                let root = desired.as_object_mut().ok_or("配置根不是对象")?;
                root.entry("models")
                    .or_insert_with(|| Value::Array(Vec::new()));
            }
            let list = if let Some(items) = desired.as_array_mut() {
                items
            } else {
                desired
                    .as_object_mut()
                    .and_then(|root| root.get_mut("models"))
                    .and_then(Value::as_array_mut)
                    .ok_or("models 不是数组")?
            };
            let entry = json!({ "id": PROVIDER_ID, "name": "AIGX", "baseUrl": base_url });
            match list
                .iter_mut()
                .find(|item| item.get("id").and_then(Value::as_str) == Some(PROVIDER_ID))
            {
                Some(existing) => *existing = entry,
                None => list.push(entry),
            }
        }
        _ => return Err(block_reason(target).to_string()),
    }
    Ok(())
}

/// 一次「接入」的完整计划（预览与写入共用同一份计算）。
struct Plan {
    target: Target,
    path: PathBuf,
    format: ConfigFormat,
    before_text: String,
    existed: bool,
    after_text: String,
    changes: Vec<Value>,
}

/// 计算计划：读现状 → 打补丁 → 渲染（渲染内部会重解析比对）。
fn build_plan(request: &WireRequest, config: &crate::config::AppConfig) -> Result<Plan, String> {
    let target = Target::parse(&request.client_id).ok_or_else(|| "未知客户端".to_string())?;
    if writable_of(target).is_none() {
        return Err(block_reason(target).to_string());
    }
    let path = target.config_path();
    let format = target.format();
    let (before_text, existed) = read_text(&path);
    // 只支持这三层格式的客户端才 writable（见模块头）。
    let mut desired = config_files::parse(format, &before_text)?;
    let base = if request.base_url.trim().is_empty() {
        gateway_base(config)
    } else {
        request.base_url.trim().to_string()
    };
    shape_patch(
        target,
        &mut desired,
        &base,
        request.api_key.trim(),
        request.model.trim(),
    )?;
    let after_text = config_files::render(format, &before_text, &desired)?;
    let before_value = config_files::parse(format, &before_text).unwrap_or(Value::Null);
    let mut changes = Vec::new();
    diff_values(&before_value, &desired, "", &mut changes);
    Ok(Plan {
        target,
        path,
        format,
        before_text,
        existed,
        after_text,
        changes,
    })
}

/// 预览令牌：绑定「客户端 + 路径 + 预览时磁盘内容 + 预览结果」。
///
/// 应用时用**当时的磁盘内容**重算，与令牌不符即说明文件在预览后被改过 → 409。
fn plan_token(plan: &Plan) -> String {
    let format = match plan.format {
        ConfigFormat::Json => "json",
        ConfigFormat::Toml => "toml",
        ConfigFormat::Yaml => "yaml",
    };
    let before_hash = fingerprint(&[plan.before_text.as_str()]);
    let after_hash = fingerprint(&[plan.after_text.as_str()]);
    let path = plan.path.to_string_lossy();
    fingerprint(&[
        plan.target.id(),
        path.as_ref(),
        format,
        before_hash.as_str(),
        after_hash.as_str(),
    ])
}

/// 令牌校验（CAS）：令牌必须对得上**此刻磁盘内容**算出的计划，否则 409。
///
/// 配置应用与资源应用共用同一条判断，避免两处提示/状态码漂移。
fn ensure_token(plan: &Plan, token: &str) -> Result<(), (StatusCode, Json<Value>)> {
    if plan_token(plan) == token.trim() {
        Ok(())
    } else {
        Err(error_response(
            "配置在预览之后被改动过，请重新预览再应用",
            StatusCode::CONFLICT,
        ))
    }
}

/// 备份一份现状（应用前调用）。返回备份文件路径。
fn backup_plan(plan: &Plan, config: &crate::config::AppConfig) -> Result<Option<PathBuf>, String> {
    if !plan.existed {
        return Ok(None);
    }
    let dir = backup_dir(config);
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建备份目录失败：{e}"))?;
    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S-%3f");
    let path = dir.join(format!("{}.{}.bak", plan.target.id(), stamp));
    std::fs::write(&path, &plan.before_text).map_err(|e| format!("写备份失败：{e}"))?;
    Ok(Some(path))
}

/// 该客户端现有的备份（新的在前）。
fn list_backups(target: Target, config: &crate::config::AppConfig) -> Vec<PathBuf> {
    let dir = backup_dir(config);
    let prefix = format!("{}.", target.id());
    let mut items: Vec<PathBuf> = match std::fs::read_dir(&dir) {
        Ok(entries) => entries
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .map(|name| name.starts_with(&prefix) && name.ends_with(".bak"))
                    .unwrap_or(false)
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    items.sort();
    items.reverse();
    items
}

/// 从现有配置里读出「是否已经接在本网关上」（只读判断，不猜测）。
fn wired_state(target: Target, value: &Value) -> Value {
    let hit = |pointer: &str| value.pointer(pointer).and_then(Value::as_str);
    let base = match target {
        Target::ClaudeCode => hit("/env/ANTHROPIC_BASE_URL"),
        Target::Codex => hit("/model_providers/aigx/base_url"),
        Target::OpenCode | Target::Zcode => hit("/provider/aigx/options/baseURL"),
        // WorkBuddy 的模型清单可能是根数组，也可能挂在 `models` 下。
        Target::Workbuddy => value
            .as_array()
            .into_iter()
            .flatten()
            .chain(
                value
                    .get("models")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten(),
            )
            .find(|item| item.get("id").and_then(Value::as_str) == Some(PROVIDER_ID))
            .and_then(|item| item.get("baseUrl"))
            .and_then(Value::as_str),
        _ => None,
    };
    json!({
        "connected": base.is_some(),
        "baseUrl": base,
    })
}

/// 通用 KV 句柄：借用账号会话存储持有的同一个 `FileStore`。
///
/// 理由见模块头「方案库落点」：`AppState` 上没有通用的裸 FileStore 访问器，
/// 账号会话的 store 是按键前缀分域的那一份，`agent:*` 键与账号/签到互不覆盖。
fn kv_store(state: &AppState) -> std::sync::Arc<crate::storage::FileStore> {
    state.account_sessions.store().clone()
}

/// 方案库键（`agent:profiles:{clientId}`）。
fn profile_key(client_id: &str) -> String {
    format!("{PROFILE_KEY_PREFIX}{client_id}")
}

/// 读全部方案库（快照用）：单个客户端读失败不影响其它客户端。
fn load_all_profiles(store: &crate::storage::FileStore) -> Map<String, Value> {
    let mut out = Map::new();
    let Ok(keys) = store.list(PROFILE_KEY_PREFIX) else {
        return out;
    };
    for key in keys {
        let Some(client_id) = key.strip_prefix(PROFILE_KEY_PREFIX) else {
            continue;
        };
        if let Ok(Some(items)) = store.get::<Vec<Value>>(&key) {
            out.insert(client_id.to_string(), Value::Array(items));
        }
    }
    out
}

/// 读路径覆盖表（`clientId → 用户填的展示路径`）。
fn load_path_overrides(store: &crate::storage::FileStore) -> Map<String, Value> {
    store
        .get::<Value>(PATH_OVERRIDE_KEY)
        .ok()
        .flatten()
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default()
}

/// 读「已写入」记录表（`clientId → 记录数组`）。
fn load_applied(store: &crate::storage::FileStore) -> Map<String, Value> {
    store
        .get::<Value>(CONFIG_APPLIED_KEY)
        .ok()
        .flatten()
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default()
}

/// 按客户端找到元数据（[`CLIENTS`] 覆盖 [`Target::ALL`]，正常不会落空）。
fn meta_of(target: Target) -> Option<&'static ClientMeta> {
    CLIENTS.iter().find(|meta| meta.target == target)
}

/// 单个客户端的检测快照（`GET /api/agents` 与 `POST /api/agents/import` 共用）。
///
/// `configPath` / `resolvedPath` 永远来自 [`Target::config_path()`]；
/// `pathOverride` 只是用户填的展示值，**不参与任何写盘路径解析**。
fn client_snapshot(
    meta: &ClientMeta,
    config: &crate::config::AppConfig,
    overrides: &Map<String, Value>,
    profiles: &Map<String, Value>,
    applied: &Map<String, Value>,
) -> Value {
    let target = meta.target;
    let path = target.config_path();
    let (text, existed) = read_text(&path);
    let parsed = config_files::parse(target.format(), &text);
    let stat = std::fs::metadata(&path).ok();
    let parse_ok = parsed.is_ok();
    // 先取错误字符串再消费解析结果：`err(self)` 会 move，之后再 unwrap 编译不过。
    let parse_error = parsed.as_ref().err().cloned();
    let parsed_value = parsed.unwrap_or(Value::Null);
    let installed = meta.binaries.iter().any(|name| binary_on_path(name));
    // 检测状态：读不通 > 有配置文件 > 二进制在但没配置 > 都没发现。
    let detection = if !parse_ok {
        "read_error"
    } else if existed {
        "ready"
    } else if installed {
        "uninitialized"
    } else if writable_of(target).is_some() {
        "missing"
    } else {
        "unsupported"
    };
    let write_shape = writable_of(target);
    let path_override = overrides
        .get(target.id())
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let saved = profiles
        .get(target.id())
        .cloned()
        .unwrap_or_else(|| json!([]));
    let profile_count = saved.as_array().map(Vec::len).unwrap_or(0);
    json!({
        "id": target.id(),
        "name": target.label(),
        "vendor": meta.vendor,
        "hue": meta.hue,
        "letter": meta.letter,
        "icon": meta.icon,
        "iconFull": meta.icon_full,
        "format": match target.format() {
            ConfigFormat::Json => "json",
            ConfigFormat::Toml => "toml",
            ConfigFormat::Yaml => "yaml",
        },
        "configMode": meta.config_mode,
        "protocols": meta.protocols,
        "binaries": meta.binaries,
        "installed": installed,
        "detected": existed,
        "detectionState": detection,
        "configPath": path.to_string_lossy(),
        "resolvedPath": path.to_string_lossy(),
        "pathOverride": path_override,
        "exists": existed,
        "size": stat.as_ref().map(|m| m.len()).unwrap_or(0),
        "modifiedAt": stat
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64),
        "parsed": parse_ok,
        "parseError": parse_error,
        "writable": write_shape.is_some(),
        "writeShape": write_shape,
        "blockReason": if write_shape.is_some() { Value::Null } else { json!(block_reason(target)) },
        "wired": if parse_ok {
            wired_state(target, &parsed_value)
        } else {
            json!({ "connected": false, "baseUrl": Value::Null })
        },
        "backups": list_backups(target, config)
            .into_iter()
            .take(5)
            .map(|p| p.to_string_lossy().to_string())
            .collect::<Vec<_>>(),
        // 方案库与「已写入」记录：界面刷新后不必再猜，直接跟着快照回来。
        "profiles": saved,
        "profileCount": profile_count,
        "applied": applied.get(target.id()).cloned().unwrap_or_else(|| json!([])),
    })
}

/// GET /api/agents —— 全部客户端的检测快照。
pub async fn list_agents(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = verify_admin(&state, &headers).await?;
    let store = kv_store(&state);
    let overrides = load_path_overrides(&store);
    let profiles = load_all_profiles(&store);
    let applied = load_applied(&store);
    let gateway = gateway_base(&config);
    let clients: Vec<Value> = CLIENTS
        .iter()
        .map(|meta| client_snapshot(meta, &config, &overrides, &profiles, &applied))
        .collect();
    Ok(Json(json!({
        "success": true,
        "data": {
            "gateway": { "baseUrl": gateway, "openaiBaseUrl": format!("{gateway}/v1"), "anthropicBaseUrl": format!("{gateway}/v1") },
            "clients": clients,
        }
    })))
}

/// GET /api/agents/models —— 可供客户端选择的模型（取自启用渠道）。
pub async fn list_models(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let mut models: Vec<String> = state
        .channel_store
        .list()
        .into_iter()
        .filter(|channel| channel.is_enabled())
        .flat_map(|channel| channel.models.clone())
        .collect();
    models.sort();
    models.dedup();
    Ok(Json(
        json!({ "success": true, "data": { "models": models } }),
    ))
}

/// POST /api/agents/preview —— 算改动、返回差异与令牌（**不写盘**）。
pub async fn preview(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = verify_admin(&state, &headers).await?;
    let request = parse_request(&body)?;
    target_of(&request)?;
    let plan = build_plan(&request, &config)
        .map_err(|message| error_response(&message, StatusCode::BAD_REQUEST))?;
    if plan.changes.is_empty() {
        return Ok(Json(json!({
            "success": true,
            "data": { "noop": true, "path": plan.path.to_string_lossy(), "changes": [] }
        })));
    }
    Ok(Json(json!({
        "success": true,
        "data": {
            "token": plan_token(&plan),
            "clientId": plan.target.id(),
            "path": plan.path.to_string_lossy(),
            "exists": plan.existed,
            "changes": plan.changes,
            "after": plan.after_text,
        }
    })))
}

/// POST /api/agents/apply —— 校验令牌 → 备份 → 原子写 → 回读确认。
pub async fn apply(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = verify_admin(&state, &headers).await?;
    let request = parse_request(&body)?;
    target_of(&request)?;
    let plan = build_plan(&request, &config)
        .map_err(|message| error_response(&message, StatusCode::BAD_REQUEST))?;
    ensure_token(&plan, &request.token)?;
    let backup = backup_plan(&plan, &config)
        .map_err(|message| error_response(&message, StatusCode::INTERNAL_SERVER_ERROR))?;
    write_atomic(&plan.path, &plan.after_text)
        .map_err(|message| error_response(&message, StatusCode::INTERNAL_SERVER_ERROR))?;
    // 回读确认：写进去的内容必须能被重新解析且与计划一致（防编码/换行问题）。
    let (written, _) = read_text(&plan.path);
    let reparsed = config_files::parse(plan.format, &written);
    let verified = match reparsed {
        Ok(value) => {
            let mut changes = Vec::new();
            diff_values(
                &value,
                &config_files::parse(plan.format, &plan.after_text).unwrap_or(Value::Null),
                "",
                &mut changes,
            );
            changes.is_empty()
        }
        Err(_) => false,
    };
    Ok(Json(json!({
        "success": true,
        "data": {
            "applied": true,
            "verified": verified,
            "clientId": plan.target.id(),
            "path": plan.path.to_string_lossy(),
            "backup": backup.map(|p| p.to_string_lossy().to_string()),
            "needsRestart": true,
        }
    })))
}

/// POST /api/agents/restore —— 从备份回滚（默认最新一份）。
pub async fn restore(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = verify_admin(&state, &headers).await?;
    let request = parse_request(&body)?;
    let target = target_of(&request)?;
    let backups = list_backups(target, &config);
    let chosen = if request.token.trim().is_empty() {
        backups.into_iter().next()
    } else {
        // token 字段在恢复时复用为「备份文件名」：界面从快照里选一份。
        let wanted = request.token.trim();
        backups
            .into_iter()
            .find(|path| path.file_name().and_then(|n| n.to_str()) == Some(wanted))
    };
    let backup =
        chosen.ok_or_else(|| error_response("没有找到该客户端的备份", StatusCode::NOT_FOUND))?;
    let (text, _) = read_text(&backup);
    let path = target.config_path();
    write_atomic(&path, &text)
        .map_err(|message| error_response(&message, StatusCode::INTERNAL_SERVER_ERROR))?;
    Ok(Json(json!({
        "success": true,
        "data": {
            "restored": true,
            "clientId": target.id(),
            "path": path.to_string_lossy(),
            "from": backup.to_string_lossy(),
            "needsRestart": true,
        }
    })))
}

/* ─── 方案库（profiles）────────────────────────────────────── */

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct ProfilesRequest {
    #[serde(default)]
    client_id: String,
    /// `path` / `config-save` / `config-remove`；缺省 = 保存方案数组
    #[serde(default)]
    action: String,
    /// 方案数组（`id` 必填且唯一）
    #[serde(default)]
    profiles: Vec<Value>,
    /// `config-save` / `config-remove` 选中的方案 id
    #[serde(default)]
    profile_id: String,
    /// `path` 动作的展示路径（仅写覆盖表，不参与写盘）
    #[serde(default)]
    config_path: String,
}

/// 校验并规范化方案数组：`id` 必填、去空白、不重复，最多 [`MAX_PROFILES`] 套。
///
/// 只拒绝「明显坏的数据」：方案内容是给客户端看的草稿，这里不解释字段含义；
/// 但 `id` 是主键（导入去重、已写入记录都按它索引），空/重复一律拒。
fn normalize_profiles(items: &[Value]) -> Result<Vec<Value>, String> {
    if items.len() > MAX_PROFILES {
        return Err(format!("最多保存 {MAX_PROFILES} 套方案"));
    }
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        let object = item
            .as_object()
            .ok_or_else(|| format!("第 {} 套方案不是对象", index + 1))?;
        let id = object
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        if id.is_empty() {
            return Err(format!("第 {} 套方案的 ID 不能为空", index + 1));
        }
        if !seen.insert(id.to_string()) {
            return Err(format!("方案 ID 重复：{id}"));
        }
        let mut object = object.clone();
        object.insert("id".into(), json!(id));
        out.push(Value::Object(object));
    }
    Ok(out)
}

/// POST /api/agents/profiles —— 方案库 / 路径覆盖 / 已写入记录。
///
/// 三种东西都只落 AIGX 自己的 KV（见模块头），**不写客户端配置文件**：
/// - 缺省 action：保存方案数组（≤200 套，`id` 非空且不重复）；
/// - `path`：保存配置路径覆盖（仅展示；写入目标仍由 `Target` 的路径表决定）；
/// - `config-save` / `config-remove`：登记/撤销「这套方案已写进客户端」。
pub async fn save_profiles(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let request: ProfilesRequest = parse_payload(&body)?;
    let target = Target::parse(&request.client_id).ok_or_else(|| {
        error_response(
            &format!("未知客户端：{}", request.client_id.trim()),
            StatusCode::BAD_REQUEST,
        )
    })?;
    let store = kv_store(&state);
    match request.action.trim() {
        // 路径覆盖：只写覆盖表。真正写盘的目标永远来自 `Target` 的路径表，
        // 请求里的路径不参与（A2A `paths::save_override` 同口径）。
        "path" => {
            let path = request.config_path.trim();
            if !path.is_empty() && !Path::new(path).is_absolute() {
                return Err(error_response(
                    "配置路径必须是不带换行的绝对路径（仅作展示覆盖）",
                    StatusCode::BAD_REQUEST,
                ));
            }
            let mut map = load_path_overrides(&store);
            if path.is_empty() {
                map.remove(target.id());
            } else {
                map.insert(target.id().into(), json!(path));
            }
            store
                .put(PATH_OVERRIDE_KEY, &Value::Object(map))
                .map_err(|error| {
                    error_response(
                        &format!("保存路径覆盖失败：{error}"),
                        StatusCode::INTERNAL_SERVER_ERROR,
                    )
                })?;
            Ok(Json(json!({
                "success": true,
                "data": {
                    "clientId": target.id(),
                    "pathOverride": path,
                    "stored": !path.is_empty(),
                }
            })))
        }
        "config-save" | "config-remove" => {
            let profile_id = request.profile_id.trim();
            if profile_id.is_empty() {
                return Err(error_response("缺少 profileId", StatusCode::BAD_REQUEST));
            }
            let saving = request.action.trim() == "config-save";
            let mut all = load_applied(&store);
            let mut records: Vec<Value> = all
                .get(target.id())
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            records
                .retain(|item| item.get("profileId").and_then(Value::as_str) != Some(profile_id));
            if saving {
                records.push(json!({
                    "profileId": profile_id,
                    "path": target.config_path().to_string_lossy(),
                    "appliedAt": chrono::Utc::now().timestamp_millis(),
                }));
            }
            all.insert(target.id().into(), Value::Array(records.clone()));
            store
                .put(CONFIG_APPLIED_KEY, &Value::Object(all))
                .map_err(|error| {
                    error_response(
                        &format!("保存写入记录失败：{error}"),
                        StatusCode::INTERNAL_SERVER_ERROR,
                    )
                })?;
            Ok(Json(json!({
                "success": true,
                "data": {
                    "clientId": target.id(),
                    "profileId": profile_id,
                    "saved": saving,
                    "applied": records,
                }
            })))
        }
        _ => {
            let profiles = normalize_profiles(&request.profiles)
                .map_err(|message| error_response(&message, StatusCode::BAD_REQUEST))?;
            store
                .put(&profile_key(target.id()), &profiles)
                .map_err(|error| {
                    error_response(
                        &format!("保存方案失败：{error}"),
                        StatusCode::INTERNAL_SERVER_ERROR,
                    )
                })?;
            Ok(Json(json!({
                "success": true,
                "data": {
                    "clientId": target.id(),
                    "count": profiles.len(),
                    "profiles": profiles,
                }
            })))
        }
    }
}

/* ─── 导入检测（import）────────────────────────────────────── */

/// 反向解析用的候选键位（按客户端已核实的键，命中第一个非空值）。
fn base_candidates(target: Target) -> &'static [&'static str] {
    match target {
        Target::ClaudeCode => &["/env/ANTHROPIC_BASE_URL", "/env/ANTHROPIC_API_URL"],
        Target::Codex => &["/model_providers/aigx/base_url", "/base_url"],
        Target::OpenCode | Target::Zcode => &[
            "/provider/aigx/options/baseURL",
            "/provider/aigx/options/baseUrl",
        ],
        Target::Workbuddy => &["/models/0/baseUrl", "/0/baseUrl"],
        Target::KimiCode => &["/provider/aigx/base_url", "/base_url"],
        Target::Dsh => &["/providers/aigx/base_url", "/base_url"],
        Target::GeminiCli | Target::CursorCli => &["/baseUrl", "/api/baseUrl"],
        Target::ClaudeDesktop => &[],
    }
}

/// 浅层兜底扫描：找键名匹配的字符串（深度受限，不整树遍历）。
fn find_first_string(value: &Value, keys: &[&str], depth: usize) -> Option<String> {
    if depth == 0 {
        return None;
    }
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                let lower = key.to_ascii_lowercase();
                if keys.iter().any(|want| lower == *want) {
                    if let Some(text) = child.as_str() {
                        let text = text.trim();
                        if !text.is_empty() {
                            return Some(text.to_string());
                        }
                    }
                }
            }
            map.values()
                .find_map(|child| find_first_string(child, keys, depth - 1))
        }
        Value::Array(items) => items
            .iter()
            .find_map(|item| find_first_string(item, keys, depth - 1)),
        _ => None,
    }
}

/// 反向解析：从现有配置读出「接在哪个网关、选了哪个模型、用的哪个 provider」。
///
/// 先按各客户端**已验证**的键位取；取不到再做一次浅层兜底（只认
/// `base_url` / `baseurl` / `api_base` / `model` 这类键名）。这里只做检测，
/// 不推断写入形状；命中不了就老实返回 `matchesGateway=false`。
fn detected_upstream(target: Target, gateway: &str, value: &Value, existed: bool) -> Value {
    let pointer = |candidates: &[&str]| -> Option<String> {
        candidates
            .iter()
            .find_map(|path| value.pointer(path).and_then(Value::as_str))
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string)
    };
    let base = pointer(base_candidates(target))
        .or_else(|| find_first_string(value, &["base_url", "baseurl", "api_base"], 3));
    let model = pointer(&["/model"])
        .or_else(|| pointer(&["/models/0/id", "/models/0/name"]))
        .or_else(|| find_first_string(value, &["model"], 3));
    let provider = pointer(&["/model_provider"]).or_else(|| {
        value
            .get("provider")
            .and_then(Value::as_object)
            .and_then(|map| map.keys().next().cloned())
    });
    let matches_gateway = base
        .as_deref()
        .map(|text| text.trim_end_matches('/') == gateway.trim_end_matches('/'))
        .unwrap_or(false);
    json!({
        "connected": base.is_some(),
        "baseUrl": base,
        "model": model,
        "provider": provider,
        "gateway": gateway,
        "matchesGateway": matches_gateway,
        "detected": existed,
        "note": if existed {
            Value::Null
        } else {
            json!("客户端配置文件不存在或不可解析，导入只返回检测结果")
        },
    })
}

/// POST /api/agents/import —— 读当前客户端配置，反向解析它接的是哪个网关/模型。
///
/// 只读：不写盘、不落方案库。响应 `data` 与 `GET /api/agents` 的
/// `clients[i]` 同形，额外带 `imported` 字段（反向解析结果）。
pub async fn import_client(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = verify_admin(&state, &headers).await?;
    let request = parse_request(&body)?;
    let target = target_of(&request)?;
    let meta = meta_of(target)
        .ok_or_else(|| error_response("客户端元数据缺失", StatusCode::INTERNAL_SERVER_ERROR))?;
    let store = kv_store(&state);
    let overrides = load_path_overrides(&store);
    let profiles = load_all_profiles(&store);
    let applied = load_applied(&store);
    let mut entry = client_snapshot(meta, &config, &overrides, &profiles, &applied);
    let path = target.config_path();
    let (text, existed) = read_text(&path);
    let parsed = config_files::parse(target.format(), &text).unwrap_or(Value::Null);
    let detected = detected_upstream(target, &gateway_base(&config), &parsed, existed);
    if let Some(object) = entry.as_object_mut() {
        object.insert("imported".into(), detected);
    }
    Ok(Json(json!({ "success": true, "data": entry })))
}

/* ─── 客户端资源（MCP / Skills）────────────────────────────── */

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct ResourceRequest {
    /// 工具服务 id（`crate::tool_services::SERVICE_IDS`）
    #[serde(default)]
    service: String,
    /// 网关地址；空 = 本机默认
    #[serde(default)]
    base_url: String,
    /// 网关密钥；空 = 只写地址，不写 Authorization
    #[serde(default)]
    api_key: String,
    /// MCP 条目名；空 = 服务的包名
    #[serde(default)]
    name: String,
    /// 预览令牌（应用时必填）
    #[serde(default)]
    token: String,
}

/// 一条 MCP 条目（名称/路径/端点/启用状态/来源=配置文件）。
fn server_row(name: &str, entry: &Value, path: &str) -> Value {
    let endpoint = entry
        .get("url")
        .or_else(|| entry.get("httpUrl"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| match entry.get("command") {
            Some(Value::String(text)) => text.clone(),
            Some(Value::Array(items)) => items
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" "),
            _ => String::new(),
        });
    // 各家停用标记不同：`disabled: true` 或 `enabled: false` 都算停用。
    let enabled = !matches!(entry.get("disabled").and_then(Value::as_bool), Some(true))
        && !matches!(entry.get("enabled").and_then(Value::as_bool), Some(false));
    json!({
        "name": name,
        "path": path,
        "endpoint": endpoint,
        "enabled": enabled,
        "source": "file",
        "config": entry,
    })
}

/// 取（必要时创建）MCP 容器；已存在但不是对象时拒绝覆盖（不把用户配置改坏）。
fn ensure_container<'a>(
    value: &'a mut Value,
    path: &[&str],
) -> Result<&'a mut Map<String, Value>, String> {
    if path.is_empty() {
        return value
            .as_object_mut()
            .ok_or_else(|| "MCP 配置容器不是对象".to_string());
    }
    let map = value
        .as_object_mut()
        .ok_or_else(|| "MCP 配置根不是对象".to_string())?;
    let entry = map.entry(path[0].to_string()).or_insert_with(|| json!({}));
    if !entry.is_object() {
        return Err(format!("MCP 配置里的 {} 不是对象，拒绝覆盖", path[0]));
    }
    ensure_container(entry, &path[1..])
}

/// MCP 条目快照：真实读一次客户端 MCP 配置文件（只读）。
fn mcp_snapshot(target: Target) -> Value {
    let Some(path) = target.mcp_path() else {
        return json!({
            "supported": false,
            "path": Value::Null,
            "exists": false,
            "servers": [],
            "error": "该客户端暂不支持 MCP 配置（JSONC 无损渲染未覆盖）",
        });
    };
    let (text, existed) = read_text(&path);
    let path_text = path.to_string_lossy().to_string();
    let value = match config_files::parse(target.format(), &text) {
        Ok(value) => value,
        Err(error) => {
            return json!({
                "supported": true,
                "path": path_text,
                "exists": existed,
                "servers": [],
                "error": error,
            });
        }
    };
    let mut node = &value;
    for key in target.mcp_container() {
        match node.get(*key) {
            Some(child) => node = child,
            None => {
                return json!({
                    "supported": true,
                    "path": path_text,
                    "exists": existed,
                    "container": target.mcp_container(),
                    "servers": [],
                });
            }
        }
    }
    let servers = match node.as_object() {
        Some(map) => map
            .iter()
            .map(|(name, entry)| server_row(name, entry, &path_text))
            .collect::<Vec<_>>(),
        None => {
            return json!({
                "supported": true,
                "path": path_text,
                "exists": existed,
                "container": target.mcp_container(),
                "servers": [],
                "error": "MCP 配置容器不是对象，拒绝按条目解析",
            });
        }
    };
    json!({
        "supported": true,
        "path": path_text,
        "exists": existed,
        "container": target.mcp_container(),
        "servers": servers,
    })
}

/// 读 YAML frontmatter 的单个字段（只认开头 `---` 包住的首块，够 SKILL.md 用）。
fn frontmatter(content: &str, field: &str) -> Option<String> {
    let mut lines = content.lines();
    if lines.next()?.trim() != "---" {
        return None;
    }
    let prefix = format!("{field}:");
    for line in lines {
        if line.trim() == "---" {
            break;
        }
        if let Some(value) = line.strip_prefix(prefix.as_str()) {
            let value = value.trim().trim_matches(['\'', '"']);
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

/// frontmatter 里的布尔开关（`true/yes/1/on`、`false/no/0/off`）。
fn frontmatter_flag(content: &str, field: &str) -> Option<bool> {
    let value = frontmatter(content, field)?.to_ascii_lowercase();
    match value.as_str() {
        "true" | "yes" | "1" | "on" => Some(true),
        "false" | "no" | "0" | "off" => Some(false),
        _ => None,
    }
}

/// 目录里有没有「我们装的」标记：安装器写的 `skill.json`（`name` 非空）。
fn marker_is_ours(path: &Path) -> bool {
    let (text, existed) = read_text(&path.join("skill.json"));
    if !existed {
        return false;
    }
    serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|value| {
            value
                .get("name")
                .and_then(Value::as_str)
                .map(|name| !name.trim().is_empty())
        })
        .unwrap_or(false)
}

/// 扫描一个 Skill 根目录：目录形态（`<name>/SKILL.md`）与单文件形态
/// （`<name>.md`）都收进 `out`；跳过隐藏项、跳过没有文档的目录。
fn skill_items_in(root: &Path, root_index: usize, out: &mut Vec<Value>) {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        // 根目录不存在 = 客户端还没装过 Skill，不是错误。
        Err(_) => return,
    };
    for entry in entries.flatten() {
        if out.len() >= MAX_SKILLS {
            return;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        let is_dir = path.is_dir();
        let doc = if is_dir {
            path.join("SKILL.md")
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("md") {
            path.clone()
        } else {
            continue;
        };
        if !doc.is_file() {
            continue;
        }
        let linked = std::fs::symlink_metadata(&path)
            .map(|meta| meta.file_type().is_symlink())
            .unwrap_or(false);
        let (content, _) = read_text(&doc);
        let label = frontmatter(&content, "name")
            .unwrap_or_else(|| name.trim_end_matches(".md").to_string());
        let description = frontmatter(&content, "description");
        // 启用状态只认 frontmatter 显式开关；没写 = 启用（客户端默认行为）。
        let enabled = frontmatter_flag(&content, "enabled")
            .or_else(|| frontmatter_flag(&content, "disabled").map(|off| !off))
            .unwrap_or(true);
        out.push(json!({
            "id": format!("{root_index}:{name}"),
            "name": label,
            "description": description,
            "path": doc.to_string_lossy(),
            "root": root.to_string_lossy(),
            "source": if is_dir { "directory" } else { "file" },
            "enabled": enabled,
            "linked": linked,
            "managed": is_dir && marker_is_ours(&path),
        }));
    }
}

/// Skill 快照：全部根目录的真实扫描结果（只读）。
fn skill_snapshot(target: Target) -> Value {
    let roots = target.skill_roots();
    let mut items = Vec::new();
    for (index, root) in roots.iter().enumerate() {
        skill_items_in(root, index, &mut items);
    }
    items.sort_by(|left, right| {
        left.get("name")
            .and_then(Value::as_str)
            .cmp(&right.get("name").and_then(Value::as_str))
    });
    json!({
        "roots": roots
            .iter()
            .map(|root| root.to_string_lossy().to_string())
            .collect::<Vec<_>>(),
        "items": items,
        "note": "目录形态要求 `<name>/SKILL.md`；单文件形态只认 `*.md`；隐藏项不列出。",
    })
}

/// GET /api/agents/{id}/resources —— 真实文件扫描出的 MCP 条目与 Skill 目录。
///
/// 只读客户端同样可读（读取不写文件）；能不能写由 `writable` / `blockReason`
/// 给出，写入入口会按同一原因拒绝。
pub async fn list_resources(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let target = Target::parse(&id)
        .ok_or_else(|| error_response(&format!("未知客户端：{id}"), StatusCode::BAD_REQUEST))?;
    let write_shape = writable_of(target);
    Ok(Json(json!({
        "success": true,
        "data": {
            "clientId": target.id(),
            "name": target.label(),
            "writable": write_shape.is_some(),
            "blockReason": if write_shape.is_some() { Value::Null } else { json!(block_reason(target)) },
            "mcp": mcp_snapshot(target),
            "skills": skill_snapshot(target),
            "note": "扫描的是网关所在机器上的真实文件；写入需先预览拿令牌，目标路径只来自客户端路径表。",
        }
    })))
}

/// MCP 条目的客户端形状（与工具服务安装同口径：Codex 用 `http_headers`，
/// Gemini CLI 用 `httpUrl`，其余用 `type/url/headers`）。
fn mcp_client_entry(target: Target, url: &str, api_key: &str) -> Value {
    let mut entry = match target {
        Target::Codex => json!({ "url": url }),
        Target::GeminiCli => json!({ "httpUrl": url }),
        _ => json!({ "type": "http", "url": url }),
    };
    if !api_key.is_empty() {
        let header = if matches!(target, Target::Codex) {
            "http_headers"
        } else {
            "headers"
        };
        if let Some(object) = entry.as_object_mut() {
            object.insert(
                header.into(),
                json!({ "Authorization": format!("Bearer {api_key}") }),
            );
        }
    }
    entry
}

/// 「把网关工具服务写进该客户端」的计划：读现状 → 补 MCP 条目 → 渲染。
///
/// 与 [`build_plan`] 走同一条「diff + 令牌」通道，所以资源写入的 CAS 语义与
/// 配置应用完全一致，差异只在补丁内容（`<container>.<name>` 这一项）。
/// 目标路径只来自 [`Target::mcp_path()`]：请求里没有、也不接受路径参数。
fn build_resource_plan(
    target: Target,
    request: &ResourceRequest,
    config: &crate::config::AppConfig,
) -> Result<Plan, String> {
    if writable_of(target).is_none() {
        return Err(block_reason(target).to_string());
    }
    let path = target
        .mcp_path()
        .ok_or_else(|| "该客户端暂不支持 MCP 配置写入（JSONC 无损渲染未覆盖）".to_string())?;
    let service = request.service.trim();
    if service.is_empty() {
        return Err("缺少工具服务 id（service）".to_string());
    }
    if !crate::tool_services::is_service(service) {
        return Err(format!("未知工具服务：{service}"));
    }
    let endpoint = crate::tool_services::endpoint(service, "mcp");
    if endpoint.is_empty() {
        return Err(format!("工具服务 {service} 没有 MCP 端点"));
    }
    let name = if request.name.trim().is_empty() {
        crate::tool_services::package_name(service).to_string()
    } else {
        request.name.trim().to_string()
    };
    // 名称会变成配置键：拒绝路径分隔符/控制字符（与 A2A `resources::mcp` 同口径）。
    if name.is_empty()
        || name.len() > 128
        || name.contains(['/', '\\'])
        || name.chars().any(char::is_control)
    {
        return Err("MCP 条目名称无效".to_string());
    }
    let base = if request.base_url.trim().is_empty() {
        gateway_base(config)
    } else {
        request.base_url.trim().to_string()
    };
    let url = format!("{}{endpoint}", base.trim_end_matches('/'));
    let format = target.format();
    let (before_text, existed) = read_text(&path);
    let mut desired = config_files::parse(format, &before_text)?;
    let entry = mcp_client_entry(target, &url, request.api_key.trim());
    // 容器可能是 `mcp.servers` 这种多级键：逐级建对象再插条目；
    // 某级已存在但不是对象时直接拒绝，绝不用空对象盖掉用户内容。
    let servers = ensure_container(&mut desired, target.mcp_container())?;
    servers.insert(name, entry);
    let after_text = config_files::render(format, &before_text, &desired)?;
    let before_value = config_files::parse(format, &before_text).unwrap_or(Value::Null);
    let mut changes = Vec::new();
    diff_values(&before_value, &desired, "", &mut changes);
    Ok(Plan {
        target,
        path,
        format,
        before_text,
        existed,
        after_text,
        changes,
    })
}

/// 资源写入前的备份：落在 `agents/backups/resources`，与整份配置的备份分开，
/// 免得 `restore` 把 MCP 备份当成整份配置回滚。
fn backup_resource(
    plan: &Plan,
    config: &crate::config::AppConfig,
) -> Result<Option<PathBuf>, String> {
    if !plan.existed {
        return Ok(None);
    }
    let dir = backup_dir(config).join("resources");
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建备份目录失败：{e}"))?;
    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S-%3f");
    let path = dir.join(format!("{}.mcp.{}.bak", plan.target.id(), stamp));
    std::fs::write(&path, &plan.before_text).map_err(|e| format!("写备份失败：{e}"))?;
    Ok(Some(path))
}

/// POST /api/agents/{id}/resources/preview —— MCP 变更差异 + 令牌（不写盘）。
pub async fn preview_resources(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
    body: Bytes,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = verify_admin(&state, &headers).await?;
    let target = Target::parse(&id)
        .ok_or_else(|| error_response(&format!("未知客户端：{id}"), StatusCode::BAD_REQUEST))?;
    let request: ResourceRequest = parse_payload(&body)?;
    let plan = build_resource_plan(target, &request, &config)
        .map_err(|message| error_response(&message, StatusCode::BAD_REQUEST))?;
    if plan.changes.is_empty() {
        return Ok(Json(json!({
            "success": true,
            "data": { "noop": true, "path": plan.path.to_string_lossy(), "changes": [] }
        })));
    }
    Ok(Json(json!({
        "success": true,
        "data": {
            "token": plan_token(&plan),
            "clientId": target.id(),
            "kind": "mcp",
            "path": plan.path.to_string_lossy(),
            "exists": plan.existed,
            "changes": plan.changes,
            "after": plan.after_text,
        }
    })))
}

/// POST /api/agents/{id}/resources/apply —— 令牌 CAS → 备份 → 原子写 → 回读确认。
pub async fn apply_resources(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
    body: Bytes,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = verify_admin(&state, &headers).await?;
    let target = Target::parse(&id)
        .ok_or_else(|| error_response(&format!("未知客户端：{id}"), StatusCode::BAD_REQUEST))?;
    let request: ResourceRequest = parse_payload(&body)?;
    let plan = build_resource_plan(target, &request, &config)
        .map_err(|message| error_response(&message, StatusCode::BAD_REQUEST))?;
    ensure_token(&plan, &request.token)?;
    let backup = backup_resource(&plan, &config)
        .map_err(|message| error_response(&message, StatusCode::INTERNAL_SERVER_ERROR))?;
    write_atomic(&plan.path, &plan.after_text)
        .map_err(|message| error_response(&message, StatusCode::INTERNAL_SERVER_ERROR))?;
    // 回读确认：写进去的条目必须能被重新解析且与计划一致。
    let (written, _) = read_text(&plan.path);
    let verified = match config_files::parse(plan.format, &written) {
        Ok(value) => {
            let mut changes = Vec::new();
            diff_values(
                &value,
                &config_files::parse(plan.format, &plan.after_text).unwrap_or(Value::Null),
                "",
                &mut changes,
            );
            changes.is_empty()
        }
        Err(_) => false,
    };
    Ok(Json(json!({
        "success": true,
        "data": {
            "applied": true,
            "verified": verified,
            "clientId": target.id(),
            "kind": "mcp",
            "path": plan.path.to_string_lossy(),
            "backup": backup.map(|p| p.to_string_lossy().to_string()),
            "needsRestart": true,
        }
    })))
}

/// 解析管理面 JSON 请求体（体积上限 + 形状）；空体等同 `{}`。
fn parse_payload<T: serde::de::DeserializeOwned>(
    body: &Bytes,
) -> Result<T, (StatusCode, Json<Value>)> {
    if body.len() > MAX_BODY_BYTES {
        return Err(error_response("请求体过大", StatusCode::PAYLOAD_TOO_LARGE));
    }
    let empty: &[u8] = b"{}";
    let slice: &[u8] = if body.is_empty() {
        empty
    } else {
        body.as_ref()
    };
    serde_json::from_slice(slice)
        .map_err(|e| error_response(&format!("请求不是有效 JSON：{e}"), StatusCode::BAD_REQUEST))
}

/// 解析 `WireRequest`（预览/应用/恢复/导入共用）。
fn parse_request(body: &Bytes) -> Result<WireRequest, (StatusCode, Json<Value>)> {
    parse_payload(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_reports_only_changed_keys() {
        let before = json!({ "env": { "A": "1" }, "keep": true });
        let after = json!({ "env": { "A": "2" }, "keep": true, "added": 1 });
        let mut out = Vec::new();
        diff_values(&before, &after, "", &mut out);
        assert_eq!(out.len(), 2, "{out:?}");
        assert!(out
            .iter()
            .any(|item| item["path"] == "/env/A" && item["after"] == "2"));
        assert!(out
            .iter()
            .any(|item| item["path"] == "/added" && item["before"].is_null()));
    }

    #[test]
    fn pointer_escape_matches_json_pointer_spec() {
        assert_eq!(escape_pointer("a/b~c"), "a~1b~0c");
    }

    #[test]
    fn gateway_base_rewrites_wildcard_host() {
        let mut config = crate::config::AppConfig::default();
        config.server.host = "0.0.0.0".into();
        config.server.port = 9527;
        assert_eq!(gateway_base(&config), "http://127.0.0.1:9527");
        config.server.host = "10.0.0.5".into();
        assert_eq!(gateway_base(&config), "http://10.0.0.5:9527");
    }

    #[test]
    fn non_writable_client_is_refused_with_reason() {
        let request = WireRequest {
            client_id: "cursor-cli".into(),
            ..WireRequest::default()
        };
        let config = crate::config::AppConfig::default();
        let error = build_plan(&request, &config).unwrap_err();
        assert!(error.contains("Cursor"), "{error}");
    }

    #[test]
    fn token_changes_when_disk_content_changes() {
        let mut config = crate::config::AppConfig::default();
        config.server.port = 9527;
        let request = WireRequest {
            client_id: "claude-code".into(),
            ..WireRequest::default()
        };
        // 只校验令牌与 before/after 绑定：直接构造两份 Plan 比较。
        let base = Plan {
            target: Target::ClaudeCode,
            path: PathBuf::from("/tmp/settings.json"),
            format: ConfigFormat::Json,
            before_text: "{}".into(),
            existed: true,
            after_text: "{\"a\":1}".into(),
            changes: Vec::new(),
        };
        let mut changed = Plan {
            target: Target::ClaudeCode,
            path: PathBuf::from("/tmp/settings.json"),
            format: ConfigFormat::Json,
            before_text: "{\"x\":0}".into(),
            existed: true,
            after_text: "{\"a\":1}".into(),
            changes: Vec::new(),
        };
        assert_ne!(plan_token(&base), plan_token(&changed));
        changed.before_text = "{}".into();
        assert_eq!(plan_token(&base), plan_token(&changed));
        let _ = request;
        let _ = config;
    }

    #[test]
    fn profiles_reject_empty_and_duplicate_ids() {
        let empty = vec![json!({ "id": "  " })];
        assert!(normalize_profiles(&empty).unwrap_err().contains("不能为空"));
        // 去空白之后撞车也算重复。
        let duplicated = vec![json!({ "id": "a" }), json!({ "id": " a " })];
        assert!(normalize_profiles(&duplicated)
            .unwrap_err()
            .contains("重复"));
    }

    #[test]
    fn profiles_are_capped_at_200() {
        let over: Vec<Value> = (0..=MAX_PROFILES)
            .map(|index| json!({ "id": format!("p{index}") }))
            .collect();
        assert!(normalize_profiles(&over).unwrap_err().contains("200"));
        let exact: Vec<Value> = (0..MAX_PROFILES)
            .map(|index| json!({ "id": format!("p{index}") }))
            .collect();
        assert_eq!(
            normalize_profiles(&exact).map(|items| items.len()),
            Ok(MAX_PROFILES)
        );
    }

    #[test]
    fn stale_token_is_conflict() {
        let plan = Plan {
            target: Target::ClaudeCode,
            path: PathBuf::from("/tmp/settings.json"),
            format: ConfigFormat::Json,
            before_text: "{}".into(),
            existed: true,
            after_text: "{\"a\":1}".into(),
            changes: Vec::new(),
        };
        let token = plan_token(&plan);
        assert!(ensure_token(&plan, &token).is_ok());
        // 预览之后文件被别的程序改过：同一令牌对不上新计划 → 409。
        let changed = Plan {
            target: Target::ClaudeCode,
            path: PathBuf::from("/tmp/settings.json"),
            format: ConfigFormat::Json,
            before_text: "{\"x\":0}".into(),
            existed: true,
            after_text: "{\"a\":1}".into(),
            changes: Vec::new(),
        };
        let error = ensure_token(&changed, &token).expect_err("令牌必须失效");
        assert_eq!(error.0, StatusCode::CONFLICT);
    }

    #[test]
    fn readonly_client_resource_write_is_refused_with_reason() {
        let config = crate::config::AppConfig::default();
        let request = ResourceRequest {
            service: "web-search".into(),
            ..ResourceRequest::default()
        };
        let error = build_resource_plan(Target::CursorCli, &request, &config).unwrap_err();
        assert_eq!(error, block_reason(Target::CursorCli));
        assert!(!error.is_empty());
    }

    #[test]
    fn import_detects_gateway_and_model() {
        let value = json!({
            "env": { "ANTHROPIC_BASE_URL": "http://127.0.0.1:9527/" },
            "model": "aigx-pro"
        });
        let detected = detected_upstream(Target::ClaudeCode, "http://127.0.0.1:9527", &value, true);
        assert_eq!(detected["baseUrl"], "http://127.0.0.1:9527/");
        assert_eq!(detected["model"], "aigx-pro");
        // 末尾斜杠不影响「是不是本网关」的判定。
        assert_eq!(detected["matchesGateway"], true);
        assert_eq!(detected["connected"], true);
    }

    #[test]
    fn skill_scan_reads_directory_and_file_forms() {
        let dir = tempfile::tempdir().unwrap();
        let pack = dir.path().join("web-search");
        std::fs::create_dir_all(&pack).unwrap();
        std::fs::write(
            pack.join("SKILL.md"),
            "---\nname: 网络搜索\ndescription: 搜网页\nenabled: false\n---\n",
        )
        .unwrap();
        std::fs::write(pack.join("skill.json"), "{\"name\":\"web-search\"}").unwrap();
        std::fs::write(
            dir.path().join("solo.md"),
            "---\nname: solo\ndescription: 单文件\n---\n",
        )
        .unwrap();
        // 隐藏项与没有文档的目录都不该出现。
        std::fs::create_dir_all(dir.path().join(".hidden")).unwrap();
        std::fs::create_dir_all(dir.path().join("empty")).unwrap();
        let mut items = Vec::new();
        skill_items_in(dir.path(), 1, &mut items);
        assert_eq!(items.len(), 2, "{items:?}");
        let dir_item = items
            .iter()
            .find(|item| item["source"] == "directory")
            .expect("目录形态");
        assert_eq!(dir_item["name"], "网络搜索");
        assert_eq!(dir_item["enabled"], false);
        assert_eq!(dir_item["managed"], true);
        assert_eq!(dir_item["id"], "1:web-search");
        let file_item = items
            .iter()
            .find(|item| item["source"] == "file")
            .expect("单文件形态");
        assert_eq!(file_item["name"], "solo");
        assert_eq!(file_item["enabled"], true);
    }
}
