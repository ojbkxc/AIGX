// 网络层 API 调用模块
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

// 获取网络层指标
export async function getNetworkMetrics() {
  try {
    const res = await fetch(`${BASE_URL}/api/network/metrics`, {
      headers: authHeaders(),
    });
    if (res.status === 401) {
      handleUnauthorized();
      throw new Error('Unauthorized');
    }
    return await res.json();
  } catch (error) {
    throw error;
  }
}

// 获取监控历史数据
export async function getMetricsHistory(startTime, endTime, interval = '1m') {
  return request('GET', `/api/network/metrics/history`, {
    startTime,
    endTime,
    interval,
  });
}

// 获取分布式节点列表
export async function getDistributedNodes() {
  return request('GET', '/api/network/distributed/nodes');
}

// 节点健康状况
export async function getNodeHealth(nodeId) {
  return request('GET', `/api/network/distributed/nodes/${nodeId}/health`);
}

// 节点控制
export async function controlNode(nodeId, action) {
  return request('POST', `/api/network/distributed/nodes/${nodeId}/${action}`, {});
}

// 自动扩缩容控制
export async function updateScalingConfig(config) {
  return request('PUT', '/api/network/scaling', config);
}

// 获取扩缩容状态
export async function getScalingStatus() {
  return request('GET', '/api/network/scaling/status');
}

// 获取告警配置
export async function getAlertConfig() {
  return request('GET', '/api/network/alerts/config');
}

// 更新告警配置
export async function updateAlertConfig(config) {
  return request('PUT', '/api/network/alerts/config', config);
}

// 测试告警发送
export async function testAlert(alertType) {
  return request('POST', '/api/network/alerts/test', { alertType });
}

// 导出监控数据
export async function exportMetrics(format = 'json') {
  try {
    const res = await fetch(`${BASE_URL}/api/network/metrics/export?format=${format}`, {
      headers: authHeaders(),
    });
    if (res.status === 401) {
      handleUnauthorized();
      throw new Error('Unauthorized');
    }
    return await res.blob();
  } catch (error) {
    throw error;
  }
}