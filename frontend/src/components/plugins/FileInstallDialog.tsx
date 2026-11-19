// 本地 zip 安装弹层（A2A `PluginFileInstallDialog` 的 AIGX 形态）。
//
// 契约：POST /api/plugins/install-file（原始 zip 字节；后端路由级放大 body limit）。
// 逐文件独立安装：单项失败不影响其余；账号与既有设置保留。
// 无预览能力（AIGX 安装链路不返回 preview）——选文件后直接安装并回显结果。

import { useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Loader2, Upload } from 'lucide-react';
import { api } from '../../api';
import { Button } from '../ui';
import './ProviderSettingsDialog.css';
import './PluginManagerPage.css';

const MIB = 1024 * 1024;

type RowState = 'ready' | 'installing' | 'installed' | 'failed';

interface FileRow {
  key: string;
  file: File;
  state: RowState;
  error?: string;
  /** 安装期失败（可重试）；选择期校验失败（不可重试） */
  retryable?: boolean;
}

export default function FileInstallDialog({
  open,
  maxPackBytes,
  onClose,
  onInstalled,
}: {
  open: boolean;
  maxPackBytes: number;
  onClose: () => void;
  /** 任一文件安装成功后的回调（调用方刷新列表） */
  onInstalled: () => void;
}): JSX.Element | null {
  const { t } = useTranslation();
  const input = useRef<HTMLInputElement>(null);
  const [rows, setRows] = useState<FileRow[]>([]);
  const [running, setRunning] = useState(false);
  const max = maxPackBytes > 0 ? maxPackBytes : 24 * MIB;

  const ready = rows.filter((row) => row.state === 'ready').length;
  const failed = rows.filter((row) => row.state === 'failed').length;
  const done = rows.filter((row) => row.state === 'installed' || row.state === 'failed').length;

  const addFiles = (files: File[]): void => {
    if (running || !files.length) return;
    setRows((prev) => {
      const next = [...prev];
      for (const file of files) {
        const duplicate = next.some(
          (row) => row.file.name === file.name && row.file.size === file.size && row.file.lastModified === file.lastModified,
        );
        if (duplicate) continue;
        const key = `${Date.now()}:${next.length}`;
        let error = '';
        if (!file.name.toLocaleLowerCase().endsWith('.zip')) error = t('只支持 .zip 插件包');
        else if (!file.size) error = t('文件是空的');
        else if (file.size > max) error = t('插件包不能超过 {{n}} MiB', { n: Math.round(max / MIB) });
        next.push(error ? { key, file, state: 'failed', error } : { key, file, state: 'ready' });
      }
      return next;
    });
  };

  const start = async (retry = false): Promise<void> => {
    if (running) return;
    const pending = rows.filter((item) =>
      retry ? item.state === 'failed' && item.retryable === true : item.state === 'ready',
    );
    if (!pending.length) return;
    setRunning(true);
    try {
      for (const row of pending) {
        setRows((prev) =>
          prev.map((item) =>
            item.key === row.key ? { ...item, state: 'installing', error: undefined, retryable: false } : item,
          ),
        );
        try {
          await api.installPluginFile(row.file);
          setRows((prev) => prev.map((item) => (item.key === row.key ? { ...item, state: 'installed' } : item)));
          onInstalled();
        } catch (err) {
          const message = err instanceof Error ? err.message : String(err);
          setRows((prev) =>
            prev.map((item) =>
              item.key === row.key ? { ...item, state: 'failed', error: message, retryable: true } : item,
            ),
          );
        }
      }
    } finally {
      setRunning(false);
    }
  };

  const reset = (): void => {
    if (!running) setRows([]);
  };

  if (!open) return null;

  return (
    <div className="ps-overlay" role="dialog" aria-modal="true" aria-label={t('本地安装插件包')}>
      <div className="ps-panel">
        <div className="ps-head">
          <div>
            <h2>{t('本地安装插件包')}</h2>
            <p>{t('从本地 zip 安装插件包。单项失败不会中断其余文件；账号与原有设置均保留。')}</p>
          </div>
          <div className="ps-head-actions">
            <Button variant="outline" size="sm" onClick={onClose}>
              {running ? t('收起') : t('关闭')}
            </Button>
          </div>
        </div>
        <div className="ps-body">
          <input
            ref={input}
            type="file"
            multiple
            accept=".zip,application/zip"
            style={{ display: 'none' }}
            onChange={(event) => {
              const files = Array.from(event.currentTarget.files || []);
              event.currentTarget.value = '';
              addFiles(files);
            }}
          />
          <button
            type="button"
            className="pl-file-drop"
            disabled={running}
            onClick={() => input.current?.click()}
            onDragOver={(event) => event.preventDefault()}
            onDrop={(event) => {
              event.preventDefault();
              addFiles(Array.from(event.dataTransfer.files));
            }}
          >
            <Upload size={22} />
            <span>
              <span className="pl-file-drop-title">{t('拖入插件包，或选择文件')}</span>
              <span className="pl-file-drop-hint">
                {t('支持多个 zip · 单个不超过 {{n}} MiB', { n: Math.round(max / MIB) })}
              </span>
            </span>
          </button>

          <div className="pl-file-list">
            <div className="pl-file-list-head">
              <span>{t('已选 {{count}} 个文件', { count: rows.length })}</span>
              {!running && rows.length > 0 ? (
                <Button variant="outline" size="sm" onClick={reset}>
                  {t('清空列表')}
                </Button>
              ) : null}
            </div>
            {rows.map((row) => (
              <div key={row.key} className="pl-file-row">
                <div className="pl-file-main">
                  <p className="pl-file-name">{row.file.name}</p>
                  <p className="pl-file-meta">
                    {row.file.size >= MIB
                      ? `${(row.file.size / MIB).toFixed(1)} MiB`
                      : `${Math.max(1, Math.round(row.file.size / 1024))} KiB`}
                  </p>
                  {row.error ? <p className="pl-file-err">{row.error}</p> : null}
                </div>
                <span className={`pl-file-state is-${row.state}`}>
                  {row.state === 'installing' ? <Loader2 size={13} className="pl-spin" /> : null}
                  {row.state === 'ready'
                    ? t('待安装')
                    : row.state === 'installing'
                      ? t('正在安装…')
                      : row.state === 'installed'
                        ? t('已安装')
                        : t('安装失败')}
                </span>
              </div>
            ))}
            {rows.length === 0 ? (
              <p className="pl-empty-hint">{t('选择插件包后，文件清单会显示在这里')}</p>
            ) : null}
          </div>

          <div className="pl-file-foot">
            <span className="pl-file-summary">
              {running ? t('正在安装 {{done}} / {{total}}…', { done, total: done + ready }) : t('待安装 {{count}} 个插件包', { count: ready })}
            </span>
            <div className="pl-file-actions">
              {!running && failed > 0 ? (
                <Button variant="outline" size="sm" onClick={() => void start(true)}>
                  {t('重试失败项')}
                </Button>
              ) : null}
              <Button size="sm" disabled={running || ready === 0} onClick={() => void start()}>
                {running ? <Loader2 size={13} className="pl-spin" /> : null}
                {t('开始安装（{{count}}）', { count: ready })}
              </Button>
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}
