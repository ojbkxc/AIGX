pub mod admin;
pub mod anthropic;
pub mod auth;
pub mod client_id;
pub mod common;
pub mod failover;
pub mod openai;
pub mod tool_runtime; // 工具服务数据面（/api/web-search/*、MCP 分发；对齐 A2A api/tool_runtime.rs）
