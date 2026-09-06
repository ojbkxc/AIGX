import { useState, useEffect } from 'react';
import { useTranslation } from 'react-i18next';
import { Link } from 'react-router-dom';
import { api } from '../api';
import { isAdmin } from '../lib/utils';
import { SkeletonCards } from '../components/ui';
import './Dashboard.css';

// ── 类型定义 ──────────────────────────────────────────────

interface TrendData {
  value?: number;
  cost?: number;
  tokens?: number;
  label?: string;
  date?: string;
}

interface ModelDist {
  label?: string;
  model?: string;
  name?: string;
  value?: number;
  count?: number;
}

interface UserRanking {
  user_id: string | number;
  email?: string;
  username?: string;
  request_count?: number;
  requests?: number;
  count?: number;
  total_tokens?: number;
  tokens?: number;
  total_cost?: number;
  cost?: number;
}

interface ChannelHealth {
  id?: string | number;
  name?: string;
  success_rate?: number | null;
  success_count?: number;
  total_requests?: number;
  requests?: number;
  error_rate?: number | null;
  circuit_breaker?: string;
  avg_latency_ms?: number;
  consecutive_empty?: number | null;
  health?: {
    error_rate?: number | null;
    last_error?: string;
  } | null;
}

interface RealtimeStats {
  qps?: number;
  rps?: number;
  avg_latency_ms?: number;
}

interface UsageStats {
  total_tokens?: number;
  total_input_tokens?: number;
  total_output_tokens?: number;
}

interface TodayTokens {
  total_tokens?: number;
  input_tokens?: number;
  output_tokens?: number;
  request_count?: number;
}

interface LimitsInfo {
  monthly_used?: number;
  monthly_limit?: number | null;
}

interface TrendChartProps {
  data: TrendData[];
}

interface PieChartProps {
  data: ModelDist[];
}

// ── 工具函数 ──────────────────────────────────────────────

function fmtLimit(val: number | null | undefined): string {
  if (val == null) return '—';
  const n = Number(val);
  if (n >= 100000) return (n / 10000).toFixed(1) + 'w';
  if (n >= 1000) return (n / 1000).toFixed(1) + 'K';
  return n.toLocaleString();
}

function fmtTok(val: number | null | undefined): string {
  if (val == null) return '—';
  const n = Number(val);
  if (n >= 1000000000) return (n / 1000000000).toFixed(1) + 'B';
  if (n >= 1000000) return (n / 1000000).toFixed(1) + 'M';
  if (n >= 1000) return (n / 1000).toFixed(1) + 'K';
  return n.toLocaleString();
}

function fmtMoney(val: number | null | undefined): string {
  if (val == null) return '—';
  return '¥' + Number(val).toFixed(2);
}

// 趋势数据点取值：后端消费趋势返回 cost 字段，兼容 value/tokens；
// 使用 ?? 而非 ||，避免成本为 0 的合法数据点被短路误判为无值
function trendVal(d: TrendData): number {
  return d.value ?? d.cost ?? d.tokens ?? 0;
}

// ── 图表组件 ──────────────────────────────────────────────

function TrendChart({ data }: TrendChartProps): JSX.Element | null {
  const [hoverIdx, setHoverIdx] = useState<number | null>(null);
  if (!data || data.length === 0) return null;
  const maxVal = Math.max(...data.map(trendVal), 1);
  const w = 600;
  const h = 200;
  const pad = { top: 20, right: 20, bottom: 30, left: 50 };
  const chartW = w - pad.left - pad.right;
  const chartH = h - pad.top - pad.bottom;

  const points = data.map((d, i) => {
    const x = pad.left + (i + 0.5) * (chartW / data.length);
    const y = pad.top + chartH - (trendVal(d) / maxVal) * chartH;
    return `${x},${y}`;
  });

  const hover = hoverIdx != null ? data[hoverIdx] : null;
  const hoverX = hoverIdx != null ? pad.left + (hoverIdx + 0.5) * (chartW / data.length) : 0;
  const hoverY = hoverIdx != null ? pad.top + chartH - (trendVal(data[hoverIdx]) / maxVal) * chartH : 0;
  const hoverVal = hover ? (hover.value ?? hover.cost ?? hover.tokens ?? 0) : 0;
  // tooltip 框位置：靠近右缘时左移，避免溢出
  const tipW = 110;
  const tipX = hoverX > w - pad.right - tipW - 8 ? hoverX - tipW - 12 : hoverX + 12;
  const tipY = Math.max(pad.top, hoverY - 44);

  const onMove = (e: React.MouseEvent<SVGSVGElement>): void => {
    const rect = e.currentTarget.getBoundingClientRect();
    // viewBox 600 宽映射到实际渲染宽度
    const ratio = w / rect.width;
    const x = (e.clientX - rect.left) * ratio;
    if (x < pad.left || x > w - pad.right) {
      setHoverIdx(null);
      return;
    }
    const idx = Math.floor(((x - pad.left) / chartW) * data.length);
    setHoverIdx(Math.max(0, Math.min(data.length - 1, idx)));
  };

  return (
    <svg
      viewBox={`0 0 ${w} ${h}`}
      style={{ width: '100%', height: 'auto', display: 'block' }}
      onMouseMove={onMove}
      onMouseLeave={() => setHoverIdx(null)}
    >
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
          <stop offset="0%" stopColor="rgba(47, 111, 237, 0.4)" />
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
        const y = pad.top + chartH - (trendVal(d) / maxVal) * chartH;
        return (
          <g key={i}>
            <circle
              cx={x}
              cy={y}
              r={hoverIdx === i ? 5 : 3}
              fill="var(--accent-color)"
              stroke="var(--card-bg)"
              strokeWidth="2"
              style={{ transition: 'r 0.15s ease' }}
            />
            <text x={x} y={pad.top + chartH + 16} textAnchor="middle" fill="var(--text-muted)" fontSize="9">
              {d.label || d.date || ''}
            </text>
          </g>
        );
      })}

      {/* 悬停参考线 + 数值提示 */}
      {hoverIdx != null && (
        <g pointerEvents="none">
          <line
            x1={hoverX}
            y1={pad.top}
            x2={hoverX}
            y2={pad.top + chartH}
            stroke="var(--accent-color)"
            strokeWidth="1"
            strokeDasharray="4 3"
            opacity={0.6}
          />
          <rect x={tipX} y={tipY} width={tipW} height={40} rx="7" fill="var(--card-bg)" stroke="var(--border-color)" />
          <text x={tipX + 10} y={tipY + 17} fill="var(--text-main)" fontSize="11" fontWeight="600">
            {hover?.label || hover?.date || ''}
          </text>
          <text x={tipX + 10} y={tipY + 32} fill="var(--text-muted)" fontSize="10">
            {fmtMoney(hoverVal)}
          </text>
        </g>
      )}
    </svg>
  );
}

function PieChart({ data }: PieChartProps): JSX.Element | null {
  const [hoverSlice, setHoverSlice] = useState<number | null>(null);
  const { t } = useTranslation();
  if (!data || data.length === 0) return null;
  const raw = data.map((d) => ({
    label: d.label || d.model || d.name || '—',
    val: Number(d.value || d.count || 0),
  })).filter((d) => d.val > 0);
  if (raw.length === 0) return null;

  // 超过 9 个模型时，第 10 名及之后聚合为「其他」扇区，避免扇区过多难读
  const MAX_SLICES = 10;
  const top = raw.slice(0, MAX_SLICES - 1);
  const rest = raw.slice(MAX_SLICES - 1);
  const slicesRaw = rest.length
    ? [...top, { label: `其他 (${rest.length})`, val: rest.reduce((s, d) => s + d.val, 0) }]
    : top;
  const total = slicesRaw.reduce((s, d) => s + d.val, 0);

  const colors = ['#7ca4f5', '#7ca4f5', '#f472b6', '#fb923c', '#fbbf24', '#34d399', '#22d3ee', '#60a5fa', '#a78bfa', '#94a3b8'];
  const cx = 120;
  const cy = 120;
  const r = 90;
  let cumAngle = -Math.PI / 2;

  const slices = slicesRaw.map((d, i) => {
    const angle = (d.val / total) * 2 * Math.PI;
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
      pct: (d.val / total) * 100,
      label: d.label,
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
          <path
            key={i}
            d={s.path}
            fill={s.color}
            stroke="var(--card-bg)"
            strokeWidth="2"
            opacity={hoverSlice === null || hoverSlice === i ? 1 : 0.35}
            style={{ transition: 'opacity 0.2s ease', cursor: 'pointer' }}
            onMouseEnter={() => setHoverSlice(i)}
            onMouseLeave={() => setHoverSlice(null)}
          />
        ))}
        {/* 中心统计：悬停显示该扇区数值，默认显示总量 */}
        {hoverSlice != null ? (
          <text x={cx} y={cy - 2} textAnchor="middle" fill="var(--text-main)" fontSize="13" fontWeight="700">
            {slices[hoverSlice].pct.toFixed(1)}%
          </text>
        ) : (
          <text x={cx} y={cy - 2} textAnchor="middle" fill="var(--text-main)" fontSize="16" fontWeight="700">
            {fmtLimit(total)}
          </text>
        )}
        <text x={cx} y={cy + 16} textAnchor="middle" fill="var(--text-muted)" fontSize="10">
          {hoverSlice != null ? slices[hoverSlice].label.length > 12
            ? slices[hoverSlice].label.slice(0, 12) + '…'
            : slices[hoverSlice].label : t('总量')}
        </text>
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

// ── 主组件 ──────────────────────────────────────────────

export default function Dashboard(): JSX.Element {
  const [usage, setUsage] = useState<UsageStats | null>(null);
  const [tokenStats, setTokenStats] = useState<TodayTokens | null>(null);
  const [limits, setLimits] = useState<LimitsInfo | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  const [trend, setTrend] = useState<TrendData[] | null>(null);
  const { t } = useTranslation();

  // 增强看板数据
  const [consumptionTrend, setConsumptionTrend] = useState<TrendData[] | null>(null);
  const [modelDist, setModelDist] = useState<ModelDist[] | null>(null);
  const [userRanking, setUserRanking] = useState<UserRanking[] | null>(null);
  const [channelHealth, setChannelHealth] = useState<ChannelHealth[] | null>(null);
  const [realtime, setRealtime] = useState<RealtimeStats | null>(null);

  useEffect(() => {
    loadData();
    // 实时指标轮询：30s 刷新一次 RPS / 活跃用户 / 活跃渠道 / 平均延迟
    // 仅管理员轮询管理指标，普通用户静默展示个人配额视图
    const timer = setInterval(() => {
      if (isAdmin()) loadRealtimeOnly();
    }, 30000);
    return () => clearInterval(timer);
  }, []);

  // 仅刷新实时指标，避免整页 loading 闪烁
  const loadRealtimeOnly = async (): Promise<void> => {
    try {
      const [rtData, urData, chData] = await Promise.all([
        api.getRealtime().catch(() => null),
        api.getUserRanking().catch(() => null),
        api.getChannelHealth().catch(() => null),
      ]);
      setRealtime((rtData?.data ?? rtData) as RealtimeStats | null);
      if (urData) setUserRanking((urData.data ?? urData) as UserRanking[]);
      if (chData) setChannelHealth((chData.data ?? chData) as ChannelHealth[]);
    } catch {
      // 轮询失败静默，不打扰用户
    }
  };

  const loadData = async (): Promise<void> => {
    setLoading(true);
    setError('');
    try {
      // 普通用户兼容：管理员统计端点（verify_admin）403 时回退到 /users/me 个人配额数据，
      // 保证非管理员登录后仪表盘仍展示个人配额而非全空。
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
      setUsage((usageData?.data ?? usageData) as UsageStats | null);
      setTokenStats((tokenData?.data ?? tokenData) as TodayTokens | null);
      setLimits((limitsData?.data ?? limitsData) as LimitsInfo | null);
      setTrend((trendData?.data ?? trendData) as TrendData[] | null);
      setConsumptionTrend((ctData?.data ?? ctData) as TrendData[] | null);
      setModelDist((mdData?.data ?? mdData) as ModelDist[] | null);
      setUserRanking((urData?.data ?? urData) as UserRanking[] | null);
      setChannelHealth((chData?.data ?? chData) as ChannelHealth[] | null);
      setRealtime((rtData?.data ?? rtData) as RealtimeStats | null);

      // 普通用户回退：无任何管理员数据时拉取个人配额展示
      const hasAnyData = usageData || tokenData || limitsData;
      if (!hasAnyData) {
        try {
          const me = await api.users.getMe();
          const meData = (me ?? null) as unknown as {
            used_quota?: number;
            quota?: number | null;
          } | null;
          if (meData) {
            setLimits({
              monthly_used: meData.used_quota ?? 0,
              monthly_limit: meData.quota ?? null,
            });
          }
        } catch {
          // 静默：保持空态展示
        }
      }
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  };

  if (loading) {
    return (
      <div>
        <div className="page-header">
          <div>
            <h1>{t('仪表盘')}</h1>
            <p>{t('AI 网关使用概览')}</p>
          </div>
        </div>
        <SkeletonCards count={6} />
      </div>
    );
  }

  const u = usage || {};
  const ts = tokenStats || {};
  const l = limits || {};
  const trendData: TrendData[] = trend || [];

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
          <div className="stat-value" style={{ fontSize: '24px' }}>{fmtLimit(rt.qps ?? rt.rps ?? 0)}</div>
          <div className="stat-desc" style={{ fontSize: '11px' }}>{t('每秒请求数')}</div>
        </div>
        <div className="stat-card">
          <div className="stat-card-top">
            <div className="stat-title">{t('活跃用户')}</div>
            <div className="stat-icon-badge" style={{ background: 'rgba(47, 111, 237, 0.15)', color: '#7ca4f5' }}>👥</div>
          </div>
          <div className="stat-value" style={{ fontSize: '24px' }}>{fmtLimit(ur.length)}</div>
          <div className="stat-desc" style={{ fontSize: '11px' }}>{t('近 5 分钟')}</div>
        </div>
        <div className="stat-card">
          <div className="stat-card-top">
            <div className="stat-title">{t('活跃渠道')}</div>
            <div className="stat-icon-badge" style={{ background: 'rgba(47, 111, 237, 0.15)', color: '#7ca4f5' }}>🔗</div>
          </div>
          <div className="stat-value" style={{ fontSize: '24px' }}>{fmtLimit(ch.length)}</div>
          <div className="stat-desc" style={{ fontSize: '11px' }}>{t('在线渠道数')}</div>
        </div>
        <div className="stat-card">
          <div className="stat-card-top">
            <div className="stat-title">{t('平均延迟')}</div>
            <div className="stat-icon-badge" style={{ background: 'rgba(251, 146, 60, 0.15)', color: '#fb923c' }}>⏱️</div>
          </div>
          <div className="stat-value" style={{ fontSize: '24px' }}>{fmtLimit(rt.avg_latency_ms || 0)}ms</div>
          <div className="stat-desc" style={{ fontSize: '11px' }}>{t('近 5 分钟平均')}</div>
        </div>
      </div>

      {/* 基础统计 */}
      <div className="stats-grid">
        <div className="stat-card">
          <div className="stat-card-top">
            <div className="stat-title">{t('今日用量')}</div>
            <div className="stat-icon-badge" style={{ background: 'rgba(47, 111, 237, 0.15)', color: '#7ca4f5' }}>📊</div>
          </div>
          <div className="stat-value" style={{ fontSize: '26px' }}>{fmtTok(todayTokens)}</div>
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
            <div className="stat-icon-badge" style={{ background: 'rgba(47, 111, 237, 0.15)', color: '#7ca4f5' }}>🔤</div>
          </div>
          <div className="stat-value" style={{ fontSize: '26px' }}>{fmtTok(u.total_tokens)}</div>
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
          <div className="stat-value" style={{ fontSize: '26px' }}>{monthlyLimit != null ? fmtLimit(monthlyLimit) : '∞'}</div>
          <div className="stat-desc" style={{ fontSize: '11px' }}>
            <span>{t('已用')} {fmtTok(monthlyUsed)}</span>
            {monthlyPct != null && <span style={{ marginLeft: 12 }}>{monthlyPct.toFixed(1)}%</span>}
          </div>
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
            <div className="empty-state" style={{ padding: '22px' }}>
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
              <div className="empty-state" style={{ padding: '22px' }}>
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
                        <td>{fmtLimit(r.request_count || r.requests || r.count || 0)}</td>
                        <td>{fmtTok(r.total_tokens || r.tokens || 0)}</td>
                        <td>{fmtMoney(r.total_cost || r.cost || 0)}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            ) : (
              <div className="empty-state" style={{ padding: '22px' }}>
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
                const totalReq = c.total_requests ?? 0;
                const successRate = c.success_rate != null ? c.success_rate : (totalReq > 0 ? ((c.success_count ?? 0) / totalReq) * 100 : 0);
                const errorRate = c.error_rate != null ? c.error_rate : (totalReq > 0 ? ((totalReq - (c.success_count ?? 0)) / totalReq) * 100 : 0);
                const isHealthy = successRate >= 95;
                const isWarning = successRate >= 80 && successRate < 95;
                // 断路器状态：后端 circuit_breaker 已为机器可读小写枚举
                // ("open"/"halfopen"/"closed"，见 channel/circuit_breaker.rs get_state)
                const breakerState = String(c.circuit_breaker || 'closed').toLowerCase();
                const breakerColor = breakerState === 'open' ? 'rgb(239,68,68)' : breakerState === 'halfopen' ? 'rgb(234,179,8)' : 'rgb(34,197,94)';
                const breakerLabel = breakerState === 'open' ? t('熔断') : breakerState === 'halfopen' ? t('半开') : t('正常');
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
                    {/* 断路器状态：用圆点 + 文字标识 */}
                    <div style={{ fontSize: 13, color: 'var(--text-muted)', marginBottom: 4, display: 'flex', alignItems: 'center', gap: 6 }}>
                      <span style={{ width: 8, height: 8, borderRadius: '50%', background: breakerColor, display: 'inline-block' }} />
                      {t('断路器')}: <span style={{ color: breakerColor, fontWeight: 600 }}>{breakerLabel}</span>
                    </div>
                    <div style={{ fontSize: 13, color: 'var(--text-muted)', marginBottom: 4 }}>
                      {t('成功率')}: <span style={{ color: 'var(--text-main)', fontWeight: 600 }}>{successRate.toFixed(1)}%</span>
                    </div>
                    {/* 错误率：百分比 + 进度条 */}
                    <div style={{ fontSize: 13, color: 'var(--text-muted)', marginBottom: 4 }}>
                      {t('错误率')}: <span style={{ color: errorRate > 20 ? 'rgb(239,68,68)' : 'var(--text-main)', fontWeight: 600 }}>{errorRate.toFixed(1)}%</span>
                    </div>
                    <div style={{ height: 4, background: 'var(--bg-color)', borderRadius: 2, marginBottom: 4, overflow: 'hidden' }}>
                      <div style={{
                        width: `${Math.min(100, errorRate)}%`,
                        height: '100%',
                        background: errorRate > 20 ? 'rgb(239,68,68)' : errorRate > 5 ? 'rgb(234,179,8)' : 'rgb(34,197,94)',
                        borderRadius: 2,
                        transition: 'width 0.3s ease',
                      }} />
                    </div>
                    <div style={{ fontSize: 13, color: 'var(--text-muted)', marginBottom: 4 }}>
                      {t('平均延迟')}: <span style={{ color: 'var(--text-main)' }}>{fmtLimit(c.avg_latency_ms || 0)}ms</span>
                    </div>
                    {/* 连续空响应计数：超过阈值提示风险 */}
                    {c.consecutive_empty != null && c.consecutive_empty > 0 && (
                      <div style={{ fontSize: 13, color: c.consecutive_empty >= 3 ? 'rgb(239,68,68)' : 'var(--text-muted)' }}>
                        {t('连续空响应')}: <span style={{ fontWeight: 600 }}>{c.consecutive_empty}</span>
                      </div>
                    )}
                    {/* 批次6：health_tracker 实时快照（错误率 EMA 视角） */}
                    {c.health && c.health.error_rate != null && c.health.error_rate > 0 && (
                      <div style={{ fontSize: 13, color: 'var(--text-muted)' }}>
                        {t('实时错误率')}: <span style={{ color: c.health.error_rate > 0.5 ? 'rgb(234,179,8)' : 'var(--text-main)', fontWeight: 600 }}>
                          {(c.health.error_rate * 100).toFixed(1)}%
                        </span>
                      </div>
                    )}
                    {c.health && c.health.last_error && (
                      <div style={{ fontSize: 12, color: 'var(--text-muted)', marginTop: 4, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }} title={c.health.last_error}>
                        {t('最近错误')}: {c.health.last_error}
                      </div>
                    )}
                    <div style={{ fontSize: 13, color: 'var(--text-muted)', marginTop: 4 }}>
                      {t('请求数')}: <span style={{ color: 'var(--text-main)' }}>{fmtLimit(c.total_requests || c.requests || 0)}</span>
                    </div>
                  </div>
                );
              })}
            </div>
          ) : (
            <div className="empty-state" style={{ padding: '22px' }}>
              <p>{t('暂无渠道健康数据')}</p>
            </div>
          )}
        </div>
      </div>

      {/* 快捷操作（按角色渲染：普通用户只看到自己的工具箱） */}
      <div className="card" style={{ marginTop: 16 }}>
        <div className="card-header">
          <h2>{t('快捷操作')}</h2>
        </div>
        {isAdmin() ? (
          <div className="card-body quick-actions">
            <Link to="/channels" className="btn btn-primary">{t('管理账号')}</Link>
            <Link to="/keys" className="btn btn-outline">{t('管理 API 密钥')}</Link>
            <Link to="/mappings" className="btn btn-outline">{t('配置模型映射')}</Link>
            <Link to="/logs" className="btn btn-outline">{t('查看日志')}</Link>
            <Link to="/redemptions" className="btn btn-outline">{t('兑换码管理')}</Link>
            <Link to="/settings" className="btn btn-outline">{t('调整限额')}</Link>
          </div>
        ) : (
          <div className="card-body quick-actions">
            <Link to="/playground" className="btn btn-primary">{t('去 Playground 调试')}</Link>
            <Link to="/keys" className="btn btn-outline">{t('管理 API 密钥')}</Link>
            <Link to="/wallet" className="btn btn-outline">{t('钱包充值')}</Link>
            <Link to="/profile" className="btn btn-outline">{t('个人中心')}</Link>
          </div>
        )}
      </div>
    </div>
  );
}
