/**
 * AIGX核心类型定义
 * 统一的TypeScript类型系统，为整个前端应用提供类型基础
 */

// ============================================================================
// 核心领域类型
// ============================================================================

export enum UserRole {
  ADMIN = 'admin',
  MANAGER = 'manager',
  USER = 'user',
  AUDITOR = 'auditor'
}

export enum ChannelType {
  OPENAI = 'openai',
  ANTHROPIC = 'anthropic',
  CLOUDFLARE = 'cloudflare',
  GEMINI = 'gemini',
  CUSTOM = 'custom'
}

export enum ChannelStatus {
  ACTIVE = 'active',
  INACTIVE = 'inactive',
  DEPRECATED = 'deprecated',
  FAILED = 'failed',
  MAINTENANCE = 'maintenance'
}

export enum Permission {
  // 系统管理
  SYSTEM_ADMIN = 'system:admin',
  SYSTEM_MONITOR = 'system:monitor',

  // 用户管理
  USER_VIEW = 'users:view',
  USER_CREATE = 'users:create',
  USER_EDIT = 'users:edit',
  USER_DELETE = 'users:delete',

  // 渠道管理
  CHANNEL_VIEW = 'channels:view',
  CHANNEL_CREATE = 'channels:create',
  CHANNEL_EDIT = 'channels:edit',
  CHANNEL_DELETE = 'channels:delete',
  CHANNEL_TEST = 'channels:test',

  // API密钥管理
  KEY_VIEW = 'keys:view',
  KEY_CREATE = 'keys:create',
  KEY_EDIT = 'keys:edit',
  KEY_DELETE = 'keys:delete',
  KEY_ROTATE = 'key:rotate',

  // 定价与分组
  PRICE_VIEW = 'price:view',
  PRICE_UPDATE = 'price:update',
  GROUP_VIEW = 'group:view',
  GROUP_EDIT = 'group:edit',

  // 财务相关
  WALLET_VIEW = 'wallet:view',
  ORDER_VIEW = 'order:view',
  EPAY_CONFIG = 'epay:config',

  // 日志审计
  LOG_VIEW = 'log:view',
  LOG_EXPORT = 'log:export'
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
  type?: string;
  base_url?: string;
  models?: string[];
  status?: string;
  enabled?: boolean;
  priority?: number;
  weight?: number;
  max_concurrent?: number;
  api_key?: string;
  model_mapping?: Record<string, string>;
  cost_pricing?: Record<string, { input_price?: number; output_price?: number; price_type?: string }>;
  created_at?: string | number;
  updated_at?: string;
  last_used_at?: number | null;
  last_error?: string | null;
  health?: ChannelHealth;
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

export interface RedemptionItem {
  id: string;
  code: string;
  usage_count?: number;
  status?: string;
  created_at?: string;
  expired_at?: string;
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

export interface PlaygroundImagesRequest {
  channel_id?: string;
  model: string;
  prompt: string;
  n?: number;
  size?: string;
  [key: string]: unknown;
}

export interface PlaygroundChatResult {
  stream?: Array<{ content?: string }>;
  data?: { content?: string; error?: string; usage?: unknown };
  error?: string;
  success?: boolean;
}

/** /api/playground/chat 的响应 data（Playground V2） */
export interface PlaygroundChatData {
  content?: string;
  model?: string;
  usage?: unknown;
  error?: string;
}

/** Playground V2 双栏 JSON 展示：原始响应 + 预格式化文本 */
export interface PlaygroundRawResult {
  data?: unknown;
  success?: boolean;
  error?: string;
  message?: string;
}

/**
 * 真·流式回调：SSE 每解析到一个增量（OpenAI delta / Anthropic delta.text）
 * 立即回调一次。isEnd 标记 [DONE] / message_stop 帧。
 * kind 区分内容增量：reasoning 为深度思考（DeepSeek 式折叠面板），
 * content 为正文。error/end 帧沿用 content。
 */
export type ChatStreamDelta = {
  content: string;
  isEnd: boolean;
  kind?: 'content' | 'reasoning';
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
