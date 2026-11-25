//! Trae（字节跳动 AI IDE）SOLO 通道登录态 Bridge。
//!
//! 协议抄自 Agent2API `providers/trae`：
//! - `POST {base}/api/agent/v3/llm_utils_chat`；
//! - 鉴权是 `Authorization: Cloud-IDE-JWT <token>` + 一批客户端身份头
//!   （`X-Cloudide-Token` / `X-Uid` / `X-Machine-Id` / `X-Device-Id` / `X-App-Id`…），
//!   身份头来自 provider 默认表 + 账号级额外请求头（账号没填的就不发）；
//! - 请求体是**白名单重建**的 SOLO 信封（`function=solo_work_lite`），
//!   多带字段会被上游拒绝；
//! - 响应 SSE 用 `event:` 区分帧（`output` / `token_usage` / `done` / `error`），
//!   **上游不发 `[DONE]`**。
//!
//! ── 会话路径只有一条（2026-11-25 从 wasm 与实测双向确认）──────
//! `agent2api-wasm-trae` 0.1.1 的 `trae.wasm` 里，`/api/agent/v3/*` 只有
//! `llm_utils_chat` 一个串（偏移 705904，紧邻 `https://trae-api-cn.mchost.guru`
//! 705862）；`/api/ide/v1/get_detail_param`（712824）与
//! `/api/ide/v1/batch_get_detail_param`（712584）是**模型目录**端点，
//! `chat_v3`（698644）与 `solo_work_*` / `solo_agent*` 是**同一枚举里的 function
//! 取值**，不是路径。所以 SOLO / IDE 两个变体的对话路径相同，
//! **不存在"SOLO 要走另一条 path"这回事**。
//!
//! 线上 `404 <html><center>TLB</center>` 的来源因此不是路径，而是**站**：
//! 7 个 `trae-solo-*` 账号把 `base_url` 写成 `https://api.trae.cn`（网页/控制台站），
//! 那个站由字节 TLB 直接回 404；同一份 token 换 `trae-api-cn.mchost.guru` 立即
//! 出 token（实测两种写法各打一次）。归一见 `account_session::adapter`。
//!
//! ── function 取值与模型表的对应（实测）──────────────────────
//! `function` 决定上游给你哪张模型表，选择不当就是流内 4001：
//! `/api/ide/v1/get_detail_param` 实测 `solo_work_lite`→54 个 config_name、
//! `solo_work_remote`→53、`solo_agent`→75、`chat_v3`→65，**这四个都含
//! `deepseek-v4.1-flash`**；`solo_coder`→41 个且**不含**它，于是
//! `function=solo_coder` + `deepseek-v4.1-flash` 必得
//! `event:error {"code":4001,"message":"We're sorry, the param is invalid…"}`。
//! 本通道固定用 `solo_work_lite`（A2A 的口径，也是四条里 catalog 最全的一条）。

use std::collections::VecDeque;

use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{json, Map, Value};

use crate::bridge::{
    Bridge, BridgeContext, BridgeError, ChatChunk, ChatChunkStream, ChatDelta, ChatFormat,
    ChatMessage, ChatResponse, FinishReason, Role, ToolCall, ToolCallDelta, UsageStats,
};

const CHAT_PATH: &str = "/api/agent/v3/llm_utils_chat";
const DEFAULT_MODEL: &str = "glm-5.2";

/// Trae 上游认得的 `config_name` 全表（90 个）。
///
/// ── 为什么必须有这张表（2026-11-25 线上实测的根因）──────────────
/// `config_name` 是 Trae **自己的模型清单键**，不是 OpenAI 的 `model` 字段：
/// 上游拿它去查 `get_detail_param` 那张表，查不到就回流内
/// `event:error {"code":4001,"message":"We're sorry, the param is invalid…"}`。
/// 而 AIGX 的全局模型映射（`kv.model_mappings`）会把请求侧名字改写成**内部 ID**
/// 再发给上游 —— 线上那条是
/// `deepseek-v4.1-flash` → `sn-deepseek-v4-1-flash-1017`（`provider: null`，
/// 对所有供应商生效）。于是 failover 里 9 个账号**全**报
/// `[4001] the param is invalid`，而同一个 token 直接打上游出 token ——
/// 账号、token、指纹头**都没有问题**，错的是这一格模型名。
///
/// 逐条实测（同一 token、同一头集，只改 `config_name`）：
///   `deepseek-v4.1-flash`            → 200 + 真回复
///   `deepseek-v4-1-flash`            → 4001
///   `deepseek-v4-1-flash-1017`       → 4001
///   `sn-deepseek-v4-1-flash-1017`    → 4001
///   `DeepSeek-V4.1-Flash`            → 4001（大小写敏感）
///   `DeepSeek-V4-Pro`                → 200 + 真回复
///   `deepseek-v4-pro-0813`           → 4001
/// 另一条全局映射同理：`glm-5.3-flash` 能过，`sn-glm-5-3-flash` 4001。
///
/// `model` 字段**不参与**这个判定（填内部 ID、填别名、留空都出 200 + 真回复），
/// 所以只归一 `config_name`，`model` 保持调用方原样，日志/计费口径不变。
///
/// 表内容取自 `POST /api/ide/v1/get_detail_param` 的
/// `function=solo_work_lite`(54) ∪ `chat_v3`(65) ∪ `solo_agent`(75) 去重，
/// 即三条 function 的并集（90 个），避免只认一条时把别的 function 的模型挡掉。
const TRAE_CONFIG_NAMES: &[&str] = &[
    "DeepSeek-V4-Flash",
    "DeepSeek-V4-Flash-Official",
    "DeepSeek-V4-Pro",
    "DeepSeek-V4-Pro-Official",
    "Doubao-Seed-2.0-Code",
    "Doubao-Seed-2.0-Code-auto",
    "Doubao-Seed-2.1-Pro",
    "Doubao-Seed-2.1-Turbo",
    "Doubao-Seed-2.1-Turbo-auto",
    "Doubao-Seed-Code",
    "Doubao-Seed-Evolving",
    "Doubao-Seed-Evolving-auto",
    "Doubao_1_6",
    "aquila",
    "browser_use_subagent",
    "computer_use_subagent",
    "custom_model_1M",
    "custom_model_1M_text",
    "custom_model_200k",
    "custom_model_200k_text",
    "custom_model_claude",
    "custom_model_deepseek_chat",
    "custom_model_deepseek_reasoner",
    "custom_model_deepseek_v4",
    "custom_model_doubao_1M",
    "custom_model_doubao_256k",
    "custom_model_gemini",
    "custom_model_gpt-5",
    "custom_model_gpt-6",
    "custom_model_kimi",
    "custom_model_no-fc",
    "custom_model_placeholder",
    "custom_model_tokenpony_deepseek_v4_1_flash",
    "custom_model_tokenpony_glm_5_3",
    "custom_model_tokenpony_qwen3_8_max",
    "custom_model_vercel",
    "custom_model_vercel_gemini",
    "deepseek-v4.1-flash",
    "doubao-for-auto",
    "explore_sub_agent_v2",
    "fast_apply",
    "fast_apply_new",
    "file_search_agent",
    "glm-4.6",
    "glm-4.6-auto",
    "glm-4.7",
    "glm-4.7-auto",
    "glm-5",
    "glm-5-turbo",
    "glm-5.1",
    "glm-5.1_advisor",
    "glm-5.2",
    "glm-5.2_advisor_default",
    "glm-5.2_advisor_doubao",
    "glm-5.3",
    "glm-5.3-flash",
    "glm-5.3-flashx",
    "glm-5.4",
    "input_optimization",
    "kimi-k2",
    "kimi-k2-0905",
    "kimi-k2.6",
    "kimi-k2.7-code",
    "kimi-k2.8-preview",
    "kimi-k3",
    "kimi-k3-auto",
    "kimi-k3.1-pro",
    "kimi-k3.1-pro-auto",
    "kimi-k3.1-pro-highspeed",
    "mimo-v2.6-flash",
    "mimo-v2.6-pro",
    "minimax-m2",
    "minimax-m2-auto",
    "minimax-m2.1",
    "minimax-m2.1-auto",
    "minimax-m3",
    "minimax-m3.1-flash",
    "qwen-3.5",
    "qwen-3.7-plus",
    "qwen3-coder",
    "qwen3.8-flash",
    "qwen3.8-max",
    "qwen4-max",
    "sagitta",
    "search_agent_qwen_fast",
    "search_agent_qwen_fast-0710",
    "seed-code-pro-0430",
    "step-5-preview",
    "summary",
    "title_generation",
];

/// 把请求侧模型名归一成 Trae 认得的 `config_name`。
///
/// 三级（顺序有意义，逐条对应线上实测）：
/// 1. **原样命中**（逐字节相等）→ 用它。`deepseek-v4.1-flash` / `DeepSeek-V4-Pro`
///    这类正常请求走这一支，行为逐字节不变。
/// 2. **规范化命中**：全局映射给的是 slug 化的内部 ID（`sn-deepseek-v4-1-flash-1017`），
///    那里 `-` 既可能代表 `.` 也可能代表原来的 `-`（`sn-glm-5-3-flash` 对应的是
///    `glm-5.3-flash`，不是 `glm.5.3.flash`），所以**不能**做字符替换再比 ——
///    改成「只留字母数字、小写」后比对（[`fold_key`]），并允许逐级剥掉尾部的
///    纯数字版本段（`-1017` / `-0813`）。
/// 3. 都不中 → **原样发出**并留一行 warn。
///
/// 为什么第 3 支不换成默认模型：换掉等于「用户要 A 却拿到 B」，是一次静默的语义
/// 改写（错模型答得越顺，越没人发现）。原样发出至少让上游如实 4001，告警里也点了名。
/// 认不出的名字本来也不该走到这里 —— 那是映射表配错，得让人看见。
fn resolve_config_name(raw: &str) -> &str {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return DEFAULT_MODEL;
    }
    // 1) 原样命中（零涟漪：线上能过的真名一个字节都不动）
    if let Some(hit) = TRAE_CONFIG_NAMES.iter().find(|name| **name == trimmed) {
        return hit;
    }
    // 2) 规范化命中：先剥 `sn-` 前缀，再逐级剥尾部纯数字段
    let mut candidate = trimmed.strip_prefix("sn-").unwrap_or(trimmed);
    loop {
        let key = fold_key(candidate);
        if !key.is_empty() {
            if let Some(hit) = TRAE_CONFIG_NAMES.iter().find(|name| fold_key(name) == key) {
                return hit;
            }
        }
        // 剥掉尾部 `-<纯数字>` 这一段（版本后缀），剥不动就收手
        match candidate.rsplit_once('-') {
            Some((head, tail)) if !head.is_empty() && tail.chars().all(|c| c.is_ascii_digit()) => {
                candidate = head;
            }
            _ => break,
        }
    }
    // 3) 原样发出 + 告警（不替换成别的模型，见函数注释）
    tracing::warn!(
        "trae: 模型 '{}' 不在上游 config_name 清单里（全局模型映射可能把它改写成了内部 ID），\
         原样发出，上游大概率回 [4001] the param is invalid —— 请核对模型映射",
        trimmed
    );
    trimmed
}

/// 比对键：只留 ASCII 字母数字并小写。
///
/// 用它而不是逐字符替换 `-`↔`.`：slug 化是有损的，`glm-5.3-flash` 与
/// `glm-5-3-flash` 的 `-` 含义不同（一个是分隔符、一个是小数点），替换法会把
/// 后者变成 `glm.5.3.flash` 而永远比不上。抹掉分隔符后两种写法落在同一个键上。
fn fold_key(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}
const DEFAULT_MAX_TOKENS: i64 = 1_000_000;
/// 真实 Trae 客户端 UA —— **两套**，由账号凭证里的 `variant` 决定。
///
/// 上游 WAF 会校验客户端身份：AIGX 早先用 `Trae/1.0 antigravity-cockpit-tools`
/// 一律被拦成 `{"code":"11-128","msg":"request illegal"}` —— 不是 token 失效
/// （那是 401），是请求被判成非法客户端。取值来自 A2A `agent2api-wasm-trae`
/// 0.1.1 wasm 的字面量（`trae/src/headers.rs`），那里 SOLO 与 IDE 是两个不同的串。
const UA_SOLO: &str = "VSCode 1.107.1 (TRAE SOLO CN)";
const UA_IDE: &str = "VSCode 1.107.1 (TRAE CN)";

/// 内部约定：adapter 把凭证里的 `variant` 以这个头名传进来，用它选 UA。
/// **不能发给上游**（见 `new` 里摘除）。
const VARIANT_HEADER: &str = "x-aigx-trae-variant";

/// 真实客户端固定随请求带的身份/渠道头。同样是 wasm 里的字面量，缺一个都可能被
/// 判非法客户端。`x-request-id` / `x-trae-request-id` 每次请求现生成（见 `request`）。
const DEFAULT_HEADERS: &[(&str, &str)] = &[
    ("accept", "*/*"),
    ("accept-language", "zh-CN"),
    ("x-user-region", "CN"),
    ("package-type", "stable_cn"),
    ("x-lgw-req-sdk-type", "3"),
    ("x-market-client-id", "VSCode 1.107.1"),
    // 下面两个是**独立的键**。早先这里写成 ("x-device-type", "x-app-version")
    // 一对 —— 那是把 wasm 里两个相邻的键名当成了键值，实际 `x-device-type`
    // 的值是客户端设备类别、`x-app-version` 是 IDE 版本。
    ("x-device-type", "ide"),
    ("x-app-version", "1.107.1"),
    ("x-ide-version", "1.107.1"),
    ("x-ide-version-type", "stable"),
    ("x-ide-version-code", "20260917"),
    ("x-plugin-channel", "icube-ai"),
    ("request-traffic-type", "prod"),
    ("sec-fetch-dest", "empty"),
    ("sec-fetch-mode", "no-cors"),
    ("sec-fetch-site", "same-origin"),
    // `x-uid` / `x-machine-id` / `x-device-id` / `x-app-version-code` 依赖账号
    // 凭证里的身份字段（wasm 里 token 那侧带 `uid` / `machineId` / `deviceId`），
    // AIGX 的 trae 账号目前 `extra_headers` 是空的，没有来源 —— 按契约
    // 「账号没填的就不发」，这里不编造值。
];

pub struct TraeBridge {
    base: String,
    client: reqwest::Client,
    headers: Vec<(String, String)>,
}

impl TraeBridge {
    pub fn new(
        base: impl Into<String>,
        credential: impl Into<String>,
        headers: Vec<(String, String)>,
        client: reqwest::Client,
    ) -> Self {
        let base = base.into().trim_end_matches('/').to_string();
        let mut merged = headers;

        // 取 variant（adapter 从凭证里带过来），用它决定 UA；取完即摘掉内部头，
        // 别把 AIGX 的内部约定发给上游。
        let variant = merged
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(VARIANT_HEADER))
            .map(|(_, value)| value.trim().to_lowercase())
            .unwrap_or_default();
        merged.retain(|(name, _)| !name.eq_ignore_ascii_case(VARIANT_HEADER));

        if !merged
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("user-agent"))
        {
            // `solo` 走 SOLO 那套 UA，其余（含缺省）走 IDE 那套 —— wasm 里
            // `trae_managed_variant_invalid` 说明变体必须与客户端形态一致。
            let ua = if variant == "solo" { UA_SOLO } else { UA_IDE };
            merged.push(("user-agent".to_string(), ua.to_string()));
        }
        // 客户端身份/渠道头：账号里显式配了同名头的以账号为准（下面 only 判断）
        for (name, value) in DEFAULT_HEADERS {
            if !merged
                .iter()
                .any(|(existing, _)| existing.eq_ignore_ascii_case(name))
            {
                merged.push((name.to_string(), value.to_string()));
            }
        }
        // 认证头：真实客户端把 token 同时放在三处（`authorization` +
        // `x-cloudide-token` + `x-ide-token`）。早先这里把 credential 参数写成
        // `_credential` 整个忽略掉，请求里没有 authorization → 上游一律
        // `{"code":"11-128","msg":"request illegal"}`（不是 401，是「非法客户端」）。
        // 账号里显式配了同名头的以账号为准，不覆盖。
        let credential = credential.into();
        let credential = credential.trim();
        if !credential.is_empty() {
            let auth_value = format!("Cloud-IDE-JWT {credential}");
            for (name, value) in [
                ("authorization", auth_value.as_str()),
                ("x-cloudide-token", credential),
                ("x-ide-token", credential),
            ] {
                if !merged
                    .iter()
                    .any(|(existing, _)| existing.eq_ignore_ascii_case(name))
                {
                    merged.push((name.to_string(), value.to_string()));
                }
            }
        }
        Self {
            base,
            client,
            headers: merged,
        }
    }

    fn request(&self, body: &Value) -> reqwest::RequestBuilder {
        let mut req = self
            .client
            .post(format!("{}{CHAT_PATH}", self.base))
            .header("Content-Type", "application/json")
            .header("Accept", "text/event-stream");
        for (name, value) in &self.headers {
            // 私有桥自己管理 Content-Type / Accept，避免账号级额外头覆盖它们
            if name.eq_ignore_ascii_case("content-type") || name.eq_ignore_ascii_case("accept") {
                continue;
            }
            req = req.header(name.as_str(), value.as_str());
        }
        // 每请求现生成的两个追踪 id：真实客户端每次都换（wasm `trae/src/headers.rs`
        // 里 `x-request-id` / `x-trae-request-id`）。复用同一个值会被当成重放。
        let request_id = uuid::Uuid::new_v4().to_string();
        // `x-tt-trace-id` 是同类追踪头（wasm 里与 x-request-id 相邻构造），一样每请求换。
        let trace_id = uuid::Uuid::new_v4().simple().to_string();
        req = req
            .header("x-request-id", &request_id)
            .header("x-trae-request-id", request_id.as_str())
            .header("x-tt-trace-id", trace_id.as_str());
        req.json(body)
    }
}

#[async_trait]
impl Bridge for TraeBridge {
    fn name(&self) -> &'static str {
        "trae"
    }

    async fn chat(
        &self,
        req: &ChatFormat,
        ctx: &BridgeContext,
    ) -> Result<ChatResponse, BridgeError> {
        let mut stream = self.chat_stream(req, ctx).await?;
        let mut content = String::new();
        let mut reasoning = String::new();
        let mut calls: Vec<ToolCall> = Vec::new();
        let mut usage = UsageStats::default();
        let mut finish = FinishReason::Stop;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            if let Some(text) = chunk.delta.content {
                content.push_str(&text);
            }
            if let Some(text) = chunk.delta.reasoning {
                reasoning.push_str(&text);
            }
            if let Some(deltas) = chunk.delta.tool_calls {
                for delta in deltas {
                    let Some(name) = delta.function_name else {
                        continue;
                    };
                    calls.push(ToolCall {
                        id: delta.id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                        function_name: name,
                        arguments: delta.arguments.unwrap_or_default(),
                    });
                }
            }
            if let Some(reason) = chunk.finish_reason {
                finish = reason;
            }
            if let Some(stats) = chunk.usage {
                usage = stats;
            }
        }
        let mut message = ChatMessage::assistant(content);
        message.reasoning = (!reasoning.is_empty()).then_some(reasoning);
        message.tool_calls = (!calls.is_empty()).then_some(calls);
        Ok(ChatResponse {
            id: format!("trae-{}", uuid::Uuid::new_v4()),
            model: req.model.clone(),
            message,
            finish_reason: finish,
            usage,
        })
    }

    async fn chat_stream(
        &self,
        req: &ChatFormat,
        _ctx: &BridgeContext,
    ) -> Result<ChatChunkStream, BridgeError> {
        let body = build_body(req);
        let resp = self
            .request(&body)
            .send()
            .await
            .map_err(|e| BridgeError::Transport(e.to_string()))?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            let text = resp.text().await.unwrap_or_default();
            return Err(BridgeError::upstream_status(
                status,
                format!("Trae 返回 HTTP {status}：{}", head(&text)),
            ));
        }

        let id = format!("trae-{}", uuid::Uuid::new_v4());
        let model = req.model.clone();
        let bytes = resp.bytes_stream();

        let stream = futures::stream::unfold(
            (
                bytes,
                Vec::<u8>::new(),
                VecDeque::<Result<ChatChunk, BridgeError>>::new(),
                Flow {
                    id,
                    model,
                    ended: false,
                    pending_usage: None,
                    event: String::new(),
                    data: String::new(),
                },
            ),
            |(mut bytes, mut buffer, mut pending, mut flow)| async move {
                loop {
                    if let Some(item) = pending.pop_front() {
                        return Some((item, (bytes, buffer, pending, flow)));
                    }
                    if flow.ended {
                        return None;
                    }
                    match bytes.next().await {
                        Some(Ok(chunk)) => {
                            buffer.extend_from_slice(&chunk);
                            while let Some(newline) = buffer.iter().position(|byte| *byte == b'\n')
                            {
                                let line: Vec<u8> = buffer.drain(..=newline).collect();
                                let text = String::from_utf8_lossy(&line)
                                    .trim_end_matches(['\r', '\n'])
                                    .to_string();
                                if let Some((event, data)) = scan_line(&text, &mut flow) {
                                    apply_event(
                                        &event,
                                        &data,
                                        &flow.id,
                                        &flow.model,
                                        &mut flow.pending_usage,
                                        &mut pending,
                                        &mut flow.ended,
                                    );
                                }
                            }
                        }
                        Some(Err(e)) => {
                            pending.push_back(Err(BridgeError::Transport(e.to_string())));
                            flow.ended = true;
                        }
                        None => {
                            // 没见到 done 就 EOF：补一条结束帧（上游不保证发 done）
                            pending.push_back(Ok(finish_chunk(
                                &flow.id,
                                &flow.model,
                                flow.pending_usage.take(),
                            )));
                            flow.ended = true;
                        }
                    }
                }
            },
        );

        Ok(stream.boxed())
    }
}

struct Flow {
    id: String,
    model: String,
    ended: bool,
    pending_usage: Option<UsageStats>,
    event: String,
    data: String,
}

/// 喂一行，攒出完整事件时返回 `(event, data)`。
fn scan_line(line: &str, flow: &mut Flow) -> Option<(String, String)> {
    if line.is_empty() {
        if flow.event.is_empty() && flow.data.trim().is_empty() {
            return None;
        }
        let event = std::mem::take(&mut flow.event);
        let data = std::mem::take(&mut flow.data);
        return Some((event, data));
    }
    if let Some(rest) = line.strip_prefix("event:") {
        flow.event = rest.trim().to_string();
    } else if let Some(rest) = line.strip_prefix("data:") {
        // 上游多行 data 直接拼接（参考实现同口径）
        flow.data.push_str(rest);
    }
    None
}

fn apply_event(
    event: &str,
    data: &str,
    id: &str,
    model: &str,
    pending_usage: &mut Option<UsageStats>,
    pending: &mut VecDeque<Result<ChatChunk, BridgeError>>,
    ended: &mut bool,
) {
    if data.trim().is_empty() {
        if event == "done" {
            pending.push_back(Ok(finish_chunk(id, model, pending_usage.take())));
            *ended = true;
        }
        return;
    }
    let Ok(value) = serde_json::from_str::<Value>(data) else {
        return;
    };
    match event {
        "output" => {
            let text = value.get("response").and_then(Value::as_str).unwrap_or("");
            let reasoning = value
                .get("reasoning_content")
                .and_then(Value::as_str)
                .unwrap_or("");
            let calls = value.get("tool_calls").and_then(Value::as_array);
            let mut delta = ChatDelta::default();
            if !text.is_empty() {
                delta.content = Some(text.to_string());
            }
            if !reasoning.is_empty() {
                delta.reasoning = Some(reasoning.to_string());
            }
            if let Some(calls) = calls {
                let parsed: Vec<ToolCallDelta> = calls
                    .iter()
                    .enumerate()
                    .filter_map(|(index, call)| {
                        let function = call.get("function")?;
                        Some(ToolCallDelta {
                            index,
                            id: call.get("id").and_then(Value::as_str).map(str::to_string),
                            function_name: function
                                .get("name")
                                .and_then(Value::as_str)
                                .map(str::to_string),
                            arguments: function.get("arguments").map(|args| {
                                args.as_str()
                                    .map(str::to_string)
                                    .unwrap_or_else(|| args.to_string())
                            }),
                            signature: None,
                        })
                    })
                    .collect();
                if !parsed.is_empty() {
                    delta.tool_calls = Some(parsed);
                }
            }
            if delta.content.is_some() || delta.reasoning.is_some() || delta.tool_calls.is_some() {
                pending.push_back(Ok(ChatChunk {
                    id: id.to_string(),
                    model: model.to_string(),
                    delta,
                    finish_reason: None,
                    usage: None,
                }));
            }
        }
        "token_usage" => {
            *pending_usage = Some(usage_of(&value));
        }
        "done" => {
            let finish = map_finish(
                value
                    .get("finish_reason")
                    .and_then(Value::as_str)
                    .unwrap_or(""),
            );
            pending.push_back(Ok(ChatChunk {
                id: id.to_string(),
                model: model.to_string(),
                delta: ChatDelta::default(),
                finish_reason: Some(finish),
                usage: pending_usage.take(),
            }));
            *ended = true;
        }
        "error" => {
            let code = value
                .get("code")
                .and_then(Value::as_i64)
                .unwrap_or_default();
            let message = value
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("上游错误");
            pending.push_back(Err(BridgeError::upstream_status(
                502,
                format!("Trae 流内错误：[{code}] {message}"),
            )));
            *ended = true;
        }
        _ => {}
    }
}

fn finish_chunk(id: &str, model: &str, usage: Option<UsageStats>) -> ChatChunk {
    ChatChunk {
        id: id.to_string(),
        model: model.to_string(),
        delta: ChatDelta::default(),
        finish_reason: Some(FinishReason::Stop),
        usage,
    }
}

fn map_finish(raw: &str) -> FinishReason {
    if crate::bridge::is_length_finish(raw) {
        return FinishReason::Length;
    }
    match raw.trim().to_ascii_lowercase().as_str() {
        "tool_calls" | "tool-calls" => FinishReason::ToolCalls,
        "content_filter" | "safety" => FinishReason::ContentFilter,
        _ => FinishReason::Stop,
    }
}

fn usage_of(usage: &Value) -> UsageStats {
    let number = |keys: &[&str]| -> u64 {
        keys.iter()
            .find_map(|key| usage.get(*key).and_then(Value::as_i64))
            .unwrap_or(0) as u64
    };
    let prompt = number(&["prompt_tokens", "input_tokens"]);
    let completion = number(&["completion_tokens", "output_tokens"]);
    let total = number(&["total_tokens"]);
    UsageStats {
        prompt_tokens: prompt,
        completion_tokens: completion,
        total_tokens: if total > 0 {
            total
        } else {
            prompt + completion
        },
        cached_prompt_tokens: 0,
        cache_creation_tokens: 0,
        cache_read_tokens: 0,
    }
}

/// ChatFormat → SOLO 信封（白名单重建：只搬上游认的键）。
pub fn build_body(req: &ChatFormat) -> Value {
    let mut messages: Vec<Value> = Vec::new();
    for message in &req.messages {
        let text = message.content_str();
        let role = match message.role {
            Role::System => "system",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
            _ => "user",
        };
        let mut entry = Map::new();
        entry.insert("role".to_string(), json!(role));
        // `content` 必须是**数组**，不能是字符串：Trae 端的 Go 类型是
        // `[]*idecopilot.LLMRawMessageContent`，发字符串会被上游
        // `json: cannot unmarshal string into Go struct field ... of type
        // []*idecopilot.LLMRawMessageContent` 直接 400 掉（线上实测）。
        //
        // ── 空文本**不能省略 content**（2026-11-25 实测修正）──────────
        // 早先这里写成 `if !text.is_empty()`，理由是"空数组更容易被判内容缺失"。
        // 实测正好相反：上游对**缺 content 或空数组**的消息一律回
        // `event:error data:{"code":4001,"message":"We're sorry, the param is invalid…"}`，
        // 而 `content:[{type:text,text:""}]` 能被接受。三种形态都单独打过一遍：
        //   system 无 content   → 4001      system content=[]  → 4001
        //   system 空 text 块   → 200       user   content=[]  → 4001
        //   tool   无 content   → 4001      tool   空 text 块  → 200
        // 客户端历史里"空 system 提示 / 空 tool 结果"极常见，所以这条是线上
        // `[4001] param is invalid` 的真因之一（同一条凭据换写法立即出 token）。
        //
        // 唯一例外是**只带 tool_calls 的 assistant**：上游按纯调用消息处理，
        // 省略 content 与给空 text 块都能过，保留省略以贴近真实客户端形态。
        let tool_calls_only_assistant =
            message.role == Role::Assistant && message.tool_calls.is_some();
        if !text.is_empty() {
            entry.insert(
                "content".to_string(),
                json!([{ "type": "text", "text": text }]),
            );
        } else if !tool_calls_only_assistant {
            entry.insert(
                "content".to_string(),
                json!([{ "type": "text", "text": "" }]),
            );
        }
        if message.role == Role::Tool {
            if let Some(id) = &message.tool_call_id {
                entry.insert("tool_call_id".to_string(), json!(id));
            }
        }
        if let Some(calls) = &message.tool_calls {
            let calls: Vec<Value> = calls
                .iter()
                .map(|call| {
                    json!({
                        "id": call.id,
                        "type": "function",
                        "function_call": {
                            "name": call.function_name,
                            "arguments": call.arguments,
                        },
                    })
                })
                .collect();
            entry.insert("tool_calls".to_string(), Value::Array(calls));
        }
        messages.push(Value::Object(entry));
    }

    let model = req.model.trim();
    let model = if model.is_empty() {
        DEFAULT_MODEL
    } else {
        model
    };
    // `config_name` 必须是 Trae 上游认得的名字（见 [`resolve_config_name`]：全局模型
    // 映射会把请求侧名字改写成 `sn-…` 内部 ID，直接发过去必得 4001）。
    let config_name = resolve_config_name(model);

    let mut out = Map::new();
    out.insert("messages".to_string(), Value::Array(messages));
    out.insert("function".to_string(), json!("solo_work_lite"));
    out.insert("stream".to_string(), json!(true));
    out.insert("config_name".to_string(), json!(config_name));
    // `model` 不参与上游的 config 查表（实测填内部 ID / 别名 / 留空都出 200 + 真回复），
    // 保持调用方原样，免得动到日志与计费的上游口径。
    out.insert("model".to_string(), json!(model));
    if let Some(tools) = &req.tools {
        // SOLO 要求 `function.parameters` 是 JSON 字符串
        let tools: Vec<Value> = tools
            .iter()
            .map(|tool| {
                let mut tool = tool.clone();
                if let Some(parameters) = tool
                    .get_mut("function")
                    .and_then(|function| function.get_mut("parameters"))
                {
                    if parameters.is_object() {
                        let text = parameters.to_string();
                        *parameters = Value::String(text);
                    }
                }
                tool
            })
            .collect();
        out.insert("tools".to_string(), Value::Array(tools));
    }
    // `tool_choice` 必须是**裸字符串**：上游 Go 类型是 `string`，
    // 发 OpenAI 的对象形态（`{"type":"auto"}` / `{"type":"function","function":{"name":…}}`）
    // 会以 `json: cannot unmarshal object into Go struct field
    // LLMUtilsChatRequest.tool_choice of type string` 收场（实测 HTTP 400 + 4001）。
    // 折算规则与 A2A `trae/payload.rs::normalize_tool_choice` 一致：
    //   "none"（不论裸串还是对象）→ 不发 tool_choice，并连带删掉 tools；
    //   auto / required       → 同名裸串；
    //   {type:function,name}  → 函数名裸串（缺名回 auto）；
    //   其它形态               → 不发 tool_choice（不猜）。
    if let Some(choice) = &req.tool_choice {
        let mut suppress_tools = false;
        let mut out_choice: Option<String> = None;
        match choice {
            Value::String(name) => {
                if name.trim().eq_ignore_ascii_case("none") {
                    suppress_tools = true;
                } else {
                    out_choice = Some(name.trim().to_string());
                }
            }
            Value::Object(wrapper) => {
                let kind = wrapper
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .trim()
                    .to_lowercase();
                match kind.as_str() {
                    "none" => suppress_tools = true,
                    "auto" | "required" => out_choice = Some(kind),
                    "function" => {
                        let name = wrapper
                            .get("function")
                            .and_then(|function| function.get("name"))
                            .and_then(Value::as_str)
                            .or_else(|| wrapper.get("name").and_then(Value::as_str))
                            .unwrap_or_default()
                            .trim()
                            .to_string();
                        out_choice = Some(if name.is_empty() {
                            "auto".to_string()
                        } else {
                            name
                        });
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        if suppress_tools {
            out.remove("tools");
        } else if let Some(choice) = out_choice {
            out.insert("tool_choice".to_string(), json!(choice));
        }
    }
    if let Some(temperature) = req.temperature {
        out.insert("temperature".to_string(), json!(temperature));
    }
    if let Some(top_p) = req.top_p {
        out.insert("top_p".to_string(), json!(top_p));
    }
    out.insert(
        "max_tokens".to_string(),
        json!(req.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS as u32)),
    );
    if let Some(effort) = &req.reasoning_effort {
        let level = effort.trim().to_lowercase();
        if !matches!(level.as_str(), "" | "auto" | "none" | "off") {
            out.insert("reasoning_effort".to_string(), json!(effort));
        }
    }
    Value::Object(out)
}

fn head(text: &str) -> String {
    text.chars().take(240).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(text: &str) -> ChatMessage {
        ChatMessage {
            role: Role::User,
            content: Some(text.to_string()),
            content_blocks: None,
            name: None,
            tool_call_id: None,
            tool_calls: None,
            reasoning: None,
        }
    }

    fn format(messages: Vec<ChatMessage>) -> ChatFormat {
        ChatFormat {
            model: "glm-5.2".into(),
            messages,
            tools: None,
            max_tokens: None,
            temperature: None,
            top_p: None,
            stream: true,
            top_k: None,
            stop: None,
            tool_choice: None,
            reasoning_effort: None,
            web_search_options: None,
            extra: None,
        }
    }

    #[test]
    fn body_uses_solo_whitelist() {
        let body = build_body(&format(vec![user("你好")]));
        assert_eq!(body["function"], "solo_work_lite");
        assert_eq!(body["stream"], true);
        assert_eq!(body["config_name"], "glm-5.2");
        assert_eq!(body["messages"][0]["role"], "user");
        assert!(body.get("top_k").is_none());
    }

    /// `content` 必须是数组（线上实测：发字符串被 Trae 以 4001 拒掉，
    /// 报 `cannot unmarshal string into ... []*idecopilot.LLMRawMessageContent`）。
    #[test]
    fn message_content_is_array_of_text_parts() {
        let body = build_body(&format(vec![user("你好")]));
        let content = &body["messages"][0]["content"];
        assert!(content.is_array(), "content 必须是数组: {content}");
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "你好");
    }

    /// 空文本**必须**发空 text 块，不能省略 content（2026-11-25 实测）。
    ///
    /// 上游对「缺 content / 空数组」的消息回流内 4001（system、user、tool 三种角色
    /// 都实测过），而 `content:[{type:text,text:""}]` 能被接受 —— 客户端历史里的
    /// 空 system 提示与空 tool 结果因此成了线上 `[4001] param is invalid` 的来源。
    #[test]
    fn empty_text_still_sends_an_empty_text_part() {
        for role in [Role::System, Role::User, Role::Tool] {
            let mut message = user("");
            message.role = role;
            let body = build_body(&format(vec![message]));
            let content = &body["messages"][0]["content"];
            assert!(
                content.is_array(),
                "空文本也要给数组形态的 content: {content}"
            );
            assert_eq!(content[0]["type"], "text");
            assert_eq!(content[0]["text"], "");
        }
    }

    /// 只带 tool_calls 的 assistant 省略 content（上游按纯调用消息处理，两种形态都行）。
    #[test]
    fn tool_calls_only_assistant_omits_content() {
        let mut message = ChatMessage::assistant("");
        message.tool_calls = Some(vec![ToolCall {
            id: "c1".into(),
            function_name: "read_file".into(),
            arguments: "{}".into(),
        }]);
        let body = build_body(&format(vec![message]));
        assert!(
            body["messages"][0].get("content").is_none(),
            "纯 tool_calls 消息不该被补空 text 块: {}",
            body["messages"][0]
        );
        assert_eq!(
            body["messages"][0]["tool_calls"][0]["function_call"]["name"],
            "read_file"
        );
    }

    /// `tool_choice` 必须折成裸字符串；对象形态会被上游按类型错误拒掉（4001）。
    #[test]
    fn tool_choice_is_flattened_to_a_bare_string() {
        let cases: [(Value, Option<&str>, bool); 5] = [
            (json!({"type": "auto"}), Some("auto"), true),
            (json!({"type": "required"}), Some("required"), true),
            (
                json!({"type": "function", "function": {"name": "read_file"}}),
                Some("read_file"),
                true,
            ),
            (json!({"type": "function"}), Some("auto"), true),
            (json!({"type": "none"}), None, false),
        ];
        for (choice, want, tools_kept) in cases {
            let mut req = format(vec![user("hi")]);
            req.tools = Some(vec![json!({
                "type": "function",
                "function": {"name": "read_file", "parameters": {"type": "object"}}
            })]);
            req.tool_choice = Some(choice.clone());
            let body = build_body(&req);
            match want {
                Some(text) => assert_eq!(body["tool_choice"], json!(text), "choice={choice}"),
                None => assert!(body.get("tool_choice").is_none(), "choice={choice}"),
            }
            assert_eq!(body.get("tools").is_some(), tools_kept, "choice={choice}");
        }
    }

    /// 裸字符串形态原样透传（含函数名）。
    #[test]
    fn bare_tool_choice_string_survives() {
        let mut req = format(vec![user("hi")]);
        req.tool_choice = Some(json!("read_file"));
        assert_eq!(build_body(&req)["tool_choice"], "read_file");
    }

    /// `config_name` 必须是 Trae 认得的真名 —— 全局模型映射把请求侧名字改写成
    /// `sn-…` 内部 ID 时（线上那条是 `sn-deepseek-v4-1-flash-1017`），直接发过去
    /// 必得流内 `[4001] the param is invalid`，而 failover 会把它当「账号坏了」，
    /// 把好账号一个个打进冷却。
    ///
    /// 改前行为（可区分性）：`config_name` 直接取 `req.model`，`sn-…` 原样发出 → 4001。
    #[test]
    fn config_name_is_normalized_back_from_internal_ids() {
        // 线上实际被映射出来的那个内部 ID → 必须还原成上游真名
        for raw in [
            "sn-deepseek-v4-1-flash-1017",
            "deepseek-v4-1-flash-1017",
            "sn-glm-5-3-flash",
        ] {
            let mut req = format(vec![user("hi")]);
            req.model = raw.to_string();
            let body = build_body(&req);
            let sent = body["config_name"].as_str().unwrap_or("");
            assert!(
                TRAE_CONFIG_NAMES.contains(&sent),
                "config_name 必须落在上游清单里，实际 {sent:?}（输入 {raw:?}）"
            );
            assert_ne!(sent, raw, "内部 ID 不得原样发给上游：{raw:?}");
        }
    }

    /// 正常名字逐字节不变（零涟漪）：`deepseek-v4.1-flash` / `DeepSeek-V4-Pro`
    /// 这些线上实测能过的一律原样，不做任何改写。
    #[test]
    fn a_real_config_name_passes_through_untouched() {
        for raw in [
            "deepseek-v4.1-flash",
            "DeepSeek-V4-Pro",
            "glm-5.3-flash",
            "qwen4-max",
            "step-5-preview",
        ] {
            let mut req = format(vec![user("hi")]);
            req.model = raw.to_string();
            assert_eq!(
                build_body(&req)["config_name"],
                json!(raw),
                "{raw:?} 是上游真名，不该被改写"
            );
        }
    }

    /// `model` 字段不参与上游 config 查表（实测填内部 ID / 别名 / 留空都出真回复），
    /// 所以只归一 `config_name`，`model` 保持调用方原样 —— 日志与计费口径不变。
    #[test]
    fn model_field_keeps_the_callers_value() {
        let mut req = format(vec![user("hi")]);
        req.model = "sn-deepseek-v4-1-flash-1017".to_string();
        let body = build_body(&req);
        assert_eq!(body["model"], "sn-deepseek-v4-1-flash-1017");
        assert_eq!(body["config_name"], "deepseek-v4.1-flash");
    }

    /// 大小写不敏感兜底：`GLM-5.3` 能落到 `glm-5.3`（上游大小写敏感，实测
    /// `DeepSeek-V4.1-Flash` 4001 而 `deepseek-v4.1-flash` 200）。
    #[test]
    fn case_insensitive_fallback_maps_to_the_listed_spelling() {
        let mut req = format(vec![user("hi")]);
        req.model = "GLM-5.3".to_string();
        assert_eq!(build_body(&req)["config_name"], "glm-5.3");

        // 但**不是**任何大小写都改：`DeepSeek-V4.1-Flash` 上游没有这个拼法，
        // 表里只有小写 `deepseek-v4.1-flash`，兜底会把它折过去（避免 4001）。
        let mut req = format(vec![user("hi")]);
        req.model = "DeepSeek-V4.1-Flash".to_string();
        assert_eq!(build_body(&req)["config_name"], "deepseek-v4.1-flash");
    }

    /// 认不出的名字**原样发出**并告警，不静默换成别的模型：
    /// 换模型等于「用户要 A 却拿到 B」，错模型答得越顺越没人发现。
    #[test]
    fn an_unknown_name_is_sent_as_is_rather_than_silently_replaced() {
        let mut req = format(vec![user("hi")]);
        req.model = "totally-made-up-model".to_string();
        assert_eq!(
            build_body(&req)["config_name"],
            "totally-made-up-model",
            "认不出的名字不该被替换成默认模型"
        );
    }

    /// 空模型名回落 `DEFAULT_MODEL`（既有契约），且该默认值本身必须在上游清单里。
    #[test]
    fn empty_model_falls_back_to_a_name_trae_actually_knows() {
        let mut req = format(vec![user("hi")]);
        req.model = "   ".to_string();
        let body = build_body(&req);
        assert_eq!(body["config_name"], DEFAULT_MODEL);
        assert!(
            TRAE_CONFIG_NAMES.contains(&DEFAULT_MODEL),
            "DEFAULT_MODEL 必须是上游认得的 config_name，否则空模型请求必 4001"
        );
    }

    #[test]
    fn output_event_becomes_delta() {
        let mut pending = VecDeque::new();
        let mut usage = None;
        let mut ended = false;
        apply_event(
            "output",
            r#"{"response":"你","reasoning_content":"想"}"#,
            "x",
            "m",
            &mut usage,
            &mut pending,
            &mut ended,
        );
        match pending.pop_front() {
            Some(Ok(chunk)) => {
                assert_eq!(chunk.delta.content.as_deref(), Some("你"));
                assert_eq!(chunk.delta.reasoning.as_deref(), Some("想"));
            }
            _ => panic!("output 应产出增量"),
        }
    }

    #[test]
    fn done_event_finishes_with_pending_usage() {
        let mut pending = VecDeque::new();
        let mut usage = None;
        let mut ended = false;
        apply_event(
            "token_usage",
            r#"{"prompt_tokens":3,"completion_tokens":2}"#,
            "x",
            "m",
            &mut usage,
            &mut pending,
            &mut ended,
        );
        apply_event(
            "done",
            r#"{"finish_reason":"stop"}"#,
            "x",
            "m",
            &mut usage,
            &mut pending,
            &mut ended,
        );
        match pending.pop_front() {
            Some(Ok(chunk)) => {
                assert_eq!(chunk.finish_reason, Some(FinishReason::Stop));
                assert_eq!(chunk.usage.map(|u| u.prompt_tokens), Some(3));
            }
            _ => panic!("done 应产出结束帧"),
        }
        assert!(ended);
    }
}
