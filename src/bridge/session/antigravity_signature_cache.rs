//! Antigravity 思考签名（`thoughtSignature`）的服务端缓存。
//!
//! 移植自 Agent2API `core/protocol/antigravity_signature_cache.rs`（issue #291）。
//!
//! ── 为什么需要缓存 ─────────────────────────────────────────
//! 上游把历史回放里的 `functionCall` 签名当**硬性要求**：不带直接 400
//! （「Function call is missing a thought_signature in functionCall parts」）；
//! 伪造同样 400（「Corrupted thought signature」—— 签名本体被校验）。
//! 网关把签名放在 `tool_calls[].extra_content.google.thought_signature`
//! （Google 自家 OpenAI 兼容端点的字段位置）随响应带出——但那是非标字段，
//! 多数 OpenAI 兼容客户端下一轮会把它丢掉，于是「模型调工具 → 客户端回传
//! 结果」的多轮循环第二轮必 400。
//!
//! 解法是服务端缓存签名、历史不带时回填：
//!   - **写**（响应侧）：流式解码器解析到带签名的 functionCall part 时写入；
//!   - **读**（请求侧）：`build_envelope` 组信封时回填——客户端带回的签名
//!     仍最优先（那是最忠实的一份），缓存只是客户端没带时的回填。
//!
//! ── 两档键 ─────────────────────────────────────────────────
//!   1. `sessionId + call_id → signature`（主键）：上游给 functionCall 的 id，
//!      OpenAI 形态的客户端必须原样引用，回放历史里就在手上；
//!   2. `sessionId + 函数名 → signature`（兜底）：个别框架会重排/重生成 id。
//!      签名校验若绑定到「这一次调用」而非「会话」，这一档可能仍被上游拒
//!      —— 那就退回 400（与没有缓存时同状），不会更糟。
//!
//! session 维度是硬要求：不同客户端/不同会话里的 `call_1` 互不串签名
//! （与上游 `antigravity_signature_cache.rs` 逐字对齐）。
//!
//! ── 容量与生命周期：两代（two-generation）有界缓存 ─────────
//! 写入只进当代；当代写满（`CAPACITY` 条）就整体转正为前代、当代清空重来；
//! 读先查当代再查前代，命中前代不搬回当代。最坏常驻 2×CAPACITY 条（约几 MB）。
//! 不引 `lru` 依赖：两代缓存对「最近对话的工具签名」访问模式已足够。
//! 全在内存：网关重启即清空。

use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::OnceLock;

/// 当代容量（最坏常驻 2×CAPACITY 条，见模块头）
const CAPACITY: usize = 2048;

/// 两个「世代」的键值表：写入只进 `current`，写满整体转正为 `previous`
#[derive(Default)]
struct Generations {
    current: HashMap<String, String>,
    previous: HashMap<String, String>,
}

static CACHE: OnceLock<Mutex<Generations>> = OnceLock::new();

fn cache() -> &'static Mutex<Generations> {
    CACHE.get_or_init(|| Mutex::new(Generations::default()))
}

/// 键前缀：call id 档（与会话档同表混存，前缀互不撞）
const CALL_PREFIX: &str = "c|";
/// 键前缀：会话 + 函数名档
const SESSION_PREFIX: &str = "s|";

/// 记一个「会话 + call id → 签名」（响应侧：tool_call 带签名下发时调用）。
///
/// 空会话键**不记**：空 session 会退化成跨会话的全局档，而 OpenAI 形态的
/// 客户端大量复用 `call_1` 这类 id——串签名的后果比未命中更糟。
pub fn remember_call(session: &str, call_id: &str, signature: &str) {
    if session.is_empty() || call_id.is_empty() || signature.is_empty() {
        return;
    }
    insert(
        format!("{CALL_PREFIX}{session}|{call_id}"),
        signature.to_string(),
    );
}

/// 记一个「会话 + 函数名 → 签名」（响应侧兜底档；会话键拿不到时不记，
/// 空键会变成跨会话串味的全局档）。
pub fn remember_for_session(session: &str, name: &str, signature: &str) {
    if session.is_empty() || name.is_empty() || signature.is_empty() {
        return;
    }
    insert(
        format!("{SESSION_PREFIX}{session}|{name}"),
        signature.to_string(),
    );
}

/// 按「会话 + call id」取回签名（请求侧回填的主键档）。
pub fn lookup_call(session: &str, call_id: &str) -> Option<String> {
    if session.is_empty() || call_id.is_empty() {
        return None;
    }
    lookup(&format!("{CALL_PREFIX}{session}|{call_id}"))
}

/// 按「会话 + 函数名」取回签名（请求侧回填的兜底档）。
#[allow(dead_code)] // 预留/线表符号（见 main.rs 债务清单）
pub fn lookup_for_session(session: &str, name: &str) -> Option<String> {
    if session.is_empty() || name.is_empty() {
        return None;
    }
    lookup(&format!("{SESSION_PREFIX}{session}|{name}"))
}

/// 占位签名（Gemini 官方哨兵）：视为「没有真实签名」，允许被缓存回填覆盖。
const SENTINEL_SIGNATURES: [&str; 2] = [
    "skip_thought_signature_validator",
    "context_engineering_is_the_way_to_go",
];

/// 把服务端缓存的签名回填进一份 **Gemini 请求信封**（请求构造点调用）。
///
/// 与上游 `plugin_host/gemini_signatures.rs::fill_cached` 同口径，但
/// **不做模型前缀门**：AIGX 的调用点是 antigravity 原生桥的信封组装，全部
/// 走 Gemini 协议（模型名如 `gemini-pro-agent` 不在上游白名单里）；保持
/// 「客户端没带签名 → 按缓存回填」的既有行为不变。
/// - session 取 `request.sessionId`，兼容 AIGX 原生 antigravity 信封的
///   根级 `sessionId`（两处都有时以 `request.sessionId` 为准）；
/// - 只扫 `model` 轮的 parts，跳过已有真实签名的 part（客户端带回的签名
///   永远最优先）；官方哨兵占位视为缺签名，可被覆盖；
/// - 只回填 **主键档**（session + call id）；函数名档会串「同名重复调用」，
///   不在群回填里用（与上游注释同立场）。
///
/// 返回回填数量（调用方仅用于日志）。
pub fn fill_cached(body: &mut serde_json::Value) -> usize {
    let session = body
        .pointer("/request/sessionId")
        .and_then(serde_json::Value::as_str)
        .or_else(|| body.get("sessionId").and_then(serde_json::Value::as_str))
        .unwrap_or("")
        .to_string();
    if session.is_empty() {
        return 0;
    }
    let path = if body.pointer("/request/contents").is_some() {
        "/request/contents"
    } else {
        "/contents"
    };
    let Some(contents) = body
        .pointer_mut(path)
        .and_then(serde_json::Value::as_array_mut)
    else {
        return 0;
    };
    let mut filled = 0usize;
    for turn in contents
        .iter_mut()
        .filter(|turn| turn.get("role").and_then(serde_json::Value::as_str) == Some("model"))
    {
        let Some(parts) = turn
            .get_mut("parts")
            .and_then(serde_json::Value::as_array_mut)
        else {
            continue;
        };
        for part in parts
            .iter_mut()
            .filter_map(serde_json::Value::as_object_mut)
        {
            let current = part
                .get("thoughtSignature")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .trim();
            if !current.is_empty() && !SENTINEL_SIGNATURES.contains(&current) {
                continue;
            }
            let id = part
                .get("functionCall")
                .and_then(|call| call.get("id"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            if let Some(signature) = lookup_call(&session, id) {
                part.insert(
                    "thoughtSignature".to_string(),
                    serde_json::Value::String(signature),
                );
                filled += 1;
            }
        }
    }
    filled
}

/// 写入：当代满且是新键时先整体轮转（旧键若还热，前代里还有一份可读）
fn insert(key: String, value: String) {
    let mut guard = cache().lock();
    if guard.current.len() >= CAPACITY && !guard.current.contains_key(&key) {
        guard.previous = std::mem::take(&mut guard.current);
    }
    guard.current.insert(key, value);
}

/// 读取：先当代后前代
fn lookup(key: &str) -> Option<String> {
    let guard = cache().lock();
    guard
        .current
        .get(key)
        .or_else(|| guard.previous.get(key))
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_call_and_session_keys() {
        remember_call("sess-1", "call-1", "sig-a");
        assert_eq!(lookup_call("sess-1", "call-1").as_deref(), Some("sig-a"));
        // 空键/空值拒绝
        remember_call("", "call-1", "sig");
        assert_eq!(lookup_call("", "call-1"), None);
        remember_call("sess-1", "call-2", "");
        assert_eq!(lookup_call("sess-1", "call-2"), None);

        remember_for_session("sess-1", "get_weather", "sig-b");
        assert_eq!(
            lookup_for_session("sess-1", "get_weather").as_deref(),
            Some("sig-b")
        );
    }

    /// 主键档与兜底档都必须按 session 隔离：不同会话里的同名 call id /
    /// 同名函数互不串签名（上游两档键的硬要求）。
    #[test]
    fn two_tier_keys_are_session_isolated() {
        remember_call("sess-a", "call_1", "sig-a");
        assert_eq!(lookup_call("sess-a", "call_1").as_deref(), Some("sig-a"));
        assert_eq!(lookup_call("sess-b", "call_1"), None, "跨 session 不得命中");
        assert_eq!(lookup_call("sess-a", "call_2"), None, "未命中的 call id");

        remember_for_session("sess-a", "read_file", "sig-name");
        assert_eq!(
            lookup_for_session("sess-a", "read_file").as_deref(),
            Some("sig-name")
        );
        assert_eq!(lookup_for_session("sess-b", "read_file"), None);
        assert_eq!(lookup_for_session("sess-a", "write_file"), None);
    }

    #[test]
    fn generation_rotation_keeps_previous_readable() {
        // 塞满当代
        for i in 0..CAPACITY {
            remember_call("rot", &format!("rot-{i}"), "s");
        }
        // 新键触发轮转
        remember_call("rot", "rot-new", "fresh");
        // 旧键在前代仍可读
        assert_eq!(lookup_call("rot", "rot-0").as_deref(), Some("s"));
        assert_eq!(lookup_call("rot", "rot-new").as_deref(), Some("fresh"));
    }

    fn gemini_envelope(model: &str, session: &str) -> serde_json::Value {
        serde_json::json!({
            "model": model,
            "sessionId": session,
            "request": {
                "sessionId": session,
                "contents": [
                    { "role": "user", "parts": [{ "text": "hi" }] },
                    { "role": "model", "parts": [
                        { "functionCall": { "id": "call_1", "name": "read_file" } },
                        { "functionCall": { "id": "call_2", "name": "write_file" },
                          "thoughtSignature": "client-brought" },
                    ]},
                ],
            },
        })
    }

    #[test]
    fn fill_cached_backfills_missing_and_keeps_client_signature() {
        remember_call("s-fill", "call_1", "cached-1");
        remember_call("s-fill", "call_2", "cached-2");
        let mut body = gemini_envelope("gemini-2.0-pro", "s-fill");
        assert_eq!(fill_cached(&mut body), 1, "只为缺签名且有缓存的 part 回填");
        let parts = &body["request"]["contents"][1]["parts"];
        assert_eq!(parts[0]["thoughtSignature"], "cached-1");
        assert_eq!(
            parts[1]["thoughtSignature"], "client-brought",
            "客户端带回的签名最优先"
        );
    }

    #[test]
    fn fill_cached_replaces_sentinel_and_gates_by_session() {
        remember_call("s-sentinel", "call_1", "real-sig");
        let mut body = gemini_envelope("gemini-2.0-pro", "s-sentinel");
        body["request"]["contents"][1]["parts"][0]["thoughtSignature"] =
            serde_json::json!("skip_thought_signature_validator");
        assert_eq!(fill_cached(&mut body), 1, "官方哨兵占位可被覆盖");
        assert_eq!(
            body["request"]["contents"][1]["parts"][0]["thoughtSignature"],
            "real-sig"
        );

        // session 门：未知 session 不回填（模型名不设门——AIGX 调用点全走
        // Gemini 协议，见函数注释）
        remember_call("s-other", "call_1", "x");
        let mut wrong = gemini_envelope("gemini-2.0-pro", "s-unknown");
        assert_eq!(fill_cached(&mut wrong), 0);

        // 根级 sessionId（AIGX 原生 antigravity 信封形态）同样命中
        let mut root_level = serde_json::json!({
            "sessionId": "s-sentinel",
            "request": {
                "contents": [
                    { "role": "model", "parts": [
                        { "functionCall": { "id": "call_1", "name": "read_file" } },
                    ]},
                ],
            },
        });
        assert_eq!(fill_cached(&mut root_level), 1);
        assert_eq!(
            root_level["request"]["contents"][0]["parts"][0]["thoughtSignature"],
            "real-sig"
        );
    }
}
