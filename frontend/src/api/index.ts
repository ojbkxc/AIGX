/**
 * AIGX API客户端
 * 统一的API调用层，为前端应用提供类型安全的数据访问接口
 */

import type {
  ApiResponse,
  ApiList,
  User,
  Channel,
  ChannelUsage,
  ApiKey,
  RealtimeMetrics,
  LoginRequest,
  RegisterRequest,
  AuthResponse,
  LoginResult,
  LoginCodeResult,
  RegisterResult,
  MessageResult,
  ChannelItem,
  TokenItem,
  TokenKeyResult,
  PriceEntry,
  GroupItem,
  OrderItem,
  RedemptionItem,
  AlertRule,
  AlertEvent,
  NotifyConfigItem,
  IpFilterEntry,
  CacheStatsItem,
  SystemMonitorItem,
  PricingSyncConfigItem,
  ExchangeRatesItem,
  IpFilterConfigItem,
  RateLimitConfigItem,
  SettingsItem,
  ModelInfo,
  TotpSetupResult,
  TotpEnableResult,
  TotpDisableResult,
  PlaygroundChatRequest,
  PlaygroundChatResult,
  UsageSummaryItem,
  TrendItem,
  DashboardItem,
} from '@/types';

const API_BASE = '/api';

function getToken(): string | null {
  return localStorage.getItem('token');
}

function authHeaders(): Record<string, string> {
  const token = getToken();
  if (!token) return {};
  return { Authorization: `Bearer ${token}` };
}

/** 把分页/筛选参数安全转成查询串（数字自动字符串化） */
function buildQuery(params: Record<string, string | number>): string {
  const entries = Object.entries(params)
    .filter(([, v]) => v !== undefined && v !== null && v !== '')
    .map(([k, v]) => [k, String(v)] as [string, string]);
  return new URLSearchParams(entries).toString();
}

async function request<T = unknown>(method: string, path: string, body: unknown = null): Promise<T> {
  const headers: Record<string, string> = {
    'Content-Type': 'application/json',
    ...authHeaders(),
  };
  const options: RequestInit = { method, headers };
  if (body !== null) {
    options.body = JSON.stringify(body);
  }
  let res: Response;
  try {
    res = await fetch(path, options);
  } catch (networkErr) {
    // 请求层 L1：网络中断/DNS 失败 → 统一可读错误（不上抛 TypeError 到页面组件）
    if (networkErr instanceof DOMException && networkErr.name === 'AbortError') {
      throw networkErr;
    }
    throw new Error('网络连接失败，请检查网络后重试');
  }
  // 仅真实登录态失效才清除会话并跳转登录页；避免「已登录但访问
  // 无权限页面」时被误踢（后端对越权访问返回 403，而非 401）。
  if (res.status === 401) {
    const token = getToken();
    if (token && !window.location.pathname.startsWith('/login')) {
      try {
        localStorage.removeItem('token');
        localStorage.removeItem('email');
        localStorage.removeItem('username');
        localStorage.removeItem('role');
        localStorage.removeItem('expires_at');
      } catch {
        // ignore
      }
      if (window.location.pathname !== '/login') {
        window.location.href = '/login';
      }
    }
    throw new Error('Unauthorized');
  }
  if (res.status === 204) {
    return null as T;
  }
  const text = await res.text();
  let data: unknown = null;
  if (text) {
    const contentType = res.headers.get('Content-Type') || '';
    if (contentType.includes('application/json')) {
      try {
        data = JSON.parse(text) as unknown;
      } catch {
        if (!res.ok) throw new Error(text || `Request failed with status ${res.status}`);
        data = text;
      }
    } else if (res.ok) {
      data = text;
    }
  }
  if (!res.ok) {
    const msg: unknown =
      (data && typeof data === 'object' && (((data as Record<string, unknown>).error as string) || ((data as Record<string, unknown>).message as string))) ||
      (typeof text === 'string' && text) ||
      `Request failed with status ${res.status}`;
    throw new Error(typeof msg === 'string' ? msg : String(msg));
  }
  return data as T;
}

class ApiError extends Error {
  constructor(
    public status: number,
    public code: string,
    message: string,
    public details?: Record<string, unknown>
  ) {
    super(message);
    this.name = 'ApiError';
  }
}

class UnauthorizedError extends ApiError {
  constructor(message: string = '未授权的访问') {
    super(401, 'unauthorized', message);
  }
}

class ForbiddenError extends ApiError {
  constructor(message: string = '您没有执行此操作的权限') {
    super(403, 'forbidden', message);
  }
}

class NotFoundError extends ApiError {
  constructor(message: string = '请求的资源不存在') {
    super(404, 'not_found', message);
  }
}

class ValidationError extends ApiError {
  constructor(message: string = '请求参数验证失败') {
    super(422, 'validation_error', message);
  }
}

class ServerError extends ApiError {
  constructor(message: string = '服务器内部错误') {
    super(500, 'server_error', message);
  }
}

function handleApiError(response: Response): never {
  switch (response.status) {
    case 401:
      throw new UnauthorizedError();
    case 403:
      throw new ForbiddenError();
    case 404:
      throw new NotFoundError();
    case 422:
      throw new ValidationError();
    default:
      throw new ServerError();
  }
}

export const api = {
  // ==================== 顶层便捷方法（页面直调，沿用后端路由契约） ====================
  login: (email: string, password: string): Promise<ApiResponse<LoginResult>> =>
    request<ApiResponse<LoginResult>>('POST', `${API_BASE}/auth/login`, { email, password }),
  loginSendCode: (email: string): Promise<ApiResponse<LoginCodeResult>> =>
    request<ApiResponse<LoginCodeResult>>('POST', `${API_BASE}/auth/login/send-code`, { email }),
  loginWithCode: (email: string, code: string): Promise<ApiResponse<LoginResult>> =>
    request<ApiResponse<LoginResult>>('POST', `${API_BASE}/auth/login/code`, { email, code }),
  loginTotp: (tmp_token: string, code: string): Promise<ApiResponse<LoginResult>> =>
    request<ApiResponse<LoginResult>>('POST', `${API_BASE}/auth/login/totp`, { tmp_token, code }),
  // 兼容历史调用顺序 (email, password, username?)：后端仅使用 email/password/username 字段
  register: (emailOrUsername: string, password: string, username?: string): Promise<ApiResponse<RegisterResult>> =>
    request<ApiResponse<RegisterResult>>('POST', `${API_BASE}/auth/register`, {
      email: emailOrUsername,
      password,
      username: username ?? undefined,
    }),
  forgotPassword: (email: string): Promise<ApiResponse<MessageResult>> =>
    request<ApiResponse<MessageResult>>('POST', `${API_BASE}/auth/forgot-password`, { email }),
  changePassword: (old_password: string, new_password: string): Promise<ApiResponse<MessageResult>> =>
    request<ApiResponse<MessageResult>>('POST', `${API_BASE}/auth/change-password`, { old_password, new_password }),
  getUsageSummary: (): Promise<ApiResponse<UsageSummaryItem>> =>
    request<ApiResponse<UsageSummaryItem>>('GET', `${API_BASE}/usage/summary`),
  getTodayTokens: (): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('GET', `${API_BASE}/tokens/today`),
  getLimits: (): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('GET', `${API_BASE}/limits`),
  getTrend: (): Promise<ApiResponse<TrendItem[]>> =>
    request<ApiResponse<TrendItem[]>>('GET', `${API_BASE}/usage/trend`),
  getConsumptionTrend: (): Promise<ApiResponse<TrendItem[]>> =>
    request<ApiResponse<TrendItem[]>>('GET', `${API_BASE}/dashboard/consumption_trend`),
  getModelDistribution: (): Promise<ApiResponse<DashboardItem[]>> =>
    request<ApiResponse<DashboardItem[]>>('GET', `${API_BASE}/dashboard/model_distribution`),
  getUserRanking: (): Promise<ApiResponse<DashboardItem[]>> =>
    request<ApiResponse<DashboardItem[]>>('GET', `${API_BASE}/dashboard/user_ranking`),
  getChannelHealth: (): Promise<ApiResponse<DashboardItem[]>> =>
    request<ApiResponse<DashboardItem[]>>('GET', `${API_BASE}/dashboard/channel_health`),
  getRealtime: (): Promise<ApiResponse<DashboardItem>> =>
    request<ApiResponse<DashboardItem>>('GET', `${API_BASE}/dashboard/realtime`),
  getCacheSavings: (): Promise<ApiResponse<DashboardItem>> =>
    request<ApiResponse<DashboardItem>>('GET', `${API_BASE}/dashboard/cache_savings`),
  saveEpayConfig: (config: Record<string, unknown>): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('PUT', `${API_BASE}/epay/config`, config),
  listGroups: (): Promise<ApiList<GroupItem>> =>
    request<ApiList<GroupItem>>('GET', `${API_BASE}/groups`),
  getIpLists: (): Promise<ApiResponse<IpFilterEntry[]>> =>
    request<ApiResponse<IpFilterEntry[]>>('GET', `${API_BASE}/ip/filter`),
  addIpWhitelist: (ip: string): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('POST', `${API_BASE}/ip/whitelist`, { pattern: ip }),
  addIpBlacklist: (ip: string): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('POST', `${API_BASE}/ip/blacklist`, { pattern: ip }),
  removeIpWhitelist: (pattern: string): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('DELETE', `${API_BASE}/ip/whitelist/${encodeURIComponent(pattern)}`),
  removeIpBlacklist: (pattern: string): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('DELETE', `${API_BASE}/ip/blacklist/${encodeURIComponent(pattern)}`),
  listKeys: (): Promise<ApiList<TokenItem>> =>
    request<ApiList<TokenItem>>('GET', `${API_BASE}/keys`),
  getRequestLogs: (): Promise<ApiResponse<DashboardItem[]>> =>
    request<ApiResponse<DashboardItem[]>>('GET', `${API_BASE}/logs/requests`),
  getModelMappings: (): Promise<ApiResponse<SettingsItem>> =>
    request<ApiResponse<SettingsItem>>('GET', `${API_BASE}/settings`),
  saveModelMapping: (mapping: unknown): Promise<ApiResponse<SettingsItem>> =>
    request<ApiResponse<SettingsItem>>('PUT', `${API_BASE}/settings`, { mappings: mapping, replace_all: false }),
  deleteModelMapping: (_id: string): Promise<ApiResponse<SettingsItem>> =>
    request<ApiResponse<SettingsItem>>('PUT', `${API_BASE}/settings`, { mappings: {}, replace_all: false }),
  listOrders: (): Promise<ApiList<OrderItem>> =>
    request<ApiList<OrderItem>>('GET', `${API_BASE}/orders`),
  chatCompletions: (payload: Record<string, unknown>): Promise<ApiResponse<DashboardItem>> =>
    request<ApiResponse<DashboardItem>>('POST', '/v1/chat/completions', payload),
  listPrices: (): Promise<ApiList<PriceEntry>> =>
    request<ApiList<PriceEntry>>('GET', `${API_BASE}/prices`),
  listRedemptions: (params: Record<string, string | number> = {}): Promise<ApiList<RedemptionItem>> =>
    request<ApiList<RedemptionItem>>('GET', `${API_BASE}/redemptions?${buildQuery(params)}`),
  getSecurityIncidents: (): Promise<ApiList<AlertEvent>> =>
    request<ApiList<AlertEvent>>('GET', `${API_BASE}/monitor/security/events`),
  getSecurityAlerts: (): Promise<ApiList<AlertEvent>> =>
    request<ApiList<AlertEvent>>('GET', `${API_BASE}/alerts/active`),
  saveSettings: (usage: unknown, limits: unknown, notification: unknown): Promise<ApiResponse<SettingsItem>> =>
    request<ApiResponse<SettingsItem>>('PUT', `${API_BASE}/settings`, { usage, limits, notification }),
  getBalance: (): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('GET', `${API_BASE}/users/me`),
  getTransactions: (): Promise<ApiList<OrderItem>> =>
    request<ApiList<OrderItem>>('GET', `${API_BASE}/orders/me`),

  // ==================== 迁移自 api.js 的顶层方法（页面直调契约） ====================

  // Auth
  logout: (): Promise<ApiResponse<MessageResult>> =>
    request<ApiResponse<MessageResult>>('POST', `${API_BASE}/auth/logout`),

  // Models（网关可用模型列表，用于对话调试模型下拉）
  // 通用网关语义：模型来自渠道声明的 models 聚合。登录用户均可访问
  // /api/models/available（对齐 new-api /api/user/models）；不用 /v1/models
  //（数据面需 sk-xxx，会 401 误踢登录），也不用 mappings keys。
  // P1：后端已附带元信息（owned_by/context_length/capabilities），兼容旧
  // 字符串数组与新对象数组两种形状。
  listModels: async (): Promise<ApiResponse<ModelInfo[]>> => {
    const res = await request<ApiResponse<unknown[]>>('GET', `${API_BASE}/models/available`);
    const list: unknown[] = Array.isArray(res?.data) ? res.data : [];
    return {
      success: true,
      data: list.map((item) =>
        typeof item === 'string'
          ? { id: item, object: 'model', owned_by: 'aigx' }
          : item as ModelInfo,
      ),
    };
  },

  // Settings / Model Mappings
  getSettings: (): Promise<ApiResponse<SettingsItem>> =>
    request<ApiResponse<SettingsItem>>('GET', `${API_BASE}/settings`),
  updateSettings: (mappings: unknown, replace_all = false): Promise<ApiResponse<SettingsItem>> =>
    request<ApiResponse<SettingsItem>>('PUT', `${API_BASE}/settings`, { mappings, replace_all }),

  // Limits
  updateLimits: (data: Record<string, unknown>): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('PUT', `${API_BASE}/limits`, data),

  // Users
  listUsers: (): Promise<ApiList<User>> =>
    request<ApiList<User>>('GET', `${API_BASE}/users`),
  createUser: (data: Record<string, unknown>): Promise<ApiResponse<User>> =>
    request<ApiResponse<User>>('POST', `${API_BASE}/users`, data),
  updateUser: (id: string | number, data: Record<string, unknown>): Promise<ApiResponse<User>> =>
    request<ApiResponse<User>>('PUT', `${API_BASE}/users/${id}`, data),
  deleteUser: (id: string | number): Promise<ApiResponse<MessageResult>> =>
    request<ApiResponse<MessageResult>>('DELETE', `${API_BASE}/users/${id}`),
  getMe: (): Promise<ApiResponse<User>> =>
    request<ApiResponse<User>>('GET', `${API_BASE}/users/me`),
  checkUsername: (username: string): Promise<ApiResponse<{ available?: boolean }>> =>
    request<ApiResponse<{ available?: boolean }>>('GET', `${API_BASE}/users/check?username=${encodeURIComponent(username)}`),

  // Epay
  getEpayConfig: (): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('GET', `${API_BASE}/epay/config`),
  // 用户侧充值页信息（对齐 new-api /api/user/topup/info）
  getEpayInfo: (): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('GET', `${API_BASE}/epay/info`),
  updateEpayConfig: (data: Record<string, unknown>): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('PUT', `${API_BASE}/epay/config`, data),

  // Orders & Topup
  myOrders: (): Promise<ApiList<OrderItem>> =>
    request<ApiList<OrderItem>>('GET', `${API_BASE}/orders/me`),
  topup: (amount: number, payment_method: string): Promise<ApiResponse<OrderItem>> =>
    request<ApiResponse<OrderItem>>('POST', `${API_BASE}/topup`, { amount, payment_method }),

  // 通用渠道管理
  listChannels: (): Promise<ApiList<ChannelItem>> =>
    request<ApiList<ChannelItem>>('GET', `${API_BASE}/channels`),
  addChannel: (data: Record<string, unknown>): Promise<ApiResponse<ChannelItem>> =>
    request<ApiResponse<ChannelItem>>('POST', `${API_BASE}/channels`, data),
  updateChannel: (id: string | number, data: Record<string, unknown>): Promise<ApiResponse<ChannelItem>> =>
    request<ApiResponse<ChannelItem>>('PUT', `${API_BASE}/channels/${id}`, data),
  patchChannel: (id: string | number, data: Record<string, unknown>): Promise<ApiResponse<ChannelItem>> =>
    request<ApiResponse<ChannelItem>>('PATCH', `${API_BASE}/channels/${id}`, data),
  deleteChannel: (id: string | number): Promise<ApiResponse<MessageResult>> =>
    request<ApiResponse<MessageResult>>('DELETE', `${API_BASE}/channels/${id}`),
  testChannel: (id: string | number): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('POST', `${API_BASE}/channels/${id}/test`),
  resetChannelCircuit: (id: string | number): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('POST', `${API_BASE}/channels/${id}/reset-circuit`),
  fetchChannelModels: (data: Record<string, unknown>): Promise<ApiResponse<{ models?: string[] }>> =>
    request<ApiResponse<{ models?: string[] }>>('POST', `${API_BASE}/channels/fetch_models`, data),
  // 渠道对话调试：流式（text/event-stream）返回 { stream: [{ content }] }，
  // 非流式返回后端 JSON。与 Playground 页共用 SSE 解析模式。
  testChannelChat: async (data: PlaygroundChatRequest): Promise<PlaygroundChatResult> => {
    const headers: Record<string, string> = {
      'Content-Type': 'application/json',
      ...authHeaders(),
    };
    const res = await fetch(`${API_BASE}/channels/chat_test`, {
      method: 'POST',
      headers,
      body: JSON.stringify(data),
    });
    if (res.status === 401) {
      const token = getToken();
      if (token && !window.location.pathname.startsWith('/login')) {
        try {
          localStorage.removeItem('token');
          localStorage.removeItem('email');
          localStorage.removeItem('username');
          localStorage.removeItem('expires_at');
        } catch {
          // ignore
        }
        if (window.location.pathname !== '/login') {
          window.location.href = '/login';
        }
      }
      throw new Error('Unauthorized');
    }
    const contentType = res.headers.get('Content-Type') || '';
    if (contentType.includes('text/event-stream')) {
      // 流式：把 SSE 逐条解析为 { content } 增量数组
      const text = await res.text();
      const chunks: { content: string }[] = [];
      let buf = '';
      const pushBuf = () => {
        // 单帧可能含多条 data: 行（ANSI 续行协议），把每条 data: 行独立解析
        const lines = buf
          .split('\n')
          .map((l) => l.trim())
          .filter((l) => l.startsWith('data:') && l.length > 5)
          .map((l) => l.slice(5).trim())
          .filter((l) => l.length > 0);
        for (const data of lines) {
          if (data === '[DONE]') continue;
          try {
            const parsed: Record<string, unknown> = JSON.parse(data) as Record<string, unknown>;
            const choices = parsed.choices as Array<{ delta?: { content?: string; text?: string; reasoning_content?: string; reasoning?: string } }> | undefined;
            const firstDelta = choices && choices[0] && choices[0].delta;
            const delta = parsed.delta as { text?: string } | undefined;
            const contentBlocks = parsed.content as Array<{ text?: string }> | undefined;
            const content =
              (firstDelta &&
                (firstDelta.content || firstDelta.text || firstDelta.reasoning_content || firstDelta.reasoning)) ||
              (delta && delta.text) ||
              (contentBlocks && contentBlocks[0] && contentBlocks[0].text) ||
              '';
            if (content) chunks.push({ content });
          } catch {
            // 忽略非 JSON 帧
          }
        }
        buf = '';
      };
      for (const line of text.split('\n')) {
        if (line.trim() === '') {
          pushBuf();
        } else {
          buf += line + '\n';
        }
      }
      pushBuf();
      return { stream: chunks };
    }
    // 非流式：解析 JSON，错误时抛出后端错误信息
    const text = await res.text();
    let parsed: unknown = null;
    if (text) {
      try {
        parsed = JSON.parse(text);
      } catch {
        // ignore
      }
    }
    if (!res.ok) {
      const errMsg: unknown =
        (parsed && typeof parsed === 'object' && (((parsed as Record<string, unknown>).error as string) || ((parsed as Record<string, unknown>).message as string))) ||
        text ||
        `Request failed with status ${res.status}`;
      throw new Error(typeof errMsg === 'string' ? errMsg : String(errMsg));
    }
    return parsed as PlaygroundChatResult;
  },

  // 令牌管理
  listTokens: (): Promise<ApiList<TokenItem>> =>
    request<ApiList<TokenItem>>('GET', `${API_BASE}/tokens`),
  addToken: (data: Record<string, unknown>): Promise<ApiResponse<TokenItem>> =>
    request<ApiResponse<TokenItem>>('POST', `${API_BASE}/tokens`, data),
  updateToken: (id: string | number, data: Record<string, unknown>): Promise<ApiResponse<TokenItem>> =>
    request<ApiResponse<TokenItem>>('PUT', `${API_BASE}/tokens/${id}`, data),
  deleteToken: (id: string | number): Promise<ApiResponse<MessageResult>> =>
    request<ApiResponse<MessageResult>>('DELETE', `${API_BASE}/tokens/${id}`),
  // 按需取回明文密钥：登录用户可随时查看/复制自己的令牌（对齐 new-api）
  getTokenKey: (id: string | number): Promise<ApiResponse<TokenKeyResult>> =>
    request<ApiResponse<TokenKeyResult>>('GET', `${API_BASE}/tokens/${id}/key`),
  resetTokenUsed: (id: string | number): Promise<ApiResponse<MessageResult>> =>
    request<ApiResponse<MessageResult>>('POST', `${API_BASE}/tokens/${id}/reset_used`),
  rotateToken: (id: string | number): Promise<ApiResponse<TokenKeyResult>> =>
    request<ApiResponse<TokenKeyResult>>('POST', `${API_BASE}/tokens/${id}/rotate`),

  // 模型定价目录
  upsertPrice: (data: Record<string, unknown>): Promise<ApiResponse<PriceEntry>> =>
    request<ApiResponse<PriceEntry>>('POST', `${API_BASE}/prices`, data),
  deletePrice: (model: string): Promise<ApiResponse<MessageResult>> =>
    request<ApiResponse<MessageResult>>('DELETE', `${API_BASE}/prices/${encodeURIComponent(model)}`),

  // 倍率配置
  getRatios: (): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('GET', `${API_BASE}/ratios`),
  updateRatios: (data: Record<string, unknown>): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('PUT', `${API_BASE}/ratios`, data),

  // 用户分组管理
  upsertGroup: (data: Record<string, unknown>): Promise<ApiResponse<GroupItem>> =>
    request<ApiResponse<GroupItem>>('POST', `${API_BASE}/groups`, data),
  deleteGroup: (name: string): Promise<ApiResponse<MessageResult>> =>
    request<ApiResponse<MessageResult>>('DELETE', `${API_BASE}/groups/${encodeURIComponent(name)}`),

  // 日志与审计
  listRequestLogs: (params: Record<string, string | number> = {}): Promise<ApiList<Record<string, unknown>>> =>
    request<ApiList<Record<string, unknown>>>('GET', `${API_BASE}/logs/requests?${buildQuery(params)}`),
  listAuditLogs: (params: Record<string, string | number> = {}): Promise<ApiList<Record<string, unknown>>> =>
    request<ApiList<Record<string, unknown>>>('GET', `${API_BASE}/logs/audits?${buildQuery(params)}`),

  // 兑换码
  batchRedemptions: (data: Record<string, unknown>): Promise<ApiResponse<{ count?: number; data?: RedemptionItem[] }>> =>
    request<ApiResponse<{ count?: number; data?: RedemptionItem[] }>>('POST', `${API_BASE}/redemptions/batch`, data),
  deleteRedemption: (id: string | number): Promise<ApiResponse<MessageResult>> =>
    request<ApiResponse<MessageResult>>('DELETE', `${API_BASE}/redemptions/${id}`),
  redeem: (code: string): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('POST', `${API_BASE}/redemptions/redeem`, { code }),

  // 限流配置
  getRateLimitConfig: (): Promise<ApiResponse<RateLimitConfigItem>> =>
    request<ApiResponse<RateLimitConfigItem>>('GET', `${API_BASE}/ratelimit/config`),
  updateRateLimitConfig: (data: Record<string, unknown>): Promise<ApiResponse<RateLimitConfigItem>> =>
    request<ApiResponse<RateLimitConfigItem>>('PUT', `${API_BASE}/ratelimit/config`, data),

  // 通知系统（Telegram + SMTP + Slack + Webhook）
  getNotifyConfig: (): Promise<ApiResponse<NotifyConfigItem>> =>
    request<ApiResponse<NotifyConfigItem>>('GET', `${API_BASE}/notify/config`),
  updateNotifyConfig: (data: Record<string, unknown>): Promise<ApiResponse<NotifyConfigItem>> =>
    request<ApiResponse<NotifyConfigItem>>('PUT', `${API_BASE}/notify/config`, data),
  testTelegram: (): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('POST', `${API_BASE}/notify/test-telegram`),
  testEmail: (to: string): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('POST', `${API_BASE}/notify/test-email`, { to }),
  testSlack: (): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('POST', `${API_BASE}/notify/test-slack`),
  testWebhook: (message: string): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('POST', `${API_BASE}/notify/test-webhook`, { message }),

  // 告警规则
  getAlertRules: (): Promise<ApiList<AlertRule>> =>
    request<ApiList<AlertRule>>('GET', `${API_BASE}/alerts/rules`),
  updateAlertRules: (rules: AlertRule[]): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('PUT', `${API_BASE}/alerts/rules`, { rules }),
  getActiveAlerts: (): Promise<ApiList<AlertEvent>> =>
    request<ApiList<AlertEvent>>('GET', `${API_BASE}/alerts/active`),
  getAlertHistory: (limit = 100): Promise<ApiList<AlertEvent>> =>
    request<ApiList<AlertEvent>>('GET', `${API_BASE}/alerts/history?limit=${limit}`),
  testAlert: (kind: string, value: unknown): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('POST', `${API_BASE}/alerts/test`, { kind, value }),

  // 系统监控
  getSystemMonitor: (): Promise<ApiResponse<SystemMonitorItem>> =>
    request<ApiResponse<SystemMonitorItem>>('GET', `${API_BASE}/monitor/system`),

  // Playground
  playgroundChat: (data: PlaygroundChatRequest): Promise<ApiResponse<PlaygroundChatResult>> =>
    request<ApiResponse<PlaygroundChatResult>>('POST', `${API_BASE}/playground/chat`, data),

  // 安全监控
  getSecurityOverview: (): Promise<ApiResponse<DashboardItem>> =>
    request<ApiResponse<DashboardItem>>('GET', `${API_BASE}/monitor/security`),
  getSecurityEvents: (params: Record<string, string | number> = {}): Promise<ApiList<AlertEvent>> =>
    request<ApiList<AlertEvent>>('GET', `${API_BASE}/monitor/security/events?${buildQuery(params)}`),

  // IP 管理
  getIpFilter: (): Promise<ApiResponse<IpFilterConfigItem>> =>
    request<ApiResponse<IpFilterConfigItem>>('GET', `${API_BASE}/ip/filter`),
  updateIpFilter: (data: Record<string, unknown>): Promise<ApiResponse<IpFilterConfigItem>> =>
    request<ApiResponse<IpFilterConfigItem>>('PUT', `${API_BASE}/ip/filter`, data),
  addWhitelist: (pattern: string, note: string): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('POST', `${API_BASE}/ip/whitelist`, { pattern, note }),
  removeWhitelist: (pattern: string): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('DELETE', `${API_BASE}/ip/whitelist/${encodeURIComponent(pattern)}`),
  addBlacklist: (pattern: string, note: string): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('POST', `${API_BASE}/ip/blacklist`, { pattern, note }),
  removeBlacklist: (pattern: string): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('DELETE', `${API_BASE}/ip/blacklist/${encodeURIComponent(pattern)}`),

  // 缓存管理
  getCacheStats: (): Promise<ApiResponse<CacheStatsItem>> =>
    request<ApiResponse<CacheStatsItem>>('GET', `${API_BASE}/cache/stats`),
  clearCache: (): Promise<ApiResponse<MessageResult>> =>
    request<ApiResponse<MessageResult>>('POST', `${API_BASE}/cache/clear`),

  // 忘记密码/重置密码
  resetPassword: (token: string, password: string): Promise<ApiResponse<MessageResult>> =>
    request<ApiResponse<MessageResult>>('POST', `${API_BASE}/auth/reset-password`, { token, password }),

  // 2FA/TOTP（P1）：用户侧自助管理
  totpSetup: (): Promise<ApiResponse<TotpSetupResult>> =>
    request<ApiResponse<TotpSetupResult>>('POST', `${API_BASE}/auth/totp/setup`),
  totpEnable: (code: string): Promise<ApiResponse<TotpEnableResult>> =>
    request<ApiResponse<TotpEnableResult>>('POST', `${API_BASE}/auth/totp/enable`, { code }),
  totpDisable: (password: string): Promise<ApiResponse<TotpDisableResult>> =>
    request<ApiResponse<TotpDisableResult>>('POST', `${API_BASE}/auth/totp/disable`, { password }),

  // 价格同步
  getPriceSyncConfig: (): Promise<ApiResponse<PricingSyncConfigItem>> =>
    request<ApiResponse<PricingSyncConfigItem>>('GET', `${API_BASE}/pricing/sync-config`),
  updatePriceSyncConfig: (data: Record<string, unknown>): Promise<ApiResponse<PricingSyncConfigItem>> =>
    request<ApiResponse<PricingSyncConfigItem>>('PUT', `${API_BASE}/pricing/sync-config`, data),
  triggerPriceSync: (): Promise<ApiResponse<MessageResult>> =>
    request<ApiResponse<MessageResult>>('POST', `${API_BASE}/pricing/sync`),

  // 汇率配置
  getExchangeRates: (): Promise<ApiResponse<ExchangeRatesItem>> =>
    request<ApiResponse<ExchangeRatesItem>>('GET', `${API_BASE}/pricing/exchange-rates`),
  updateExchangeRates: (data: Record<string, unknown>): Promise<ApiResponse<ExchangeRatesItem>> =>
    request<ApiResponse<ExchangeRatesItem>>('PUT', `${API_BASE}/pricing/exchange-rates`, data),

  /**
   * 认证相关API
   */
  auth: {
    /**
     * 用户登录
     */
    async login(credentials: LoginRequest): Promise<AuthResponse> {
      const response = await fetch(`${API_BASE}/auth/login`, {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json',
        },
        body: JSON.stringify(credentials),
      });

      if (!response.ok) {
        throw handleApiError(response);
      }

      return response.json();
    },

    /**
     * 用户注册
     */
    async register(data: RegisterRequest): Promise<AuthResponse> {
      const response = await fetch(`${API_BASE}/auth/register`, {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json',
        },
        body: JSON.stringify(data),
      });

      if (!response.ok) {
        throw handleApiError(response);
      }

      return response.json();
    },

    /**
     * 用户登出
     */
    async logout(): Promise<void> {
      const response = await fetch(`${API_BASE}/auth/logout`, {
        method: 'POST',
      });

      if (!response.ok) {
        throw handleApiError(response);
      }
    },

    /**
     * 登出（后端无 /auth/refresh 端点，会话由 expires_at 控制有效期）
     */
  },

  /**
   * 用户相关API
   */
  users: {
    /**
     * 获取当前用户信息
     */
    async getMe(): Promise<User> {
      const response = await fetch(`${API_BASE}/users/me`);

      if (!response.ok) {
        throw handleApiError(response);
      }

      return response.json();
    },

    /**
     * 获取用户列表
     */
    async list(params?: { page?: number; limit?: number }): Promise<ApiResponse<{
      data: User[];
      pagination: {
        total: number;
        page: number;
        limit: number;
        total_pages: number;
      };
    }>> {
      const queryParams = new URLSearchParams();
      if (params?.page) queryParams.append('page', params.page.toString());
      if (params?.limit) queryParams.append('limit', params.limit.toString());

      const response = await fetch(
        `${API_BASE}/users?${queryParams.toString()}`
      );

      if (!response.ok) {
        throw handleApiError(response);
      }

      return response.json();
    },

    /**
     * 获取用户详情
     */
    async get(id: string): Promise<User> {
      const response = await fetch(`${API_BASE}/users/${id}`);

      if (!response.ok) {
        throw handleApiError(response);
      }

      return response.json();
    },
  },

  /**
   * 渠道管理API
   */
  channels: {
    /**
     * 获取渠道列表
     */
    async list(): Promise<ApiResponse<{
      data: Channel[];
      pagination: {
        total: number;
        page: number;
        limit: number;
        total_pages: number;
      };
    }>> {
      const response = await fetch(`${API_BASE}/channels`);

      if (!response.ok) {
        throw handleApiError(response);
      }

      return response.json();
    },

    /**
     * 获取渠道详情
     */
    async get(id: string): Promise<Channel> {
      const response = await fetch(`${API_BASE}/channels/${id}`);

      if (!response.ok) {
        throw handleApiError(response);
      }

      return response.json();
    },

    /**
     * 创建渠道
     */
    async create(channelData: Partial<Channel>): Promise<Channel> {
      const response = await fetch(`${API_BASE}/channels`, {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json',
        },
        body: JSON.stringify(channelData),
      });

      if (!response.ok) {
        throw handleApiError(response);
      }

      return response.json();
    },

    /**
     * 更新渠道信息
     */
    async update(id: string, channelData: Partial<Channel>): Promise<Channel> {
      const response = await fetch(`${API_BASE}/channels/${id}`, {
        method: 'PUT',
        headers: {
          'Content-Type': 'application/json',
        },
        body: JSON.stringify(channelData),
      });

      if (!response.ok) {
        throw handleApiError(response);
      }

      return response.json();
    },

    /**
     * 删除渠道
     */
    async delete(id: string): Promise<void> {
      const response = await fetch(`${API_BASE}/channels/${id}`, {
        method: 'DELETE',
      });

      if (!response.ok) {
        throw handleApiError(response);
      }
    },

    /**
     * 测试渠道连接
     */
    async test(id: string): Promise<{ success: boolean; message: string }> {
      const response = await fetch(`${API_BASE}/channels/${id}/test`, {
        method: 'POST',
      });

      if (!response.ok) {
        throw handleApiError(response);
      }

      return response.json();
    },
  },

  /**
   * API密钥管理API
   */
  keys: {
    /**
     * 获取密钥列表
     */
    async list(userId?: string): Promise<ApiResponse<{
      data: ApiKey[];
      pagination: {
        total: number;
        page: number;
        limit: number;
        total_pages: number;
      };
    }>> {
      const queryParams = new URLSearchParams();
      if (userId) queryParams.append('user_id', userId);

      const response = await fetch(
        `${API_BASE}/keys?${queryParams.toString()}`
      );

      if (!response.ok) {
        throw handleApiError(response);
      }

      return response.json();
    },

    /**
     * 创建API密钥
     */
    async create(keyData: Partial<ApiKey>): Promise<ApiKey> {
      const response = await fetch(`${API_BASE}/keys`, {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json',
        },
        body: JSON.stringify(keyData),
      });

      if (!response.ok) {
        throw handleApiError(response);
      }

      return response.json();
    },

    /**
     * 删除API密钥
     */
    async delete(id: string): Promise<void> {
      const response = await fetch(`${API_BASE}/keys/${id}`, {
        method: 'DELETE',
      });

      if (!response.ok) {
        throw handleApiError(response);
      }
    },

    /**
     * 旋转API密钥
     */
    async rotate(id: string): Promise<ApiKey> {
      const response = await fetch(`${API_BASE}/keys/${id}/rotate`, {
        method: 'POST',
      });

      if (!response.ok) {
        throw handleApiError(response);
      }

      return response.json();
    },
  },

  /**
   * Dashboard相关API
   */
  dashboard: {
    /**
     * 获取实时指标
     */
    async getRealtime(): Promise<RealtimeMetrics> {
      const response = await fetch(`${API_BASE}/dashboard/realtime`);

      if (!response.ok) {
        throw handleApiError(response);
      }

      return response.json();
    },

    /**
     * 获取渠道使用情况
     */
    async getChannelUsage(
      channelId: string,
      params?: { start_date?: string; end_date?: string }
    ): Promise<ApiResponse<{ data: ChannelUsage[] }>> {
      const queryParams = new URLSearchParams();
      if (params?.start_date) queryParams.append('start_date', params.start_date);
      if (params?.end_date) queryParams.append('end_date', params.end_date);

      const response = await fetch(
        `${API_BASE}/dashboard/channels/${channelId}/usage?${queryParams.toString()}`
      );

      if (!response.ok) {
        throw handleApiError(response);
      }

      return response.json();
    },
  },

  // ==================== 网络层 API ====================

  network: {
    async getStatus(): Promise<ApiResponse<Record<string, unknown>>> {
      const response = await fetch(`${API_BASE}/network/status`);
      if (!response.ok) throw handleApiError(response);
      return response.json();
    },

    async getConfig(configId: string): Promise<ApiResponse<Record<string, unknown>>> {
      const response = await fetch(`${API_BASE}/network/config/${configId}`);
      if (!response.ok) throw handleApiError(response);
      return response.json();
    },

    async updateConfig(
      configId: string,
      enabled: boolean,
      strategy?: string
    ): Promise<ApiResponse<Record<string, unknown>>> {
      const config: { enabled: boolean; strategy?: string } = { enabled };
      if (strategy) config.strategy = strategy;

      const response = await fetch(`${API_BASE}/network/config/${configId}`, {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(config),
      });
      if (!response.ok) throw handleApiError(response);
      return response.json();
    },

    async restart(): Promise<ApiResponse<Record<string, unknown>>> {
      const response = await fetch(`${API_BASE}/network/restart`, { method: 'POST' });
      if (!response.ok) throw handleApiError(response);
      return response.json();
    },

    async addAccount(accountId: string): Promise<ApiResponse<Record<string, unknown>>> {
      const response = await fetch(`${API_BASE}/network/accounts/${accountId}`, { method: 'POST' });
      if (!response.ok) throw handleApiError(response);
      return response.json();
    },

    async removeAccount(accountId: string): Promise<ApiResponse<Record<string, unknown>>> {
      const response = await fetch(`${API_BASE}/network/accounts/${accountId}`, { method: 'DELETE' });
      if (!response.ok) throw handleApiError(response);
      return response.json();
    },
  },

};

export default api;
