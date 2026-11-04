//! 运行日志环形缓冲 + tracing layer 捕获（P1-1 运行日志页后端）。
//!
//! 对照 Agent2API「运行日志页」：level/category 字典 + 500 条环形 +
//! sinceId 未读水位 + JSONL 导出。
//!
//! tracing layer 在每个事件产生时捕获到环形缓冲（不阻塞数据面——
//! 缓冲写入是 O(1) 的 VecDeque push_back + 偶发 pop_front）。

use std::collections::VecDeque;
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};

use tracing::field::Visit;

/// 环形缓冲最大条数。
pub const MAX_ENTRIES: usize = 500;

/// 一条运行日志记录。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeLogEntry {
    /// 自增 ID（sinceId 未读水位用）。
    pub id: u64,
    /// 毫秒 Unix 时间戳。
    pub ts: i64,
    /// 日志级别：error/warn/info/debug/trace。
    pub level: String,
    /// 模块分类（tracing target，如 "aigx::api::admin::settings"）。
    pub category: String,
    /// 日志消息（message 字段；无则空串）。
    pub message: String,
}

/// 环形缓冲 + 自增 ID。
static BUFFER: Mutex<VecDeque<RuntimeLogEntry>> = Mutex::new(VecDeque::new());
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// 记录一条日志。
fn push(entry: RuntimeLogEntry) {
    let mut buffer = BUFFER.lock();
    while buffer.len() >= MAX_ENTRIES {
        buffer.pop_front();
    }
    buffer.push_back(entry);
}

/// 提取 tracing 事件的 message 字段。
struct MessageVisitor {
    message: String,
}

impl Visit for MessageVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{:?}", value);
        }
    }
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        }
    }
}

/// tracing layer 回调：捕获事件到环形缓冲。
pub fn capture_event(event: &tracing::Event<'_>) {
    let meta = event.metadata();
    let level = meta.level().to_string().to_lowercase();
    let category = meta.target().to_string();

    let mut visitor = MessageVisitor {
        message: String::new(),
    };
    event.record(&mut visitor);

    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);

    push(RuntimeLogEntry {
        id,
        ts,
        level,
        category,
        message: visitor.message,
    });
}

/// tracing Layer：每个事件推入环形缓冲。
pub struct RuntimeLogLayer;

impl<S: tracing::Subscriber> tracing_subscriber::layer::Layer<S> for RuntimeLogLayer {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        capture_event(event);
    }
}

/// 读取日志快照（最新在前）。`since_id` 用于未读水位筛选。
pub fn snapshot(since_id: Option<u64>) -> Value {
    let buffer = BUFFER.lock();
    let all: Vec<RuntimeLogEntry> = buffer.iter().cloned().collect();
    let entries: Vec<RuntimeLogEntry> = match since_id {
        Some(sid) => all
            .into_iter()
            .rev()
            .filter(|e| e.id > sid)
            .collect(),
        None => all.into_iter().rev().collect(),
    };
    serde_json::json!({
        "entries": entries,
        "maxEntries": MAX_ENTRIES,
        "latestId": NEXT_ID.load(Ordering::Relaxed) - 1,
    })
}

/// 导出全部日志为 JSONL（每行一条）。
pub fn export_jsonl() -> String {
    let buffer = BUFFER.lock();
    let mut lines = String::new();
    for entry in buffer.iter() {
        if let Ok(json) = serde_json::to_string(entry) {
            lines.push_str(&json);
            lines.push('\n');
        }
    }
    lines
}
