import React, { useState, useEffect } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api';
import SystemMonitorPanel from '../components/SystemMonitorPanel';
import './Security.css';

// 安全事件类型选项（用于筛选下拉）
const EVENT_TYPES = [
  { value: '', labelKey: '全部' },
  { value: 'auth_failure', labelKey: '认证失败' },
  { value: 'rate_limit', labelKey: '限流触发' },
  { value: 'ip_blocked', labelKey: 'IP 拦截' },
  { value: 'abuse', labelKey: '滥用检测' },
  { value: 'intrusion', labelKey: '入侵尝试' },
];

// 时间范围选项
const TIME_RANGES = [
  { value: '1h', labelKey: '近 1 小时' },
  { value: '24h', labelKey: '近 24 小时' },
  { value: '7d', labelKey: '近 7 天' },
  { value: '30d', labelKey: '近 30 天' },
  { value: '', labelKey: '全部' },
];

// Security 页面：安全监控，显示安全概览与事件列表。
// 参照 Logs.jsx 的表格样式与筛选交互模式。
export default function Security() {
  const { t } = useTranslation();

  const [overview, setOverview] = useState(null);
  const [events, setEvents] = useState([]);
  const [total, setTotal] = useState(0);
  const [page, setPage] = useState(1);
  const [size, setSize] = useState(20);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');

  // 筛选条件
  const [timeRange, setTimeRange] = useState('24h');
  const [eventType, setEventType] = useState('');

  // 概览加载（独立于事件列表，避免分页触发概览重复请求）
  useEffect(() => {
    loadOverview();
  }, []);

  // 事件列表：分页/筛选条件变化自动加载（不再依赖查询按钮显式触发）
  useEffect(() => {
    loadEvents();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [page, size, timeRange, eventType]);

  const loadOverview = async () => {
    try {
      const res = await api.getSecurityOverview();
      setOverview(res?.data || res || null);
    } catch (err) {
      // 概览加载失败不阻塞事件列表
      setError(err.message);
    }
  };

  const loadEvents = async () => {
    setLoading(true);
    setError('');
    try {
      const params = { page, size };
      if (timeRange) params.range = timeRange;
      if (eventType) params.type = eventType;
      const res = await api.getSecurityEvents(params);
      setEvents(res?.data || []);
      setTotal(res?.total || 0);
    } catch (err) {
      setError(err.message);
    } finally {
      setLoading(false);
    }
  };

  // 查询按钮：筛选条件变化时显式触发
  const handleSearch = (e) => {
    e.preventDefault();
    if (page === 1) {
      loadEvents();
    } else {
      setPage(1);
    }
  };

  const fmtTime = (ts) => {
    if (!ts) return '—';
    const n = Number(ts);
    // 兼容秒级与毫秒级时间戳
    const ms = n > 1e12 ? n : n * 1000;
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

      {/* 批次6：系统资源监控（CPU/内存/负载/进程，10s 轮询） */}
      <div className="card" style={{ marginBottom: 16 }}>
        <div className="card-header"><h2>{t('系统资源')}</h2></div>
        <div className="card-body">
          <SystemMonitorPanel />
        </div>
      </div>

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
      <div className="card">
        <div className="card-header"><h2>{t('筛选')}</h2></div>
        <div className="card-body">
          <div className="security-filter-row">
            <div className="form-group">
              <label>{t('时间范围')}</label>
              <select
                className="form-input"
                value={timeRange}
                onChange={(e) => setTimeRange(e.target.value)}
              >
                {TIME_RANGES.map((r) => (
                  <option key={r.value} value={r.value}>{t(r.labelKey)}</option>
                ))}
              </select>
            </div>
            <div className="form-group">
              <label>{t('事件类型')}</label>
              <select
                className="form-input"
                value={eventType}
                onChange={(e) => setEventType(e.target.value)}
              >
                {EVENT_TYPES.map((tp) => (
                  <option key={tp.value} value={tp.value}>{t(tp.labelKey)}</option>
                ))}
              </select>
            </div>
            <div className="security-filter-action">
              <button className="btn btn-primary btn-sm" onClick={handleSearch}>{t('查询')}</button>
            </div>
          </div>
        </div>
      </div>

      {/* 事件列表 */}
      <div className="card">
        <div className="card-header">
          <h2>{t('安全事件')} ({total})</h2>
        </div>
        <div className="card-body">
          {loading ? (
            <div className="loading">{t('加载中')}</div>
          ) : events.length === 0 ? (
            <div className="empty-state"><p>{t('暂无安全事件')}</p></div>
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
                  {events.map((ev, i) => {
                    const severity = ev.severity || ev.level || 'info';
                    const isCritical = severity === 'critical' || severity === 'high';
                    return (
                      <tr key={ev.id ?? i}>
                        <td style={{ fontSize: 12, color: 'var(--text-muted)' }}>{fmtTime(ev.created_at || ev.time || ev.timestamp)}</td>
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
              <button className="btn btn-outline btn-sm" disabled={page <= 1} onClick={() => setPage(page - 1)}>{t('上一页')}</button>
              <span style={{ fontSize: 13, color: 'var(--text-muted)' }}>{page} / {totalPages}</span>
              <button className="btn btn-outline btn-sm" disabled={page >= totalPages} onClick={() => setPage(page + 1)}>{t('下一页')}</button>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
