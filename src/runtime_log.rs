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
    /// 结构化事件字段（仅 `target = "plugin"` 的生命周期事件捕获）。
    ///
    /// 对齐 A2A `logs_store/plugin_events.rs`：插件启停事件带 `action` /
    /// `pluginId` / `routine` 等字段，日志页据此归类（`routine` = 常规启停，
    /// 不是异常）。普通日志为 None，不增加序列化体积。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
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

/// 提取 tracing 事件的 message 字段，并为插件事件收集结构化字段。
struct EventVisitor {
    message: String,
    /// `Some` = 收集结构化字段（仅插件生命周期事件）。
    data: Option<serde_json::Map<String, Value>>,
}

impl EventVisitor {
    /// 把一个非 message 字段塞进结构化数据（无收集目标时忽略）。
    fn put(&mut self, field: &tracing::field::Field, value: Value) {
        if let Some(data) = self.data.as_mut() {
            data.insert(field.name().to_string(), value);
        }
    }
}

impl Visit for EventVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{:?}", value);
            return;
        }
        // `%value`（Display）与 `?value`（Debug）都走这里；去掉 Debug 可能带的
        // 外层引号，让 `pluginId = %id` 落成纯字符串。
        let text = format!("{:?}", value);
        let cleaned = text
            .strip_prefix('"')
            .and_then(|inner| inner.strip_suffix('"'))
            .unwrap_or(&text)
            .to_string();
        self.put(field, Value::String(cleaned));
    }
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
            return;
        }
        self.put(field, Value::String(value.to_string()));
    }
    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        self.put(field, Value::Bool(value));
    }
    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        self.put(field, Value::from(value));
    }
    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.put(field, Value::from(value));
    }
    fn record_f64(&mut self, field: &tracing::field::Field, value: f64) {
        self.put(field, Value::from(value));
    }
}

/// tracing layer 回调：捕获事件到环形缓冲。
pub fn capture_event(event: &tracing::Event<'_>) {
    let meta = event.metadata();
    let level = meta.level().to_string().to_lowercase();
    let category = meta.target().to_string();

    // 只有插件生命周期事件需要结构化字段（与 A2A plugin_events 同范围）。
    let capture_data = meta.target() == "plugin";
    let mut visitor = EventVisitor {
        message: String::new(),
        data: capture_data.then(serde_json::Map::new),
    };
    event.record(&mut visitor);

    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);

    let data = visitor
        .data
        .filter(|fields| !fields.is_empty())
        .map(Value::Object);

    push(RuntimeLogEntry {
        id,
        ts,
        level,
        category,
        message: visitor.message,
        data,
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
        Some(sid) => all.into_iter().rev().filter(|e| e.id > sid).collect(),
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
