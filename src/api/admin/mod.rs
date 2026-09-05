pub mod auth;
pub mod cache;
pub mod channels;
pub mod common;
pub mod dashboard;
pub mod logs;
pub mod monitor;
pub mod network;
pub mod notify;
pub mod orders;
pub mod playground;
pub mod pricing;
pub mod redemptions;
pub mod security;
pub mod settings;
pub mod tokens;
pub mod users; // 网络层管理模块

// 模块公开接口（保持 main.rs 零改动）
pub use common::{
    admin_id_from_session, default_page, default_size, error_response, extract_session_token,
    record_audit, verify_admin, verify_user,
};

pub use auth::{
    handle_forgot_password, handle_github_oauth_authorize, handle_github_oauth_callback,
    handle_google_oauth_authorize, handle_google_oauth_callback, handle_login, handle_logout,
    handle_register, handle_reset_password, GithubCallbackParams, GoogleCallbackParams,
    LoginRequest, OAuthCallback, RegisterRequest,
};

pub use users::{
    handle_create_user, handle_delete_user, handle_list_users, handle_update_user, mask_user,
    CreateUserRequest, UpdateUserRequest,
};

pub use logs::{handle_export_request_logs, handle_list_audit_logs, handle_list_request_logs};

pub use channels::{
    handle_add_channel, handle_delete_channel, handle_list_channels, handle_update_channel,
    mask_channel, ChannelRequest,
};

pub use tokens::{
    handle_add_token, handle_delete_token, handle_list_tokens, handle_reset_token_used,
    handle_update_token, mask_token, KeyRequest, UpdateTokenRequest,
};

pub use pricing::{handle_add_pricing, handle_delete_pricing, handle_list_pricing, PriceRequest};

pub use orders::{
    handle_delete_order, handle_get_order, handle_list_orders, handle_topup_request, TopupRequest,
};

pub use dashboard::{
    handle_api_stats, handle_channel_stats, handle_consumption_trend, handle_model_distribution,
    handle_overview, handle_user_stats, DashboardQuery,
};

pub use settings::{
    handle_get_limits, handle_get_settings, handle_update_limits, handle_update_settings,
    SettingsRequest,
};

pub use notify::{
    handle_get_notify_config, handle_send_test_notify, handle_update_notify_config,
    UpdateNotifyConfigRequest,
};

pub use security::{
    handle_reset_security, handle_security_events, handle_security_summary, SecurityEventsQuery,
};

pub use monitor::{handle_health, handle_healthz, handle_monitor_system};

pub use cache::{handle_cache_clear, handle_cache_info, handle_cache_stats};
pub use redemptions::{
    handle_batch_redemptions, handle_delete_redemption, handle_list_redemptions, handle_redeem,
    AuditLogQuery, BatchRedemptionRequest, RedeemRequest,
};

pub use playground::{handle_playground_channels, handle_playground_chat, PlaygroundChatRequest};

// 后续模块的 pub use 将按迁移顺序补充...

// 遗留 handler 归集（60 个，含用户侧 /api/user/* 与告警/IP/汇率等未迁移端点）
pub mod legacy;

pub use legacy::{
    handle_add_account, handle_add_ip_blacklist, handle_add_ip_whitelist, handle_add_key,
    handle_alert_rules_list, handle_alert_rules_update, handle_alert_test, handle_alerts_active,
    handle_alerts_history, handle_channel_chat_test, handle_channel_health, handle_delete_account,
    handle_delete_group, handle_delete_key, handle_delete_price, handle_epay_notify,
    handle_epay_return, handle_fetch_channel_models, handle_get_exchange_rates,
    handle_get_ip_filter, handle_get_price_sync_config, handle_get_ratelimit_config,
    handle_list_accounts, handle_list_groups, handle_list_keys, handle_list_prices,
    handle_openapi_json, handle_patch_channel, handle_realtime, handle_refresh_usage,
    handle_remove_ip_blacklist, handle_remove_ip_whitelist, handle_reset_channel_circuit,
    handle_rotate_token, handle_stripe_topup, handle_stripe_webhook, handle_swagger_ui,
    handle_test_account, handle_test_channel, handle_test_email, handle_test_slack,
    handle_test_telegram, handle_test_webhook, handle_tokens_today, handle_update_account,
    handle_update_ip_filter, handle_update_price_sync_config, handle_update_ratelimit_config,
    handle_upsert_group, handle_upsert_price, handle_upsert_price_by_model, handle_usage_models,
    handle_usage_summary, handle_usage_trend, handle_user_ranking,
};

// 用户侧端点与未迁移端点（main.rs 恢复路由时需要）
pub use legacy::{
    handle_check_username, handle_get_epay_config, handle_get_ratios, handle_me, handle_my_orders,
    handle_trigger_price_sync, handle_update_epay_config, handle_update_exchange_rates,
    handle_update_ratios, handle_upsert_group_by_name,
};

// 网络层管理（main.rs /api/network/* 路由）
pub use network::{
    add_network_account, health_check, remove_network_account, restart_network,
    update_network_config,
};
