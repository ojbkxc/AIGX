// 单一 target（原 lib+bin 双编译已于 P1 收口时删除 src/lib.rs）：
// 全部 62 个模块在这里以私有 mod 声明，测试也跑在 bin target 上
// （`cargo test --bin aigx`）。编译单元减半 —— 此前同一批模块要分别编进
// lib 与 bin 两个 crate，两个 crate root 还各复制了一份 rustls provider
// 安装逻辑。
//
// ── lint 遮罩状态（双 target 时代的两条 crate 级 allow 均已拆除）──────
// 1. `#![allow(unused_imports)]`：清理波次 1 删除（同时清掉 19 处真实未用
//    import 与 6 个无人消费的 re-export）。
// 2. `#![allow(dead_code)]`：清理波次 1/2/3 与「六块零消费方子系统删除」
//    （rate_budget / video_billing / response_quality / aimd_limiter /
//    semantic 整块、hub 只留专用桥）把 never-used 从 377 压到 274 后，
//    剩余项**全部落到项级或模块级**显式登记，crate 级遮罩已删除。
//    两类剩余，逐项都带注释说明：
//    ① 编译器不可见的线表契约（plugin_host/script/rpc：宿主 JS 的
//       dispatch/callback 名，只有测试对表）；
//    ② 已注明「未接入」或「预留」的移植件 / 未接线的调度与 CF 代理解析
//       （chatglm_stream / scheduler / proxy / script/source 等整块模块级；
//       其余零散项级）。
//
// 纪律：**不得再加 crate 级 lint 遮罩**。新增代码需要预留符号时，必须在
// `#[allow(dead_code)]` 后写一行注释注明「为什么未接线 + 计划消费方」，
// 并同步本清单；`cargo check --all-targets` 与
// `cargo clippy --workspace --all-targets -- -D warnings` 必须保持 0 warning。
#![recursion_limit = "512"]

// ── reqwest 0.13 测试侧 provider 安装（波次2）──────────────────────
// 单 target 后本函数是全进程唯一一份（原 lib.rs 的同名副本已随 lib 删除）；
// 各模块的 `#[cfg(test)]` 测试统一走 `crate::test_http_client`。

/// 单元测试内构建 reqwest Client 的统一入口：先装 provider 再建客户端。
#[cfg(test)]
pub(crate) fn test_http_client() -> reqwest::Client {
    install_rustls_provider_for_tests();
    reqwest::Client::new()
}

/// 在 bin 测试进程内安装一次 rustls ring provider（全局操作，Once 防重入）。
#[cfg(test)]
pub(crate) fn install_rustls_provider_for_tests() {
    static INSTALL: std::sync::Once = std::sync::Once::new();
    INSTALL.call_once(|| {
        rustls::crypto::ring::default_provider()
            .install_default()
            .expect("install rustls ring provider");
    });
}

mod account;
mod account_login;
mod account_session;
mod accounts_list;
mod agent;
mod altcha;
mod api;
mod auth;
mod bridge;
mod cache;
mod channel;
mod chat_session;
mod checkin;
mod config;
mod continuation;
mod credential_version;
mod cron;
mod debug_traffic;
mod desktop_launch;
mod egress;
mod error_policy;
mod error_translate;
mod graphql;
mod guardrail;
mod health;
mod hub;
mod image_compat;
mod ip;
mod log;
mod maintenance;
mod metrics;
mod model;
mod model_key_source;
mod monitor;
mod net_guard;
mod notify;
mod oauth;
mod payment;
mod plan;
mod plugin_host;
mod plugin_ledger;
mod pricing;
mod prompt;
mod proxy;
mod proxy_pool;
mod purge;
mod quota_monitor;
mod ratelimit;
mod redemption;
mod route_special;
mod runtime_capacity;
mod runtime_log;
mod sanitize;
mod scheduled_tasks;
mod server_planes;
mod sse;
mod storage;
mod sync;
mod ticket;
mod token_estimate;
mod tool_services;
mod update;
mod usage;
mod user;
mod user_group;
mod web;
mod zcode_captcha;

// SeaORM 多数据库后端模块（仅当启用 sea-orm feature 时编译）
#[cfg(feature = "sea-orm")]
mod db;

use std::sync::Arc;

use axum::extract::State;
use axum::routing::{delete, get, patch, post, put};
use axum::Router;
use std::time::Duration;
use tower_http::compression::CompressionLayer;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tracing_subscriber::prelude::*;

use account::AccountPool;
use api::auth::ApiKeyStore;
use api::openai::AppState;
use bridge::cf::CloudflareBridge;
use channel::ChannelStore;
use config::ConfigManager;
use health::{HealthTracker, LivezState};
use hub::Hub;
use ip::IpFilterStore;
use log::LogStore;
use model::ModelMapper;
use notify::NotifyService;
use payment::order_store::OrderStore;
use payment::stripe::StripeClient;
use payment::EpayClient;
use pricing::exchange_rate::ExchangeRateService;
use pricing::price_sync::PriceSyncService;
use pricing::PricingStore;
use proxy::CfApiClient;
use ratelimit::RateLimiter;
use redemption::RedemptionStore;
use storage::FileStore;
use ticket::TicketStore;
use usage::UsageTracker;
use user::{Role, UserStore};
use user_group::UserGroupStore;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // reqwest 0.13 选了 `rustls-no-provider`（不捆绑 crypto provider），
    // 构建任何 reqwest::Client 前必须安装一次全局默认 provider；
    // 缺失会在 Client 构建时 panic。此处放 main 入口最早期，只调用一次。
    // 与 tokio-rustls 0.26（SMTP STARTTLS）统一用 ring，单一 crypto provider。
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("install rustls ring provider");

    // 初始化日志：默认保持文本格式（不改变现有运维习惯）；
    // 设置 AIGX_LOG_FORMAT=json 时切换为结构化 JSON（每行一个对象）。
    let log_filter =
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    let json_logs = std::env::var("AIGX_LOG_FORMAT")
        .map(|v| v.eq_ignore_ascii_case("json"))
        .unwrap_or(false);
    // 运行日志捕获层：每个 tracing 事件推入环形缓冲（P1-1 运行日志页）
    let runtime_log_layer = crate::runtime_log::RuntimeLogLayer;
    if json_logs {
        tracing_subscriber::registry()
            .with(log_filter)
            .with(tracing_subscriber::fmt::layer().event_format(JsonLogFormat))
            .with(runtime_log_layer)
            .init();
    } else {
        tracing_subscriber::registry()
            .with(log_filter)
            .with(tracing_subscriber::fmt::layer())
            .with(runtime_log_layer)
            .init();
    }

    // 记录进程启动时刻（/api/system-info 的 uptime 读数；须在配置加载前登记）
    crate::api::admin::system_info::record_start();

    // 加载配置
    let config_manager = Arc::new(ConfigManager::new(None).await);
    // 启动时确保 session_secret 非空并持久化，避免登录后 token 无法鉴权
    ensure_session_secret(&config_manager).await;
    let config = config_manager.get().await;
    // XFF/X-Real-IP 信任快照：默认不信任（客户端可伪造），
    // 部署在可信反代后时在 config.toml 设 trust_proxy_headers = true
    crate::config::set_trust_proxy_headers(config.trust_proxy_headers);
    // 请求日志快照开关快照（数据面同步路径读取，见 config::log_body_enabled）
    crate::config::set_log_body(config.usage.log_body);
    // 路由回放快照开关快照（resolve/选号路径读取，见 config::route_replay_enabled）
    crate::config::set_route_replay(config.usage.route_replay);
    // 聊天助手工具回路开关快照（见 config::assistant_tools_enabled）
    crate::config::set_assistant_tools(config.usage.assistant_tools);
    // 调试模式（上游原始报文）开关快照（桥接层同步读取，见 config::debug_traffic_enabled）
    crate::config::set_debug_traffic(config.usage.debug_traffic);
    // 换行保活帧过滤开关快照（桥接层同步读取，见 config::strip_newline_keepalive）
    crate::config::set_strip_newline_keepalive(config.usage.strip_newline_keepalive);
    // models.dev 元数据补齐开关快照（模型元信息查询与刷新协程同步读取）
    crate::config::set_models_dev_enabled(config.usage.models_dev_enabled);
    // 排队等待设置快照（Qoder 桥每个请求同步读取，见 config::queue_settings）
    crate::config::set_queue_settings(
        config.usage.queue_max_waits as u64,
        config.usage.queue_wait_seconds as u64,
    );
    // 截断自动续写设置快照（数据面每个请求同步读取，见 config::auto_continue_enabled）
    crate::config::set_auto_continue_settings(
        config.usage.auto_continue,
        config.usage.auto_continue_max_rounds as u64,
    );
    // 按次计费固定额度快照（数据面同步路径读取，见 config::billing_flat_quota）
    crate::config::set_billing_flat_quota(config.usage.billing_flat_quota);
    // 会话图片压缩与图片兼容设置快照（数据面每请求读快照；压缩默认开、
    // 视觉兼容默认关，未配置段时快照回落内置默认值）
    crate::image_compat::set_settings(config.image_compat.clone());
    // 统一错误处理策略快照（bridge/account.rs 自身重试/换号判定读取）
    crate::error_policy::install_snapshot(std::sync::Arc::new(
        crate::error_policy::build_snapshot(&config),
    ));
    // 运行容量设置快照（数据面准入中间件读取）
    crate::runtime_capacity::install_settings(config.runtime_capacity.clone());
    // 网页工具调用模拟开关快照（缺省开启）
    crate::config::set_web_tool_emulation(config.web_tools.emulation);

    // 初始化存储（默认 SQLite 后端；--no-default-features 构建降级为 JSON 文件）
    let data_dir = crate::config::expand_path(&config.server.data_dir);
    tokio::fs::create_dir_all(&data_dir).await?;
    // 凭证加密的密钥文件落在数据目录下（account.key，与库同目录、0600 权限）。
    // 必须在任何账号读写之前设定 —— 密钥路径错了会导致解不开存量凭证。
    crate::account_session::secret::set_data_dir(&data_dir);
    #[cfg(feature = "sqlite-kv")]
    let store = {
        // PostgreSQL 后端：database.url 非空且启用了 postgres feature 时走 PG，
        // 否则用默认 SQLite 文件后端。二者同为 FileStore 类型，业务层无感知。
        // 打开 PG 时顺带做一次 SQLite→PG 历史数据迁移（幂等）：老版本 SQLite
        // 库在 data_dir 或 data_dir/data 下，两个路径都探测。
        #[cfg(feature = "postgres")]
        let s = if config.database.is_enabled() {
            // 旧布局优先（老版本直接 data_dir/aigx.db），避免被新布局空库干扰
            let sqlite_dirs = vec![data_dir.clone(), data_dir.join("data")];
            match FileStore::open_postgres(
                &config.database.url,
                config.database.max_connections,
                &sqlite_dirs,
            ) {
                Ok(pg) => {
                    tracing::info!(
                        "PostgreSQL KV store enabled: {}",
                        crate::config::redact_url_credentials(&config.database.url)
                    );
                    pg
                }
                Err(e) => {
                    tracing::error!(
                        "Failed to open PostgreSQL KV store ({}), falling back to SQLite: {}",
                        crate::config::redact_url_credentials(&config.database.url),
                        e
                    );
                    FileStore::open(data_dir.join("data"))?
                }
            }
        } else {
            FileStore::open(data_dir.join("data"))?
        };
        #[cfg(not(feature = "postgres"))]
        let s = FileStore::open(data_dir.join("data"))?;
        Arc::new(s)
    };
    #[cfg(not(feature = "sqlite-kv"))]
    let store = Arc::new(FileStore::new(data_dir.join("data")));

    // 初始化账号池
    let account_pool = Arc::new(AccountPool::new(store.clone()));
    // SSRF 逃生阀快照：账号 Worker 地址发送前校验用（false = 拒绝内网/环回/元数据）
    account_pool.set_allow_internal_upstream(config.usage.allow_internal_upstream);

    // 调试报文持久化：注入 FileStore + 从上次运行恢复最近报文
    crate::debug_traffic::set_store(store.clone());
    crate::debug_traffic::load_persisted();

    // 初始化模型映射
    let model_mapper = Arc::new(ModelMapper::new(store.clone()));
    if let Err(e) = model_mapper.load() {
        tracing::error!("Failed to load model mapper: {e}");
    }

    // 初始化模型元信息注册表（P1：owned_by/上下文长度/能力 + 管理员覆盖）
    let model_metadata = Arc::new(model::metadata::ModelMetadataRegistry::new(store.clone()));
    if let Err(e) = model_metadata.load() {
        tracing::error!("Failed to load model metadata registry: {e}");
    }

    // models.dev 元数据目录：KV 缓存优先、内嵌快照保底（补值层，见 model::models_dev）
    {
        let (source, count) = model::models_dev::init(&store);
        tracing::info!("models.dev metadata catalog loaded: {count} entries from {source}");
    }

    // 插件「提供商设置」持久化存储：单例注入（未注入时读写一律 503「设置存储
    // 尚未就绪」——声明仍能注册，所以前端能看到字段却存不进去，是典型的
    // 「编译绿 ∧ 路由通 ∧ 功能不可用」半死形态）。
    crate::plugin_host::provider_settings::install(store.clone());

    // 模型能力三态覆盖存储：单例注入（未注入时管理面写入返回 500，数据面
    // `/v1/models` 静默按「无覆盖」降级）。
    crate::model::capability_overrides::install(store.clone());

    // 初始化统一模型管理的启停规则（provider × model）
    let model_rules = Arc::new(model::manage::ModelRules::new(store.clone()));
    if let Err(e) = model_rules.load() {
        tracing::error!("Failed to load model rules: {e}");
    }

    // 初始化用量追踪
    let usage_tracker = Arc::new(UsageTracker::new(store.clone(), account_pool.clone()));

    // 初始化 API 密钥存储（数据目录与库同目录：enc1: 信封密钥文件落在这里）
    let mut api_key_store = ApiKeyStore::new(store.clone(), data_dir.clone());
    if let Err(e) = api_key_store.load() {
        tracing::error!("Failed to load API key store: {e}");
    }

    // 初始化用户系统
    let user_store = Arc::new(UserStore::new(store.clone()));

    // 确保默认管理员账户存在
    ensure_default_admin(&user_store);

    // 注入用户存储到 API Key 存储（余额预检：零余额用户拒绝请求）
    api_key_store.with_user_store(user_store.clone());
    let api_key_store = Arc::new(api_key_store);

    // 发布初始 Key 权威代（撤销闸；之后运营改 Key 时经 /api/model-key-source/refresh 刷新）。
    crate::model_key_source::publish_keys(
        api_key_store
            .list()
            .into_iter()
            .map(|key| crate::model_key_source::KeyDefinition {
                id: key.id,
                name: key.name,
                key: key.key,
                enabled: key.is_active && key.status == "active",
                allowed_providers: key.allowed_providers.unwrap_or_default(),
            })
            .collect(),
        true,
    );

    // 初始化订单存储
    let order_store = Arc::new(OrderStore::new(store.clone()));

    // 初始化签到存储（对齐 new-api 签到送配额）
    let checkin_store = Arc::new(crate::user::checkin::CheckinStore::new(store.clone()));

    // 初始化签到中心存储（替上游账号每日领额度，与上面的用户签到是两套）
    let upstream_checkin = Arc::new(crate::checkin::CheckinStore::new(store.clone()));
    if let Err(e) = upstream_checkin.load() {
        tracing::error!("Failed to load upstream checkin store: {e}");
    }
    // 同步签到时区偏移到 scheduler 静态变量（启动时按已加载的设置初始化）
    crate::checkin::scheduler::set_checkin_tz_offset(upstream_checkin.settings().timezone_offset);

    // 同步报表时区偏移到进程级快照（报表日口径单一来源，见 config::report_tz_offset）
    crate::config::set_report_tz_offset(config.report.timezone_offset);

    // 初始化易支付客户端（运行时按配置即时构造，无需常驻）
    let epay_client = Arc::new(EpayClient::new(config.epay.clone()));
    let stripe_client = Arc::new(StripeClient::new(config.stripe.clone()));

    // 初始化 CF API 客户端（AI Binding 桥接方式）
    let mut cf_api_client = CfApiClient::new(account_pool.clone(), model_mapper.clone());
    // 配置兜底 cf-ai-gw Worker 地址（无账号时使用），保证"零账号即可跑通"
    cf_api_client.with_fallback(config.cf_binding_url.clone(), String::new());
    let api_client = Arc::new(cf_api_client);

    // 初始化 Hub 并注册 Cloudflare Bridge
    let hub = Arc::new(Hub::new());
    let cf_bridge = Arc::new(CloudflareBridge::new(
        api_client.clone(),
        model_mapper.clone(),
    ));
    hub.register_specialized("cloudflare", cf_bridge);

    // 初始化健康检查
    let health_tracker = Arc::new(HealthTracker::new());
    let livez_state = Arc::new(LivezState::new());

    // 初始化通用渠道存储（混用 CF + 第三方 OpenAI 兼容上游）
    let channel_store = Arc::new(ChannelStore::new(store.clone()));

    // 路由策略（对照 Agent2API「设置 → 路由」）：策略/粘性/断路器/调度权重
    // 可配可热更；数据面 resolve_bridges 每请求读快照分派
    let routing_policy =
        Arc::new(crate::channel::routing_policy::RoutingPolicyStore::with_store(store.clone()));
    // 终端用户工具权限策略（#34 P0）：与路由策略同构的快照存储，默认全关
    let user_tool_policy =
        Arc::new(crate::agent::user_tool_policy::UserToolPolicyStore::with_store(store.clone()));
    // 启动时把持久化的路由策略热应用到各组件（免重启生效）：
    // 断路器（开关+三参）、亲和 TTL、调度权重
    {
        let p = routing_policy.snapshot();
        channel_store
            .circuit_breaker()
            .set_enabled(p.breaker.enabled);
        if p.breaker.enabled {
            channel_store.circuit_breaker().update_params(
                p.breaker.threshold,
                p.breaker.cooldown_secs,
                p.breaker.success_threshold,
            );
        }
        if let Some((sticky_secs, hard_secs)) = p.sticky.ttls() {
            channel_store.affinity_cache().update_ttls(
                Duration::from_secs(sticky_secs),
                Duration::from_secs(hard_secs),
            );
        }
        channel_store.set_scheduler_config(p.scheduler_weights.clone());
    }

    // 系统提示词注入（对照 Agent2API「设置 → 网关 → 系统提示词」）：
    // passthrough/custom/append 三模式，保存即生效
    let gateway_prompt = Arc::new(crate::prompt::GatewayPromptStore::with_store(store.clone()));
    crate::prompt::install_snapshot(gateway_prompt.snapshot());
    // 破甲控制台：配置热更新；启动时刻只用于页面展示运行时长
    let purge = Arc::new(crate::purge::PurgeStore::with_store(store.clone()));
    crate::purge::init_started_at();
    // 护栏（输入/输出黑名单 + 200 假答复门控）：进程级存储，默认全关；
    // 启动时从盘恢复，管理面保存即热生效
    crate::guardrail::install(store.clone());

    // 额外脱敏请求头名单（对照 Agent2API「设置 → 网关 → 指纹脱敏」）：
    // 启动时从盘恢复进程级快照；保存设置时热更新
    if let Ok(Some(list)) = store.get::<Vec<String>>("sanitize_header_blacklist") {
        crate::config::set_extra_sensitive_headers(list);
    }

    // 用户脱敏词表（SPEC-1.0.49 §8）：与上面同一模式 —— 词表落 KV，进程内快照
    // （`sanitize::USER_RULES`）不在这里回填的话，重启后 `GET /api/sanitize` 会
    // 报「未配置」（它读快照）而 KV 里其实还留着旧词表，脱敏也静默回落内置规则
    // —— 管理端看着已保存、实际不生效，属假成功。缺省 / `null` = 沿用内置，不动。
    match store.get::<Option<Vec<crate::sanitize::SanitizeRule>>>(
        crate::api::admin::settings::SANITIZE_RULES_KEY,
    ) {
        Ok(Some(rules)) => {
            if let Err(e) = crate::sanitize::set_rules(rules) {
                tracing::warn!("恢复脱敏词表失败，沿用内置规则：{e}");
            }
        }
        Ok(None) => {}
        Err(e) => tracing::warn!("读取脱敏词表失败，沿用内置规则：{e}"),
    }

    // 脱敏 provider 作用域（SPEC-1.0.49 §8）：同款启动恢复；缺省 / 空数组 = 全部。
    match store.get::<Vec<String>>(crate::api::admin::settings::SANITIZE_PROVIDERS_KEY) {
        Ok(Some(list)) => crate::sanitize::set_providers(list),
        Ok(None) => {}
        Err(e) => tracing::warn!("读取脱敏 provider 作用域失败，按「全部」处理：{e}"),
    }

    // 初始化账号型渠道账号池（渠道内第二级调度：账号级优先级/冷却/并发/粘性）
    let account_sessions = crate::account_session::AccountSessionStore::with_egress(
        store.clone(),
        Arc::new(crate::egress::EgressCache::with_timeouts(
            Duration::from_secs(config.usage.api_timeout_secs.max(1)),
            Duration::from_secs(config.usage.connect_timeout_secs.max(1)),
            Duration::from_secs(config.usage.stream_idle_secs.max(1)),
        )),
    );
    // 换号重试间隔（「请求重试」设置；保存设置时也会实时更新）
    account_sessions.set_retry_interval_ms(config.usage.retry_interval_ms);
    account_sessions.set_retry_on_status(config.usage.retry_on_status.clone());
    // 渠道级重试次数（「请求重试」设置；保存设置时也会实时更新）
    crate::config::set_max_retries(config.usage.max_retries);
    // 账号池「同账号重试」档与非流式响应体超时
    crate::config::set_retry_self_count(config.usage.retry_self_count);
    // 切换账号重试次数（映射到 RotationPolicy.max_attempts）
    crate::config::set_retry_switch_count(config.usage.retry_switch_count);
    let mut policy = account_sessions.policy();
    policy.max_attempts = (config.usage.retry_switch_count as usize + 1).clamp(1, 20);
    let _ = account_sessions.set_policy(policy);
    crate::config::set_body_timeout_secs(config.usage.body_timeout_secs);
    // 出站指纹脱敏开关（请求体侧；保存设置时实时更新）
    crate::config::set_sanitize_fingerprints(config.usage.sanitize_fingerprints);
    // 第三方模型回答完自动折叠过程（Responses phase 标记；保存设置时热更新）
    crate::config::set_third_party_reasoning_collapse(config.usage.third_party_reasoning_collapse);
    // 缺省模型回落（请求未带 model 时的默认值；保存设置时热更新）
    crate::config::set_default_model(&config.usage.default_model);
    // Cline 伪装头覆盖表（对齐 Agent2API「设置 → 网关 → Cline 伪装头」）：
    // 启动时从配置恢复进程级快照；保存设置时热更新
    crate::config::set_cline_header_overrides(
        config
            .usage
            .cline_headers
            .overrides
            .clone()
            .into_iter()
            .collect(),
    );
    // ZCode 验证码令牌池的铸造节奏（库存目标 / 轮询间隔 / 单轮上限）：
    // 随 `GET /api/zcode/captcha` 下发给前端铸造器；保存设置时热更新。
    crate::config::set_zcode_captcha_pool_settings(
        config.usage.zcode_captcha_pool_target,
        config.usage.zcode_captcha_pool_poll_ms,
        config.usage.zcode_captcha_pool_max_per_round,
    );
    // 直接换号/换渠道状态码名单（选路时同步判定，保存设置时实时更新）
    crate::config::set_retry_on_status_codes(config.usage.retry_on_status.clone());
    // 初始化模型定价目录
    let pricing_store = Arc::new(PricingStore::new(store.clone()));

    // 初始化全局 IP 过滤（白名单/黑名单，批次3）
    let ip_filter = Arc::new(IpFilterStore::new(store.clone()));
    if let Err(e) = ip_filter.load() {
        tracing::error!("Failed to load IP filter: {e}");
    }

    // 初始化价格同步服务（多源定价同步，批次3；tokio Mutex 跨 await 持锁）
    let price_sync = Arc::new(tokio::sync::Mutex::new(PriceSyncService::new(
        pricing_store.clone(),
    )));

    // 初始化汇率服务（多币种转换，批次4）
    let exchange_rate = Arc::new(ExchangeRateService::with_store(store.clone()));

    // 初始化用户分组存储
    let user_group_store = Arc::new(UserGroupStore::new(store.clone()));

    // 初始化日志与审计存储（功能 1）
    let log_store = Arc::new(LogStore::new(store.clone()));
    // 脚本插件 `script.usage` 回调的日志存储注入：不注入时回调恒返回
    // 「用量日志存储未就绪」，插件上报的用量静默丢失（无报错、无日志）。
    crate::plugin_host::script::usage::install(log_store.clone());
    // 客户端识别规则（KV 持久化 + 进程级快照；数据面逐请求只读快照）
    crate::api::client_id::install(store.clone());
    // 特殊路由（压缩/标题）：启动时装载配置快照；数据面 chat 入口逐请求只读
    crate::route_special::install(store.clone());
    // 插件 provider 模型目录的落盘底座：不装时 `set_remote` 只写内存，重启后
    // 目录全丢（静默）。此前只在 `enable_wasm` 里装 = 从不启用 Wasm 插件的
    // 实例永远落不了盘。
    crate::plugin_host::model_catalog::install(store.clone());
    // 定时任务排期存储（含 wasm 任务循环与云同步的 claim/finish）：不装时
    // 那些循环整轮「存储不可用」跳过。此前靠 `scheduled_tasks::spawn` 内部
    // 装填，顺序依赖只写在注释里。
    crate::scheduled_tasks::task_state::install(store.clone());
    // 在途请求登记表（进程内）：配合日志页「查看并终止正在进行的请求」
    let inflight = Arc::new(crate::log::inflight::InflightRegistry::new());

    // 初始化兑换码存储（功能 2）
    let redemption_store = Arc::new(RedemptionStore::new(store.clone()));

    // 初始化套餐模板存储（按量套餐：模板 → 发 key 交付）
    let plan_store = Arc::new(plan::PlanStore::new(store.clone()));

    // 初始化订阅存储（时长订阅：余额购买 → 独立配额池 + 分组升降级，#85）
    let subscription_store = Arc::new(plan::subscription::SubscriptionStore::new(store.clone()));

    // 初始化工单存储（用户提交问题 / 管理员回复）
    let ticket_store = Arc::new(TicketStore::new(store.clone()));
    let chat_sessions = crate::chat_session::ChatSessionStore::new(store.clone());

    // 初始化限流器（功能 3，带持久化配置）
    let rate_limiter = Arc::new(RateLimiter::with_store(store.clone()));

    // 初始化通知服务（Telegram + SMTP + Slack + Webhook）
    // 启动时先从 FileStore 恢复管理界面保存过的配置（无记录则用 config.toml 值）
    let notify_service = Arc::new(NotifyService::new(config.notify.clone()));
    notify_service.restore_config(&store).await;

    // 初始化告警规则评估器（持久化：FileStore 加载规则集与历史）
    let alert_evaluator = notify::alert_patrol::shared_evaluator_persistent(&store);

    // 共享 HTTP 客户端（性能热点 H5/H6）。
    // 全应用复用同一个 reqwest::Client，避免每次请求新建客户端（连接池/TLS 握手开销）。
    // 超时 300s 覆盖大多数上游推理时长；连接池由 reqwest 内部管理。
    // 强制 HTTP/1.1（参照 rust-tunnel upstream 客户端策略）：reqwest 的
    // read_timeout 按每个 HTTP 帧重置，HTTP/2 的 PING 帧会持续重置读超时，
    // 挂连接将永远检测不到空闲超时——后续若把超时策略收紧为 read_timeout
    // 空闲检测，h2 下挂连接无法检出；且部分上游网关对 h2 连接有激进空闲
    // 超时，连接被静默关闭后复用会批量失败。HTTP/1.1 无此两类风险。
    let http_client = Arc::new(
        reqwest::Client::builder()
            .http1_only()
            .timeout(Duration::from_secs(config.usage.api_timeout_secs.max(1)))
            .connect_timeout(Duration::from_secs(
                config.usage.connect_timeout_secs.max(1),
            ))
            .read_timeout(Duration::from_secs(config.usage.stream_idle_secs.max(1)))
            // 连接池/TCP 调优：空闲连接 30s 回收，单 host 保留至多 16 条复用连接；
            // TCP keepalive 60s 探活挂连接，nodelay 关闭 Nagle 合并降低小包时延。
            .tcp_keepalive(Duration::from_secs(60))
            .pool_max_idle_per_host(16)
            .pool_idle_timeout(Duration::from_secs(30))
            .tcp_nodelay(true)
            .build()
            .expect("failed to build shared reqwest::Client"),
    );
    // SSE 首事件守卫上限随「响应头等待」配置生效（保存设置时也会实时更新）
    crate::bridge::set_sse_first_event_deadline(Duration::from_secs(
        config.usage.headers_timeout_secs.max(1),
    ));

    // 插件宿主（外置插件包的安装/启停/崩溃退避，对齐 Agent2API provider-protocol）：
    // - 包注册表落 KV（`plugin:packs`），启动时 load 一次；
    // - 根目录：config.toml [plugins] plugins_dir 非空则用之，否则 {data_dir}/plugins；
    // - 插件环境：网关版本 / 目标平台（TARGET 三元组）/ 空代理；
    // - [plugins] enabled=true 时启动恢复已启用的包（见下方后台任务）。
    let plugins_root = if config.plugins.plugins_dir.trim().is_empty() {
        data_dir.join("plugins")
    } else {
        std::path::PathBuf::from(config.plugins.plugins_dir.trim())
    };
    let pack_registry = Arc::new(crate::plugin_host::registry::PackRegistry::new(
        store.clone(),
    ));
    if let Err(e) = pack_registry.load() {
        tracing::warn!("[PluginHost] 读取插件包注册表失败（按空注册表启动）: {e}");
    }
    let host_env = crate::plugin_host::process::HostEnv {
        data_dir: data_dir.to_string_lossy().into_owned(),
        host_version: env!("CARGO_PKG_VERSION").to_string(),
        target: crate::plugin_host::host_target_triple(),
        proxy: String::new(),
        debug_traffic: config.usage.debug_traffic,
        loopback_port: config.server.port,
    };
    let plugin_host = Arc::new(crate::plugin_host::PluginHostManager::new(
        pack_registry.clone(),
        plugins_root,
        http_client.as_ref().clone(),
        host_env.clone(),
        Arc::new(account_sessions.clone()),
        store.clone(),
    ));
    // wasm 辅助程序宿主：注入包注册表 + 原生插件进程 + FileStore 授权源。
    // 未接线时 `native.snapshot` 恒返回空表、helper 调用回落 Unavailable
    // （「编译绿但功能半死」形态），故显式接线。
    crate::plugin_host::wasm::native::install_source(
        crate::plugin_host::wasm::native::RegistryHelperSource::new(
            plugin_host.clone(),
            store.clone(),
        ),
    );

    // 脚本插件宿主（一个 Bun 进程承载全部 npm 风格脚本包，对齐 A2A 3.5
    // 的 Magpie 脚本插件体系）：目录刷新任务在 Catalog::spawn 里启动，
    // 已启用包的恢复在下方后台任务（`script_host.start()`）。
    let script_catalog = crate::plugin_host::script::catalog::Catalog::spawn(
        &crate::plugin_host::script::script_plugins_root(&data_dir),
    );
    let script_host = crate::plugin_host::script::ScriptHostManager::new(
        data_dir.clone(),
        store.clone(),
        Arc::new(account_sessions.clone()),
        http_client.as_ref().clone(),
        script_catalog,
        pack_registry,
    );
    // 注入宿主环境：不注入时 `script_host.host_env()` 返回 default()，
    // 脚本包 init 载荷的 `env` 会是空串（data_dir=""/target=""），Bun 宿主
    // 会按空目录跑。这是「编译绿但功能不可用」的半死形态，必须显式接线。
    script_host.set_host_env(host_env);
    // 脚本插件图标的缓存根：此前只经 `script_host.start()`（后台任务里的
    // `icons::spawn`）注入 —— 路由先于该任务可服务时，图标读出口恒
    // 「缓存根未就绪」。启动段同步注入消除这个窗口（install 幂等）。
    crate::plugin_host::script::icons::install(script_host.script_root().join("icons"));

    // 初始化公开注册速率限制器（per-IP，60s 窗口，最多 10000 个 IP 条目）
    // 使用基于 dashmap 的 AsyncCache 替代 moka（离线环境 moka 不可得，行为等效）
    let register_limiter = Arc::new(
        crate::cache::AsyncCache::<String, u32>::builder()
            .max_capacity(10_000)
            .time_to_live(Duration::from_secs(60))
            .build(),
    );

    // 初始化登录限流器（per-IP，60s 窗口，最多 10000 个 IP 条目）
    let login_limiter = Arc::new(
        crate::cache::AsyncCache::<String, u32>::builder()
            .max_capacity(10_000)
            .time_to_live(Duration::from_secs(60))
            .build(),
    );

    // 登录失败锁定状态：<ip> → 连续失败次数。
    //
    // TTL = 锁定时长（配置 `security.lock_minutes`，默认 5 分钟）；三个门槛里
    // 只有它建缓存时固定，改后需重启服务生效 —— 与网关监听地址/端口同口径，
    // 页面会给出同样的提示。其余两项（阈值 / 每分钟上限）走原子快照，保存即生效。
    let security = config_manager.get().await.security.normalized();
    let login_failures = Arc::new(
        crate::cache::AsyncCache::<String, u32>::builder()
            .max_capacity(10_000)
            .time_to_live(Duration::from_secs(security.lock_minutes * 60))
            .build(),
    );

    // 安全策略的进程级快照（登录 handler 同步读取，保存即生效）
    crate::config::set_security_policy(
        security.lockout_enabled,
        security.fail_threshold,
        security.rate_limit_per_minute,
    );
    // 机器人校验开关快照（登录 / 注册 handler 同步读取）
    config::set_security_captcha(security.captcha_enabled);
    // 机器人校验提供方快照（altcha | turnstile + Turnstile site key）
    config::set_security_captcha_provider(&security.captcha_provider, &security.turnstile_site_key);

    // 邮箱验证码登录：<email> → 6 位验证码（TTL=5 分钟）
    let login_code_cache = Arc::new(
        crate::cache::AsyncCache::<String, String>::builder()
            .max_capacity(10_000)
            .time_to_live(Duration::from_secs(300))
            .build(),
    );

    // 2FA/TOTP 待验证缓存：<tmp_token> → email（TTL=5 分钟）
    // 密码验证成功且用户开启 TOTP 时，先生成 tmp_token 挂起，
    // 客户端二次提交 TOTP 码后才签发 session。
    let totp_pending_cache = Arc::new(
        crate::cache::AsyncCache::<String, String>::builder()
            .max_capacity(10_000)
            .time_to_live(Duration::from_secs(300))
            .build(),
    );

    // 2FA/TOTP 启用暂存缓存：<user_id> → base32 secret（TTL=5 分钟）
    // setup 生成的新 secret 先暂存，enable 验证一次后落库；
    // 未在窗口内启用则作废，不产生残留。
    let totp_setup_cache = Arc::new(
        crate::cache::AsyncCache::<String, String>::builder()
            .max_capacity(10_000)
            .time_to_live(Duration::from_secs(300))
            .build(),
    );

    // 会话注册表（P1：会话撤销）——无状态 HMAC token 的服务端撤销层
    let session_registry = Arc::new(api::auth::SessionRegistry::new());

    // 初始化 SeaORM 数据库连接（可选后端）
    //
    // 渐进式迁移策略：
    // - config.database.url 为空 → 使用默认 FileStore，db_conn = None
    // - config.database.url 有值 → 启用 SeaORM，db_conn = Some(conn)
    //
    // 仅当启用 sea-orm feature 时编译此段；否则 db_conn 字段不存在。
    #[cfg(feature = "sea-orm")]
    let db_conn: Option<sea_orm::DatabaseConnection> = {
        if config.database.is_enabled() {
            match db::DatabaseManager::connect(&config.database.url).await {
                Ok(manager) => {
                    tracing::info!(
                        "SeaORM backend enabled: {} (backend={})",
                        crate::config::redact_url_credentials(&config.database.url),
                        manager.backend().as_str()
                    );
                    Some(manager.connection().clone())
                }
                Err(e) => {
                    tracing::error!(
                        "Failed to connect SeaORM backend ({}), falling back to FileStore: {}",
                        crate::config::redact_url_credentials(&config.database.url),
                        e
                    );
                    None
                }
            }
        } else {
            tracing::info!("Database URL not configured, using default FileStore backend");
            None
        }
    };

    // 注册 OpenAI 适配器族 bridge（预留）
    // hub.register_family(Adapter::Openai, cf_openai_bridge);

    // 构建应用状态

    let response_cache = crate::cache::AsyncCache::<String, serde_json::Value>::builder()
        .max_capacity(1000)
        .time_to_live(std::time::Duration::from_secs(300))
        .build();
    let response_cache = std::sync::Arc::new(response_cache);

    let oauth_state_cache = std::sync::Arc::new(
        crate::cache::AsyncCache::<String, i64>::builder()
            .max_capacity(10_000)
            .time_to_live(std::time::Duration::from_secs(600))
            .build(),
    );

    // OAuth 绑定 state → 目标 email（个人中心发起绑定；TTL 10 分钟）
    let oauth_bind_state = std::sync::Arc::new(
        crate::cache::AsyncCache::<String, String>::builder()
            .max_capacity(10_000)
            .time_to_live(std::time::Duration::from_secs(600))
            .build(),
    );
    // OAuth 登录一次性票据 ticket → 会话 token（token 不进 URL，SPA 用 ticket 换回；TTL 2 分钟）
    let oauth_ticket = std::sync::Arc::new(
        crate::cache::AsyncCache::<String, String>::builder()
            .max_capacity(10_000)
            .time_to_live(std::time::Duration::from_secs(120))
            .build(),
    );

    // 服务器自动更新管理器（Release 检测 / 原地替换 / exec 自重启）
    let update = crate::update::UpdateManager::new(store.clone(), data_dir.clone());

    // 网络代理池（网络代理页）：账号 proxy 字段的 pool:/rotate 编码解析依赖它
    crate::proxy_pool::install(store.clone());

    let state = AppState {
        api_client,
        model_mapper,
        model_metadata,
        model_rules,
        usage_tracker,
        account_pool,
        api_key_store,
        config_manager: config_manager.clone(),
        hub,
        user_store,
        order_store,
        checkin_store,
        upstream_checkin,
        epay_client,
        stripe_client,
        health_tracker: health_tracker.clone(),
        livez_state: livez_state.clone(),
        channel_store,
        routing_policy,
        user_tool_policy,
        gateway_prompt,
        purge,
        account_sessions,
        pricing_store,
        user_group_store,
        log_store,
        inflight,
        redemption_store,
        plan_store,
        subscription_store,
        ticket_store,
        chat_sessions,
        rate_limiter,
        notify_service,
        alert_evaluator,
        alert_store: store.clone(),
        ip_filter,
        price_sync,
        exchange_rate,
        register_limiter,
        login_limiter,
        login_failures,
        login_code_cache,
        totp_pending_cache,
        totp_setup_cache,
        session_registry,
        http_client,
        response_cache,
        oauth_state_cache,
        oauth_bind_state,
        oauth_ticket,
        #[cfg(feature = "sea-orm")]
        db_conn,
        // Agent 运行时恒存在；enabled 由运行期配置驱动（设置页保存后立即生效）
        agent_state: Some(std::sync::Arc::new(crate::agent::AgentState::new(
            store.clone(),
            config.agent.clone(),
        ))),
        prompt_source_cache: std::sync::Arc::new(
            crate::api::admin::prompts::PromptSourceCache::new(),
        ),
        update,
        scheduled_task_store: store.clone(),
        provider_gates: {
            let gates = crate::api::admin::plugins::ProviderGates::new(store.clone());
            if let Err(e) = gates.load() {
                tracing::warn!("[Plugins] {e}（按全启用处理）");
            }
            gates
        },
        plugin_host: plugin_host.clone(),
        script_host: script_host.clone(),
        // 工具服务 / 维护运行时（对齐 A2A core::tool_services / core::maintenance；
        // 配置存储 + 排期记录 + 执行入口均已接线，管理面见 api::admin::{tools,maintenance}）
        tool_services: std::sync::Arc::new(crate::tool_services::ToolServices::new(store.clone())),
        maintenance: std::sync::Arc::new(crate::maintenance::Maintenance::new(store.clone())),
    };

    // 工具服务 / 维护启动期初始化（装载已存配置；未配置时用内置默认）
    crate::tool_services::init(state.alert_store.clone());
    crate::maintenance::init(state.alert_store.clone());

    // 账号型搜索运行时：把插件宿主的 provider 搜索能力接进工具服务。
    // 未接线时账号型搜索返回稳定的 `configuration("当前插件宿主未接线")`，
    // 不会 panic；接的是三级路由桥（native → script → wasm，同 resolve.rs 顺序）。
    state
        .tool_services
        .set_account_search_host(std::sync::Arc::new(
            crate::tool_services::PluginHostSearchBridge::new(
                plugin_host.clone(),
                state.script_host.clone(),
            ),
        ));

    tracing::info!(
        "Configuration loaded: {}:{} | Accounts: {} | API Keys: {} | Users: {} | EpayReady: {} | SeaORM: {}",
        config.server.host,
        config.server.port,
        state.account_pool.list().len(),
        state.api_key_store.len(),
        state.user_store.list().len(),
        state.epay_client.config().ready(),
        if state.has_sea_orm_backend() { "enabled" } else { "disabled (FileStore)" },
    );

    // Prometheus 指标快照：渠道数量 + 模型健康等级（/metrics 查询用）。
    // 构建期先填一次，避免首个采集窗口空窗；随后由后台任务每 30s 刷新
    // （渠道增删 / 健康变化不经由 metrics 模块调用点，故不能只写一次）。
    refresh_metrics_snapshot(&state.channel_store, &state.health_tracker);
    spawn_metrics_refresh_task(state.channel_store.clone(), state.health_tracker.clone());

    // 插件宿主启动恢复：把「已启用」的外置包重新拉起（后台任务；单个包起
    // 不来只记错误、按退避表重试，不拖网关启动）。宿主**恒开**（无总开关），
    // 运维的控制面在包级 / provider 级开关上。
    let ph = plugin_host.clone();
    tokio::spawn(async move {
        ph.restore_enabled().await;
    });

    // 脚本宿主启动恢复：有已启用脚本包就拉起 Bun 宿主（后台任务；Bun 首次
    // 可能触发下载，慢启动不拖网关）。无已启用包则什么都不做。
    let sh = script_host.clone();
    tokio::spawn(async move {
        sh.start().await;
    });

    // 构建路由
    // Start CF quota monitoring background task
    crate::quota_monitor::spawn_monitor(
        state.account_pool.clone(),
        state.notify_service.clone(),
        state.http_client.as_ref().clone(),
    );

    // 启动告警巡检后台任务（批次5：渠道健康/内存 → 规则评估 → 多渠道分发）
    notify::alert_patrol::spawn_alert_patrol(
        state.channel_store.clone(),
        state.notify_service.clone(),
        state.alert_evaluator.clone(),
        store.clone(),
    );

    // 启动渠道探活后台任务（批次7c：周期 1-token 探测 → 断路器/健康追踪）
    // 默认关闭：[channel] probe_interval_secs 缺省 0，不对上游发探测请求。
    {
        let probe_interval_secs = state.config_manager.get().await.channel.probe_interval_secs;
        channel::prober::spawn_channel_prober(
            state.channel_store.clone(),
            state.http_client.as_ref().clone(),
            probe_interval_secs,
        );
    }

    // P1-17 定时任务框架：注册会话撤销表清扫（第一个接入的任务）。
    // 撤销记录与会话 TTL 对齐：会话自然过期后撤销记录无意义，
    // 周期清扫避免注册表无限膨胀。清扫门槛 = now - session_ttl，
    // 保证在有效期内被撤销的会话仍被拦截。
    {
        let session_registry = state.session_registry.clone();
        let config_manager = state.config_manager.clone();
        let scheduler = cron::Scheduler::new();
        scheduler.spawn(cron::TaskSpec {
            name: "session-registry-sweep",
            interval: Duration::from_secs(3600),
            first_run_delay: Duration::from_secs(600),
            run: Box::new(move || {
                let reg = session_registry.clone();
                let cfg = config_manager.clone();
                Box::pin(async move {
                    let ttl_hours = cfg.get().await.admin.session_ttl_hours.max(1);
                    let cutoff = chrono::Utc::now().timestamp() - ttl_hours * 3600;
                    reg.sweep_expired_revocations(cutoff) as u64
                })
            }),
        });
        // G2 预留 TTL 解冻：进程崩溃/请求挂死留下的冻结额度定期归还。
        // TTL 30 分钟，远超单请求上限（max_tokens 请求通常在数分钟内完成）。
        {
            let user_store = state.user_store.clone();
            let api_key_store = state.api_key_store.clone();
            scheduler.spawn(cron::TaskSpec {
                name: "reservation-sweep",
                interval: Duration::from_secs(600),
                first_run_delay: Duration::from_secs(300),
                run: Box::new(move || {
                    let us = user_store.clone();
                    let ks = api_key_store.clone();
                    Box::pin(async move {
                        let ttl = 30 * 60;
                        us.release_stale_reservations(ttl) + ks.release_stale_reservations(ttl)
                    })
                }),
            });
        }
        // C1 渠道健康档案落盘 + 30 天滚动清理：内存累加器每 5 分钟 flush，
        // 进程重启/崩溃最多丢一个采样周期的数据（运营趋势可接受）。
        {
            let channel_store = state.channel_store.clone();
            scheduler.spawn(cron::TaskSpec {
                name: "health-archive-flush",
                interval: Duration::from_secs(300),
                first_run_delay: Duration::from_secs(120),
                run: Box::new(move || {
                    let cs = channel_store.clone();
                    Box::pin(async move {
                        let a = cs.health_archive();
                        a.flush() + a.prune()
                    })
                }),
            });
        }
        // 签到中心调度：30 秒轮询，按计划的 HH:MM（UTC+8）触发，当天去重
        {
            let store = state.upstream_checkin.clone();
            let sessions = state.account_sessions.clone();
            let egress = state.account_sessions.egress().clone();
            crate::checkin::scheduler::spawn(store, sessions, egress);
        }
        // 猫猫旅行巡检调度：30 秒 tick，按排程时刻真打上游；无插件时稳定跳过、
        // 不落当日标记（装插件后补跑当天）。
        {
            let store = state.upstream_checkin.clone();
            let executor = api::admin::checkin_center::travel_executor(&state);
            crate::checkin::travel::spawn(store, executor);
        }
        // 账号池周期维护：清理到期冷却记录 + 凭证过期状态同步（每 60s 一轮）
        {
            let sessions = state.account_sessions.clone();
            scheduler.spawn(cron::TaskSpec {
                name: "account-session-sweep",
                interval: Duration::from_secs(60),
                first_run_delay: Duration::from_secs(30),
                run: Box::new(move || {
                    let pool = sessions.clone();
                    Box::pin(async move { crate::account_session::maintenance::sweep(&pool) })
                }),
            });
        }
        // 账号 token 自动刷新：临期（qoder/autoclaw 5 分钟、trae 24 小时）即刷，
        // 凭证自动维护：已迁入 scheduled_tasks 间隔型定时任务循环（`scheduled_tasks::spawn`），
        // 开关与间隔在「定时任务」页可配，排期跨重启保留（见 scheduled_tasks 模块）。
        // 余额自动查询：按账号独立间隔（usage_query_interval_secs）到期即查，
        // 写回快照供界面上次读数展示。未配间隔的账号一律跳过。
        {
            let sessions = state.account_sessions.clone();
            let http = state.http_client.clone();
            scheduler.spawn(cron::TaskSpec {
                name: "account-session-usage-query",
                interval: Duration::from_secs(60),
                first_run_delay: Duration::from_secs(120),
                run: Box::new(move || {
                    let pool = sessions.clone();
                    let http = http.clone();
                    Box::pin(async move {
                        crate::account_session::usage_query::sweep(&pool, &http).await;
                        0
                    })
                }),
            });
        }
        // 渠道余额周期刷新：按渠道独立间隔（balance_query_interval_secs）到期
        // 即查，写回 balance/balance_unit 快照供界面展示。未配间隔的渠道一律
        // 跳过（opt-in）；手动 GET /api/channels/:id/balance 不受影响。
        // 回滚口径：删掉本块即关停，存量渠道字段保留无害。
        {
            let channel_store = state.channel_store.clone();
            let http = state.http_client.clone();
            scheduler.spawn(cron::TaskSpec {
                name: "channel-balance-refresh",
                interval: Duration::from_secs(60),
                first_run_delay: Duration::from_secs(120),
                run: Box::new(move || {
                    let cs = channel_store.clone();
                    let http = http.clone();
                    Box::pin(async move {
                        crate::channel::balance_refresh::sweep(&cs, &http).await;
                        0
                    })
                }),
            });
        }
        // 登录态提醒巡检（#35）：把「需要人工重登」这件事送到站内告警面。
        // 刻意与数据面解耦 —— 每次 401 都要过的路径不顺手拿告警锁（性能红线），
        // 逐出选路是即时的，这里只负责让人看见（≤一个周期的延迟）。
        // 只写告警中心（active + history），不经 NotifyService，那 4 个渠道不动。
        {
            let sessions = state.account_sessions.clone();
            let alerts = state.alert_evaluator.clone();
            scheduler.spawn(cron::TaskSpec {
                name: "account-session-relogin-notice",
                interval: Duration::from_secs(300),
                first_run_delay: Duration::from_secs(45),
                run: Box::new(move || {
                    let pool = sessions.clone();
                    let alerts = alerts.clone();
                    Box::pin(async move {
                        crate::account_session::maintenance::check_relogin(&pool, &alerts)
                    })
                }),
            });
        }
        // models.dev 元数据目录刷新 / 服务器自动更新检查：已迁入 scheduled_tasks
        // 间隔型定时任务循环（见 `scheduled_tasks::spawn`），开关与间隔在「定时任务」页可配。
        // 服务器自动更新检查：已迁入 scheduled_tasks 间隔型定时任务循环
        // （`scheduled_tasks::spawn` 内的「软件版本检查」任务）。
        // 日志保留清理：按运维设置的「保留天数」每日扫一次 reqlog/auditlog
        // 前缀，删除早于截止时间的日志（参照 new-api DeleteOldLogBatch）。
        // 天数未配置时不清理（容量上限 purge_overflow 仍然兜底）。
        {
            let log_store = state.log_store.clone();
            let kv_store = state.scheduled_task_store.clone();
            let cfg_manager = state.config_manager.clone();
            scheduler.spawn(cron::TaskSpec {
                name: "log-retention-sweep",
                interval: Duration::from_secs(86400),
                first_run_delay: Duration::from_secs(600),
                run: Box::new(move || {
                    let ls = log_store.clone();
                    let kv = kv_store.clone();
                    let cm = cfg_manager.clone();
                    Box::pin(async move {
                        let (days, _capacity, audit_days, daily_days) =
                            ls.requests.retention_full();
                        // 容量上限由写入路径的 purge_overflow 消费（同样读
                        // retention 配置），此处只负责按天清理。
                        // 两类日志各自独立：审计可单独设天数（未设时跟随请求日志），
                        // 因此请求日志未设天数时仍要清理审计。
                        let mut removed: u64 = 0;
                        let req_cutoff =
                            days.map(|d| chrono::Utc::now().timestamp() - (d as i64) * 86400);
                        if let Some(cutoff) = req_cutoff {
                            match ls.requests.delete_older_than(cutoff) {
                                Ok(n) => removed += n as u64,
                                Err(e) => tracing::warn!("请求日志保留清理失败: {e}"),
                            }
                        }
                        let audit_cutoff = audit_days
                            .map(|d| chrono::Utc::now().timestamp() - (d as i64) * 86400)
                            .or(req_cutoff);
                        if let Some(cutoff) = audit_cutoff {
                            match ls.audits.delete_older_than(cutoff) {
                                Ok(m) => removed += m as u64,
                                Err(e) => tracing::warn!("审计日志保留清理失败: {e}"),
                            }
                        }
                        // 第三档：日聚合保留（None = 永久）。键区间尾段是
                        // YYYY-MM-DD，含今天 N 个自然日 → cutoff=今天−(N−1)。
                        if let Some(n) = daily_days {
                            let cutoff = crate::log::daily_agg::cutoff_date_key(n);
                            match ls.daily.prune_before(&cutoff) {
                                Ok(k) => removed += k as u64,
                                Err(e) => tracing::warn!("日聚合保留清理失败: {e}"),
                            }
                        }
                        // 第四档：遗留 KV 键清扫（08-prod P2-8）。`token_daily_*` /
                        // `token_monthly_*` 是日聚合上线前的旧格式记账键（数据已
                        // 折进 dailyagg 主档），`price:e2e-*` 是测试残留。清完即
                        // 静默（幂等：之后每轮 0 条）。
                        removed += legacy_kv_sweep(&kv);
                        // 第五档：生成图片资产目录的容量淘汰（#48）。上限
                        // 与日志 max_capacity 同口径（容量而非天数）；落盘
                        // 开关默认关时目录不存在，本档空转、行为不变。
                        let cfg = cm.get().await;
                        let assets_dir = crate::config::expand_path(&cfg.server.data_dir)
                            .join("generated_images");
                        removed += crate::api::openai::sweep_generated_assets(
                            &assets_dir,
                            cfg.server.generated_images_max_bytes,
                        )
                        .removed;
                        // 第六档：终态审批留痕 TTL（#35 批1 后半）。approval.rs 模块头
                        // 一直宣称「终态留痕，7 天后清扫」但全仓无实现，`agent_approval:*`
                        // 只增不减；只删「已终结 ∩ 超 7 天」，pending 一律不动。
                        // kv 与 AgentState 用的是同一个 FileStore（main 构造处 store.clone()），
                        // 所以这里不需要另取 agent_state。
                        removed += crate::agent::approval::sweep_terminal_approvals(
                            &kv,
                            crate::agent::approval::TRACE_TTL_DAYS,
                        );
                        removed
                    })
                }),
            });
        }
        // 日聚合历史回填：聚合表上线前已有明细，启动后一次性折进按天主档
        // （只动 date<今天 的日子，今天归运行期记账）。回填标记存在即跳过
        // ——marker 同时是报表读路径的聚合闸门（阶段 2 的 agg_available）。
        // 24h 周期只为兜住「首次启动失败/期间明细增长」的补跑。
        {
            let log_store = state.log_store.clone();
            scheduler.spawn(cron::TaskSpec {
                name: "daily-agg-backfill",
                interval: Duration::from_secs(86400),
                first_run_delay: Duration::from_secs(60),
                run: Box::new(move || {
                    let ls = log_store.clone();
                    Box::pin(async move {
                        if ls.daily.agg_available() {
                            return 0;
                        }
                        match ls.backfill_history(5000) {
                            Ok(r) => {
                                tracing::info!(
                                    "日聚合回填完成: 扫描 {} 条明细, 主档 {} 天, 用户行 {}, shrink 守卫跳过 {}",
                                    r.scanned,
                                    r.days_written,
                                    r.user_rows_written,
                                    r.skipped_shrink
                                );
                                r.days_written as u64
                            }
                            Err(e) => {
                                tracing::warn!("日聚合回填失败（下一周期重试）: {e}");
                                0
                            }
                        }
                    })
                }),
            });
        }
        // 用户日志索引一次性回填：`list_by_user`（用户积分流水 / 管理面用户
        // 日志）原先全表扫，加 `reqlogu:` 索引后要先把历史日志补齐再开快路径
        // （`RequestLogStore::USER_INDEX_MARKER` 闸门）。幂等：标记在即空转，
        // 24h 周期只为兜住「首次失败」的补跑。
        {
            let log_store = state.log_store.clone();
            scheduler.spawn(cron::TaskSpec {
                name: "reqlog-user-index-backfill",
                interval: Duration::from_secs(86400),
                first_run_delay: Duration::from_secs(90),
                run: Box::new(move || {
                    let ls = log_store.clone();
                    Box::pin(async move {
                        match ls.requests.backfill_user_index() {
                            Ok(0) => 0,
                            Ok(n) => {
                                tracing::info!("用户日志索引回填完成: 补写 {n} 条");
                                n as u64
                            }
                            Err(e) => {
                                tracing::warn!("用户日志索引回填失败（下一周期重试）: {e}");
                                0
                            }
                        }
                    })
                }),
            });
        }
        // 读侧（find_active/try_charge）已做动态过期判断，此处落库保证
        // 用户订阅列表与分组权益最终一致。
        {
            let sub_store = state.subscription_store.clone();
            let user_store = state.user_store.clone();
            let plan_store = state.plan_store.clone();
            scheduler.spawn(cron::TaskSpec {
                name: "subscription-expire-reset",
                interval: Duration::from_secs(60),
                first_run_delay: Duration::from_secs(30),
                run: Box::new(move || {
                    let ss = sub_store.clone();
                    let us = user_store.clone();
                    let ps = plan_store.clone();
                    Box::pin(async move {
                        let now = chrono::Utc::now().timestamp();
                        // 1) 到期扫描：status 落库 + 分组回退
                        let expired = ss.expire_due(now);
                        let mut actions = expired.len() as u64;
                        for sub in &expired {
                            if let Err(e) =
                                crate::plan::subscription::downgrade_user_group(&us, &ss, sub, now)
                            {
                                tracing::error!(
                                    "subscription group downgrade failed for {}: {e}",
                                    sub.id
                                );
                            }
                        }
                        // 2) 周期重置：到点的活跃订阅清零已用配额并推进锚点
                        for sub in ss.list_reset_due(now) {
                            let Some(plan) = ps.get(&sub.plan_id) else {
                                continue;
                            };
                            // 基准点：上次重置时间或购买时间
                            let mut base = if sub.last_reset_time > 0 {
                                sub.last_reset_time
                            } else {
                                sub.start_time
                            };
                            let mut next = crate::plan::subscription::calc_next_reset(
                                base,
                                &plan,
                                sub.end_time,
                            );
                            let mut advanced = false;
                            while next > 0 && next <= now {
                                advanced = true;
                                base = next;
                                next = crate::plan::subscription::calc_next_reset(
                                    base,
                                    &plan,
                                    sub.end_time,
                                );
                            }
                            if !advanced {
                                continue;
                            }
                            if let Err(e) = ss.apply_reset(&sub.id, base, next) {
                                tracing::error!(
                                    "subscription quota reset failed for {}: {e}",
                                    sub.id
                                );
                            } else {
                                actions += 1;
                            }
                        }
                        actions
                    })
                }),
            });
        }
        tracing::info!(
            "cron scheduler started: {} task(s) (session-registry-sweep, reservation-sweep, health-archive-flush, account-session-sweep, account-session-usage-query, channel-balance-refresh, account-session-relogin-notice, log-retention-sweep, daily-agg-backfill, reqlog-user-index-backfill, subscription-expire-reset)",
            scheduler.task_count()
        );
    }

    // 间隔型定时任务调度循环：凭证维护 / 模型刷新 / 版本检查三条后端任务按各自
    // 配置的开关与间隔执行，排期跨重启保留。前端三条自动刷新任务只存配置，由界面读。
    crate::scheduled_tasks::spawn(state.clone());
    tracing::info!(
        "scheduled task loop started (credential maintenance / model refresh / update check)"
    );

    // wasm 插件任务：① 配置持久化（不注入时管理面改的开关/时刻只在内存，重启即丢）；
    // ② 30s 调度循环 —— **必须**在 `scheduled_tasks::spawn` 之后：后者内部做
    // `task_state::install(store)`，而 wasm 任务循环的 claim/finish 依赖它，放前面
    // 会「存储不可用」整轮跳过。两处都是「编译绿但功能不可用」的半死形态。
    crate::plugin_host::wasm::tasks::install_store_settings(state.scheduled_task_store.clone());
    crate::plugin_host::wasm::tasks::spawn();
    tracing::info!("wasm plugin task loop started (30s tick)");

    // 云同步调度循环（WebDAV / S3）：开关与间隔在 SyncConfig（KV）里，排期状态
    // 复用 scheduled_tasks::task_state（键 cloudSync），失败退避跨重启保留。
    crate::sync::spawn(state.clone());
    tracing::info!("cloud sync loop started (WebDAV / S3)");

    // 启动装配自检：进程级单例类底座必须已在启动段装好，漏一处就是一类
    // 「功能第一次被触发才初始化」的历史复发 bug（route_special 的模块头
    // 记录过同类事故：管理面 500、数据面不生效，且不报错）。
    // `debug_assert` 而非 `assert`：release 不 panic，但 debug/CIS 构建会
    // 立刻暴露漏装。
    debug_assert!(
        crate::api::client_id::installed(),
        "client_id 未在启动段装填"
    );
    debug_assert!(
        crate::route_special::installed(),
        "route_special 未在启动段装填"
    );
    debug_assert!(
        crate::plugin_host::model_catalog::installed(),
        "plugin_host::model_catalog 未在启动段装填"
    );
    debug_assert!(
        crate::scheduled_tasks::task_state::installed(),
        "scheduled_tasks::task_state 未在启动段装填"
    );
    debug_assert!(
        crate::plugin_host::script::usage::installed(),
        "script::usage 未在启动段装填"
    );
    debug_assert!(
        crate::plugin_host::script::icons::cache_installed(),
        "script::icons 未在启动段装填"
    );

    // 关停时要 flush 日志队列，故先取出 Arc（state 随后被 build_router 消费）。
    let log_store = state.log_store.clone();
    // 插件宿主也要在 state 被消费前取出（关停序列里优雅关闭子进程）。
    let plugin_host = state.plugin_host.clone();
    let script_host = state.script_host.clone();
    let app = build_router(state, &config);

    // 启动服务器
    let addr = format!("{}:{}", config.server.host, config.server.port);
    tracing::info!("Starting server at {}", addr);

    // 管理面独立监听（可选，对标 Agent2API 2.7.0）：配置了 admin_bind 时
    // 主端口只承载数据面 + 探针 + 第三方回调，管理 API / SPA 走独立端口。
    // 未配置时以下整块逻辑与既往单端口行为逐字一致（零迁移）。
    if let Some(admin_bind) = config.server.admin_bind.clone() {
        const DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);
        server_planes::serve_split(
            &config.server,
            &admin_bind,
            app,
            livez_state.clone(),
            shutdown_signal(),
            DRAIN_TIMEOUT,
        )
        .await?;
        plugin_host.shutdown_all().await;
        crate::plugin_host::wasm::tasks::shutdown();
        log_store.flush();
        tracing::info!("Graceful shutdown complete");
        return Ok(());
    }

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("Server listening on {}", addr);

    // 优雅关闭：收到 SIGTERM/SIGINT 后
    //   1) 标记 draining（/readyz 立刻返回 503，摘流量）
    //   2) axum 停止接受新连接并等待在途请求
    //   3) 等待有上限（20s），超时则强制退出，绝不无限挂起
    //   4) 关闭前 flush 日志异步队列，避免最后一批日志丢失
    // `into_make_service_with_connect_info` 让 handler 能拿到真实对端 IP
    // （ConnectInfo<SocketAddr>）：反向代理未开 trust_proxy_headers 时，
    // 登录/注册限流按真实来源 IP 计数，而不是全站共用一个 "unknown" 键。
    let livez = livez_state.clone();
    // oneshot 只用于「信号已收到」这一事件，让 drain 超时从信号时刻起算，
    // 而不是从进程启动起算（否则正常运行 20s 后也会被误杀）。
    let (drain_started_tx, drain_started_rx) = tokio::sync::oneshot::channel::<()>();
    let serve = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        shutdown_signal().await;
        tracing::info!("Shutdown signal received, marking draining...");
        livez.mark_shutting_down();
        let _ = drain_started_tx.send(());
    });

    const DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);
    let mut serve = Box::pin(std::future::IntoFuture::into_future(serve));
    let serve_result = tokio::select! {
        // 正常路径：所有在途请求完成（或 serve 出错）后返回
        r = &mut serve => Some(r),
        // 兜底路径：信号已到，但在途请求在 20s 内没排空 → 放弃等待强制退出
        _ = async {
            // 信号未到前不启用超时；`Err` 表示启动前 serve 已结束，此时分支永不触发
            if drain_started_rx.await.is_err() {
                std::future::pending::<()>().await;
            }
            tokio::time::sleep(DRAIN_TIMEOUT).await;
        } => None,
    };
    match serve_result {
        Some(result) => result?,
        None => tracing::warn!(
            "Graceful shutdown timed out after {}s of draining; forcing exit with in-flight requests pending",
            DRAIN_TIMEOUT.as_secs()
        ),
    }

    // 插件宿主：优雅关闭全部插件子进程（先 RPC shutdown 等 5s，超时 kill，
    // 不留孤儿进程；崩溃退避监督循环见到 shutdown_requested 标记即退出）。
    plugin_host.shutdown_all().await;
    // wasm 插件任务调度循环：取消后本轮 tick 立即返回、在跑任务在下个边界退出。
    crate::plugin_host::wasm::tasks::shutdown();
    // 脚本宿主：先 RPC shutdown 等宿主自行退出，5s 超时 kill Bun。
    script_host.shutdown().await;

    // 关闭序列最后一步：把日志异步写队列刷干净（内部自带 10s 超时兜底）。
    log_store.flush();
    tracing::info!("Graceful shutdown complete");

    Ok(())
}

/// 等待进程终止信号：Unix 上监听 SIGTERM 或 SIGINT（Ctrl-C），
/// 其他平台（Windows）没有 unix 信号模块，只等 Ctrl-C。
///
/// Docker/K8s 默认发 SIGTERM；不接它会把进程直接硬杀，
/// 在途请求与日志队列全部丢失。
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigterm = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("Failed to install SIGTERM handler: {e}");
                tokio::signal::ctrl_c().await.ok();
                return;
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => tracing::info!("SIGINT received"),
            _ = sigterm.recv() => tracing::info!("SIGTERM received"),
        }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await.ok();
        tracing::info!("ctrl-c received");
    }
}

/// 计算并写入 `aigx_channels_*` 与 `aigx_health_level` 指标快照。
fn refresh_metrics_snapshot(channel_store: &ChannelStore, health: &HealthTracker) {
    let channels = channel_store.snapshot();
    let enabled = channels.iter().filter(|c| c.is_enabled()).count() as u64;
    let disabled = channels.len() as u64 - enabled;
    let m = crate::metrics::global();
    m.set_channels(enabled, disabled);
    m.replace_health(
        health
            .all_health()
            .into_iter()
            .map(|(model, level)| (model, level.into())),
    );
}

/// `/metrics` 中渠道数与健康等级的后台刷新任务（每 30s 一次）。
///
/// 这些值不经过请求处理路径更新，若只在启动写一次，运行期增删渠道
/// 或健康状态变化都不会反映到 Prometheus，导致仪表盘长期显示陈旧数据。
fn spawn_metrics_refresh_task(channel_store: Arc<ChannelStore>, health: Arc<HealthTracker>) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(30));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // 启动时已填过一次，跳过第一次立即触发。
        tick.tick().await;
        loop {
            tick.tick().await;
            refresh_metrics_snapshot(&channel_store, &health);
        }
    });
}

/// JSON 日志格式化器（`AIGX_LOG_FORMAT=json` 时启用）。
///
/// 不依赖 tracing-subscriber 的 `json` feature（未在依赖特性中开启），
/// 手写一个 `FormatEvent`：每行一个 JSON 对象，字段为
/// `timestamp / level / target / message / span` 加上事件自带的结构化字段。
/// 默认（不设该环境变量）仍走文本格式，避免影响现有日志采集。
struct JsonLogFormat;

struct JsonVisitor<'a> {
    map: &'a mut serde_json::Map<String, serde_json::Value>,
}

impl tracing::field::Visit for JsonVisitor<'_> {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.map.insert(
            field.name().to_string(),
            serde_json::Value::String(value.to_string()),
        );
    }

    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        self.map
            .insert(field.name().to_string(), serde_json::Value::Bool(value));
    }

    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        self.map
            .insert(field.name().to_string(), serde_json::json!(value));
    }

    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.map
            .insert(field.name().to_string(), serde_json::json!(value));
    }

    fn record_f64(&mut self, field: &tracing::field::Field, value: f64) {
        self.map
            .insert(field.name().to_string(), serde_json::json!(value));
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.map.insert(
            field.name().to_string(),
            serde_json::Value::String(format!("{value:?}")),
        );
    }
}

impl<S, N> tracing_subscriber::fmt::FormatEvent<S, N> for JsonLogFormat
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
    N: for<'a> tracing_subscriber::fmt::FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &tracing_subscriber::fmt::FmtContext<'_, S, N>,
        mut writer: tracing_subscriber::fmt::format::Writer<'_>,
        event: &tracing::Event<'_>,
    ) -> std::fmt::Result {
        let meta = event.metadata();
        let mut map = serde_json::Map::new();
        map.insert(
            "timestamp".to_string(),
            serde_json::Value::String(chrono::Utc::now().to_rfc3339()),
        );
        map.insert(
            "level".to_string(),
            serde_json::Value::String(meta.level().as_str().to_string()),
        );
        map.insert(
            "target".to_string(),
            serde_json::Value::String(meta.target().to_string()),
        );
        if let Some(span) = ctx.lookup_current() {
            map.insert(
                "span".to_string(),
                serde_json::Value::String(span.name().to_string()),
            );
        }
        event.record(&mut JsonVisitor { map: &mut map });
        let line = serde_json::to_string(&serde_json::Value::Object(map)).unwrap_or_else(|_| {
            "{\"level\":\"ERROR\",\"message\":\"log serialize failed\"}".into()
        });
        writeln!(writer, "{line}")
    }
}

/// 确保配置中存在非空的 session_secret，否则生成随机 UUID 并持久化。
///
/// 修复场景：当 config.admin.session_secret 为空（默认初始状态）时，
/// handle_login 临时生成随机 UUID 作为 secret 签名 session token 返回给客户端，
/// 但该 secret 未持久化，后续 verify_admin/verify_user 仍用空 secret 创建 SessionStore，
/// HMAC 签名不匹配，所有需鉴权的接口返回 401。
///
/// 本函数在启动时一次性补齐并持久化 secret，确保签名与验证使用同一密钥。
async fn ensure_session_secret(config_manager: &ConfigManager) {
    let config = config_manager.get().await;
    if !config.admin.session_secret.is_empty() {
        return;
    }
    let mut new_config = config.clone();
    new_config.admin.session_secret = uuid::Uuid::new_v4().to_string();
    match config_manager.update(new_config).await {
        Ok(_) => tracing::info!("Generated and persisted empty session_secret on first startup"),
        Err(e) => tracing::error!("Failed to persist generated session_secret: {e}"),
    }
}

/// 确保默认管理员账户存在（仅首次启动时创建）
///
/// 首次启动（系统中尚不存在任何 Admin 角色用户）时自动创建内置管理员账户：
/// - 邮箱：`admin@gmail.com`
/// - 用户名：`admin`
/// - 密码：**首次启动随机生成**（18 字节随机数 → base64url 24 字符，
///   `Aigx-` 前缀），仅一次性打印到 stdout/日志，不写入配置文件、不落库。
///
/// 此前密码写死 `123456`：写死的弱口令一旦泄露（仓库/文档/截图）即等于
/// 管理员账号泄露，首登强制改密只是兜底。已存在 Admin 用户时直接跳过，
/// 绝不删除或重建已存在的管理员。
///
/// 邮箱常量在 `crate::user::DEFAULT_ADMIN_EMAIL`（users.rs 锁定逻辑共用）。
const DEFAULT_ADMIN_USERNAME: &str = "admin";

/// 生成一次性默认管理员密码：18 字节随机数 → base64url（24 字符），
/// `Aigx-` 前缀便于肉眼识别。每次调用结果不同。
fn generate_default_admin_password() -> String {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine as _;
    let mut bytes = [0u8; 18];
    getrandom::getrandom(&mut bytes).expect("系统随机源不可用，无法生成默认管理员密码");
    format!("Aigx-{}", URL_SAFE_NO_PAD.encode(bytes))
}

fn ensure_default_admin(user_store: &UserStore) {
    // 检查是否已存在任意 Admin 角色用户；若已存在则跳过，绝不删除/重建
    let has_admin = user_store.list().iter().any(|u| u.role == Role::Admin);
    if has_admin {
        tracing::info!("Admin account already exists, skip default admin initialization");
        return;
    }

    // 随机一次性密码：不写配置文件、不落库，仅本次启动打印一次
    let password = generate_default_admin_password();

    // 创建内置管理员账户
    match user_store.create_with_username(
        crate::user::DEFAULT_ADMIN_EMAIL,
        DEFAULT_ADMIN_USERNAME,
        &password,
        Role::Admin,
        0,
    ) {
        Ok(user) => {
            // 标记「首次登录必须改密」：一次性密码同样不应长期使用，
            // 前端登录后据此强制跳转改密页。
            if let Err(e) = user_store.update(&user.id, |u| {
                u.must_change_password = true;
            }) {
                tracing::warn!("Failed to set must_change_password for default admin: {e}");
            }
            tracing::warn!(
                "First-time setup: built-in admin account created with a RANDOM one-time password. \
                 email={} | username={} | password={} \
                 — PLEASE LOGIN AND CHANGE THE PASSWORD IMMEDIATELY. \
                 This password is not stored anywhere and will not be shown again.",
                crate::user::DEFAULT_ADMIN_EMAIL,
                DEFAULT_ADMIN_USERNAME,
                password,
            );
        }
        Err(e) => {
            tracing::error!("Failed to create default admin account: {}", e);
        }
    }
}

fn build_router(state: AppState, config: &config::AppConfig) -> Router {
    // 请求体大小上限在运行期可经管理后台修改，故此处用中间件每次请求
    // 实时读配置快照（config_manager 为 tokio RwLock），不依赖启动时的
    // config 副本，保证后台改完后无需重启即生效。
    let body_limit_state = state.clone();
    let body_limit_mw = tower::ServiceBuilder::new().map_request(
        move |mut req: axum::http::Request<axum::body::Body>| {
            let limit = body_limit_state.config_manager.max_request_body_mb().max(1) * 1024 * 1024;
            req.extensions_mut()
                .insert(axum::extract::DefaultBodyLimit::max(limit));
            req
        },
    );

    // 注意：axum 0.7（matchit 0.7）的路由参数语法是 `:id`，不是 `{id}`。
    // `{id}` 是 axum 0.8（matchit 0.8）的语法，在 0.7 下会被当作字面量路径段，
    // 导致所有带参数路由匹配失败、请求掉到 fallback_service 返回 405/404。
    // 管理 API 路由（模块化）
    let admin_routes = Router::new()
        // Auth handlers (来自 api::admin::auth)
        .route("/api/auth/login", post(api::admin::handle_login))
        // 公开端点：机器人校验领题（登录页提交前先解一道 ALTCHA proof-of-work）
        .route("/api/auth/captcha", get(api::admin::handle_captcha))
        // 公开端点：按客户端 IP 推荐界面语言（登录页默认语言；无代理头时返回 null）
        .route("/api/public/geo-lang", get(api::admin::handle_geo_lang))
        // 公开端点：面板品牌信息（登录页在拿到会话前就要渲染名称与 Logo）
        .route(
            "/api/public/branding",
            get(api::admin::handle_get_public_branding),
        )
        // 公开端点：品牌 Logo 字节流（登录页/页签图标；带 ETag，免鉴权）
        .route(
            "/api/public/branding/logo",
            get(api::admin::handle_get_public_branding_logo),
        )
        // 系统信息读数（设置页「系统信息」；版本/环境/开关/计数，?hideUsername=1 脱敏路径）
        .route("/api/system-info", get(api::admin::handle_get_system_info))
        .route(
            "/api/auth/login/send-code",
            post(api::admin::handle_login_send_code),
        )
        .route(
            "/api/auth/login/code",
            post(api::admin::handle_login_with_code),
        )
        .route("/api/auth/login/totp", post(api::admin::handle_login_totp))
        // 2FA/TOTP 用户侧自助管理（P1 收尾）
        .route("/api/auth/totp/setup", post(api::admin::handle_totp_setup))
        .route(
            "/api/auth/totp/enable",
            post(api::admin::handle_totp_enable),
        )
        .route(
            "/api/auth/totp/disable",
            post(api::admin::handle_totp_disable),
        )
        .route("/api/auth/register", post(api::admin::handle_register))
        // 公开端点：注册页公共选项（注册开关/邮箱白名单后缀/机器人校验开关）
        .route(
            "/api/auth/register/options",
            get(api::admin::handle_register_options),
        )
        .route(
            "/api/auth/github",
            get(api::admin::handle_github_oauth_authorize),
        )
        .route(
            "/api/auth/github/callback",
            get(api::admin::handle_github_oauth_callback),
        )
        // 阶段1：忘记密码 / 重置密码（参照 burncloud auth.rs）
        .route(
            "/api/auth/forgot-password",
            post(api::admin::handle_forgot_password),
        )
        .route(
            "/api/auth/reset-password",
            post(api::admin::handle_reset_password),
        )
        .route("/api/auth/logout", post(api::admin::handle_logout))
        // session 首屏/刷新（对照 Agent2API GET /api/session + POST /api/session/refresh）
        .route("/api/session", get(api::admin::handle_get_session))
        .route(
            "/api/session/refresh",
            post(api::admin::handle_refresh_session),
        )
        .route(
            "/api/auth/change-password",
            post(api::admin::handle_change_password),
        )
        // 阶段1：Google OAuth
        .route(
            "/api/auth/google",
            get(api::admin::handle_google_oauth_authorize),
        )
        .route(
            "/api/auth/google/callback",
            get(api::admin::handle_google_oauth_callback),
        )
        // LinuxDO OAuth
        .route(
            "/api/auth/linuxdo",
            get(api::admin::handle_linuxdo_oauth_authorize),
        )
        .route(
            "/api/auth/linuxdo/callback",
            get(api::admin::handle_linuxdo_oauth_callback),
        )
        // OAuth 个人中心绑定：start 需登录态（verify_user），返回授权 URL 供前端整页跳；
        // 三家 callback 复用上面同一路由，由 state 命中 oauth_bind_state 判定走绑定流。
        .route(
            "/api/auth/github/bind/start",
            post(api::admin::handle_github_bind_start),
        )
        .route(
            "/api/auth/google/bind/start",
            post(api::admin::handle_google_bind_start),
        )
        .route(
            "/api/auth/linuxdo/bind/start",
            post(api::admin::handle_linuxdo_bind_start),
        )
        .route(
            "/api/auth/oauth/unbind",
            post(api::admin::handle_oauth_unbind),
        )
        // OAuth 登录回跳：callback 只发一次性 ticket，SPA 用它换回会话数据（token 不进 URL）。
        .route(
            "/api/auth/oauth/consume",
            post(api::admin::handle_oauth_consume),
        )
        // 用户管理
        .route("/api/users", get(api::admin::handle_list_users))
        .route("/api/users", post(api::admin::handle_create_user))
        .route("/api/users/check", get(api::admin::handle_check_username))
        .route("/api/users/me", get(api::admin::handle_me))
        // 用户自助与管理操作（对齐 new-api PUT /self、POST /manage）
        .route("/api/users/self", put(api::admin::handle_update_self))
        .route("/api/users/manage", post(api::admin::handle_manage_user))
        // 邀请返利（对齐 new-api /api/user/aff 与 /api/user/aff_transfer）
        .route("/api/aff", get(api::admin::handle_get_aff_code))
        .route("/api/aff_transfer", post(api::admin::handle_aff_transfer))
        // 签到（对齐 new-api GET/POST /api/user/checkin）—— 用户侧，发本站配额
        .route("/api/checkin", get(api::admin::handle_checkin_status))
        .route("/api/checkin", post(api::admin::handle_do_checkin))
        // 用户侧积分流水（签到获得 + API 消耗合并时间线，分页）
        .route("/api/user/records", get(api::admin::handle_user_records))
        // 聊天会话（按用户隔离的对话记录；与 Agent 会话同套机制）
        .route(
            "/api/chat/sessions",
            get(crate::chat_session::handle_list_chat_sessions)
                .post(crate::chat_session::handle_create_chat_session),
        )
        .route(
            "/api/chat/sessions/:id",
            get(crate::chat_session::handle_get_chat_session)
                .delete(crate::chat_session::handle_delete_chat_session),
        )
        .route(
            "/api/chat/sessions/:id/rename",
            post(crate::chat_session::handle_rename_chat_session),
        )
        .route(
            "/api/chat/sessions/:id/messages",
            post(crate::chat_session::handle_append_chat_message)
                .put(crate::chat_session::handle_replace_chat_messages),
        )
        .route(
            "/api/chat/sessions/:id/fork",
            post(crate::chat_session::handle_fork_chat_session),
        )
        // 签到中心（管理员替上游账号签到，与上面的用户签到是两套）
        .route(
            "/api/checkin-center",
            get(api::admin::handle_checkin_center),
        )
        .route(
            "/api/checkin-center/run",
            post(api::admin::handle_checkin_run),
        )
        .route(
            "/api/checkin-center/records",
            get(api::admin::handle_checkin_records)
                .delete(api::admin::handle_clear_checkin_records),
        )
        .route(
            "/api/checkin-center/records/mark-kept-active",
            post(api::admin::handle_mark_kept_active),
        )
        .route(
            "/api/checkin-center/settings",
            get(api::admin::handle_get_checkin_settings)
                .put(api::admin::handle_update_checkin_settings),
        )
        .route(
            "/api/checkin-center/schedules",
            get(api::admin::handle_list_checkin_schedules)
                .post(api::admin::handle_upsert_checkin_schedule),
        )
        .route(
            "/api/checkin-center/schedules/remove",
            post(api::admin::handle_remove_checkin_schedule),
        )
        .route(
            "/api/checkin-center/schedules/run",
            post(api::admin::handle_run_checkin_schedule),
        )
        // 猫猫旅行巡检：PATCH 改开关/时刻，POST 立即真跑一轮
        .route(
            "/api/checkin-center/travel",
            patch(api::admin::handle_patch_checkin_travel),
        )
        .route(
            "/api/checkin-center/travel/run",
            post(api::admin::handle_run_checkin_travel),
        )
        // 一次性 / 限时福利通道（新手礼 / 保活 / 成长任务 / 限时套餐 / UMID）
        .route(
            "/api/checkin-center/benefits",
            get(api::admin::handle_benefits_list),
        )
        .route(
            "/api/checkin-center/benefits/run",
            post(api::admin::handle_benefit_run),
        )
        .route(
            "/api/checkin-center/benefits/keepalive",
            get(api::admin::handle_get_keepalive).put(api::admin::handle_put_keepalive),
        )
        .route("/api/users/:id", put(api::admin::handle_update_user))
        .route("/api/users/:id", delete(api::admin::handle_delete_user))
        // 管理员强制禁用用户 2FA（对齐 new-api DELETE /api/user/:id/2fa）
        .route(
            "/api/users/:id/2fa",
            delete(api::admin::handle_admin_disable_2fa),
        )
        // 工单（用户侧 / 管理侧，对齐 v2board Ticket）
        .route("/api/tickets", get(api::admin::handle_user_list_tickets))
        .route("/api/tickets", post(api::admin::handle_user_save_ticket))
        .route(
            "/api/tickets/reply",
            post(api::admin::handle_user_reply_ticket),
        )
        .route(
            "/api/tickets/close",
            post(api::admin::handle_user_close_ticket),
        )
        .route(
            "/api/admin/tickets",
            get(api::admin::handle_admin_list_tickets),
        )
        .route(
            "/api/admin/tickets/reply",
            post(api::admin::handle_admin_reply_ticket),
        )
        .route(
            "/api/admin/tickets/close",
            post(api::admin::handle_admin_close_ticket),
        )
        // 渠道管理
        .route("/api/channels", get(api::admin::handle_list_channels))
        // 用户侧可用模型聚合（对齐 new-api /api/user/models）
        .route(
            "/api/models/available",
            get(api::admin::handle_available_models),
        )
        // 公开提示词源拉取（Prompts 页一键导入社区提示词）
        .route(
            "/api/prompts/sources",
            get(api::admin::handle_prompt_sources),
        )
        .route("/api/prompts/fetch", post(api::admin::handle_prompt_fetch))
        .route(
            "/api/prompts/translate",
            post(api::admin::handle_prompt_translate),
        )
        .route("/api/channels", post(api::admin::handle_add_channel))
        .route("/api/channels/:id", put(api::admin::handle_update_channel))
        .route("/api/channels/:id", patch(api::admin::handle_patch_channel))
        .route(
            "/api/channels/:id",
            delete(api::admin::handle_delete_channel),
        )
        // 令牌管理
        .route("/api/tokens", get(api::admin::handle_list_tokens))
        .route("/api/tokens", post(api::admin::handle_add_token))
        // 按需取回明文密钥（对齐 new-api POST /api/token/:id/key）
        .route("/api/tokens/:id/key", get(api::admin::handle_get_token_key))
        .route("/api/tokens/:id", put(api::admin::handle_update_token))
        .route("/api/tokens/:id", delete(api::admin::handle_delete_token))
        .route(
            "/api/tokens/:id/reset_used",
            post(api::admin::handle_reset_token_used),
        )
        // 定价管理
        .route("/api/pricing", get(api::admin::handle_list_pricing))
        .route("/api/pricing", post(api::admin::handle_add_pricing))
        .route(
            "/api/pricing/:id",
            delete(api::admin::handle_delete_pricing),
        )
        .route(
            "/api/pricing/estimate",
            post(api::admin::handle_cost_estimate),
        )
        .route(
            "/api/pricing/exchange-rates",
            get(api::admin::handle_get_exchange_rates),
        )
        .route(
            "/api/pricing/exchange-rates",
            put(api::admin::handle_update_exchange_rates),
        )
        .route(
            "/api/pricing/sync",
            post(api::admin::handle_trigger_price_sync),
        )
        // P1 缺失模型检测：启用渠道模型 vs 定价目录差集
        .route(
            "/api/pricing/missing-models",
            get(api::admin::handle_missing_pricing_models),
        )
        // 统一模型管理（渠道模型 + 账号池模型聚合视图）
        .route("/api/models/manage", get(api::admin::handle_manage_view))
        .route("/api/models/state", post(api::admin::handle_set_state))
        // 模型测试（模型管理页「测试」按钮：钉 provider+model 走真实链路）
        .route("/api/models/test", post(api::admin::handle_model_test))
        .route(
            "/api/models/price",
            post(api::admin::handle_upsert_model_price),
        )
        .route(
            "/api/models/price/remove",
            post(api::admin::handle_remove_price),
        )
        .route("/api/models/alias", post(api::admin::handle_set_alias))
        .route(
            "/api/models/alias/state",
            post(api::admin::handle_set_alias_state),
        )
        .route(
            "/api/models/alias/remove",
            post(api::admin::handle_remove_alias),
        )
        .route(
            "/api/models/alias/reasoning",
            post(api::admin::handle_set_alias_reasoning),
        )
        // 批量新增别名前缀映射（`prefix+sep+target` 全局映射，整批先校验后写）
        .route(
            "/api/models/alias/prefix",
            post(api::admin::model_manage::handle_add_alias_prefix),
        )
        // 映射逐条读写（A2A `GET/POST /api/models/mappings`；映射弹窗只拉这一小块）
        .route(
            "/api/models/mappings",
            get(api::admin::model_manage::handle_list_mappings)
                .post(api::admin::model_manage::handle_upsert_mapping),
        )
        .route(
            "/api/models/mappings/remove",
            post(api::admin::model_manage::handle_remove_mapping),
        )
        .route(
            "/api/models/capabilities",
            post(api::admin::handle_set_capabilities),
        )
        .route(
            "/api/models/custom",
            post(api::admin::handle_add_custom_model),
        )
        .route(
            "/api/models/custom/remove",
            post(api::admin::handle_remove_custom_model),
        )
        // P1 模型元信息覆盖管理
        .route("/api/models/meta", get(api::admin::handle_model_meta_list))
        .route(
            "/api/models/meta/:model",
            put(api::admin::handle_model_meta_set),
        )
        .route(
            "/api/models/meta/:model",
            delete(api::admin::handle_model_meta_delete),
        )
        // 易支付配置
        .route("/api/epay/config", get(api::admin::handle_get_epay_config))
        // 用户侧充值页信息（对齐 new-api /api/user/topup/info）
        .route("/api/epay/info", get(api::admin::handle_get_epay_info))
        .route(
            "/api/epay/config",
            put(api::admin::handle_update_epay_config),
        )
        // 订单管理
        .route("/api/orders", get(api::admin::handle_list_orders))
        .route("/api/orders/me", get(api::admin::handle_my_orders))
        .route("/api/topup", post(api::admin::handle_topup_request))
        .route("/api/orders/:id", delete(api::admin::handle_delete_order))
        // 对齐 new-api：订单详情 / 手动补单 / 充值试算
        .route("/api/orders/:id", get(api::admin::handle_get_order))
        .route(
            "/api/orders/:id/complete",
            post(api::admin::handle_complete_order),
        )
        .route("/api/topup/amount", post(api::admin::handle_topup_amount))
        // 日志审计
        .route(
            "/api/logs/requests",
            get(api::admin::handle_list_request_logs),
        )
        .route("/api/logs/audits", get(api::admin::handle_list_audit_logs))
        .route(
            "/api/logs/requests/export",
            get(api::admin::handle_export_request_logs),
        )
        // 日志删除/清空（仅管理员）
        .route(
            "/api/logs/requests",
            delete(api::admin::handle_delete_request_logs),
        )
        .route(
            "/api/logs/requests/clear",
            delete(api::admin::handle_clear_request_logs),
        )
        .route(
            "/api/logs/audits",
            delete(api::admin::handle_delete_audit_logs),
        )
        .route(
            "/api/logs/audits/clear",
            delete(api::admin::handle_clear_audit_logs),
        )
        // 清空预览：先告诉管理员会删多少条，再让他确认
        .route(
            "/api/logs/requests/clear-preview",
            get(api::admin::handle_clear_preview),
        )
        // 在途请求（查看 + 终止）：RequestLog 只在结束后落库，
        // 这里让管理员能看到「谁在跑、跑了多久」并中断卡住的请求。
        .route("/api/logs/inflight", get(api::admin::handle_list_inflight))
        .route(
            "/api/logs/inflight/:id/terminate",
            post(api::admin::handle_terminate_inflight),
        )
        // 日志保留配置（运维设置：保留天数/容量上限/手动清理）
        .route(
            "/api/logs/retention",
            get(api::admin::handle_get_log_retention),
        )
        .route(
            "/api/logs/retention",
            put(api::admin::handle_update_log_retention),
        )
        .route("/api/logs/cleanup", post(api::admin::handle_cleanup_logs))
        // 运行日志环形缓冲（P1-1：level/category 字典 + 500 条环形 + sinceId 水位 + JSONL 导出）
        .route(
            "/api/logs/runtime",
            get(api::admin::handle_get_runtime_logs),
        )
        .route(
            "/api/logs/runtime/export",
            get(api::admin::handle_export_runtime_logs),
        )
        // 日聚合手动重建（逃生门：聚合数字可疑时管理员一键重算历史）
        .route(
            "/api/logs/agg/rebuild",
            post(api::admin::handle_agg_rebuild),
        )
        // 报表时区迁移（逃生门：改了 [report] timezone_offset 后重算存量聚合行，幂等）
        .route(
            "/api/logs/agg/tz-migrate",
            post(api::admin::handle_agg_tz_migrate),
        )
        // 数据看板
        .route(
            "/api/dashboard/consumption_trend",
            get(api::admin::handle_consumption_trend),
        )
        .route(
            "/api/dashboard/model_distribution",
            get(api::admin::handle_model_distribution),
        )
        .route(
            "/api/dashboard/user_ranking",
            get(api::admin::handle_user_ranking),
        )
        .route(
            "/api/dashboard/channel_health",
            get(api::admin::handle_channel_health),
        )
        // 渠道概览统计（渠道数 / 启用 / 冷却；数据源与 channel_health 不同）
        .route(
            "/api/dashboard/channel-stats",
            get(api::admin::dashboard::handle_channel_stats),
        )
        .route("/api/dashboard/realtime", get(api::admin::handle_realtime))
        .route(
            "/api/dashboard/cache_savings",
            get(api::admin::handle_cache_savings),
        )
        .route(
            "/api/dashboard/scheduler_status",
            get(api::admin::handle_scheduler_status),
        )
        // 报表扩展（Agent2API report 视角：热力图/Top来源/Token趋势/总览卡）
        .route(
            "/api/dashboard/report/heatmap",
            get(api::admin::handle_report_heatmap),
        )
        .route(
            "/api/dashboard/report/top_sources",
            get(api::admin::handle_report_top_sources),
        )
        .route(
            "/api/dashboard/report/token_trend",
            get(api::admin::handle_report_token_trend),
        )
        .route(
            "/api/dashboard/report/overview",
            get(api::admin::handle_report_overview),
        )
        .route(
            "/api/dashboard/report/models",
            get(api::admin::handle_report_models),
        )
        .route(
            "/api/dashboard/report/cache_rates",
            get(api::admin::handle_report_cache_rates),
        )
        .route(
            "/api/dashboard/report/cache_trend_24h",
            get(api::admin::handle_report_cache_trend_24h),
        )
        .route(
            "/api/dashboard/report/daily_trend",
            get(api::admin::handle_report_daily_trend),
        )
        // 设置管理
        .route("/api/settings", get(api::admin::handle_get_settings))
        .route("/api/settings", put(api::admin::handle_update_settings))
        // 面板品牌（管理员 GET/PUT；登录页另用公开 GET /api/public/branding）
        .route(
            "/api/branding",
            get(api::admin::handle_get_branding).put(api::admin::handle_update_branding),
        )
        .route(
            "/api/oauth/config",
            get(api::admin::handle_get_oauth_config),
        )
        .route(
            "/api/oauth/config",
            put(api::admin::handle_update_oauth_config),
        )
        .route("/api/limits", get(api::admin::handle_get_limits))
        .route("/api/limits", put(api::admin::handle_update_limits))
        .route(
            "/api/agent/settings",
            get(api::admin::handle_get_agent_settings)
                .put(api::admin::handle_update_agent_settings),
        )
        .route(
            "/api/chat/settings",
            get(api::admin::handle_get_chat_settings).put(api::admin::handle_update_chat_settings),
        )
        // 报表时区配置（[report] timezone_offset，整数小时 UTC 偏移，改后需调迁移端点重算历史）
        .route(
            "/api/report/settings",
            get(api::admin::handle_get_report_settings)
                .put(api::admin::handle_update_report_settings),
        )
        .route(
            "/api/gateway/settings",
            get(api::admin::handle_get_gateway_settings)
                .put(api::admin::handle_update_gateway_settings),
        )
        // 统一错误处理策略（routingErrorPolicy，对照 beta.2 /api/routing/errors）
        .route(
            "/api/routing/errors",
            get(api::admin::handle_get_error_policy).put(api::admin::handle_update_error_policy),
        )
        // 运行容量管理（runtimeCapacity，对照 beta.2 /api/plugins/capacity GET/PATCH）
        .route(
            "/api/plugins/capacity",
            get(api::admin::handle_get_runtime_capacity)
                .patch(api::admin::handle_update_runtime_capacity),
        )
        // 网页工具调用模拟开关（webToolEmulation，对照 beta.2 /api/web-tool-emulation）
        .route(
            "/api/web-tool-emulation",
            get(api::admin::handle_get_web_tools).put(api::admin::handle_update_web_tools),
        )
        // 系统提示词规则引擎（promptRules，对照 beta.2 /api/prompt/rules + preview）
        .route(
            "/api/prompt/rules",
            get(api::admin::handle_get_prompt_rules).put(api::admin::handle_update_prompt_rules),
        )
        .route(
            "/api/prompt/rules/preview",
            post(api::admin::handle_preview_prompt_rules),
        )
        // 插件受控任务授权（pluginManagedTaskGrants，对照 beta.2）
        .route(
            "/api/plugin-task-grants",
            get(api::admin::handle_get_plugin_task_grants)
                .put(api::admin::handle_update_plugin_task_grants),
        )
        // 模型 Key 进程内权威（撤销闸状态 + 重建代）
        .route(
            "/api/model-key-source",
            get(api::admin::handle_get_model_key_source),
        )
        .route(
            "/api/model-key-source/refresh",
            post(api::admin::handle_refresh_model_key_source),
        )
        // 安全策略（登录失败锁定 / 登录限流阈值，对照 Agent2API「设置 → 安全」）
        .route(
            "/api/security/policy",
            get(api::admin::handle_get_security_policy)
                .put(api::admin::handle_update_security_policy),
        )
        // 定时任务（对照 Agent2API「定时任务」页：间隔型任务的开关/间隔/立即执行）
        .route(
            "/api/scheduled-tasks",
            get(api::admin::scheduled_tasks::list_handler),
        )
        .route(
            "/api/scheduled-tasks/:id",
            patch(api::admin::scheduled_tasks::configure_handler)
                .post(api::admin::scheduled_tasks::configure_handler),
        )
        .route(
            "/api/scheduled-tasks/:id/run",
            post(api::admin::scheduled_tasks::run_handler),
        )
        // 调试模式：上游原始报文（对照 Agent2API「设置 → 网关 → 调试模式」）
        .route(
            "/api/debug",
            get(api::admin::handle_get_debug).put(api::admin::handle_update_debug),
        )
        .route(
            "/api/debug/traffic",
            get(api::admin::handle_get_debug_traffic)
                .delete(api::admin::handle_clear_debug_traffic),
        )
        // models.dev 模型元数据（对照 Agent2API /api/models/metadata：开关 + 手动刷新）
        .route(
            "/api/models/metadata",
            get(api::admin::handle_get_model_metadata).put(api::admin::handle_put_model_metadata),
        )
        .route(
            "/api/models/metadata/refresh",
            post(api::admin::handle_refresh_model_metadata),
        )
        .route(
            "/api/models/refresh",
            post(api::admin::handle_refresh_models),
        )
        // 路由策略（对照 Agent2API「设置 → 路由」：策略/粘性/断路器/调度权重）
        .route(
            "/api/routing",
            get(api::admin::handle_get_routing).put(api::admin::handle_update_routing),
        )
        .route(
            "/api/routing/defaults",
            get(api::admin::handle_routing_defaults),
        )
        // 粘滞会话管理面（#38：管理员查看/解除粘滞，替代「只能等 TTL」）
        .route(
            "/api/routing/sticky",
            get(api::admin::handle_get_route_sticky),
        )
        .route(
            "/api/routing/sticky/clear",
            post(api::admin::handle_clear_route_sticky),
        )
        // 排队设置（对照 Agent2API「设置 → 排队」：queueMaxWaits / queueWaitSeconds）
        .route(
            "/api/queue",
            get(api::admin::handle_get_queue).put(api::admin::handle_update_queue),
        )
        // 路由队列预演（对照 Agent2API「设置 → 路由 → 候选队列」：只读，
        // 不推进 round_robin 游标；round_robin 下实际首挑由游标决定）
        .route("/api/routing/queue", get(api::admin::handle_routing_queue))
        // 多规则档案管理面（#40 第 2 步，契约 §5）：整份档案读写 + 单组 CRUD
        // + 判定预演（与 queue 共用同一判定序列，可指定 group_id 预演）
        .route(
            "/api/routing/profiles",
            get(api::admin::handle_get_routing_profiles)
                .put(api::admin::handle_put_routing_profiles),
        )
        .route(
            "/api/routing/profiles/group/:id",
            get(api::admin::handle_get_routing_group)
                .put(api::admin::handle_put_routing_group)
                .delete(api::admin::handle_delete_routing_group),
        )
        .route(
            "/api/routing/preview",
            post(api::admin::handle_routing_preview),
        )
        // 路由工作台（对照 Agent2API v3.5.0-beta.1 workspace/flow/trace/replays）
        .route(
            "/api/routing/workspace",
            get(api::admin::handle_get_routing_workspace),
        )
        .route(
            "/api/routing/flow",
            get(api::admin::handle_get_routing_flow),
        )
        .route(
            "/api/routing/trace",
            get(api::admin::handle_get_routing_trace),
        )
        .route(
            "/api/routing/trace/request",
            get(api::admin::handle_get_routing_trace_request),
        )
        // 回放列表（单条详情仍是既有 /api/routing/replay/:request_id）
        .route(
            "/api/routing/replays",
            get(api::admin::handle_get_routing_replays),
        )
        // 用户工具策略（#34 §5.1，喂 #65 UserTools 配置页）：快照读写 + 工具目录
        // （配置 100% 管理员；终端用户只消费生效结果，不新增用户侧配置端点）
        .route(
            "/api/user-tools/policy",
            get(api::admin::user_tools::handle_get_user_tool_policy)
                .put(api::admin::user_tools::handle_put_user_tool_policy),
        )
        .route(
            "/api/user-tools/catalog",
            get(api::admin::user_tools::handle_get_user_tool_catalog),
        )
        // 路由回放快照（Phase1-B：只读投影，route_replay 开关开启的请求
        // 才有完整数据；管理员鉴权，冷路径）
        .route(
            "/api/routing/replay/:request_id",
            get(api::admin::logs::handle_get_route_replay),
        )
        // 系统提示词注入（对照 Agent2API「设置 → 网关 → 系统提示词」）
        .route(
            "/api/gateway/prompt",
            get(api::admin::handle_get_gateway_prompt)
                .put(api::admin::handle_update_gateway_prompt),
        )
        // 额外脱敏请求头名单（对照 Agent2API「设置 → 网关 → 指纹脱敏」）
        .route(
            "/api/sanitize",
            get(api::admin::handle_get_sanitize).put(api::admin::handle_update_sanitize),
        )
        // 破甲控制台（请求治理总控：归一化/改写/降档/异源/技能注入）
        .route(
            "/api/purge",
            get(api::admin::handle_get_purge).put(api::admin::handle_update_purge),
        )
        // 破甲改写试跑（dry-run：不写统计、不落库）
        .route("/api/purge/preview", post(api::admin::handle_preview_purge))
        // 破甲预设库：检索 / 详情 / 应用到结构化模板或技能目录
        .route(
            "/api/purge/presets",
            get(api::admin::handle_list_purge_presets),
        )
        .route(
            "/api/purge/presets/:id",
            get(api::admin::handle_get_purge_preset),
        )
        .route(
            "/api/purge/presets/:id/apply",
            post(api::admin::handle_apply_purge_preset),
        )
        // 护栏（输入/输出黑名单 + 200 假答复门控；默认全关）
        .route(
            "/api/guardrail",
            get(api::admin::handle_get_guardrail).put(api::admin::handle_update_guardrail),
        )
        .route(
            "/api/guardrail/degrade/clear",
            post(api::admin::handle_clear_guardrail_degrade),
        )
        // 图片压缩与视觉兼容（对照 Agent2API /api/image-compat/settings；
        // 压缩默认开、视觉兼容默认关，保存即热生效）
        .route(
            "/api/image-compat/settings",
            get(api::admin::handle_get_image_compat_settings)
                .put(api::admin::handle_update_image_compat_settings),
        )
        // 视觉兼容规则集（对照 Agent2API /api/vision-compatibility；规则集权威 + revision CAS）
        .route(
            "/api/vision-compatibility",
            get(api::admin::handle_get_vision_compatibility)
                .put(api::admin::handle_update_vision_compatibility),
        )
        // 特殊路由（对照 Agent2API /api/models/compact-routes 与 /title-routes：
        // 压缩/标题改写配置；revision CAS，保存即热生效）
        .route(
            "/api/models/compact-routes",
            get(api::admin::handle_get_compact_routes).put(api::admin::handle_put_compact_routes),
        )
        .route(
            "/api/models/title-routes",
            get(api::admin::handle_get_title_routes).put(api::admin::handle_put_title_routes),
        )
        // Cline 伪装头覆盖表（对照 Agent2API「设置 → 网关 → Cline 伪装头」）
        .route(
            "/api/cline-headers",
            get(api::admin::handle_get_cline_headers).put(api::admin::handle_update_cline_headers),
        )
        // ZCode 人机验证令牌池（对照 Agent2API「网关 → 验证码令牌池」）：
        // 概况/入池走同一路径，铸造节奏单开一条（参照 Agent2API 的
        // /api/zcode/captcha 与 /api/zcode/captcha-pool 两条端点）
        .route(
            "/api/zcode/captcha",
            get(api::admin::handle_get_zcode_captcha).post(api::admin::handle_push_zcode_captcha),
        )
        .route(
            "/api/zcode/captcha/config",
            get(api::admin::handle_get_zcode_captcha_config),
        )
        .route(
            "/api/zcode/captcha-pool",
            get(api::admin::handle_get_zcode_captcha_pool)
                .put(api::admin::handle_update_zcode_captcha_pool),
        )
        // 数据管理（存储概览 + 全量备份/恢复；对照 Agent2API「设置 → 数据」）
        .route("/api/storage", get(api::admin::handle_storage_overview))
        // 明文导出端点已移除（#33 安全缺口本体：全量 KV 含凭证/密钥落浏览器
        // 磁盘）；改用加密导出三件套。明文导入保留（存量旧包迁移）。
        .route("/api/backup/import", post(api::admin::handle_backup_import))
        // 加密整库备份（#33）：口令 AEAD 流式包（AIGXBAK1，Argon2id + AES-256-GCM），
        // 口令只经请求体（不落盘不进日志不进审计载荷）
        .route(
            "/api/backup/export-encrypted",
            post(api::admin::backup_crypto::handle_backup_export_encrypted),
        )
        .route(
            "/api/backup/download",
            get(api::admin::backup_crypto::handle_backup_download),
        )
        .route(
            "/api/backup/import-encrypted",
            post(api::admin::backup_crypto::handle_backup_import_encrypted),
        )
        // 云同步（WebDAV / S3 兼容；对照 Agent2API「数据 → 云同步」）：
        // 设置 / 状态 / 连通性测试 / 立即同步 / 拉取恢复。
        .route(
            "/api/admin/sync/config",
            get(api::admin::sync::handle_get_sync_config)
                .put(api::admin::sync::handle_put_sync_config),
        )
        .route(
            "/api/admin/sync/status",
            get(api::admin::sync::handle_get_sync_status),
        )
        .route(
            "/api/admin/sync/test",
            post(api::admin::sync::handle_sync_test),
        )
        .route(
            "/api/admin/sync/now",
            post(api::admin::sync::handle_sync_now),
        )
        .route(
            "/api/admin/sync/pull",
            post(api::admin::sync::handle_sync_pull),
        )
        // 数据升级（对照 Agent2API /api/upgrade + /api/upgrade/run；AIGX 原生 SQLite 无旧数据，端点保留对齐 API 面）
        .route("/api/upgrade", get(api::admin::handle_get_upgrade))
        .route("/api/upgrade/run", post(api::admin::handle_run_upgrade))
        // 通知配置
        .route(
            "/api/notify/config",
            get(api::admin::handle_get_notify_config),
        )
        .route(
            "/api/notify/config",
            put(api::admin::handle_update_notify_config),
        )
        .route(
            "/api/notify/test-telegram",
            post(api::admin::handle_test_telegram),
        )
        .route(
            "/api/notify/test-email",
            post(api::admin::handle_test_email),
        )
        .route(
            "/api/notify/test-slack",
            post(api::admin::handle_test_slack),
        )
        .route(
            "/api/notify/test-webhook",
            post(api::admin::handle_test_webhook),
        )
        // 告警规则
        .route(
            "/api/alerts/rules",
            get(api::admin::handle_alert_rules_list),
        )
        .route(
            "/api/alerts/rules",
            put(api::admin::handle_alert_rules_update),
        )
        .route("/api/alerts/active", get(api::admin::handle_alerts_active))
        .route(
            "/api/alerts/resolve",
            post(api::admin::handle_alert_resolve),
        )
        .route(
            "/api/alerts/history",
            get(api::admin::handle_alerts_history),
        )
        .route("/api/alerts/test", post(api::admin::handle_alert_test))
        // 缓存管理
        .route("/api/cache/stats", get(api::admin::handle_cache_stats))
        .route("/api/cache/clear", post(api::admin::handle_cache_clear))
        // 安全监控
        .route(
            "/api/monitor/security",
            get(api::admin::handle_security_summary),
        )
        .route(
            "/api/monitor/security/events",
            get(api::admin::handle_security_events),
        )
        // 游乐场
        .route(
            "/api/playground/tts",
            post(api::admin::handle_playground_tts),
        )
        .route(
            "/api/playground/transcriptions",
            post(api::admin::handle_playground_transcriptions),
        )
        // 系统监控
        .route(
            "/api/monitor/system",
            get(api::admin::handle_monitor_system),
        )
        .route("/api/monitor/health", get(api::admin::handle_health))
        .route("/api/monitor/healthz", get(api::admin::handle_healthz))
        // 只读诊断端点套件（AI 可运维第①步：全部只读，管理面鉴权）
        .route(
            "/api/diagnostics/summary",
            get(api::admin::handle_diagnostics_summary),
        )
        .route(
            "/api/diagnostics/channels",
            get(api::admin::handle_diagnostics_channels),
        )
        .route(
            "/api/diagnostics/breakers",
            get(api::admin::handle_diagnostics_breakers),
        )
        // 内嵌 MCP server（AI 可运维第②步：Streamable HTTP，JSON-RPC 2.0 单响应；
        // initialize/tools/list/tools/call 三方法 + 非高危工具白名单（名单与
        // registry::mcp_tools 同源，条数随注册表变化），管理面鉴权）
        .route("/api/mcp", post(api::admin::handle_mcp))
        // 管理面 API 自描述（OpenAPI 3.1，零依赖静态构造；管理面鉴权）
        .route("/api/openapi.json", get(api::admin::handle_openapi_v31))
        // 渠道探活
        .route(
            "/api/channels/fetch_models",
            post(api::admin::handle_fetch_channel_models),
        )
        .route(
            "/api/channels/chat_test",
            post(api::admin::handle_channel_chat_test),
        )
        .route(
            "/api/channels/:id/reset-circuit",
            post(api::admin::handle_reset_channel_circuit),
        )
        .route(
            "/api/channels/:id/health-archive",
            get(api::admin::handle_channel_health_archive),
        )
        .route(
            "/api/channels/:id/test",
            post(api::admin::handle_test_channel),
        )
        .route(
            "/api/channels/:id/balance",
            get(api::admin::handle_get_channel_balance),
        )
        // IP 过滤
        .route("/api/ip/filter", get(api::admin::handle_get_ip_filter))
        .route("/api/ip/filter", put(api::admin::handle_update_ip_filter))
        .route(
            "/api/ip/whitelist",
            post(api::admin::handle_add_ip_whitelist),
        )
        .route(
            "/api/ip/blacklist",
            post(api::admin::handle_add_ip_blacklist),
        )
        .route(
            "/api/ip/whitelist/:pattern",
            delete(api::admin::handle_remove_ip_whitelist),
        )
        .route(
            "/api/ip/blacklist/:pattern",
            delete(api::admin::handle_remove_ip_blacklist),
        )
        // 价格同步配置
        .route(
            "/api/pricing/sync-config",
            get(api::admin::handle_get_price_sync_config),
        )
        .route(
            "/api/pricing/sync-config",
            put(api::admin::handle_update_price_sync_config),
        )
        .route(
            "/api/channels/:id/patch",
            patch(api::admin::handle_patch_channel),
        )
        .route(
            "/api/user/epay/notify",
            post(api::admin::handle_epay_notify).get(api::admin::handle_epay_notify),
        )
        .route(
            "/api/user/epay/return",
            post(api::admin::handle_epay_return).get(api::admin::handle_epay_return),
        )
        .route("/api/stripe/topup", post(api::admin::handle_stripe_topup))
        .route(
            "/api/user/stripe/webhook",
            post(api::admin::handle_stripe_webhook),
        )
        // OpenAPI 文档
        .route(
            "/api-docs/openapi.json",
            get(api::admin::handle_openapi_json),
        )
        .route("/swagger-ui", get(api::admin::handle_swagger_ui))
        // 模型定价
        .route("/api/prices", get(api::admin::handle_list_prices))
        .route("/api/prices", post(api::admin::legacy::handle_upsert_price))
        .route(
            "/api/prices/:model",
            put(api::admin::handle_upsert_price_by_model),
        )
        .route(
            "/api/prices/:model",
            delete(api::admin::handle_delete_price),
        )
        // 用户分组
        .route("/api/groups", get(api::admin::handle_list_groups))
        .route("/api/groups", post(api::admin::handle_upsert_group))
        .route(
            "/api/groups/:name",
            put(api::admin::handle_upsert_group_by_name),
        )
        .route("/api/groups/:name", delete(api::admin::handle_delete_group))
        // 倍率配置
        .route("/api/ratios", get(api::admin::handle_get_ratios))
        .route("/api/ratios", put(api::admin::handle_update_ratios))
        // 兑换码
        .route("/api/redemptions", get(api::admin::handle_list_redemptions))
        .route(
            "/api/redemptions/batch",
            post(api::admin::handle_batch_redemptions),
        )
        .route(
            "/api/redemptions/:id",
            delete(api::admin::handle_delete_redemption),
        )
        .route("/api/redemptions/redeem", post(api::admin::handle_redeem))
        // 套餐管理（按量套餐：模板 CRUD + 按套餐发放 API Key）
        .route("/api/plans", get(api::admin::handle_list_plans))
        .route("/api/plans", post(api::admin::handle_upsert_plan))
        .route("/api/plans/:id", delete(api::admin::handle_delete_plan))
        .route(
            "/api/plans/:id/issue",
            post(api::admin::handle_issue_plan_key),
        )
        // 订阅（#85 时长订阅：用户侧购买 + 管理端绑定/取消）
        .route(
            "/api/subscription/plans",
            get(api::admin::handle_subscription_plans),
        )
        .route(
            "/api/subscription/self",
            get(api::admin::handle_subscription_self),
        )
        .route(
            "/api/subscription/balance/pay",
            post(api::admin::handle_subscription_balance_pay),
        )
        .route(
            "/api/subscription/admin/bind",
            post(api::admin::handle_subscription_admin_bind),
        )
        .route(
            "/api/subscription/admin/users/:id/subscriptions",
            get(api::admin::handle_subscription_admin_list),
        )
        .route(
            "/api/subscription/admin/subscriptions/:id/cancel",
            post(api::admin::handle_subscription_admin_cancel),
        )
        // 限流配置
        .route(
            "/api/ratelimit/config",
            get(api::admin::handle_get_ratelimit_config),
        )
        .route(
            "/api/ratelimit/config",
            put(api::admin::handle_update_ratelimit_config),
        )
        // 令牌增强
        .route("/api/tokens/today", get(api::admin::handle_tokens_today))
        .route("/api/usage/trend", get(api::admin::handle_usage_trend))
        .route("/api/usage/models", get(api::admin::handle_usage_models))
        .route("/api/usage/summary", get(api::admin::handle_usage_summary))
        .route("/api/usage/summary", post(api::admin::handle_refresh_usage))
        // 账号（A2A canonical，对齐 beta.2）：GET 列表 / POST 新增（同路径合并一条 .route）
        .route(
            "/api/accounts",
            get(api::admin::accounts_query::handle_accounts_root)
                .post(api::admin::accounts_query::handle_add_account),
        )
        .route("/api/accounts/test", post(api::admin::handle_test_account))
        // 查询 / 选路 / 置顶 / 重排 / 批量 / 导入 / 签到 / 导出 / 用量 / 连接数
        .route(
            "/api/accounts/query",
            post(api::admin::accounts_query::handle_accounts_query_post),
        )
        .route(
            "/api/accounts/selection",
            post(api::admin::accounts_query::handle_accounts_selection_post),
        )
        .route(
            "/api/accounts/current",
            post(api::admin::accounts_query::handle_set_current_account),
        )
        .route(
            "/api/accounts/reorder",
            post(api::admin::accounts_query::handle_reorder_accounts),
        )
        .route(
            "/api/accounts/batch",
            post(api::admin::accounts_query::handle_batch_accounts),
        )
        .route(
            "/api/accounts/import",
            post(api::admin::accounts_query::handle_import_accounts),
        )
        .route(
            "/api/accounts/checkin",
            post(api::admin::accounts_query::handle_accounts_checkin),
        )
        .route(
            "/api/accounts/refresh",
            post(api::admin::accounts_query::handle_refresh_account),
        )
        .route(
            "/api/accounts/export",
            get(api::admin::accounts_query::handle_export_accounts),
        )
        .route(
            "/api/accounts/usage",
            get(api::admin::accounts_query::handle_accounts_usage),
        )
        .route(
            "/api/accounts/connections",
            get(api::admin::accounts_query::handle_accounts_connections),
        )
        // 用量窗口 scoped 快照（A2A canonical；GET ?ids= / POST {ids:[]}，与 /api/accounts/usage 相邻不串）
        .route(
            "/api/accounts/usage/snapshot",
            get(api::admin::account_sessions::handle_accounts_usage_snapshot)
                .post(api::admin::account_sessions::handle_usage_snapshot_scoped),
        )
        // A2A 形状账号列表（查询/分页/计数/ids + 凭证替换，对齐 beta.2）
        .route(
            "/api/accounts/list",
            get(api::admin::accounts_query::handle_accounts_list_query),
        )
        .route(
            "/api/accounts/summary",
            get(api::admin::accounts_query::handle_accounts_summary),
        )
        .route(
            "/api/accounts/ids",
            get(api::admin::accounts_query::handle_accounts_ids),
        )
        .route(
            "/api/accounts/:id/credentials/replace",
            axum::routing::put(api::admin::accounts_query::handle_replace_account_credentials),
        )
        .route(
            "/api/accounts/:id/move",
            post(api::admin::accounts_query::handle_move_account),
        )
        // 单账号：GET 详情 / PATCH 部分更新 / DELETE 删除（同路径合并一条 .route）
        .route(
            "/api/accounts/:id",
            get(api::admin::accounts_query::handle_account_detail)
                .patch(api::admin::accounts_query::handle_patch_account)
                .delete(api::admin::accounts_query::handle_delete_account),
        )
        // 代理池（对齐 A2A proxies：出口测试 / 系统代理快照 / 连通性设置 / Clash 池管理）
        .route(
            "/api/proxies",
            get(api::admin::proxy_pool_api::handle_proxies_list),
        )
        .route(
            "/api/proxies/system",
            get(api::admin::proxy_pool_api::handle_proxies_system),
        )
        .route(
            "/api/proxies/test",
            post(api::admin::proxy_pool_api::handle_proxies_test),
        )
        .route(
            "/api/proxies/settings",
            get(api::admin::proxy_pool_api::handle_proxies_settings_get)
                .post(api::admin::proxy_pool_api::handle_proxies_settings_save),
        )
        .route(
            "/api/proxies/pool",
            get(api::admin::proxy_pool_api::handle_proxies_pool_list)
                .post(api::admin::proxy_pool_api::handle_proxies_pool_create),
        )
        .route(
            "/api/proxies/pool/update",
            post(api::admin::proxy_pool_api::handle_proxies_pool_update),
        )
        .route(
            "/api/proxies/pool/enable",
            post(api::admin::proxy_pool_api::handle_proxies_pool_enable),
        )
        .route(
            "/api/proxies/pool/remove",
            post(api::admin::proxy_pool_api::handle_proxies_pool_remove),
        )
        .route(
            "/api/proxies/pool/test",
            post(api::admin::proxy_pool_api::handle_proxies_pool_test),
        )
        .route(
            "/api/proxies/pool/sync-clash",
            post(api::admin::proxy_pool_api::handle_proxies_pool_sync_clash),
        )
        // 账号型渠道账号池（账号级调度）：列表/新增/批量导入/改/删/测试/重置冷却
        .route(
            "/api/account-sessions",
            get(api::admin::account_sessions::handle_list_account_sessions)
                .post(api::admin::account_sessions::handle_create_account_session),
        )
        .route(
            "/api/account-sessions/batch",
            post(api::admin::account_sessions::handle_batch_account_sessions),
        )
        .route(
            "/api/account-sessions/providers",
            get(api::admin::account_sessions::handle_list_account_providers),
        )
        .route(
            "/api/account-sessions/rotation",
            get(api::admin::account_sessions::handle_get_rotation_policy)
                .put(api::admin::account_sessions::handle_set_rotation_policy),
        )
        .route(
            "/api/account-sessions/stats",
            get(api::admin::account_sessions::handle_account_session_stats),
        )
        // 各账号实时并发占用快照（连接数列 2s 轮询；静态段与 :id 同级，matchit 静态优先）
        .route(
            "/api/account-sessions/connections",
            get(api::admin::account_sessions::handle_account_session_connections),
        )
        // 登录态供应商的网页登录（服务端无头登录 + 任务表）
        .route(
            "/api/account-sessions/login/start",
            post(api::admin::account_login::handle_login_start),
        )
        .route(
            "/api/account-sessions/login/wait",
            get(api::admin::account_login::handle_login_wait),
        )
        .route(
            "/api/account-sessions/login/cancel",
            post(api::admin::account_login::handle_login_cancel),
        )
        .route(
            "/api/account-sessions/login/callback",
            post(api::admin::account_login::handle_login_callback),
        )
        .route(
            "/api/account-sessions/login/sms/send",
            post(api::admin::account_login::handle_login_sms_send),
        )
        .route(
            "/api/account-sessions/login/sms/verify",
            post(api::admin::account_login::handle_login_sms_verify),
        )
        .route(
            "/api/account-sessions/login/paste",
            post(api::admin::account_login::handle_login_paste),
        )
        // AutoClaw OAuth 登录（Zai / Google）：风控验证码配置 + 发起
        .route(
            "/api/account-sessions/login/oauth/captcha-config",
            get(api::admin::account_login::handle_login_oauth_captcha_config),
        )
        .route(
            "/api/account-sessions/login/oauth/start",
            post(api::admin::account_login::handle_login_oauth_start),
        )
        .route(
            "/api/account-sessions/bulk",
            post(api::admin::account_sessions::handle_bulk_import_account_sessions),
        )
        // 导出/导入 JSON 迁移闭环（导出具明文凭证，仅管理员）
        .route(
            "/api/account-sessions/export",
            get(api::admin::account_sessions::handle_export_account_sessions),
        )
        .route(
            "/api/account-sessions/import/json",
            post(api::admin::account_sessions::handle_import_account_sessions_json),
        )
        .route(
            "/api/account-sessions/:id",
            put(api::admin::account_sessions::handle_update_account_session)
                .patch(api::admin::account_sessions::handle_patch_account_session)
                .delete(api::admin::account_sessions::handle_delete_account_session),
        )
        .route(
            "/api/account-sessions/:id/test",
            post(api::admin::account_sessions::handle_test_account_session),
        )
        .route(
            "/api/account-sessions/:id/reset-cooldown",
            post(api::admin::account_sessions::handle_reset_account_session_cooldowns),
        )
        // 账号级余额/额度查询（qoder / autoclaw / 小浣熊 / zcode）
        .route(
            "/api/account-sessions/:id/balance",
            get(api::admin::account_sessions::handle_account_balance),
        )
        .route(
            "/api/account-sessions/:id/refresh",
            post(api::admin::account_sessions::handle_account_refresh),
        )
        // SPEC-1.0.49 §9（流 H）：组内置顶 + 整队重排（sort_order）。
        // `/order` 是固定段（单段），`:id/promote` 是两段；axum 按段数与静态
        // 优先级匹配，两者与既有 `:id` 家族不冲突。
        .route(
            "/api/account-sessions/:id/promote",
            post(api::admin::account_sessions::handle_promote_account_session),
        )
        .route(
            "/api/account-sessions/order",
            put(api::admin::account_sessions::handle_reorder_account_sessions),
        )
        // 用量窗口（Phase1-A3，对齐上游 accounts_usage.rs）：快照轮询 +
        // 手动批量/单账号查询（写回快照；有界并发 3，温路径）
        .route(
            "/api/account-sessions/usage/snapshot",
            get(api::admin::account_sessions::handle_usage_snapshot),
        )
        // 桌面端登录态导入（#23 desktop 段）：固定路径白名单只读 + 平台门
        //（仅 Windows；DPAPI 形态来源如实不支持）。安全红线见
        // api/admin/desktop_import.rs 模块头
        .route(
            "/api/account-sessions/desktop/sources",
            get(api::admin::desktop_import::handle_desktop_sources),
        )
        .route(
            "/api/account-sessions/desktop-import",
            post(api::admin::desktop_import::handle_desktop_import),
        )
        // 客户端识别（对照 Agent2API core/client 非插件段）：规则 CRUD +
        // 样例预览 + Top 客户端报表。识别只读请求头，不改转发行为；
        // 规则写库后原子替换进程快照，下一个请求立即生效。
        .route(
            "/api/client-id",
            get(api::admin::client_id::handle_get_client_id),
        )
        .route(
            "/api/client-id/rules",
            post(api::admin::client_id::handle_create_client_rule),
        )
        .route(
            "/api/client-id/rules/:id",
            put(api::admin::client_id::handle_update_client_rule)
                .delete(api::admin::client_id::handle_delete_client_rule),
        )
        .route(
            "/api/client-id/preview",
            post(api::admin::client_id::handle_preview_client_id),
        )
        .route(
            "/api/dashboard/report/top_clients",
            get(api::admin::client_id::handle_report_top_clients),
        )
        // 聊天助手工具回路（#34）：工具目录 + 多轮 tool-call 循环
        //（计费走 playground 同链；tools 限管理员，开关默认关）
        .route(
            "/api/assistant/tools",
            get(api::admin::assistant::handle_assistant_tools),
        )
        .route(
            "/api/assistant/chat",
            post(api::admin::assistant::handle_assistant_chat),
        )
        .route(
            "/api/account-sessions/usage",
            post(api::admin::account_sessions::handle_usage_query),
        )
        // 账号池 A2A 对齐新增端点（T4 交付）：服务端排序 / 临期凭证批量刷新 /
        // 逐账号凭证导出 / 异步余额查询任务 / 单账号限速清零。
        // 影子路径 `/api/accounts/*` 与 A2A 上游同口径（前端按上游路径调用），
        // 主前缀仍是 `/api/account-sessions/*`（既有 33 条路由一致）。
        .route(
            "/api/account-sessions/sorting",
            get(api::admin::account_sessions::handle_get_account_sorting)
                .put(api::admin::account_sessions::handle_set_account_sorting),
        )
        .route(
            "/api/account-sessions/refresh-expiring",
            post(api::admin::account_sessions::handle_refresh_expiring),
        )
        .route(
            "/api/account-sessions/export-files",
            post(api::admin::account_sessions::handle_export_account_files),
        )
        .route(
            "/api/account-sessions/usage/start",
            post(api::admin::account_sessions::handle_usage_start),
        )
        .route(
            "/api/account-sessions/usage/cancel",
            post(api::admin::account_sessions::handle_usage_cancel),
        )
        .route(
            "/api/account-sessions/usage/states",
            get(api::admin::account_sessions::handle_usage_states),
        )
        .route(
            "/api/account-sessions/:id/rate-limits/clear",
            post(api::admin::account_sessions::handle_clear_rate_limits),
        )
        .route(
            "/api/accounts/sorting",
            get(api::admin::account_sessions::handle_get_account_sorting)
                .put(api::admin::account_sessions::handle_set_account_sorting),
        )
        .route(
            "/api/accounts/refresh-expiring",
            post(api::admin::account_sessions::handle_refresh_expiring),
        )
        .route(
            "/api/accounts/export-files",
            post(api::admin::account_sessions::handle_export_account_files),
        )
        // 上游模型目录发现（抄 Agent2API 的 provider 模型目录）
        .route(
            "/api/account-sessions/catalog/sync",
            post(api::admin::account_sessions::handle_sync_account_catalog),
        )
        .route(
            "/api/account-sessions/catalog/providers",
            get(api::admin::account_sessions::handle_catalog_providers),
        )
        // 从本机客户端（cc-switch）导入账号
        .route(
            "/api/account-sessions/import/sources",
            get(api::admin::account_sessions::handle_scan_import_sources),
        )
        .route(
            "/api/account-sessions/import",
            post(api::admin::account_sessions::handle_run_import),
        )
        // 从 Agent2API 账号库导入（SQLite 路径或记录 JSON；幂等更新）
        .route(
            "/api/account-sessions/import/agent2api",
            post(api::admin::account_sessions::handle_import_agent2api),
        )
        // 网络代理池（网络代理页）：CRUD / 启停 / 删除（引用检查）/ 连通性测试
        .route(
            "/api/proxy-pool",
            get(api::admin::proxy_pool_api::handle_pool_list)
                .post(api::admin::proxy_pool_api::handle_pool_create),
        )
        .route(
            "/api/proxy-pool/update",
            post(api::admin::proxy_pool_api::handle_pool_update),
        )
        .route(
            "/api/proxy-pool/enable",
            post(api::admin::proxy_pool_api::handle_pool_enable),
        )
        .route(
            "/api/proxy-pool/remove",
            post(api::admin::proxy_pool_api::handle_pool_remove),
        )
        .route(
            "/api/proxy-pool/test",
            post(api::admin::proxy_pool_api::handle_pool_test),
        )
        .route(
            "/api/proxy-pool/test-account",
            post(api::admin::proxy_pool_api::handle_pool_test_account),
        )
        // Clash Verge 出口同步（读取本机 verge.yaml，镜像监听器进池）
        .route(
            "/api/proxy-pool/sync-clash",
            post(api::admin::proxy_pool_api::handle_pool_sync_clash),
        )
        // 插件管理面（对照 Agent2API /api/plugins*；内置目录 + 外置插件包）
        .route(
            "/api/plugins",
            get(api::admin::plugins::handle_list_plugins),
        )
        .route(
            "/api/plugins/install",
            post(api::admin::plugins::handle_install_plugin),
        )
        .route(
            "/api/plugins/enable",
            post(api::admin::plugins::handle_set_plugin_enabled),
        )
        .route(
            "/api/plugins/provider",
            post(api::admin::plugins::handle_set_plugin_provider_enabled),
        )
        .route(
            "/api/plugins/uninstall",
            post(api::admin::plugins::handle_uninstall_plugin),
        )
        .route(
            "/api/plugins/update",
            get(api::admin::plugins::handle_check_plugin_updates),
        )
        .route(
            "/api/plugins/release",
            get(api::admin::plugins::handle_refresh_plugin_release),
        )
        .route(
            "/api/plugins/progress",
            get(api::admin::plugins::handle_get_plugin_progress),
        )
        // beta.2 批量生命周期（后台执行，进度经 /api/plugins/progress 轮询）
        .route(
            "/api/plugins/batch",
            post(api::admin::plugins::handle_batch_plugins),
        )
        // beta.2 受控任务审批（运维显式批准/撤销某包的 managed_task 执行权）
        .route(
            "/api/plugins/managed-task",
            post(api::admin::plugins::handle_managed_task_approval),
        )
        // 脚本插件管理面（Magpie 脚本包：一个 Bun 宿主承载全部 npm 风格脚本）
        .route(
            "/api/plugins/scripts",
            get(api::admin::script_plugins::handle_list_script_packages),
        )
        .route(
            "/api/plugins/scripts/catalog",
            get(api::admin::script_plugins::handle_script_catalog),
        )
        .route(
            "/api/plugins/scripts/refresh",
            post(api::admin::script_plugins::handle_script_refresh),
        )
        .route(
            "/api/plugins/scripts/install",
            post(api::admin::script_plugins::handle_script_install),
        )
        .route(
            "/api/plugins/scripts/enable",
            post(api::admin::script_plugins::handle_script_enable),
        )
        .route(
            "/api/plugins/scripts/uninstall",
            post(api::admin::script_plugins::handle_script_uninstall),
        )
        .route(
            "/api/plugins/scripts/options",
            post(api::admin::script_plugins::handle_script_options),
        )
        .route(
            "/api/plugins/scripts/rebuild",
            post(api::admin::script_plugins::handle_script_rebuild),
        )
        // 脚本插件 A2A 对齐补全（T1 交付）：改设置 / 改 provider 启停 /
        // 提示词清单 / 更新检查 / 目录刷新。
        .route(
            "/api/plugins/scripts/change",
            post(api::admin::script_plugins::handle_script_change),
        )
        .route(
            "/api/plugins/scripts/settings",
            post(api::admin::script_plugins::handle_script_settings),
        )
        .route(
            "/api/plugins/scripts/prompts",
            post(api::admin::script_plugins::handle_script_prompts),
        )
        .route(
            "/api/plugins/scripts/check",
            get(api::admin::script_plugins::handle_script_check),
        )
        .route(
            "/api/plugins/scripts/catalog/refresh",
            post(api::admin::script_plugins::handle_script_catalog_refresh),
        )
        // 服务器自动更新（检查/应用/重启/回滚/设置）
        .route(
            "/api/update/status",
            get(api::admin::update::handle_update_status),
        )
        .route(
            "/api/update/check",
            post(api::admin::update::handle_update_check),
        )
        .route(
            "/api/update/apply",
            post(api::admin::update::handle_update_apply),
        )
        .route(
            "/api/update/restart",
            post(api::admin::update::handle_update_restart),
        )
        .route(
            "/api/update/rollback",
            post(api::admin::update::handle_update_rollback),
        )
        .route(
            "/api/update/cleanup-artifacts",
            post(api::admin::update::handle_update_cleanup_artifacts),
        )
        .route(
            "/api/update/settings",
            get(api::admin::update::handle_get_update_settings)
                .put(api::admin::update::handle_put_update_settings),
        )
        // legacy /api/keys 系列路由已于 1.0.x 删除（07 D 章-3 收敛重复端点）：前端已
        // 全量收敛到 /api/tokens（能力全：分组/额度/轮换/双角色）。此处曾注记「三个
        // handle_*_key 保留、供运维 Agent 复用」——该说法不实：工具面从未引用它们
        // （aigx_key_add/aigx_key_delete 调 handle_add_token/handle_delete_token，
        // aigx_list_keys 已无）。三个死 handler 已于批3（B3-3）清理。
        .route(
            "/api/tokens/:id/rotate",
            post(api::admin::handle_rotate_token),
        )
        // 网络层管理路由（新增 AIGX Network Layer）
        .route("/api/network/status", get(api::admin::health_check))
        .route(
            "/api/network/config/:config_id",
            put(api::admin::update_network_config),
        )
        .route("/api/network/restart", post(api::admin::restart_network))
        .route(
            "/api/network/accounts/:account_id",
            post(api::admin::add_network_account),
        )
        .route(
            "/api/network/accounts/:account_id",
            delete(api::admin::remove_network_account),
        )
        .route(
            "/api/network/accounts",
            get(api::admin::list_network_accounts),
        );

    // EasyPay 回调（异步通知 + 同步跳转）— 无需鉴权
    let epay_callback_routes = Router::new();

    // Stripe 回调
    let stripe_callback_routes = Router::new();

    // OpenAI 兼容 API 路由
    let openai_routes = Router::new()
        .route(
            "/v1/chat/completions",
            post(api::openai::handle_chat_completions),
        )
        .route("/v1/responses", post(api::openai::handle_responses))
        .route("/v1/completions", post(api::openai::handle_completions))
        .route("/v1/embeddings", post(api::openai::handle_embeddings))
        .route("/v1/rerank", post(api::openai::handle_rerank))
        .route(
            "/v1/images/generations",
            post(api::openai::handle_images_generations),
        )
        .route("/v1/images/edits", post(api::openai::handle_images_edits))
        .route(
            "/v1/images/assets/:name",
            get(api::openai::handle_generated_asset),
        )
        .route(
            "/v1/audio/transcriptions",
            post(api::openai::handle_audio_transcriptions),
        )
        .route(
            "/v1/audio/translations",
            post(api::openai::handle_audio_translations),
        )
        .route("/v1/audio/speech", post(api::openai::handle_audio_speech))
        .route("/v1/models", get(api::openai::handle_list_models))
        .route("/v1/models/:model", get(api::openai::handle_get_model))
        // 无前缀兼容：客户端 base_url 填根地址（http://host:9527）时
        // 请求 /chat/completions 等无 /v1 路径 —— new-api 同样双挂，避免 405。
        .route(
            "/chat/completions",
            post(api::openai::handle_chat_completions),
        )
        .route("/responses", post(api::openai::handle_responses))
        .route("/completions", post(api::openai::handle_completions))
        .route("/embeddings", post(api::openai::handle_embeddings))
        .route("/rerank", post(api::openai::handle_rerank))
        .route(
            "/images/generations",
            post(api::openai::handle_images_generations),
        )
        .route("/images/edits", post(api::openai::handle_images_edits))
        .route(
            "/audio/transcriptions",
            post(api::openai::handle_audio_transcriptions),
        )
        .route(
            "/audio/translations",
            post(api::openai::handle_audio_translations),
        )
        .route("/audio/speech", post(api::openai::handle_audio_speech))
        .route("/models", get(api::openai::handle_list_models))
        .route("/models/:model", get(api::openai::handle_get_model));

    // Anthropic 兼容 API 路由（同样双挂无前缀 /messages）
    let anthropic_routes = Router::new()
        .route("/v1/messages", post(api::anthropic::handle_messages))
        .route("/messages", post(api::anthropic::handle_messages))
        .route(
            "/v1/messages/count_tokens",
            post(api::anthropic::handle_count_tokens),
        )
        .route(
            "/messages/count_tokens",
            post(api::anthropic::handle_count_tokens),
        );

    Router::new()
        .merge(admin_routes)
        .merge(epay_callback_routes)
        .merge(stripe_callback_routes)
        .merge(openai_routes)
        .merge(anthropic_routes)
        .merge(crate::agent::api::router())
        // 工具服务数据面（/api/web-search/*、MCP 分发；对齐 A2A api/tool_runtime.rs）
        .merge(crate::api::tool_runtime::router())
        // 工具服务 / 维护管理面（配置、执行与记录；见 api::admin::{tools,maintenance}）
        .merge(crate::api::admin::tools::router())
        .merge(crate::api::admin::tool_image_admin::router())
        .merge(crate::api::admin::maintenance::router())
        .merge(crate::tool_services::router())
        .merge(crate::maintenance::router())
        // 签到台账 / 计划（见 api::admin::{checkin_records,checkin_schedules}）
        .merge(crate::api::admin::checkin_records::router())
        .merge(crate::api::admin::checkin_schedules::router())
        // 插件管理面子页（provider 设置 / 图标 / 主题 / UI / 登录 / 任务 / helpers）
        .merge(crate::api::admin::plugin_provider_settings::router())
        .merge(crate::api::admin::plugin_icons::router())
        .merge(crate::api::admin::plugin_theme::router())
        .merge(crate::api::admin::plugin_ui::router())
        .merge(crate::api::admin::plugin_login::router())
        .merge(crate::api::admin::plugin_tasks::router())
        .merge(crate::api::admin::plugin_helpers::router())
        // 插件管理面新增子路由（startup / settings / reload；T2 交付）
        .merge(crate::api::admin::plugins::router())
        // AutoClaw OAuth 浏览器回调（免鉴权）
        .route(
            "/auth/callback-zai",
            get(api::admin::account_login::handle_browser_oauth_callback),
        )
        .route(
            "/auth/callback-google",
            get(api::admin::account_login::handle_browser_oauth_callback),
        )
        .route("/livez", get(handle_livez))
        .route("/readyz", get(handle_readyz))
        .route("/health", get(handle_health))
        .route("/metrics", get(handle_metrics))
        .fallback_service(web::serve_static_files())
        .layer(build_cors_layer(config))
        .layer(compression_layer())
        .layer(body_limit_mw)
        // 最外层：每请求维护 in-flight 计数（含探针与静态资源请求）
        .layer(axum::middleware::from_fn(track_inflight))
        .with_state(state)
}

/// in-flight 计数守卫：Drop 时把 `aigx_requests_inflight` 减一。
///
/// 用 Drop 而非「handler 返回后减一」是必须的：axum 的响应体是流式的，
/// handler 返回时 body 往往尚未发送完毕；只有 body 被读完或客户端断开
/// （body 被丢弃）时才真正算一次请求结束。
struct InflightGuard(bool);

impl Drop for InflightGuard {
    fn drop(&mut self) {
        if self.0 {
            crate::metrics::global().request_finished();
        }
    }
}

/// 包装响应体，把守卫的生命周期绑到 body 上。
struct InflightBody {
    inner: axum::body::Body,
    _guard: InflightGuard,
    /// 运行容量票据：随 body 结束/丢弃释放并发计数（数据面路径才有）。
    _capacity: Option<crate::runtime_capacity::Ticket>,
}

impl hyper::body::Body for InflightBody {
    type Data = bytes::Bytes;
    type Error = axum::Error;

    fn poll_frame(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<hyper::body::Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        <axum::body::Body as hyper::body::Body>::poll_frame(std::pin::Pin::new(&mut this.inner), cx)
    }

    fn size_hint(&self) -> hyper::body::SizeHint {
        <axum::body::Body as hyper::body::Body>::size_hint(&self.inner)
    }

    fn is_end_stream(&self) -> bool {
        <axum::body::Body as hyper::body::Body>::is_end_stream(&self.inner)
    }
}

/// 每请求 in-flight 计数中间件：进入 +1，响应体结束/丢弃时 -1。
///
/// 顺带做**运行容量准入**（对齐 Agent2API runtimeCapacity）：数据面路径
/// （`/v1/*` 与无前缀兼容路由）按 `Class::Request` 尝试取票，取不到即 503
/// 快速失败；票据随响应体 Drop 释放。管理面/静态资源不占容量。
async fn track_inflight(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    crate::metrics::global().request_started();
    let capacity = if is_data_plane_path(req.uri().path()) {
        match crate::runtime_capacity::try_acquire(crate::runtime_capacity::Class::Request, "") {
            Ok(ticket) => Some(ticket),
            Err(()) => {
                let body = serde_json::json!({
                    "error": {
                        "message": "运行容量暂时不足，请稍后重试",
                        "type": "gateway_busy",
                        "code": "gateway_busy",
                    }
                });
                return axum::response::Response::builder()
                    .status(axum::http::StatusCode::SERVICE_UNAVAILABLE)
                    .header(axum::http::header::CONTENT_TYPE, "application/json")
                    .header(axum::http::header::RETRY_AFTER, "1")
                    .body(axum::body::Body::from(body.to_string()))
                    .unwrap_or_else(|_| axum::response::Response::new(axum::body::Body::empty()));
            }
        }
    } else {
        None
    };
    // 若本中间件在构造响应前被取消（客户端断开/任务 abort），守卫随之 Drop，
    // 计数仍然会被正确回收，不会泄漏。
    let guard = InflightGuard(true);
    let res = next.run(req).await;
    let (parts, body) = res.into_parts();
    axum::response::Response::from_parts(
        parts,
        axum::body::Body::new(InflightBody {
            inner: body,
            _guard: guard,
            _capacity: capacity,
        }),
    )
}

/// 数据面路径判据：`/v1/*` 或与路由表一致的无前缀兼容路径。
fn is_data_plane_path(path: &str) -> bool {
    if path.starts_with("/v1/") {
        return true;
    }
    matches!(
        path,
        "/chat/completions"
            | "/responses"
            | "/completions"
            | "/embeddings"
            | "/rerank"
            | "/images/generations"
            | "/images/edits"
            | "/audio/transcriptions"
            | "/audio/translations"
            | "/audio/speech"
            | "/models"
            | "/messages"
            | "/messages/count_tokens"
    ) || path.starts_with("/models/")
}

/// 遗留 KV 键清扫（log-retention-sweep 第四档，08-prod P2-8）。
///
/// 生产实证的垃圾键：`token_daily_*` / `token_monthly_*`（日聚合上线前的
/// 旧格式记账键，2026-09~11 遗留 17 条，数据已折进 dailyagg 主档）、
/// `price:e2e-*`（E2E 测试残留）。幂等：清完即 0，无副作用的纯删除。
fn legacy_kv_sweep(store: &std::sync::Arc<crate::storage::FileStore>) -> u64 {
    const LEGACY_PREFIXES: &[&str] = &["token_daily_", "token_monthly_", "price:e2e-"];
    let mut removed = 0u64;
    for prefix in LEGACY_PREFIXES {
        let keys = match store.list(prefix) {
            Ok(keys) => keys,
            Err(e) => {
                tracing::debug!("遗留键清扫跳过前缀 {prefix}（读失败：{e}）");
                continue;
            }
        };
        for key in keys {
            match store.delete(&key) {
                Ok(()) => removed += 1,
                Err(e) => tracing::debug!("遗留键 {key} 删除失败：{e}"),
            }
        }
    }
    if removed > 0 {
        tracing::info!(
            "[Retention] 清理遗留 KV 键 {removed} 条（token_daily_*/token_monthly_*/price:e2e-*）"
        );
    }
    removed
}

/// 根据配置构建受限 CORS 层。
///
/// - 若 `config.cors_origins` 非空：仅允许列表中的来源（生产环境推荐）。
/// - 若 `config.cors_origins` 为空但 `config.server_address` 非空：允许 server_address 来源。
/// - 否则：默认允许 localhost 开发来源（127.0.0.1 与 localhost，常见端口）。
///
/// 不再使用 `CorsLayer::permissive()` 以避免任意来源带来的安全风险。
fn build_cors_layer(config: &config::AppConfig) -> CorsLayer {
    use axum::http::HeaderValue;

    let origins: Vec<HeaderValue> = if !config.cors_origins.is_empty() {
        config
            .cors_origins
            .iter()
            .filter_map(|s| s.trim().parse::<HeaderValue>().ok())
            .collect()
    } else if !config.server_address.trim().is_empty() {
        // 兜底：使用 server_address 作为唯一允许来源
        config
            .server_address
            .trim()
            .parse::<HeaderValue>()
            .into_iter()
            .collect()
    } else {
        // 无配置：不发 CORS 头（AllowOrigin::list 为空数组 → 不匹配任何外部来源），
        // 浏览器同源请求不受影响；跨源请求将被浏览器拦截。
        // 对外暴露时务必在 config 中显式配置 `cors_origins`。
        Vec::new()
    };

    if origins.is_empty() {
        tracing::warn!(
            "No valid CORS origins configured; falling back to localhost-only. \
             Set `cors_origins` in config for production."
        );
    }

    CorsLayer::new()
        .allow_origin(AllowOrigin::list(origins))
        .allow_methods([
            axum::http::Method::GET,
            axum::http::Method::POST,
            axum::http::Method::PUT,
            axum::http::Method::PATCH,
            axum::http::Method::DELETE,
            axum::http::Method::OPTIONS,
        ])
        .allow_headers([
            axum::http::header::AUTHORIZATION,
            axum::http::header::CONTENT_TYPE,
            axum::http::header::ACCEPT,
            // Anthropic 客户端用 x-api-key 头（与 Authorization 二选一）
            axum::http::header::HeaderName::from_static("x-api-key"),
        ])
        .allow_credentials(true)
}

/// gzip 压缩中间件：对可压缩响应（JSON/HTML/CSS/JS/文本）做 gzip。
///
/// 公开提示词源单次可达 1.1MB JSON，压缩后约 300KB，显著降低带宽；
/// SSE 流式响应（text/event-stream）不含 `Content-Length` 时不做压缩，
/// 保持流式透传。静态文件已由 `ServeDir` 按预压缩/按需处理，此处只
/// 兜底 API 响应与 SPA index.html。
fn compression_layer() -> CompressionLayer<
    tower_http::compression::predicate::And<
        tower_http::compression::DefaultPredicate,
        tower_http::compression::predicate::SizeAbove,
    >,
> {
    use tower_http::compression::predicate::{DefaultPredicate, SizeAbove};
    use tower_http::compression::Predicate;

    // 必须用 `.and()` 组合 DefaultPredicate 与 SizeAbove：DefaultPredicate
    // 按 Content-Type 排除 text/event-stream（SSE 流式透传不能被 gzip 破坏），
    // SizeAbove 跳过小于 256 字节的响应（错误 JSON、探针等，压缩收益为负）。
    // 若只用 SizeAbove 替换默认谓词，会丢失 event-stream 排除，把 SSE 流也压坏。
    CompressionLayer::new().compress_when(DefaultPredicate::new().and(SizeAbove::new(256)))
}

/// GET /metrics — Prometheus 指标文本输出
///
/// 指标含各渠道模型调用量/成本/延迟等业务敏感数据，不设防会向任意
/// 未认证访客泄露运营概况。
///
/// 鉴权（满足其一即可）：
/// 1. `Authorization: Bearer <AIGX_METRICS_TOKEN>` —— 供 Prometheus 采集器
///    使用；仅当环境变量 `AIGX_METRICS_TOKEN` 已配置时该方式生效。
///    采用常量时间比较，避免按字节比较泄露 token 前缀。
/// 2. 管理员会话 —— 保留原有行为，供运维在后台手工查看。
///
/// 未配置 token 且无有效会话时返回 401（绝不匿名开放）。
async fn handle_metrics(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    use axum::http::{header::CONTENT_TYPE, HeaderValue, StatusCode};
    use axum::response::IntoResponse;

    let bearer_ok = metrics_bearer_authorized(&headers);
    if !bearer_ok
        && crate::api::admin::common::verify_admin(&state, &headers)
            .await
            .is_err()
    {
        return (
            StatusCode::UNAUTHORIZED,
            [(
                CONTENT_TYPE,
                HeaderValue::from_static("text/plain; charset=utf-8"),
            )],
            "unauthorized",
        )
            .into_response();
    }
    let headers = [(
        CONTENT_TYPE,
        HeaderValue::from_static("text/plain; version=0.0.4; charset=utf-8"),
    )];
    (StatusCode::OK, headers, metrics::global().render()).into_response()
}

/// 校验 `Authorization: Bearer <AIGX_METRICS_TOKEN>`。
///
/// 未设置 `AIGX_METRICS_TOKEN` 时恒为 false（该通道不可用，回落到会话鉴权）。
fn metrics_bearer_authorized(headers: &axum::http::HeaderMap) -> bool {
    let Ok(expected) = std::env::var("AIGX_METRICS_TOKEN") else {
        return false;
    };
    if expected.is_empty() {
        return false;
    }
    let Some(value) = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    else {
        return false;
    };
    let Some(token) = value
        .strip_prefix("Bearer ")
        .or_else(|| value.strip_prefix("bearer "))
    else {
        return false;
    };
    constant_time_eq(token.trim().as_bytes(), expected.as_bytes())
}

/// 常量时间字节比较：长度不同直接 false，长度相同则逐字节累加异或，
/// 不提前 return，避免比较耗时随匹配前缀长度变化。
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// GET /livez — 存活检查
async fn handle_livez(State(state): State<AppState>) -> axum::response::Response {
    health::livez_response(&state.livez_state, false)
}

/// GET /readyz — 就绪检查
///
/// 除 draining 状态与配置就绪外，再做一次**轻量**存储读探测：
/// 读一个从不写入的探针键，读不到（`Ok(None)`）视为正常——目标是验证
/// 「存储层可访问」，而不是键存在。仅当读操作本身报错（DB 打不开/查询失败）
/// 才判失败并返回 503。`/readyz` 被容器探针高频调用，因此只做单次 KV 读。
async fn handle_readyz(State(state): State<AppState>) -> axum::response::Response {
    let storage_ok = storage_probe(&state.alert_store).await;
    health::readyz_response(&state.livez_state, true, storage_ok, false)
}

/// 轻量存储可访问性探测。
///
/// `FileStore::get` 是同步阻塞调用（SQLite/PG），放到阻塞线程池执行，
/// 避免在 async handler 里阻塞 tokio worker。用 `spawn_blocking` 的 join
/// 失败（panic、runtime 关闭）同样视为探测失败。
async fn storage_probe(store: &std::sync::Arc<crate::storage::FileStore>) -> bool {
    let store = store.clone();
    tokio::task::spawn_blocking(move || store.get::<serde_json::Value>("__readyz_probe").is_ok())
        .await
        .unwrap_or(false)
}

/// GET /health — 模型健康状态汇总
async fn handle_health(State(state): State<AppState>) -> axum::Json<serde_json::Value> {
    let models = state.health_tracker.all_health();
    let mut result = serde_json::Map::new();
    for (model, level) in models {
        result.insert(model, serde_json::json!(u8::from(level)));
    }
    axum::Json(serde_json::Value::Object(result))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// P0：一次性随机密码——格式（前缀 + 长度 + base64url 字符集）与唯一性。
    #[test]
    fn default_admin_password_is_random_and_wellformed() {
        let p1 = generate_default_admin_password();
        let p2 = generate_default_admin_password();
        assert!(p1.starts_with("Aigx-"), "格式：{p1}");
        assert_eq!(
            p1.len(),
            "Aigx-".len() + 24,
            "18 字节随机数 base64url = 24 字符：{p1}"
        );
        assert!(
            p1.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "仅 base64url 字符集：{p1}"
        );
        assert_ne!(p1, p2, "两次生成必须不同（随机）");
        assert_ne!(p1, "123456", "不得再是写死的弱口令");
    }

    /// P0：首次启动建号——随机密码可登录、强制首登改密；二次调用不重复建号。
    #[test]
    fn ensure_default_admin_creates_random_password_admin_once() {
        let dir = std::env::temp_dir().join(format!("aigx-main-admin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = Arc::new(FileStore::new(dir.join("data")));
        let user_store = UserStore::new(store);

        ensure_default_admin(&user_store);
        let users = user_store.list();
        assert_eq!(users.len(), 1, "首次启动建一个管理员");
        let admin = &users[0];
        assert_eq!(admin.role, Role::Admin);
        assert_eq!(admin.email, crate::user::DEFAULT_ADMIN_EMAIL);
        assert!(admin.must_change_password, "一次性密码必须强制首登改密");
        // 旧写死口令不得再可登录
        assert!(
            user_store
                .authenticate(crate::user::DEFAULT_ADMIN_EMAIL, "123456")
                .is_none(),
            "写死的 123456 不得再可登录"
        );

        // 二次调用跳过（不重复建号、不覆盖已有管理员）
        ensure_default_admin(&user_store);
        assert_eq!(user_store.list().len(), 1, "已有管理员时跳过");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
