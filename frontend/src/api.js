const BASE_URL = '';

function getToken() {
  return localStorage.getItem('token');
}

function authHeaders() {
  const token = getToken();
  if (!token) return {};
  return { 'Authorization': `Bearer ${token}` };
}

// 清除本地登录态并跳转登录页（401 / 鉴权失败时调用）
function handleUnauthorized() {
  try {
    localStorage.removeItem('token');
    localStorage.removeItem('email');
    localStorage.removeItem('username');
    localStorage.removeItem('expires_at');
  } catch {
    // 忽略 localStorage 异常
  }
  // 避免在已在登录页时再次跳转造成循环
  if (window.location.pathname !== '/login') {
    window.location.href = '/login';
  }
}

async function request(method, path, body = null) {
  const headers = {
    'Content-Type': 'application/json',
    ...authHeaders(),
  };
  const options = { method, headers };
  if (body !== null) {
    options.body = JSON.stringify(body);
  }
  const res = await fetch(`${BASE_URL}${path}`, options);

  // 401 未授权：清登录态并跳登录
  if (res.status === 401) {
    handleUnauthorized();
    throw new Error('Unauthorized');
  }

  // 204 No Content：直接返回 null，不尝试解析 body
  if (res.status === 204) {
    return null;
  }

  // 先取文本，再按 Content-Type 决定是否解析为 JSON
  const text = await res.text();
  const contentType = res.headers.get('Content-Type') || '';
  let data = null;
  if (text) {
    if (contentType.includes('application/json')) {
      try {
        data = JSON.parse(text);
      } catch {
        // 声明是 JSON 但解析失败：把原文当作错误信息
        if (!res.ok) throw new Error(text || `Request failed with status ${res.status}`);
        data = text;
      }
    } else if (res.ok) {
      // 非 JSON 成功响应：返回原始文本
      data = text;
    }
    // 非 JSON 且失败：fallthrough 到下面的错误处理
  }

  if (!res.ok) {
    const errMsg =
      (data && typeof data === 'object' && (data.error || data.message)) ||
      (typeof text === 'string' && text) ||
      `Request failed with status ${res.status}`;
    throw new Error(errMsg);
  }
  return data;
}

export const api = {
  // Auth
  login: (email, password) =>
    request('POST', '/api/auth/login', { email, password }),

  register: (email, password, username) =>
    request('POST', '/api/auth/register', { email, password, username }),

  logout: () =>
    request('POST', '/api/auth/logout'),

  // Usage
  getUsageSummary: () =>
    request('GET', '/api/usage/summary'),

  // Models（网关可用模型列表，用于对话调试模型下拉）
  listModels: () =>
    request('GET', '/v1/models'),

  // API Keys
  listKeys: () =>
    request('GET', '/api/keys'),

  // Settings / Model Mappings
  getSettings: () =>
    request('GET', '/api/settings'),

  updateSettings: (mappings, replace_all = false) =>
    request('PUT', '/api/settings', { mappings, replace_all }),

  // Limits
  getLimits: () =>
    request('GET', '/api/limits'),

  updateLimits: (data) =>
    request('PUT', '/api/limits', data),

  // Token Stats
  getTodayTokens: () =>
    request('GET', '/api/tokens/today'),

  // Trend
  getTrend: () =>
    request('GET', '/api/usage/trend'),


  // Users
  listUsers: () => request('GET', '/api/users'),
  createUser: (data) => request('POST', '/api/users', data),
  updateUser: (id, data) => request('PUT', `/api/users/${id}`, data),
  deleteUser: (id) => request('DELETE', `/api/users/${id}`),
  getMe: () => request('GET', '/api/users/me'),

  // Epay
  getEpayConfig: () => request('GET', '/api/epay/config'),
  updateEpayConfig: (data) => request('PUT', '/api/epay/config', data),

  // Orders & Topup
  listOrders: () => request('GET', '/api/orders'),
  myOrders: () => request('GET', '/api/orders/me'),
  topup: (amount, payment_method) =>
    request('POST', '/api/topup', { amount, payment_method }),

  // ── 通用渠道管理（功能 1）──
  listChannels: () => request('GET', '/api/channels'),
  addChannel: (data) => request('POST', '/api/channels', data),
  updateChannel: (id, data) => request('PUT', `/api/channels/${id}`, data),
  patchChannel: (id, data) => request('PATCH', `/api/channels/${id}`, data),
  deleteChannel: (id) => request('DELETE', `/api/channels/${id}`),
  testChannel: (id) => request('POST', `/api/channels/${id}/test`),

  // 拉取上游模型列表（后端代理转发，避免浏览器 CORS 限制）
  fetchChannelModels: (data) =>
    request('POST', '/api/channels/fetch_models', data),

  // 渠道对话调试（OpenAI / Anthropic 协议，流式时返回 SSE 事件数组）
  testChannelChat: async (data) => {
    const headers = {
      'Content-Type': 'application/json',
      ...authHeaders(),
    };
    const res = await fetch(`${BASE_URL}/api/channels/chat_test`, {
      method: 'POST',
      headers,
      body: JSON.stringify(data),
    });
    if (res.status === 401) {
      handleUnauthorized();
      throw new Error('Unauthorized');
    }
    const contentType = res.headers.get('Content-Type') || '';
    if (contentType.includes('text/event-stream')) {
      // 流式：把 SSE 逐条解析为 { content } 增量数组
      const text = await res.text();
      const chunks = [];
      let buf = '';
      for (const line of text.split('\n')) {
        const t = line.trim();
        if (t.startsWith('data:')) {
          buf += t.slice(5).trim();
        } else if (t === '') {
          if (buf && buf !== '[DONE]') {
            try {
              const parsed = JSON.parse(buf);
              const content =
                (parsed.choices && parsed.choices[0] && parsed.choices[0].delta &&
                  (parsed.choices[0].delta.content || parsed.choices[0].delta.text)) ||
                (parsed.delta && parsed.delta.text) ||
                (parsed.content && parsed.content[0] && parsed.content[0].text) || '';
              if (content) chunks.push({ content });
            } catch {
              // 忽略非 JSON 帧
            }
          }
          buf = '';
        } else {
          buf += t;
        }
      }
      if (buf && buf !== '[DONE]') {
        try {
          const parsed = JSON.parse(buf);
          const content =
            (parsed.choices && parsed.choices[0] && parsed.choices[0].delta &&
              (parsed.choices[0].delta.content || parsed.choices[0].delta.text)) ||
            (parsed.delta && parsed.delta.text) ||
            (parsed.content && parsed.content[0] && parsed.content[0].text) || '';
          if (content) chunks.push({ content });
        } catch {
          // ignore
        }
      }
      return { stream: chunks };
    }
    // 非流式：走通用 request 解析
    const text = await res.text();
    let parsed = null;
    if (text) {
      try { parsed = JSON.parse(text); } catch { /* ignore */ }
    }
    if (!res.ok) {
      const errMsg =
        (parsed && typeof parsed === 'object' && (parsed.error || parsed.message)) || text || `Request failed with status ${res.status}`;
      throw new Error(errMsg);
    }
    return parsed;
  },

  // ── 令牌管理增强（功能 2）──
  listTokens: () => request('GET', '/api/tokens'),
  addToken: (data) => request('POST', '/api/tokens', data),
  updateToken: (id, data) => request('PUT', `/api/tokens/${id}`, data),
  deleteToken: (id) => request('DELETE', `/api/tokens/${id}`),
  resetTokenUsed: (id) => request('POST', `/api/tokens/${id}/reset_used`),

  // ── 模型定价目录（功能 3）──
  listPrices: () => request('GET', '/api/prices'),
  upsertPrice: (data) => request('POST', '/api/prices', data),

  deletePrice: (model) => request('DELETE', `/api/prices/${model}`),

  // ── 倍率配置 ──
  getRatios: () => request('GET', '/api/ratios'),
  updateRatios: (data) => request('PUT', '/api/ratios', data),

  // ── 用户分组管理（功能 4）──
  listGroups: () => request('GET', '/api/groups'),
  upsertGroup: (data) => request('POST', '/api/groups', data),

  deleteGroup: (name) => request('DELETE', `/api/groups/${name}`),

  // ── 日志与审计（功能 1）──
  listRequestLogs: (params = {}) => request('GET', `/api/logs/requests?${new URLSearchParams(params)}`),
  listAuditLogs: (params = {}) => request('GET', `/api/logs/audits?${new URLSearchParams(params)}`),


  // ── 兑换码（功能 2）──
  listRedemptions: (params = {}) => request('GET', `/api/redemptions?${new URLSearchParams(params)}`),
  batchRedemptions: (data) => request('POST', '/api/redemptions/batch', data),
  deleteRedemption: (id) => request('DELETE', `/api/redemptions/${id}`),
  redeem: (code) => request('POST', '/api/redemptions/redeem', { code }),

  // ── 限流配置（功能 3）──
  getRateLimitConfig: () => request('GET', '/api/ratelimit/config'),
  updateRateLimitConfig: (data) => request('PUT', '/api/ratelimit/config', data),

  // ── 数据看板增强（功能 4）──
  getConsumptionTrend: () => request('GET', '/api/dashboard/consumption_trend'),
  getModelDistribution: () => request('GET', '/api/dashboard/model_distribution'),
  getUserRanking: () => request('GET', '/api/dashboard/user_ranking'),
  getChannelHealth: () => request('GET', '/api/dashboard/channel_health'),
  getRealtime: () => request('GET', '/api/dashboard/realtime'),

  // ── 通知系统（Telegram + SMTP + Slack + Webhook）──
  getNotifyConfig: () => request('GET', '/api/notify/config'),
  updateNotifyConfig: (data) => request('PUT', '/api/notify/config', data),
  testTelegram: () => request('POST', '/api/notify/test-telegram'),
  testEmail: (to) => request('POST', '/api/notify/test-email', { to }),
  testSlack: () => request('POST', '/api/notify/test-slack'),
  testWebhook: (message) => request('POST', '/api/notify/test-webhook', { message }),

  // ── 告警规则（批次5）──
  getAlertRules: () => request('GET', '/api/alerts/rules'),
  updateAlertRules: (rules) => request('PUT', '/api/alerts/rules', { rules }),
  getActiveAlerts: () => request('GET', '/api/alerts/active'),
  testAlert: (kind, value) => request('POST', '/api/alerts/test', { kind, value }),

  // ── 系统监控（批次6）──
  getSystemMonitor: () => request('GET', '/api/monitor/system'),

  // ── Playground ──
  playgroundChat: (data) => request('POST', '/api/playground/chat', data),

  // ── 安全监控 ──
  getSecurityOverview: () => request('GET', '/api/monitor/security'),
  getSecurityEvents: (params = {}) => request('GET', `/api/security/events?${new URLSearchParams(params)}`),

  // ── IP 管理 ──
  getIpFilter: () => request('GET', '/api/ip/filter'),
  updateIpFilter: (data) => request('PUT', '/api/ip/filter', data),
  addWhitelist: (pattern, note) => request('POST', '/api/ip/whitelist', { pattern, note }),
  removeWhitelist: (pattern) => request('DELETE', `/api/ip/whitelist/${encodeURIComponent(pattern)}`),
  addBlacklist: (pattern, note) => request('POST', '/api/ip/blacklist', { pattern, note }),
  removeBlacklist: (pattern) => request('DELETE', `/api/ip/blacklist/${encodeURIComponent(pattern)}`),

  // ── 缓存管理 ──
  getCacheStats: () => request('GET', '/api/cache/stats'),
  clearCache: () => request('POST', '/api/cache/clear'),

  // ── 令牌轮换 ──
  rotateToken: (id) => request('POST', `/api/tokens/${id}/rotate`),

  // ── 忘记密码/重置密码 ──
  forgotPassword: (email) => request('POST', '/api/auth/forgot-password', { email }),
  resetPassword: (token, password) => request('POST', '/api/auth/reset-password', { token, password }),

  // ── 用户名检查 ──
  checkUsername: (username) => request('GET', `/api/users/check?username=${encodeURIComponent(username)}`),

  // ── 价格同步 ──
  getPriceSyncConfig: () => request('GET', '/api/pricing/sync-config'),
  updatePriceSyncConfig: (data) => request('PUT', '/api/pricing/sync-config', data),
  triggerPriceSync: () => request('POST', '/api/pricing/sync'),

  // ── 汇率配置 ──
  getExchangeRates: () => request('GET', '/api/pricing/exchange-rates'),
  updateExchangeRates: (data) => request('PUT', '/api/pricing/exchange-rates', data),
};