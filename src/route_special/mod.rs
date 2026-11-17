// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//!
//! 特殊路由：把两类「不是正常对话轮次」的请求改写到指定模型。
//!
//! ── 移植范围 ─────────────────────────────────────────────────
//! 对齐 A2A v3.5.0-beta.1 的 `core/compact_routing`（ZCode 上下文压缩模板
//! 识别 + 换模型后的参数收口）、`core/title_routing`（Codex / ZCode /
//! Claude Code 三家标题生成请求识别），以及 `config/compact_routes`、
//! `config/title_routes` 的配置形状、校验与修订号（revision）语义。
//!
//! ── AIGX 侧适配点（与 A2A 的差异，逐条说明）───────────────────
//! 1. **持久化**：A2A 走进程内 config map（`CONFIG` + `try_update_raw_field`）。
//!    AIGX 用 `FileStore` KV（键 [`compact::KEY_COMPACT_ROUTES`] /
//!    [`title::KEY_TITLE_ROUTES`]），整块读写。修订号沿用 A2A 语义：对
//!    **存储值**做 sha256（键不存在或为 `null` 时对 `Value::Null` 求哈希）。
//! 2. **进程级快照**：数据面每请求零分配读配置。A2A 读 `current()`；
//!    AIGX 用 [`OnceLock`] + 每半份配置一个 `RwLock<Arc<..Snapshot>>`，
//!    由 [`install`] 在 `main.rs` 启动时装载、管理面保存后原子替换。
//!    **install 必须在 main.rs 被真实调用**，否则管理面 500、数据面不生效。
//! 3. **客户端识别**：A2A 用 `pipeline::request_source` 的 `behavior_client`。
//!    AIGX 数据面**没有** behavior_client；退化为读 [`crate::api::client_id`]
//!    的 [`ClientIdentity`]（UA / 显式声明 `X-Client-Name` / 自定义规则），
//!    再映射成稳定 id（见 [`client_id_from_identity`]）。ZCode 已在
//!    `api::client_id::BUILTIN_CLIENTS` 注册内置 UA 令牌 `zcode`（对齐 A2A
//!    `core/client/catalog.rs`），显式声明 `X-Client-Name: zcode` 也归一到
//!    同一规范名 `ZCode` —— 两条路径都能命中压缩路由。
//! 4. **挂钩点**：只挂钩 chat 数据面（[`apply_chat_request`]），只做「识别 +
//!    改写 `body["model"]` + 请求体留痕」，不碰协议转换。Responses /
//!    Anthropic 两个入口本波未接（A2A 支持，AIGX 待后续）。
//! 5. **未移植**：`parameters::cap_output`（按目标模型 `maxOutputTokens`
//!    收口）需要模型目录查询，数据面 hook 为纯函数、拿不到 provider 目录，
//!    本波不做；`title_routes` 的旧形状（`clients`+`model`）折叠不做 ——
//!    AIGX 无历史遗留配置。模型存在性校验放在管理面 PUT（见 `api::admin`）。

use std::sync::{Arc, OnceLock};

use axum::http::HeaderMap;
use parking_lot::{Mutex, RwLock};
use serde_json::Value;

use crate::api::client_id::ClientIdentity;
use crate::storage::FileStore;

pub mod compact;
pub mod title;

pub use compact::{CompactRoutes, CompactRoutesSnapshot};
pub use title::{Fallback, TitleRoutes, TitleRoutesSnapshot};

/// 支持的客户端 id（稳定、小写短横线）。新增一家要同时在 `title` / `compact`
/// 的判据与 `api::admin` 的校验里加上，否则界面能勾、请求永不命中。
pub const CLIENT_ZCODE: &str = "zcode";
pub const CLIENT_CLAUDE_CODE: &str = "claude-code";
pub const CLIENT_CODEX: &str = "codex";

/// 客户端目录 `(id, label)`。管理面据此下发，前端不各写一份。
pub const CLIENTS: &[(&str, &str)] = &[
    (CLIENT_ZCODE, "ZCode"),
    (CLIENT_CLAUDE_CODE, "Claude Code"),
    (CLIENT_CODEX, "Codex"),
];

/// 客户端 id → 展示名。
pub fn client_label(id: &str) -> Option<&'static str> {
    CLIENTS
        .iter()
        .find(|(key, _)| *key == id)
        .map(|(_, label)| *label)
}

/// 改写痕迹字段：随请求体带到转发层。
///
/// chat 数据面用 [`ChatFormat`](crate::bridge::ChatFormat) 重建上游请求体，
/// 未知顶层字段不会转发；本标记只作请求内可观测痕迹（[`take_route`] 可取走）。
pub const ROUTE_FIELD: &str = "_aigx_special_route";

/// 改写种类。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// 上下文压缩（ZCode）
    Compact,
    /// 标题生成（Codex / ZCode / Claude Code）
    Title,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Compact => "compact",
            Self::Title => "title",
        }
    }
}

/// 一次改写的结果。调用方据此把选路/日志的模型切到 `target`，并把
/// `source` 记进 `RequestLog.origin_model`（用户原始请求名）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rewrite {
    pub kind: Kind,
    /// 命中的客户端 id（[`CLIENT_CODEX`] 等静态值）。
    pub client: &'static str,
    /// 原始（未改写）模型名。
    pub source: String,
    /// 改写后的模型名。
    pub target: String,
    /// 仅标题路由：目标不可用时的回退策略（压缩路由恒为 None）。
    pub fallback: Option<Fallback>,
}

// ── 进程级快照 ────────────────────────────────────────────────────────

struct SpecialRouteStore {
    store: Arc<FileStore>,
    compact: RwLock<Arc<CompactRoutesSnapshot>>,
    title: RwLock<Arc<TitleRoutesSnapshot>>,
}

static STORE: OnceLock<SpecialRouteStore> = OnceLock::new();

/// 保存串行锁：`save_*` 的「读存储 → 校验修订号 → 写回 → 换快照」必须整体原子，
/// 否则两个并发保存会互相覆盖（FileStore 无事务）。
fn save_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// 启动时安装（`main.rs` 调用一次；重复调用忽略后者）。
///
/// 从 `FileStore` 装载两份配置到进程级快照。**必须在 main.rs 被真实调用**。
pub fn install(store: Arc<FileStore>) {
    let engine = SpecialRouteStore {
        compact: RwLock::new(compact::load(&store)),
        title: RwLock::new(title::load(&store)),
        store,
    };
    if STORE.set(engine).is_err() {
        tracing::warn!("特殊路由存储已初始化，忽略重复 install");
    }
}

/// 是否已安装（测试/诊断用）。
pub fn installed() -> bool {
    STORE.get().is_some()
}

/// 当前压缩路由快照（数据面每请求调用；未安装时返回内置默认）。
pub fn compact_current() -> Arc<CompactRoutesSnapshot> {
    match STORE.get() {
        Some(store) => store.compact.read().clone(),
        None => Arc::new(CompactRoutesSnapshot::default()),
    }
}

/// 当前标题路由快照（数据面每请求调用；未安装时返回内置默认）。
pub fn title_current() -> Arc<TitleRoutesSnapshot> {
    match STORE.get() {
        Some(store) => store.title.read().clone(),
        None => Arc::new(TitleRoutesSnapshot::default()),
    }
}

/// 保存错误的分类，供管理面映射到 HTTP 状态码。
pub enum SaveError {
    /// 修订号不符（并发修改）→ 409。
    Conflict(String),
    /// 配置非法 → 400。
    BadRequest(String),
    /// 存储/序列化失败 → 500。
    Internal(String),
}

/// 整块保存压缩路由：校验 + 修订号 CAS + 落盘 + 换快照。
pub fn save_compact(settings: CompactRoutes, expected: &str) -> Result<(), SaveError> {
    let store = STORE.get().ok_or_else(|| {
        SaveError::Internal("特殊路由存储未初始化（main.rs 未调用 install）".into())
    })?;
    compact::validate(&settings).map_err(SaveError::BadRequest)?;
    let _guard = save_lock().lock();
    let raw: Option<Value> = store
        .store
        .get(compact::KEY_COMPACT_ROUTES)
        .map_err(|e| SaveError::Internal(format!("读取压缩路由配置失败：{e}")))?;
    if compact::revision(raw.as_ref()) != expected {
        return Err(SaveError::Conflict(
            "压缩路由已在其他位置修改，请重新读取后再保存；当前草稿未丢失".into(),
        ));
    }
    let value = serde_json::to_value(&settings)
        .map_err(|e| SaveError::Internal(format!("序列化压缩路由失败：{e}")))?;
    store
        .store
        .put(compact::KEY_COMPACT_ROUTES, &value)
        .map_err(|e| SaveError::Internal(format!("压缩路由持久化失败：{e}")))?;
    *store.compact.write() = Arc::new(CompactRoutesSnapshot {
        revision: compact::revision(Some(&value)),
        settings: Ok(settings),
    });
    Ok(())
}

/// 整块保存标题路由：校验 + 修订号 CAS + 落盘 + 换快照。
pub fn save_title(settings: TitleRoutes, expected: &str) -> Result<(), SaveError> {
    let store = STORE.get().ok_or_else(|| {
        SaveError::Internal("特殊路由存储未初始化（main.rs 未调用 install）".into())
    })?;
    title::validate(&settings).map_err(SaveError::BadRequest)?;
    let _guard = save_lock().lock();
    let raw: Option<Value> = store
        .store
        .get(title::KEY_TITLE_ROUTES)
        .map_err(|e| SaveError::Internal(format!("读取标题路由配置失败：{e}")))?;
    if title::revision(raw.as_ref()) != expected {
        return Err(SaveError::Conflict(
            "标题生成路由已在其他位置修改，请重新读取后再保存；当前草稿未丢失".into(),
        ));
    }
    let value = serde_json::to_value(&settings)
        .map_err(|e| SaveError::Internal(format!("序列化标题路由失败：{e}")))?;
    store
        .store
        .put(title::KEY_TITLE_ROUTES, &value)
        .map_err(|e| SaveError::Internal(format!("标题路由持久化失败：{e}")))?;
    *store.title.write() = Arc::new(TitleRoutesSnapshot {
        revision: title::revision(Some(&value)),
        settings: Ok(settings),
    });
    Ok(())
}

// ── 数据面入口 ────────────────────────────────────────────────────────

/// 把 [`ClientIdentity`] 映射成稳定客户端 id。
///
/// AIGX 的 `client_id` 输出**规范化展示名**（如 `"Claude Code"` / `"Codex"` /
/// `"ZCode"`）。显式声明自定义名时按小写归一 —— 这里大小写不敏感地归一到三家 id。
pub fn client_id_from_identity(identity: &ClientIdentity) -> Option<&'static str> {
    let name = identity.name.trim().to_ascii_lowercase();
    match name.as_str() {
        "codex" | "codex cli" | "codex-cli" => Some(CLIENT_CODEX),
        "claude code" | "claude-code" | "claude cli" | "claude-cli" => Some(CLIENT_CLAUDE_CODE),
        "zcode" | "z-code" | "bigmodel" | "z.ai" => Some(CLIENT_ZCODE),
        _ => None,
    }
}

/// chat 数据面入口：识别并改写模型名。
///
/// 返回 `Some` 时已完成 `body["model"]` 改写、会话参数收口与请求体留痕。
/// **调用方必须**：把本地 `model` 变量更新为 `rewrite.target`（否则后续选路
/// 仍用旧名），并把 `rewrite.source` 记进 `RequestLog.origin_model`。
///
/// 顺序与 A2A 一致：先判压缩（它的模板里也含「summary of the conversation」
/// 一类措辞，先判可保证压缩请求不被标题路由抢走），再判标题。
pub fn apply_chat_request(
    identity: &ClientIdentity,
    headers: &HeaderMap,
    body: &mut Value,
) -> Option<Rewrite> {
    let client = client_id_from_identity(identity)?;
    let source = body
        .get("model")
        .and_then(Value::as_str)?
        .trim()
        .to_string();
    if source.is_empty() {
        return None;
    }

    let rewrite = if let Some(target) = compact::resolve(client, &source, body) {
        compact::clear_source_reasoning(body);
        Rewrite {
            kind: Kind::Compact,
            client,
            source,
            target,
            fallback: None,
        }
    } else {
        let (target, fallback) = title::resolve(client, headers, body)?;
        // 目标与来源同名时不做改写（A2A `rewrite_title` 同口径）。
        if title::model_key(&target) == title::model_key(&source) {
            return None;
        }
        compact::clear_source_reasoning(body);
        Rewrite {
            kind: Kind::Title,
            client,
            source,
            target,
            fallback: Some(fallback),
        }
    };

    if let Some(object) = body.as_object_mut() {
        object.insert("model".into(), Value::String(rewrite.target.clone()));
    }
    mark(body, &rewrite);
    tracing::info!(
        kind = rewrite.kind.as_str(),
        client = rewrite.client,
        source = %rewrite.source,
        target = %rewrite.target,
        "特殊路由改写模型"
    );
    Some(rewrite)
}

/// 写入请求体痕迹字段。
pub fn mark(body: &mut Value, rewrite: &Rewrite) {
    if let Some(object) = body.as_object_mut() {
        object.insert(
            ROUTE_FIELD.into(),
            serde_json::json!({
                "kind": rewrite.kind.as_str(),
                "client": rewrite.client,
                "source": rewrite.source.clone(),
                "target": rewrite.target.clone(),
            }),
        );
    }
}

/// 取走请求体痕迹字段（若某转发路径会原样透传 body，则发送前应调用）。
pub fn take_route(body: &mut Value) -> Option<Value> {
    body.as_object_mut()?.remove(ROUTE_FIELD)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(name: &str, source: &'static str) -> ClientIdentity {
        ClientIdentity {
            name: name.to_string(),
            version: String::new(),
            source,
        }
    }

    #[test]
    fn maps_zcode_builtin_and_declared_names() {
        // 内置 UA 识别后的规范名。
        assert_eq!(
            client_id_from_identity(&identity("ZCode", "builtin_ua")),
            Some(CLIENT_ZCODE)
        );
        // 显式声明/自定义规则的小写形态（加内置条目之前的行为）。
        assert_eq!(
            client_id_from_identity(&identity("zcode", "declared")),
            Some(CLIENT_ZCODE)
        );
        assert_eq!(
            client_id_from_identity(&identity("z-code", "declared")),
            Some(CLIENT_ZCODE)
        );
        assert_eq!(
            client_id_from_identity(&identity("BigModel", "declared")),
            Some(CLIENT_ZCODE)
        );
    }

    #[test]
    fn maps_codex_and_claude_names() {
        assert_eq!(
            client_id_from_identity(&identity("Codex", "builtin_ua")),
            Some(CLIENT_CODEX)
        );
        assert_eq!(
            client_id_from_identity(&identity("Claude Code", "builtin_ua")),
            Some(CLIENT_CLAUDE_CODE)
        );
        // 未知客户端不参与特殊路由。
        assert_eq!(
            client_id_from_identity(&identity("unknown", "unknown")),
            None
        );
    }
}
