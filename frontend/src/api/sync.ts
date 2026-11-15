/**
 * 云同步 API 客户端（设置 → 数据 → 云同步）
 *
 * 后端契约见 `src/api/admin/sync.rs` 与 `src/sync/config.rs`：
 *   GET  /api/admin/sync/config   设置 + 远端路径（密码 / Secret 只回 has_xxx）
 *   PUT  /api/admin/sync/config   保存设置（空密码 = 沿用旧值）
 *   GET  /api/admin/sync/status   运行状态（上次成功 / 错误 / 哈希 / 连续失败）
 *   POST /api/admin/sync/test     测试连通性（用请求体里未保存的新值）
 *   POST /api/admin/sync/now      立即同步 { force?: bool }
 *   POST /api/admin/sync/pull     拉取恢复 { mode?: "merge"|"local"|"remote" }
 *
 * 独立成文件的原因：`api/index.ts` 正被其他会话并行编辑（品牌面板等），
 * 共享大文件互卷在制品；这里自成一体，复用同样的鉴权与错误信封约定。
 */

const API_BASE = '/api';

function getToken(): string | null {
  try {
    return localStorage.getItem('token');
  } catch {
    return null;
  }
}

function authHeaders(): Record<string, string> {
  const token = getToken();
  if (!token) return {};
  return { Authorization: `Bearer ${token}` };
}

function clearSessionAndRedirect(): void {
  const token = getToken();
  if (!token || window.location.pathname.startsWith('/login')) return;
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
    res = await fetch(`${API_BASE}${path}`, options);
  } catch {
    throw new Error('网络连接失败，请检查网络后重试');
  }
  if (res.status === 401) {
    clearSessionAndRedirect();
    throw new Error('Unauthorized');
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
      (data && typeof data === 'object' &&
        (((data as Record<string, unknown>).error as string) ||
          ((data as Record<string, unknown>).message as string))) ||
      (typeof text === 'string' && text) ||
      `Request failed with status ${res.status}`;
    const err = new Error(typeof msg === 'string' ? msg : String(msg)) as Error & { status?: number };
    err.status = res.status;
    throw err;
  }
  return data as T;
}

// ─── 类型（对齐后端 public_json / status_json） ────────────────

export interface SyncWebDavData {
  url: string;
  username: string;
  /** 是否已设置密码（界面不回显） */
  has_password: boolean;
}

export interface SyncS3Data {
  endpoint: string;
  bucket: string;
  region: string;
  access_key: string;
  prefix: string;
  /** 是否已设置 Secret（界面不回显） */
  has_secret_key: boolean;
}

/** GET /api/admin/sync/config 的 data，也是 PUT 的返回 data */
export interface SyncConfigData {
  enabled: boolean;
  backend: 'webdav' | 's3' | string;
  interval_secs: number;
  min_interval_secs: number;
  user_agent: string;
  path: string;
  /** 远端路径的展示形态（如 /aigx-sync 或 bucket/prefix/aigx-sync） */
  remote_path: string;
  webdav: SyncWebDavData;
  s3: SyncS3Data;
}

export interface SyncWebDavInput {
  url?: string;
  username?: string;
  /** 空串 = 沿用旧值 */
  password?: string;
}

export interface SyncS3Input {
  endpoint?: string;
  bucket?: string;
  region?: string;
  access_key?: string;
  /** 空串 = 沿用旧值 */
  secret_key?: string;
  prefix?: string;
}

/** PUT /api/admin/sync/config 与 POST /api/admin/sync/test 的请求体（全部可选） */
export interface SyncSaveInput {
  enabled?: boolean;
  backend?: string;
  interval_secs?: number;
  min_interval_secs?: number;
  user_agent?: string;
  path?: string;
  webdav?: SyncWebDavInput;
  s3?: SyncS3Input;
}

/** GET /api/admin/sync/status 的 data（时间戳均为毫秒，0/null = 无） */
export interface SyncStatusData {
  enabled: boolean;
  backend: string;
  remote_path: string;
  interval_secs: number;
  min_interval_secs: number;
  last_attempt_at: number;
  last_success_at: number;
  last_action: string;
  last_hash: string;
  last_error: string;
  consecutive_failures: number;
  retry_at: number;
  last_bytes: number;
  next_run_at: number;
}

export interface SyncTestResult {
  success: boolean;
  backend: string;
  remote_path: string;
}

export interface SyncNowResult {
  status: 'uploaded' | 'skipped' | string;
  remote_path: string;
  bytes: number;
  entries: number;
  content_hash: string;
  attempts: number;
  manual: boolean;
  /** skipped 时的原因 */
  reason?: string;
}

export type SyncPullMode = 'merge' | 'local' | 'remote';

export interface SyncPullMerged {
  kept_local: number;
  updated_remote: number;
  added: number;
  ignored_local: number;
  ignored_remote: number;
}

export interface SyncPullResult {
  status: string;
  mode: string;
  remote_path: string;
  device: string;
  created_at: number;
  written: number;
  merged: SyncPullMerged;
  content_hash: string;
  safety_backup: string;
}

interface ApiEnvelope<T> {
  success: boolean;
  data?: T;
  error?: string;
  message?: string;
}

// ─── 调用封装 ────────────────────────────────────────────────

export const syncApi = {
  getSyncConfig: (): Promise<ApiEnvelope<SyncConfigData>> =>
    request<ApiEnvelope<SyncConfigData>>('GET', '/admin/sync/config'),

  /** 保存设置；密码 / Secret 字段传空串 = 沿用旧值（不要省略字段） */
  saveSyncConfig: (input: SyncSaveInput): Promise<ApiEnvelope<SyncConfigData>> =>
    request<ApiEnvelope<SyncConfigData>>('PUT', '/admin/sync/config', input),

  /** 连通性测试：可只传要测的新值；空 body = 用已保存设置 */
  testSyncConnection: (input?: SyncSaveInput): Promise<ApiEnvelope<SyncTestResult>> =>
    request<ApiEnvelope<SyncTestResult>>('POST', '/admin/sync/test', input ?? {}),

  /** 立即同步；force=true 越过「内容未变化 / 最小间隔」闸门 */
  syncNow: (force = false): Promise<ApiEnvelope<SyncNowResult>> =>
    request<ApiEnvelope<SyncNowResult>>('POST', '/admin/sync/now', { force }),

  /** 从云端恢复：merge（默认）/ local（本机强制）/ remote（远端强制） */
  syncPull: (mode: SyncPullMode = 'merge'): Promise<ApiEnvelope<SyncPullResult>> =>
    request<ApiEnvelope<SyncPullResult>>('POST', '/admin/sync/pull', { mode }),

  getSyncStatus: (): Promise<ApiEnvelope<SyncStatusData>> =>
    request<ApiEnvelope<SyncStatusData>>('GET', '/admin/sync/status'),
};

export default syncApi;