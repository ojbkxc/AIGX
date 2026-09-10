/**
 * AIGX API客户端
 * 统一的API调用层，为前端应用提供类型安全的数据访问接口
 */

import type {
  ApiResponse,
  User,
  ApiList,
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
  PlanItem,
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
  ModelMetaOverride,
  TotpSetupResult,
  TotpEnableResult,
  TotpDisableResult,
  PlaygroundChatRequest,
  PlaygroundChatResult,
  PlaygroundChatData,
  PlaygroundImagesRequest,
  PlaygroundRawResult,
  ChatStreamCallback,
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
  // 兼容历史调用顺序 (email, password, username?)：后端仅使用 email/password/username/aff_code 字段
  register: (emailOrUsername: string, password: string, username?: string, affCode?: string): Promise<ApiResponse<RegisterResult>> =>
    request<ApiResponse<RegisterResult>>('POST', `${API_BASE}/auth/register`, {
      email: emailOrUsername,
      password,
      username: username ?? undefined,
      aff_code: affCode ?? undefined,
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
  listOrders: (params: Record<string, string | number> = {}): Promise<ApiList<OrderItem>> =>
    request<ApiList<OrderItem>>('GET', `${API_BASE}/orders${Object.keys(params).length ? `?${buildQuery(params)}` : ''}`),
  chatCompletions: (payload: Record<string, unknown>): Promise<ApiResponse<DashboardItem>> =>
    request<ApiResponse<DashboardItem>>('POST', '/v1/chat/completions', payload),
  listPrices: (): Promise<ApiList<PriceEntry>> =>
    request<ApiList<PriceEntry>>('GET', `${API_BASE}/prices`),
  listRedemptions: (params: Record<string, string | number> = {}): Promise<ApiList<RedemptionItem>> =>
    request<ApiList<RedemptionItem>>('GET', `${API_BASE}/redemptions?${buildQuery(params)}`),
  // 套餐管理（按量套餐：模板 CRUD + 按套餐发放 API Key）
  listPlans: (): Promise<ApiList<PlanItem>> =>
    request<ApiList<PlanItem>>('GET', `${API_BASE}/plans`),
  upsertPlan: (data: Record<string, unknown>): Promise<ApiResponse<PlanItem>> =>
    request<ApiResponse<PlanItem>>('POST', `${API_BASE}/plans`, data),
  deletePlan: (id: string | number): Promise<ApiResponse<MessageResult>> =>
    request<ApiResponse<MessageResult>>('DELETE', `${API_BASE}/plans/${id}`),
  issuePlanKey: (id: string | number, data: Record<string, unknown>): Promise<ApiResponse<TokenKeyResult>> =>
    request<ApiResponse<TokenKeyResult>>('POST', `${API_BASE}/plans/${id}/issue`, data),
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
  // 模型元信息覆盖（P1 收尾：owned_by/上下文长度/能力管理员覆盖）
  listModelMeta: (): Promise<ApiResponse<Record<string, ModelMetaOverride>>> =>
    request<ApiResponse<Record<string, ModelMetaOverride>>>('GET', `${API_BASE}/models/meta`),
  setModelMeta: (model: string, meta: ModelMetaOverride): Promise<ApiResponse<ModelMetaOverride>> =>
    request<ApiResponse<ModelMetaOverride>>('PUT', `${API_BASE}/models/meta/${encodeURIComponent(model)}`, meta),
  deleteModelMeta: (model: string): Promise<ApiResponse<null>> =>
    request<ApiResponse<null>>('DELETE', `${API_BASE}/models/meta/${encodeURIComponent(model)}`),

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
  /** 用户自助更新资料（对齐 new-api PUT /api/user/self）：username/改密（需原密码） */
  updateSelf: (data: { username?: string; original_password?: string; password?: string }): Promise<ApiResponse<User>> =>
    request<ApiResponse<User>>('PUT', `${API_BASE}/users/self`, data),
  /** 管理端用户操作（对齐 new-api POST /api/user/manage）：enable/disable */
  manageUser: (id: string, action: 'enable' | 'disable'): Promise<ApiResponse<User>> =>
    request<ApiResponse<User>>('POST', `${API_BASE}/users/manage`, { id, action }),
  /** 管理员强制禁用用户 2FA（对齐 new-api DELETE /api/user/:id/2fa） */
  adminDisable2FA: (id: string): Promise<ApiResponse<null>> =>
    request<ApiResponse<null>>('DELETE', `${API_BASE}/users/${encodeURIComponent(id)}/2fa`),
  /** 获取当前用户邀请码（对齐 new-api GET /api/user/aff，data 为码字符串） */
  getAffCode: (): Promise<ApiResponse<string>> =>
    request<ApiResponse<string>>('GET', `${API_BASE}/aff`),
  /** 邀请奖励划转到可用配额（对齐 new-api POST /api/user/aff_transfer） */
  affTransfer: (): Promise<ApiResponse<{ transferred: number }>> =>
    request<ApiResponse<{ transferred: number }>>('POST', `${API_BASE}/aff_transfer`, {}),
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
  /** 管理端手动补单（对齐 new-api AdminCompleteTopUp）：pending → paid 并入账 */
  completeOrder: (tradeNo: string): Promise<ApiResponse<OrderItem>> =>
    request<ApiResponse<OrderItem>>('POST', `${API_BASE}/orders/${encodeURIComponent(tradeNo)}/complete`),
  /** 充值试算（对齐 new-api RequestAmount）：amount → pay_money/quota */
  topupAmount: (amount: number): Promise<ApiResponse<{ amount: number; pay_money: number; quota: number }>> =>
    request<ApiResponse<{ amount: number; pay_money: number; quota: number }>>('POST', `${API_BASE}/topup/amount`, { amount }),

  // 通用渠道管理（page/page_size 缺省 = 后端全量返回，兼容旧行为）
  listChannels: (params: Record<string, string | number> = {}): Promise<ApiList<ChannelItem>> =>
    request<ApiList<ChannelItem>>('GET', `${API_BASE}/channels${Object.keys(params).length ? `?${buildQuery(params)}` : ''}`),
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
  getChannelBalance: (id: string | number): Promise<ApiResponse<{ balance?: number; credit?: number }>> =>
    request<ApiResponse<{ balance?: number; credit?: number }>>('GET', `${API_BASE}/channels/${id}/balance`),
  // 渠道对话调试：流式（text/event-stream）返回 { stream: [{ content }] }，
  // 非流式返回后端 JSON。与 Playground 页共用 SSE 解析模式。
  testChannelChat: async (data: PlaygroundChatRequest): Promise<PlaygroundChatResult> => {
    // 非流式走原路径；流式转发给 testChannelChatStream（真·增量渲染）
    if (data.stream) {
      let acc = '';
      await testChannelChatStream(data, (d) => {
        if (!d.isEnd && d.kind !== 'reasoning') acc += d.content;
      });
      return acc ? { stream: [{ content: acc }] } : { stream: [] };
    }
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
  playgroundChat: (data: PlaygroundChatRequest): Promise<ApiResponse<PlaygroundChatData>> =>
    request<ApiResponse<PlaygroundChatData>>('POST', `${API_BASE}/playground/chat`, data),
  playgroundImages: (data: PlaygroundImagesRequest): Promise<ApiResponse<PlaygroundRawResult>> =>
    request<ApiResponse<PlaygroundRawResult>>('POST', `${API_BASE}/playground/images`, data),
  /** Playground TTS：文本转语音，返回 base64 音频 */
  playgroundTts: (data: { model: string; input: string; voice?: string }): Promise<ApiResponse<{ audio_base64: string; content_type: string }>> =>
    request<ApiResponse<{ audio_base64: string; content_type: string }>>('POST', `${API_BASE}/playground/tts`, data),
  /** Playground 语音转文字：multipart 上传音频 blob，返回 { text } */
  playgroundTranscribe: async (blob: Blob, model: string): Promise<{ text?: string }> => {
    const form = new FormData();
    form.append('file', blob, 'audio.webm');
    form.append('model', model);
    const res = await fetch(`${API_BASE}/playground/transcriptions`, {
      method: 'POST',
      headers: authHeaders(),
      body: form,
    });
    if (res.status === 401) {
      throw new Error('登录已过期，请重新登录');
    }
    const j = (await res.json()) as { success?: boolean; data?: { text?: string }; message?: string; error?: string };
    if (!res.ok || !j.success) {
      throw new Error(j.message || j.error || `转写失败 (HTTP ${res.status})`);
    }
    return j.data ?? {};
  },

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
};

/**
 * 解析单个 SSE 帧（OpenAI / Anthropic 协议）为增量回调序列。
 * 纯函数，便于单元测试。返回是否有终止帧（[DONE] / message_stop / error）。
 */
export function parseSseFrame(
  frame: string,
  onDelta: ChatStreamCallback,
): boolean {
  let ended = false;
  const lines = frame
    .split('\n')
    .map((l) => l.trim())
    .filter((l) => l.startsWith('data:') && l.length > 5)
    .map((l) => l.slice(5).trim())
    .filter((l) => l.length > 0);
  for (const data of lines) {
    if (data === '[DONE]') {
      if (!ended) onDelta({ content: '', isEnd: true });
      ended = true;
      continue;
    }
    try {
      const parsed = JSON.parse(data) as Record<string, unknown>;
      // Anthropic message_stop
      if (parsed.type === 'message_stop') {
        if (!ended) onDelta({ content: '', isEnd: true });
        ended = true;
        continue;
      }
      if (parsed.type === 'error' || parsed.error) {
        const raw = parsed.error as Record<string, unknown> | string | undefined;
        const msg = typeof raw === 'string' ? raw : (raw?.message as string | undefined) ?? '上游流式错误';
        onDelta({ content: msg, isEnd: true });
        ended = true;
        continue;
      }
      // Anthropic delta
      const anthropicDelta = parsed.delta as { text?: string } | undefined;
      if (anthropicDelta && typeof anthropicDelta.text === 'string' && anthropicDelta.text) {
        onDelta({ content: anthropicDelta.text, isEnd: false });
        continue;
      }
      // OpenAI delta（content / reasoning_content / text）
      const choices = parsed.choices as Array<{ delta?: Record<string, unknown> }> | undefined;
      const firstDelta = choices?.[0]?.delta;
      if (firstDelta) {
        const reasoning = [firstDelta.reasoning_content, firstDelta.reasoning]
          .find((v) => typeof v === 'string') as string | undefined;
        if (reasoning) onDelta({ content: reasoning, isEnd: false, kind: 'reasoning' });
        const content = ([firstDelta.content, firstDelta.text]
          .find((v) => typeof v === 'string') ?? '') as string;
        if (content) onDelta({ content, isEnd: false, kind: 'content' });
      }
    } catch {
      // 非 JSON 帧（心跳/注释）忽略
    }
  }
  return ended;
}

/**
 * 真·流式对话调试：逐块读取 text/event-stream 并增量解析。
 *
 * 与 testChannelChat 的区别：不等整个响应结束，每解析到一个增量
 * （OpenAI choices[0].delta 或 Anthropic delta.text）立即回调，
 * ChatDebugger 用它在界面上逐 token 渲染。
 */
export async function testChannelChatStream(
  data: PlaygroundChatRequest,
  onDelta: ChatStreamCallback,
  signal?: AbortSignal,
): Promise<void> {
  const res = await fetch(`${API_BASE}/channels/chat_test`, {
    method: 'POST',
    headers: {
      'Content-Type': 'application/json',
      ...authHeaders(),
    },
    body: JSON.stringify(data),
    signal,
  });

  if (res.status === 401) {
    // 与 request() 对齐：会话失效时清理本地登录态，避免 SSE 通道被 401 后
    // 页面停留在「已登录但令牌已死」的状态。
    try {
      localStorage.removeItem('token');
      localStorage.removeItem('email');
      localStorage.removeItem('username');
      localStorage.removeItem('role');
      localStorage.removeItem('expires_at');
    } catch {
      // ignore
    }
    throw new Error('Unauthorized');
  }

  if (!res.ok || !res.body) {
    const text = await res.text().catch(() => '');
    let msg = text;
    if (text) {
      try {
        const parsed = JSON.parse(text) as Record<string, unknown>;
        const raw = parsed.error ?? parsed.message;
        if (typeof raw === 'string') msg = raw;
      } catch {
        // 非 JSON 错误体，直接抛原文
      }
    }
    throw new Error(msg || `Request failed with status ${res.status}`);
  }

  const contentType = res.headers.get('Content-Type') || '';
  if (!contentType.includes('text/event-stream')) {
    // 非流式 JSON（例如上游降级或后端报错），解析后一次性回调
    const text = await res.text();
    let parsed: unknown = null;
    if (text) {
      try {
        parsed = JSON.parse(text);
      } catch {
        // ignore
      }
    }
    const p = (parsed ?? {}) as Record<string, unknown>;
    const data = (p.data ?? {}) as Record<string, unknown>;
    const content = typeof data.content === 'string' ? data.content : '';
    const error = typeof data.error === 'string' ? data.error : typeof p.error === 'string' ? p.error : '';
    if (error) {
      onDelta({ content: error, isEnd: true });
    } else if (content) {
      onDelta({ content, isEnd: true });
    }
    return;
  }

  const reader = res.body.getReader();
  const decoder = new TextDecoder('utf-8');
  let buffer = '';
  let ended = false;

  const handleFrame = (frame: string): void => {
    ended = parseSseFrame(frame, onDelta) || ended;
  };

  for (;;) {
    if (signal?.aborted) throw new DOMException('Aborted', 'AbortError');
    const { done, value } = await reader.read();
    if (done) break;
    buffer += decoder.decode(value, { stream: true });
    // 统一行尾：剥离所有 \r（上游透传可能用 CRLF）。
    // SSE 数据行内不应有孤立 \r，strip 是安全的。
    buffer = buffer.replace(/\r/g, '');
    // 空行 = SSE 帧边界，只处理完整帧，避免半个 JSON 被解析失败
    let idx = buffer.indexOf('\n\n');
    while (idx !== -1) {
      const frame = buffer.slice(0, idx);
      buffer = buffer.slice(idx + 2);
      handleFrame(frame);
      idx = buffer.indexOf('\n\n');
    }
  }
  // 尾部残留帧
  if (buffer.trim()) handleFrame(buffer);
  if (!ended) onDelta({ content: '', isEnd: true });
}

export default api;
