import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import {
  syncApi,
  type SyncConfigData,
  type SyncStatusData,
  type SyncSaveInput,
} from '../api/sync';
import { useToast } from '../components/Toast';
import { Switch, SkeletonList } from '../components/ui';

/**
 * 云同步（设置 → 数据 → 云同步）
 *
 * 把本机业务数据的快照自动同步到远端（WebDAV / S3 兼容存储），多台 AIGX
 * 之间保持数据一致。对照 Agent2API 的多机同步能力。
 *
 * - 设置：总开关 / 后端类型（WebDAV / S3）/ 按类型的连接字段 / 间隔 /
 *   最小间隔 / 自定义 User-Agent / 同步路径
 * - 操作：连通性测试 / 立即同步 / 从云端恢复（危险操作 + 合并策略二次确认）
 * - 状态：上次成功 / 上次尝试 / 上次错误 / 连续失败 / 内容哈希 / 下次同步
 *
 * 凭据安全：密码 / Secret 只回 has_xxx 布尔位，界面不回显；保存时留空 =
 * 沿用旧值（后端 AES-256-GCM 加密存储，见 src/sync/config.rs）。
 */

type BackendType = 'webdav' | 's3';
type PullMode = 'merge' | 'local' | 'remote';

/** 表单状态：数值字段用 string 承载（允许空 / 非法输入，保存时转换） */
interface CloudSyncForm {
  enabled: boolean;
  backend: BackendType;
  interval_secs: string;
  min_interval_secs: string;
  user_agent: string;
  path: string;
  webdav_url: string;
  webdav_username: string;
  webdav_password: string;
  s3_endpoint: string;
  s3_bucket: string;
  s3_region: string;
  s3_access_key: string;
  s3_secret_key: string;
  s3_prefix: string;
}

const DEFAULT_FORM: CloudSyncForm = {
  enabled: false,
  backend: 'webdav',
  interval_secs: '3600',
  min_interval_secs: '60',
  user_agent: '',
  path: 'aigx-sync',
  webdav_url: '',
  webdav_username: '',
  webdav_password: '',
  s3_endpoint: '',
  s3_bucket: '',
  s3_region: '',
  s3_access_key: '',
  s3_secret_key: '',
  s3_prefix: '',
};

/** 云端恢复的合并策略选项（与后端 MergeMode 对齐） */
const PULL_MODES: Array<{ value: PullMode; label: string; hint: string }> = [
  {
    value: 'merge',
    label: '远新胜（推荐）',
    hint: '同键比较更新时间，新者胜；远端缺键保留本机；本地缺键采纳远端；本地独有键永不被远端删除',
  },
  {
    value: 'remote',
    label: '远端强制覆盖',
    hint: '忽略本机所有数据，只采纳远端内容，其余丢弃',
  },
  {
    value: 'local',
    label: '本机强制覆盖',
    hint: '忽略远端更新时间，全部保留本机数据',
  },
];

function fmtBytes(bytes: number | null | undefined): string {
  const n = Number(bytes || 0);
  if (n >= 1073741824) return (n / 1073741824).toFixed(2) + ' GB';
  if (n >= 1048576) return (n / 1048576).toFixed(2) + ' MB';
  if (n >= 1024) return (n / 1024).toFixed(2) + ' KB';
  return n + ' B';
}

function fmtTime(ms: number | null | undefined): string {
  const n = Number(ms || 0);
  if (!n) return '—';
  try {
    return new Date(n).toLocaleString();
  } catch {
    return '—';
  }
}

function truncateHash(hash: string): string {
  if (!hash) return '—';
  return hash.length > 16 ? `${hash.slice(0, 8)}…${hash.slice(-6)}` : hash;
}

export default function CloudSyncPanel(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();

  const [form, setForm] = useState<CloudSyncForm>(DEFAULT_FORM);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [testing, setTesting] = useState(false);
  const [syncing, setSyncing] = useState(false);
  const [pulling, setPulling] = useState(false);
  const [error, setError] = useState('');

  // 凭据位（界面不回显密码，只显示「已设置」）
  const [hasPassword, setHasPassword] = useState(false);
  const [hasSecretKey, setHasSecretKey] = useState(false);
  const [remotePath, setRemotePath] = useState('');

  // 运行状态面板
  const [status, setStatus] = useState<SyncStatusData | null>(null);
  const [statusLoading, setStatusLoading] = useState(false);
  // 上次手动同步返回的哈希（用于「哈希是否变化」对比）
  const [lastSyncHash, setLastSyncHash] = useState<string | null>(null);

  // 连通性测试结果
  const [testResult, setTestResult] = useState<{ ok: boolean; message: string } | null>(null);

  // 云端恢复弹窗
  const [pullOpen, setPullOpen] = useState(false);
  const [pullMode, setPullMode] = useState<PullMode>('merge');
  const [pullError, setPullError] = useState('');

  // ── 加载配置 ──
  const loadConfig = async (): Promise<void> => {
    setLoading(true);
    setError('');
    try {
      const res = await syncApi.getSyncConfig();
      const d = res?.data as SyncConfigData | undefined;
      if (d) {
        setForm({
          enabled: Boolean(d.enabled),
          backend: d.backend === 's3' ? 's3' : 'webdav',
          interval_secs: String(d.interval_secs ?? ''),
          min_interval_secs: String(d.min_interval_secs ?? ''),
          user_agent: d.user_agent ?? '',
          path: d.path ?? '',
          webdav_url: d.webdav?.url ?? '',
          webdav_username: d.webdav?.username ?? '',
          webdav_password: '',
          s3_endpoint: d.s3?.endpoint ?? '',
          s3_bucket: d.s3?.bucket ?? '',
          s3_region: d.s3?.region ?? '',
          s3_access_key: d.s3?.access_key ?? '',
          s3_secret_key: '',
          s3_prefix: d.s3?.prefix ?? '',
        });
        setHasPassword(Boolean(d.webdav?.has_password));
        setHasSecretKey(Boolean(d.s3?.has_secret_key));
        setRemotePath(d.remote_path ?? '');
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  };

  // ── 加载运行状态 ──
  const loadStatus = async (): Promise<void> => {
    setStatusLoading(true);
    try {
      const res = await syncApi.getSyncStatus();
      setStatus((res?.data as SyncStatusData | undefined) ?? null);
    } catch {
      // 状态加载失败不阻塞设置表单
    } finally {
      setStatusLoading(false);
    }
  };

  useEffect(() => {
    void loadConfig();
    void loadStatus();
  }, []);

  // ── 表单字段更新 ──
  const updateField = <K extends keyof CloudSyncForm>(field: K, value: CloudSyncForm[K]): void => {
    setForm((prev) => ({ ...prev, [field]: value }));
  };

  // ── 构建保存 / 测试请求体（密码留空 = 沿用旧值） ──
  const buildPayload = (): SyncSaveInput => ({
    enabled: form.enabled,
    backend: form.backend,
    interval_secs: Number(form.interval_secs) || 0,
    min_interval_secs: Number(form.min_interval_secs) || 0,
    user_agent: form.user_agent,
    path: form.path,
    webdav: {
      url: form.webdav_url,
      username: form.webdav_username,
      password: form.webdav_password,
    },
    s3: {
      endpoint: form.s3_endpoint,
      bucket: form.s3_bucket,
      region: form.s3_region,
      access_key: form.s3_access_key,
      secret_key: form.s3_secret_key,
      prefix: form.s3_prefix,
    },
  });

  // ── 保存前校验（与后端 validate 对齐；后端仍会再校验一次） ──
  const validate = (): string | null => {
    const interval = Number(form.interval_secs);
    if (!form.interval_secs.trim() || Number.isNaN(interval) || interval < 10 || interval > 86400) {
      return t('自动同步间隔必须在 10–86400 秒之间');
    }
    const minInterval = Number(form.min_interval_secs);
    if (
      form.min_interval_secs.trim() &&
      !Number.isNaN(minInterval) &&
      (minInterval < 0 || minInterval > 3600)
    ) {
      return t('最小同步间隔必须在 0–3600 秒之间');
    }
    if (!form.path.trim()) {
      return t('同步路径不能为空');
    }
    if (form.backend === 'webdav') {
      const url = form.webdav_url.trim();
      if (!url) return t('WebDAV 地址不能为空');
      try {
        const parsed = new URL(url);
        if (parsed.protocol !== 'http:' && parsed.protocol !== 'https:') {
          return t('WebDAV 只支持 http/https 地址');
        }
      } catch {
        return t('WebDAV 地址无效');
      }
    } else {
      if (!form.s3_bucket.trim()) return t('S3 存储桶不能为空');
      if (!form.s3_region.trim()) return t('S3 区域不能为空');
      if (!form.s3_access_key.trim()) return t('S3 Access Key 不能为空');
      if (!form.s3_secret_key.trim() && !hasSecretKey) return t('S3 Secret Key 不能为空');
    }
    return null;
  };

  // ── 保存 ──
  const handleSave = async (): Promise<void> => {
    const validationError = validate();
    if (validationError) {
      setError(validationError);
      return;
    }
    setSaving(true);
    setError('');
    try {
      const res = await syncApi.saveSyncConfig(buildPayload());
      const d = res?.data as SyncConfigData | undefined;
      if (d) {
        setHasPassword(Boolean(d.webdav?.has_password));
        setHasSecretKey(Boolean(d.s3?.has_secret_key));
        setRemotePath(d.remote_path ?? '');
        // 密码字段用后即清（不留在页面状态里）
        setForm((prev) => ({ ...prev, webdav_password: '', s3_secret_key: '' }));
      }
      addToast(t('云同步设置已保存'));
      await loadStatus();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  };

  // ── 连通性测试 ──
  const handleTest = async (): Promise<void> => {
    setTesting(true);
    setError('');
    setTestResult(null);
    try {
      const res = await syncApi.testSyncConnection(buildPayload());
      const d = res?.data as { remote_path?: string } | undefined;
      setTestResult({
        ok: true,
        message: t('连通性测试成功：远端路径 {{path}}', { path: d?.remote_path ?? remotePath }),
      });
      addToast(t('连通性测试成功'));
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      setTestResult({ ok: false, message: msg });
      setError(msg);
    } finally {
      setTesting(false);
    }
  };

  // ── 立即同步 ──
  const handleSyncNow = async (): Promise<void> => {
    setSyncing(true);
    setError('');
    try {
      const res = await syncApi.syncNow(false);
      const d = res?.data as { status?: string; bytes?: number; entries?: number; content_hash?: string; reason?: string } | undefined;
      if (d) {
        setLastSyncHash(d.content_hash ?? null);
        if (d.status === 'uploaded') {
          addToast(
            t('同步成功：{{bytes}}，{{entries}} 个条目', {
              bytes: fmtBytes(d.bytes),
              entries: d.entries ?? 0,
            }),
          );
        } else {
          addToast(t('同步已跳过：{{reason}}', { reason: d.reason ?? '' }));
        }
      }
      await loadStatus();
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      setError(msg);
      addToast(msg, 'error');
    } finally {
      setSyncing(false);
    }
  };

  // ── 从云端恢复（危险操作） ──
  const openPullDialog = (): void => {
    setPullMode('merge');
    setPullError('');
    setPullOpen(true);
  };

  const handlePull = async (): Promise<void> => {
    setPulling(true);
    setPullError('');
    try {
      const res = await syncApi.syncPull(pullMode);
      const d = res?.data as { written?: number; content_hash?: string } | undefined;
      if (d) {
        setLastSyncHash(d.content_hash ?? null);
        addToast(t('云端恢复完成：写入 {{written}} 键', { written: d.written ?? 0 }));
      }
      setPullOpen(false);
      await loadStatus();
    } catch (err) {
      // 失败时保持弹窗打开，让用户看到错误或重试
      setPullError(err instanceof Error ? err.message : String(err));
    } finally {
      setPulling(false);
    }
  };

  // ── 状态面板：哈希变化检测（与上次手动同步结果对比） ──
  const hashChanged =
    lastSyncHash !== null && Boolean(status?.last_hash) && status?.last_hash !== lastSyncHash;

  if (loading) {
    return (
      <div className="card">
        <div className="card-body">
          <SkeletonList rows={6} />
        </div>
      </div>
    );
  }

  return (
    <>
      <div className="card">
        <div className="card-header">
          <h2>{t('云同步设置')}</h2>
          <p className="card-subtitle">
            {t('把本机数据快照自动同步到 WebDAV / S3 远端，多台 AIGX 之间保持数据一致。')}
          </p>
        </div>
        <div className="card-body">
          {error && <div className="error-message">{error}</div>}

          <div className="settings-form">
            {/* 总开关 */}
            <div className="form-group">
              <label style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
                <Switch
                  checked={form.enabled}
                  onChange={(v) => updateField('enabled', v)}
                />
                {t('启用云同步')}
              </label>
              <span className="form-hint">
                {t('启用后按间隔自动把本机数据快照同步到远端；关闭后手动按钮不可用。')}
              </span>
            </div>

            {/* 后端类型切换 */}
            <div className="form-group">
              <label>{t('存储后端')}</label>
              <div style={{ display: 'flex', gap: 8 }}>
                <button
                  type="button"
                  className={form.backend === 'webdav' ? 'btn btn-primary' : 'btn btn-outline'}
                  onClick={() => updateField('backend', 'webdav')}
                >
                  {t('WebDAV')}
                </button>
                <button
                  type="button"
                  className={form.backend === 's3' ? 'btn btn-primary' : 'btn btn-outline'}
                  onClick={() => updateField('backend', 's3')}
                >
                  {t('S3 兼容存储')}
                </button>
              </div>
            </div>

            {/* WebDAV 字段 */}
            {form.backend === 'webdav' && (
              <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fill, minmax(240px, 1fr))', gap: 16 }}>
                <div className="form-group">
                  <label>{t('WebDAV 地址')}</label>
                  <input
                    className="form-input"
                    placeholder="https://dav.example.com/dav"
                    value={form.webdav_url}
                    onChange={(e) => updateField('webdav_url', e.target.value)}
                    spellCheck={false}
                  />
                  <span className="form-hint">{t('例如 https://dav.example.com/dav')}</span>
                </div>
                <div className="form-group">
                  <label>{t('WebDAV 用户名')}</label>
                  <input
                    className="form-input"
                    placeholder="username"
                    value={form.webdav_username}
                    onChange={(e) => updateField('webdav_username', e.target.value)}
                    autoComplete="off"
                  />
                  <span className="form-hint">{t('留空 = 匿名访问')}</span>
                </div>
                <div className="form-group">
                  <label>{t('WebDAV 密码')}</label>
                  <input
                    className="form-input"
                    type="password"
                    autoComplete="new-password"
                    placeholder={hasPassword ? t('已设置密码，留空 = 沿用旧值') : t('留空 = 沿用旧值')}
                    value={form.webdav_password}
                    onChange={(e) => updateField('webdav_password', e.target.value)}
                  />
                  <span className="form-hint">
                    {hasPassword ? t('已设置密码，留空 = 沿用旧值') : t('尚未设置密码')}
                  </span>
                </div>
              </div>
            )}

            {/* S3 字段 */}
            {form.backend === 's3' && (
              <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fill, minmax(240px, 1fr))', gap: 16 }}>
                <div className="form-group">
                  <label>{t('S3 Endpoint')}</label>
                  <input
                    className="form-input"
                    placeholder="https://s3.example.com"
                    value={form.s3_endpoint}
                    onChange={(e) => updateField('s3_endpoint', e.target.value)}
                    spellCheck={false}
                  />
                  <span className="form-hint">{t('留空 = AWS 官方')}</span>
                </div>
                <div className="form-group">
                  <label>{t('S3 存储桶')}</label>
                  <input
                    className="form-input"
                    placeholder="my-bucket"
                    value={form.s3_bucket}
                    onChange={(e) => updateField('s3_bucket', e.target.value)}
                    spellCheck={false}
                  />
                </div>
                <div className="form-group">
                  <label>{t('S3 区域')}</label>
                  <input
                    className="form-input"
                    placeholder="us-east-1"
                    value={form.s3_region}
                    onChange={(e) => updateField('s3_region', e.target.value)}
                    spellCheck={false}
                  />
                  <span className="form-hint">{t('Cloudflare R2 填 auto')}</span>
                </div>
                <div className="form-group">
                  <label>{t('S3 Access Key')}</label>
                  <input
                    className="form-input"
                    placeholder="AKIA..."
                    value={form.s3_access_key}
                    onChange={(e) => updateField('s3_access_key', e.target.value)}
                    autoComplete="off"
                    spellCheck={false}
                  />
                </div>
                <div className="form-group">
                  <label>{t('S3 Secret Key')}</label>
                  <input
                    className="form-input"
                    type="password"
                    autoComplete="new-password"
                    placeholder={hasSecretKey ? t('已设置 Secret，留空 = 沿用旧值') : t('留空 = 沿用旧值')}
                    value={form.s3_secret_key}
                    onChange={(e) => updateField('s3_secret_key', e.target.value)}
                  />
                  <span className="form-hint">
                    {hasSecretKey ? t('已设置 Secret，留空 = 沿用旧值') : t('尚未设置 Secret')}
                  </span>
                </div>
                <div className="form-group">
                  <label>{t('S3 对象前缀')}</label>
                  <input
                    className="form-input"
                    placeholder="backups"
                    value={form.s3_prefix}
                    onChange={(e) => updateField('s3_prefix', e.target.value)}
                    spellCheck={false}
                  />
                  <span className="form-hint">{t('可选，与同步路径拼接')}</span>
                </div>
              </div>
            )}

            {/* 同步路径 */}
            <div className="form-group">
              <label>{t('同步路径')}</label>
              <input
                className="form-input"
                placeholder="aigx-sync"
                value={form.path}
                onChange={(e) => updateField('path', e.target.value)}
                spellCheck={false}
              />
              <span className="form-hint">
                {remotePath
                  ? t('远端路径显示：{{path}}', { path: remotePath })
                  : t('远端目录名（WebDAV 相对路径 / S3 键路径）')}
              </span>
            </div>

            {/* 间隔字段 */}
            <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fill, minmax(200px, 1fr))', gap: 16 }}>
              <div className="form-group">
                <label>{t('自动同步间隔（秒）')}</label>
                <input
                  className="form-input"
                  type="number"
                  min="10"
                  max="86400"
                  placeholder="3600"
                  value={form.interval_secs}
                  onChange={(e) => updateField('interval_secs', e.target.value)}
                />
                <span className="form-hint">{t('10–86400 秒')}</span>
              </div>
              <div className="form-group">
                <label>{t('最小同步间隔（秒）')}</label>
                <input
                  className="form-input"
                  type="number"
                  min="0"
                  max="3600"
                  placeholder="60"
                  value={form.min_interval_secs}
                  onChange={(e) => updateField('min_interval_secs', e.target.value)}
                />
                <span className="form-hint">{t('两次成功同步之间的硬下限，0 = 不限')}</span>
              </div>
            </div>

            {/* 自定义 User-Agent */}
            <div className="form-group">
              <label>{t('自定义 User-Agent')}</label>
              <input
                className="form-input"
                placeholder="AIGX-CloudSync/1.0"
                value={form.user_agent}
                onChange={(e) => updateField('user_agent', e.target.value)}
                spellCheck={false}
              />
              <span className="form-hint">{t('留空 = 使用默认')}</span>
            </div>

            {/* 操作区 */}
            <div className="settings-actions" style={{ display: 'flex', gap: 8, flexWrap: 'wrap', marginTop: 16 }}>
              <button className="btn btn-primary" onClick={() => void handleSave()} disabled={saving}>
                {saving ? t('保存中...') : t('保存设置')}
              </button>
              <button className="btn btn-secondary" onClick={() => void handleTest()} disabled={testing}>
                {testing ? t('测试中...') : t('测试连通性')}
              </button>
              <button
                className="btn btn-outline"
                onClick={() => void handleSyncNow()}
                disabled={syncing || !form.enabled}
                title={!form.enabled ? t('请先启用云同步') : undefined}
              >
                {syncing ? t('同步中...') : t('立即同步')}
              </button>
              <button
                className="btn btn-danger"
                onClick={openPullDialog}
                disabled={pulling || !form.enabled}
                title={!form.enabled ? t('请先启用云同步') : undefined}
              >
                {pulling ? t('恢复中...') : t('从云端恢复')}
              </button>
            </div>

            {/* 连通性测试结果 */}
            {testResult && (
              <div
                style={{
                  marginTop: 12,
                  padding: 10,
                  borderRadius: 6,
                  fontSize: 13,
                  fontWeight: 600,
                  color: testResult.ok ? 'var(--text-main)' : 'var(--error)',
                  background: testResult.ok ? 'var(--card-bg)' : undefined,
                }}
              >
                {testResult.message}
              </div>
            )}
          </div>
        </div>
      </div>

      {/* 运行状态面板 */}
      <div className="card" style={{ marginTop: 16 }}>
        <div className="card-header">
          <h2>{t('运行状态')}</h2>
        </div>
        <div className="card-body">
          {statusLoading ? (
            <SkeletonList rows={3} />
          ) : status ? (
            <div className="settings-form">
              <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fill, minmax(200px, 1fr))', gap: 16 }}>
                <div className="form-group">
                  <label>{t('上次成功时间')}</label>
                  <div style={{ fontSize: 14, fontWeight: 600, color: 'var(--text-main)' }}>
                    {status.last_success_at > 0 ? fmtTime(status.last_success_at) : t('从未')}
                  </div>
                </div>
                <div className="form-group">
                  <label>{t('上次尝试时间')}</label>
                  <div style={{ fontSize: 14, fontWeight: 600, color: 'var(--text-main)' }}>
                    {status.last_attempt_at > 0 ? fmtTime(status.last_attempt_at) : t('从未')}
                  </div>
                </div>
                <div className="form-group">
                  <label>{t('上次错误')}</label>
                  <div
                    style={{
                      fontSize: 14,
                      fontWeight: 600,
                      color: status.last_error ? 'var(--error)' : 'var(--text-main)',
                      wordBreak: 'break-all',
                    }}
                  >
                    {status.last_error || t('无')}
                  </div>
                </div>
                <div className="form-group">
                  <label>{t('连续失败次数')}</label>
                  <div
                    style={{
                      fontSize: 20,
                      fontWeight: 600,
                      color: status.consecutive_failures > 0 ? 'var(--error)' : 'var(--text-main)',
                    }}
                  >
                    {status.consecutive_failures}
                  </div>
                </div>
                <div className="form-group">
                  <label>{t('内容哈希')}</label>
                  <div style={{ fontSize: 14, fontWeight: 600, color: 'var(--text-main)', fontFamily: 'monospace' }}>
                    {truncateHash(status.last_hash)}
                  </div>
                  <span className="form-hint">
                    {status.last_success_at > 0 ? t('已同步') : t('未同步过')}
                    {hashChanged ? ' · ' + t('自上次同步后哈希有变化') : ''}
                  </span>
                </div>
                <div className="form-group">
                  <label>{t('下次同步时间')}</label>
                  <div style={{ fontSize: 14, fontWeight: 600, color: 'var(--text-main)' }}>
                    {status.next_run_at > 0 ? fmtTime(status.next_run_at) : t('未排期')}
                  </div>
                </div>
              </div>
              <div className="settings-actions">
                <button className="btn btn-outline" onClick={() => void loadStatus()} disabled={statusLoading}>
                  {t('刷新状态')}
                </button>
              </div>
            </div>
          ) : (
            <div className="empty-state">
              <p>{t('状态信息加载失败')}</p>
            </div>
          )}
        </div>
      </div>

      {/* 云端恢复弹窗（危险操作 + 合并策略） */}
      {pullOpen && (
        <div
          className="modal-overlay"
          onClick={() => !pulling && setPullOpen(false)}
          role="dialog"
          aria-modal="true"
          aria-labelledby="pull-dialog-title"
        >
          <div className="modal" style={{ maxWidth: 480, width: '90%' }} onClick={(e) => e.stopPropagation()}>
            <div className="modal-header">
              <h3 id="pull-dialog-title">{t('从云端恢复')}</h3>
              <button
                className="modal-close"
                onClick={() => !pulling && setPullOpen(false)}
                aria-label={t('关闭')}
              >
                &times;
              </button>
            </div>
            <div className="modal-body" style={{ fontSize: 13.5, lineHeight: 1.6, color: 'var(--text-main)' }}>
              <div className="error-message" style={{ marginBottom: 12 }}>
                {t('此操作会从远端拉取数据并写回本机，不可逆。请选择合并策略：')}
              </div>
              <div className="form-group">
                <label>{t('合并策略')}</label>
                <div style={{ display: 'flex', flexDirection: 'column', gap: 10 }}>
                  {PULL_MODES.map((m) => (
                    <label key={m.value} style={{ display: 'flex', alignItems: 'flex-start', gap: 8, cursor: 'pointer' }}>
                      <input
                        type="radio"
                        name="pull-mode"
                        checked={pullMode === m.value}
                        onChange={() => setPullMode(m.value)}
                        disabled={pulling}
                      />
                      <span>
                        <strong>{t(m.label)}</strong>
                        <span className="form-hint" style={{ display: 'block', marginTop: 2 }}>
                          {t(m.hint)}
                        </span>
                      </span>
                    </label>
                  ))}
                </div>
              </div>
              {pullError && <div className="error-message" style={{ marginTop: 10 }}>{pullError}</div>}
            </div>
            <div className="modal-footer">
              <button className="btn btn-outline" onClick={() => setPullOpen(false)} disabled={pulling}>
                {t('取消')}
              </button>
              <button className="btn btn-danger" onClick={() => void handlePull()} disabled={pulling}>
                {pulling ? t('恢复中...') : t('确认恢复')}
              </button>
            </div>
          </div>
        </div>
      )}
    </>
  );
}
