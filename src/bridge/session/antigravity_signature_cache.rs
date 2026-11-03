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
//!   1. `call_id → signature`（主键）：上游给 functionCall 的 id，OpenAI 形态
//!      的客户端必须原样引用，回放历史里就在手上；
//!   2. `sessionId + 函数名 → signature`（兜底）：个别框架会重排/重生成 id。
//!      签名校验若绑定到「这一次调用」而非「会话」，这一档可能仍被上游拒
//!      —— 那就退回 400（与没有缓存时同状），不会更糟。
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

/// 记一个「call id → 签名」（响应侧：tool_call 带签名下发时调用）。
pub fn remember_call(call_id: &str, signature: &str) {
    if call_id.is_empty() || signature.is_empty() {
        return;
    }
    insert(format!("{CALL_PREFIX}{call_id}"), signature.to_string());
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

/// 按 call id 取回签名（请求侧回填的主键档）。
pub fn lookup_call(call_id: &str) -> Option<String> {
    if call_id.is_empty() {
        return None;
    }
    lookup(&format!("{CALL_PREFIX}{call_id}"))
}

/// 按「会话 + 函数名」取回签名（请求侧回填的兜底档）。
pub fn lookup_for_session(session: &str, name: &str) -> Option<String> {
    if session.is_empty() || name.is_empty() {
        return None;
    }
    lookup(&format!("{SESSION_PREFIX}{session}|{name}"))
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
        remember_call("call-1", "sig-a");
        assert_eq!(lookup_call("call-1").as_deref(), Some("sig-a"));
        // 空键/空值拒绝
        remember_call("", "sig");
        assert_eq!(lookup_call(""), None);
        remember_call("call-2", "");
        assert_eq!(lookup_call("call-2"), None);

        remember_for_session("sess-1", "get_weather", "sig-b");
        assert_eq!(
            lookup_for_session("sess-1", "get_weather").as_deref(),
            Some("sig-b")
        );
        // 会话隔离
        assert_eq!(lookup_for_session("sess-2", "get_weather"), None);
    }

    #[test]
    fn generation_rotation_keeps_previous_readable() {
        // 塞满当代
        for i in 0..CAPACITY {
            remember_call(&format!("rot-{i}"), "s");
        }
        // 新键触发轮转
        remember_call("rot-new", "fresh");
        // 旧键在前代仍可读
        assert_eq!(lookup_call("rot-0").as_deref(), Some("s"));
        assert_eq!(lookup_call("rot-new").as_deref(), Some("fresh"));
    }
}
