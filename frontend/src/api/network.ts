// 网络层 API 调用模块
// 仅保留后端已实现的路由（/api/network/*），未实现端点的方法已移除
import type {
  NetworkConfigRequest,
  AccountConfigRequest,
  NetworkStatusRaw,
} from '../types/network';

const BASE_URL = '';
const getToken = (): string | null => localStorage.getItem('token');

function authHeaders(): Record<string, string> {
  const token = getToken();
  if (!token) return {};
  return { Authorization: `Bearer ${token}` };
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
    return null as T;
  }

  const data = await res.json() as Record<string, unknown>;
  if (!res.ok) {
    throw new Error(String(data.detail || data.error || '请求失败'));
  }
  return data as T;
}

// 获取网络层健康状态（后端返回 snake_case 原始结构）
export async function getNetworkStatus(): Promise<NetworkStatusRaw> {
  return request<NetworkStatusRaw>('GET', '/api/network/status');
}

// 更新网络层配置
export async function updateNetworkConfig(configId: string | number, config: NetworkConfigRequest): Promise<unknown> {
  return request('PUT', `/api/network/config/${configId}`, config);
}

// 重启网络层
export async function restartNetwork(): Promise<unknown> {
  return request('POST', '/api/network/restart');
}

// 添加网络层账号
export async function addNetworkAccount(accountId: string | number, accountConfig: AccountConfigRequest): Promise<unknown> {
  return request('POST', `/api/network/accounts/${accountId}`, accountConfig);
}

// 删除网络层账号
export async function removeNetworkAccount(accountId: string | number): Promise<unknown> {
  return request('DELETE', `/api/network/accounts/${accountId}`);
}

// 获取网络层原始状态（供监控面板聚合换算为指标）
export async function getNetworkMetrics(): Promise<NetworkStatusRaw> {
  const res = await fetch(`${BASE_URL}/api/network/status`, {
    headers: authHeaders(),
  });
  if (res.status === 401) {
    throw new Error('Unauthorized');
  }
  return res.json() as Promise<NetworkStatusRaw>;
}

// 保持与旧 import { api as networkApi } 用法兼容的聚合对象
export const api = {
  getNetworkStatus,
  updateNetworkConfig,
  restartNetwork,
  addNetworkAccount,
  removeNetworkAccount,
  getNetworkMetrics,
};
