import { useState, useEffect, type MouseEvent } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api';
import { useToast } from '../components/Toast';
import { Button, Card, Loading, EmptyState } from '../components/ui';

type LogTab = 'requests' | 'audits';
type LogView = 'table' | 'timeline';

interface RequestLogItem {
  id: string | number;
  created_at?: number;
  user_id?: string;
  model?: string;
  input_tokens?: number;
  output_tokens?: number;
  cost?: number;
  latency_ms?: number;
  status_code?: number;
  error_msg?: string;
}

interface AuditLogItem {
  id: string | number;
  created_at?: number;
  admin_id?: string;
  action?: string;
  target?: string;
  after?: string;
}

type LogItem = RequestLogItem | AuditLogItem;

interface Filters {
  user: string;
  model: string;
  channel: string;
  start: string;
  end: string;
}

export default function Logs(): JSX.Element {
  const [tab, setTab] = useState<LogTab>('requests');
  const [view, setView] = useState<LogView>('table');
  const [logs, setLogs] = useState<LogItem[]>([]);
  const [total, setTotal] = useState(0);
  const [page, setPage] = useState(1);
  const size = 20;
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  const addToast = useToast();
  const { t } = useTranslation();

  const [filters, setFilters] = useState<Filters>({ user: '', model: '', channel: '', start: '', end: '' });

  // 仅 tab / 分页变化时自动加载；筛选条件由「查询」按钮显式触发，避免每键一请求
  useEffect(() => {
    void load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [tab, page, size]);

  const load = async () => {
    setLoading(true);
    setError('');
    try {
      if (tab === 'requests') {
        const params: Record<string, string | number> = { page, size };
        if (filters.user) params.user = filters.user;
        if (filters.model) params.model = filters.model;
        if (filters.channel) params.channel = filters.channel;
        const res = await api.listRequestLogs(params);
        setLogs(Array.isArray(res?.data) ? res.data : []);
        setTotal(res?.total || 0);
      } else {
        const res = await api.listAuditLogs({ page, size });
        setLogs(Array.isArray(res?.data) ? res.data : []);
        setTotal(res?.total || 0);
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  };

  const handleExport = async (format: 'json' | 'csv') => {
    try {
      const token = localStorage.getItem('token');
      const res = await fetch(`/api/logs/requests/export?format=${format}`, {
        headers: { 'Authorization': `Bearer ${token}` },
      });
      if (!res.ok) {
        const errData = await res.json().catch(() => null);
        throw new Error(errData?.error || errData?.message || `${t('导出失败')} (${res.status})`);
      }
      const blob = await res.blob();
      const url = URL.createObjectURL(blob);
      const a = document.createElement('a');
      a.href = url;
      a.download = `request_logs.${format}`;
      a.click();
      // Safari/Firefox 在同一帧内 revoke 会导致下载被取消，延迟到下一帧再释放
      setTimeout(() => URL.revokeObjectURL(url), 1000);
      addToast(t('导出成功'));
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  };

  // 查询按钮触发筛选：已在第 1 页时直接加载，否则重置页码由 useEffect 自动加载（保证单次请求）
  const handleSearch = (e: MouseEvent<HTMLButtonElement>) => {
    e.preventDefault();
    if (page === 1) {
      void load();
    } else {
      setPage(1);
    }
  };

  const fmtTime = (ts: number | undefined): string => (ts ? new Date(ts * 1000).toLocaleString() : '—');
  const totalPages = Math.ceil(total / size);
  const isRequest = (_l: LogItem): _l is RequestLogItem => tab === 'requests';

  // ── 时间线聚合：当前页日志按小时分桶（成功/失败计数 + 总费用） ──
  const timelineBuckets = (() => {
    const map = new Map<number, { ok: number; fail: number; cost: number; models: Set<string> }>();
    for (const l of logs) {
      if (!isRequest(l) || !l.created_at) continue;
      const hour = Math.floor(l.created_at / 3600) * 3600;
      const b = map.get(hour) || { ok: 0, fail: 0, cost: 0, models: new Set<string>() };
      if ((l.status_code ?? 0) < 400) b.ok += 1;
      else b.fail += 1;
      b.cost += l.cost ?? 0;
      if (l.model) b.models.add(l.model);
      map.set(hour, b);
    }
    return Array.from(map.entries()).sort((a, b) => a[0] - b[0]);
  })();

  const renderTimeline = (): JSX.Element => {
    const buckets = timelineBuckets;
    if (buckets.length === 0) return <EmptyState message={t('暂无时间线数据')} />;
    const W = 720;
    const H = 160;
    const PAD_L = 46;
    const PAD_B = 26;
    const PAD_T = 12;
    const PAD_R = 12;
    const plotW = W - PAD_L - PAD_R;
    const plotH = H - PAD_T - PAD_B;
    const maxCount = Math.max(1, ...buckets.map(([, b]) => b.ok + b.fail));
    const barGap = 3;
    const barW = Math.max(2, Math.floor((plotW - barGap * (buckets.length - 1)) / buckets.length));
    const yFor = (n: number): number => PAD_T + plotH - (n / maxCount) * plotH;
    const ticks = [0, Math.round(maxCount / 2), maxCount];
    return (
      <div className="log-timeline">
        <svg viewBox={`0 0 ${W} ${H}`} preserveAspectRatio="none" style={{ width: '100%', height: 'auto' }} role="img" aria-label={t('请求时间线')}>
          {ticks.map((tk) => (
            <g key={tk}>
              <line x1={PAD_L} y1={yFor(tk)} x2={W - PAD_R} y2={yFor(tk)} stroke="var(--border-color)" strokeWidth={1} />
              <text x={PAD_L - 6} y={yFor(tk) + 3} fontSize={9} fill="var(--text-muted)" textAnchor="end">{tk}</text>
            </g>
          ))}
          {buckets.map(([hour, b], i) => {
            const x = PAD_L + i * (barW + barGap);
            const okH = (b.ok / maxCount) * plotH;
            const failH = (b.fail / maxCount) * plotH;
            const totalH = okH + failH;
            const label = new Date(hour * 1000).toLocaleString(undefined, { month: '2-digit', day: '2-digit', hour: '2-digit' });
            return (
              <g key={hour} data-hour={hour}>
                <title>{`${label} — ${t('成功')} ${b.ok} / ${t('失败')} ${b.fail}`}</title>
                <rect x={x} y={PAD_T + plotH - totalH} width={barW} height={totalH} rx={2} fill="rgba(47,111,237,0.25)" />
                <rect x={x} y={PAD_T + plotH - okH} width={barW} height={okH} rx={2} fill="#34d399" />
                {b.fail > 0 && <rect x={x} y={PAD_T + plotH - totalH} width={barW} height={failH} rx={2} fill="rgba(239,68,68,0.8)" />}
                {buckets.length <= 14 && (
                  <text x={x + barW / 2} y={H - PAD_B + 12} fontSize={8} fill="var(--text-muted)" textAnchor="middle">{label}</text>
                )}
              </g>
            );
          })}
        </svg>
        <div className="log-timeline-legend">
          <span><i style={{ background: '#34d399' }} />{t('成功')}</span>
          <span><i style={{ background: 'rgba(239,68,68,0.8)' }} />{t('失败')}</span>
          <span className="log-timeline-note">{t('当前页日志按小时聚合')}</span>
        </div>
      </div>
    );
  };

  return (
    <div>
      <div className="page-header">
        <h1>{t('日志与审计')}</h1>
        <p>{t('请求日志检索与管理员操作审计')}</p>
      </div>

      {error && <div className="error-message">{error}</div>}

      <Card bodyClassName="">
        <div style={{ display: 'flex', gap: 12, flexWrap: 'wrap', alignItems: 'center' }}>
          <Button variant={tab === 'requests' ? 'primary' : 'outline'} onClick={() => { setTab('requests'); setPage(1); setView('table'); }}>
            {t('请求日志')}
          </Button>
          <Button variant={tab === 'audits' ? 'primary' : 'outline'} onClick={() => { setTab('audits'); setPage(1); setView('table'); }}>
            {t('审计日志')}
          </Button>
          {tab === 'requests' && (
            <>
              <Button variant="outline" size="sm" onClick={() => void handleExport('json')}>{t('导出 JSON')}</Button>
              <Button variant="outline" size="sm" onClick={() => void handleExport('csv')}>{t('导出 CSV')}</Button>
            </>
          )}
          {tab === 'requests' && (
            <div className="ui-tabs" role="tablist" aria-label={t('视图切换')}>
              <button type="button" className={`ui-tab ${view === 'table' ? 'active' : ''}`} onClick={() => setView('table')}>
                {t('表格')}
              </button>
              <button type="button" className={`ui-tab ${view === 'timeline' ? 'active' : ''}`} onClick={() => setView('timeline')}>
                {t('时间线')}
              </button>
            </div>
          )}
        </div>
      </Card>

      {tab === 'requests' && (
        <Card title={t('筛选')} className="" bodyClassName="">
          <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fill, minmax(200px, 1fr))', gap: 12 }}>
            <div className="form-group">
              <label>{t('用户 ID')}</label>
              <input className="form-input" value={filters.user} onChange={(e) => setFilters({ ...filters, user: e.target.value })} placeholder={t('按用户 ID 过滤')} />
            </div>
            <div className="form-group">
              <label>{t('模型')}</label>
              <input className="form-input" value={filters.model} onChange={(e) => setFilters({ ...filters, model: e.target.value })} placeholder={t('按模型过滤')} />
            </div>
            <div className="form-group">
              <label>{t('渠道 ID')}</label>
              <input className="form-input" value={filters.channel} onChange={(e) => setFilters({ ...filters, channel: e.target.value })} placeholder={t('按渠道过滤')} />
            </div>
          </div>
          <div style={{ marginTop: 12 }}>
            <Button size="sm" onClick={handleSearch}>{t('查询')}</Button>
          </div>
        </Card>
      )}

      <Card title={`${tab === 'requests' ? t('请求日志') : t('审计日志')} (${total})`}>
        {loading ? (
          <Loading text={t('加载中')} />
        ) : view === 'timeline' && tab === 'requests' ? (
          renderTimeline()
        ) : logs.length === 0 ? (
          <EmptyState message={t('暂无日志')} />
        ) : (
          <div className="table-wrapper">
            <table>
              <thead>
                {tab === 'requests' ? (
                  <tr>
                    <th>{t('时间')}</th>
                    <th>{t('用户')}</th>
                    <th>{t('模型')}</th>
                    <th>{t('输入')}</th>
                    <th>{t('输出')}</th>
                    <th>{t('费用')}</th>
                    <th>{t('延迟')}</th>
                    <th>{t('状态')}</th>
                    <th>{t('错误')}</th>
                  </tr>
                ) : (
                  <tr>
                    <th>{t('时间')}</th>
                    <th>{t('管理员')}</th>
                    <th>{t('操作')}</th>
                    <th>{t('目标')}</th>
                    <th>{t('变更')}</th>
                  </tr>
                )}
              </thead>
              <tbody>
                {logs.map((l) =>
                  isRequest(l) ? (
                    <tr key={l.id}>
                      <td>{fmtTime(l.created_at)}</td>
                      <td>{l.user_id || '—'}</td>
                      <td>{l.model}</td>
                      <td>{l.input_tokens}</td>
                      <td>{l.output_tokens}</td>
                      <td>{l.cost}</td>
                      <td>{l.latency_ms}ms</td>
                      <td>
                        <span className={(l.status_code ?? 0) < 400 ? 'badge badge-success' : 'badge badge-danger'}>{l.status_code}</span>
                      </td>
                      <td style={{ maxWidth: 200, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                        {l.error_msg || '—'}
                      </td>
                    </tr>
                  ) : (
                    <tr key={l.id}>
                      <td>{fmtTime(l.created_at)}</td>
                      <td>{l.admin_id}</td>
                      <td><code style={{ background: 'var(--card-bg)', padding: '2px 6px', borderRadius: 4 }}>{l.action}</code></td>
                      <td style={{ maxWidth: 200, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{l.target}</td>
                      <td style={{ maxWidth: 300, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', fontSize: 11, color: 'var(--text-muted)' }}>
                        {l.after || '—'}
                      </td>
                    </tr>
                  )
                )}
              </tbody>
            </table>
          </div>
        )}

        {totalPages > 1 && (
          <div style={{ display: 'flex', justifyContent: 'center', gap: 8, marginTop: 16, alignItems: 'center' }}>
            <button className="btn btn-outline btn-sm" disabled={page <= 1} onClick={() => setPage(page - 1)}>{t('上一页')}</button>
            <span style={{ fontSize: 13, color: 'var(--text-muted)' }}>{page} / {totalPages}</span>
            <button className="btn btn-outline btn-sm" disabled={page >= totalPages} onClick={() => setPage(page + 1)}>{t('下一页')}</button>
          </div>
        )}
      </Card>
    </div>
  );
}
