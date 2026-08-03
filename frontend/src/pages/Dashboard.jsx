import React, { useState, useEffect } from 'react';
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

function TrendChart({ data }) {
  if (!data || data.length === 0) return null;
  const maxVal = Math.max(...data.map((d) => d.value), 1);
  const w = 600;
  const h = 200;
  const pad = { top: 20, right: 20, bottom: 30, left: 50 };
  const chartW = w - pad.left - pad.right;
  const chartH = h - pad.top - pad.bottom;
  const barW = Math.max(8, chartW / data.length - 8);

  const points = data.map((d, i) => {
    const x = pad.left + (i + 0.5) * (chartW / data.length);
    const y = pad.top + chartH - (d.value / maxVal) * chartH;
    return `${x},${y}`;
  });

  return (
    <svg viewBox={`0 0 ${w} ${h}`} style={{ width: '100%', height: 'auto' }}>
      {/* Grid lines */}
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

      {/* Area fill */}
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

      {/* Line */}
      <polyline
        points={points.join(' ')}
        fill="none"
        stroke="var(--accent-color)"
        strokeWidth="2"
        strokeLinecap="round"
        strokeLinejoin="round"
      />

      {/* Dots */}
      {data.map((d, i) => {
        const x = pad.left + (i + 0.5) * (chartW / data.length);
        const y = pad.top + chartH - (d.value / maxVal) * chartH;
        return (
          <g key={i}>
            <circle cx={x} cy={y} r="3" fill="var(--accent-color)" stroke="var(--card-bg)" strokeWidth="2" />
            <text x={x} y={pad.top + chartH + 16} textAnchor="middle" fill="var(--text-muted)" fontSize="9">
              {d.label}
            </text>
          </g>
        );
      })}
    </svg>
  );
}

export default function Dashboard() {
  const [usage, setUsage] = useState(null);
  const [tokenStats, setTokenStats] = useState(null);
  const [limits, setLimits] = useState(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  const [trend, setTrend] = useState(null);

  useEffect(() => {
    loadData();
  }, []);

  const loadData = async () => {
    setLoading(true);
    setError('');
    try {
      const [usageData, tokenData, limitsData, trendData] = await Promise.all([
        api.getUsageSummary().catch(() => null),
        api.getTodayTokens().catch(() => null),
        api.getLimits().catch(() => null),
        api.getTrend().catch(() => null),
      ]);
      setUsage(usageData?.data || usageData);
      setTokenStats(tokenData?.data || tokenData);
      setLimits(limitsData?.data || limitsData);
      setTrend(trendData?.data || trendData);
    } catch (err) {
      setError(err.message);
    } finally {
      setLoading(false);
    }
  };

  if (loading) return <div className="loading">加载看板数据</div>;

  const u = usage || {};
  const t = tokenStats || {};
  const l = limits || {};
  const trendData = trend || [];

  const todayTokens = t.total_tokens || 0;
  const todayInput = t.input_tokens || 0;
  const todayOutput = t.output_tokens || 0;
  const todayRequests = t.request_count || 0;
  const monthlyUsed = l.monthly_used || 0;
  const monthlyLimit = l.monthly_limit;
  const monthlyPct = monthlyLimit && monthlyLimit > 0 ? Math.min(100, (monthlyUsed / monthlyLimit) * 100) : null;

  return (
    <div>
      <div className="page-header">
        <div>
          <h1>Dashboard</h1>
          <p>AI 网关使用概览</p>
        </div>
        <button className="btn btn-outline btn-sm" onClick={loadData} style={{ gap: '6px' }}>
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
            <polyline points="23 4 23 10 17 10" />
            <polyline points="1 20 1 14 7 14" />
            <path d="M3.51 9a9 9 0 0 1 14.85-3.36L23 10M1 14l4.64 4.36A9 9 0 0 0 20.49 15" />
          </svg>
          刷新
        </button>
      </div>

      {error && <div className="error-message">{error}</div>}

      {/* Stats Grid */}
      <div className="stats-grid">
        {/* Today's Usage */}
        <div className="stat-card">
          <div className="stat-card-top">
            <div className="stat-title">今日用量</div>
            <div className="stat-icon-badge" style={{ background: 'rgba(99, 102, 241, 0.15)', color: '#818cf8' }}>
              📊
            </div>
          </div>
          <div className="stat-value" style={{ fontSize: '32px' }}>{fmtTok(todayTokens)}</div>
          <div className="stat-desc" style={{ display: 'flex', gap: '16px', fontSize: '11px' }}>
            <span>↑ 输入 {fmtTok(todayInput)}</span>
            <span>↓ 输出 {fmtTok(todayOutput)}</span>
          </div>
          <div className="stat-sub">
            <span>{todayRequests.toLocaleString()} 次请求</span>
          </div>
        </div>

        {/* Token Stats */}
        <div className="stat-card">
          <div className="stat-card-top">
            <div className="stat-title">Token 统计</div>
            <div className="stat-icon-badge" style={{ background: 'rgba(168, 85, 247, 0.15)', color: '#c084fc' }}>
              🔤
            </div>
          </div>
          <div className="stat-value" style={{ fontSize: '32px' }}>{fmtTok(u.total_tokens)}</div>
          <div className="stat-desc" style={{ fontSize: '11px' }}>
            <div>输入: {fmtTok(u.total_input_tokens || u.total_tokens || 0)}</div>
            <div>输出: {fmtTok(u.total_output_tokens || 0)}</div>
          </div>
        </div>

        {/* Monthly Limit */}
        <div className="stat-card">
          <div className="stat-card-top">
            <div className="stat-title">本月用量限额</div>
            <div className="stat-icon-badge" style={{ background: 'rgba(16, 185, 129, 0.15)', color: '#34d399' }}>
              📅
            </div>
          </div>
          <div className="stat-value" style={{ fontSize: '32px' }}>{monthlyLimit != null ? fmtLimit(monthlyLimit) : '∞'}</div>
          <div className="stat-desc" style={{ fontSize: '11px' }}>
            <span>已用 {fmtTok(monthlyUsed)}</span>
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

      {/* Trend Chart */}
      <div className="section-card">
        <div className="section-card-header">
          <h2>近 7 日消耗趋势</h2>
        </div>
        <div className="section-card-body">
          {trendData.length > 0 ? (
            <TrendChart data={trendData} />
          ) : (
            <div className="empty-state" style={{ padding: '30px' }}>
              <p>暂无趋势数据</p>
            </div>
          )}
        </div>
      </div>

      {/* Quick Actions */}
      <div className="card">
        <div className="card-header">
          <h2>快捷操作</h2>
        </div>
        <div className="card-body quick-actions">
          <a href="/accounts" className="btn btn-primary">管理账号</a>
          <a href="/keys" className="btn btn-outline">管理 API 密钥</a>
          <a href="/mappings" className="btn btn-outline">配置模型映射</a>
          <a href="/settings" className="btn btn-outline">调整限额</a>
        </div>
      </div>
    </div>
  );
}