import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import type { RequestLogItem, AuditLogItem } from '../pages/Logs';
import { sourceLabel } from '../pages/Logs';
import { Tabs } from './ui';

/** 详情弹窗的视图分档：概览 / 对话 / 路由 / 原始报文 */
type DetailTab = 'overview' | 'conversation' | 'route' | 'raw';

/** 请求日志详情弹窗：全字段结构化展示（参照 new-api DetailsDialog） */
export function RequestLogDetail({ log, admin, onClose }: {
  log: RequestLogItem;
  admin: boolean;
  onClose: () => void;
}): JSX.Element {
  const { t } = useTranslation();
  const [tab, setTab] = useState<DetailTab>('overview');

  useEffect(() => {
    const onKey = (e: KeyboardEvent): void => { if (e.key === 'Escape') onClose(); };
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, [onClose]);

  const src = sourceLabel(log.key_id);
  const rows: Array<{ label: string; value: string; mono?: boolean }> = [
    { label: t('请求 ID'), value: String(log.id), mono: true },
    { label: t('时间'), value: log.created_at ? new Date(log.created_at * 1000).toLocaleString() : '—' },
    ...(admin ? [{ label: t('用户'), value: log.user_email || log.user_id || '—' }] : []),
    ...(admin && src ? [{ label: t('来源'), value: src }] : []),
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
    ...(admin && log.key_id && !src ? [{ label: 'API Key ID', value: log.key_id, mono: true }] : []),
    ...(admin && log.ip ? [{ label: 'IP', value: log.ip, mono: true }] : []),
  ];

  // 分档可见性：后三档都是 admin 专属数据（后端对普通用户已剥离），
  // 没有数据就不给空 Tab，避免用户点进去看到一片空白。
  const hasConv = admin && !!log.conversation && (log.conversation?.messages.length ?? 0) > 0;
  const hasRoute =
    admin &&
    !!(log.candidate_channels?.length || log.filtered_channels?.length || log.selected_channel);
  const hasRaw = admin && !!log.debug;

  const tabs: Array<{ key: DetailTab; label: string }> = [
    { key: 'overview', label: t('概览') },
    ...(hasConv ? [{ key: 'conversation' as DetailTab, label: t('对话内容') }] : []),
    ...(hasRoute ? [{ key: 'route' as DetailTab, label: t('路由回放') }] : []),
    ...(hasRaw ? [{ key: 'raw' as DetailTab, label: t('原始报文') }] : []),
  ];
  // 数据在弹窗打开后不会再被替换；但切档越界（例如从带报文的条目换到
  // 不带的条目）会留下一个不存在的 key，这里回落到概览。
  const active = tabs.some((x) => x.key === tab) ? tab : 'overview';

  return (
    <LogDetailShell title={t('请求日志详情')} onClose={onClose}>
      {tabs.length > 1 && (
        <Tabs<DetailTab>
          className="log-detail-tabs"
          ariaLabel={t('请求日志详情')}
          items={tabs}
          active={active}
          onChange={setTab}
        />
      )}

      {active === 'overview' && (
        <>
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
        </>
      )}

      {/* 对话浏览：管理员点开日志就能看到这轮对话说了什么，
          无需去系统设置开调试模式。普通用户拿不到该字段（后端已剥离）。 */}
      {active === 'conversation' && hasConv && log.conversation && (
        <div className="log-detail-conv">
          <div className="log-detail-label">
            {t('对话内容')}
            {log.conversation.system_prompt_chars ? (
              <span className="log-detail-conv-meta">
                {t('（系统提示 {{n}} 字，未展示）', { n: log.conversation.system_prompt_chars })}
              </span>
            ) : null}
          </div>
          {log.conversation.truncated_history && (
            <div className="log-detail-conv-note">{t('较早的消息已省略（仅保留最近 20 条）')}</div>
          )}
          <div className="log-detail-conv-list">
            {log.conversation.messages.map((m, i) => (
              <div key={i} className={`log-msg log-msg-${m.role}`}>
                <div className="log-msg-role">{m.role}</div>
                <div className="log-msg-text">{m.text}</div>
                {m.truncated && <div className="log-msg-trunc">{t('（已截断）')}</div>}
              </div>
            ))}
          </div>
        </div>
      )}

      {/* 路由回放：管理员可看到这次请求的调度决策 —— 候选渠道、谁被过滤及
          原因、最终选中谁。普通用户拿不到这些字段（后端已剥离）。 */}
      {active === 'route' && hasRoute && (
        <div className="log-detail-route">
          <div className="log-detail-label">{t('路由回放')}</div>
          {log.candidate_channels && log.candidate_channels.length > 0 && (
            <div className="log-detail-route-row">
              <span className="log-detail-route-key">{t('候选（{{n}}）', { n: log.candidate_channels.length })}</span>
              <span className="log-detail-route-chips">
                {log.candidate_channels.map((c) => (
                  <span key={c} className={`log-detail-chip ${c === log.selected_channel ? 'log-detail-chip-on' : ''}`}>{c}</span>
                ))}
              </span>
            </div>
          )}
          {log.selected_channel && (
            <div className="log-detail-route-row">
              <span className="log-detail-route-key">{t('选中')}</span>
              <span className="log-detail-route-chips">
                <span className="log-detail-chip log-detail-chip-on">{log.selected_channel}</span>
              </span>
            </div>
          )}
          {log.filtered_channels && log.filtered_channels.length > 0 && (
            <div className="log-detail-route-row">
              <span className="log-detail-route-key">{t('被过滤（{{n}}）', { n: log.filtered_channels.length })}</span>
              <div className="log-detail-route-filtered">
                {log.filtered_channels.map((f) => (
                  <div key={f.channel_id} className="log-detail-route-filtered-item">
                    <code>{f.channel_id}</code>
                    <span className="log-detail-route-reason">{f.reason}</span>
                  </div>
                ))}
              </div>
            </div>
          )}
        </div>
      )}

      {active === 'raw' && hasRaw && log.debug && (
        <div className="log-detail-debug">
          {log.debug.upstream_url && (
            <div className="log-detail-debug-block">
              <div className="log-detail-label">{t('上游端点')}</div>
              <pre className="log-detail-pre">{log.debug.upstream_url}</pre>
            </div>
          )}
          {log.debug.request_body && (
            <div className="log-detail-debug-block">
              <div className="log-detail-label">{t('请求体')}</div>
              <pre className="log-detail-pre">{log.debug.request_body}</pre>
            </div>
          )}
          {log.debug.response_body && (
            <div className="log-detail-debug-block">
              <div className="log-detail-label">{t('响应体')}</div>
              <pre className="log-detail-pre">{log.debug.response_body}</pre>
            </div>
          )}
        </div>
      )}
    </LogDetailShell>
  );
}

/** 审计日志详情弹窗：操作上下文 + 逐字段 before/after diff */
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

  type ChangeKind = 'added' | 'removed' | 'modified';
  const changes: Array<{ key: string; kind: ChangeKind; before: string; after: string }> = [];
  const unchanged: Array<{ key: string; value: string }> = [];

  const fmt = (v: unknown): string => {
    if (v === undefined) return '—';
    if (typeof v === 'string') return v;
    return JSON.stringify(v);
  };

  if (before && after) {
    // 双快照：逐字段归类为 新增 / 删除 / 修改 / 未变
    for (const k of new Set([...Object.keys(before), ...Object.keys(after)])) {
      const b = before[k], a = after[k];
      if (JSON.stringify(b) === JSON.stringify(a)) {
        unchanged.push({ key: k, value: fmt(b) });
      } else if (b === undefined) {
        changes.push({ key: k, kind: 'added', before: '', after: fmt(a) });
      } else if (a === undefined) {
        changes.push({ key: k, kind: 'removed', before: fmt(b), after: '' });
      } else {
        changes.push({ key: k, kind: 'modified', before: fmt(b), after: fmt(a) });
      }
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
      {changes.length > 0 && (
        <div className="log-detail-changes">
          <div className="log-detail-label">{t('变更内容')}（{changes.length}）</div>
          <div className="log-detail-change-rows">
            {changes.map((c) => (
              <div key={c.key} className={`log-detail-change-row log-detail-change-${c.kind}`}>
                <div className="log-detail-change-row-head">
                  <span className="log-detail-change-badge">{c.key}</span>
                  <span className={`log-detail-change-kind log-detail-kind-${c.kind}`}>
                    {c.kind === 'added' ? t('新增') : c.kind === 'removed' ? t('删除') : t('修改')}
                  </span>
                </div>
                {c.kind !== 'added' && (
                  <div className="log-detail-change-line log-detail-line-old">- {c.before || '—'}</div>
                )}
                {c.kind !== 'removed' && (
                  <div className="log-detail-change-line log-detail-line-new">+ {c.after || '—'}</div>
                )}
              </div>
            ))}
          </div>
        </div>
      )}
      {unchanged.length > 0 && (
        <div className="log-detail-unchanged">
          <details>
            <summary>{t('未变更字段')}（{unchanged.length}）</summary>
            <div className="log-detail-grid">
              {unchanged.map((f) => (
                <div key={f.key} className="log-detail-row">
                  <span className="log-detail-label">{f.key}</span>
                  <span className="log-detail-value log-detail-mono">{f.value}</span>
                </div>
              ))}
            </div>
          </details>
        </div>
      )}
      {(!before || !after) && (log.before || log.after) && (
        <div className="log-detail-diff">
          {log.before && (
            <div className="log-detail-diff-col">
              <div className="log-detail-label">{t('变更前')}</div>
              <pre className="log-detail-pre">{log.before}</pre>
            </div>
          )}
          {log.after && (
            <div className="log-detail-diff-col">
              <div className="log-detail-label">{t('变更后')}</div>
              <pre className="log-detail-pre">{log.after}</pre>
            </div>
          )}
        </div>
      )}
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
