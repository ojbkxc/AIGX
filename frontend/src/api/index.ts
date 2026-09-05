/**
 * AIGX API客户端
 * 统一的API调用层，为前端应用提供类型安全的数据访问接口
 */

import type {
  ApiResponse,
  User,
  Channel,
  ChannelUsage,
  ApiKey,
  DashboardStats,
  RealtimeMetrics,
  LoginRequest,
  RegisterRequest,
  AuthResponse,
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

async function request(method: string, path: string, body: unknown = null): Promise<any> {
  const headers: Record<string, string> = {
    'Content-Type': 'application/json',
    ...authHeaders(),
  };
  const options: RequestInit = { method, headers };
  if (body !== null) {
    options.body = JSON.stringify(body);
  }
  const res = await fetch(path, options);
  if (res.status === 401) {
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
    throw new Error('Unauthorized');
  }
  if (res.status === 204) {
    return null;
  }
  const text = await res.text();
  let data: any = null;
  if (text) {
    const contentType = res.headers.get('Content-Type') || '';
    if (contentType.includes('application/json')) {
      try {
        data = JSON.parse(text);
      } catch {
        if (!res.ok) throw new Error(text || `Request failed with status ${res.status}`);
        data = text;
      }
    } else if (res.ok) {
      data = text;
    }
  }
  if (!res.ok) {
    const msg =
      (data && typeof data === 'object' && (data.error || data.message)) ||
      (typeof text === 'string' && text) ||
      `Request failed with status ${res.status}`;
    throw new Error(msg);
  }
  return data;
}

class ApiError extends Error {
  constructor(
    public status: number,
    public code: string,
    message: string,
    public details?: Record<string, any>
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

export const api = {
  // ==================== 顶层便捷方法（页面直调，沿用后端路由契约） ====================
  login: (email: string, password: string): Promise<any> =>
    request('POST', `${API_BASE}/auth/login`, { email, password }),
  register: (username: string, email: string, password: string): Promise<any> =>
    request('POST', `${API_BASE}/auth/register`, { email, password, username }),
  forgotPassword: (email: string): Promise<any> =>
    request('POST', `${API_BASE}/auth/forgot-password`, { email }),
  getUsageSummary: (): Promise<any> => request('GET', `${API_BASE}/usage/summary`),
  getTodayTokens: (): Promise<any> => request('GET', `${API_BASE}/tokens/today`),
  getLimits: (): Promise<any> => request('GET', `${API_BASE}/limits`),
  getTrend: (): Promise<any> => request('GET', `${API_BASE}/usage/trend`),
  saveEpayConfig: (config: any): Promise<any> =>
    request('PUT', `${API_BASE}/epay/config`, config),
  listGroups: (): Promise<any> => request('GET', `${API_BASE}/groups`),
  getIpLists: (): Promise<any> => request('GET', `${API_BASE}/ip/filter`),
  addIpWhitelist: (ip: string): Promise<any> =>
    request('POST', `${API_BASE}/ip/whitelist`, { pattern: ip }),
  addIpBlacklist: (ip: string): Promise<any> =>
    request('POST', `${API_BASE}/ip/blacklist`, { pattern: ip }),
  removeIpWhitelist: (pattern: string): Promise<any> =>
    request('DELETE', `${API_BASE}/ip/whitelist/${encodeURIComponent(pattern)}`),
  removeIpBlacklist: (pattern: string): Promise<any> =>
    request('DELETE', `${API_BASE}/ip/blacklist/${encodeURIComponent(pattern)}`),
  listKeys: (): Promise<any> => request('GET', `${API_BASE}/keys`),
  getRequestLogs: (): Promise<any> => request('GET', `${API_BASE}/logs/requests`),
  getModelMappings: (): Promise<any> => request('GET', `${API_BASE}/settings`),
  saveModelMapping: (mapping: any): Promise<any> =>
    request('PUT', `${API_BASE}/settings`, { mappings: mapping, replace_all: false }),
  deleteModelMapping: (_id: string): Promise<any> =>
    request('PUT', `${API_BASE}/settings`, { mappings: {}, replace_all: false }),
  listOrders: (): Promise<any> => request('GET', `${API_BASE}/orders`),
  chatCompletions: (payload: any): Promise<any> =>
    request('POST', '/v1/chat/completions', payload),
  listPrices: (): Promise<any> => request('GET', `${API_BASE}/prices`),
  listRedemptions: (): Promise<any> => request('GET', `${API_BASE}/redemptions`),
  getSecurityIncidents: (): Promise<any> =>
    request('GET', `${API_BASE}/monitor/security/events`),
  getSecurityAlerts: (): Promise<any> =>
    request('GET', `${API_BASE}/alerts/active`),
  saveSettings: (usage: any, limits: any, notification: any): Promise<any> =>
    request('PUT', `${API_BASE}/settings`, { usage, limits, notification }),
  getBalance: (): Promise<any> => request('GET', `${API_BASE}/users/me`),
  getTransactions: (): Promise<any> => request('GET', `${API_BASE}/orders/me`),
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
        throw this.handleError(response);
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
        throw this.handleError(response);
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
        throw this.handleError(response);
      }
    },

    /**
     * 刷新Token
     */
    async refreshToken(refreshToken: string): Promise<AuthResponse> {
      const response = await fetch(`${API_BASE}/auth/refresh`, {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json',
        },
        body: JSON.stringify({ refresh_token: refreshToken }),
      });

      if (!response.ok) {
        throw this.handleError(response);
      }

      return response.json();
    },
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
        throw this.handleError(response);
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
        throw this.handleError(response);
      }

      return response.json();
    },

    /**
     * 获取用户详情
     */
    async get(id: string): Promise<User> {
      const response = await fetch(`${API_BASE}/users/${id}`);

      if (!response.ok) {
        throw this.handleError(response);
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
        throw this.handleError(response);
      }

      return response.json();
    },

    /**
     * 获取渠道详情
     */
    async get(id: string): Promise<Channel> {
      const response = await fetch(`${API_BASE}/channels/${id}`);

      if (!response.ok) {
        throw this.handleError(response);
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
        throw this.handleError(response);
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
        throw this.handleError(response);
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
        throw this.handleError(response);
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
        throw this.handleError(response);
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
        throw this.handleError(response);
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
        throw this.handleError(response);
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
        throw this.handleError(response);
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
        throw this.handleError(response);
      }

      return response.json();
    },
  },

  /**
   * Dashboard相关API
   */
  dashboard: {
    /**
     * 获取仪表盘统计数据
     */
    async getStats(): Promise<DashboardStats> {
      const response = await fetch(`${API_BASE}/dashboard/stats`);

      if (!response.ok) {
        throw this.handleError(response);
      }

      return response.json();
    },

    /**
     * 获取实时指标
     */
    async getRealtime(): Promise<RealtimeMetrics> {
      const response = await fetch(`${API_BASE}/dashboard/realtime`);

      if (!response.ok) {
        throw this.handleError(response);
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
        throw this.handleError(response);
      }

      return response.json();
    },
  },

  /**
   * 通用错误处理
   */
  handleError(response: Response): never {
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
  },

  // ==================== 网络层 API ====================

  network: {
    async getStatus(): Promise<ApiResponse<Record<string, any>>> {
      const response = await fetch(`${API_BASE}/network/status`);
      if (!response.ok) throw this.handleError(response);
      return response.json();
    },

    async getConfig(configId: string): Promise<ApiResponse<Record<string, any>>> {
      const response = await fetch(`${API_BASE}/network/config/${configId}`);
      if (!response.ok) throw this.handleError(response);
      return response.json();
    },

    async updateConfig(
      configId: string,
      enabled: boolean,
      strategy?: string
    ): Promise<ApiResponse<Record<string, any>>> {
      const config: { enabled: boolean; strategy?: string } = { enabled };
      if (strategy) config.strategy = strategy;

      const response = await fetch(`${API_BASE}/network/config/${configId}`, {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(config),
      });
      if (!response.ok) throw this.handleError(response);
      return response.json();
    },

    async restart(): Promise<ApiResponse<Record<string, any>>> {
      const response = await fetch(`${API_BASE}/network/restart`, { method: 'POST' });
      if (!response.ok) throw this.handleError(response);
      return response.json();
    },

    async addAccount(accountId: string): Promise<ApiResponse<Record<string, any>>> {
      const response = await fetch(`${API_BASE}/network/accounts/${accountId}`, { method: 'POST' });
      if (!response.ok) throw this.handleError(response);
      return response.json();
    },

    async removeAccount(accountId: string): Promise<ApiResponse<Record<string, any>>> {
      const response = await fetch(`${API_BASE}/network/accounts/${accountId}`, { method: 'DELETE' });
      if (!response.ok) throw this.handleError(response);
      return response.json();
    },
  },

};

export default api;
