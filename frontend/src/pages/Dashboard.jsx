import React, { useState, useEffect } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api';
import './Dashboard.css';

function fmtLimit(val) {
  if (val == null) return '—';
  const n = Number(val);
  if (n >= 100000) return (n / 10000).toFixed(1) + 'w';
  if (n >= 1000) return (n / 1000).toFixed(1) + 'K';
  return n.toLocaleString();
}

function fmtTok(val) {
  if (val == null) return '—';
  const n = Number(val);
  if (n >= 1000000000) return (n / 1000000000).toFixed(1) + 'B';
  if (n >= 1000000) return (n / 1000000).toFixed(1) + 'M';
  if (n >= 1000) return (n / 1000).toFixed(1) + 'K';
  return n.toLocaleString();
}

function fmtMoney(val) {
  if (val == null) return '—';
  return '¥' + Number(val).toFixed(2);
}

function TrendChart({ data }) {
  if (!data || data.length === 0) return null;
  const maxVal = Math.max(...data.map((d) => d.value || d.tokens || 0), 1);
  const w = 600;
  const h = 200;
  const pad = { top: 20, right: 20, bottom: 30, left: 50 };
  const chartW = w - pad.left - pad.right;
  const chartH = h - pad.top - pad.bottom;

  const points = data.map((d, i) => {
    const x = pad.left + (i + 0.5) * (chartW / data.length);
    const y = pad.top + chartH - ((d.value || d.tokens || 0) / maxVal) * chartH;
    return `${x},${y}`;
  });

  return (
    <svg viewBox={`0 0 ${w} ${h}`} style={{ width: '100%', height: 'auto' }}>
      {[0, 0.25, 0.5, 0.75, 1].map((r) => {
        const y = pad.top + chartH * (1 - r);
        return (
          <g key={r}>
            <line x1={pad.left} y1={y} x2={w - pad.right} y2={y} stroke="var(--border-color)" strokeWidth="1" />
            <text x={pad.left - 8} y={y + 4} textAnchor="end" fill="var(--text-muted)" fontSize="10">
              {fmtLimit(maxVal * r)}
            </text>
          </g>
        );
      })}

      <defs>
        <linearGradient id="trendGrad" x1="0" y1="0" x2="0" y2="1">
          <stop offset="0%" stopColor="rgba(168, 85, 247, 0.4)" />
          <stop offset="100%" stopColor="rgba(168, 85, 247, 0)" />
        </linearGradient>
      </defs>
      <polygon
        points={`${pad.left},${pad.top + chartH} ${points.join(' ')} ${w - pad.right},${pad.top + chartH}`}
        fill="url(#trendGrad)"
      />

      <polyline
        points={points.join(' ')}
        fill="none"
        stroke="var(--accent-color)"
        strokeWidth="2"
        strokeLinecap="round"
        strokeLinejoin="round"
      />

      {data.map((d, i) => {
        const x = pad.left + (i + 0.5) * (chartW / data.length);
        const y = pad.top + chartH - ((d.value || d.tokens || 0) / maxVal) * chartH;
        return (
          <g key={i}>
            <circle cx={x} cy={y} r="3" fill="var(--accent-color)" stroke="var(--card-bg)" strokeWidth="2" />
            <text x={x} y={pad.top + chartH + 16} textAnchor="middle" fill="var(--text-muted)" fontSize="9">
              {d.label || d.date || ''}
            </text>
          </g>
        );
      })}
    </svg>
  );
}

function PieChart({ data }) {
  if (!data || data.length === 0) return null;
  const total = data.reduce((s, d) => s + (d.value || d.count || 0), 0);
  if (total === 0) return null;

  const colors = ['#818cf8', '#c084fc', '#f472b6', '#fb923c', '#fbbf24', '#34d399', '#22d3ee', '#60a5fa', '#a78bfa', '#f87171'];
  const cx = 120;
  const cy = 120;
  const r = 90;
  let cumAngle = -Math.PI / 2;

  const slices = data.slice(0, 10).map((d, i) => {
    const val = d.value || d.count || 0;
    const angle = (val / total) * 2 * Math.PI;
    const x1 = cx + r * Math.cos(cumAngle);
    const y1 = cy + r * Math.sin(cumAngle);
    const x2 = cx + r * Math.cos(cumAngle + angle);
    const y2 = cy + r * Math.sin(cumAngle + angle);
    const largeArc = angle > Math.PI ? 1 : 0;
    const midAngle = cumAngle + angle / 2;
    const labelX = cx + (r * 0.6) * Math.cos(midAngle);
    const labelY = cy + (r * 0.6) * Math.sin(midAngle);
    const slice = {
      path: `M ${cx} ${cy} L ${x1} ${y1} A ${r} ${r} 0 ${largeArc} 1 ${x2} ${y2} Z`,
      color: colors[i % colors.length],
      pct: (val / total) * 100,
      label: d.label || d.model || d.name || '—',
      labelX,
      labelY,
    };
    cumAngle += angle;
    return slice;
  });

  return (
    <div style={{ display: 'flex', gap: 24, flexWrap: 'wrap', alignItems: 'center' }}>
      <svg viewBox="0 0 240 240" style={{ width: 240, height: 240, flexShrink: 0 }}>
        {slices.map((s, i) => (
          <path key={i} d={s.path} fill={s.color} stroke="var(--card-bg)" strokeWidth="2" />
        ))}
      </svg>
      <div style={{ display: 'flex', flexDirection: 'column', gap: 8, flex: 1, minWidth: 200 }}>
        {slices.map((s, i) => (
          <div key={i} style={{ display: 'flex', alignItems: 'center', gap: 8, fontSize: 13 }}>
            <span style={{ width: 12, height: 12, borderRadius: 3, background: s.color, flexShrink: 0 }} />
            <span style={{ flex: 1, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{s.label}</span>
            <span style={{ color: 'var(--text-muted)', fontWeight: 600 }}>{s.pct.toFixed(1)}%</span>
          </div>
        ))}
      </div>
    </div>
  );
}

export default function Dashboard() {
  const [usage, setUsage] = useState(null);
  const [tokenStats, setTokenStats] = useState(null);
  const [limits, setLimits] = useState(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  const [trend, setTrend] = useState(null);
  const { t } = useTranslation();

  // 增强看板数据
  const [consumptionTrend, setConsumptionTrend] = useState(null);
  const [modelDist, setModelDist] = useState(null);
  const [userRanking, setUserRanking] = useState(null);
  const [channelHealth, setChannelHealth] = useState(null);
  const [realtime, setRealtime] = useState(null);

  useEffect(() => {
    loadData();
  }, []);

  const loadData = async () => {
    setLoading(true);
    setError('');
    try {
      const [
        usageData,
        tokenData,
        limitsData,
        trendData,
        ctData,
        mdData,
        urData,
        chData,
        rtData,
      ] = await Promise.all([
        api.getUsageSummary().catch(() => null),
        api.getTodayTokens().catch(() => null),
        api.getLimits().catch(() => null),
        api.getTrend().catch(() => null),
        api.getConsumptionTrend().catch(() => null),
        api.getModelDistribution().catch(() => null),
        api.getUserRanking().catch(() => null),
        api.getChannelHealth().catch(() => null),
        api.getRealtime().catch(() => null),
      ]);
      setUsage(usageData?.data || usageData);
      setTokenStats(tokenData?.data || tokenData);
      setLimits(limitsData?.data || limitsData);
      setTrend(trendData?.data || trendData);
      setConsumptionTrend(ctData?.data || ctData);
      setModelDist(mdData?.data || mdData);
      setUserRanking(urData?.data || urData);
      setChannelHealth(chData?.data || chData);
      setRealtime(rtData?.data || rtData);
    } catch (err) {
      setError(err.message);
    } finally {
      setLoading(false);
    }
  };

  if (loading) return <div className="loading">{t('加载看板数据')}</div>;

  const u = usage || {};
  const ts = tokenStats || {};
  const l = limits || {};
  const trendData = trend || [];

  const todayTokens = ts.total_tokens || 0;
  const todayInput = ts.input_tokens || 0;
  const todayOutput = ts.output_tokens || 0;
  const todayRequests = ts.request_count || 0;
  const monthlyUsed = l.monthly_used || 0;
  const monthlyLimit = l.monthly_limit;
  const monthlyPct = monthlyLimit && monthlyLimit > 0 ? Math.min(100, (monthlyUsed / monthlyLimit) * 100) : null;

  // 增强数据
  const ct = consumptionTrend || [];
  const md = modelDist || [];
  const ur = userRanking || [];
  const ch = channelHealth || [];
  const rt = realtime || {};

  return (
    <div>
      <div className="page-header">
        <div>
          <h1>{t('仪表盘')}</h1>
          <p>{t('AI 网关使用概览')}</p>
        </div>
        <button className="btn btn-outline btn-sm" onClick={loadData} style={{ gap: '6px' }}>
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
            <polyline points="23 4 23 10 17 10" />
            <polyline points="1 20 1 14 7 14" />
            <path d="M3.51 9a9 9 0 0 1 14.85-3.36L23 10M1 14l4.64 4.36A9 9 0 0 0 20.49 15" />
          </svg>
          {t('刷新')}
        </button>
      </div>

      {error && <div className="error-message">{error}</div>}

      {/* 实时监控数字 */}
      <div className="stats-grid" style={{ gridTemplateColumns: 'repeat(auto-fill, minmax(180px, 1fr))', marginBottom: 16 }}>
        <div className="stat-card">
          <div className="stat-card-top">
            <div className="stat-title">{t('实时 RPS')}</div>
            <div className="stat-icon-badge" style={{ background: 'rgba(34, 197, 94, 0.15)', color: '#34d399' }}>⚡</div>
          </div>
          <div className="stat-value" style={{ fontSize: '28px' }}>{fmtLimit(rt.rps || rt.requests_per_second || 0)}</div>
          <div className="stat-desc" style={{ fontSize: '11px' }}>{t('每秒请求数')}</div>
        </div>
        <div className="stat-card">
          <div className="stat-card-top">
            <div className="stat-title">{t('活跃用户')}</div>
            <div className="stat-icon-badge" style={{ background: 'rgba(99, 102, 241, 0.15)', color: '#818cf8' }}>👥</div>
          </div>
          <div className="stat-value" style={{ fontSize: '28px' }}>{fmtLimit(rt.active_users || 0)}</div>
          <div className="stat-desc" style={{ fontSize: '11px' }}>{t('近 5 分钟')}</div>
        </div>
        <div className="stat-card">
          <div className="stat-card-top">
            <div className="stat-title">{t('活跃渠道')}</div>
            <div className="stat-icon-badge" style={{ background: 'rgba(168, 85, 247, 0.15)', color: '#c084fc' }}>🔗</div>
          </div>
          <div className="stat-value" style={{ fontSize: '28px' }}>{fmtLimit(rt.active_channels || 0)}</div>
          <div className="stat-desc" style={{ fontSize: '11px' }}>{t('在线渠道数')}</div>
        </div>
        <div className="stat-card">
          <div className="stat-card-top">
            <div className="stat-title">{t('平均延迟')}</div>
            <div className="stat-icon-badge" style={{ background: 'rgba(251, 146, 60, 0.15)', color: '#fb923c' }}>⏱️</div>
          </div>
          <div className="stat-value" style={{ fontSize: '28px' }}>{fmtLimit(rt.avg_latency_ms || 0)}ms</div>
          <div className="stat-desc" style={{ fontSize: '11px' }}>{t('近 5 分钟平均')}</div>
        </div>
      </div>

      {/* 基础统计 */}
      <div className="stats-grid">
        <div className="stat-card">
          <div className="stat-card-top">
            <div className="stat-title">{t('今日用量')}</div>
            <div className="stat-icon-badge" style={{ background: 'rgba(99, 102, 241, 0.15)', color: '#818cf8' }}>📊</div>
          </div>
          <div className="stat-value" style={{ fontSize: '32px' }}>{fmtTok(todayTokens)}</div>
          <div className="stat-desc" style={{ display: 'flex', gap: '16px', fontSize: '11px' }}>
            <span>↑ {t('输入')} {fmtTok(todayInput)}</span>
            <span>↓ {t('输出')} {fmtTok(todayOutput)}</span>
          </div>
          <div className="stat-sub">
            <span>{todayRequests.toLocaleString()} {t('次请求')}</span>
          </div>
        </div>

        <div className="stat-card">
          <div className="stat-card-top">
            <div className="stat-title">{t('Token 统计')}</div>
            <div className="stat-icon-badge" style={{ background: 'rgba(168, 85, 247, 0.15)', color: '#c084fc' }}>🔤</div>
          </div>
          <div className="stat-value" style={{ fontSize: '32px' }}>{fmtTok(u.total_tokens)}</div>
          <div className="stat-desc" style={{ fontSize: '11px' }}>
            <div>{t('输入')}: {fmtTok(u.total_input_tokens || u.total_tokens || 0)}</div>
            <div>{t('输出')}: {fmtTok(u.total_output_tokens || 0)}</div>
          </div>
        </div>

        <div className="stat-card">
          <div className="stat-card-top">
            <div className="stat-title">{t('本月用量限额')}</div>
            <div className="stat-icon-badge" style={{ background: 'rgba(16, 185, 129, 0.15)', color: '#34d399' }}>📅</div>
          </div>
          <div className="stat-value" style={{ fontSize: '32px' }}>{monthlyLimit != null ? fmtLimit(monthlyLimit) : '∞'}</div>
          <div className="stat-desc" style={{ fontSize: '11px' }}>
            <span>{t('已用')} {fmtTok(monthlyUsed)}</span>
          </div>
          {monthlyPct != null && (
            <div className="stat-progress">
              <div className="usage-progress-container">
                <div className="usage-progress-bar" style={{ width: `${monthlyPct}%` }} />
              </div>
              <span className="stat-progress-label">{monthlyPct.toFixed(1)}%</span>
            </div>
          )}
        </div>
      </div>

      {/* 消费趋势（增强） */}
      <div className="section-card">
        <div className="section-card-header">
          <h2>{t('消费趋势（近 30 日）')}</h2>
        </div>
        <div className="section-card-body">
          {ct.length > 0 ? (
            <TrendChart data={ct} />
          ) : trendData.length > 0 ? (
            <TrendChart data={trendData} />
          ) : (
            <div className="empty-state" style={{ padding: '30px' }}>
              <p>{t('暂无趋势数据')}</p>
            </div>
          )}
        </div>
      </div>

      {/* 模型分布 + 用户排行 */}
      <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fit, minmax(400px, 1fr))', gap: 16, marginTop: 16 }}>
        <div className="section-card">
          <div className="section-card-header">
            <h2>{t('模型调用分布')}</h2>
          </div>
          <div className="section-card-body">
            {md.length > 0 ? (
              <PieChart data={md} />
            ) : (
              <div className="empty-state" style={{ padding: '30px' }}>
                <p>{t('暂无模型分布数据')}</p>
              </div>
            )}
          </div>
        </div>

        <div className="section-card">
          <div className="section-card-header">
            <h2>{t('用户消费排行（Top 10）')}</h2>
          </div>
          <div className="section-card-body">
            {ur.length > 0 ? (
              <div className="table-wrapper">
                <table>
                  <thead>
                    <tr>
                      <th>#</th>
                      <th>{t('用户')}</th>
                      <th>{t('请求数')}</th>
                      <th>{t('Token')}</th>
                      <th>{t('消费')}</th>
                    </tr>
                  </thead>
                  <tbody>
                    {ur.slice(0, 10).map((r, i) => (
                      <tr key={i}>
                        <td style={{ color: 'var(--text-muted)' }}>{i + 1}</td>
                        <td>{r.email || r.username || `User#${r.user_id}`}</td>
                        <td>{fmtLimit(r.request_count || r.requests || 0)}</td>
                        <td>{fmtTok(r.total_tokens || r.tokens || 0)}</td>
                        <td>{fmtMoney(r.total_cost || r.cost || 0)}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            ) : (
              <div className="empty-state" style={{ padding: '30px' }}>
                <p>{t('暂无用户排行数据')}</p>
              </div>
            )}
          </div>
        </div>
      </div>

      {/* 渠道健康 */}
      <div className="section-card" style={{ marginTop: 16 }}>
        <div className="section-card-header">
          <h2>{t('渠道健康状态')}</h2>
        </div>
        <div className="section-card-body">
          {ch.length > 0 ? (
            <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fill, minmax(240px, 1fr))', gap: 12 }}>
              {ch.map((c, i) => {
                const successRate = c.success_rate != null ? c.success_rate : (c.total_requests > 0 ? (c.success_count / c.total_requests) * 100 : 0);
                const isHealthy = successRate >= 95;
                const isWarning = successRate >= 80 && successRate < 95;
                return (
                  <div key={i} className="stat-card" style={{ padding: 16 }}>
                    <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', marginBottom: 8 }}>
                      <span style={{ fontWeight: 600, fontSize: 14 }}>{c.name || `Channel#${c.id || i}`}</span>
                      <span style={{
                        padding: '2px 8px', borderRadius: 999, fontSize: 11,
                        background: isHealthy ? 'rgba(34,197,94,0.15)' : isWarning ? 'rgba(234,179,8,0.15)' : 'rgba(239,68,68,0.15)',
                        color: isHealthy ? 'rgb(34,197,94)' : isWarning ? 'rgb(234,179,8)' : 'rgb(239,68,68)',
                      }}>
                        {isHealthy ? t('健康') : isWarning ? t('警告') : t('异常')}
                      </span>
                    </div>
                    <div style={{ fontSize: 13, color: 'var(--text-muted)', marginBottom: 4 }}>
                      {t('成功率')}: <span style={{ color: 'var(--text-main)', fontWeight: 600 }}>{successRate.toFixed(1)}%</span>
                    </div>
                    <div style={{ fontSize: 13, color: 'var(--text-muted)', marginBottom: 4 }}>
                      {t('请求数')}: <span style={{ color: 'var(--text-main)' }}>{fmtLimit(c.total_requests || c.requests || 0)}</span>
                    </div>
                    <div style={{ fontSize: 13, color: 'var(--text-muted)' }}>
                      {t('平均延迟')}: <span style={{ color: 'var(--text-main)' }}>{fmtLimit(c.avg_latency_ms || 0)}ms</span>
                    </div>
                  </div>
                );
              })}
            </div>
          ) : (
            <div className="empty-state" style={{ padding: '30px' }}>
              <p>{t('暂无渠道健康数据')}</p>
            </div>
          )}
        </div>
      </div>

      {/* 快捷操作 */}
      <div className="card" style={{ marginTop: 16 }}>
        <div className="card-header">
          <h2>{t('快捷操作')}</h2>
        </div>
        <div className="card-body quick-actions">
          <a href="/accounts" className="btn btn-primary">{t('管理账号')}</a>
          <a href="/keys" className="btn btn-outline">{t('管理 API 密钥')}</a>
          <a href="/mappings" className="btn btn-outline">{t('配置模型映射')}</a>
          <a href="/logs" className="btn btn-outline">{t('查看日志')}</a>
          <a href="/redemptions" className="btn btn-outline">{t('兑换码管理')}</a>
          <a href="/settings" className="btn btn-outline">{t('调整限额')}</a>
        </div>
      </div>
    </div>
  );
}
