const BASE_URL = '';

function getToken() {
  return localStorage.getItem('token');
}

function authHeaders() {
  const token = getToken();
  if (!token) return {};
  return { 'Authorization': `Bearer ${token}` };
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
  const data = await res.json();
  if (!res.ok) {
    throw new Error(data.error || data.message || `Request failed with status ${res.status}`);
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

  // ── 通知系统（Telegram + SMTP）──
  getNotifyConfig: () => request('GET', '/api/notify/config'),
  updateNotifyConfig: (data) => request('PUT', '/api/notify/config', data),
  testTelegram: () => request('POST', '/api/notify/test-telegram'),
  testEmail: (to) => request('POST', '/api/notify/test-email', { to }),
};