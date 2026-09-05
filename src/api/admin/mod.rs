pub mod common;
pub mod auth;
pub mod users;
pub mod channels;
pub mod tokens;
pub mod pricing;
pub mod orders;
pub mod logs;
pub mod dashboard;
pub mod settings;
pub mod notify;
pub mod security;
pub mod playground;
pub mod monitor;
pub mod cache;
pub mod redemptions;
pub mod network; // 网络层管理模块

// 模块公开接口（保持 main.rs 零改动）
pub use common::{
    error_response,
    verify_admin,
    verify_user,
    extract_session_token,
    admin_id_from_session,
    record_audit,
    default_page,
    default_size,
};

pub use auth::{
    handle_login,
    handle_register,
    handle_logout,
    handle_forgot_password,
    handle_reset_password,
    handle_github_oauth_authorize,
    handle_github_oauth_callback,
    handle_google_oauth_authorize,
    handle_google_oauth_callback,
    LoginRequest,
    RegisterRequest,
    OAuthCallback,
    GoogleCallbackParams,
    GithubCallbackParams,
};

pub use users::{
    handle_list_users,
    handle_create_user,
    handle_update_user,
    handle_delete_user,
    CreateUserRequest,
    UpdateUserRequest,
    mask_user,
};

pub use logs::{
    handle_list_request_logs,
    handle_list_audit_logs,
    handle_export_request_logs,
};


pub use channels::{
    handle_list_channels,
    handle_add_channel,
    handle_update_channel,
    handle_delete_channel,
    ChannelRequest,
    mask_channel,
};

pub use tokens::{
    handle_list_tokens,
    handle_add_token,
    handle_update_token,
    handle_delete_token,
    handle_reset_token_used,
    KeyRequest,
    UpdateTokenRequest,
    mask_token,
};

pub use pricing::{
    handle_list_pricing,
    handle_add_pricing,
    handle_delete_pricing,
    PriceRequest,
};

pub use orders::{
    handle_topup_request,
    handle_list_orders,
    handle_get_order,
    handle_delete_order,
    TopupRequest,
};

pub use dashboard::{
    handle_consumption_trend,
    handle_model_distribution,
    handle_channel_stats,
    handle_user_stats,
    handle_api_stats,
    handle_overview,
    DashboardQuery,
};

pub use settings::{
    handle_get_settings,
    handle_update_settings,
    handle_get_limits,
    handle_update_limits,
    SettingsRequest,
};

pub use notify::{
    handle_get_notify_config,
    handle_update_notify_config,
    handle_send_test_notify,
    UpdateNotifyConfigRequest,
};

pub use security::{
    handle_security_summary,
    handle_security_events,
    handle_reset_security,
    SecurityEventsQuery,
};

pub use monitor::{
    handle_monitor_system,
    handle_health,
    handle_healthz,
};

pub use cache::{
    handle_cache_stats,
    handle_cache_clear,
    handle_cache_info,
};
pub use redemptions::{
    handle_batch_redemptions,
    handle_list_redemptions,
    handle_delete_redemption,
    handle_redeem,
    BatchRedemptionRequest,
    AuditLogQuery,
    RedeemRequest,
};


pub use playground::{
    handle_playground_chat,
    handle_playground_channels,
    PlaygroundChatRequest,
};

// 后续模块的 pub use 将按迁移顺序补充...

// 遗留 handler 归集（60 个，含用户侧 /api/user/* 与告警/IP/汇率等未迁移端点）
pub mod legacy;

pub use legacy::{
    handle_usage_summary,
    handle_refresh_usage,
    handle_list_accounts,
    handle_add_account,
    handle_test_account,
    handle_update_account,
    handle_delete_account,
    handle_list_keys,
    handle_add_key,
    handle_delete_key,
    handle_tokens_today,
    handle_usage_trend,
    handle_usage_models,
    handle_list_prices,
    handle_upsert_price,
    handle_upsert_price_by_model,
    handle_delete_price,
    handle_list_groups,
    handle_upsert_group,
    handle_delete_group,
    handle_get_ratelimit_config,
    handle_update_ratelimit_config,
    handle_user_ranking,
    handle_channel_health,
    handle_reset_channel_circuit,
    handle_realtime,
    handle_alert_rules_list,
    handle_alert_rules_update,
    handle_alerts_active,
    handle_alerts_history,
    handle_alert_test,
    handle_stripe_topup,
    handle_stripe_webhook,
    handle_openapi_json,
    handle_swagger_ui,
    handle_rotate_token,
    handle_get_ip_filter,
    handle_update_ip_filter,
    handle_add_ip_whitelist,
    handle_add_ip_blacklist,
    handle_remove_ip_whitelist,
    handle_remove_ip_blacklist,
    handle_get_price_sync_config,
    handle_update_price_sync_config,
    handle_get_exchange_rates,
    handle_patch_channel,
    handle_test_channel,
    handle_fetch_channel_models,
    handle_epay_notify,
    handle_epay_return,
    handle_channel_chat_test,
    handle_test_telegram,
    handle_test_email,
    handle_test_slack,
    handle_test_webhook,
};

// 用户侧端点与未迁移端点（main.rs 恢复路由时需要）
pub use legacy::{
    handle_me,
    handle_my_orders,
    handle_check_username,
    handle_get_epay_config,
    handle_update_epay_config,
    handle_get_ratios,
    handle_update_ratios,
    handle_upsert_group_by_name,
    handle_trigger_price_sync,
    handle_update_exchange_rates,
};

// 网络层管理（main.rs /api/network/* 路由）
pub use network::{
    health_check,
    update_network_config,
    restart_network,
    add_network_account,
    remove_network_account,
};