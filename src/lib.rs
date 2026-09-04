pub mod account;
pub mod api;
pub mod bridge;
pub mod cache;
pub mod channel;
pub mod config;
pub mod error_translate;
pub mod graphql;
pub mod guardrail;
pub mod health;
pub mod hub;
pub mod ip;
pub mod log;
pub mod metrics;
pub mod model;
pub mod notify;
pub mod oauth;
pub mod payment;
pub mod pricing;
pub mod proxy;
pub mod quota_monitor;
pub mod ratelimit;
pub mod redemption;
pub mod semantic;
pub mod sse;
pub mod storage;
pub mod token_estimate;
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
