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

  // Accounts
  listAccounts: () =>
    request('GET', '/api/accounts'),

  addAccount: (name, account_id, api_token) =>
    request('POST', '/api/accounts', { name, account_id, api_token }),

  testAccount: (name, account_id, api_token) =>
    request('POST', '/api/accounts/test', { name, account_id, api_token }),

  updateAccount: (id, data) =>
    request('PUT', `/api/accounts/${id}`, data),

  deleteAccount: (id) =>
    request('DELETE', `/api/accounts/${id}`),

  // API Keys
  listKeys: () =>
    request('GET', '/api/keys'),

  generateKey: (name) =>
    request('POST', '/api/keys', { name }),

  deleteKey: (id) =>
    request('DELETE', `/api/keys/${id}`),

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

  // Model Usage
  getModelUsage: () =>
    request('GET', '/api/usage/models'),

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
};