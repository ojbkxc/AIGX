pub mod config;
pub mod storage;
pub mod account;
pub mod channel;
pub mod pricing;
pub mod user_group;
pub mod model;
pub mod usage;
pub mod graphql;
pub mod proxy;
pub mod api;
pub mod web;
pub mod bridge;
pub mod hub;
pub mod sse;
pub mod error_translate;
pub mod user;
pub mod payment;
pub mod ratelimit;
pub mod subscription;
pub mod guardrail;
pub mod health;
pub mod token_estimate;
pub mod cache;
pub mod log;
pub mod notify;
pub mod redemption;
pub mod metrics;
pub mod quota_monitor;
pub mod semantic;
pub mod oauth;

// SeaORM 多数据库后端模块（仅当启用 sea-orm feature 时编译）。
//
// 默认构建不引入 sea-orm 依赖，不影响现有功能与构建产物体积。
// 启用方式：cargo build --features "sea-orm postgres"（或 sqlite / mysql）
#[cfg(feature = "sea-orm")]
pub mod db;
