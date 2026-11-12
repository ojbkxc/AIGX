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
 * - 加密备份导出：口令加密包（AIGXBAK1）。明文导出端点因安全整改已移除
 *   （生产实测 404，不要恢复），本页走 export-encrypted → download
 *   三段式；口令只经请求体，不落盘、不写日志、不进 localStorage
 * - 备份导入：明文 JSON 旧包走 /api/backup/import；加密包走
 *   /api/backup/import-encrypted（multipart：file + password）。后端都会先把
 *   当前全量快照到 backups/ 再写回，导入坏了还能回滚
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
  const [exportPassword, setExportPassword] = useState('');
  const [exportPasswordConfirm, setExportPasswordConfirm] = useState('');
  const [importPassword, setImportPassword] = useState('');
  const [importing, setImporting] = useState(false);
  const [encryptedImporting, setEncryptedImporting] = useState(false);
  const [confirmState, setConfirmState] = useState<ConfirmState | null>(null);
  const pendingImport = useRef<Record<string, unknown> | null>(null);
  const pendingEncrypted = useRef<{ file: File; password: string } | null>(null);
  const fileInput = useRef<HTMLInputElement | null>(null);
  const encryptedFileInput = useRef<HTMLInputElement | null>(null);

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

  // 字符数口径与后端 `password.chars().count()` 对齐（.length 数 UTF-16 单元，emoji 会多算）
  const pwChars = (s: string): number => [...s].length;

  // 加密导出三段式：POST export-encrypted（生成服务端包）→ 取 data.file →
  // GET download?file=…（带认证头取 blob）→ a.download 触发浏览器下载。
  // 口令只活在本函数栈与请求体里：不打日志、不进 localStorage、成功后清空。
  const exportEncrypted = async (): Promise<void> => {
    if (pwChars(exportPassword) < 8) {
      setError(t('备份口令至少 8 个字符（丢失口令=备份不可解）'));
      return;
    }
    if (exportPassword !== exportPasswordConfirm) {
      setError(t('两次输入的口令不一致'));
      return;
    }
    setExporting(true);
    setError('');
    try {
      const res = await api.exportEncryptedBackup(exportPassword, includeLogs);
      const info = res?.data;
      if (!info?.file) {
        throw new Error(res?.error?.message || t('导出失败：服务端未返回备份文件名'));
      }
      const blob = await api.downloadBackup(info.file);
      const url = URL.createObjectURL(blob);
      const a = document.createElement('a');
      a.href = url;
      a.download = info.file;
      a.click();
      // Safari/Firefox 同帧 revoke 会取消下载，延迟释放
      setTimeout(() => URL.revokeObjectURL(url), 1000);
      addToast(
        t('加密备份已导出（{{count}} 键，{{size}}）', {
          count: info.records ?? 0,
          size: fmtBytes(info.bytes),
        }),
      );
      // 口令用完即弃：清空输入框，不在页面状态里留痕
      setExportPassword('');
      setExportPasswordConfirm('');
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

  // 加密备份导入：multipart（file + password）→ 后端先解密全量校验再写回。
  // 口令只进本次 FormData；成功后清空，不留页面状态。
  const onEncryptedFilePicked = async (file: File): Promise<void> => {
    setError('');
    if (pwChars(importPassword) < 8) {
      setError(t('备份口令至少 8 个字符'));
      if (encryptedFileInput.current) encryptedFileInput.current.value = '';
      return;
    }
    pendingEncrypted.current = { file, password: importPassword };
    setConfirmState({
      title: t('导入加密备份'),
      message: t(
        '将恢复加密包 {{name}} 中的全部键（合并覆盖同名键）。导入前会自动把当前数据快照到服务器 backups/ 目录；口令错误或包被篡改会在写回之前失败。继续？',
        { name: file.name },
      ),
      confirmText: t('开始导入'),
      danger: true,
      onConfirm: async () => {
        setEncryptedImporting(true);
        try {
          const pending = pendingEncrypted.current;
          if (!pending) return;
          const res = await api.importEncryptedBackup(pending.file, pending.password);
          const d = res?.data as { written?: number; skipped?: number } | undefined;
          addToast(
            t('加密备份导入完成：写入 {{written}} 键，跳过 {{skipped}}', {
              written: d?.written ?? 0,
              skipped: d?.skipped ?? 0,
            }),
          );
          setImportPassword('');
          await load();
        } catch (err) {
          addToast(err instanceof Error ? err.message : String(err), 'error');
        } finally {
          setEncryptedImporting(false);
          setConfirmState(null);
          pendingEncrypted.current = null;
          if (encryptedFileInput.current) encryptedFileInput.current.value = '';
        }
      },
    });
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
          {t('存储概览与加密备份/恢复；加密包=AIGXBAK1 口令加密的全量键值快照，明文导出已下线，导入前自动快照当前数据。')}
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
          <label>{t('导出加密备份')}</label>
          <input
            className="form-input"
            type="password"
            autoComplete="new-password"
            placeholder={t('设置备份口令（至少 8 个字符，丢失后备份不可解）')}
            value={exportPassword}
            disabled={exporting}
            onChange={(e) => setExportPassword(e.target.value)}
          />
          <input
            className="form-input"
            type="password"
            autoComplete="new-password"
            placeholder={t('再次输入备份口令')}
            value={exportPasswordConfirm}
            disabled={exporting}
            onChange={(e) => setExportPasswordConfirm(e.target.value)}
            style={{ marginTop: 8 }}
          />
          <span className="form-hint" style={{ marginTop: 6 }}>
            {t('口令只随本次请求发往后端，不写日志、不存浏览器；请自行妥善保管，丢失无法恢复。')}
          </span>
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
            onClick={() => void exportEncrypted()}
            disabled={
              exporting || pwChars(exportPassword) < 8 || exportPassword !== exportPasswordConfirm
            }
          >
            {exporting ? t('导出中...') : t('导出加密备份')}
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

        <div className="form-group" style={{ marginTop: 16 }}>
          <label>{t('导入加密备份')}</label>
          <input
            className="form-input"
            type="password"
            autoComplete="current-password"
            placeholder={t('输入加密包口令')}
            value={importPassword}
            disabled={encryptedImporting}
            onChange={(e) => setImportPassword(e.target.value)}
          />
          <span className="form-hint" style={{ marginTop: 6 }}>
            {t('选择 .aigxbak 加密包后，用该包导出时设置的口令解密恢复。')}
          </span>
          <div style={{ marginTop: 8 }}>
            <button
              className="btn btn-secondary"
              onClick={() => encryptedFileInput.current?.click()}
              disabled={encryptedImporting || pwChars(importPassword) < 8}
            >
              {encryptedImporting ? t('读取中...') : t('选择加密包并导入')}
            </button>
            <input
              ref={encryptedFileInput}
              type="file"
              accept=".aigxbak,application/octet-stream"
              style={{ display: 'none' }}
              onChange={(e) => {
                const f = e.target.files?.[0];
                if (f) void onEncryptedFilePicked(f);
              }}
            />
          </div>
        </div>
      </div>

      <ConfirmDialog state={confirmState} onClose={() => setConfirmState(null)} />
    </div>
  );
}
