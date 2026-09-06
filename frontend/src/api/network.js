// 网络层 API 调用模块
// 仅保留后端已实现的路由（/api/network/*），未实现端点的方法已移除
const BASE_URL = '';
const getToken = () => localStorage.getItem('token');

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

  if (res.status === 401) {
    // 清除登录态并跳转登录
    try {
      localStorage.removeItem('token');
      localStorage.removeItem('email');
      localStorage.removeItem('username');
      localStorage.removeItem('expires_at');
    } catch {
      // 忽略 localStorage 异常
    }
    if (window.location.pathname !== '/login') {
      window.location.href = '/login';
    }
    throw new Error('Unauthorized');
  }

  if (res.status === 204) {
    return null;
  }

  const data = await res.json();
  if (!res.ok) {
    throw new Error(data.detail || data.error || '请求失败');
  }
  return data;
}

// 获取网络层健康状态
export async function getNetworkStatus() {
  return request('GET', '/api/network/status');
}

// 更新网络层配置
export async function updateNetworkConfig(configId, config) {
  return request('PUT', `/api/network/config/${configId}`, config);
}

// 重启网络层
export async function restartNetwork() {
  return request('POST', '/api/network/restart');
}

// 添加网络层账号
export async function addNetworkAccount(accountId, accountConfig) {
  return request('POST', `/api/network/accounts/${accountId}`, accountConfig);
}

// 删除网络层账号
export async function removeNetworkAccount(accountId) {
  return request('DELETE', `/api/network/accounts/${accountId}`);
}

// 获取网络层指标（聚合自 /api/network/status）
export async function getNetworkMetrics() {
  const res = await fetch(`${BASE_URL}/api/network/status`, {
    headers: authHeaders(),
  });
  if (res.status === 401) {
    throw new Error('Unauthorized');
  }
  return await res.json();
}
