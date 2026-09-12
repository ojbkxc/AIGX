import { useEffect } from 'react';
import { useTranslation } from 'react-i18next';
import type { RequestLogItem, AuditLogItem } from '../pages/Logs';

/** 请求日志详情弹窗：全字段结构化展示（参照 new-api DetailsDialog） */
export function RequestLogDetail({ log, admin, onClose }: {
  log: RequestLogItem;
  admin: boolean;
  onClose: () => void;
}): JSX.Element {
  const { t } = useTranslation();

  useEffect(() => {
    const onKey = (e: KeyboardEvent): void => { if (e.key === 'Escape') onClose(); };
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, [onClose]);

  const rows: Array<{ label: string; value: string; mono?: boolean }> = [
    { label: t('请求 ID'), value: String(log.id), mono: true },
    { label: t('时间'), value: log.created_at ? new Date(log.created_at * 1000).toLocaleString() : '—' },
    ...(admin ? [{ label: t('用户'), value: log.user_email || log.user_id || '—' }] : []),
    { label: t('模型（映射后）'), value: log.model || '—', mono: true },
    ...(admin ? [{ label: t('原始模型名'), value: log.origin_model || '—', mono: true }] : []),
    ...(admin ? [{ label: t('渠道'), value: log.channel_name ? `${log.channel_name}${log.channel_id ? ` (#${log.channel_id})` : ''}` : (log.channel_id ? `#${log.channel_id}` : '—') }] : []),
    { label: t('输入 Tokens'), value: String(log.input_tokens ?? 0) },
    { label: t('输出 Tokens'), value: String(log.output_tokens ?? 0) },
    { label: t('费用'), value: `¥${log.cost ?? 0}` },
    ...(admin ? [
      { label: t('渠道成本'), value: `¥${log.channel_cost ?? log.cost ?? 0}` },
      { label: t('利润'), value: log.channel_cost != null ? `¥${(log.cost ?? 0) - log.channel_cost}` : '—' },
    ] : []),
    { label: t('延迟'), value: `${log.latency_ms ?? 0}ms` },
    { label: t('状态码'), value: String(log.status_code ?? '—') },
    ...(admin && log.key_id ? [{ label: 'API Key ID', value: log.key_id, mono: true }] : []),
    ...(admin && log.ip ? [{ label: 'IP', value: log.ip, mono: true }] : []),
  ];

  return (
    <LogDetailShell title={t('请求日志详情')} onClose={onClose}>
      <div className="log-detail-grid">
        {rows.map((r) => (
          <div key={r.label} className="log-detail-row">
            <span className="log-detail-label">{r.label}</span>
            <span className={`log-detail-value ${r.mono ? 'log-detail-mono' : ''}`}>{r.value}</span>
          </div>
        ))}
      </div>
      {log.error_msg && (
        <div className="log-detail-error">
          <div className="log-detail-label">{t('错误信息')}</div>
          <pre className="log-detail-pre">{log.error_msg}</pre>
        </div>
      )}
    </LogDetailShell>
  );
}

/** 审计日志详情弹窗：操作上下文 + before/after JSON diff */
export function AuditLogDetail({ log, onClose }: {
  log: AuditLogItem;
  onClose: () => void;
}): JSX.Element {
  const { t } = useTranslation();

  useEffect(() => {
    const onKey = (e: KeyboardEvent): void => { if (e.key === 'Escape') onClose(); };
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, [onClose]);

  const parse = (s: string | undefined): Record<string, unknown> | null => {
    if (!s) return null;
    try { return JSON.parse(s) as Record<string, unknown>; } catch { return null; }
  };
  const before = parse(log.before);
  const after = parse(log.after);

  /** 变更字段集合：值不同的 key（diff 高亮） */
  const changedKeys = new Set<string>();
  if (before && after) {
    const keys = new Set([...Object.keys(before), ...Object.keys(after)]);
    for (const k of keys) {
      if (JSON.stringify(before[k]) !== JSON.stringify(after[k])) changedKeys.add(k);
    }
  }

  return (
    <LogDetailShell title={t('审计日志详情')} onClose={onClose}>
      <div className="log-detail-grid">
        <div className="log-detail-row"><span className="log-detail-label">{t('时间')}</span><span className="log-detail-value">{log.created_at ? new Date(log.created_at * 1000).toLocaleString() : '—'}</span></div>
        <div className="log-detail-row"><span className="log-detail-label">{t('管理员')}</span><span className="log-detail-value">{log.admin_email || log.admin_id || '—'}</span></div>
        <div className="log-detail-row"><span className="log-detail-label">{t('操作')}</span><span className="log-detail-value"><code style={{ background: 'var(--card-bg)', padding: '2px 6px', borderRadius: 4 }}>{log.action}</code></span></div>
        <div className="log-detail-row"><span className="log-detail-label">{t('目标')}</span><span className="log-detail-value log-detail-mono">{log.target || '—'}</span></div>
      </div>
      {changedKeys.size > 0 && (
        <div className="log-detail-changes">
          <div className="log-detail-label">{t('变更字段')}（{changedKeys.size}）</div>
          <div className="log-detail-change-list">
            {Array.from(changedKeys).map((k) => (
              <span key={k} className="log-detail-change-badge">{k}</span>
            ))}
          </div>
        </div>
      )}
      <div className="log-detail-diff">
        <div className="log-detail-diff-col">
          <div className="log-detail-label">{t('变更前')}</div>
          <pre className="log-detail-pre">{before ? JSON.stringify(before, null, 2) : (log.before || '—')}</pre>
        </div>
        <div className="log-detail-diff-col">
          <div className="log-detail-label">{t('变更后')}</div>
          <pre className="log-detail-pre">{after ? JSON.stringify(after, null, 2) : (log.after || '—')}</pre>
        </div>
      </div>
    </LogDetailShell>
  );
}

/** 弹窗外壳：遮罩 + modal（复用全局 .modal 样式），点遮罩/Esc 关闭 */
function LogDetailShell({ title, onClose, children }: {
  title: string;
  onClose: () => void;
  children: React.ReactNode;
}): JSX.Element {
  return (
    <div className="modal-overlay" onClick={(e) => { if (e.target === e.currentTarget) onClose(); }}>
      <div className="modal log-detail-modal" role="dialog" aria-label={title}>
        <div className="modal-header">
          <h3>{title}</h3>
          <button type="button" className="modal-close" onClick={onClose} aria-label="×">×</button>
        </div>
        <div className="modal-body">{children}</div>
      </div>
    </div>
  );
}
