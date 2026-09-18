//! 公开提示词源拉取（P1 前端体验：Prompts 页一键导入社区提示词）。
//!
//! 三个精选源（均为 GitHub 公开仓库，`fetch` 时实时抓取）：
//! - `prompts.chat`：`prompts.csv`（act,prompt 两列，约 5.7MB）
//! - `awesome-prompts`：`prompts/*.txt|md`（文件名即标题，正文即内容）
//! - `big-prompt-library`：`CustomInstructions/ChatGPT/*.md`（文件名含 GPT 名）
//!
//! 安全：只 GET 固定 URL；正文内容进文件后原样透传（前端渲染时
//! 由 React 转义防 XSS）。不落地持久化，走 5 分钟内存缓存。

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use futures::StreamExt;
use serde_json::{json, Value};
use tokio::sync::{Mutex as TokioMutex, RwLock};

use super::super::openai::AppState;
use super::common::{error_response, verify_user};

/// 单个源元信息
const SOURCES: &[(&str, &str, &str, &str)] = &[
    (
        "prompts-chat",
        "prompts.chat",
        "全球最大开源提示词库（CSV 约 300+ 条，变量模板生态）",
        "f/prompts.chat",
    ),
    (
        "awesome-prompts",
        "ai-boost/awesome-prompts",
        "开发者/Agent 系统提示词军火库（400+ 硬核角色）",
        "ai-boost/awesome-prompts",
    ),
    (
        "big-prompt-library",
        "The Big Prompt Library",
        "大厂系统提示词 / 1600+ 自定义 GPT 指令研究库",
        "0xeb/TheBigPromptLibrary",
    ),
];

/// 单源缓存条目：`(抓取时间戳, 源数据 Arc, 是否截断)`。
/// 源数据存 `Arc<Value>`：命中时仅复制引用计数（O(1)），不再深拷贝
/// 数 MB 的 JSON 树；缓存与响应体共享同一底层 `Value`。
/// `truncated` 是数据本身的属性（导入时因体积闸丢弃过尾部条目），必须随
/// 缓存一起返回，否则 5 分钟内的缓存命中会丢失「内容已截断」提示。
type SourceEntry = (i64, Arc<Value>, bool);

/// 内存缓存：key → 单源缓存条目。
pub struct PromptSourceCache {
    map: Arc<RwLock<HashMap<String, SourceEntry>>>,
    /// 按 key 的在途抓取闸门：防止同一源被并发触发多次 GitHub 抓取。
    inflight: Arc<TokioMutex<HashMap<String, ()>>>,
    /// 自环翻译每日字符预算：`(当日 YYYYMMDD, 已用字符数)`。
    /// 跨天自动清零；单次锁内完成检查+累加，保证原子不超卖。
    daily_budget: Arc<TokioMutex<(String, u64)>>,
}

impl PromptSourceCache {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Default for PromptSourceCache {
    fn default() -> Self {
        Self {
            map: Arc::new(RwLock::new(HashMap::new())),
            inflight: Arc::new(TokioMutex::new(HashMap::new())),
            daily_budget: Arc::new(TokioMutex::new((String::new(), 0))),
        }
    }
}

const CACHE_TTL_SECS: i64 = 300;

async fn hit(cache: &PromptSourceCache, key: &str) -> Option<(Arc<Value>, bool)> {
    // 两段式：命中走读锁快速路径（共享锁，多个并发命中互不阻塞，克隆
    // 的是 `Arc` 引用计数——O(1)，不再深拷贝底层 JSON），只有过期/缺失
    // 才升写锁清理过期垃圾。
    {
        let map = cache.map.read().await;
        if let Some((ts, v, truncated)) = map.get(key) {
            if chrono::Utc::now().timestamp() - *ts < CACHE_TTL_SECS {
                return Some((Arc::clone(v), *truncated));
            }
        }
    }
    // 过期：升写锁 double-check 后 remove。double-check 防并发 store 已
    // 刷新成新值——此时不删，避免误删刚抓取的缓存。
    let mut map = cache.map.write().await;
    if let Some((ts, _, _)) = map.get(key) {
        if chrono::Utc::now().timestamp() - *ts >= CACHE_TTL_SECS {
            map.remove(key);
        }
    }
    None
}

async fn store(cache: &PromptSourceCache, key: &str, v: Arc<Value>, truncated: bool) {
    let mut map = cache.map.write().await;
    map.insert(key.to_string(), (chrono::Utc::now().timestamp(), v, truncated));
}

/// 抓取闸门：同一 key 正在抓取时返回 `None`（调用方直接放行，等已发起的
/// 请求自己落缓存），否则占位返回守卫。守卫 Drop 时释放占位，保证后续
/// 请求（含失败后重试）不被永久卡死。
struct FetchGuard {
    inflight: Arc<TokioMutex<HashMap<String, ()>>>,
    key: String,
}

impl Drop for FetchGuard {
    fn drop(&mut self) {
        // 同步 Drop 无法 await，用 try_lock 尽力清理。极端竞争下若锁被
        // 短暂占用，此处可能漏删——但占位仅是"在途"标记，下一次请求会
        // 走正常抓取路径重新建占位，不会形成永久死锁。
        if let Ok(mut m) = self.inflight.try_lock() {
            m.remove(&self.key);
        }
    }
}

async fn begin_fetch(cache: &PromptSourceCache, key: &str) -> Option<FetchGuard> {
    let mut m = cache.inflight.lock().await;
    if m.contains_key(key) {
        None
    } else {
        m.insert(key.to_string(), ());
        Some(FetchGuard {
            inflight: cache.inflight.clone(),
            key: key.to_string(),
        })
    }
}

/// GET /api/prompts/sources — 列出可拉取的公开源（登录用户即可）。
pub async fn handle_prompt_sources(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _user = verify_user(&state, &headers).await?;
    let data: Vec<Value> = SOURCES
        .iter()
        .map(|(id, name, desc, repo)| {
            json!({ "id": id, "name": name, "description": desc, "repo": repo })
        })
        .collect();
    Ok(Json(json!({ "success": true, "data": data })))
}

/// POST /api/prompts/fetch — 抓取指定源并返回提示词数组。
///
/// 鉴权：必须登录。该端点会触发服务端向 GitHub 发起数百个并发抓取请求，
/// 未鉴权将沦为免费代理与 DoS 放大器（借 AIGX 服务器压 GitHub API 配额）。
pub async fn handle_prompt_fetch(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _user = verify_user(&state, &headers).await?;
    let id = body
        .get("source")
        .and_then(|s| s.as_str())
        .unwrap_or_default();
    if id.is_empty() {
        return Err(error_response(
            "source is required",
            StatusCode::BAD_REQUEST,
        ));
    }
    let key = format!("prompt_source:{id}");
    if let Some((v, truncated)) = hit(&state.prompt_source_cache, &key).await {
        // 命中缓存：`v` 是 `Arc<Value>`，`&*v` 借用底层 JSON 序列化，零深拷贝。
        // `truncated` 随缓存一起返回，保证缓存命中窗口内提示不丢失。
        return Ok(Json(json!({ "success": true, "truncated": truncated, "data": &*v })));
    }
    // 同一源已在途抓取：返回 409，告知前端「正在抓取，稍后重试」。
    // 这比返回空数组更明确——空数组会被前端误判为「该源无可用提示词」。
    // `_guard` 必须绑定存活到本函数末尾，其 Drop 才在 fetch 完成后释放
    // 占位；若写成 `.is_none()` 临时值，guard 会被立即 Drop、闸门失效。
    let _guard = match begin_fetch(&state.prompt_source_cache, &key).await {
        Some(g) => g,
        None => {
            return Err(error_response(
                "该源正在抓取中，请稍后重试",
                StatusCode::CONFLICT,
            ))
        }
    };

    let client = state.http_client.clone();
    // 先校验 source id，再构造「可被整体超时包裹」的抓取 Future。
    // 三个 async fn 的返回 Future 是互不相同的 opaque type，需 Box 统一
    // 到 `Pin<Box<dyn Future<Output=...>>>` 才能放进同一 match 分支。
    let fetch_fut: std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<PromptList, String>> + Send>,
    > = match id {
        "prompts-chat" => Box::pin(fetch_prompts_chat(&client)),
        "awesome-prompts" => Box::pin(fetch_awesome_prompts(&client)),
        "big-prompt-library" => Box::pin(fetch_big_prompt_library(&client)),
        _ => return Err(error_response("unknown source", StatusCode::NOT_FOUND)),
    };
    // 整源抓取总超时：三个源都可能因上游 GitHub 慢/挂起拖住请求，
    // 尤其是逐文件并发抓取的 awesome-prompts/big-prompt-library。
    // 超时即失败，在途闸门随守卫 Drop 释放，不会残留占位。
    let result = match tokio::time::timeout(std::time::Duration::from_secs(30), fetch_fut).await {
        Ok(r) => r,
        Err(_) => {
            return Err(error_response(
                "拉取公开源超时，请稍后重试",
                StatusCode::GATEWAY_TIMEOUT,
            ))
        }
    };

    let mut data = result
        .map_err(|e| error_response(&format!("拉取公开源失败：{e}"), StatusCode::BAD_GATEWAY))?;
    // 逐源截断到 MAX_FETCH_ITEMS（并发抓取已在 fetch 函数内 truncate，
    // 这里再兜底一次，覆盖未来新增源忘截断的情况）。
    data.truncate(MAX_FETCH_ITEMS);
    // 逐条再次裁剪内容到 MAX_CONTENT_CHARS（fetch 函数已裁剪，此处为
    // 二次防线，防止个别源绕过 entry() 直接构造超长条目）。
    for item in &mut data {
        if let Some(content) = item.get("content").and_then(|c| c.as_str()) {
            if content.chars().count() > MAX_CONTENT_CHARS {
                let truncated: String = content.chars().take(MAX_CONTENT_CHARS).collect();
                item["content"] = json!(format!("{truncated}\n…（内容过长，已截断）"));
            }
        }
    }
    // 按 UTF-16 码元估算（localStorage 配额口径）做最终体积闸：非 ASCII
    // 字符占 1 个码元，但实际编码为 2 字节；用「每字符按 2 码元」的保守
    // 上限，超阈值即丢弃尾部条目并置 truncated 标记。防止 UTF-8 体积看似
    // 安全（如 awesome-prompts 2.96MB）实则 UTF-16 触达 5MB 配额而静默丢失。
    let mut truncated = false;
    let mut utf16_est: usize = 0;
    data.retain(|item| {
        if truncated {
            return false;
        }
        let content = item.get("content").and_then(|c| c.as_str()).unwrap_or("");
        utf16_est += content.chars().count() * 2;
        if utf16_est > MAX_SOURCE_UTF16_ESTIMATE {
            truncated = true;
            return false;
        }
        true
    });
    // 冷路径最后一次深拷贝的消除：data 只搬一次进 Arc（不 clone），
    // 缓存与响应体共享同一底层数组；响应经 `&*v` 借用序列化，零复制。
    let v = Arc::new(Value::Array(data));
    store(&state.prompt_source_cache, &key, Arc::clone(&v), truncated).await;
    Ok(Json(json!({
        "success": true,
        "truncated": truncated,
        "data": &*v,
    })))
}

type PromptList = Vec<Value>;

/// 单次翻译请求的提示词数量上限：批量翻译自环调用成本高，
/// 且公开源拉取一次可产生数百条，翻译全部会触发海量 LLM 调用。
/// 前端按批次切片（默认 20），单批超过上限时后端直接拒绝。
const MAX_TRANSLATE_BATCH: usize = 40;

/// 自环翻译每日字符预算（按翻译源文本字符数计，跨天重置）。
///
/// 拉取 awesome-prompts（377 条）或 big-prompt-library（200 条）时，
/// 若开启自动翻译会触发数百次自环 LLM 调用，消耗 `[agent]` 配置渠道的
/// 真实配额。这里是成本保护的硬闸：单日累计超过预算即拒绝新翻译，
/// 防止一次误点把自环渠道配额烧穿。1.2M 字符约对应 300 条 × 4KB 提示词。
const DAILY_TRANSLATE_CHAR_BUDGET: u64 = 1_200_000;

/// 从当日预算中申请 `chars` 个字符。返回 `true` 表示批准（并已原子累加），
/// `false` 表示超出预算（跨天时自动清零重新计）。
async fn reserve_translate_budget(cache: &PromptSourceCache, chars: u64) -> bool {
    let mut g = cache.daily_budget.lock().await;
    let today = chrono::Utc::now().format("%Y%m%d").to_string();
    if g.0 != today {
        g.0 = today;
        g.1 = 0;
    }
    if g.1.saturating_add(chars) > DAILY_TRANSLATE_CHAR_BUDGET {
        return false;
    }
    g.1 += chars;
    true
}

/// 翻译目标语言（前端下拉选项，值即 system prompt 中的语言名）。
const TRANSLATE_TARGETS: &[&str] = &["简体中文", "English", "日本語", "한국어"];

/// 判断文本是否需要翻译（保守：仅当出现非 ASCII 且含英文字母时视为英文）。
///
/// 中文标题（如本地自建提示词）应直接跳过，避免无谓的 LLM 调用。
fn looks_english(text: &str) -> bool {
    let has_letter = text.chars().any(|c| c.is_ascii_alphabetic());
    let has_cjk = text.chars().any(|c| {
        ('\u{4e00}'..='\u{9fff}').contains(&c)
            || ('\u{3040}'..='\u{30ff}').contains(&c)
            || ('\u{ac00}'..='\u{d7af}').contains(&c)
    });
    has_letter && !has_cjk
}

/// POST /api/prompts/translate — 自环翻译提示词（登录即可）。
///
/// 复用 AI 运维 Agent 的进程内推理（`llm::chat_once`）走 AIGX 自己的渠道，
/// 把英文提示词翻译成目标语言。与客户请求的本质区别：不经 HTTP 端口、
/// 不计费。翻译失败（如 `[agent]` 未配置模型/渠道）返回 503 并提示原因。
///
/// 鉴权：登录用户即可（翻译走自环，与数据面计费无关）。
pub async fn handle_prompt_translate(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _user = verify_user(&state, &headers).await?;

    // 空数组直接返回，避免无谓的模型/预算校验与自环调用。
    let items = body
        .get("items")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if items.is_empty() {
        return Ok(Json(
            json!({ "success": true, "data": { "translated": [] } }),
        ));
    }
    if items.len() > MAX_TRANSLATE_BATCH {
        return Err(error_response(
            &format!("单次翻译最多 {MAX_TRANSLATE_BATCH} 条"),
            StatusCode::BAD_REQUEST,
        ));
    }

    let agent = state.agent_state.as_deref().ok_or_else(|| {
        error_response(
            "自环翻译未启用：请先在配置中启用 [agent] 并设置 model/channel",
            StatusCode::SERVICE_UNAVAILABLE,
        )
    })?;
    let model = agent.config.model.trim().to_string();
    if model.is_empty() {
        return Err(error_response(
            "自环翻译未启用：请先在配置中启用 [agent] 并设置 model/channel",
            StatusCode::SERVICE_UNAVAILABLE,
        ));
    }

    let target = body
        .get("target")
        .and_then(|v| v.as_str())
        .unwrap_or("简体中文")
        .trim()
        .to_string();
    if !TRANSLATE_TARGETS.contains(&target.as_str()) {
        return Err(error_response(
            &format!("不支持的目标语言：{target}"),
            StatusCode::BAD_REQUEST,
        ));
    }

    // 单条内容上限：防止 14 万字符的巨型提示词被原样送入自环翻译，
    // 既可能烧穿每日字符预算，也会让单次 LLM 调用上下文过大而失败。
    if let Some(too_long) = items.iter().find(|it| {
        it.get("content")
            .and_then(|c| c.as_str())
            .is_some_and(|s| s.chars().count() > MAX_CONTENT_CHARS)
    }) {
        let len = too_long
            .get("content")
            .and_then(|c| c.as_str())
            .map(|s| s.chars().count())
            .unwrap_or(0);
        return Err(error_response(
            &format!("单条内容超长（{len} 字符，上限 {MAX_CONTENT_CHARS}）"),
            StatusCode::BAD_REQUEST,
        ));
    }

    let system = format!(
        "你是专业翻译。把用户给的每个提示词内容准确翻译成{target}，\
         保持原意、语气、格式与代码/变量原样。只输出翻译后的文本，\
         不要解释、不要添加引号或任何额外内容。"
    );

    // 只翻译疑似英文的条目，跳过空文本与非英文（中文标题等）。
    let pending: Vec<(usize, String)> = items
        .iter()
        .enumerate()
        .filter_map(|(i, item)| {
            let src = item.get("content").and_then(|v| v.as_str())?.trim();
            if src.is_empty() || !looks_english(src) {
                return None;
            }
            Some((i, src.to_string()))
        })
        .collect();

    // 每日字符预算：整批一次性申请，不足则整体拒绝（不做半批）。这样调用方
    // 会得到一个明确的可重试失败，而不是悄悄翻译一部分、另一部分丢失。
    let total_chars: u64 = pending.iter().map(|(_, s)| s.chars().count() as u64).sum();
    if !reserve_translate_budget(&state.prompt_source_cache, total_chars).await {
        return Err(error_response(
            "今日自环翻译预算已用尽，请明日再试（避免一次拉取烧穿自环渠道配额）",
            StatusCode::TOO_MANY_REQUESTS,
        ));
    }

    // 逐条并发自环翻译（并发度 4），单条 60s 超时，失败/超时条目静默跳过
    // （保持部分成功），避免某条卡死拖垮整个批量请求。
    // AppState/AgentConfig 均为 Arc 包裹字段的轻量 Clone，逐条克隆进 async 块
    // 以消除对函数局部引用生命周期的依赖。
    let state_owned = state.clone();
    let config_owned = agent.config.clone();
    let results = futures::stream::iter(pending)
        .map(|(i, src)| {
            let system = system.clone();
            let state_owned = state_owned.clone();
            let config_owned = config_owned.clone();
            async move {
                let convo = vec![
                    crate::agent::llm::system_message(system),
                    crate::agent::llm::user_message(src),
                ];
                let fut = crate::agent::llm::chat_once(&state_owned, &config_owned, convo, None);
                match tokio::time::timeout(std::time::Duration::from_secs(60), fut).await {
                    Ok(Ok((resp, _, _))) => {
                        let text = resp.message.content.unwrap_or_default().trim().to_string();
                        // 兜底：译文仍是纯英文时视为翻译失败，丢弃该条保留原文。
                        if looks_english(&text) {
                            None
                        } else {
                            Some((i, text))
                        }
                    }
                    _ => None,
                }
            }
        })
        .buffer_unordered(4)
        .collect::<Vec<_>>()
        .await;

    let mut out = Vec::with_capacity(results.len());
    for (i, text) in results.into_iter().flatten() {
        out.push(json!({ "index": i, "content": text }));
    }
    // 按原始 index 升序返回，前端可直接下标定位。
    out.sort_by_key(|v| v["index"].as_u64().unwrap_or(0));
    Ok(Json(
        json!({ "success": true, "data": { "translated": out } }),
    ))
}

/// 单条内容上限：公开源里存在 14 万字符的巨型提示词，前端卡片渲染会卡顿，
/// 且 localStorage 有约 5MB 配额，必须裁剪以控制单源体积。
const MAX_CONTENT_CHARS: usize = 20_000;

/// 单源最大条目数：prompts.chat 实测 2169 条、JSON 序列化约 5.4MB，逼近
/// localStorage 约 5MB 配额——一次性导入会触发 savePrompts 静默失败，用户
/// 看到「新增 2169 条」但刷新后全部丢失。按条目数截断到 800 条，JSON 体积
/// 控制在约 2MB，安全落在配额内；800 条对「参考提示词」场景也已足够。
const MAX_FETCH_ITEMS: usize = 800;

/// 单源最大 UTF-16 字节估算：localStorage 配额按 UTF-16 码元计（Chrome 约
/// 5M 码元）。awesome-prompts 377 条实测 UTF-8 约 2.96MB、但 UTF-16 估约
/// 5.6MB——单按 UTF-8 体积判断会误判「安全」，实际导入仍触达配额导致
/// savePrompts 静默失败。超过该阈值即在响应中丢弃超限条目（返回时带
/// `truncated` 标记），把单源稳定压在约 3MB UTF-16 以内。
const MAX_SOURCE_UTF16_ESTIMATE: usize = 3_000_000;

fn entry(name: &str, content: &str, tags: &[&str], source: &str) -> Value {
    let content = if content.chars().count() > MAX_CONTENT_CHARS {
        let truncated: String = content.chars().take(MAX_CONTENT_CHARS).collect();
        format!("{truncated}\n…（内容过长，已截断）")
    } else {
        content.to_string()
    };
    json!({
        "name": name,
        "content": content,
        "tags": tags,
        "source": source,
    })
}

async fn fetch_prompts_chat(client: &reqwest::Client) -> Result<PromptList, String> {
    const URL: &str = "https://raw.githubusercontent.com/f/prompts.chat/main/prompts.csv";
    let body = client
        .get(URL)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .text()
        .await
        .map_err(|e| e.to_string())?;

    let mut rdr = csv::Reader::from_reader(body.as_bytes());
    let mut out = Vec::new();
    for rec in rdr.records() {
        let rec = match rec {
            Ok(r) => r,
            Err(_) => continue,
        };
        if rec.len() < 2 {
            continue;
        }
        let name = rec.get(0).unwrap_or("").trim().to_string();
        let content = rec.get(1).unwrap_or("").trim().to_string();
        if name.is_empty() || content.is_empty() || name == "act" {
            continue;
        }
        out.push(entry(&name, &content, &["prompts.chat"], "prompts-chat"));
        // 达到单源条目上限即停止解析，避免 5.4MB 的完整 CSV 全部进响应与
        // 前端 localStorage（会触达配额上限导致静默丢失）。
        if out.len() >= MAX_FETCH_ITEMS {
            break;
        }
    }
    if out.is_empty() {
        return Err("prompts.csv 无有效数据".to_string());
    }
    Ok(out)
}

async fn fetch_awesome_prompts(client: &reqwest::Client) -> Result<PromptList, String> {
    const BASE: &str = "https://api.github.com/repos/ai-boost/awesome-prompts/contents/prompts";
    let body = client
        .get(BASE)
        .header("User-Agent", "AIGX")
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .text()
        .await
        .map_err(|e| e.to_string())?;
    let list: Value = serde_json::from_str(&body).map_err(|e| e.to_string())?;
    let items = list.as_array().ok_or_else(|| "目录结构异常".to_string())?;

    // 先收集待下载的 (标题, download_url) 对，再并发抓取正文。
    // 单文件失败静默跳过（保持部分成功），避免一条 404/超时拖垮整个源。
    let pending: Vec<(String, String)> = items
        .iter()
        .filter_map(|item| {
            let name = item.get("name").and_then(|n| n.as_str()).unwrap_or("");
            if !(name.ends_with(".txt") || name.ends_with(".md")) {
                return None;
            }
            let dl = item
                .get("download_url")
                .and_then(|u| u.as_str())?
                .to_string();
            let title = name
                .trim_end_matches(".txt")
                .trim_end_matches(".md")
                .replace('_', " ");
            Some((title, dl))
        })
        .collect();

    let client = client.clone();
    let results = futures::stream::iter(pending)
        .map(|(title, dl)| {
            let client = client.clone();
            async move {
                let resp = client
                    .get(&dl)
                    .header("User-Agent", "AIGX")
                    .send()
                    .await
                    .ok()?;
                let content = resp.error_for_status().ok()?.text().await.ok()?;
                if content.trim().is_empty() {
                    return None;
                }
                Some(entry(
                    &title,
                    content.trim(),
                    &["awesome-prompts"],
                    "awesome-prompts",
                ))
            }
        })
        .buffer_unordered(8)
        .collect::<Vec<_>>()
        .await;

    let mut out: Vec<Value> = results.into_iter().flatten().collect();
    if out.is_empty() {
        return Err("awesome-prompts 无有效数据".to_string());
    }
    // 统一单源条目上限（并发抓取无法中途 break，收齐后截断）。
    out.truncate(MAX_FETCH_ITEMS);
    Ok(out)
}

async fn fetch_big_prompt_library(client: &reqwest::Client) -> Result<PromptList, String> {
    const TREE: &str =
        "https://api.github.com/repos/0xeb/TheBigPromptLibrary/git/trees/main?recursive=1";
    let body = client
        .get(TREE)
        .header("User-Agent", "AIGX")
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .text()
        .await
        .map_err(|e| e.to_string())?;
    let tree: Value = serde_json::from_str(&body).map_err(|e| e.to_string())?;
    let paths: Vec<String> = tree
        .get("tree")
        .and_then(|t| t.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|t| t.get("path").and_then(|p| p.as_str()))
                .filter(|p| p.starts_with("CustomInstructions/ChatGPT/") && p.ends_with(".md"))
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default();

    if paths.is_empty() {
        return Err("TheBigPromptLibrary 无有效数据".to_string());
    }
    // 上限：只抓前 200 个（库体量巨大，避免单次请求过重）。
    // 并发抓取，单文件失败静默跳过，避免单条超时拖垮整个源。
    let client = client.clone();
    let results = futures::stream::iter(paths.iter().take(200).cloned())
        .map(|path| {
            let client = client.clone();
            async move {
                let url = format!(
                    "https://raw.githubusercontent.com/0xeb/TheBigPromptLibrary/main/{path}"
                );
                let resp = client
                    .get(&url)
                    .header("User-Agent", "AIGX")
                    .send()
                    .await
                    .ok()?;
                let content = resp.error_for_status().ok()?.text().await.ok()?;
                if content.trim().is_empty() {
                    return None;
                }
                let name = path
                    .rsplit('/')
                    .next()
                    .unwrap_or("")
                    .trim_end_matches(".md")
                    .to_string();
                Some(entry(
                    &name,
                    content.trim(),
                    &["gpt-instruction"],
                    "big-prompt-library",
                ))
            }
        })
        .buffer_unordered(8)
        .collect::<Vec<_>>()
        .await;

    let mut out: Vec<Value> = results.into_iter().flatten().collect();
    if out.is_empty() {
        return Err("TheBigPromptLibrary 无有效数据".to_string());
    }
    out.truncate(MAX_FETCH_ITEMS);
    Ok(out)
}
