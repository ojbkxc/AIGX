import { useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api';
import { useToast } from '../components/Toast';
import ConfirmDialog, { type ConfirmState } from './ConfirmDialog';
import type { StorageOverviewItem } from '../types';

/**
 * 数据管理（系统设置 → 运维 → 数据）
 *
 * 对照 Agent2API「设置 → 数据」：
 * - 存储概览：KV 总键数、按业务分组的键数、库文件大小
 * - 备份导出：全量 KV 键值导出为 JSON（可选含请求/审计日志）
 * - 备份导入：上传备份 JSON 恢复；后端会先把当前全量快照到 backups/
 *   再写回，导入坏了还能回滚
 */

function fmtBytes(bytes: number | null | undefined): string {
  const n = Number(bytes || 0);
  if (n >= 1073741824) return (n / 1073741824).toFixed(2) + ' GB';
  if (n >= 1048576) return (n / 1048576).toFixed(2) + ' MB';
  if (n >= 1024) return (n / 1024).toFixed(1) + ' KB';
  return n + ' B';
}

export default function DataManagement(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  const [overview, setOverview] = useState<StorageOverviewItem | null>(null);
  const [includeLogs, setIncludeLogs] = useState(false);
  const [exporting, setExporting] = useState(false);
  const [importing, setImporting] = useState(false);
  const [confirmState, setConfirmState] = useState<ConfirmState | null>(null);
  const pendingImport = useRef<Record<string, unknown> | null>(null);
  const fileInput = useRef<HTMLInputElement | null>(null);

  const load = async (): Promise<void> => {
    try {
      const res = await api.getStorageOverview();
      setOverview((res?.data as StorageOverviewItem) ?? null);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    void load();
  }, []);

  const exportBackup = async (): Promise<void> => {
    setExporting(true);
    setError('');
    try {
      const doc = await api.exportBackup(includeLogs);
      const blob = new Blob([JSON.stringify(doc, null, 2)], {
        type: 'application/json',
      });
      const url = URL.createObjectURL(blob);
      const a = document.createElement('a');
      const ts = new Date().toISOString().slice(0, 19).replace(/[:T]/g, '-');
      a.href = url;
      a.download = `aigx-backup-${ts}${includeLogs ? '-with-logs' : ''}.json`;
      a.click();
      URL.revokeObjectURL(url);
      const count = (doc as { kv?: Record<string, unknown> })?.kv
        ? Object.keys((doc as { kv: Record<string, unknown> }).kv).length
        : 0;
      addToast(t('备份已导出（{{count}} 键）', { count }));
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setExporting(false);
    }
  };

  const onFilePicked = async (file: File): Promise<void> => {
    setImporting(true);
    setError('');
    try {
      const text = await file.text();
      const doc = JSON.parse(text) as Record<string, unknown>;
      const kvCount = doc.kv && typeof doc.kv === 'object'
        ? Object.keys(doc.kv as Record<string, unknown>).length
        : 0;
      if (!kvCount) {
        setError(t('备份文件里没有任何键值（kv 为空），不是有效的 AIGX 备份'));
        return;
      }
      pendingImport.current = doc;
      setConfirmState({
        title: t('导入备份'),
        message: t(
          '将写入 {{count}} 个键（合并覆盖同名键）。导入前会自动把当前数据快照到服务器 backups/ 目录。部分数据（供应商/用户等）需刷新页面后重新加载。继续？',
          { count: kvCount },
        ),
        confirmText: t('开始导入'),
        danger: true,
        onConfirm: async () => {
          try {
            const res = await api.importBackup(pendingImport.current ?? {});
            const d = res?.data as { written?: number; skipped?: number } | undefined;
            addToast(
              t('导入完成：写入 {{written}} 键，跳过 {{skipped}}', {
                written: d?.written ?? 0,
                skipped: d?.skipped ?? 0,
              }),
            );
            await load();
          } catch (err) {
            addToast(err instanceof Error ? err.message : String(err), 'error');
          } finally {
            setConfirmState(null);
            pendingImport.current = null;
          }
        },
      });
    } catch (err) {
      setError(
        t('解析备份文件失败') + '：' + (err instanceof Error ? err.message : String(err)),
      );
    } finally {
      setImporting(false);
      if (fileInput.current) fileInput.current.value = '';
    }
  };

  if (loading) {
    return <div className="loading">{t('加载中')}</div>;
  }

  const groups = Object.entries(overview?.groups ?? {});

  return (
    <div className="card">
      <div className="card-header">
        <h2>{t('数据管理')}</h2>
        <p className="card-subtitle">
          {t('存储概览与全量备份/恢复；备份=全部业务键值的一份 JSON 快照，导入前自动快照当前数据。')}
        </p>
      </div>
      <div className="card-body">
        {error && <div className="error-message">{error}</div>}

        <div className="form-group">
          <label>{t('存储概览')}</label>
          <div className="form-hint">
            {t('共 {{count}} 个键 · 数据库 {{size}}', {
              count: overview?.total_keys ?? 0,
              size: fmtBytes(overview?.db_size_bytes),
            })}
          </div>
          {overview?.data_dir && (
            <div className="form-hint" style={{ wordBreak: 'break-all' }}>
              {t('数据目录：{{dir}}（备份快照存于其下 backups/）', { dir: overview.data_dir })}
            </div>
          )}
          {groups.length > 0 && (
            <div style={{ display: 'flex', gap: 6, flexWrap: 'wrap', marginTop: 8 }}>
              {groups.map(([label, count]) => (
                <span
                  key={label}
                  className="badge"
                  style={{ padding: '2px 10px', fontSize: 12 }}
                >
                  {t(label)} {count}
                </span>
              ))}
            </div>
          )}
        </div>

        <div className="form-group">
          <label style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
            <input
              type="checkbox"
              checked={includeLogs}
              onChange={(e) => setIncludeLogs(e.target.checked)}
            />
            <span>{t('备份包含请求/审计日志（体积大，日常备份可不勾）')}</span>
          </label>
        </div>

        <div className="settings-actions" style={{ display: 'flex', gap: 8, flexWrap: 'wrap' }}>
          <button
            className="btn btn-primary"
            onClick={() => void exportBackup()}
            disabled={exporting}
          >
            {exporting ? t('导出中...') : t('导出备份')}
          </button>
          <button
            className="btn btn-secondary"
            onClick={() => fileInput.current?.click()}
            disabled={importing}
          >
            {importing ? t('读取中...') : t('导入备份')}
          </button>
          <input
            ref={fileInput}
            type="file"
            accept="application/json,.json"
            style={{ display: 'none' }}
            onChange={(e) => {
              const f = e.target.files?.[0];
              if (f) void onFilePicked(f);
            }}
          />
        </div>
      </div>

      <ConfirmDialog state={confirmState} onClose={() => setConfirmState(null)} />
    </div>
  );
}
