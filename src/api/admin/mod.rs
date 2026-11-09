pub mod account_login;
pub mod account_sessions;
pub mod assistant;
pub mod auth;
pub mod backup;
pub mod backup_crypto;
pub mod cache;
pub mod channels;
pub mod checkin_center;
pub mod cline_headers;
pub mod common;
pub mod dashboard;
pub mod debug;
pub mod desktop_import;
pub mod diagnostics;
pub mod logs;
pub mod mcp;
pub mod model_manage;
pub mod model_metadata;
pub mod model_test;
pub mod monitor;
pub mod network;
pub mod notify;
pub mod openapi;
pub mod orders;
pub mod plans;
pub mod playground;
pub mod plugins; // 插件管理面（对照 Agent2API /api/plugins*，契约化不进程化）
pub mod pricing;
pub mod prompts;
pub mod proxy_pool_api;
pub mod queue_api;
pub mod redemptions;
pub mod routing;
pub mod scheduled_tasks;
pub mod security;
pub mod settings;
pub mod subscriptions;
pub mod tickets;
pub mod tokens;
pub mod update;
pub mod upgrade; // 数据升级端点（对照 Agent2API /api/upgrade）
pub mod user_tools; // 用户工具策略管理面（#34 §5.1：GET/PUT policy + GET catalog）
pub mod users; // 网络层管理模块
pub mod zcode_captcha; // ZCode 人机验证令牌池（对照 Agent2API /api/zcode/captcha*）

pub use auth::{
    handle_captcha, handle_change_password, handle_forgot_password, handle_geo_lang,
    handle_get_session, handle_github_bind_start, handle_github_oauth_authorize,
    handle_github_oauth_callback, handle_google_bind_start, handle_google_oauth_authorize,
    handle_google_oauth_callback, handle_linuxdo_bind_start, handle_linuxdo_oauth_authorize,
    handle_linuxdo_oauth_callback, handle_login, handle_login_send_code, handle_login_totp,
    handle_login_with_code, handle_logout, handle_oauth_consume, handle_oauth_unbind,
    handle_refresh_session, handle_register, handle_reset_password, handle_totp_disable,
    handle_totp_enable, handle_totp_setup,
};

pub use users::{
    handle_admin_disable_2fa, handle_create_user, handle_delete_user, handle_list_users,
    handle_manage_user, handle_update_self, handle_update_user,
};

pub use logs::{
    handle_agg_rebuild, handle_agg_tz_migrate, handle_cleanup_logs, handle_clear_audit_logs,
    handle_clear_preview, handle_clear_request_logs, handle_delete_audit_logs,
    handle_delete_request_logs, handle_export_request_logs, handle_export_runtime_logs,
    handle_get_log_retention, handle_get_runtime_logs, handle_list_audit_logs,
    handle_list_inflight, handle_list_request_logs, handle_terminate_inflight,
    handle_update_log_retention,
};

pub use channels::{
    handle_add_channel, handle_available_models, handle_channel_health_archive,
    handle_delete_channel, handle_list_channels, handle_update_channel,
};

pub use tokens::{
    handle_add_token, handle_delete_token, handle_get_token_key, handle_list_tokens,
    handle_reset_token_used, handle_update_token,
};

pub use pricing::{
    handle_add_pricing, handle_cost_estimate, handle_delete_pricing, handle_list_pricing,
    handle_missing_pricing_models, handle_model_meta_delete, handle_model_meta_list,
    handle_model_meta_set,
};

// 统一模型管理（渠道模型 + 账号池模型聚合视图，参照 Agent2API 模型管理）
pub use model_manage::{
    handle_add_custom_model, handle_manage_view, handle_remove_alias, handle_remove_custom_model,
    handle_remove_price, handle_set_alias, handle_set_alias_reasoning, handle_set_alias_state,
    handle_set_capabilities, handle_set_state, handle_upsert_model_price,
};
// 模型测试（钉 provider/账号/渠道的真实链路最小对话，对照 Agent2API model_test）
pub use model_test::handle_model_test;

// 签到中心（管理员替上游账号签到，区别于用户侧的 /api/checkin）
pub use checkin_center::{
    handle_benefit_run, handle_benefits_list, handle_checkin_center, handle_checkin_records,
    handle_checkin_run, handle_clear_checkin_records, handle_get_checkin_settings,
    handle_get_keepalive, handle_list_checkin_schedules, handle_mark_kept_active,
    handle_put_keepalive, handle_remove_checkin_schedule, handle_run_checkin_schedule,
    handle_update_checkin_settings, handle_upsert_checkin_schedule,
};

pub use orders::{
    handle_complete_order, handle_delete_order, handle_get_order, handle_list_orders,
    handle_topup_amount, handle_topup_request,
};

pub use tickets::{
    handle_admin_close_ticket, handle_admin_list_tickets, handle_admin_reply_ticket,
    handle_user_close_ticket, handle_user_list_tickets, handle_user_reply_ticket,
    handle_user_save_ticket,
};

pub use plans::{handle_delete_plan, handle_issue_plan_key, handle_list_plans, handle_upsert_plan};

// 订阅（#85 套餐订阅化：时长订阅 + 余额购买 + 独立配额池）
pub use subscriptions::{
    handle_subscription_admin_bind, handle_subscription_admin_cancel,
    handle_subscription_admin_list, handle_subscription_balance_pay, handle_subscription_plans,
    handle_subscription_self,
};

pub use dashboard::{
    handle_cache_savings, handle_consumption_trend, handle_model_distribution,
    handle_report_cache_rates, handle_report_cache_trend_24h, handle_report_daily_trend,
    handle_report_heatmap, handle_report_models, handle_report_overview, handle_report_token_trend,
    handle_report_top_sources, handle_scheduler_status,
};

pub use settings::{
    handle_get_agent_settings, handle_get_chat_settings, handle_get_gateway_prompt,
    handle_get_gateway_settings, handle_get_limits, handle_get_oauth_config,
    handle_get_report_settings, handle_get_sanitize, handle_get_security_policy,
    handle_get_settings, handle_update_agent_settings, handle_update_chat_settings,
    handle_update_gateway_prompt, handle_update_gateway_settings, handle_update_limits,
    handle_update_oauth_config, handle_update_report_settings, handle_update_sanitize,
    handle_update_security_policy, handle_update_settings,
};

// 数据管理（存储概览 + 全量备份/恢复；对照 Agent2API「设置 → 数据」）
pub use backup::{handle_backup_import, handle_storage_overview};

pub use notify::{handle_get_notify_config, handle_update_notify_config};

pub use security::{handle_security_events, handle_security_summary};

// 调试模式：上游原始报文查看/清空（对照 Agent2API `/api/debug/traffic`）
pub use debug::{
    handle_clear_debug_traffic, handle_get_debug, handle_get_debug_traffic, handle_update_debug,
};

// models.dev 模型元数据：开关 + 状态 + 手动刷新（对照 Agent2API `/api/models/metadata`）
pub use model_metadata::{
    handle_get_model_metadata, handle_put_model_metadata, handle_refresh_model_metadata,
    handle_refresh_models,
};

pub use monitor::{handle_health, handle_healthz, handle_monitor_system};

// 只读诊断端点套件（AI 可运维第①步：summary/channels/breakers 三件套）
pub use diagnostics::{
    handle_diagnostics_breakers, handle_diagnostics_channels, handle_diagnostics_summary,
};

// 内嵌 MCP server（AI 可运维第②步：Streamable HTTP JSON-RPC 端点 + 非高危工具白名单，
// 名单与 registry::mcp_tools 同源，条数随注册表变化）
pub use mcp::handle_mcp;

// 管理面 OpenAPI 3.1 自描述（零依赖静态构造，覆盖全部管理面真实路由）
pub use openapi::handle_openapi_v31;

pub use cache::{handle_cache_clear, handle_cache_stats};
pub use redemptions::{
    handle_batch_redemptions, handle_delete_redemption, handle_list_redemptions, handle_redeem,
};

pub use playground::handle_playground_chat;
pub use playground::handle_playground_images;
pub use playground::handle_playground_transcriptions;
pub use playground::handle_playground_tts;

// 公开提示词源拉取（Prompts 页「拉取公开源」）
pub use prompts::{handle_prompt_fetch, handle_prompt_sources, handle_prompt_translate};

// 后续模块的 pub use 将按迁移顺序补充...

// 遗留 handler 归集（60 个，含用户侧 /api/user/* 与告警/IP/汇率等未迁移端点）
pub mod legacy;

pub use legacy::{
    handle_add_account, handle_add_ip_blacklist, handle_add_ip_whitelist, handle_alert_resolve,
    handle_alert_rules_list, handle_alert_rules_update, handle_alert_test, handle_alerts_active,
    handle_alerts_history, handle_channel_chat_test, handle_channel_health, handle_delete_account,
    handle_delete_group, handle_delete_price, handle_epay_notify, handle_epay_return,
    handle_fetch_channel_models, handle_get_channel_balance, handle_get_exchange_rates,
    handle_get_ip_filter, handle_get_price_sync_config, handle_get_ratelimit_config,
    handle_list_accounts, handle_list_groups, handle_list_prices, handle_openapi_json,
    handle_patch_channel, handle_realtime, handle_refresh_usage, handle_remove_ip_blacklist,
    handle_remove_ip_whitelist, handle_reset_channel_circuit, handle_rotate_token,
    handle_stripe_topup, handle_stripe_webhook, handle_swagger_ui, handle_test_account,
    handle_test_channel, handle_test_email, handle_test_slack, handle_test_telegram,
    handle_test_webhook, handle_tokens_today, handle_update_account, handle_update_ip_filter,
    handle_update_price_sync_config, handle_update_ratelimit_config, handle_upsert_group,
    handle_upsert_price_by_model, handle_usage_models, handle_usage_summary, handle_usage_trend,
    handle_user_ranking,
};

// 用户侧端点与未迁移端点（main.rs 恢复路由时需要）
pub use legacy::{
    handle_aff_transfer, handle_check_username, handle_do_checkin, handle_get_aff_code,
    handle_get_epay_config, handle_get_epay_info, handle_get_ratios, handle_me, handle_my_orders,
    handle_trigger_price_sync, handle_update_epay_config, handle_update_exchange_rates,
    handle_update_ratios, handle_upsert_group_by_name, handle_user_records,
};

// 签到（对齐 new-api GET/POST /api/user/checkin）
pub use legacy::handle_checkin_status;

// 上游模型发现（供 openai::handle_list_models 懒加载复用）
pub use legacy::fetch_upstream_models;

// 网络层管理（main.rs /api/network/* 路由；gate/load 供数据面闸门内部使用）
pub use network::{
    add_network_account, health_check, list_network_accounts, remove_network_account,
    restart_network, update_network_config,
};

// 路由策略（对照 Agent2API「设置 → 路由」；数据面 resolve_bridges 每请求读快照）
pub use routing::{
    handle_clear_route_sticky, handle_delete_routing_group, handle_get_route_sticky,
    handle_get_routing, handle_get_routing_group, handle_get_routing_profiles,
    handle_put_routing_group, handle_put_routing_profiles, handle_routing_defaults,
    handle_routing_preview, handle_routing_queue, handle_update_routing,
};

pub use queue_api::{handle_get_queue, handle_update_queue};

// 间隔型定时任务（凭证维护/模型刷新/版本检查 + 三页自动刷新的开关与间隔）
pub use scheduled_tasks::{configure_handler, list_handler, run_handler};

// Cline 伪装头覆盖表（对照 Agent2API「设置 → 网关 → Cline 伪装头」）
pub use cline_headers::{handle_get_cline_headers, handle_update_cline_headers};

// 数据升级端点（对照 Agent2API /api/upgrade + /api/upgrade/run）
pub use upgrade::{handle_get_upgrade, handle_run_upgrade};

// ZCode 人机验证令牌池（概况 / 入池 / 风控配置 / 铸造节奏）
pub use zcode_captcha::{
    handle_get_zcode_captcha, handle_get_zcode_captcha_config, handle_get_zcode_captcha_pool,
    handle_push_zcode_captcha, handle_update_zcode_captcha_pool,
};
