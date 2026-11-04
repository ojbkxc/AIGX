// OpenAPI 文档里有一个多层嵌套的 `json!` 字面量（路径 → 方法 → 响应 schema），
// 默认宏递归上限（128）不够，编译期直接报 "recursion limit reached"。
// 提高 crate 级上限即可，不影响运行期。
#![recursion_limit = "512"]

pub mod account;
pub mod account_login;
pub mod account_session;
pub mod agent;
pub mod altcha;
pub mod api;
pub mod auth;
pub mod bridge;
pub mod cache;
pub mod channel;
pub mod chat_session;
pub mod checkin;
pub mod config;
pub mod continuation;
pub mod cron;
pub mod debug_traffic;
pub mod runtime_log;
pub mod egress;
pub mod error_translate;
pub mod graphql;
pub mod guardrail;
pub mod health;
pub mod hub;
pub mod ip;
pub mod log;
pub mod metrics;
pub mod model;
pub mod monitor;
pub mod net_guard;
pub mod notify;
pub mod oauth;
pub mod payment;
pub mod plan;
pub mod pricing;
pub mod prompt;
pub mod proxy;
pub mod proxy_pool;
pub mod quota_monitor;
pub mod ratelimit;
pub mod redemption;
pub mod sanitize;
pub mod scheduled_tasks;
pub mod semantic;
pub mod sse;
pub mod storage;
pub mod ticket;
pub mod token_estimate;
pub mod update;
pub mod usage;
pub mod user;
pub mod user_group;
pub mod web;

// SeaORM 多数据库后端模块（仅当启用 sea-orm feature 时编译）。
//
// 默认构建不引入 sea-orm 依赖，不影响现有功能与构建产物体积。
// 启用方式：cargo build --features "sea-orm postgres"（或 sqlite / mysql）
#[cfg(feature = "sea-orm")]
pub mod db;
