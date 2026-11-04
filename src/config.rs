use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::sync::RwLock;

use crate::notify::NotifyConfig;
use crate::oauth::github::GithubOauthConfig;
use crate::oauth::google::GoogleOauthConfig;
use crate::oauth::linuxdo::LinuxDoOauthConfig;
use crate::payment::stripe::StripeConfig;
use crate::payment::EpayConfig;

/// 默认 cf-ai-gw Worker 地址（AI Binding 方式调用 Cloudflare Workers AI）。
///
/// AIGX 不再直接调用 Cloudflare REST API（`api.cloudflare.com`），而是通过
/// cf-ai-gw Worker 桥接——Worker 内部使用 **AI Binding**（`env.AI.run()`）调用
/// Workers AI。本地址即 cf-ai-gw Worker 的部署地址。
pub const DEFAULT_CF_BINDING_URL: &str = "http://127.0.0.1:8787";

// ── Default value functions ──────────────────────────────────────────

fn default_host() -> String {
    "127.0.0.1".to_string()
}

fn default_port() -> u16 {
    8080
}

fn default_data_dir() -> String {
    "~/.aigx".to_string()
}

fn default_daily_limit() -> u64 {
    10000
}

fn default_monthly_limit() -> u64 {
    100_000
}

/// 注册赠送配额默认 200（用户要求：注册即送 200 额度）。
fn default_register_quota() -> i64 {
    200
}

/// 注册开关默认开启（向后兼容：升级不停用既有注册入口）。
fn default_register_enabled() -> bool {
    true
}

// ── Config structs ───────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_data_dir")]
    pub data_dir: String,
    /// 日志目录（可选，默认 = data_dir/logs）
    #[serde(default)]
    pub log_dir: Option<String>,
    /// 请求统计目录（可选，默认 = data_dir/stats）
    #[serde(default)]
    pub request_stats_dir: Option<String>,
    /// 调试数据目录（可选，默认 = data_dir/debug）
    #[serde(default)]
    pub debug_dir: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AdminConfig {
    /// 会话签名密钥。为空时首次启动由 `ensure_session_secret` 生成并持久化。
    #[serde(default)]
    pub session_secret: String,
    /// 会话有效期（小时），默认 24。
    #[serde(default = "default_session_ttl")]
    pub session_ttl_hours: i64,
}

fn default_session_ttl() -> i64 {
    24
}

fn default_cf_binding_url() -> String {
    DEFAULT_CF_BINDING_URL.to_string()
}

/// 数据库配置 — 多数据库后端支持。
///
/// 渐进式迁移策略：
/// - `url` 为空（默认）：使用现有 FileStore（rusqlite bundled SQLite），零配置零依赖
/// - `url` 有值：启用 SeaORM 连接，支持 PostgreSQL/MySQL
///
/// 支持的 URL 格式：
/// - `postgres://user:pass@localhost:5432/aigx` — PostgreSQL
/// - `mysql://user:pass@localhost:3306/aigx` — MySQL
///
/// 注意：启用 SeaORM 后端需要编译时启用对应 feature：
/// ```text
/// cargo build --no-default-features --features "sea-orm,postgres"
/// cargo build --no-default-features --features "sea-orm,mysql"
/// ```
///
/// SQLite 场景由默认的 FileStore/rusqlite 后端覆盖，无需 SeaORM。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DatabaseConfig {
    /// 数据库连接 URL。留空则使用默认 FileStore（rusqlite）。
    ///
    /// 示例：
    /// - `sqlite://./data/aigx.db`
    /// - `postgres://user:pass@localhost:5432/aigx`
    /// - `mysql://user:pass@localhost:3306/aigx`
    #[serde(default)]
    pub url: String,
    /// 连接池最大连接数（默认 10）
    #[serde(default = "default_max_connections")]
    pub max_connections: u32,
}

fn default_max_connections() -> u32 {
    10
}

impl DatabaseConfig {
    /// 是否启用 SeaORM 后端（url 非空时启用）。
    pub fn is_enabled(&self) -> bool {
        !self.url.trim().is_empty()
    }
}

/// 渠道调度配置（`[channel]` 段）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChannelConfig {
    /// 主动探活巡检周期（秒）。**默认 0 = 关闭**——部署后不对上游渠道
    /// 发任何探测请求，渠道健康完全依赖真实流量的被动熔断。
    ///
    /// 需要主动探活时显式配置（如 3600 = 每小时一轮，每轮对每个启用
    /// 渠道发一次 1-token 探测，会消耗上游请求）。
    #[serde(default)]
    pub probe_interval_secs: u64,
}

/// 服务器自动更新配置（`[update]` 段）。
///
/// 只支持 Linux 服务器 / 容器（原地替换二进制与 static 目录后 exec 自重启）；
/// 其它平台仍可手动检查版本，应用更新时返回明确的不支持提示。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateConfig {
    /// 是否启用更新模块（关闭后定时任务不再检查，手动检查/更新仍可用）。
    #[serde(default = "default_update_enabled")]
    pub enabled: bool,
    /// 是否按 `interval_hours` 周期自动检查新版本。
    #[serde(default = "default_update_auto_check")]
    pub auto_check: bool,
    /// 发现新版本后是否自动应用（下载 → 校验 → 替换 → 重启），默认关闭。
    #[serde(default)]
    pub auto_apply: bool,
    /// 自动检查间隔（小时，最小 1）。
    #[serde(default = "default_update_interval_hours")]
    pub interval_hours: u64,
    /// 更新来源仓库（`owner/repo`）。
    #[serde(default = "default_update_repo")]
    pub repo: String,
    /// 自建更新源根地址（与 GitHub API 同形）；空 = 官方 GitHub API。
    #[serde(default)]
    pub api_base: String,
    /// 是否把预发布版本也纳入更新候选。
    #[serde(default)]
    pub include_prerelease: bool,
    /// 访问更新源的代理（http/https/socks5），空 = 直连（仍读环境变量代理）。
    #[serde(default)]
    pub proxy: String,
    /// GitHub 令牌（可选，提升 API 限额；不填走匿名 60 次/小时）。
    #[serde(default)]
    pub token: String,
    /// 应用更新后是否自动重启服务（exec 自重启），默认开启。
    #[serde(default = "default_update_restart")]
    pub restart_after_apply: bool,
}

fn default_update_enabled() -> bool {
    true
}

fn default_update_auto_check() -> bool {
    true
}

fn default_update_interval_hours() -> u64 {
    24
}

fn default_update_repo() -> String {
    "ojbkxc/AIGX".to_string()
}

fn default_update_restart() -> bool {
    true
}

impl Default for UpdateConfig {
    fn default() -> Self {
        Self {
            enabled: default_update_enabled(),
            auto_check: default_update_auto_check(),
            auto_apply: false,
            interval_hours: default_update_interval_hours(),
            repo: default_update_repo(),
            api_base: String::new(),
            include_prerelease: false,
            proxy: String::new(),
            token: String::new(),
            restart_after_apply: default_update_restart(),
        }
    }
}

/// AI 运维 Agent 配置（`[agent]` 段）——自举式运维工作台。
///
/// Agent 的"大脑"走 AIGX 自己的渠道（进程内直调 bridge，复用渠道调度/
/// 熔断/亲和，但不经 HTTP 端口与计费），模型与渠道均可配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    /// 是否启用 AI 运维 Agent（默认关闭，避免未配置模型时误启）。
    #[serde(default)]
    pub enabled: bool,
    /// Agent 推理模型名（走 AIGX 渠道，缺省用全局映射解析）。
    #[serde(default)]
    pub model: String,
    /// 锁定渠道 ID：非空时只走该渠道（排障用）；空 = 走全局调度。
    #[serde(default)]
    pub channel: String,
    /// 单次任务最大推理轮数（防死循环）。
    #[serde(default = "default_max_turns")]
    pub max_turns: usize,
    /// 高危工具审批超时（秒），超时未响应视为拒绝。
    #[serde(default = "default_approval_timeout")]
    pub approval_timeout_secs: u64,
    /// 对话历史最大保留条数（上下文压缩：超出时只保留最近 N 条）。
    #[serde(default = "default_max_history")]
    pub max_history: usize,
    /// 思考强度（off/low/medium/high，空或 off = 不向模型发送思考参数）。
    /// 前端每次请求可覆盖，便于按任务复杂度选择"更快"或"更准"。
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    /// 自定义系统提示词（空 = 使用内置默认提示词）。
    #[serde(default)]
    pub system_prompt: String,
    /// 禁用的工具名列表（不下发给模型，也不会被执行）。
    #[serde(default)]
    pub disabled_tools: Vec<String>,
}

fn default_max_turns() -> usize {
    12
}

fn default_approval_timeout() -> u64 {
    300
}

fn default_max_history() -> usize {
    20
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            model: String::new(),
            channel: String::new(),
            max_turns: default_max_turns(),
            approval_timeout_secs: default_approval_timeout(),
            max_history: default_max_history(),
            reasoning_effort: None,
            system_prompt: String::new(),
            disabled_tools: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageConfig {
    #[serde(default = "default_daily_limit")]
    pub daily_limit: u64,
    #[serde(default = "default_monthly_limit")]
    pub monthly_limit: u64,
    /// 新用户注册赠送配额（与 monthly_limit 分离，避免"月度限额"被当作
    /// 充值配额赠送——原实现二者混用导致计费口径错乱）。
    #[serde(default = "default_register_quota")]
    pub register_quota: i64,
    /// 是否开放自助注册（对齐 new-api RegisterEnabled，false 时注册端点 403）
    #[serde(default = "default_register_enabled")]
    pub register_enabled: bool,
    /// 每个邀请人的奖励配额（对齐 new-api QuotaForInviter，0=关闭邀请奖励）
    #[serde(default)]
    pub quota_for_inviter: i64,
    /// 每个受邀新用户的奖励配额（对齐 new-api QuotaForInvitee，0=无奖励）
    #[serde(default)]
    pub quota_for_invitee: i64,
    /// 是否启用注册邮箱白名单（对齐 v2board email_whitelist_enable）
    #[serde(default)]
    pub email_whitelist_enable: bool,
    /// 允许注册的邮箱后缀（对齐 v2board email_whitelist_suffix，如 ["gmail.com"]）
    #[serde(default)]
    pub email_whitelist_suffix: Vec<String>,
    /// 签到设置（对齐 new-api CheckinSetting：开关 + 奖励区间）
    #[serde(default)]
    pub checkin: crate::user::checkin::CheckinSetting,
    #[serde(default)]
    pub threshold: f64,
    #[serde(default = "default_api_timeout")]
    pub api_timeout_secs: u64,
    /// 建立连接超时（秒）——上游握手阶段，默认 10。
    #[serde(default = "default_connect_timeout")]
    pub connect_timeout_secs: u64,
    /// 响应头等待上限（秒）——同时用作 SSE 首事件守卫，默认 60。
    #[serde(default = "default_headers_timeout")]
    pub headers_timeout_secs: u64,
    /// 流空闲超时（秒）——两次读取之间的最长静默，默认 30。
    #[serde(default = "default_stream_idle_secs")]
    pub stream_idle_secs: u64,
    /// 非流式响应体超时（秒）——非流式请求读完整 body 的上限，默认 300。
    /// 与 `api_timeout_secs`（请求总超时）独立：长上下文非流式请求可单独放宽。
    #[serde(default = "default_body_timeout_secs")]
    pub body_timeout_secs: u64,
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    /// 同一账号内的重试次数（0~10，默认 1）——上游抖动时先原地重试，
    /// 避免立刻换号放大上游压力。换号次数由账号池的「账号路由」
    /// （`RotationPolicy.max_attempts`）单独控制，两者职责不重叠。
    #[serde(default = "default_retry_self_count")]
    pub retry_self_count: u32,
    /// 切换账号重试次数（0~10，默认 5）——单次请求最多尝试几个账号。
    /// 映射到 `RotationPolicy.max_attempts`（账号池路由），保存即生效。
    #[serde(default = "default_retry_switch_count")]
    pub retry_switch_count: u32,
    /// 账号换号之间的重试间隔（毫秒），默认 0（立即换号）。
    #[serde(default)]
    pub retry_interval_ms: u64,
    /// 命中这些上游状态码时不给指数退避冷却（只给基础冷却），直接换号。
    /// 默认 402（积分/余额不足）——账号已被上游判死，换号比冷却更有意义。
    #[serde(default = "default_retry_on_status")]
    pub retry_on_status: Vec<u16>,
    /// 请求体大小上限（MB），作用于数据面接口（OpenAI/Anthropic 兼容
    /// 端点与音频 multipart 上传）。运行期经管理员后台「使用限额」页
    /// 修改后立即生效（build_router 每次请求实时读配置）。
    #[serde(default = "default_max_request_body_mb")]
    pub max_request_body_mb: usize,
    /// 请求日志快照开关（log_body）：开启后把客户端请求体/上游响应体
    /// 截断快照进请求日志（每字段 4KB 上限），用于排障回放。
    /// 默认关闭——关闭时零开销（无序列化、无存储、无字符截断拷贝）。
    #[serde(default)]
    pub log_body: bool,
    /// 调试模式（上游原始报文）：开启后抓取 AIGX **实际发往上游**的
    /// URL / 脱敏头 / 请求体，落在进程内环形缓冲（不写库），供设置页查看。
    /// 与 `log_body` 互补——那边是下游（客户端）侧，这边是上游侧。
    /// 默认关闭——关闭时桥接层零开销（`try_clone`/`build` 都不执行）。
    #[serde(default)]
    pub debug_traffic: bool,
    /// 丢弃上游「换行保活帧」（对齐 Agent2API `stripNewlineKeepalive`）。
    ///
    /// 部分上游会在正文分片之间插 content 只含换行的帧给自己保活；客户端按
    /// markdown 渲染就成了一行一个词。开启后在下发前丢掉这类帧（只改下发字节，
    /// 不动上游请求）。默认关闭。
    #[serde(default)]
    pub strip_newline_keepalive: bool,
    /// models.dev 模型元数据补齐开关（对齐 Agent2API `modelsDevEnabled`，默认**开**）。
    ///
    /// 开启时：内置推断未声明的上下文长度 / 能力标签用 models.dev 目录只填
    /// 空位（管理员覆盖永远优先）；定时刷新 + 手动刷新照常。关闭时：补值
    /// 与刷新都停，已装载目录也不再参与展示。
    #[serde(default = "default_models_dev_enabled")]
    pub models_dev_enabled: bool,
    /// 按次计费每次固定扣减的额度数（全局统一，`billing_mode=count` 的
    /// 订阅套餐生效）。默认 2，与聊天页「1 次 = 2 额度」口径一致。
    /// 与 token 计费互斥：订阅计费模式为 count 时按此固定值扣，否则按 token 量。
    #[serde(default = "default_billing_flat_quota")]
    pub billing_flat_quota: i64,
    /// 是否允许上游地址指向内网/环回地址（SSRF 防护的逃生阀）。
    ///
    /// 默认 `false`：保存渠道/账号时拒绝 `127.0.0.1`、`10.x`、`169.254.169.254`
    /// 这类地址，避免网关被当成内网探测跳板。自托管 Ollama / 内网 vLLM 等
    /// 确实需要指向内网的场景，由管理员显式开启。
    #[serde(default)]
    pub allow_internal_upstream: bool,
    /// 排队退避最多等待次数（对齐 Agent2API `queueMaxWaits`，0~10，默认 2）。
    ///
    /// Qoder 类上游在模型繁忙时不报错而是排队：回「暂不可服务，建议 N 秒后
    /// 再来」（HTTP 403 + 业务码 10605，或 SSE 信封 statusCodeValue 403）。
    /// 桥接层在**首字节下发之前**按建议时长睡一觉再重发同一请求（每轮重建
    /// request_id 与签名——重放旧签名会被上游判成重复请求）。0 = 不等：
    /// 排队直接落 503 快速失败。
    #[serde(default = "default_queue_max_waits")]
    pub queue_max_waits: u8,
    /// 排队单次等待秒数（对齐 Agent2API `queueWaitSeconds`，0~120，默认 0）。
    ///
    /// 0 = 跟随上游建议（`retryAfterSeconds`，钳在 5~30 秒，上游没给时 15 秒）；
    /// `>0` = 写死这个间隔。排队等待吃的是「响应头等待超时」那份预算，
    /// 本设置不会突破该预算。
    #[serde(default)]
    pub queue_wait_seconds: u8,
    /// 截断自动续写总开关（对齐 Agent2API `autoContinue`，默认**关**）。
    ///
    /// 开启时：网关判定上游答复被截断（`length` 停止原因、围栏/括号没闭合、
    /// 句子吊尾等六条判据，见 `crate::continuation`）后，自动把半截原文当
    /// 上下文再发一轮「请继续」，直到自然停止或触到轮数上限。关闭时数据面
    /// 连包装都不做，零开销。
    #[serde(default)]
    pub auto_continue: bool,
    /// 续写最多额外轮数（对齐 Agent2API `autoContinueMaxRounds`，1~8，默认 3）。
    ///
    /// 只统计续写轮，不含首轮；复读检测（两轮文本几乎一模一样）会提前收手。
    #[serde(default = "default_auto_continue_max_rounds")]
    pub auto_continue_max_rounds: u8,
    /// 出站指纹脱敏开关（对齐 Agent2API `sanitizeBlacklistFingerprints`，默认**开**）。
    ///
    /// 开启时转发层在发送上游前剥离请求体里的上游审核黑名单指纹
    /// （表头键值整段删除、模板句最小改写，见 `crate::sanitize`）。与
    /// `/api/sanitize` 的「额外脱敏请求头名单」（表头侧）互补：那边管
    /// 请求头，这边管请求体。关掉等于把客户端 system 模板原样发给上游
    /// ——那正是模板句被误拦的原因，所以默认开启。
    #[serde(default = "default_sanitize_fingerprints")]
    pub sanitize_fingerprints: bool,
    /// Cline 伪装头覆盖表（对齐 Agent2API `clineHeaders`，只存用户改过的键）。
    ///
    /// 默认值取自 `account_session::registry::SESSION_PROVIDERS` 里 cline
    /// 渠道的请求头模板，不落盘；空串值 = 这个头不发；键缺失 = 回落默认。
    #[serde(default)]
    pub cline_headers: ClineHeadersConfig,
    /// ZCode 验证码令牌池的库存目标（对齐 Agent2API `zcodeCaptchaPoolTarget`，1~10，默认 3）。
    ///
    /// 只影响**前端铸造节奏**：库存低于它才补。「够下一条请求立刻有得用」即可，
    /// 铸多了纯属烧阿里云风控配额（令牌寿命只有两分钟，见 `zcode_captcha::TOKEN_TTL_MS`）。
    #[serde(default = "default_zcode_captcha_pool_target")]
    pub zcode_captcha_pool_target: u8,
    /// 令牌池轮询间隔毫秒（对齐 Agent2API `zcodeCaptchaPoolPollMs`，1000~30000，默认 2500）。
    #[serde(default = "default_zcode_captcha_pool_poll_ms")]
    pub zcode_captcha_pool_poll_ms: u32,
    /// 单轮铸造上限（对齐 Agent2API `zcodeCaptchaPoolMaxPerRound`，1~5，默认 1）。
    ///
    /// 1 = 单实例单次铸造，避开并发冲突与 DOM 重建竞态（SDK 同一个实例连铸两轮会互踩）。
    #[serde(default = "default_zcode_captcha_pool_max_per_round")]
    pub zcode_captcha_pool_max_per_round: u8,
}

/// 令牌池库存目标默认值（够下一条请求立刻有得用）。
fn default_zcode_captcha_pool_target() -> u8 {
    3
}

/// 令牌池轮询间隔默认值（毫秒）。
fn default_zcode_captcha_pool_poll_ms() -> u32 {
    2_500
}

/// 单轮铸造上限默认值。
fn default_zcode_captcha_pool_max_per_round() -> u8 {
    1
}

/// Cline 伪装头覆盖表（键 → 值；只存用户改过/新增的键）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClineHeadersConfig {
    /// 用户改过的头（原样持久化；空串 = 不发这个头）。
    #[serde(default)]
    pub overrides: std::collections::HashMap<String, String>,
}

/// 续写最多额外轮数的默认值（对齐上游 autoContinueMaxRounds）。
fn default_auto_continue_max_rounds() -> u8 {
    3
}

/// 指纹脱敏默认开启（关闭意味着客户端模板原样出站，可能被上游误拦）。
fn default_sanitize_fingerprints() -> bool {
    true
}

/// 排队退避次数上限的默认值（对齐上游 queueMaxWaits）。
fn default_queue_max_waits() -> u8 {
    2
}

fn default_billing_flat_quota() -> i64 {
    2
}

fn default_api_timeout() -> u64 {
    300
}

fn default_connect_timeout() -> u64 {
    10
}

fn default_headers_timeout() -> u64 {
    60
}

fn default_stream_idle_secs() -> u64 {
    30
}

fn default_body_timeout_secs() -> u64 {
    300
}

fn default_retry_self_count() -> u32 {
    1
}

fn default_retry_switch_count() -> u32 {
    5
}

fn default_retry_on_status() -> Vec<u16> {
    vec![402]
}

fn default_max_retries() -> u32 {
    2
}

/// 请求体大小上限默认 25MB（对齐 audio multipart 原有阈值）。
fn default_max_request_body_mb() -> usize {
    25
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub admin: AdminConfig,
    #[serde(default)]
    pub usage: UsageConfig,
    /// 易支付配置
    #[serde(default)]
    pub epay: EpayConfig,
    /// Stripe 支付配置
    #[serde(default)]
    pub stripe: StripeConfig,
    /// GitHub OAuth configuration
    #[serde(default)]
    pub github_oauth: GithubOauthConfig,
    /// Google OAuth configuration
    #[serde(default)]
    pub google_oauth: GoogleOauthConfig,
    /// LinuxDO OAuth configuration
    #[serde(default)]
    pub linuxdo_oauth: LinuxDoOauthConfig,
    /// 站点对外访问地址，用于构造回调 URL
    #[serde(default)]
    pub server_address: String,
    /// 通知系统配置（Telegram + SMTP）
    #[serde(default)]
    pub notify: NotifyConfig,
    /// cf-ai-gw Worker 部署地址（AI Binding 桥接）。
    ///
    /// AIGX 通过该地址调用 cf-ai-gw Worker，Worker 内部使用 **AI Binding**
    /// （`env.AI.run()`）调用 Cloudflare Workers AI，而非 REST API。
    /// 默认 `http://127.0.0.1:8787`（wrangler dev 本地调试）。
    #[serde(default = "default_cf_binding_url")]
    pub cf_binding_url: String,
    /// 数据库配置（多数据库后端支持）
    ///
    /// 留空则使用默认 FileStore（rusqlite），填 URL 则启用 SeaORM。
    #[serde(default)]
    pub database: DatabaseConfig,
    /// CORS 允许的来源列表（对外暴露时务必显式配置）。
    ///
    /// 生产环境应显式配置允许的前端来源（如 `["https://admin.example.com"]`）。
    /// 留空时优先使用 `server_address` 作为唯一允许来源；两者皆空则不发 CORS 头
    /// （跨源请求被浏览器拦截，同源不受影响），见 main.rs build_cors_layer。
    #[serde(default)]
    pub cors_origins: Vec<String>,
    /// 是否信任反向代理的 `X-Forwarded-For` / `X-Real-IP` 头。
    ///
    /// - `false`（默认）：忽略这两个头，IP 记为 "unknown"（直连部署无代理，
    ///   客户端可伪造 XFF 头绕过 IP 白名单/黑名单或伪造日志归属）。
    /// - `true`：仅在确认部署于可信反代（Nginx/CF 等）之后开启，此时
    ///   由代理负责剥离外部传入的 XFF 并追加真实 IP。
    #[serde(default)]
    pub trust_proxy_headers: bool,
    /// AI 运维 Agent 配置（`[agent]` 段，自举式运维工作台）。
    #[serde(default)]
    pub agent: AgentConfig,
    /// 渠道调度配置（`[channel]` 段）。
    #[serde(default)]
    pub channel: ChannelConfig,
    /// 服务器自动更新配置（`[update]` 段）。
    #[serde(default)]
    pub update: UpdateConfig,
    /// 间隔型定时任务配置（凭证维护 / 模型刷新 / 版本检查 + 三页自动刷新）
    #[serde(default)]
    pub scheduled_tasks: crate::scheduled_tasks::types::ScheduledSettings,
    /// 安全策略配置（`[security]` 段）：登录失败锁定与登录限流的可配参数。
    #[serde(default)]
    pub security: SecurityPolicyConfig,
}

/// 安全策略（`[security]` 段）。
///
/// 现状的这三个门槛此前硬编码在 `api::admin::auth`（阈值 5 / 限流 10）与
/// `main.rs`（锁定 TTL 300s）。抽成配置后默认值与原硬编码一致，行为不变。
///
/// ── 生效时机 ────────────────────────────────────────────────
/// `fail_threshold` / `rate_limit_per_minute` / `lockout_enabled` 走进程级
/// 原子快照，**保存即生效**；`lock_minutes` 是失败计数缓存的 TTL（建缓存时
/// 固定），**改后需重启服务生效** —— 与网关监听地址/端口同口径。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityPolicyConfig {
    /// 是否启用「连续失败锁定」（默认开）。关闭后仅保留每分钟限流。
    #[serde(default = "default_lockout_enabled")]
    pub lockout_enabled: bool,
    /// 连续登录失败达到该次数即锁定来源 IP（默认 5，范围 1-100）。
    #[serde(default = "default_fail_threshold")]
    pub fail_threshold: u32,
    /// 锁定时长（分钟，默认 5，范围 1-1440）。缓存 TTL 在启动时确定，改后需重启。
    #[serde(default = "default_lock_minutes")]
    pub lock_minutes: u64,
    /// 同一来源 IP 每分钟允许的登录尝试次数（默认 10，范围 1-1000）。
    #[serde(default = "default_rate_limit_per_minute")]
    pub rate_limit_per_minute: u32,
    /// 是否启用机器人校验（ALTCHA proof-of-work，默认开）。
    ///
    /// 开启后，登录 / 注册这类**公开认证边界**必须先领题、由客户端算力解出
    /// answer 再随请求提交；脚本每试一个密码都要先花一次算力，暴破与抢注从
    /// 零成本变成逐请求成本。对真人无感——题目在页面加载时就算完了。
    /// 失败锁定拦的是「同源高频失败」（换 IP 就绕开），本项补的是另一维。
    #[serde(default = "default_captcha_enabled")]
    pub captcha_enabled: bool,
}

fn default_captcha_enabled() -> bool {
    true
}

fn default_models_dev_enabled() -> bool {
    true
}

fn default_lockout_enabled() -> bool {
    true
}

fn default_fail_threshold() -> u32 {
    5
}

fn default_lock_minutes() -> u64 {
    5
}

fn default_rate_limit_per_minute() -> u32 {
    10
}

impl Default for SecurityPolicyConfig {
    fn default() -> Self {
        Self {
            lockout_enabled: default_lockout_enabled(),
            fail_threshold: default_fail_threshold(),
            lock_minutes: default_lock_minutes(),
            rate_limit_per_minute: default_rate_limit_per_minute(),
            captcha_enabled: default_captcha_enabled(),
        }
    }
}

impl SecurityPolicyConfig {
    /// 落盘与运行期都用归一化后的值（越界输入夹到合法区间）。
    pub fn normalized(&self) -> Self {
        Self {
            lockout_enabled: self.lockout_enabled,
            fail_threshold: self.fail_threshold.clamp(1, 100),
            lock_minutes: self.lock_minutes.clamp(1, 1440),
            rate_limit_per_minute: self.rate_limit_per_minute.clamp(1, 1000),
            captcha_enabled: self.captcha_enabled,
        }
    }
}

// ── Default implementations ──────────────────────────────────────────

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
            data_dir: default_data_dir(),
            log_dir: None,
            request_stats_dir: None,
            debug_dir: None,
        }
    }
}

impl Default for UsageConfig {
    fn default() -> Self {
        Self {
            daily_limit: default_daily_limit(),
            monthly_limit: default_monthly_limit(),
            register_quota: default_register_quota(),
            register_enabled: default_register_enabled(),
            quota_for_inviter: 0,
            quota_for_invitee: 0,
            email_whitelist_enable: false,
            email_whitelist_suffix: Vec::new(),
            checkin: Default::default(),
            threshold: 0.0,
            api_timeout_secs: default_api_timeout(),
            connect_timeout_secs: default_connect_timeout(),
            headers_timeout_secs: default_headers_timeout(),
            stream_idle_secs: default_stream_idle_secs(),
            body_timeout_secs: default_body_timeout_secs(),
            max_retries: default_max_retries(),
            retry_self_count: default_retry_self_count(),
            retry_switch_count: default_retry_switch_count(),
            retry_interval_ms: 0,
            retry_on_status: default_retry_on_status(),
            max_request_body_mb: default_max_request_body_mb(),
            log_body: false,
            debug_traffic: false,
            strip_newline_keepalive: false,
            models_dev_enabled: default_models_dev_enabled(),
            allow_internal_upstream: false,
            queue_max_waits: default_queue_max_waits(),
            queue_wait_seconds: 0,
            auto_continue: false,
            auto_continue_max_rounds: default_auto_continue_max_rounds(),
            sanitize_fingerprints: default_sanitize_fingerprints(),
            cline_headers: Default::default(),
            zcode_captcha_pool_target: default_zcode_captcha_pool_target(),
            zcode_captcha_pool_poll_ms: default_zcode_captcha_pool_poll_ms(),
            zcode_captcha_pool_max_per_round: default_zcode_captcha_pool_max_per_round(),
            billing_flat_quota: default_billing_flat_quota(),
        }
    }
}

// ── ConfigManager ────────────────────────────────────────────────────

/// 是否信任代理头的进程级快照（由 main 启动时写入）。
///
/// `extract_client_ip` 是同步函数（18 个调用点都无法 await），而配置
/// 在运行期可经管理端修改——这里取启动时快照即可：改这个开关需要
/// 同时调整反代部署方式，重启进程应用是合理语义。
static TRUST_PROXY_HEADERS: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// 读取 trust_proxy_headers 快照（供 `extract_client_ip` 同步访问）
pub fn trust_proxy_headers() -> bool {
    TRUST_PROXY_HEADERS.load(std::sync::atomic::Ordering::Relaxed)
}

/// 启动时由 main 写入快照（见 main.rs）
pub fn set_trust_proxy_headers(v: bool) {
    TRUST_PROXY_HEADERS.store(v, std::sync::atomic::Ordering::Relaxed);
}

/// 请求日志快照开关的进程级快照（由 main 启动时写入）。
///
/// 数据面计费/日志路径是同步代码（StreamBillingState::finalize 无法
/// await config_manager.get()），走原子快照零开销：关闭时分支预测
/// 直通，无锁无拷贝。管理端改 log_body 后重启进程生效。
static LOG_BODY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 读取 log_body 快照（供数据面日志路径同步访问）
pub fn log_body_enabled() -> bool {
    LOG_BODY.load(std::sync::atomic::Ordering::Relaxed)
}

/// 由 main 与 settings 更新端点写入快照
pub fn set_log_body(v: bool) {
    LOG_BODY.store(v, std::sync::atomic::Ordering::Relaxed);
}

/// 调试模式（上游原始报文）开关的进程级快照（由 main 启动时写入）。
///
/// 与 `log_body` 同理：桥接层每个请求都要判一次开关，走原子快照零开销
/// （关闭时分支预测直通，不做 `try_clone`/`build`）。管理端保存后立即生效，
/// 无需重启（与 `log_body` 的区别——这里读出后直接决定抓不抓，没有缓存）。
static DEBUG_TRAFFIC: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 读取调试模式开关（供桥接层同步访问）
pub fn debug_traffic_enabled() -> bool {
    DEBUG_TRAFFIC.load(std::sync::atomic::Ordering::Relaxed)
}

/// 由 main 与 settings 更新端点写入快照
pub fn set_debug_traffic(v: bool) {
    DEBUG_TRAFFIC.store(v, std::sync::atomic::Ordering::Relaxed);
}

/// 换行保活帧过滤开关（桥接层每条 SSE 帧读取；关闭时零开销）。
static STRIP_NEWLINE_KEEPALIVE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// 读取「丢弃换行保活帧」开关（桥接层同步访问）。
pub fn strip_newline_keepalive() -> bool {
    STRIP_NEWLINE_KEEPALIVE.load(std::sync::atomic::Ordering::Relaxed)
}

/// 由 main 与 settings 更新端点写入快照。
pub fn set_strip_newline_keepalive(v: bool) {
    STRIP_NEWLINE_KEEPALIVE.store(v, std::sync::atomic::Ordering::Relaxed);
}

/// models.dev 元数据补齐开关（默认开；模型元信息查询与刷新协程同步读取）。
static MODELS_DEV_ENABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// 读取 models.dev 补齐开关。
pub fn models_dev_enabled() -> bool {
    MODELS_DEV_ENABLED.load(std::sync::atomic::Ordering::Relaxed)
}

/// 由 main 启动与模型元数据更新端点写入快照。
pub fn set_models_dev_enabled(v: bool) {
    MODELS_DEV_ENABLED.store(v, std::sync::atomic::Ordering::Relaxed);
}

/// 排队等待设置快照（桥接层每个请求同步读取，管理端保存即生效）。
///
/// `max_waits = 0` 表示快速失败（排队立即落 503）；`wait_seconds = 0` 表示
/// 单次时长跟随上游建议。与上游 `config::queue_settings` 同语义。
static QUEUE_MAX_WAITS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(2);
static QUEUE_WAIT_SECONDS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// 读取排队设置：`(最多等待次数, 单次秒数; 0=跟随上游建议)`。
pub fn queue_settings() -> (u64, u64) {
    (
        QUEUE_MAX_WAITS.load(std::sync::atomic::Ordering::Relaxed),
        QUEUE_WAIT_SECONDS.load(std::sync::atomic::Ordering::Relaxed),
    )
}

/// 由 main 启动与限额更新端点写入快照（越界值钳制后再存，快照即合法域）。
pub fn set_queue_settings(max_waits: u64, wait_seconds: u64) {
    QUEUE_MAX_WAITS.store(max_waits.min(10), std::sync::atomic::Ordering::Relaxed);
    QUEUE_WAIT_SECONDS.store(wait_seconds.min(120), std::sync::atomic::Ordering::Relaxed);
}

/// 截断自动续写开关的进程级快照（数据面每个请求同步读取，管理端保存即生效）。
static AUTO_CONTINUE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// 续写最多额外轮数的进程级快照（1~8）。
static AUTO_CONTINUE_MAX_ROUNDS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(3);

/// 续写开关是否开启（关闭时数据面不做任何包装，零开销）。
pub fn auto_continue_enabled() -> bool {
    AUTO_CONTINUE.load(std::sync::atomic::Ordering::Relaxed)
}

/// 续写最多额外轮数（已钳 1~8）。
pub fn auto_continue_max_rounds() -> u64 {
    AUTO_CONTINUE_MAX_ROUNDS.load(std::sync::atomic::Ordering::Relaxed)
}

/// 由 main 启动与限额更新端点写入快照（越界值钳制后再存，快照即合法域）。
pub fn set_auto_continue_settings(enabled: bool, max_rounds: u64) {
    AUTO_CONTINUE.store(enabled, std::sync::atomic::Ordering::Relaxed);
    AUTO_CONTINUE_MAX_ROUNDS.store(max_rounds.clamp(1, 8), std::sync::atomic::Ordering::Relaxed);
}

/// 按次计费每次固定扣减额度的进程级快照（数据面同步路径读取）。
///
/// 数据面计费路径（charge/reserve/settle）是同步函数，无法 await
/// config_manager.get()；与 log_body 同理，走原子快照零开销。默认 2。
static BILLING_FLAT_QUOTA: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(2);

/// 读取 billing_flat_quota 快照（供数据面按次计费同步访问）
pub fn billing_flat_quota() -> i64 {
    BILLING_FLAT_QUOTA.load(std::sync::atomic::Ordering::Relaxed)
}

/// 由 main 与 settings 更新端点写入快照
pub fn set_billing_flat_quota(v: i64) {
    BILLING_FLAT_QUOTA.store(v, std::sync::atomic::Ordering::Relaxed);
}

// ── 安全策略的进程级快照 ──────────────────────────────────────
//
// 登录 handler 里读这三项是同步比较（限流上限 / 锁定阈值 / 锁定开关），
// 与 log_body 同理走原子快照：管理端保存后（`handle_update_security_policy`）
// 与启动时（main）各写一次，数据面无锁零开销。
// 锁定时长（`lock_minutes`）不在此列 —— 它是失败计数缓存的 TTL，建缓存时固定。

/// 是否启用连续失败锁定（默认开）。
static SECURITY_LOCKOUT_ENABLED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(true);
/// 连续失败锁定阈值（默认 5）。
static SECURITY_FAIL_THRESHOLD: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(5);
/// 每分钟登录尝试上限（默认 10）。
static SECURITY_RATE_LIMIT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(10);
/// 是否启用机器人校验（ALTCHA proof-of-work，默认开）。
static SECURITY_CAPTCHA_ENABLED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(true);

/// 读取「是否启用失败锁定」快照。
pub fn security_lockout_enabled() -> bool {
    SECURITY_LOCKOUT_ENABLED.load(std::sync::atomic::Ordering::Relaxed)
}

/// 读取「连续失败锁定阈值」快照（至少 1，避免 0 导致首次失败即锁）。
pub fn security_fail_threshold() -> u32 {
    SECURITY_FAIL_THRESHOLD
        .load(std::sync::atomic::Ordering::Relaxed)
        .max(1)
}

/// 读取「每分钟登录尝试上限」快照（至少 1）。
pub fn security_rate_limit_per_minute() -> u32 {
    SECURITY_RATE_LIMIT
        .load(std::sync::atomic::Ordering::Relaxed)
        .max(1)
}

/// 读取「是否启用机器人校验」快照（登录 / 注册 handler 同步读取）。
pub fn security_captcha_enabled() -> bool {
    SECURITY_CAPTCHA_ENABLED.load(std::sync::atomic::Ordering::Relaxed)
}

/// 由 main 启动与 settings 更新端点写入三个快照。
pub fn set_security_policy(lockout_enabled: bool, fail_threshold: u32, rate_limit_per_minute: u32) {
    SECURITY_LOCKOUT_ENABLED.store(lockout_enabled, std::sync::atomic::Ordering::Relaxed);
    SECURITY_FAIL_THRESHOLD.store(fail_threshold.max(1), std::sync::atomic::Ordering::Relaxed);
    SECURITY_RATE_LIMIT.store(
        rate_limit_per_minute.max(1),
        std::sync::atomic::Ordering::Relaxed,
    );
}

/// 由 main 启动与 settings 更新端点写入机器人校验开关。
pub fn set_security_captcha(v: bool) {
    SECURITY_CAPTCHA_ENABLED.store(v, std::sync::atomic::Ordering::Relaxed);
}

/// 渠道级最大重试次数的进程级快照（数据面选路同步读取，默认 2）。
///
/// `resolve_bridges` 是同步函数且在每个数据面端点共用，无法 await 配置；
/// 用原子快照把「最大重试次数」落到候选渠道截断（重试次数 = 额外尝试数，
/// 因此最多尝试 `max_retries + 1` 个渠道）。
static MAX_RETRIES: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(2);

/// 读取渠道级最大重试次数（供数据面选路同步访问）。
pub fn max_retries() -> u32 {
    MAX_RETRIES.load(std::sync::atomic::Ordering::Relaxed)
}

/// 由 main 与 settings 更新端点写入快照（上限 10）。
pub fn set_max_retries(v: u32) {
    MAX_RETRIES.store(v.min(10), std::sync::atomic::Ordering::Relaxed);
}

/// 管理员额外指定的「脱敏请求头」名单（大小写不敏感）。
///
/// 内置清单（`channel::is_sensitive_header` 与 `debug_traffic`）覆盖通用凭据头，
/// 但各家上游的自定义头名会变——管理员可在设置页「网关 → 指纹脱敏」补充。
/// 支持两种形态：完整头名（`x-foo-token`）与尾部通配（`x-foo-*`）。
static EXTRA_SENSITIVE_HEADERS: parking_lot::RwLock<Vec<String>> =
    parking_lot::RwLock::new(Vec::new());

/// 归一化一条名单项：去空白、转小写；空串丢弃。
fn normalize_sensitive_pattern(raw: &str) -> Option<String> {
    let trimmed = raw.trim().to_ascii_lowercase();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

/// 写入额外脱敏头名单（去重、去空、归一化、上限 200 条）。
pub fn set_extra_sensitive_headers(list: Vec<String>) {
    let mut cleaned: Vec<String> = Vec::new();
    for raw in list {
        if let Some(pat) = normalize_sensitive_pattern(&raw) {
            if !cleaned.contains(&pat) {
                cleaned.push(pat);
            }
        }
        if cleaned.len() >= 200 {
            break;
        }
    }
    {
        let mut guard = EXTRA_SENSITIVE_HEADERS.write();
        *guard = cleaned;
    }
}

/// 读取额外脱敏头名单（管理页回显用）。
pub fn extra_sensitive_headers() -> Vec<String> {
    EXTRA_SENSITIVE_HEADERS.read().clone()
}

/// 命中额外名单？`name` 需已是小写。支持尾部 `*` 通配前缀匹配。
pub fn is_extra_sensitive_header(name: &str) -> bool {
    let lower = name.trim().to_ascii_lowercase();
    EXTRA_SENSITIVE_HEADERS
        .read()
        .iter()
        .any(|pat| match pat.strip_suffix('*') {
            Some(prefix) => lower.starts_with(prefix),
            None => lower == *pat,
        })
}

/// 非流式响应体超时（秒）的进程级快照。
///
/// 共享 client 的总超时是 `api_timeout_secs`；该快照用于给**非流式**请求
/// 单独覆盖请求级超时（`RequestBuilder::timeout`），保存设置后立即生效。
static BODY_TIMEOUT_SECS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(300);

/// 读取非流式响应体超时（秒）。
pub fn body_timeout_secs() -> u64 {
    BODY_TIMEOUT_SECS.load(std::sync::atomic::Ordering::Relaxed)
}

/// 由 main 与 settings 更新端点写入快照（1–3600）。
pub fn set_body_timeout_secs(v: u64) {
    BODY_TIMEOUT_SECS.store(v.clamp(1, 3_600), std::sync::atomic::Ordering::Relaxed);
}

/// 出站指纹脱敏开关的进程级快照（转发层每请求同步读取，默认**开**）。
///
/// 数据面转发路径无法 await 配置，与 `log_body` 同理走原子快照零开销。
static SANITIZE_FINGERPRINTS: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(true);

/// 读取指纹脱敏开关（供转发层同步访问）。
pub fn sanitize_fingerprints_enabled() -> bool {
    SANITIZE_FINGERPRINTS.load(std::sync::atomic::Ordering::Relaxed)
}

/// 由 main 与 settings 更新端点写入快照。
pub fn set_sanitize_fingerprints(v: bool) {
    SANITIZE_FINGERPRINTS.store(v, std::sync::atomic::Ordering::Relaxed);
}

/// Cline 伪装头覆盖表的进程级快照（键值对列表；空表 = 全部回落代码内默认）。
///
/// `HashMap::new` 不是 const fn，与 `RETRY_ON_STATUS` 同型用
/// `Vec<(String, String)>` 承载（写少读多：读路径拷贝列表，写路径仅管理端
/// 保存时触发）。键值对顺序不保证——消费方按需重建 HashMap。
static CLINE_HEADER_OVERRIDES: parking_lot::RwLock<Vec<(String, String)>> =
    parking_lot::RwLock::new(Vec::new());

/// 读取 Cline 伪装头覆盖表快照（转发层同步访问）。
pub fn cline_header_overrides() -> Vec<(String, String)> {
    CLINE_HEADER_OVERRIDES.read().clone()
}

/// 由 main 与 cline-headers 更新端点写入快照。
pub fn set_cline_header_overrides(entries: Vec<(String, String)>) {
    {
        let mut list = CLINE_HEADER_OVERRIDES.write();
        *list = entries;
    }
}

/// Cline 伪装头里网关保留、**不允许覆盖**的固定头（小写比较）。
///
/// `Authorization` 由桥接层按账号凭证逐请求生成，覆盖它等于把请求发成别人
/// 身份；`Content-Type` 是协议必需。管理端写入口与转发侧合并都按这张表拦截
/// （前者的作用是把非法配置挡在落盘之前，后者是配置文件被手改后的兜底）。
pub const CLINE_FIXED_HEADER_NAMES: [&str; 2] = ["authorization", "content-type"];

/// 把 Cline 伪装头覆盖表应用到一组请求头上。
///
/// - 仅对 `cline*` 渠道生效（`cline` / `cline-free` / `cline-pass` 共用同一套头）；
/// - 大小写不敏感：覆盖表里的键先把已存在的同名头摘掉，再按覆盖表里的原样写法插入；
/// - 值为**空串 = 主动不发这个头**（对齐 Agent2API 的 `merge_headers`）；
/// - 固定头（Authorization / Content-Type）不做覆盖，防止配置文件被手改后顶掉凭证。
pub fn apply_cline_header_overrides(
    provider: &str,
    headers: Vec<(String, String)>,
) -> Vec<(String, String)> {
    if !provider.starts_with("cline") {
        return headers;
    }
    merge_cline_header_overrides(headers, &cline_header_overrides())
}

/// 覆盖表合并的纯函数本体（不读快照，便于单测与复用）。
pub fn merge_cline_header_overrides(
    headers: Vec<(String, String)>,
    overrides: &[(String, String)],
) -> Vec<(String, String)> {
    let mut merged = headers;
    for (name, value) in overrides {
        let lower = name.to_ascii_lowercase();
        if CLINE_FIXED_HEADER_NAMES.contains(&lower.as_str()) {
            continue;
        }
        merged.retain(|(existing, _)| existing.to_ascii_lowercase() != lower);
        if !value.is_empty() {
            merged.push((name.clone(), value.clone()));
        }
    }
    merged
}

// ── ZCode 验证码令牌池的节奏设置 ───────────────────────────────
//
// 与其余网关设置同构：进程级快照，写少读多。
// 读方是「每轮都问一次「要不要铸」的前端铸造器」（`GET /api/zcode/captcha`
// 把这三个值随概况一起下发），所以它们必须是同步、零 IO 的 —— 每 2.5 秒
// 就走一次配置文件的文件 I/O 毫不合理。

/// 库存目标的合法范围（与前端铸造器的夹取区间同源）。
pub const ZCODE_POOL_TARGET_MIN: u8 = 1;
pub const ZCODE_POOL_TARGET_MAX: u8 = 10;
/// 轮询间隔的合法范围（毫秒）。
pub const ZCODE_POOL_POLL_MIN_MS: u32 = 1_000;
pub const ZCODE_POOL_POLL_MAX_MS: u32 = 30_000;
/// 单轮铸造上限的合法范围。
pub const ZCODE_POOL_PER_ROUND_MIN: u8 = 1;
pub const ZCODE_POOL_PER_ROUND_MAX: u8 = 5;

static ZCODE_POOL_TARGET: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(3);
static ZCODE_POOL_POLL_MS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(2_500);
static ZCODE_POOL_MAX_PER_ROUND: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(1);

/// 读取令牌池节奏设置 `(target, poll_ms, max_per_round)`。
pub fn zcode_captcha_pool_settings() -> (u8, u32, u8) {
    use std::sync::atomic::Ordering::Relaxed;
    let target = ZCODE_POOL_TARGET.load(Relaxed);
    let poll = ZCODE_POOL_POLL_MS.load(Relaxed);
    let per_round = ZCODE_POOL_MAX_PER_ROUND.load(Relaxed);
    (target as u8, poll, per_round as u8)
}

/// 由 main 与 `PUT /api/zcode/captcha-pool` 写入快照（越界值夹进合法范围）。
pub fn set_zcode_captcha_pool_settings(target: u8, poll_ms: u32, max_per_round: u8) {
    use std::sync::atomic::Ordering::Relaxed;
    ZCODE_POOL_TARGET.store(
        target.clamp(ZCODE_POOL_TARGET_MIN, ZCODE_POOL_TARGET_MAX) as u32,
        Relaxed,
    );
    ZCODE_POOL_POLL_MS.store(
        poll_ms.clamp(ZCODE_POOL_POLL_MIN_MS, ZCODE_POOL_POLL_MAX_MS),
        Relaxed,
    );
    ZCODE_POOL_MAX_PER_ROUND.store(
        max_per_round.clamp(ZCODE_POOL_PER_ROUND_MIN, ZCODE_POOL_PER_ROUND_MAX) as u32,
        Relaxed,
    );
}

/// 账号池「同账号重试」次数的进程级快照（对齐 Agent2API retryCount）。
/// 换号次数由账号池「账号路由」的 `RotationPolicy.max_attempts` 控制。
static RETRY_SELF_COUNT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);

/// 读取同一账号内的重试次数。
pub fn retry_self_count() -> u32 {
    RETRY_SELF_COUNT.load(std::sync::atomic::Ordering::Relaxed)
}

/// 写入同一账号内的重试次数（上限 10）。
pub fn set_retry_self_count(v: u32) {
    RETRY_SELF_COUNT.store(v.min(10), std::sync::atomic::Ordering::Relaxed);
}

/// 切换账号重试次数的进程级快照（映射到 `RotationPolicy.max_attempts`）。
static RETRY_SWITCH_COUNT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(5);

/// 读取切换账号重试次数（0~10）。
pub fn retry_switch_count() -> u32 {
    RETRY_SWITCH_COUNT.load(std::sync::atomic::Ordering::Relaxed)
}

/// 写入切换账号重试次数（上限 10）。
pub fn set_retry_switch_count(v: u32) {
    RETRY_SWITCH_COUNT.store(v.min(10), std::sync::atomic::Ordering::Relaxed);
}

/// 「直接换号/换渠道状态码」的进程级快照（数据面同步判定）。
///
/// 该名单同时影响两处：
/// - 账号池：命中即不做指数退避冷却（只给基础冷却），立刻换号；
/// - 渠道级 failover：命中即视为可重试，切到下一个候选渠道。
static RETRY_ON_STATUS: parking_lot::RwLock<Vec<u16>> = parking_lot::RwLock::new(Vec::new());

/// 判断状态码是否命中「直接换号/换渠道」名单。
pub fn is_retry_status(code: u16) -> bool {
    RETRY_ON_STATUS.read().contains(&code)
}

/// 由 main 与 settings 更新端点写入快照（去重、100–599、上限 50）。
pub fn set_retry_on_status_codes(codes: Vec<u16>) {
    let mut cleaned: Vec<u16> = Vec::new();
    for code in codes {
        if !(100..=599).contains(&code) {
            continue;
        }
        if !cleaned.contains(&code) {
            cleaned.push(code);
        }
        if cleaned.len() >= 50 {
            break;
        }
    }
    {
        let mut list = RETRY_ON_STATUS.write();
        *list = cleaned;
    }
}

pub struct ConfigManager {
    config: RwLock<AppConfig>,
    path: PathBuf,
}

impl ConfigManager {
    /// 创建 ConfigManager。如果未指定路径，默认使用 ~/.aigx/config.toml。
    pub async fn new(path: Option<PathBuf>) -> Self {
        let path = path.unwrap_or_else(default_config_path);
        // 确保父目录存在
        if let Some(parent) = path.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }

        let config = RwLock::new(AppConfig::default());
        let manager = Self { config, path };

        // 如果配置文件已存在则加载，否则创建默认配置
        if manager.path.exists() {
            let _ = manager.load().await;
        } else {
            let _ = manager.save().await;
        }

        manager
    }

    /// 从磁盘加载配置
    pub async fn load(&self) -> AppConfig {
        let content = tokio::fs::read_to_string(&self.path)
            .await
            .unwrap_or_default();
        let config: AppConfig = toml::from_str(&content).unwrap_or_default();
        let config = apply_env_overrides(config);
        let cfg = config.clone();
        *self.config.write().await = config;
        cfg
    }

    /// 保存配置到磁盘（tmp+rename 原子写，避免断电/崩溃留下半截 TOML）
    pub async fn save(&self) -> anyhow::Result<()> {
        let content = {
            let config = self.config.read().await;
            toml::to_string_pretty(&*config)?
        };
        let tmp = self.path.with_extension("toml.tmp");
        tokio::fs::write(&tmp, content).await?;
        tokio::fs::rename(&tmp, &self.path).await?;
        Ok(())
    }

    /// 更新配置并持久化
    pub async fn update(&self, config: AppConfig) -> anyhow::Result<()> {
        *self.config.write().await = config;
        self.save().await?;
        Ok(())
    }

    /// 获取当前配置的只读副本
    pub async fn get(&self) -> AppConfig {
        self.config.read().await.clone()
    }

    /// 同步读取请求体大小上限（MB）。数据面 `map_request` 中间件与
    /// audio multipart 接口是同步路径，无法 await，故用 try_read 快照。
    /// 拿不到锁时回退默认 25，避免阻塞数据面。
    pub fn max_request_body_mb(&self) -> usize {
        let mb = self.config.try_read().map(|c| c.usage.max_request_body_mb);
        mb.unwrap_or(25)
    }

    /// 获取配置路径
    #[allow(dead_code)]
    pub fn path(&self) -> &PathBuf {
        &self.path
    }
}

/// 返回默认的配置文件路径：~/.aigx/config.toml
fn default_config_path() -> PathBuf {
    let home = dirs::home_dir().unwrap_or_default();
    home.join(".aigx").join("config.toml")
}

/// 展开 ~ 为 home 目录
pub fn expand_path(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) {
        let home = dirs::home_dir().unwrap_or_default();
        home.join(rest)
    } else if path == "~" {
        dirs::home_dir().unwrap_or_default()
    } else {
        PathBuf::from(path)
    }
}

/// 存储目录快照（logDir / requestStatsDir / debugDir）。
///
/// 未配置时回落到 `data_dir` 的子目录（logs / stats / debug）。
/// 对应基线 `storage_dirs()`。
pub struct StorageDirs {
    pub log_dir: PathBuf,
    pub request_stats_dir: PathBuf,
    pub debug_dir: PathBuf,
}

pub fn storage_dirs(config: &AppConfig) -> StorageDirs {
    let base = expand_path(&config.server.data_dir);
    let resolve = |raw: &Option<String>, sub: &str| -> PathBuf {
        raw.as_deref()
            .map(expand_path)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| base.join(sub))
    };
    StorageDirs {
        log_dir: resolve(&config.server.log_dir, "logs"),
        request_stats_dir: resolve(&config.server.request_stats_dir, "stats"),
        debug_dir: resolve(&config.server.debug_dir, "debug"),
    }
}

/// 隐藏数据库 URL 里的用户名与密码，专供日志输出。
///
/// `postgres://user:pass@host:5432/db` → `postgres://***@host:5432/db`；
/// 非法 URL 一律返回占位串，绝不把原文写进日志（原文可能含生产密码）。
pub fn redact_url_credentials(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    match url::Url::parse(trimmed) {
        Ok(mut parsed) => {
            let has_user = !parsed.username().is_empty();
            if has_user || parsed.password().is_some() {
                let _ = parsed.set_password(None);
                if has_user {
                    let _ = parsed.set_username("***");
                }
            }
            parsed.to_string()
        }
        Err(_) => "<invalid database url>".to_string(),
    }
}

/// 环境变量覆盖配置（容器部署用）。
///
/// 格式：`AIGX_<SECTION>__<FIELD>`，双下划线分隔 section 与 field，field 用小写。
/// 例如 `AIGX_SERVER__HOST=0.0.0.0` 覆盖 `config.server.host`，
/// `AIGX_SERVER__PORT=9527` 覆盖 `config.server.port`。
/// 仅支持标量覆盖（String / 整数），在 config.toml 加载后应用。
fn apply_env_overrides(mut config: AppConfig) -> AppConfig {
    if let Ok(v) = std::env::var("AIGX_SERVER__HOST") {
        config.server.host = v;
    }
    if let Ok(v) = std::env::var("AIGX_SERVER__PORT") {
        if let Ok(port) = v.parse() {
            config.server.port = port;
        }
    }
    if let Ok(v) = std::env::var("AIGX_SERVER__DATA_DIR") {
        config.server.data_dir = v;
    }
    if let Ok(v) = std::env::var("AIGX_CF_BINDING_URL") {
        config.cf_binding_url = v;
    }
    if let Ok(v) = std::env::var("AIGX_SERVER_ADDRESS") {
        config.server_address = v;
    }
    if let Ok(v) = std::env::var("AIGX_USAGE__MAX_REQUEST_BODY_MB") {
        if let Ok(n) = v.parse() {
            if n > 0 {
                config.usage.max_request_body_mb = n;
            }
        }
    }
    config
}

#[cfg(test)]
mod tests {
    use super::redact_url_credentials;

    #[test]
    fn redacts_database_credentials_in_logs() {
        assert_eq!(
            redact_url_credentials("postgres://user:secret@127.0.0.1:5432/db"),
            "postgres://***@127.0.0.1:5432/db"
        );
        assert_eq!(
            redact_url_credentials("postgres://127.0.0.1:5432/db"),
            "postgres://127.0.0.1:5432/db"
        );
        assert_eq!(redact_url_credentials(""), "");
        assert_eq!(
            redact_url_credentials("not a url"),
            "<invalid database url>"
        );
    }

    #[test]
    fn retry_status_codes_are_cleaned_and_queryable() {
        super::set_retry_on_status_codes(vec![402, 402, 99, 700, 404]);
        assert!(super::is_retry_status(402));
        assert!(super::is_retry_status(404));
        // 越界状态码被丢弃
        assert!(!super::is_retry_status(99));
        assert!(!super::is_retry_status(700));
        // 恢复默认，避免影响其它用例
        super::set_retry_on_status_codes(vec![402]);
    }

    #[test]
    fn cline_overrides_merge_case_insensitively_and_can_drop_headers() {
        let base = vec![
            ("user-agent".to_string(), "Cline/3.0.62".to_string()),
            ("x-client-type".to_string(), "cline-sdk".to_string()),
            ("x-title".to_string(), "Cline".to_string()),
        ];
        let overrides = vec![
            // 改大小写写法也必须把默认那条撑掉（不能变成两条同名头）
            ("User-Agent".to_string(), "Cline/3.0.99".to_string()),
            // 空串 = 这个头不发
            ("x-title".to_string(), String::new()),
            // 覆盖表里没有的键 = 新增自定义头
            ("x-custom".to_string(), "1".to_string()),
        ];
        let merged = super::merge_cline_header_overrides(base, &overrides);
        let ua: Vec<&(String, String)> = merged
            .iter()
            .filter(|(name, _)| name.eq_ignore_ascii_case("user-agent"))
            .collect();
        assert_eq!(ua.len(), 1, "同名头只能保留覆盖后的那一条");
        assert_eq!(ua[0].1, "Cline/3.0.99");
        assert!(!merged.iter().any(|(name, _)| name == "x-title"));
        assert!(merged
            .iter()
            .any(|(name, value)| name == "x-custom" && value == "1"));
        // 没被覆盖的默认头原样保留
        assert!(merged
            .iter()
            .any(|(name, value)| name == "x-client-type" && value == "cline-sdk"));
    }

    #[test]
    fn cline_overrides_cannot_replace_fixed_headers() {
        let base = vec![("x-title".to_string(), "Cline".to_string())];
        let overrides = vec![
            ("Authorization".to_string(), "Bearer stolen".to_string()),
            ("content-type".to_string(), "text/plain".to_string()),
        ];
        let merged = super::merge_cline_header_overrides(base, &overrides);
        assert_eq!(merged.len(), 1, "固定头不会被覆盖表写入");
        assert_eq!(merged[0].0, "x-title");
    }
}
