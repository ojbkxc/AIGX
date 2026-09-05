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
  id: string;
  username: string;
  email: string;
  role: UserRole;
  api_keys: string[];
  created_at: string;
  updated_at: string;
}

export interface Channel {
  id: string;
  name: string;
  type: ChannelType;
  status: ChannelStatus;
  config: Record<string, any>;
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
  meta: Record<string, any>;
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

export interface ApiResponse<T = any> {
  success: boolean;
  data?: T;
  error?: ApiError;
  meta?: ResponseMeta;
}

export interface ApiError {
  code: string;
  message: string;
  details?: Record<string, any>;
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

export interface FormErrors<T> {
  [K in keyof T]?: string[];
}

// ============================================================================
// 图表数据类型
// ============================================================================

export interface ChartDataPoint {
  label: string;
  value: number;
  original?: any;
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
