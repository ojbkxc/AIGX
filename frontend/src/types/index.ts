/**
 * AIGX核心类型定义
 * 统一的TypeScript类型系统，为整个前端应用提供类型基础
 */

import type { ChannelQuirks } from '../lib/channelPresets';
import type { LimiterGroup } from './limiter';

// ============================================================================
// 核心领域类型
// ============================================================================

export enum ChannelType {
  OPENAI = 'openai',
  ANTHROPIC = 'anthropic',
  CLOUDFLARE = 'cloudflare',
  GEMINI = 'gemini',
  CUSTOM = 'custom'
}

enum ChannelStatus {
  ACTIVE = 'active',
  INACTIVE = 'inactive',
  DEPRECATED = 'deprecated',
  FAILED = 'failed',
  MAINTENANCE = 'maintenance'
}

// 数据库实体类型
export interface User {
  id: string | number;
  username: string;
  email: string;
  role?: string;
  api_keys?: string[];
  group?: string;
  quota?: number | null;
  remaining?: number | null;
  used_quota?: number;
  status?: string;
  totp_enabled?: boolean;
  /** 邀请码（对齐 new-api AffCode） */
  aff_code?: string;
  /** 已邀请人数 */
  aff_count?: number;
  /** 待领取邀请奖励（可划转到可用配额） */
  aff_quota?: number;
  /** 累计邀请获得配额（历史总量） */
  aff_history_quota?: number;
  created_at?: number;
  updated_at?: number;
}

export interface Channel {
  id: string;
  name: string;
  type: ChannelType;
  status: ChannelStatus;
  config: Record<string, unknown>;
  settings: ChannelSettings;
  health: ChannelHealth;
  created_at: string;
  updated_at: string;
}

export interface ChannelSettings {
  priority: number;
  weight: number;
  max_concurrent: number;
  rate_limit: number;
  failover_enabled: boolean;
  cooldown_seconds: number;
  meta: Record<string, unknown>;
}

export interface ChannelHealth {
  status: 'healthy' | 'unhealthy' | 'degraded';
  response_time: number;
  error_rate: number;
  last_check: string;
  metrics: HealthMetrics;
}

export interface HealthMetrics {
  requests_total: number;
  failures_total: number;
  avg_response_time: number;
  error_codes: Record<string, number>;
}

export interface ApiKey {
  id: string;
  user_id: string;
  key_hash: string; // 存储时哈希处理
  key_prefix: string;
  name: string;
  creation_date: string;
  expiry_date?: string;
  last_used?: string;
  uses_total: number;
  rate_limits: RateLimitConfig;
  permissions: string[];
  created_at: string;
  updated_at: string;
}

export interface RateLimitConfig {
  requests_per_minute: number;
  tokens_per_minute: number;
  burst_allowed: boolean;
  burst_limit: number;
}

export interface BillingRecord {
  id: string;
  user_id: string;
  channel_id: string;
  tokens_used: number;
  tokens_unit: 'prompt' | 'completion';
  currency: string;
  amount: number;
  status: 'pending' | 'processed' | 'failed';
  created_at: string;
}

// ============================================================================
// API响应类型
// ============================================================================

export interface ApiResponse<T = unknown> {
  success: boolean;
  data?: T;
  error?: ApiError;
  meta?: ResponseMeta;
  /** 后端因体积/条目上限截断结果时为 true（如公开源拉取超 UTF-16 体积闸） */
  truncated?: boolean;
}

export type ApiList<T> = ApiResponse<T[]> & {
  total?: number;
  page?: number;
  size?: number;
};

export type ApiRecord = ApiResponse<Record<string, unknown>>;

export interface ApiError {
  code: string;
  message: string;
  details?: Record<string, unknown>;
  status: number;
}

export interface ResponseMeta {
  timestamp: string;
  request_id: string;
  version: string;
  prefect: string;
}

// ============================================================================
// Auth类型
// ============================================================================

export interface LoginRequest {
  email: string;
  password: string;
}

export interface RegisterRequest {
  email: string;
  password: string;
  username: string;
  confirm_password: string;
}

export interface AuthResponse {
  user: User;
  token: string;
  refresh_token: string;
}

export interface LoginResult {
  token: string;
  email: string;
  username: string;
  role: string;
  expires_at: number;
  require_2fa?: boolean;
  tmp_token?: string;
  recovery_codes?: string[];
  /** 首次启动的内置管理员用弱口令初始化：前端应强制跳转改密页 */
  must_change_password?: boolean;
}

export interface LoginCodeResult {
  sent?: boolean;
  expire_seconds?: number;
  require_2fa?: boolean;
  code?: string;
}

export interface RegisterResult {
  token: string;
  email: string;
  username?: string;
  role?: string;
  expires_at?: number;
  require_2fa?: boolean;
  tmp_token?: string;
}

export interface MessageResult {
  message?: string;
  success?: boolean;
  sent?: boolean;
  token?: string;
}

export interface RefreshTokenRequest {
  refresh_token: string;
}

// ============================================================================
// Dashboard类型
// ============================================================================

export interface DashboardStats {
  total_channels: number;
  active_channels: number;
  active_users: number;
  total_api_keys: number;
  monthly_usage: number;
  error_rate: number;
  avg_response_time: number;
}

export interface RealtimeMetrics {
  timestamp: string;
  metrics: {
    requests_per_second: number;
    active_connections: number;
    server_load: number;
    memory_usage: number;
    cpu_usage: number;
  };
  channels: {
    [channelId: string]: {
      status: string;
      requests_current: number;
      requests_failed: number;
      avg_latency: number;
    };
  };
}

export interface ChannelUsage {
  channel_id: string;
  channel_name: string;
  requests_today: number;
  tokens_today: number;
  current_load: number;
  status: string;
}

export interface ChannelItem {
  id: string;
  /** 展示用短编号（列表接口按创建顺序注入 1..N） */
  seq?: number;
  name: string;
  channel_type?: string;
  base_url?: string;
  /** 备用入口（同一上游的备用 base_url 列表，仅可信 HTTPS；按顺序尝试） */
  backup_base_urls?: string[];
  models?: string[];
  /// 上游 /models 自动发现并缓存的模型快照（"留空=全部"渠道调试下拉用）
  discovered_models?: string[];
  /// 渠道状态（"enabled" / "disabled" 等）；后端始终返回
  status?: string;
  priority?: number;
  weight?: number;
  /** 脱敏后的密钥；空字符串 = 无密钥渠道（免鉴权上游） */
  api_key?: string;
  model_mapping?: Record<string, string>;
  cost_pricing?: Record<string, { input_price?: number; output_price?: number; price_type?: string }>;
  /// Cloudflare account_id（仅 channel_type=cloudflare 时有值）
  account_id?: string;
  /// 上游特判（预置供应商卡片写入；手工新建 = 空对象）
  quirks?: ChannelQuirks | null;
  /// 插件渠道接管的 provider id（channel_type=plugin 时必填；serde(default)，老渠道空串）
  plugin_provider?: string;
  /// 余额查询方案（空 = 只试 OpenAI 计费端点）
  balance_scheme?: string;
  /// 最近一次余额查询的展示单位（USD / CNY / % / quota …）
  balance_unit?: string;
  created_at?: string | number;
  updated_at?: string;
  last_used_at?: number | null;
  last_error?: string | null;
  /// 最近一次连通性测试延迟（毫秒）；后端 Option 序列化为 null
  response_time?: number | null;
  /// 最近一次连通性测试时间戳（秒）；后端 Option 序列化为 null
  test_time?: number | null;
  /// 最近一次查询到的上游余额（美元）；后端 Option 序列化为 null
  balance?: number | null;
  /// 最近一次查询到的上游 credit；后端 Option 序列化为 null
  credit?: number | null;
  /// #32 PR3：周期余额巡检间隔（秒）。后端 `mask_channel` 三键**恒显式序列化**、
  /// 不带 `skip_serializing_if` ⇒ `null` 与「旧数据没这字段」是同一个状态，
  /// 一律按「未启用」渲染，**不能**区分成「未配置」，也不能画成「每 0 秒」。
  balance_query_interval_secs?: number | null;
  /// 上次**自动巡检查询**时间（unix **秒**）。手动「点击更新余额」刻意不写这条
  /// （只有 `channel::balance_refresh` 巡检写回），所以文案口径是「上次自动查询」，
  /// 并注明「由周期任务更新」——否则用户点了刷新却发现时间没动，会以为界面坏了。
  balance_last_query_at?: number | null;
  /// 上次自动巡检的失败文案（已过出口侧 `mask_error_text` 脱敏：截断 500、
  /// `Bearer <token>`/`sk-<alnum>` 打码；裸 JWT / 手机号不在拦围内，best-effort）。
  balance_last_error?: string | null;
}

export interface TokenItem {
  id: string;
  name: string;
  status?: string;
  used_quota?: number;
  model_limit?: string;
  created_at?: string;
  last_used_at?: string;
  [key: string]: unknown;
}

export interface TokenKeyResult {
  plain_key?: string;
  key?: string;
}

export interface PriceEntry {
  model: string;
  prompt_price?: number;
  completion_price?: number;
  prompt_ratio?: number;
  completion_ratio?: number;
  [key: string]: unknown;
}

export interface GroupItem {
  name: string;
  ratio?: number;
  allowed_models?: string[] | string;
  description?: string;
  [key: string]: unknown;
}

export interface OrderItem {
  id?: string;
  trade_no?: string;
  user_id?: string;
  amount?: number;
  money?: number;
  quota?: number | null;
  method?: string;
  payment_method?: string;
  status?: string;
  created_at?: string;
  create_time?: number;
  paid_time?: number | null;
  [key: string]: unknown;
}

export interface PlanItem {
  id: string;
  name: string;
  price?: number;
  /** 额度上限（配额单位；0 = 不限额度） */
  quota?: number;
  /** 有效期天数（0 = 永不过期） */
  duration_days?: number;
  group?: string;
  allowed_models?: string[] | null;
  description?: string;
  enabled?: boolean;
  /** 已按此套餐发放的 key 数 */
  issued_count?: number;
  created_at?: number;
  // ── 订阅化扩展（#85，对齐 new-api SubscriptionPlan）──────────────────
  /** 套餐类型：once（按量发 key，默认）/ subscription（时长订阅） */
  plan_type?: string;
  /** 订阅时长单位：year / month / day / hour / custom */
  duration_unit?: string;
  /** 订阅时长数值（custom 时忽略） */
  duration_value?: number;
  /** custom 时长的秒数 */
  custom_seconds?: number;
  /** 订阅总配额（0 = 不限） */
  total_amount?: number;
  /** 订阅配额周期重置：never / daily / weekly / monthly / custom */
  quota_reset_period?: string;
  /** custom 重置周期的秒数 */
  quota_reset_custom_seconds?: number;
  /** 是否允许余额购买订阅 */
  allow_balance_pay?: boolean;
  /** 订阅池耗尽后是否允许回退用户钱包 */
  allow_wallet_overflow?: boolean;
  /** 每用户最多购买次数（0 = 不限） */
  max_purchase_per_user?: number;
  /** 购买后升级到的用户分组（空 = 不变） */
  upgrade_group?: string;
  /** 到期后回退到的分组（空 = 回退购买前分组） */
  downgrade_group?: string;
  /** 展示排序（升序） */
  sort_order?: number;
  [key: string]: unknown;
}

/** 用户订阅实例（#85，对齐 new-api UserSubscription） */
export interface UserSubscriptionItem {
  id: string;
  user_id: string;
  plan_id: string;
  /** 订阅总配额（0 = 不限） */
  amount_total: number;
  /** 已用配额 */
  amount_used: number;
  start_time: number;
  end_time: number;
  /** active / expired / cancelled */
  status: string;
  /** balance / admin */
  source: string;
  last_reset_time: number;
  next_reset_time: number;
  upgrade_group: string;
  prev_user_group: string;
  downgrade_group: string;
  allow_wallet_overflow: boolean;
  created_at: number;
  updated_at: number;
  /** 展示快照（后端 sub_json 附加） */
  plan_name?: string;
  plan_price?: number;
  [key: string]: unknown;
}

export interface RedemptionItem {
  id: string;
  code: string;
  usage_count?: number;
  status?: string;
  created_at?: string;
  expired_at?: string;
  [key: string]: unknown;
}

/** 工单消息（is_me 以工单发起人视角标记） */
export interface TicketMessageItem {
  id: string;
  ticket_id: string;
  user_id: string;
  message: string;
  created_at: number;
  is_me: boolean;
  [key: string]: unknown;
}

/** 工单（对齐 v2board Ticket） */
export interface TicketItem {
  id: string;
  user_id: string;
  subject: string;
  level: number;
  status: number;
  reply_status: number;
  created_at: number;
  updated_at: number;
  /** 详情接口附带的消息列表 */
  message?: TicketMessageItem[];
  /** 管理端列表附带（邮箱由后端补时可选） */
  email?: string;
  [key: string]: unknown;
}

export interface AlertRule {
  [key: string]: unknown;
}

export interface AlertEvent {
  [key: string]: unknown;
}

export interface NotifyConfigItem {
  [key: string]: unknown;
}

export interface IpFilterEntry {
  [key: string]: unknown;
}

export interface CacheStatsItem {
  [key: string]: unknown;
}

export interface SystemMonitorItem {
  [key: string]: unknown;
}

export interface PricingSyncConfigItem {
  [key: string]: unknown;
}

export interface ExchangeRatesItem {
  [key: string]: unknown;
}

export interface HealthArchiveItem {
  [key: string]: unknown;
}

export interface IpFilterConfigItem {
  [key: string]: unknown;
}

export interface RateLimitConfigItem {
  [key: string]: unknown;
}

export interface SettingsItem {
  mappings?: unknown;
  usage?: unknown;
  limits?: unknown;
  notification?: unknown;
  [key: string]: unknown;
}

/** 一条在途请求（管理员日志页可见并可终止） */
export interface InflightRequestItem {
  id: string;
  user_id?: string | null;
  key_id?: string | null;
  model: string;
  channel_id?: string | null;
  channel_name?: string | null;
  /** 开始时刻（unix 秒） */
  started_at: number;
  /** 已运行毫秒 */
  elapsed_ms: number;
  stream: boolean;
}

export interface ModelInfo {
  id: string;
  object?: string;
  owned_by?: string;
  context_length?: number | null;
  capabilities?: string[];
  [key: string]: unknown;
}

export interface ModelMetaOverride {
  owned_by: string;
  context_length?: number | null;
  capabilities?: string[];
  /** 思考等级候选（可选，新后端提供；旧后端忽略该字段） */
  reasoning_levels?: string[];
}

// ==================== 上游模型目录发现 ====================

/** 目录条目 */
export interface CatalogEntry {
  id: string;
  upstream?: string;
  context_length?: number | null;
  capabilities?: string[];
}

/** 单个账号的目录同步结果 */
export interface CatalogSyncAccount {
  account_id: string;
  name: string;
  provider: string;
  /** remote（上游拉回）/ cache（命中缓存）/ fallback（静态兜底） */
  source: string;
  fetched_at: number;
  count: number;
  models: CatalogEntry[];
  /** 是否已写回账号的模型白名单 */
  applied: boolean;
  apply_error?: string | null;
}

/** 单个账号的目录同步失败 */
export interface CatalogSyncError {
  account_id: string;
  name: string;
  provider: string;
  error: string;
}

/** POST /api/account-sessions/catalog/sync 响应 */
export interface CatalogSyncResult {
  results: CatalogSyncAccount[];
  errors: CatalogSyncError[];
  ok_count: number;
  error_count: number;
  /** 同步后自动清理的失效映射明细（后端 cleanup 闸门通过才非空；旧后端无此字段） */
  removed_mappings?: Array<{ alias: string; target: string }>;
  /** 清理条数（0 = 没清或没失效映射） */
  removed_mapping_count?: number;
  /** 后端是否真的跑了清理（false = 三重闸门未通过未执行） */
  cleanup_ran?: boolean;
}

// ==================== 从本机客户端导入账号 ====================

/** cc-switch 扫描到的一条供应商 */
export interface ImportSourceEntry {
  id: string;
  appType: string;
  name: string;
  notes: string;
  baseUrl: string;
  apiKey: string;
  protocol: string;
  models: string[];
  modelMappings: Array<{ alias: string; target: string }>;
  importable: boolean;
  reason: string;
}

/** GET /api/account-sessions/import/sources 响应 */
export interface ImportSources {
  available: boolean;
  path: string;
  reason: string;
  total: number;
  providers: ImportSourceEntry[];
}

/** 提交导入的一条条目 */
export interface ImportEntry {
  id: string;
  name: string;
  base_url: string;
  api_key: string;
  protocol: string;
  models: string[];
  model_mappings: Array<{ alias: string; target: string }>;
}

/** POST /api/account-sessions/import 响应 */
export interface ImportRunResult {
  created: Array<{ id: string; name: string; base_url: string; models: string[] }>;
  errors: Array<{ id: string; name: string; error: string }>;
  created_count: number;
  error_count: number;
}

// ==================== 统一模型管理（渠道 + 账号池合流） ====================

/** 模型的一个承载方：渠道直连，或账号池里的某个账号 */
export interface ManagedModelSource {
  /** channel（渠道） / account（账号池） */
  kind: string;
  /** 渠道 id */
  channel_id: string;
  /** 渠道名或账号名 */
  name: string;
  /** 账号 id（仅 kind=account） */
  account_id?: string;
  /** 来源细分（可选，新后端提供）：remote=远程拉取 / modelsdev=社区目录补齐 / manual=手动登记 */
  source?: string;
}

/** 统一模型管理里的定价条目（对应后端 `pricing::ModelPrice`） */
export interface ManagedModelPrice {
  model_name: string;
  input_price: number;
  output_price: number;
  cache_price?: number | null;
  price_type: string;
}

/** 一条别名映射（带开关状态） */
export interface AliasEntry {
  alias: string;
  target: string;
  /** 关掉的别名仍出现在清单里，但不参与改名 */
  enabled: boolean;
  /** 映射级思考等级（None = 未设置） */
  reasoning_effort?: string | null;
}

/** 聚合后的单行模型：粒度是 (provider, model_id) */
export interface ManagedModel {
  provider: string;
  provider_label: string;
  id: string;
  /**
   * 逐模型清单来源（对齐 Agent2API per-model `source` 三态/四档）：
   * `manual`（手动登记）> `remote`（上游目录/发现拉回）> `builtin`（渠道声明/账号白名单）
   * > `modelsdev`（仅 models.dev 元数据兜底）。后端 `ManageModel.source` 总会下发一档
   * （build 收尾把空串补成 `modelsdev`）；旧后端可能不带 → 前端按空串兜底。
   */
  source?: string;
  /**
   * 派生启用态（对齐 Agent2API rowEnabled）：默认绑定开着 **或** 任一条别名开着即为 true。
   * 存储仍是 model_rules.disabled（零迁移），此值由后端 `build_view` 收尾时派生。
   */
  enabled: boolean;
  /**
   * 默认绑定（id→id 自映射）那颗开关 = `!model_rules.is_disabled(provider, id)`。
   * 「映射」列首颗 chip 用它，写入仍走 `POST /api/models/state`（SPEC §2.1）。
   */
  default_enabled: boolean;
  sources: ManagedModelSource[];
  owned_by: string;
  context_length?: number | null;
  capabilities: string[];
  /**
   * 能力/上下文取数来源标记（后端 Phase1-A4 `cap_sources`：builtin < modelsdev < user；
   * 字段级 null = 无读数）。#51 P1#4：caps 列据此上色/悬停显示可信度。旧后端可能不下发。
   */
  cap_sources?: {
    context_length?: string | null;
    capabilities?: Record<string, string>;
    reasoning_levels?: unknown;
  } | null;
  /**
   * 能力三态生效值（后端 `capability_flags`，六键；null = 「不知道」≠「不支持」）。
   * #51 P1#2：caps 列补显 maxOutputTokens（输入上下文用 context_length）。
   */
  capability_flags?: {
    maxInputTokens?: number | null;
    maxOutputTokens?: number | null;
    supportsToolCall?: boolean | null;
    supportsImages?: boolean | null;
    supportsVideo?: boolean | null;
    supportsReasoning?: boolean | null;
  } | null;
  /** 被用户覆盖的能力键（管理页「已改」标记；空/缺省 = 全部继承） */
  cap_overrides?: string[];
  /** 工具调用模式（native / emulated / disabled；缺省 = 未声明） */
  tool_calling_mode?: string | null;
  /** 定价条目（未定价为 null） */
  price?: ManagedModelPrice | null;
  /** 模型倍率（缺省 1.0） */
  model_ratio: number;
  /** 该模型的对外别名 */
  aliases: string[];
  /** 别名明细（含开关）。老后端不返回时前端回退到 aliases 合成 */
  alias_entries?: AliasEntry[];
}

/** 左栏 provider 分组 */
export interface ManagedProviderGroup {
  id: string;
  label: string;
  total: number;
  enabled: number;
  /** 是否来自账号池 */
  account: boolean;
}

/** 统一模型管理完整视图 */
export interface ManageView {
  providers: ManagedProviderGroup[];
  models: ManagedModel[];
  /** 全局别名映射 alias → target（条目值也接受对象形态，新后端可带悬空标记） */
  mappings: Record<string, string | { target?: string; dangling?: boolean; enabled?: boolean }>;
  /** 未定价的模型数 */
  unpriced: number;
  /**
   * 后端逐条映射（后端 `mapping_entries`，含 provider 维度 / 关闭 / 悬空）。
   * 新前端按这一份渲染 chip 与开关；旧后端不下发。
   */
  mapping_entries?: Array<{ alias: string; target: string; provider?: string | null; enabled?: boolean; dangling?: boolean }>;
  /**
   * 后端标记的悬空映射（后端 `dangling`；目标模型不在清单里）。
   * #51 P1#8：后端字段名是 `dangling`，旧前端误读 `dangling_mappings` 只能自行推断。
   */
  dangling?: Array<{ alias: string; target: string; provider?: string | null; dangling?: boolean }>;
  /** 旧后端/旧前端遗留字段名：后端标记的悬空映射（现后端改发 `dangling`） */
  dangling_mappings?: Array<{ alias: string; target: string }>;
}

// ==================== 模型测试（POST /api/models/test） ====================

/** 模型测试请求（钉 provider+model，可选钉账号/渠道） */
export interface ModelTestPayload {
  provider: string;
  model: string;
  account_id?: string;
  channel_id?: string;
  prompt?: string;
  system_prompt?: string;
  reasoning_effort?: string;
  stream?: boolean;
  /**
   * 对话协议（#60 / #54 后端）：chat_completions（默认，原生链路）/ responses（OpenAI
   * Responses 透传）/ anthropic（按 Anthropic 线格式构造）。缺省 = chat_completions。
   */
  protocol?: 'chat_completions' | 'responses' | 'anthropic';
  /**
   * 测试出口（#60 / #54 后端）：direct / pool / pool:<id> / node:<id> / 代理 URL；
   * 缺省 = 跟随账号自身代理 / 渠道默认直连（不传）。
   */
  proxy?: string;
  /** `proxy` 的别名（A2A 叫「测试节点」）；与 proxy 同时给出时 proxy 优先。 */
  node?: string;
}

/** 实际承载本次测试的来源 */
export interface ModelTestSource {
  kind: string;
  channel_id: string;
  name: string;
  account_id?: string | null;
}

/** 模型测试结构化结论（HTTP 恒 200，成败看 ok） */
export interface ModelTestResult {
  ok: boolean;
  error?: string;
  /** 上游 HTTP 状态（失败时给出，用于下一步建议） */
  status?: number;
  reply?: string;
  reasoning?: string;
  upstream_model?: string;
  duration_ms?: number;
  ttfb_ms?: number | null;
  prompt_tokens?: number;
  completion_tokens?: number;
  total_tokens?: number;
  stream?: boolean;
  /** 本次实际使用的对话协议（后端回显 chat_completions / responses / anthropic） */
  protocol?: string;
  source?: ModelTestSource;
}

// ==================== 签到中心（管理员替上游账号签到） ====================

export interface CheckinSettings {
  enabled: boolean;
  /** 逐个签到之间的间隔（毫秒） */
  gap_ms: number;
  /** 台账保留天数（0 = 永久保留不自动清理；后端 prune 口径） */
  keep_days?: number;
  /** 签到时区偏移（小时；缺省 8 = UTC+8，0 = 跟随系统本地时区） */
  timezone_offset?: number;
  /** 失败/窗口未开的重试间隔（毫秒；0 = 关闭重试，缺省 10 分钟） */
  retry_gap_ms?: number;
}

/** 一条定时条目的逐家行（A2A `ScheduleProviderRow`） */
export interface CheckinScheduleProviderRow {
  id: string;
  label: string;
  /** 该家生效时刻（HH:MM） */
  time: string;
  /** 区间结束（有窗口时） */
  timeEnd?: string;
  /** 内置专属时刻（null = 跟随条目时刻） */
  builtin?: string | null;
  /** 今天跑过且没有待执行账号 */
  firedToday: boolean;
  /** 当日排程账号数 / 已尝试 / 待执行 */
  accountCount?: number;
  attemptedCount?: number;
  pendingCount?: number;
  /** 下一个待执行账号的时刻（unix 毫秒） */
  nextAccountAt?: number | null;
  /** 到点了但还没跑（false 且 firedToday=false = 在等窗口） */
  overdue?: boolean;
}

/** 一轮执行的摘要（A2A `lastResult` / `checkin_history` 行） */
export interface CheckinResultSummary {
  at?: number;
  date?: string;
  reason?: string;
  succeeded?: number;
  /** 活跃保活数（WorkBuddy 国际版） */
  active?: number;
  total?: number;
  skipped?: number;
  failed?: string[];
  failedCount?: number;
  /** 今天每账号已重试次数（窗口重试轮计数） */
  retryAttempts?: Record<string, number>;
}

/** 一条定时签到条目（A2A `ScheduleEntry`，camelCase 契约） */
export interface CheckinScheduleEntry {
  id: string;
  label: string;
  /** 展示名（label 为空时退到 time） */
  displayLabel: string;
  enabled: boolean;
  /** 每天时刻 HH:MM（按配置时区） */
  time: string;
  /** 随机签到区间结束 HH:MM（空 = 无窗口） */
  timeEnd?: string;
  randomize?: boolean;
  providers: string[];
  /** 逐家时刻覆盖（{provider: "HH:MM"}；空串 = 撤销覆盖） */
  providerTimes: Record<string, string>;
  /** 逐家生效时刻与今日状态（后端算好，界面直接渲染） */
  providerRows: CheckinScheduleProviderRow[];
  /** 补签轮时刻（空 = 关闭补签轮） */
  retryTime: string;
  /** 逐家补签时刻（窗口结束 + 等待间隔，不早于 retryTime） */
  providerRetryTimes?: Record<string, string>;
  /** 中性未领取的重试间隔（分钟；0 = 不重试） */
  retryGapMinutes: number;
  /** 每账号每天的重试次数上限（0 = 不重试） */
  retryLimit: number;
  lastFiredDate: string | null;
  lastFiredToday: boolean;
  /** 今天还没跑的家（界面显示「待执行」） */
  pendingProviders: string[];
  /** 中性未领取、等窗口重试的账号数 */
  retryCount: number;
  /** 今天重试次数已用完的账号 id（今天不再重试、明天重置） */
  retryExhausted?: string[];
  /** 今天每账号已重试次数 */
  retryAttempts?: Record<string, number>;
  nextRunAt: number | null;
  lastResult: CheckinResultSummary | null;
}

/** 自动签到状态（A2A `AutoState`） */
export interface CheckinAutoState {
  enabled: boolean;
  running: boolean;
  providerOptions: Array<{ id: string; label: string; builtinTime?: string | null }>;
  /** 缺省补签时刻（「补签时刻」输入框 placeholder） */
  defaultRetryTime?: string;
  schedules: CheckinScheduleEntry[];
  /* ── 兼容视图（默认条目投影；旧界面/启动日志直读）── */
  time?: string;
  timeEnd?: string;
  providers?: string[];
  lastFiredDate?: string | null;
  lastFiredToday?: boolean;
  lastResult?: CheckinResultSummary | null;
  nextRunAt?: number | null;
  /** AIGX 扩展：签到设置（设置卡直读） */
  settings?: CheckinSettings;
}

/** 一行每日签到账号（A2A `CheckinAccount`） */
export interface CheckinAccountRow {
  id: string;
  name: string;
  /** 账号是否可用（false = 凭证失效/停用，仍列出但降级展示） */
  available: boolean;
  /** 上次签到时间（unix 毫秒） */
  checkinAt: number | null;
  checkedInToday: boolean;
  /** dailyCheckin 原始快照（插件/网关申报的当天逐项完成状态） */
  dailyCheckin?: unknown;
  /** 版本档（cn / intl），按钮文案据此区分「签到 / 领日活」 */
  edition: string | null;
  /** 成长中心连登天数（0 = 还没读到过） */
  streakDays: number;
  /* ── AIGX 展示扩展（「最近结果」列直读）── */
  lastOutcome?: string | null;
  lastMessage?: string | null;
  lastTs?: number | null;
  channel_id?: string;
}

/** 按提供商分组的每日签到行（A2A `ProviderGroup`） */
export interface CheckinProviderGroup {
  id: string;
  label: string;
  accounts: CheckinAccountRow[];
  doneCount: number;
  totalCount: number;
  /** 该家同时有每日签到与限时套餐（领取要逐份做） */
  manualPlans?: boolean;
  description?: string | null;
  modes?: string[] | null;
  uiHooks?: string[] | null;
}

/** 范围外账号（按提供商 + 原因聚合） */
export interface CheckinOutOfScope {
  label: string;
  reason: string;
  count: number;
}

/** 逐账号福利入口（新手福利 / 成长任务） */
export interface CheckinExtraEntry {
  id: string;
  name: string;
  provider: string;
}

/** 新手福利入口（A2A `extras.onboarding`） */
export interface CheckinOnboardingEntry extends CheckinExtraEntry {
  /** 一次性福利的结算记忆（已结清才非空） */
  settled?: { at?: number | null } | null;
  /** 待领项数（null = 还没盘点过） */
  pending?: number | null;
}

/** 活动套餐入口（A2A `extras.plans`；带本地领取台账） */
export interface CheckinPlanEntry extends CheckinExtraEntry {
  claimAt: number | null;
  claimPlans: Record<string, number> | null;
}

/** 活动福利入口（A2A `extras.campaigns`） */
export interface CheckinCampaignEntry extends CheckinExtraEntry {
  available: boolean;
}

/** 签到轮次时间线（A2A `HistoryRow`） */
export interface CheckinHistoryRow {
  at: number;
  date?: string;
  reason?: string;
  succeeded?: number;
  active?: number;
  total?: number;
  skipped?: number;
  failed?: string[];
  failedCount?: number;
}

/** 一家申报的签到链路信息（A2A `checkinFeatures`） */
export interface CheckinFeature {
  daily?: boolean;
  onboarding?: boolean;
  plans?: boolean;
  growth?: boolean;
  manualPlans?: boolean;
  excludeGuest?: boolean;
  description?: string | null;
  modes?: string[] | null;
  uiHooks?: string[] | null;
  /** AIGX 扩展：该家的全部福利动作 */
  benefitActions?: string[];
}

/** GET /api/checkin-center 的快照（A2A flat 形状） */
export interface CheckinCenterData {
  daily: {
    providers: CheckinProviderGroup[];
    outOfScope: CheckinOutOfScope[];
    todayDone: number;
    todayEligible: number;
  };
  extras: {
    onboarding: CheckinOnboardingEntry[];
    plans: CheckinPlanEntry[];
    campaigns: CheckinCampaignEntry[];
    growth: CheckinExtraEntry[];
  };
  auto: CheckinAutoState;
  keepalive: KeepaliveConfig;
  travel?: CheckinTravelState;
  history: CheckinHistoryRow[];
  /** 面板可见性（daily_keepalive / growth_tasks） */
  adminPanels: string[];
  checkinFeatures: Record<string, CheckinFeature>;
  pluginTasks?: unknown[];
  /* ── AIGX 扩展 ── */
  settings: CheckinSettings;
  recordsTotal?: number;
  supportedProviders?: string[];
  unsupportedProviders?: string[];
}

/** 旅行/成长巡检（对齐 A2A growth_scheduler::state，camelCase） */
export interface CheckinTravelState {
  enabled?: boolean;
  available?: boolean;
  /** 巡检能力不可用时的暂停原因 */
  pausedReason?: string | null;
  paused_reason?: string | null;
  /** 每天自动巡检时刻列表（HH:MM，一行一个） */
  times?: string[];
  /** 今日已触发的计划时刻 */
  firedToday?: string[];
  fired_today?: string[];
  lastFiredDate?: string | null;
  lastFiredTime?: string | null;
  lastResult?: string | null;
  lastOutcome?: string | null;
  lastError?: string | null;
  running?: boolean;
  last_result?: string | null;
  last_run?: number | null;
}

export interface CheckinRunResult {
  account_id: string;
  account_name: string;
  provider: string;
  success: boolean;
  outcome: string;
  message: string;
}

/** 一次性/限时福利通道的一次执行结果（POST /api/checkin-center/benefits/run） */
export interface BenefitResult {
  ok: boolean;
  outcome: 'done' | 'already' | 'pending' | 'failed' | 'skipped';
  message: string;
  data?: unknown;
}

/** 保活免费模型链配置（GET/PUT /api/checkin-center/benefits/keepalive） */
export interface KeepaliveConfig {
  models: string[];
  default_models: string[];
  /** A2A 别名（camelCase；center 快照给这个） */
  defaultModels?: string[];
}

/** 福利通道清单（GET /api/checkin-center/benefits） */
export interface CheckinBenefitsData {
  accounts: Array<{
    id: string;
    name: string;
    provider: string;
    channel_id: string;
    status: string;
    /** 该账号可用的动作 id（newbie / onboarding / onboarding_claim / keepalive / claim / keepalive_claim / growth / plan / umid） */
    actions: string[];
    /** 待领新手福利项数（None = 还没盘点过） */
    pending_onboarding?: number | null;
    keepalive?: { date?: string; model?: string; keepalive_ok?: number } | null;
  }>;
  keepalive: KeepaliveConfig;
}

/** 签到记录明细行（A2A `/api/checkin-records`，camelCase） */
export interface CheckinRecord {
  /** 行号（分页 offset + 序号；删除定位不依赖它） */
  rowId?: number;
  /** unix 秒 */
  ts: number;
  date: string;
  accountId: string;
  accountName: string;
  /** 展示名（后端已换成 provider label） */
  provider: string;
  /** provider 原始 id（筛选/展示兜底用） */
  providerId?: string;
  source: string;
  outcome: string;
  ok: boolean;
  /** 本次执行链是否附带保活动作 */
  keptActive: boolean;
  message: string;
  /** 记录类型：manual | schedule | makeup | retry —— 与 source 同值双写 */
  kind?: string;
  /** 触发原因（人话，界面的「来源/说明」列直读） */
  reason?: string;
  /** 失败原文（成功 / 今日已领时为空串）—— 台账「失败原因」列直读 */
  error?: string;
  /** 签到后回读的成长中心连登天数 */
  streakDays?: number;
}

export interface TotpSetupResult {
  secret: string;
  otpauth_url: string;
  otpauth_uri?: string;
}

export interface TotpEnableResult {
  recovery_codes?: string[];
  [key: string]: unknown;
}

export interface TotpDisableResult {
  success?: boolean;
  [key: string]: unknown;
}

export interface PlaygroundChatRequest {
  channel_id?: string;
  protocol?: string;
  model?: string;
  message?: string | unknown;
  history?: unknown[];
  stream?: boolean;
  temperature?: number;
  max_tokens?: number;
  system_prompt?: string;
  /** Playground V2 Completions 模式：顶层 prompt（缺省走 chat messages） */
  prompt?: string;
  top_p?: number;
  presence_penalty?: number;
  frequency_penalty?: number;
  response_format?: unknown;
  json_mode?: boolean;
  [key: string]: unknown;
}

export interface PlaygroundChatResult {
  stream?: Array<{ content?: string }>;
  data?: { content?: string; error?: string; usage?: unknown };
  error?: string;
  success?: boolean;
}

/**
 * 流式用量收尾（OpenAI `stream_options.include_usage` 终帧 / 后端汇总帧）。
 *
 * `estimated`（#24，接 #21-B）：后端账号池归一化链路在流尾**合成**的 usage 是
 * 估算值——prompt 优先用上游回报真值、缺失才走 `count_chat_prompt` 估算，
 * completion 同理回退到对输出文本 `count_text`；两者都齐才写这一帧。合成场景
 * 帧里带顶层 `estimated: true`，上游回报真值时**不写**该键。UI 据此画角标，
 * 让用户分得清「扣费同源估算」与「上游真值」。
 */
export interface ChatStreamUsage {
  prompt_tokens?: number;
  completion_tokens?: number;
  total_tokens?: number;
  /** true = 后端估算（非上游回报）；缺省/未带 = 上游真值 */
  estimated?: boolean;
}

/**
 * 单条 tool_call 增量（已由适配层归一化）。
 * 流式协议里同一次调用按 `index` 分片下发：首帧带 id/name，后续帧只带
 * arguments 分片；结果（role:tool 回灌）可能以 result/output 字段出现。
 */
export interface ToolCallDelta {
  index: number;
  id?: string;
  name?: string;
  arguments?: string;
  result?: string;
}

/** UI 侧聚合后的工具调用（分片已拼接；result 由回灌层落地后填充）。 */
export interface ToolCallState {
  index: number;
  id?: string;
  name?: string;
  arguments: string;
  result?: string;
}

/** 工具调用卡片状态（流式 / 完成 / 中断 / 错误） */
export type ToolCallCardState = 'streaming' | 'done' | 'interrupted' | 'error';

/**
 * 真·流式回调：SSE 每解析到一个增量（OpenAI delta / Anthropic delta.text）
 * 立即回调一次。isEnd 标记 [DONE] / message_stop 帧。
 * kind 区分内容增量：reasoning 为深度思考（DeepSeek 式折叠面板），
 * content 为正文，tool_call 为工具调用分片，usage 为用量收尾。
 * error/end 帧沿用 content。
 */
export type ChatStreamDelta = {
  content: string;
  isEnd: boolean;
  kind?: 'content' | 'reasoning' | 'tool_call' | 'usage' | 'error';
  /** kind = tool_call 时的归一化分片 */
  toolCall?: ToolCallDelta;
  /** kind = usage 时的用量 */
  usage?: ChatStreamUsage;
};

export type ChatStreamCallback = (delta: ChatStreamDelta) => void;

export interface UsageSummaryItem {
  [key: string]: unknown;
}

export interface TrendItem {
  [key: string]: unknown;
}

export interface DashboardItem {
  [key: string]: unknown;
}

// ── 调度实时状态（/api/dashboard/scheduler_status）──────────────────────

export interface SchedulerBreakerItem {
  state: 'open' | 'halfopen' | 'closed';
  failure_count: number;
  failure_type: string | null;
  cooldown_remaining_secs: number;
  rate_limit_remaining_secs: number;
  probe_in_flight: boolean;
}

export interface SchedulerHealthItem {
  auth_ok: boolean;
  balance_status: string;
  overall_error_rate: number;
  overall_avg_latency_ms: number;
  last_error: string | null;
}

export interface SchedulerAimdItem {
  current_limit: number;
  state: string;
}

export interface SchedulerArchiveItem {
  success: number;
  failure: number;
  trips: number;
  success_rate: number;
  p95_ms: number;
  last_error: string | null;
}

export interface SchedulerChannelItem {
  id: string;
  name: string;
  enabled: boolean;
  channel_type: string;
  priority: number;
  weight: number;
  models: string[];
  status: string;
  breaker: SchedulerBreakerItem | null;
  health: SchedulerHealthItem | null;
  aimd: SchedulerAimdItem | null;
  archive: SchedulerArchiveItem | null;
}

export interface SchedulerStatusItem {
  scheduler: {
    health_weight: number;
    cost_weight: number;
    rpm_weight: number;
  };
  channels: SchedulerChannelItem[];
}

// ============================================================================
// 提示词翻译
// ============================================================================

/** 提示词翻译请求项（index 对应原始数组下标） */
export interface PromptTranslateItem {
  index: number;
  content: string;
}

/** 提示词翻译结果（translated 仅含实际翻译的条目） */
export interface PromptTranslateResult {
  translated: PromptTranslateItem[];
}

/** 公开提示词源条目（/api/prompts/fetch 返回的 data 元素） */
export interface PromptSourceItem {
  name: string;
  content: string;
  tags: string[];
  source: string;
}

/** 公开提示词源列表项（/api/prompts/sources） */
export interface PromptSource {
  id: string;
  name: string;
  description: string;
  repo: string;
}

// ============================================================================
// 分页类型
// ============================================================================

export interface PaginationParams {
  page: number;
  limit: number;
  offset?: number;
}

export interface PaginationResponse<T> {
  data: T[];
  pagination: {
    total: number;
    page: number;
    limit: number;
    total_pages: number;
    has_next: boolean;
    has_prev: boolean;
  };
}

// ============================================================================
// 实用工具类型
// ============================================================================

export type ID = string;
export type UUID = string;

export interface DateTimeRange {
  start: Date;
  end: Date;
}

export interface PaginationOptions {
  page?: number;
  limit?: number;
  sortBy?: string;
  sortOrder?: 'asc' | 'desc';
}

export interface PaginatedResult<T> {
  items: T[];
  total: number;
  page: number;
  totalPages: number;
  limit: number;
}

// ============================================================================
// 状态管理类型
// ============================================================================

export interface AppState {
  user: User | null;
  isAuthenticated: boolean;
  loading: boolean;
  theme: 'light' | 'dark' | 'system';
  notifications: Notification[];
}

export interface Notification {
  id: string;
  type: 'success' | 'error' | 'warning' | 'info';
  title: string;
  message: string;
  timestamp: string;
  duration?: number;
}

// ============================================================================
// 表单类型
// ============================================================================

export interface FormValues<T> {
  values: T;
  errors: Record<string, string>;
  touched: Record<string, boolean>;
  isValid: boolean;
  isSubmitting: boolean;
}

export type FormErrors<T> = {
  [K in keyof T]?: string[];
};

// ============================================================================
// 图表数据类型
// ============================================================================

export interface ChartDataPoint {
  label: string;
  value: number;
  original?: unknown;
}

export interface ChartConfig {
  type: 'line' | 'bar' | 'pie' | 'doughnut';
  title: string;
  data: ChartDataPoint[];
  options?: ChartOptions;
}

export interface ChartOptions {
  height?: number;
  width?: number;
  responsive: boolean;
  padding?: number;
}

// ============================================================================
// 账号型渠道账号池（/api/account-sessions）
// ============================================================================

/** 渠道下的单个上游账号（凭证已脱敏） */
export interface AccountSessionItem {
  id: string;
  channel_id: string;
  channel_name?: string;
  name: string;
  /**
   * 账号邮箱（后端 `account_email_of` 由 `credential.fields["email"]` 派生，
   * 空白归一为 null；v1.0.91 #69 起随 session_json 下发）。原生账号也可有值。
   */
  email?: string | null;
  provider: string;
  /**
   * provider 可读展示名（后端 v1.0.94 下发）：原生家=注册表 label、
   * `custom-*`=「自定义供应商」、其它未知 kind 原样回 kind。
   * UI 的 provider 列/筛选条/分组带优先读它，杜绝空供应商行。
   */
  provider_label?: string;
  base_url?: string;
  /** 账号级对话路径覆盖（如 /api/chat），null = 默认 /chat/completions */
  chat_path?: string | null;
  /** 脱敏后的凭证 */
  api_key?: string;
  /** 账号级出口代理（http/https/socks5），null = 直连 */
  proxy?: string | null;
  /** 运行态：enabled / disabled / expired */
  status?: string;
  configured_status?: string;
  priority?: number;
  weight?: number;
  /**
   * 组内顺序（SPEC-1.0.49 §9 流 H）：同 channels+priority 组内升序，0 = 中位，
   * 负数置顶。列表「顺序」列直接展示它；旧后端不下发时该列渲染「—」。
   */
  sort_order?: number;
  max_concurrency?: number;
  models?: string[];
  /**
   * 模型可见性状态（SPEC-1.0.46 §2.2；后端 snake_case 序列化 `models_status`）。
   * 契约先写、后端未落地时不下发此字段 → 调用方按可选容错、不渲染。
   */
  models_status?: {
    state: 'whitelist' | 'catalog' | 'none' | 'failed';
    /** 当前可调度模型数（白名单优先；空则目录缓存计数；否则 0） */
    count: number;
    /** state==='failed' 时的失败原因（人类可读，中文） */
    error?: string | null;
  };
  /** 账号级额外请求头（登录态型供应商补专用头） */
  extra_headers?: Record<string, string>;
  /** 是否允许选路跳过该账号（A2A `allowRouteSkip`，缺省 true = 可被自动选路跳过；false = 仅显式指定用） */
  allow_route_skip?: boolean;
  /** 插件账号的出站 User-Agent 覆盖（空 = 用 provider 默认；A2A `userAgent`） */
  user_agent?: string;
  /** 是否已保存续期令牌（refreshToken / Coding Plan JWT 等） */
  has_refresh_token?: boolean;
  /** 续期令牌的脱敏形态（不回明文） */
  refresh_token?: string;
  in_flight?: number;
  success_count?: number;
  error_count?: number;
  /** 连续失败次数（成功清零，用于指数退避） */
  consecutive_errors?: number;
  avg_latency_ms?: number;
  /** 上游真名 → 剩余冷却秒数 */
  cooldowns?: Record<string, number>;
  last_error?: string | null;
  last_used_at?: number | null;
  expires_at?: number | null;
  /** 余额自动查询间隔（秒；null/缺省 = 关闭） */
  usage_query_interval_secs?: number | null;
  /** 余额查询执行参数（A2A usageQuery.retries / timeoutSeconds；后端归一化） */
  usage_query?: {
    retries: number;
    timeout_seconds: number;
  };
  /** 余额不足处理（A2A lowBalance，后端恒物化缺省）：off / skip / disable */
  low_balance?: {
    mode: 'off' | 'skip' | 'disable';
    threshold: number;
  };
  /** 上次余额查询时间（unix 秒） */
  last_balance_at?: number | null;
  /** 上次余额查询快照（与 /balance 的 data 同构） */
  last_balance?: AccountBalance | null;
  /** 上次余额查询失败原因（成功后清空） */
  last_balance_error?: string | null;
  /** 账号级限制器规则（PATCH limiters 键整块替换） */
  limiters?: AccountLimiterRule[];
  /**
   * 限制条件组（#29，camelCase 线格式，形状见 ./limiter.ts；后端
   * src/account_session/limiter.rs:141 LimiterGroup）。缺省/空 = 回落扁平
   * limiters 双读；非空时条件组优先生效。
   */
  limiter_groups?: LimiterGroup[];
  /** 出口代理当前可用性（后端 proxy_state_of；列表行内下拉的状态点） */
  proxy_state?: {
    mode: 'direct' | 'url' | 'pool' | 'rotate';
    ok: boolean;
    error?: string | null;
  };
  /** 限制器当前拦截快照（后端 limiter_state_of；「限额」筛选与状态徽章的唯一判据） */
  limiter_state?: {
    account_skip: boolean;
    skip_models: string[];
    disable: boolean;
  };
  /* ── 插件账号字段（A2A 对齐：secret 只写 / public 只读）── */
  /**
   * 只读公开字段（插件申报的公开账号信息：昵称 / uid / 版本 / 到期时间等）。
   * 后端**永不**回 secret；编辑时按只读展示，前端不参与写。
   */
  public?: Record<string, unknown> | null;
  /** 是否有插件 secret（只回布尔，不回内容；编辑时 `secret` 输入框留空 = 不修改） */
  has_secret?: boolean;
  /** 是否插件账号（后端 `is_plugin_account`；仅插件账号的 `plugin` 段有值） */
  is_plugin_account?: boolean;
  /**
   * 插件公开投影（后端 `plugin`；仅插件账号下发，含 hasAccessToken / hasRefreshToken /
   * canRefresh / expiresAt 与插件自报展示字段如 email/nickname）。#50 账号列 email 副行取它。
   */
  plugin?: Record<string, unknown> | null;
  /** 登录态推算的「需要重新登录」提醒时刻（unix 秒；仅登录态型账号有，后端 relogin_due_at） */
  relogin_due_at?: number | null;
  /**
   * 需要重新登录（后端 `needsRelogin` 合并语义：`status === "needs_relogin"` 或
   * 插件自报位）。true 时转发会被上游拒绝，需运维重新登录该账号。
   */
  needs_relogin?: boolean;
  /** 插件自报的独立 needs_relogin 位（`pluginNeedsRelogin`，插件账号才有） */
  plugin_needs_relogin?: boolean;
  created_at?: number;
  updated_at?: number;
}

/** 账号级限制器规则（对齐 Agent2API LimiterRule；枚举与后端校验白名单一致） */
export interface AccountLimiterRule {
  /** 规则类型：balance（余额阈值）/ token（周期用量阈值） */
  type: 'balance' | 'token';
  /** 触发动作：skip（本轮跳过该账号）/ disable（停用账号） */
  action: 'skip' | 'disable';
  /** 阈值：balance 为余额下限，token 为周期用量上限 */
  threshold: number;
  /** 重置方式：daily（每日零点重置）/ fixed（固定窗口，配合 period） */
  reset?: string;
  /** 固定窗口时长（秒，token+fixed 时有效，1800-86400） */
  period?: number;
  /** 是否启用（缺省 true） */
  enabled?: boolean;
}

/** 账号余额/额度归一化读数（GET /api/account-sessions/:id/balance） */
export interface AccountBalance {
  /** 可用总量（null = 上游没给，勿当 0） */
  available: number | null;
  /** 展示单位（积分 / token / 额度…） */
  unit: string;
  /** 明细桶：{type, displayName, balance, total?, used?} */
  wallets?: Array<Record<string, unknown>>;
  subscription?: Record<string, unknown> | null;
  /**
   * 分窗口额度读数（上游 usage windows：5h/周/月等多窗独立限额）。
   * 后端用量窗口端点（#12-A3）起下发；缺省 = 旧后端，余额格按主额度桶渲染。
   */
  windows?: AccountUsageWindow[];
}

/** 分窗口额度读数（余额快照 windows[] 条目；百分比与绝对值都可能缺省） */
export interface AccountUsageWindow {
  /** 结构标识（同组去重用；缺省按 group+window 拼） */
  key?: string;
  /** 分组名（多来源账号的窗口归属，如套餐名） */
  group?: string;
  /** 展示名（缺省按 group·window 拼） */
  label?: string;
  /** 窗口代码（5h / weekly / monthly / daily…） */
  window?: string;
  /** 剩余百分比（0-100；null = 上游没给） */
  remaining_percent?: number | null;
  /** 剩余绝对值（与 remaining_percent 二选一形态） */
  remaining?: number | null;
  limit?: number | null;
  /** 上游明示无上限 */
  unlimited?: boolean;
  /** 重置时刻（unix 秒；缺省 = 未返回） */
  resets_at?: number | null;
}

/** 批量操作 op（对齐上游 accounts_batch 六类；启停拆 enable/disable 两值） */
export type AccountBatchOp =
  | 'enable'
  | 'disable'
  | 'set_proxy'
  | 'set_max_concurrency'
  | 'query_settings'
  | 'limiter'
  | 'refresh_token'
  | 'delete';

/** 批量操作请求（POST /api/account-sessions/batch；逐条执行、部分失败不影响成功项） */
export interface AccountBatchRequest {
  op: AccountBatchOp;
  ids: string[];
  /** op=set_proxy：代理串（socks5/socks5h/http(s)/pool:/rotate:） */
  proxy?: string;
  /** op=set_max_concurrency：0 = 不限 */
  max_concurrency?: number;
  /** op=query_settings：自动查询开关 + 间隔秒（0 = 关闭） */
  usage_query_enabled?: boolean;
  usage_query_interval_secs?: number;
  /** op=limiter：五模式（append/replace 需带 limiters；enable/disable/clear 只翻开关） */
  limiter_mode?: 'append' | 'replace' | 'enable' | 'disable' | 'clear';
  limiters?: AccountLimiterRule[];
}

/** 批量操作逐条回执 */
export interface AccountBatchReceipt {
  id: string;
  ok: boolean;
  /** 失败原因（删除保护=渠道挂引用等；成功项缺省） */
  error?: string;
}

/** 账号导出文件（GET /api/account-sessions/export；含明文凭证，须当密码本保管） */
export interface AccountExportFile {
  version: number;
  exported_at: number;
  accounts: Array<Record<string, unknown>>;
}

/** provider 目录项（粘贴凭证型上游） */
export interface AccountProviderItem {
  kind: string;
  label: string;
  protocol: string;
  default_base_url: string;
  /** 登录态型供应商的鉴权方式（bearer / cookie / headers）；API Key 型为 null */
  auth?: string | null;
  /** 登录态型供应商的凭证粘贴提示 */
  credential_hint?: string | null;
  /** 登录态型供应商的默认对话路径（空 = 标准 /chat/completions） */
  chat_path?: string | null;
  /** 是否为 OpenAI 兼容形态；false = 上游需要私有协议/签名，简单转发会失败 */
  openai_compatible?: boolean | null;
  /** 上游非 OpenAI 协议但 AIGX 已内置专用适配（可直接转发） */
  native_supported?: boolean | null;
  /** 无默认地址 = 必须由账号自己填写上游地址 */
  requires_base_url?: boolean;
  /** 供应商形态分类（后端 C1）：agent = 客户端登录态、web = 网页形态、
   *  api = API Key 型；旧后端不带该字段（前端回退 auth 判据） */
  category?: string | null;
  /** 来源（A2A 对齐）：builtin = 网关内置 / plugin = 插件提供；旧后端不带 → 视为内置 */
  source?: 'builtin' | 'plugin' | null;
  /** 提供该 provider 的插件包 id（source=plugin 时有值） */
  pack_id?: string | null;
  /** 登录方式摘要（plugin 提供时后端下发：oauth / paste / sms / desktop / guest） */
  login?: string | null;
  /** 插件接管了原生家：true = 这家既有 AIGX 原生实现、又被插件声明，
   *  插件接管后原生降为兜底（`source=plugin` 且 `native_fallback=true`）。
   *  插件独占家 / 内置家为 false 或不下发；旧后端不带 → 视为 false。 */
  native_fallback?: boolean | null;
}

/** 账号池按渠道聚合统计 */
export interface AccountSessionStatsItem {
  channel_id: string;
  channel_name?: string;
  total: number;
  enabled: number;
  cooling: number;
  in_flight: number;
  success: number;
  error: number;
}

/** 账号路由（轮换策略）参数（GET/PUT /api/account-sessions/rotation） */
export interface AccountRotationPolicyItem {
  /** 基础冷却（秒） */
  base_cooldown_secs: number;
  /** 冷却上限（秒） */
  max_cooldown_secs: number;
  /** 认证失败冷却（秒） */
  auth_cooldown_secs: number;
  /** 单次请求最多尝试几个账号 */
  max_attempts: number;
}

/** 网络代理池条目（GET/POST /api/proxy-pool） */
export interface ProxyPoolItem {
  id: string;
  name: string;
  enabled: boolean;
  /** 来源：`manual`（手填）/ `clash`（从 Clash Verge 镜像而来） */
  source?: string;
  /** clash 来源条目的监听器 uid（manual 来源缺省） */
  listener_uid?: string;
  /** http / https / socks5 */
  protocol: string;
  host: string;
  port: number;
  username: string;
  /** 密码不回传，只回是否设置（编辑时留空 = 保留旧值） */
  has_password: boolean;
  createdAt?: number;
  updatedAt?: number;
  /** 上次测试结果（历史事实，不参与选路） */
  lastTest?: {
    success: boolean;
    ip: string;
    duration_ms: number;
    error?: string | null;
    tested_at: number;
  } | null;
  /** 引用它的账号（删除确认提示用） */
  usedBy?: Array<{ id: string; name: string; enabled: boolean }>;
}

/** 代理池列表响应（写操作也回整份，前端就地替换） */
export interface ProxyPoolResult {
  items?: ProxyPoolItem[];
  total?: number;
  entry?: ProxyPoolItem;
  [key: string]: unknown;
}

/** 代理连通性测试结果 */
export interface ProxyTestResult {
  success: boolean;
  status?: number | null;
  ip?: string;
  duration_ms?: number;
  error?: string | null;
  /** 测的是哪个出口（描述串，不含认证信息） */
  proxy?: string | null;
  saveError?: string | null;
}

/** 代理连通性测试设置（GET/POST /api/proxies/settings，A2A 对齐） */
export interface ProxySettings {
  testUrl: string;
  defaults?: { testUrl?: string };
}

/** 系统代理快照（GET /api/proxies/system；`proxy: null` = 未配置） */
export interface SystemProxySnapshot {
  proxy: {
    source: 'custom';
    protocol: 'http' | 'socks5';
    host: string;
    port: number;
    username?: string;
    password?: string;
  } | null;
}

/** AI 助手工具条目（GET /api/agent/settings） */
export interface AgentToolItem {
  name: string;
  description: string;
  risk: string;
}

/** AI 助手设置（模型 / 提示词 / 工具开关 / 轮数） */
export interface AgentSettingsItem {
  enabled: boolean;
  model: string;
  channel: string;
  max_turns: number;
  approval_timeout_secs: number;
  max_history: number;
  reasoning_effort?: string | null;
  systemPrompt: string;
  disabledTools: string[];
  tools: AgentToolItem[];
}

/**
 * 聊天功能设置（GET/PUT /api/chat/settings，#10/#11）。
 * 字段对齐后端 `src/config.rs` ChatConfig（camel 出、snake 入由 api 层封装）。
 * 保存即热生效（title.rs 每次生成前重读配置，无需重启）。
 */
export interface ChatSettingsItem {
  /** 会话 LLM 自动标题开关（默认关，与 agent.enabled 同口径） */
  autoTitle: boolean;
  /** 标题生成模型（空 = 自动发现启用+已定价模型） */
  titleModel: string;
  /** 标题生成锁定渠道（空 = 走全局调度） */
  titleChannel: string;
  /** 首条用户消息发给模型的最大字符数（默认 500，超出截断；后端不 clamp，前端兜底 ≥1，0 会被 400） */
  titlePromptChars: number;
}

/** 网关设置（监听地址 / CORS / 反代信任） */
export interface GatewaySettingsItem {
  host: string;
  port: number;
  corsOrigins: string[];
  trustProxyHeaders: boolean;
  restartRequired?: boolean;
}

/** 安全策略（GET/PUT /api/security/policy；对照 Agent2API「设置 → 安全」） */
export interface SecurityPolicyItem {
  /** 是否启用「连续失败锁定」 */
  lockoutEnabled: boolean;
  /** 连续失败达该次数即锁定来源 IP */
  failThreshold: number;
  /** 锁定时长（分钟）；改后需重启服务生效 */
  lockMinutes: number;
  /** 同一 IP 每分钟允许的登录尝试次数 */
  rateLimitPerMinute: number;
  /** 是否要求登录/注册前完成机器人校验（ALTCHA / Turnstile） */
  captchaEnabled: boolean;
  /** 机器人校验提供方：altcha（默认，自托管）/ turnstile（Cloudflare） */
  captchaProvider?: 'altcha' | 'turnstile';
  /** Turnstile site key（公开） */
  turnstileSiteKey?: string;
  /** Turnstile secret 是否已配置（明文不回显） */
  turnstileSecretSet?: boolean;
  /** 保存后需重启才生效的字段名（当前为 lockMinutes） */
  restartRequiredFields?: string[];
}

/** 调试模式 · 一条上游原始报文（GET /api/debug/traffic；请求侧 + 错误路径响应侧） */
export interface DebugTrafficEntry {
  /** 关联键：请求日志 id（详情 `?id=` 查询用；旧记录可能缺省） */
  id?: string;
  /** 记录时刻（毫秒 Unix 时间戳） */
  ts: number;
  /** 关联的请求 id（与请求日志同值） */
  request_id: string;
  /** 实际请求的上游模型真名（别名映射之后） */
  model: string;
  /** 实际承载的账号 id（账号型渠道才有） */
  account_id?: string;
  /** HTTP 方法 */
  method: string;
  /** 上游 URL（**已脱敏**） */
  url: string;
  /** 发给上游的请求头（**已脱敏**） */
  headers: Record<string, string>;
  /** 发给上游的请求体（超长已截断、凭据字段已脱敏） */
  body?: string;
  /** 请求体是否被截断 */
  truncated?: boolean;
  /** 上游响应状态码（未收到响应时缺省） */
  response_status?: number;
  /** 上游响应头（**已脱敏**） */
  response_headers?: Record<string, string>;
  /** 上游响应体（仅错误路径抓取，**已脱敏**） */
  response_body?: string;
}

/**
 * 调试模式 · 单条详情（GET /api/debug/traffic?id=；字段对齐 Agent2API
 * `detail_payload`，camelCase）。
 */
export interface DebugTrafficDetail {
  id: string;
  requestId: string;
  ts: number;
  method: string;
  url: string;
  model: string;
  accountId?: string | null;
  requestHeaders: Record<string, unknown>;
  requestBody?: string | null;
  status?: number | null;
  responseHeaders?: Record<string, unknown> | null;
  responseBody?: string | null;
  truncated: boolean;
}

/** 调试模式缓冲概览（GET /api/debug/traffic） */
export interface DebugTrafficView {
  /** 开关是否开启（关闭时 buffer 为空） */
  enabled: boolean;
  /** 环形缓冲上限条数 */
  maxEntries: number;
  /** 单条请求体截断上限（字节） */
  maxBodyBytes: number;
  /** 累计被挤掉的条数 */
  dropped: number;
  /** 最近抓到的报文（最新在前） */
  entries: DebugTrafficEntry[];
}

/** models.dev 模型元数据状态（GET/PUT /api/models/metadata；对照 Agent2API 同名端点） */
export interface ModelMetadataStatus {
  /** 补齐开关（默认开；关闭 = 不刷新也不参与补值） */
  enabled: boolean;
  /** 目录生成/刷新时刻（毫秒时间戳；内嵌快照带原值） */
  updatedAt: number | null;
  /** 已装载条目数 */
  count: number;
  /** 来源标识（embedded / cache / https://models.dev） */
  source: string;
}

/** 手动刷新结果（POST /api/models/metadata/refresh；恒 2xx，状态看 status） */
export interface ModelMetadataRefreshResult {
  status: 'refreshed' | 'disabled' | 'skipped' | 'failed';
  message?: string | null;
  count: number;
  metadata?: ModelMetadataStatus;
}

/** 路由策略（GET/PUT /api/routing；对照 Agent2API「设置 → 路由」） */
export type RouteStrategy = 'weighted' | 'round_robin' | 'ranked';
export type StickyMode = 'auto' | 'session' | 'turn' | 'off';

export interface BreakerPolicyItem {
  enabled: boolean;
  threshold: number;
  cooldown_secs: number;
  success_threshold: number;
}

export interface SchedulerWeightsItem {
  health_weight: number;
  cost_weight: number;
  rpm_weight: number;
}

export interface RoutingPolicyItem {
  strategy: RouteStrategy;
  sticky: StickyMode;
  breaker: BreakerPolicyItem;
  scheduler_weights: SchedulerWeightsItem;
  runtime?: {
    affinity_entries?: number;
    breaker_open?: number;
    breaker_halfopen?: number;
  };
}

/** 路由队列预演条目（GET /api/routing/queue；对照 Agent2API「设置 → 路由 → 候选队列」） */
export interface RoutingQueueEntryItem {
  id: string;
  name: string;
  provider: string;
  /** 断路器状态：closed / open / halfopen */
  state: string;
  /** 仅 blocked 条目携带：被挡原因（断路器打开 / 半开试探 / 冷却中或未声明该模型） */
  reason?: string;
}

/** 路由队列预演结果（GET /api/routing/queue?model=xxx；只读预演，不推进轮询游标） */
export interface RoutingQueueItem {
  strategy: RouteStrategy;
  sticky: StickyMode;
  model: string;
  /** 静态排序后的首选渠道 id（round_robin 下实际首挑由游标决定，见界面提示） */
  pickedId: string | null;
  ordered: RoutingQueueEntryItem[];
  blocked: RoutingQueueEntryItem[];
}

/** Cline 伪装头（GET/PUT /api/cline-headers；对照 Agent2API 的 cline_headers 模块） */
export interface ClineHeadersItem {
  /** 代码内置默认值（用于区分「改回了默认」与「本来就没改」） */
  defaults: Record<string, string>;
  /** 用户覆盖表原样回显（空串 = 该头不发送） */
  overrides: Record<string, string>;
  /** 默认值 + 覆盖表合并后的生效值 */
  effective: Record<string, string>;
}

/** 系统提示词注入（GET/PUT /api/gateway/prompt；对照 Agent2API 网关设置） */
export type PromptMode = 'passthrough' | 'custom' | 'append';

export interface GatewayPromptItem {
  mode: PromptMode;
  text: string;
  /** 按模型覆盖（键 = 模型名；命中则整条替代全局 mode/text） */
  models?: Record<string, { mode: PromptMode; text: string }>;
  /** 按提供商覆盖（键 = 账号 provider 名或渠道类型名；优先级：模型 > 提供商 > 全局） */
  providers?: Record<string, { mode: PromptMode; text: string }>;
  developer_role_compat?: boolean;
  /** 网关自带提示词逐家开关（键 = provider id；缺省 = 装） */
  gateway?: Record<string, boolean>;
  /** 网关自带提示词三段正文覆盖（缺段 = 官方原文） */
  gateway_text?: Record<string, { identity?: string; stable?: string; dynamic?: string }>;
}

/** 指纹脱敏命中统计（随 GET /api/sanitize 下发；进程内计数，重启归零） */
export interface SanitizeStats {
  /** 进入净化的请求数 */
  hits: number;
  /** 实际应用的改写次数 */
  rewrites: number;
}

/** 额外脱敏请求头名单（GET/PUT /api/sanitize） */
export interface SanitizeHeadersItem {
  extra_headers: string[];
  /** 只读统计：仅 GET 返回，PUT 回显时可缺省 */
  stats?: SanitizeStats;
}

/** 上游风控配置（`GET /api/zcode/captcha/config` 的 `captcha`） */
export interface ZcodeCaptchaConfig {
  enabled?: boolean;
  sceneId?: string;
  prefix?: string;
  /** 阿里云站点（`cn` / `sgp` …），无痕铸造要它选对站点 */
  region?: string;
}

/**
 * ZCode 验证码令牌池概况（GET /api/zcode/captcha）。
 *
 * 同时是**铸造器每轮的输入**：`needsTokens`/`target`/`pollMs`/`maxPerRound`
 * 三项决定了这一轮要不要铸、铸几个、隔多久再问（见 `lib/zcodeCaptchaPool.ts`）。
 */
export interface ZcodeCaptchaPoolStats {
  /** 可用库存（不含过期令牌） */
  ready: number;
  /** 令牌寿命（毫秒，网关侧 `TOKEN_TTL_MS`） */
  ttlMs: number;
  /** 最旧那枚令牌已经放了几毫秒（`null` = 池子空） */
  oldestAgeMs?: number | null;
  minted: number;
  consumed: number;
  /** 上游回 3007 的次数（变大 = 池里的令牌不可信了，立刻补货） */
  rejected: number;
  /** 因过期被丢弃的枚数 */
  stale: number;
  lastChallengeAt?: number | null;
  lastMintAt?: number | null;
  /** 库存目标（够下一条请求立刻有得用即可） */
  target: number;
  /** 轮询间隔（毫秒） */
  pollMs: number;
  /** 单轮铸造上限 */
  maxPerRound: number;
  /** 是否存在 ZCode 账号（没有账号在用这条通道时一个都不铸） */
  hasAccounts: boolean;
  /** 网关算好的「这一轮该不该铸」 */
  needsTokens: boolean;
  /** 拿去取上游风控配置的账号 id（空串 = 没有 zcode 账号） */
  captchaAccountId: string;
}

/** 令牌入池结果（POST /api/zcode/captcha） */
export interface ZcodeCaptchaPushResult {
  pushed: number;
  ready: number;
}

/** 铸造节奏（GET/PUT /api/zcode/captcha-pool） */
export interface ZcodeCaptchaPoolSettings {
  target: number;
  pollMs: number;
  maxPerRound: number;
  /** 三项各自的合法区间 `[min, max]`（界面据此渲染输入框的 min/max） */
  ranges: {
    target: [number, number];
    pollMs: [number, number];
    maxPerRound: [number, number];
  };
  ttlMs: number;
  ready: number;
}

/** 存储概览（GET /api/storage） */
export interface StorageOverviewItem {
  total_keys: number;
  groups: Record<string, number>;
  db_size_bytes: number | null;
  data_dir: string;
}

/** 备份导入结果（POST /api/backup/import） */
export interface BackupImportResult {
  written: number;
  skipped: number;
  pre_import_backup: string;
}

/** 服务器自动更新状态（GET /api/update/status） */
export interface UpdateStatusItem {
  currentVersion: string;
  latestVersion: string | null;
  hasUpdate: boolean | null;
  comparison: 'newer' | 'older' | 'same' | null;
  releaseName?: string | null;
  notes?: string | null;
  publishedAt?: string | null;
  pageUrl?: string | null;
  prerelease?: boolean;
  bundle?: { name: string; size: number; url: string } | null;
  checksumAvailable?: boolean;
  checkedAt: number;
  lastError?: string | null;
  retryAt: number;
  checking: boolean;
  applying: boolean;
  pendingRestart: boolean;
  download?: {
    active: boolean;
    done: boolean;
    received: number;
    total: number;
    percent: number;
    error?: string;
  } | null;
  supported: boolean;
  writable: boolean;
  backupAvailable: boolean;
  downloadDir: string;
  repository: string;
  apiBase: string;
  appliedVersion?: string | null;
  appliedAt?: number;
  /** 本进程启动时间（unix 秒）：exec 自重启不换 PID，前端靠它的变化判定新进程已起 */
  startedAt: number;
  /** 重启请求计数（每次点重启 +1，跨进程持久化） */
  restartEpoch: number;
  /** 最近一次 exec 自重启的失败原因（成功重启后被新进程清空） */
  restartError?: string | null;
  /** static/version.json 里的前端版本（旧构建可能没有） */
  staticVersion?: string | null;
  /** static 版本与后端二进制版本不一致（半更新） */
  versionMismatch?: boolean;
  /** 跑在容器里（原地更新/重启不可靠，应拉新镜像） */
  containerized?: boolean;
  /** 刚检查过（处于最短检查间隔冷却内） */
  recentlyChecked?: boolean;
  /** 冷却剩余秒数 */
  retryAfterSecs?: number;
}

/** 自动更新配置（GET/PUT /api/update/settings，token 为脱敏值） */
export interface UpdateSettingsItem {
  enabled: boolean;
  /** 旧契约镜像字段：检查计划的权威来源是定时任务 `updateCheck` */
  auto_check: boolean;
  auto_apply: boolean;
  /** 旧契约镜像字段（小时）：真正的间隔见定时任务 `updateCheck.interval`（分钟） */
  interval_hours: number;
  repo: string;
  api_base: string;
  include_prerelease: boolean;
  proxy: string;
  token: string;
  tokenConfigured: boolean;
  restart_after_apply: boolean;
  /** GitHub 加速源模式：auto（官方优先、失败回退）/ official / accelerated */
  github_mode: 'auto' | 'official' | 'accelerated';
  /** accelerated 模式下首选的加速源（必须在 github_mirrors 里启用） */
  github_mirror_url: string;
  /** 加速源列表（前缀式；最多 6 个） */
  github_mirrors: { url: string; enabled: boolean; supports_api: boolean }[];
  supported: boolean;
  currentVersion: string;
}

/**
 * 间隔型定时任务的一条任务项（GET /api/scheduled-tasks）。
 * 对应后端 `scheduled_tasks::task_json`。
 *
 * ─── 两类任务（区别是「谁来执行」）───
 *   - `runner: "backend"`：凭证维护 / 模型刷新 / 版本检查，后端调度循环执行，
 *     有 lastRunAt / nextRunAt 等运行状态，界面可「立即执行」。其中
 *     `updateCheck` 的配置入口在「更新」页（检查计划），定时任务页不渲染它。
 *   - `runner: "frontend"`：日志页 / 请求日志页 / 报表页自动刷新，定时器长在
 *     页面上，后端只存开关与间隔，界面自己读；`canRun` 为 false。
 */
export interface ScheduledTaskItem {
  id: string;
  label: string;
  description: string;
  /** 间隔单位：`minutes`（后端任务）或 `seconds`（前端刷新任务） */
  unit: 'minutes' | 'seconds';
  runner: 'backend' | 'frontend';
  enabled: boolean;
  interval: number;
  min: number;
  max: number;
  defaultInterval: number;
  /** 仅 backend 任务有运行状态语义；frontend 恒为 false */
  running: boolean;
  /** 最近一次执行完成时间（unix 毫秒，null 表示从未成功执行） */
  lastRunAt: number | null;
  /** 最近一次尝试开始时间（unix 毫秒） */
  lastAttemptAt: number | null;
  /** 最近一次成功执行时间（unix 毫秒） */
  lastSuccessAt: number | null;
  /** 最近一次执行的结论文案（人类可读） */
  lastResult: string | null;
  /** 最近一次失败原因 */
  lastError: string | null;
  /** 失败冷却的到期时间（unix 毫秒，null/过期表示无冷却） */
  retryAt: number | null;
  /** 下次计划执行时间（unix 毫秒；frontend 任务恒为 null） */
  nextRunAt: number | null;
  /** 是否可在后端立即执行（仅 backend 任务为 true） */
  canRun: boolean;
  /**
   * beta.7：社区插件检查（pluginCatalog）的排期来源。
   * `follow` = 跟随「软件检查更新」计划，`custom` = 自定开关/间隔；其它任务缺省。
   */
  checkMode?: 'follow' | 'custom';
  /** beta.7：社区插件检查是否自动升级已启用且已签名的插件。 */
  autoUpdate?: boolean;
  /** 改配置时后端是否已写盘（PATCH 响应携带；GET 列表没有该字段） */
  saved?: boolean;
}

/** 间隔型任务清单（GET /api/scheduled-tasks 响应） */
export interface ScheduledTaskList {
  tasks: ScheduledTaskItem[];
}

/** 改一条任务的请求体（PATCH/POST /api/scheduled-tasks/{id}，只传要改的字段） */
export interface ScheduledTaskPatch {
  enabled?: boolean;
  interval?: number;
  /**
   * beta.7：仅 `pluginCatalog` 接受。checkMode 只支持 `follow`
   * （跟随软件更新计划；follow 时后端跳过并忽略 interval）。
   */
  checkMode?: 'follow' | 'custom';
  /** beta.7：仅 `pluginCatalog` 接受；落点为软件更新的自动应用开关。 */
  autoUpdate?: boolean;
}

/** 立即执行结果（POST /api/scheduled-tasks/{id}/run 响应） */
export interface ScheduledTaskRunResult {
  summary: string;
  task: ScheduledTaskItem;
}

// ── 诊断（GET /api/diagnostics/* 三件套） ──

/** 系统体检摘要（GET /api/diagnostics/summary 响应 data） */
export interface DiagnosticsSummary {
  /** 聚合时刻（unix 秒） */
  generated_at: number;
  channels: {
    total: number;
    enabled: number;
    circuit_open: number;
    circuit_halfopen: number;
    cooldown: number;
  };
  today_usage: {
    requests: number;
    success: number;
    success_rate: number;
    input_tokens: number;
    output_tokens: number;
  };
  errors_24h: {
    total_failures: number;
    top: Array<{ status_code: number; count: number; error_msg?: string }>;
  };
  runtime: {
    version: string;
    pid: number;
    uptime_secs: number | null;
    uptime_available: boolean;
  };
}

/** 渠道批量测活条目（GET /api/diagnostics/channels 响应 data.channels[]） */
export interface DiagnosticsChannelProbe {
  channel_id: string;
  name: string;
  enabled: boolean;
  /** alive / timeout / dead */
  result: string;
  latency_ms: number;
  message: string;
}

/** 渠道批量测活（GET /api/diagnostics/channels 响应 data） */
export interface DiagnosticsChannelProbes {
  probed: number;
  alive: number;
  deadline_secs: number;
  channels: DiagnosticsChannelProbe[];
}

/** 熔断器快照条目（GET /api/diagnostics/breakers 响应 data.breakers[]） */
export interface DiagnosticsBreaker {
  channel_id: string;
  /** closed / open / halfopen */
  state: string;
  failure_count: number;
  failure_type: string;
  cooldown_remaining_secs: number;
  rate_limit_remaining_secs: number;
  probe_in_flight: boolean;
  in_store_cooldown: boolean;
}

/** 熔断器快照（GET /api/diagnostics/breakers 响应 data） */
export interface DiagnosticsBreakers {
  tracked: number;
  open: number;
  halfopen: number;
  breakers: DiagnosticsBreaker[];
}

// ==================== 插件管理（/api/plugins，后端契约对齐 Agent2API） ====================
// AIGX 后端插件管理面尚在建设（be-impl 任务 #3）；前端先按上游 api/plugins.rs 的
// 响应形状定义契约并渲染，端点 404 时整页降级为「后端未就绪」空态，就绪后零改动生效。

/** 插件包内一家提供商的启用/加载态 */
export interface PluginPackProvider {
  id: string;
  label: string;
  enabled: boolean;
  /** 包进程已把该家适配加载进内存（未加载 = 启用但进程没起/加载失败） */
  loaded: boolean;
  /** 已由其他插件接管（对齐 A2A PackProvider.overriddenBy；未下发 = null） */
  overriddenBy?: string | null;
  /** provider 级开关能力：global = 有独立开关；none = 随包启停（原生插件包）。
   *  旧后端不带该字段 → 前端按 global 兜底（避免升级顺序问题）。 */
  toggle?: 'global' | 'none';
}

/** 插件宿主进程状态（一包 = 一进程；后端未做进程化时恒 null） */
export interface PluginHostState {
  running: boolean;
  providers: string[];
  crashes: number;
  disabled: boolean;
  lastError?: string | null;
}

/** 已安装插件包摘要 */
export interface PluginPackSummary {
  id: string;
  name: string;
  description?: string | null;
  version: string;
  /** 网关要求的最小版本（低于它 = 需要补齐插件才加载） */
  requiredVersion?: string | null;
  updateRequired?: boolean;
  compatibilityError?: string | null;
  enabled: boolean;
  signed: boolean;
  installedAt: number;
  /** 包的可执行文件是否在磁盘上（被手工删过 = false） */
  entryPresent: boolean;
  protocol: number;
  homepage?: string | null;
  icon?: string | null;
  providers: PluginPackProvider[];
  host?: PluginHostState | null;
  /** 供应商形态（agent = 登录态型 / web = 网页形态 / wasm = Wasm 沙箱包） */
  providerCategory?: 'agent' | 'web' | 'wasm';
}

/** 商店（发布目录）里的一个插件发布条目 */
export interface StorePlugin {
  id: string;
  name: string;
  description: string;
  official: boolean;
  version: string | null;
  page: string | null;
  notes?: string | null;
  publishedAt?: string | null;
  prerelease?: boolean;
  error?: boolean;
  reason?: string | null;
  platforms: Array<{ target: string; size: number; url: string; current: boolean; sha256?: string | null }>;
  homepage?: string | null;
  icon?: string | null;
  /** 清单声明的 provider（动态索引才有；缺失插件横幅据此把隐藏账号映射到包） */
  providers?: Array<{ id: string; label?: string }>;
  /** 上游 index.json 分类元数据（原样透传；Wasm 徽标据此渲染） */
  category?: string | null;
  /** 条目要求签名校验（Wasm / 发布包） */
  requireSigned?: boolean;
  /** 条目标签（含 `wasm` 时明确标注跑在 Wasm 沙箱） */
  tags?: string[];
}

/** 发布目录刷新态 */
export interface PluginReleaseState {
  available: boolean;
  current: string;
  plugins: StorePlugin[];
  reason: string | null;
  error: boolean;
  checked?: boolean;
  checking?: boolean;
  checkedAt?: number;
}

/** 单包更新检查结果 */
export interface PluginUpdate {
  id: string;
  available: boolean;
  checked: boolean;
  checkedAt?: number;
  current: string;
  required?: boolean;
  requiredVersion?: string | null;
  repairing?: boolean;
  retryAt?: number;
  latest: string | null;
  notes?: string | null;
  page?: string | null;
  publishedAt?: string | null;
  prerelease?: boolean;
  /** 当前平台资产（sha256 供本地安装前的显式校验） */
  asset: { target: string; size: number; url: string; current: boolean; sha256?: string | null } | null;
  reason: string | null;
  error: boolean;
}

/** 在途的安装/更新操作（下载/校验/加载分阶段） */
export interface PluginOperation {
  id: string;
  action: string;
  stage: string;
  active: boolean;
  done: boolean;
  received: number;
  total: number;
  percent: number | null;
  version: string;
  required?: boolean;
  requiredVersion?: string | null;
  error?: string | null;
  hostError?: string | null;
  updatedAt: number;
}

/** 被管理面隐藏的账号所属 provider（对齐 A2A `hiddenAccounts.providers`） */
export interface PluginHiddenAccountProvider {
  id: string;
  label: string;
  count: number;
  /** `not_installed` / `pack_disabled` / `provider_disabled` / `not_loaded` */
  reason: string;
  packId?: string | null;
}

/** 隐藏账号摘要（GET /api/plugins 的 `hiddenAccounts`） */
export interface PluginHiddenAccounts {
  total: number;
  byProvider: Record<string, number>;
  providers: PluginHiddenAccountProvider[];
}

/** GET /api/plugins 响应 data */
export interface PluginsPayload {
  packs: PluginPackSummary[];
  host: PluginHostState | null;
  adminPanels?: string[];
  accountFeatures?: Record<string, unknown>;
  target: string;
  protocol: number;
  pluginsDir: string;
  catalog: PluginReleaseState;
  updates: PluginUpdate[];
  operations: PluginOperation[];
  /** 上传上限（本地安装用；缺省 24 MiB） */
  maxPackBytes?: number;
  /** provider → 账号数（批量条目的账号计数） */
  accountCounts?: Record<string, number>;
  /** 隐藏账号摘要（缺失插件横幅的数据源） */
  hiddenAccounts?: PluginHiddenAccounts;
}

// ── 脚本插件（Magpie；/api/plugins/scripts*）─────────────────────────
// 契约锚点：src/api/admin/script_plugins.rs（手写视图）与
// src/plugin_host/script/catalog/types.rs（PublicEntry serde camelCase）。

/** 脚本包声明的一家 provider（OpenCode v1 auth.provider 的投影） */
export interface ScriptProviderView {
  id: string;
  label: string;
  upstream?: string | null;
  models: string[];
  /** 包内该家是否启用（后端按 disabled_providers 反推）。
   *  当前后端未下发（plugin-abi 并行补）；缺失时前端按 true 兜底。 */
  enabled?: boolean;
}

/** 已安装脚本包（GET /api/plugins/scripts 的 packages[]） */
export interface ScriptPackageView {
  id: string;
  label: string;
  version: string;
  /** `magpie` / `npm` / `github` / `local` */
  source: string;
  /** `official` / `recommended` / `third-party` */
  publisher: string;
  providers: ScriptProviderView[];
  entry?: string | null;
  sourceRef: string;
  repository?: string | null;
  summary?: string | null;
  /** unix 秒 */
  installedAt: number;
  enabled: boolean;
  /** 运维显式信任（脚本不是沙箱，trusted=true 才允许启用） */
  trusted: boolean;
  options: { values: Record<string, unknown> };
  /** 任一 provider 被脚本宿主实际接管（宿主未跑/未装载 = false） */
  providerRouted: boolean;
  /** 包图标（后端若提供：data URI / 缓存图标相对路径 / 外链；缺省回落首字母） */
  icon?: string | null;
  /** 宿主回填的包级设置声明（providerOptions 开关表单数据源） */
  settingsSchema?: ScriptSettingsSchema | null;
  /**
   * 宿主回填的加载错误（`script.packages.error`）：非空表示脚本包已安装但宿主未能
   * 装载/接管（如入口缺失、运行时异常）；`null`/缺省 = 加载成功。用于向运维解释
   * 「装了但未接管」的原因。
   */
  error?: string | null;
}

/** 脚本包的单个 provider 选项声明（`settingsSchema.providerOptions` 项） */
export interface ScriptProviderOption {
  key: string;
  /** 多语标签（`labels` 优先，缺省 `label` 单语） */
  label?: string;
  labels?: Record<string, string>;
  description?: string;
  descriptions?: Record<string, string>;
  /** 开启前的二次确认文案 */
  confirm?: string;
  confirms?: Record<string, string>;
  default?: boolean;
}

/** 宿主回填的脚本包设置声明（字段只读透传） */
export interface ScriptSettingsSchema {
  version?: number;
  /** provider id → 该家可开关的选项 */
  providerOptions?: Record<string, ScriptProviderOption[]>;
  [key: string]: unknown;
}

/** GET / POST `/api/plugins/scripts/settings` 响应 data */
export interface ScriptSettingsView {
  id: string;
  /** 包级高级参数（options） */
  options: Record<string, unknown>;
  /** provider id → { optionKey: 是否开启 } */
  providerOptions: Record<string, Record<string, boolean>>;
  /** 设置声明（宿主回填；可能为空） */
  schema: ScriptSettingsSchema | null;
}

/** 脚本宿主状态（GET /api/plugins/scripts 的 host） */
export interface ScriptHostView {
  running: boolean;
  packages: number;
  providers: string[];
  lastError?: string | null;
  crashes: number;
}

/** GET /api/plugins/scripts 响应 data */
export interface ScriptPackagesPayload {
  host: ScriptHostView;
  packages: ScriptPackageView[];
}

/** Magpie 目录条目（后端 PublicEntry camelCase 投影，字段只读透传） */
export interface ScriptCatalogEntryView {
  name: string;
  label: string;
  version: string | null;
  providers: string[];
  kind: string | null;
  /** `official` / `recommended` / `third-party` */
  publisher: string;
  magpieOfficial: boolean;
  magpieRecommended: boolean;
  /** `magpie` / `npm` / `github` */
  source: string;
  /** 安装指示串（安装时原样回传 /api/plugins/scripts/install） */
  selector: string;
  summary: string | null;
  repository: string | null;
  repositoryUrl: string | null;
  stars: number | null;
  starsAt: number | null;
  iconSources: string[];
  installable: boolean;
  unavailableReason: string | null;
  pushedAt: number | null;
  manifestAt: number | null;
  providerVerified: boolean | null;
}

/** Magpie 目录状态（后端 Status；serde snake_case——字段名与后端 JSON 原样一致） */
export interface ScriptCatalogStatus {
  market_at: number;
  github_at: number;
  stars_at: number;
  market_error: string | null;
  github_error: string | null;
  github_missing: boolean;
  market_stale: boolean;
  github_stale: boolean;
  stars_stale: boolean;
  official: number;
  recommended: number;
  third_party: number;
  ignored: number;
}

/** GET /api/plugins/scripts/catalog 响应 data */
export interface ScriptCatalogPayload {
  status: ScriptCatalogStatus;
  entries: ScriptCatalogEntryView[];
}

/**
 * 聊天会话摘要（GET /api/chat/sessions 列表项 / fork 响应的 session）。
 * 后端锚点：src/chat_session/mod.rs:70-83（serde camelCase）。
 * parent_session_id / forked_from_seq（#37 父子追溯，父删子悬空容忍）；
 * title_source 三态："user"（手工改名，永不被自动标题覆盖）/"auto"（首条消息
 * 截 30 字）/缺省（旧数据）。 */
export interface ChatSessionSummary {
  id: string;
  title: string;
  model: string;
  created_at?: number;
  updated_at?: number;
  parent_session_id?: string | null;
  forked_from_seq?: number | null;
  title_source?: string | null;
}

/** 破甲控制台 · 单条改写规则（GET/PUT /api/purge） */
export interface PurgeRewriteRule {
  from: string;
  to: string;
}

/** 破甲控制台 · 结构化提示模板（active_template 命中时替代 structured_suffix） */
export interface PurgeTemplate {
  name: string;
  text: string;
}

/** 破甲控制台配置 */
export interface PurgeSettings {
  enabled: boolean;
  system_normalize: boolean;
  dead_zone_rewrite: boolean;
  synonym_enabled: boolean;
  reduce_reasoning: boolean;
  cross_source_fallback: boolean;
  skill_inject: boolean;
  structured_prompt: boolean;
  reasoning_floor: string;
  retry_limit: number;
  fallback_trigger_attempt: number;
  fallback_models: string[];
  rewrite_rules: PurgeRewriteRule[];
  synonym_pairs: PurgeRewriteRule[];
  normalize_strip_prefixes: string[];
  skill_dir: string;
  skill_inject_limit: number;
  structured_suffix: string;
  /** 结构化模板表（可为空；旧后端不返回时按空表处理） */
  structured_templates?: PurgeTemplate[];
  /** 当前启用的模板名（空 = 用 structured_suffix） */
  active_template?: string;
  /** 拒绝话术词表（空 = 关闭字面拒绝检测） */
  refusal_patterns?: string[];
  /** 「内容过滤」类失败关键词 */
  failure_filter_keywords?: string[];
  /** 「空答」类失败关键词 */
  failure_empty_keywords?: string[];
  /** 按模型自动套用预设（默认打开；旧后端不返回时按关闭处理） */
  auto_preset?: boolean;
  /** 固定套用的预设 id（空 = 按模型自动匹配） */
  auto_preset_id?: string;
  upstream_label: string;
}

/** 预览命中明细（后端 `AppliedDetail`，snake_case；旧后端不返回 → undefined） */
export interface PurgeAppliedDetail {
  normalized?: number;
  dead_zone_rules?: number;
  dead_zone_replacements?: number;
  synonym_rules?: number;
  synonym_replacements?: number;
  structured_appended?: boolean;
  skill_names?: string[];
  skill_truncated?: boolean;
  /** 每个命中技能触发匹配的关键词 `[技能名, 关键词]`（与 skill_names 同序） */
  skill_keywords?: Array<[string, string]>;
  /** 因 skill_inject_limit 长度上限未被注入的命中技能个数 */
  skill_dropped?: number;
}

/** POST /api/purge/preview 的 data 结构（dry-run 试跑结果） */
export interface PurgePreviewResult {
  applied: { changed: boolean; skill_hit: boolean };
  detail?: PurgeAppliedDetail;
  messages: Array<{ role: string; content: string | null }>;
}

/** 破甲控制台统计（进程内计数，重启归零） */
export interface PurgeStats {
  responses: number;
  retries: number;
  early_success: number;
  skill_hits: number;
  refusals: number;
  failure_refusal: number;
  failure_empty: number;
  failure_filter: number;
  failure_other: number;
  synonym_hits: number;
  /** 按模型自动套用、且真的改写了 system 提示词的次数（旧后端不返回） */
  auto_preset_hits?: number;
}

/** 时间窗统计（事件环派生；`truncated` = 环溢出，窗口值为下界估计） */
export interface PurgeWindowedStats extends PurgeStats {
  window_ms: number;
  truncated?: boolean;
}

/** 破甲控制台运行信息 */
export interface PurgeRuntime {
  pid: number;
  uptime_secs: number;
  upstream: string;
  /** 当前在途请求数（进程内即时值，非累计；旧版后端可能不返回） */
  inflight?: number;
}

/** GET /api/purge 附带的技能目录扫描快照（不含正文） */
export interface PurgeSkillBrief {
  name: string;
  keywords: string[];
  body_chars: number;
}

/** 配置体检告警（结构化形态：code 为稳定机器码，前端据此跳修复入口） */
export interface PurgeConfigWarning {
  code: string;
  message: string;
}

/** GET /api/purge 的 data 结构 */
export interface PurgeData {
  settings: PurgeSettings;
  stats: PurgeStats;
  runtime: PurgeRuntime;
  /** 配置体检：「开了但不生效 / 自相矛盾」的静态检查结果（旧版后端不返回） */
  config_warnings?: string[];
  /** 技能目录扫描快照（旧版后端不返回） */
  skills?: PurgeSkillBrief[];
  /** 时间窗统计（近 1h / 24h；旧版后端不返回时统计卡只有全程口径） */
  stats_1h?: PurgeWindowedStats;
  stats_24h?: PurgeWindowedStats;
  /** 配置体检告警（结构化：带 code；旧后端只有 config_warnings 字符串） */
  config_warning_items?: PurgeConfigWarning[];
}

/** 破甲预设库 · 列表条目（GET /api/purge/presets，不含正文，只带预览） */
export interface PurgePresetBrief {
  id: string;
  vendor: string;
  model: string;
  title: string;
  tags: string[];
  /** 正文字节数（UTF-8） */
  bytes: number;
  /** 正文 sha256（小写十六进制） */
  sha256: string;
  /** 正文前 200 字符预览 */
  preview: string;
}

/** 破甲预设库 · 厂商计数（过滤后统计，跟随搜索词变化） */
export interface PurgePresetVendorCount {
  vendor: string;
  count: number;
}

/** GET /api/purge/presets 的 data 结构 */
export interface PurgePresetList {
  items: PurgePresetBrief[];
  vendors: PurgePresetVendorCount[];
  /** 过滤后总数（items 已被 limit 截断） */
  total: number;
}

/** 破甲预设库 · 详情（GET /api/purge/presets/{id}，含完整正文） */
export interface PurgePresetDetail {
  id: string;
  vendor: string;
  model: string;
  title: string;
  tags: string[];
  bytes: number;
  sha256: string;
  body: string;
}

/** 预设应用落点：结构化模板（落库热生效）/ 技能目录（只写盘） */
export type PurgePresetApplyTarget = 'structured_template' | 'skill_file';

/** POST /api/purge/presets/{id}/apply 的 data 结构 */
export interface PurgePresetApplyResult {
  applied: {
    target: string;
    name: string;
    /** 写盘路径（仅 skill_file 有） */
    path: string | null;
  };
  settings: PurgeSettings;
}

/** POST /api/purge/skills/import 的 data 结构（技能包导入结果） */
export interface PurgeSkillImportResult {
  /** 实际落盘目录名（单级，位于技能目录下） */
  dir: string;
  files: number;
  bytes: number;
  has_skill_md: boolean;
}

/** GET /api/public/branding 的 data 结构（免鉴权） */
export interface BrandingData {
  panel_name: string;
  logo_url: string;
  favicon_url: string;
  title: string;
  /** 是否已上传 Logo（上传后 logo_url 指向公开 Logo 端点） */
  logo_uploaded?: boolean;
}

/** GET /api/branding 的 data 结构（管理员） */
export interface BrandingAdminData {
  current: BrandingData;
  defaults: {
    panel_name: string;
    logo_url: string;
    favicon_url: string;
  };
}

// ==================== A2A v3.5.0-beta.1 对齐新增类型（见 ./a2a.ts） ====================
// 拆到独立文件以保持本文件可读；这里统一再导出，调用方仍从 '@/types' / '../types' 取。
export type {
  PluginSettingsField,
  PluginProviderSettingsRow,
  PluginProviderSettingsPayload,
  AccountSortRule,
  AccountSortSettings,
  HiddenAccountProviderItem,
  HiddenAccountsSummary,
  AccountFileExportResult,
  CheckinCampaignReward,
  CheckinMonthlyGift,
  CheckinCampaignPreview,
  CheckinCampaignClaimResult,
  ToolProtocol,
  ToolServiceItem,
  ToolVariantItem,
  ProviderKeyStatus,
  ProviderKeyRow,
  ProviderKeyUpdate,
  ToolProviderRow,
  ToolProviderSelection,
  ToolProviderTestResult,
  ToolVisionConfig,
  ToolVisionKeyOption,
  ToolVisionPayload,
  ToolInstallTarget,
  ToolInstallOutcome,
  ToolApiKeyOption,
  ToolInstallInfo,
  MaintenanceTaskId,
  MaintenanceRunStatus,
  MaintenanceTaskView,
  MaintenanceItemView,
  MaintenanceRunView,
  MaintenanceDetailView,
  MaintenanceOverviewView,
  MaintenanceQueryParams,
} from './a2a';

// 维护任务清单是运行期常量（非类型），单独值导出。
export { MAINTENANCE_TASKS } from './a2a';
