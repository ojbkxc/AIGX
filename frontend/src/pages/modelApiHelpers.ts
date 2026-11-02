/**
 * 模型管理页内 API helper（Task 9 会把这些端点在后端补齐后收敛进 api/index.ts）。
 *
 * 依赖底层 fetch + api.getToken 处理鉴权头，避免复制 request 的登出逻辑；
 * 这里只做「拼路径 / 发请求 / 统一错误」三件事：
 *   · 端点不存在（404/405/501）→ 抛出带 HTTP 状态的 Error，由页面弹 toast；
 *   · 网络中断 → 同 request 层给可读文案；
 *   · 业务错误（error/message 字段）→ 原样透出。
 */

import type { ManageView } from '../types';

/** 与 api/index.ts 的 API_BASE 保持一致（页面内 helper 不 import 其内部符号） */
const API_BASE = '/api';

/** 底层请求：鉴权头与 api/index.ts 的 getToken 同源（localStorage token），错误统一转 Error（保留 status 供上层判断） */
async function req<T>(method: string, path: string, body?: unknown): Promise<T> {
  const headers: Record<string, string> = { 'Content-Type': 'application/json' };
  // 与 api/index.ts getToken 同源；拿不到 token 就裸发（端点会 401，页面兜底）
  let token: string | null = null;
  try {
    token = localStorage.getItem('token');
  } catch {
    /* 隐私模式下静默 */
  }
  if (token) headers.Authorization = `Bearer ${token}`;
  let res: Response;
  try {
    res = await fetch(path, {
      method,
      headers,
      body: body === undefined ? undefined : JSON.stringify(body),
    });
  } catch {
    throw new Error('网络连接失败，请检查网络后重试');
  }
  const text = await res.text();
  let data: unknown = null;
  if (text) {
    try {
      data = JSON.parse(text);
    } catch {
      data = text;
    }
  }
  if (!res.ok) {
    const rec = data && typeof data === 'object' ? (data as Record<string, unknown>) : null;
    const msg = rec?.error ?? rec?.message ?? text ?? `Request failed with status ${res.status}`;
    const err = new Error(typeof msg === 'string' ? msg : String(msg)) as Error & { status?: number };
    err.status = res.status;
    throw err;
  }
  return data as T;
}

/** 写接口的返回：后端成功时回读最新完整视图；端点未就绪/老后端时可能为 null */
type Viewish = ManageView | null | undefined;

/** 添加自定义模型（对齐 Agent2API addCustomModel） */
export async function addCustomModel(data: {
  id: string;
  provider: string;
  owned_by?: string;
}): Promise<Viewish> {
  const res = await req<{ data?: Viewish }>('POST', `${API_BASE}/models/custom`, data);
  return res?.data;
}

/** 删除自定义模型（对齐 Agent2API removeCustomModel） */
export async function removeCustomModel(provider: string, id: string): Promise<null> {
  await req('POST', `${API_BASE}/models/custom/remove`, { provider, id });
  return null;
}

/**
 * 批量按前缀添加映射（对齐 Agent2API 批量加映射：前缀 + 分隔符 + 同名合并）。
 * 为每个目标生成「prefix + sep + 目标模型ID」的对外名；merge=true 时允许同一
 * 对外名指向多个目标（主备切换语义，与单条映射一致）。
 */
export async function batchAddAliasPrefix(
  prefix: string,
  opts: { sep?: string; merge?: boolean; targets: string[] },
): Promise<Viewish> {
  const res = await req<{ data?: Viewish }>('POST', `${API_BASE}/models/alias/prefix`, {
    prefix,
    sep: opts.sep ?? '-',
    merge: opts.merge ?? true,
    targets: opts.targets,
  });
  return res?.data;
}