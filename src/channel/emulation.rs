//! **OpenCode 官方客户端形态伪装**（渠道记录上的 `quirks.client_emulation = "opencode"`）。
//!
//! 移植自 Agent2API `providers/custom/emulation.rs`（该文件含逐条实测记录），
//! 按 AIGX 的渠道级 quirks 语义收敛：判据在 `ChannelQuirks::emulates_opencode`，
//! 应用点在 `bridge::openai`（只有它按 OpenAI 形状发请求）。
//!
//! ── 为什么需要它（上游实测，2026-10-14 对着 opencode v2.0.27 重测）──
//! `https://opencode.ai/inference/v1` 的**免费档**模型直连一律 403：
//! ```text
//! {"type":"error","error":{"type":"FreeTierError","message":"OpenCode's free
//!  tier can only be used from within OpenCode. ..."}}
//! ```
//! 当前这道门要求四条同时成立：
//!   1. 凭证是**字面量** `public`（`Authorization: Bearer public`，匿名通道）；
//!   2. **四个会话头同时在场且同值**：`x-opencode-session`、
//!      `x-opencode-session-id`、`x-session-affinity`、`x-session-id`；
//!   3. `x-opencode-request` 与 `x-opencode-project` **一个都不能发**；
//!   4. 请求体须是「智能体形态」：`stream: true`，且 `tools` 里同时含一个
//!      shell 类工具（`bash` / `shell`）与一个 `read`（小写逐字相同）。
//!
//! 会话 id 只校验**形状与取值**（`ses_<12位小写 hex><14位 Base62>`，低 12 位
//! ≥ [`LOW12_FLOOR`]），不校验它是否在服务端注册过 —— 本模块的派生算法
//! （[`session_id`]，低 12 位钉 [`LOW12_TAG`]）实测直接放行。
//!
//! ── 第二个面：OpenCode **Go**（`/zen/go/v1`）───────────────
//! Go 是订阅制端点，要求完全不同：只要客户端带一个稳定的会话头
//! （`x-opencode-session`），不要求伪装、也拒绝被塞桩工具的请求。
//! [`apply_go_headers`] 只补会话头与产品 UA，凭证与请求体一概不碰。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap / expect / panic。

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// 匿名凭证：上游免费档认的字面量（无需注册、无轮换）
pub const ANONYMOUS_KEY: &str = "public";

/// 伪装成官方 CLI 的 User-Agent（对照实现实测可通过的版本号）
const USER_AGENT: &str = "opencode/1.18.31 (windows amd64; node22)";

/// 免费档校验要求的桩工具名（大小写敏感，实测只有小写通过）
const STUB_TOOLS: [&str; 2] = ["bash", "read"];

/// 「shell 类工具」的等价名 —— 上游认 `bash` 或 `shell` 任一
const SHELL_TOOL_ALIASES: [&str; 2] = ["bash", "shell"];

/// 会话标识的 Base62 字母表（与对照实现逐字一致）
const BASE62: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// 会话种子：**首个 user 消息**的 content 序列化。
///
/// 多轮对话里历史不断增长，取首轮才能让同一场对话一直映射到同一个会话
/// （上游按会话做提示词缓存亲和）。取不到时给空串，调用方退化成随机会话。
pub fn conversation_seed(body: &Value) -> String {
    let Some(messages) = body.get("messages").and_then(Value::as_array) else {
        return String::new();
    };
    for message in messages {
        if message.get("role").and_then(Value::as_str) != Some("user") {
            continue;
        }
        let Some(content) = message.get("content") else {
            continue;
        };
        if content.is_null() {
            continue;
        }
        let seed = match content {
            Value::String(text) => text.clone(),
            other => other.to_string(),
        };
        if !seed.is_empty() {
            return seed;
        }
    }
    String::new()
}

/// 任意种子 → 上游要求的 `ses_<12位小写 hex><14位 Base62>` 会话 id。
///
/// 合格（形状与取值）时**原样返回**，保住调用方自带的亲和键；不合格的种子
/// 当普通字符串重新派生。低 12 位钉 [`LOW12_TAG`]，其余位来自 `SHA-256`：
/// 同一场对话仍映射到同一个 id。
pub fn session_id(seed: &str) -> String {
    if is_acceptable_session_id(seed) {
        return seed.to_string();
    }
    let seed = if seed.is_empty() {
        random_seed()
    } else {
        seed.to_string()
    };
    let digest = Sha256::digest(format!("ses\u{0}{seed}").as_bytes());
    let mut bits = 0u64;
    for byte in &digest[..6] {
        bits = (bits << 8) | u64::from(*byte);
    }
    let bits = (bits & !0xFFF) | u64::from(LOW12_TAG);
    format!("ses_{bits:012x}{}", base62_fixed(&digest[6..16], 14))
}

/// `ses_` + 12 位小写 hex + 14 位 Base62
fn valid_session_id(text: &str) -> bool {
    let Some(rest) = text.strip_prefix("ses_") else {
        return false;
    };
    if rest.len() != 12 + 14 {
        return false;
    }
    let (hex, tail) = rest.split_at(12);
    hex.bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        && tail.bytes().all(|byte| BASE62.contains(&byte))
}

/// 会话 id 低 12 位的**取值下界**（2026-10-13 实测翻转点：0xFBF 拒、0xFC0 放行）
const LOW12_FLOOR: u16 = 0xFC0;

/// 会话 id 低 12 位的**产出值**：官方客户端实测恒为该值
const LOW12_TAG: u16 = 0xFFE;

/// 这个字符串能不能**直接当会话头发出去**：形状与取值都合格才算
pub fn is_acceptable_session_id(text: &str) -> bool {
    if !valid_session_id(text) {
        return false;
    }
    let Some(hex) = text.get(4..16) else {
        return false;
    };
    let Some(low) = hex
        .get(9..12)
        .and_then(|tail| u16::from_str_radix(tail, 16).ok())
    else {
        return false;
    };
    low >= LOW12_FLOOR
}

/// 随机种子（这里只是会话 id 的熵，不是密钥，退化后果仅亲和键不稳定）
fn random_seed() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// 定长 Base62（左侧补零到 `width` 位）
fn base62_fixed(bytes: &[u8], width: usize) -> String {
    let mut digits = vec![0u32; width];
    for byte in bytes {
        let mut carry = u32::from(*byte);
        for digit in digits.iter_mut().rev() {
            let value = *digit * 256 + carry;
            *digit = value % 62;
            carry = value / 62;
        }
    }
    digits
        .iter()
        .map(|digit| BASE62[(*digit as usize).min(61)] as char)
        .collect()
}

/// 出站头的伪装集合：把官方 v2 客户端会带的头补齐到 `headers` 上。
///
/// `headers` 的键名沿用本网关的默认头（`Authorization` / `User-Agent`），
/// 同名覆盖 —— **自定义 key 优先**：账号填了 key 就用它（付费模型必须），
/// 没填才用匿名字面量（免费档）。
///
/// 头集合是**实测**出来的，多一个少一个都不行：四个会话头必须同时在场且同值；
/// **不发** `x-opencode-request` / `x-opencode-project`。
pub fn apply_headers(headers: &mut Vec<(String, String)>, api_key: &str, session: &str) {
    let credential = if api_key.trim().is_empty() {
        ANONYMOUS_KEY
    } else {
        api_key.trim()
    };
    let pairs = [
        ("Authorization".to_string(), format!("Bearer {credential}")),
        ("User-Agent".to_string(), USER_AGENT.to_string()),
        ("x-opencode-client".to_string(), "cli".to_string()),
        ("x-opencode-session".to_string(), session.to_string()),
        ("x-opencode-session-id".to_string(), session.to_string()),
        ("x-session-affinity".to_string(), session.to_string()),
        ("X-Session-Id".to_string(), session.to_string()),
    ];
    merge(headers, pairs);
}

/// **Go 面**（`/zen/go/v1`）的会话头名
pub const GO_SESSION_HEADER: &str = "x-opencode-session";

/// Go 面的 User-Agent：文档要求客户端自报家门，**明确不要通用 HTTP 库名**
fn go_user_agent() -> String {
    format!("aigx/{}", env!("CARGO_PKG_VERSION"))
}

/// **Go 面**的出站头：只补会话头与 UA（不动凭证、不动请求体）
pub fn apply_go_headers(headers: &mut Vec<(String, String)>, session: &str) {
    merge(
        headers,
        [
            ("User-Agent".to_string(), go_user_agent()),
            (GO_SESSION_HEADER.to_string(), session.to_string()),
        ],
    );
}

/// 把一对键值按名合并进头表（ASCII 不区分大小写；同名覆盖，其余追加）
fn merge(headers: &mut Vec<(String, String)>, pairs: impl IntoIterator<Item = (String, String)>) {
    for (key, value) in pairs {
        match headers
            .iter_mut()
            .find(|(name, _)| name.eq_ignore_ascii_case(&key))
        {
            Some(slot) => slot.1 = value,
            None => headers.push((key, value)),
        }
    }
}

/// 请求体补齐「智能体形态」：`stream: true` + `tools` 里必须同时含一个
/// shell 类工具与一个 `read`。
///
/// 四条细节：
///   · `stream` 恒置 true（上游免费档只接受流式；非流式下游由网关聚合器收敛）；
///   · **缺哪个补哪个，客户端自带的工具一律原样保留**（不覆盖参数定义）；
///   · shell 侧认两个等价名：客户端带了 `bash` 或 `shell` 都不再补；
///   · 客户端**原本没有 tools** 时补 `tool_choice: "none"`（桩工具只为过检）；
///     客户端自带工具时不动 `tool_choice`（那会破坏它本来能用的工具调用）。
///
/// 返回是否改动了请求体（只给日志用）。
pub fn ensure_agent_shape(body: &mut Value) -> bool {
    let mut changed = false;
    let Some(object) = body.as_object_mut() else {
        return false;
    };
    if object.get("stream").and_then(Value::as_bool) != Some(true) {
        object.insert("stream".to_string(), Value::Bool(true));
        changed = true;
    }
    let had_tools = object
        .get("tools")
        .and_then(Value::as_array)
        .is_some_and(|tools| !tools.is_empty());
    let mut tools: Vec<Value> = object
        .get("tools")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let declared = tool_names(&tools);
    for name in STUB_TOOLS {
        let satisfied = if name == "bash" {
            declared
                .iter()
                .any(|item| SHELL_TOOL_ALIASES.contains(&item.as_str()))
        } else {
            declared.iter().any(|item| item.as_str() == name)
        };
        if satisfied {
            continue;
        }
        tools.push(stub_tool(name));
        changed = true;
    }
    object.insert("tools".to_string(), Value::Array(tools));
    if !had_tools {
        object.insert("tool_choice".to_string(), Value::String("none".to_string()));
    }
    changed
}

/// 请求体里 `tools` 已声明的 function 名（chat 形态，大小写不归一）
fn tool_names(tools: &[Value]) -> Vec<String> {
    tools
        .iter()
        .filter_map(|tool| {
            tool.get("function")
                .and_then(|function| function.get("name"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect()
}

/// 一个桩工具（参数与描述不参与上游校验）
fn stub_tool(name: &str) -> Value {
    let property = if name == "bash" {
        "command"
    } else {
        "filePath"
    };
    json!({
        "type": "function",
        "function": {
            "name": name,
            "description": "(internal placeholder — do not call)",
            "parameters": {
                "type": "object",
                "properties": { property: { "type": "string" } },
            },
        },
    })
}

/// 端点是否 OpenCode **Go** 面（`/zen/go/v1`）—— Go 面不受 `client_emulation`
/// 开关控制，只按端点判（那条要求是端点自身的协议要求，不是伪装）
pub fn is_opencode_go_endpoint(base_url: &str) -> bool {
    base_url.contains("/zen/go/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_id_shape_is_accepted_by_upstream_rules() {
        let id = session_id("hello world");
        assert!(id.starts_with("ses_"));
        assert_eq!(id.len(), 4 + 12 + 14);
        assert!(
            is_acceptable_session_id(&id),
            "派生值必须满足上游判据: {id}"
        );
    }

    #[test]
    fn same_seed_yields_same_session() {
        assert_eq!(session_id("conversation-a"), session_id("conversation-a"));
        assert_ne!(session_id("conversation-a"), session_id("conversation-b"));
    }

    #[test]
    fn acceptable_session_id_is_preserved() {
        let good = session_id("seed");
        assert_eq!(session_id(&good), good);
    }

    #[test]
    fn low12_floor_rejects_small_values() {
        // 低 12 位 0x100 < 0xFC0 → 拒
        assert!(!is_acceptable_session_id("ses_000000000100AAAAAAAAAAAAAA"));
    }

    #[test]
    fn agent_shape_adds_stub_tools_and_stream() {
        let mut body = json!({ "model": "m", "messages": [] });
        assert!(ensure_agent_shape(&mut body));
        assert_eq!(body["stream"], json!(true));
        assert_eq!(body["tool_choice"], json!("none"));
        let names: Vec<&str> = body["tools"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|t| t["function"]["name"].as_str())
                    .collect()
            })
            .unwrap_or_default();
        assert!(names.contains(&"bash"));
        assert!(names.contains(&"read"));
    }

    #[test]
    fn agent_shape_keeps_client_tools_and_tool_choice() {
        let mut body = json!({
            "model": "m",
            "stream": true,
            "tools": [{ "type": "function", "function": { "name": "bash" } },
                      { "type": "function", "function": { "name": "read" } }],
            "tool_choice": "auto"
        });
        assert!(!ensure_agent_shape(&mut body));
        assert_eq!(body["tool_choice"], json!("auto"));
        assert_eq!(body["tools"].as_array().map(Vec::len), Some(2));
    }

    #[test]
    fn headers_use_anonymous_key_when_empty() {
        let mut headers = Vec::new();
        apply_headers(&mut headers, "", "ses_x");
        let auth = headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("authorization"))
            .map(|(_, value)| value.clone());
        assert_eq!(auth.as_deref(), Some("Bearer public"));
    }

    #[test]
    fn go_endpoint_detection() {
        assert!(is_opencode_go_endpoint("https://opencode.ai/zen/go/v1"));
        assert!(!is_opencode_go_endpoint("https://opencode.ai/inference/v1"));
    }
}
