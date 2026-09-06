import { useState, useEffect, type MouseEvent } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api';
import SystemMonitorPanel from '../components/SystemMonitorPanel';
import { Button, Card, Loading, EmptyState, Select } from '../components/ui';
import './Security.css';

interface SecurityOverview {
  total_events?: number;
  critical_events?: number;
  recent_24h?: number;
}

interface SecurityEvent {
  id?: string | number;
  created_at?: number;
  time?: number;
  timestamp?: number;
  type?: string;
  event_type?: string;
  ip?: string;
  client_ip?: string;
  user?: string;
  user_id?: string;
  username?: string;
  detail?: string;
  details?: string;
  message?: string;
  severity?: string;
  level?: string;
}

interface TimeRangeOption {
  value: string;
  labelKey: string;
}

// 安全事件类型选项（用于筛选下拉）
const EVENT_TYPES: TimeRangeOption[] = [
  { value: '', labelKey: '全部' },
  { value: 'auth_failure', labelKey: '认证失败' },
  { value: 'rate_limit', labelKey: '限流触发' },
  { value: 'ip_blocked', labelKey: 'IP 拦截' },
  { value: 'abuse', labelKey: '滥用检测' },
  { value: 'intrusion', labelKey: '入侵尝试' },
];

// 时间范围选项
const TIME_RANGES: TimeRangeOption[] = [
  { value: '1h', labelKey: '近 1 小时' },
  { value: '24h', labelKey: '近 24 小时' },
  { value: '7d', labelKey: '近 7 天' },
  { value: '30d', labelKey: '近 30 天' },
  { value: '', labelKey: '全部' },
];

// Security 页面：安全监控，显示安全概览与事件列表。
// 参照 Logs.tsx 的表格样式与筛选交互模式。
export default function Security(): JSX.Element {
  const { t } = useTranslation();

  const [overview, setOverview] = useState<SecurityOverview | null>(null);
  const [events, setEvents] = useState<SecurityEvent[]>([]);
  const [total, setTotal] = useState(0);
  const [page, setPage] = useState(1);
  const size = 20;
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');

  // 筛选条件
  const [timeRange, setTimeRange] = useState('24h');
  const [eventType, setEventType] = useState('');

  // 概览加载（独立于事件列表，避免分页触发概览重复请求）
  useEffect(() => {
    void loadOverview();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // 事件列表：分页/筛选条件变化自动加载
  useEffect(() => {
    void loadEvents();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [page, timeRange, eventType]);

  const loadOverview = async () => {
    try {
      const res = await api.getSecurityOverview();
      setOverview(res?.data || res || null);
    } catch (err) {
      // 概览加载失败不阻塞事件列表
      setError(err instanceof Error ? err.message : String(err));
    }
  };

  const loadEvents = async () => {
    setLoading(true);
    setError('');
    try {
      const params: Record<string, string | number> = { page, size };
      if (timeRange) params.range = timeRange;
      if (eventType) params.type = eventType;
      const res = await api.getSecurityEvents(params);
      setEvents(Array.isArray(res?.data) ? res.data : []);
      setTotal(res?.total || 0);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  };

  // 查询按钮：筛选条件变化时显式触发
  const handleSearch = (e: MouseEvent<HTMLButtonElement>) => {
    e.preventDefault();
    if (page === 1) {
      void loadEvents();
    } else {
      setPage(1);
    }
  };

  const fmtTime = (ts: number | undefined): string => {
    if (!ts) return '—';
    // 兼容秒级与毫秒级时间戳
    const ms = ts > 1e12 ? ts : ts * 1000;
    return new Date(ms).toLocaleString();
  };

  const totalPages = Math.ceil(total / size);

  // 概览卡片数据（容错：后端字段缺失时回退 0）
  const totalEvents = overview?.total_events ?? 0;
  const criticalEvents = overview?.critical_events ?? 0;
  const recent24h = overview?.recent_24h ?? 0;

  return (
    <div className="security-shell">
      <div className="page-header">
        <div>
          <h1>{t('安全监控')}</h1>
          <p>{t('查看安全事件与监控概览，掌握网关安全态势')}</p>
        </div>
      </div>

      {error && <div className="error-message">{error}</div>}

      {/* 系统资源监控（CPU/内存/负载，自动轮询） */}
      <Card title={t('系统资源')} bodyClassName="">
        <SystemMonitorPanel />
      </Card>

      {/* 概览卡片 */}
      <div className="security-overview">
        <div className="card security-stat-card">
          <div className="card-body">
            <div className="security-stat-label">{t('总事件数')}</div>
            <div className="security-stat-value">{totalEvents}</div>
          </div>
        </div>
        <div className="card security-stat-card security-stat-critical">
          <div className="card-body">
            <div className="security-stat-label">{t('严重事件')}</div>
            <div className="security-stat-value">{criticalEvents}</div>
          </div>
        </div>
        <div className="card security-stat-card security-stat-recent">
          <div className="card-body">
            <div className="security-stat-label">{t('近 24 小时')}</div>
            <div className="security-stat-value">{recent24h}</div>
          </div>
        </div>
      </div>

      {/* 筛选区 */}
      <Card title={t('筛选')} bodyClassName="">
        <div className="security-filter-row">
          <Select
            label={t('时间范围')}
            value={timeRange}
            onChange={(e) => setTimeRange(e.target.value)}
          >
            {TIME_RANGES.map((r) => (
              <option key={r.value} value={r.value}>{t(r.labelKey)}</option>
            ))}
          </Select>
          <Select
            label={t('事件类型')}
            value={eventType}
            onChange={(e) => setEventType(e.target.value)}
          >
            {EVENT_TYPES.map((tp) => (
              <option key={tp.value} value={tp.value}>{t(tp.labelKey)}</option>
            ))}
          </Select>
          <div className="security-filter-action">
            <Button size="sm" onClick={handleSearch}>{t('查询')}</Button>
          </div>
        </div>
      </Card>

      {/* 事件列表 */}
      <Card title={`${t('安全事件')} (${total})`}>
        {loading ? (
          <Loading text={t('加载中')} />
        ) : events.length === 0 ? (
          <EmptyState message={t('暂无安全事件')} icon="🛡️" />
        ) : (
          <div className="table-wrapper">
            <table>
              <thead>
                <tr>
                  <th>{t('时间')}</th>
                  <th>{t('类型')}</th>
                  <th>IP</th>
                  <th>{t('用户')}</th>
                  <th>{t('详情')}</th>
                  <th>{t('严重程度')}</th>
                </tr>
              </thead>
              <tbody>
                {events.map((ev) => {
                  const severity = ev.severity || ev.level || 'info';
                  const isCritical = severity === 'critical' || severity === 'high';
                  return (
                    <tr key={ev.id ?? `${ev.created_at}-${ev.ip}`}>
                      <td style={{ fontSize: 12, color: 'var(--text-muted)' }}>
                        {fmtTime(ev.created_at || ev.time || ev.timestamp)}
                      </td>
                      <td>
                        <code style={{ background: 'var(--card-bg)', padding: '2px 6px', borderRadius: 4, fontSize: 12 }}>
                          {ev.type || ev.event_type || '—'}
                        </code>
                      </td>
                      <td style={{ fontSize: 12 }}>{ev.ip || ev.client_ip || '—'}</td>
                      <td style={{ fontSize: 12 }}>{ev.user || ev.user_id || ev.username || '—'}</td>
                      <td style={{ maxWidth: 320, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', fontSize: 12, color: 'var(--text-muted)' }}>
                        {ev.detail || ev.details || ev.message || '—'}
                      </td>
                      <td>
                        <span className={isCritical ? 'badge badge-danger' : 'badge badge-success'}>
                          {severity}
                        </span>
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
        )}

        {totalPages > 1 && (
          <div style={{ display: 'flex', justifyContent: 'center', gap: 8, marginTop: 16, alignItems: 'center' }}>
            <Button variant="outline" size="sm" disabled={page <= 1} onClick={() => setPage(page - 1)}>
              {t('上一页')}
            </Button>
            <span style={{ fontSize: 13, color: 'var(--text-muted)' }}>{page} / {totalPages}</span>
            <Button variant="outline" size="sm" disabled={page >= totalPages} onClick={() => setPage(page + 1)}>
              {t('下一页')}
            </Button>
          </div>
        )}
      </Card>
    </div>
  );
}
