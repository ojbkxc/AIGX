/**
 * 客户端接入多配置管理（#3）的类型契约。
 *
 * 与后端 `src/api/admin/client_config.rs` 一一对应：
 * - 记录视图**不含凭据**，只有 `hasApiKey`；
 * - 预览内容已由后端脱敏，`token` 是要原样回传给 apply 的确认令牌。
 */

export type ClientConfigTargetId = 'claude-code' | 'codex' | 'claude-desktop' | 'dsh';

export interface ClientConfigRecord {
  id: string;
  target: ClientConfigTargetId;
  name: string;
  enabled: boolean;
  createdAt: number;
  updatedAt: number;
  baseUrl: string;
  hasApiKey: boolean;
  model: string;
  /** 字符串（模型 ID）或含 id 的对象（DSH 可带 name/contextWindow 等） */
  models: Array<string | Record<string, unknown>>;
  options: Record<string, unknown>;
  note: string;
  lastAppliedAt: number;
}

export interface ClientConfigFileRef {
  kind: 'config' | 'credentials';
  path: string;
}

export interface ClientConfigTarget {
  id: ClientConfigTargetId;
  label: string;
  format: 'json' | 'toml' | 'yaml';
  configPath: string;
  files: ClientConfigFileRef[];
  configs: ClientConfigRecord[];
}

export interface ClientConfigListData {
  targets: ClientConfigTarget[];
}

export interface ClientConfigDiffLine {
  kind: 'same' | 'added' | 'removed';
  text: string;
  oldLine?: number;
  newLine?: number;
}

export interface ClientConfigFilePlan {
  kind: 'config' | 'credentials';
  path: string;
  format: 'json' | 'toml' | 'yaml';
  /** 脱敏后的当前内容 */
  before: string;
  /** 脱敏后的写入内容 */
  after: string;
  diff: ClientConfigDiffLine[];
  changed: boolean;
  created: boolean;
  deleted: boolean;
}

export interface ClientConfigPreviewData {
  target: ClientConfigTargetId;
  recordId: string;
  configPath: string;
  files: ClientConfigFilePlan[];
  warnings: string[];
  hasChanges: boolean;
  fingerprint: string;
  /** 写入确认令牌（apply 时必须原样回传） */
  token: string;
  record: ClientConfigRecord;
}

export interface ClientConfigAppliedFile {
  kind: 'config' | 'credentials';
  path: string;
  backupId: string;
}

export interface ClientConfigApplyData {
  changed: boolean;
  files: ClientConfigAppliedFile[];
  appliedAt: number | null;
}

export interface ClientConfigBackupItem {
  id: string;
  kind: 'config' | 'credentials';
  path: string;
  fileName: string;
  size: number;
  createdAt: number;
}

export interface ClientConfigBackupsData {
  record: ClientConfigRecord;
  keep: number;
  items: ClientConfigBackupItem[];
}

export interface ClientConfigRestoreData {
  restored: boolean;
  backupId: string;
  kind: 'config' | 'credentials';
  safetyBackups: Array<{ id: string; kind: string }>;
}

/** 新增/更新请求体（camelCase，与后端 serde 一致）。 */
export interface ClientConfigUpsertBody {
  target?: ClientConfigTargetId;
  name?: string;
  enabled?: boolean;
  baseUrl?: string;
  apiKey?: string;
  clearApiKey?: boolean;
  model?: string;
  models?: Array<string | Record<string, unknown>>;
  options?: Record<string, unknown>;
  note?: string;
}
