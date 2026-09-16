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
use axum::http::StatusCode;
use axum::Json;
use serde_json::{json, Value};
use tokio::sync::RwLock;

use super::super::openai::AppState;
use super::common::error_response;

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

/// 内存缓存：key → (抓取时间戳, 源数据)
#[derive(Default)]
pub struct PromptSourceCache {
    map: Arc<RwLock<HashMap<String, (i64, Value)>>>,
}

impl PromptSourceCache {
    pub fn new() -> Self {
        Self::default()
    }
}

const CACHE_TTL_SECS: i64 = 300;

async fn hit(cache: &PromptSourceCache, key: &str) -> Option<Value> {
    let map = cache.map.read().await;
    map.get(key).and_then(|(ts, v)| {
        if chrono::Utc::now().timestamp() - *ts < CACHE_TTL_SECS {
            Some(v.clone())
        } else {
            None
        }
    })
}

async fn store(cache: &PromptSourceCache, key: &str, v: Value) {
    let mut map = cache.map.write().await;
    map.insert(key.to_string(), (chrono::Utc::now().timestamp(), v));
}

/// GET /api/prompts/sources — 列出可拉取的公开源（登录用户即可）。
pub async fn handle_prompt_sources() -> Json<Value> {
    let data: Vec<Value> = SOURCES
        .iter()
        .map(|(id, name, desc, repo)| {
            json!({ "id": id, "name": name, "description": desc, "repo": repo })
        })
        .collect();
    Json(json!({ "success": true, "data": data }))
}

/// POST /api/prompts/fetch — 抓取指定源并返回提示词数组。
pub async fn handle_prompt_fetch(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
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
    if let Some(v) = hit(&state.prompt_source_cache, &key).await {
        return Ok(Json(json!({ "success": true, "data": v })));
    }

    let client = state.http_client.clone();
    let result = match id {
        "prompts-chat" => fetch_prompts_chat(&client).await,
        "awesome-prompts" => fetch_awesome_prompts(&client).await,
        "big-prompt-library" => fetch_big_prompt_library(&client).await,
        _ => return Err(error_response("unknown source", StatusCode::NOT_FOUND)),
    };

    let data = result
        .map_err(|e| error_response(&format!("拉取公开源失败：{e}"), StatusCode::BAD_GATEWAY))?;
    store(&state.prompt_source_cache, &key, Value::Array(data.clone())).await;
    Ok(Json(json!({ "success": true, "data": data })))
}

type PromptList = Vec<Value>;

fn entry(name: &str, content: &str, tags: &[&str], source: &str) -> Value {
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

    let mut out = Vec::new();
    for item in items {
        let name = item.get("name").and_then(|n| n.as_str()).unwrap_or("");
        if !(name.ends_with(".txt") || name.ends_with(".md")) {
            continue;
        }
        let dl = item
            .get("download_url")
            .and_then(|u| u.as_str())
            .ok_or_else(|| "缺少 download_url".to_string())?;
        let content = client
            .get(dl)
            .header("User-Agent", "AIGX")
            .send()
            .await
            .map_err(|e| e.to_string())?
            .error_for_status()
            .map_err(|e| e.to_string())?
            .text()
            .await
            .map_err(|e| e.to_string())?;
        if content.trim().is_empty() {
            continue;
        }
        let title = name
            .trim_end_matches(".txt")
            .trim_end_matches(".md")
            .replace('_', " ");
        out.push(entry(
            &title,
            content.trim(),
            &["awesome-prompts"],
            "awesome-prompts",
        ));
    }
    if out.is_empty() {
        return Err("awesome-prompts 无有效数据".to_string());
    }
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
    // 上限：只抓前 200 个（库体量巨大，避免单次请求过重）
    let mut out = Vec::new();
    for path in paths.iter().take(200) {
        let url = format!("https://raw.githubusercontent.com/0xeb/TheBigPromptLibrary/main/{path}");
        let content = client
            .get(&url)
            .header("User-Agent", "AIGX")
            .send()
            .await
            .map_err(|e| e.to_string())?
            .error_for_status()
            .map_err(|e| e.to_string())?
            .text()
            .await
            .map_err(|e| e.to_string())?;
        if content.trim().is_empty() {
            continue;
        }
        let name = path
            .rsplit('/')
            .next()
            .unwrap_or("")
            .trim_end_matches(".md")
            .to_string();
        out.push(entry(
            &name,
            content.trim(),
            &["gpt-instruction"],
            "big-prompt-library",
        ));
    }
    if out.is_empty() {
        return Err("TheBigPromptLibrary 无有效数据".to_string());
    }
    Ok(out)
}
