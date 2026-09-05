// AIGX 前端页面组件类型定义（100年不过时的接口层）

// ==================== 通用类型 ====================

/**
 * 用户角色枚举
 */
export enum UserRole {
  Admin = 'admin',
  User = 'user',
  Guest = 'guest',
}

/**
 * 用户信息接口
 */
export interface User {
  id: number;
  username: string;
  email: string;
  role: UserRole;
  created_at: string;
  monthly_limit?: number;
  monthly_used?: number;
}

/**
 * 渠道信息接口
 */
export interface Channel {
  id: string | number;
  name: string;
  base_url: string;
  api_key: string;
  models: string;
  priority: number;
  status: 'enabled' | 'disabled' | 'error';
  enabled: boolean;
  format?: string;
  retry_count?: number;
}

/**
 * API 密钥信息接口
 */
export interface ApiKey {
  id: string;
  name: string;
  key: string;
  created_at: string;
  last_used?: string;
  total_requests: number;
  success_count: number;
  error_count: number;
  request_count: number;
}

/**
 * 令牌使用统计接口
 */
export interface TokenStats {
  total_tokens: number;
  total_input_tokens: number;
  total_output_tokens: number;
  request_count: number;
  today_tokens: number;
  today_input_tokens: number;
  today_output_tokens: number;
}

/**
 * 限额信息接口
 */
export interface Limits {
  monthly_used: number;
  monthly_limit: number;
}

/**
 * 订单信息接口
 */
export interface Order {
  id: string;
  amount: number;
  method: string;
  status: string;
  created_at: string;
  trade_no?: string;
}

/**
 * 兑换码信息接口
 */
export interface Redemption {
  id: string;
  code: string;
  usage_count: number;
  status: 'active' | 'used' | 'expired';
  created_at: string;
  expired_at: string;
}

// ==================== 页面专用类型 ====================

/**
 * Channels 页面类型
 */
export interface ChannelFormState {
  name: string;
  base_url: string;
  api_key: string;
  models: string;
  priority: number;
  enabled: boolean;
  format?: string;
  retry_count?: number;
}

/**
 * Users 页面状态
 */
export interface UserFormState {
  username: string;
  email: string;
  password: string;
  role: UserRole;
  monthly_limit?: number;
}

/**
 * Keys 页面状态
 */
export interface KeyFormState {
  name: string;
  key: string;
  type: string;
}

/**
 * Epay 页面状态
 */
export interface EpayConfig {
  app_id: string;
  secret: string;
  notify_url: string;
  return_url: string;
}

/**
 * Orders 页面筛选条件
 */
export interface OrderFilter {
  start_date?: string;
  end_date?: string;
  status?: string;
}

/**
 * Settings 优先设置类型
 */
export interface SettingsConfig {
  title: string;
  description: string;
  timezone: string;
  default_locale: string;
  email_verification_enabled: boolean;
}

/**
 * Notify 配置类型
 */
export interface NotifyConfig {
  telegram_api_key: string;
  telegram_chat_id: string;
  smtp_enabled: boolean;
  smtp_host: string;
  smtp_port: number;
  smtp_user: string;
  smtp_password: string;
  smtp_from: string;
}

// ==================== API 响应类型 ====================

/**
 * API 通用响应
 */
export interface ApiResponse<T> {
  success: boolean;
  data?: T;
  message?: string;
  error?: string;
}

/**
 * 分页响应
 */
export interface PaginatedResponse<T> {
  items: T[];
  total: number;
  page: number;
  page_size: number;
  total_pages: number;
}

// ==================== 组件 Props 类型 ====================

/**
 * 登录页面 Props
 */
export interface LoginProps {}

/**
 * 注册页面 Props
 */
export interface RegisterProps {}

/**
 * Dashboard 页面 Props
 */
export interface DashboardProps {}

/**
 * Channels 页面 Props
 */
export interface ChannelsProps {}

/**
 * Users 页面 Props
 */
export interface UsersProps {}

/**
 * Keys 页面 Props
 */
export interface KeysProps {}

/**
 * Epay 页面 Props
 */
export interface EpayProps {}

/**
 * Orders 页面 Props
 */
export interface OrdersProps {}

/**
 * Settings 页面 Props
 */
export interface SettingsProps {}

/**
 * 安全监控页面 Props
 */
export interface SecurityProps {}

/**
 * 兑换码页面 Props
 */
export interface RedemptionsProps {}

/**
 * 游乐场页面 Props
 */
export interface PlaygroundProps {}

/**
 * 日志页面 Props
 */
export interface LogsProps {}

/**
 * 分组管理页面 Props
 */
export interface GroupsProps {}

/**
 * IP 管理页面 Props
 */
export interface IpManagementProps {}

/**
 * 定价页面 Props
 */
export interface PricingProps {}

/**
 * 通知管理页面 Props
 */
export interface NotifyProps {}


// ==================== 网络层专用类型 ====================

/**
 * 账号池状态
 */
export interface AccountPoolStatus {
  total_accounts: number;
  available_accounts: number;
  busy_accounts: number;
  error_accounts: number;
  invalid_accounts: number;
  total_requests: number;
  failed_requests: number;
}

/**
 * 连接池状态
 */
export interface ConnectionPoolStatus {
  total_connections: number;
  active_connections: number;
  idle_connections: number;
  total_connections_created: number;
  total_connections_closed: number;
  successful_requests: number;
  failed_requests: number;
  avg_latency_ms: number;
}

/**
 * 会话池统计
 */
export interface SessionPoolStats {
  total_sessions: number;
  active_sessions: number;
  idle_sessions: number;
  session_ttl_hours: number;
}

/**
 * 网络层状态
 */
export interface NetworkStatus {
  enabled: boolean;
  account_pool: AccountPoolStatus;
  connection_pool: ConnectionPoolStatus;
  session_pool: SessionPoolStats;
  load_balance_strategy: string;
  last_check_at: number;
}

/**
 * 网络层配置
 */
export interface NetworkConfig {
  enabled: boolean;
  strategy: string;
  account_pool_min: number;
  account_pool_max: number;
  connection_pool_max: number;
}

/**
 * 网络层管理页面 Props
 */
export interface NetworkLayerProps {}
