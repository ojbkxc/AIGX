/**
 * API 传输层（design 1.4：`api/index.ts` 按域拆分的第 1 块）。
 *
 * 统一 base、鉴权头、查询串与 fetch 封装：401 登出跳转、204 空响应、
 * JSON/文本解析、错误壳提取（`error` / `message` 两键并存的兼容）。
 */

export const API_BASE = '/api';

export function getToken(): string | null {
  return localStorage.getItem('token');
}

export function authHeaders(): Record<string, string> {
  const token = getToken();
  if (!token) return {};
  return { Authorization: `Bearer ${token}` };
}

/** 把分页/筛选参数安全转成查询串（数字自动字符串化） */
export function buildQuery(params: Record<string, string | number>): string {
  const entries = Object.entries(params)
    .filter(([, v]) => v !== undefined && v !== null && v !== '')
    .map(([k, v]) => [k, String(v)] as [string, string]);
  return new URLSearchParams(entries).toString();
}

/** 报表窗口参数（与后端 resolve_window 契约一致）：days 缺省 30；
 *  start/end 合法时后端优先生效（自定义区间档传这两个，不传 days） */
export type ReportWindow = { days?: number; start?: string; end?: string };

export function reportWindowQs(q: ReportWindow): string {
  if (q.start && q.end) {
    return new URLSearchParams({ start: q.start, end: q.end }).toString();
  }
  return `days=${q.days ?? 30}`;
}

export async function request<T = unknown>(
  method: string,
  path: string,
  body: unknown = null,
  opts: { skipAuthRedirect?: boolean; signal?: AbortSignal } = {},
): Promise<T> {
  const headers: Record<string, string> = {
    'Content-Type': 'application/json',
    ...authHeaders(),
  };
  const options: RequestInit = { method, headers, signal: opts.signal };
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
  //
  // `skipAuthRedirect`：登录 / 二次验证 / 改密这类**以 401 表达业务错误**
  // 的接口（如 TOTP 密码错误也返回 401）不参与登出逻辑 —— 否则用户在
  // 安全设置里输错一次密码就会被强制登出。这类调用方自己处理错误文案。
  if (res.status === 401 && !opts.skipAuthRedirect) {
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
    // 保留 HTTP 状态码：调用方（如翻译的 429 预算限流）可据此区分
    // 「可重试/限流」与「参数错/服务错」等不同失败语义。
    const err = new Error(typeof msg === 'string' ? msg : String(msg)) as Error & { status?: number };
    err.status = res.status;
    throw err;
  }
  return data as T;
}
