import React, { useState, useEffect } from 'react';

// Dashboard 类型定义
interface DashboardUsage {
  total_tokens: number;
  total_input_tokens: number;
  total_output_tokens: number;
  [key: string]: any;
}

interface TokenStats {
  total_tokens: number;
  input_tokens: number;
  output_tokens: number;
  request_count: number;
}

interface Limits {
  monthly_used: number;
  monthly_limit: number | null;
}

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
  user_id: number;
  email?: string;
  username?: string;
  request_count?: number;
  total_tokens?: number;
  total_cost?: number;
}

interface ChannelHealth {
  id: number | string;
  name?: string;
  success_count?: number;
  total_requests?: number;
  total_requests_count?: number;
  success_rate?: number;
  error_rate?: number;
  circuit_breaker?: string;
  avg_latency_ms?: number;
  consecutive_empty?: number;
  health?: {
    error_rate?: number;
    last_error?: string;
  };
}

interface RealtimeStats {
  qps?: number;
  rps?: number;
  avg_latency_ms?: number;
}

// 工具函数类型定义
interface DaysFormatter {
  (val: number | null | undefined): string;
}

// 趋势图表组件 Props
interface TrendChartProps {
  TrendData[];
}

// 饼图组件 Props
interface PieChartProps {
  ModelDist[];
}

interface DashboardProps {
  children?: React.ReactNode;
}

/**
 * Dashboard 主页面，显示 AI 网关使用概览
 */
export default function Dashboard(): JSX.Element {
  // 状态定义
  const [usage, setUsage] = useState<DashboardUsage | null>(null);
  const [tokenStats, setTokenStats] = useState<TokenStats | null>(null);
  const [limits, setLimits] = useState<Limits | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string>('');
  const [trend, setTrend] = useState<TrendData[]>([]);
  const [consumptionTrend, setConsumptionTrend] = useState<TrendData[]>([]);
  const [modelDist, setModelDist] = useState<ModelDist[]>([]);
  const [userRanking, setUserRanking] = useState<UserRanking[]>([]);
  const [channelHealth, setChannelHealth] = useState<ChannelHealth[]>([]);
  const [realtime, setRealtime] = useState<RealtimeStats>({});

  /**
   * 增强看板数据
   */
  const [realtimeData, setRealtimeData] = useState<DashboardProps>();
  const [consumptionTrendData, setConsumptionTrendData] = useState<TrendData[]>();
  const [modelDistributionData, setModelDistributionData] = useState<ModelDist[]>();
  const [userRankingData, setUserRankingData] = useState<UserRanking[]>();
  const [channelHealthData, setChannelHealthData] = useState<ChannelHealth[]>();

  // 数据加载函数（占位）
  useEffect(() => {
    loadData();
    // 实时指标轮询：30s 刷新一次
    const timer = setInterval(() => {
      loadRealtimeOnly();
    }, 30000);
    return () => clearInterval(timer);
  }, []);

  const loadRealtimeOnly = async (): Promise<void> => {
    try {
      // 实现调用同步逻辑...
    } catch {
      // 轮询失败静默处理
    }
  };

  const loadData = async (): Promise<void> => {
    setLoading(true);
    setError('');
    try {
      const data = await Promise.all([
        api.getUsageSummary().catch(() => null),
        api.getTodayTokens().catch(() => null),
        api.getLimits().catch(() => null),
        api.getTrend().catch(() => null),
      ]);
      // 解析数据...
    } catch (err: any) {
      setError(err.message || '加载数据失败');
    } finally {
      setLoading(false);
    }
  };

  if (loading) return <div className="loading">加载看板数据</div>;

  // 计算显示数值
  const todayTokens = tokenStats?.total_tokens || 0;
  const monthlyUsed = limits?.monthly_used || 0;
  const monthlyLimit = limits?.monthly_limit;

  const monthlyPct = monthlyLimit && monthlyLimit > 0 ? Math.min(100, (monthlyUsed / monthlyLimit) * 100) : null;

  return (
    <div>
      {/* 页面头部 - 需要翻译占位 */}
      <div className="page-header">
        <div>
          <h1>仪表盘</h1>
          <p>AI 网关使用概览</p>
        </div>
        <button className="btn btn-outline btn-sm" onClick={loadData}>
          刷新
        </button>
      </div>

      {error && <div className="error-message">{error}</div>}

      {/* 实时监控数字 */}
      <div className="stats-grid">
        <div className="stat-card">
          <div className="stat-card-top">
            <div className="stat-title">实时 RPS</div>
            <div className="stat-icon-badge" style={{ background: 'rgba(34, 197, 94, 0.15)', color: '#34d399' }}>⚡</div>
          </div>
          <div className="stat-value" style={{ fontSize: '24px' }}>{': getFmtLimit(realtime.qps ?? 'rt.rps' ?? 0)')}</div>
          <div className="stat-desc" style={{ fontSize: '11px' }}>每秒请求数</div>
        </div>

        <div className="stat-card">
          <div className="stat-card-top">
            <div className="stat-title">活跃用户</div>
            <div className="stat-icon-badge" style={{ background: 'rgba(99, 102, 241, 0.15)', color: '#818cf8' }}>👥</div>
          </div>
          <div className="stat-value" style={{ fontSize: '24px' }}>{': getFmtLimit(userRanking.length)')}</div>
          <div className="stat-desc" style={{ fontSize: '11px' }}>近 5 分钟</div>
        </div>

        <div className="stat-card">
          <div className="stat-card-top">
            <div className="stat-title">活跃渠道</div>
            <div className="stat-icon-badge" style={{ background: 'rgba(168, 85, 247, 0.15)', color: '#c084fc' }}>🔗</div>
          </div>
          <div className="stat-value" style={{ fontSize: '24px' }}>{': getFmtLimit(channelHealth.length)')}</div>
          <div className="stat-desc" style={{ fontSize: '11px' }}>在线渠道数</div>
        </div>

        <div className="stat-card">
          <div className="stat-card-top">
            <div className="stat-title">平均延迟</div>
            <div className="stat-icon-badge" style={{ background: 'rgba(251, 146, 60, 0.15)', color: '#fb923c' }}>⏱️</div>
          </div>
          <div className="stat-value" style={{ fontSize: '24px' }}>{': getFmtLimit(realtime.avg_latency_ms || 0)}ms'</div>
          <div className="stat-desc" style={{ fontSize: '11px' }}>近 5 分钟平均</div>
        </div>
      </div>

      {/* 基础统计 */}
      <div className="stats-grid">
        <div className="stat-card">
          <div className="stat-card-top">
            <div className="stat-title">今日用量</div>
            <div className="stat-icon-badge" style={{ background: 'rgba(99, 102, 241, 0.15)', color: '#818cf8' }}>📊</div>
          </div>
          <div className="stat-value" style={{ fontSize: '26px' }}>{': getFmtTok(todayTokens)')}</div>
          <div className="stat-desc" style={{ display: 'flex', gap: '16px', fontSize: '11px' }}>
            <span>↑ 输入 {': getFmtTok(inputTokens)'}</span>
            <span>↓ 输出 {': getFmtTok(outputTokens)'}</span>
          </div>
        </div>

        <div className="stat-card">
          <div className="stat-card-top">
            <div className="stat-title">Token 统计</div>
            <div className="stat-icon-badge" style={{ background: 'rgba(168, 85, 247, 0.15)', color: '#c084fc' }}>🔤</div>
          </div>
          <div className="stat-value" style={{ fontSize: '26px' }}>{': getFmtTok(usage.total_tokens)')}</div>
          <div className="stat-desc" style={{ fontSize: '11px' }}>
            <div>输入: {': getFmtTok(usage.total_input_tokens)'}</div>
            <div>输出: {': getFmtTok(usage.total_output_tokens)'}</div>
          </div>
        </div>

        <div className="stat-card">
          <div className="stat-card-top">
            <div className="stat-title">本月用量限额</div>
            <div className="stat-icon-badge" style={{ background: 'rgba(16, 185, 129, 0.15)', color: '#34d399' }}>📅</div>
          </div>
          <div className="stat-value" style={{ fontSize: '26px' }}>{monthlyLimit != null ? ': getFmtLimit(monthlyLimit)' : '∞'}</div>
          <div className="stat-desc" style={{ fontSize: '11px' }}>
            <span>已用 {': getFmtTok(monthlyUsed)'}</span>
            {monthlyPct != null && <span style={{ marginLeft: 12 }}>{monthlyPct.toFixed(1)}%</span>}
          </div>
        </div>
      </div>

      {/* 消费趋势 */}
      <div className="section-card">
        <div className="section-card-header">
          <h2>消费趋势（近 30 日）</h2>
        </div>
        <div className="section-card-body">
          {consumptionTrendData && consumptionTrendData.length > 0 ? (
            <TrendChart data={consumptionTrendData} />
          ) : (
            <div className="empty-state" style={{ padding: '22px' }}>
              <p>暂无趋势数据</p>
            </div>
          )}
        </div>
      </div>

      {/* 模型分布 + 用户排行 */}
      <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fit, minmax(400px, 1fr))', gap: 16, marginTop: 16 }}>
        <div className="section-card">
          <div className="section-card-header">
            <h2>模型调用分布</h2>
          </div>
          <div className="section-card-body">
            {modelDistributionData && modelDistributionData.length > 0 ? (
              <PieChart data={modelDistributionData} />
            ) : (
              <div className="empty-state" style={{ padding: '22px' }}>
                <p>暂无模型分布数据</p>
              </div>
            )}
          </div>
        </div>

        <div className="section-card">
          <div className="section-card-header">
            <h2>用户消费排行（Top 10）</h2>
          </div>
          <div className="section-card-body">
            {userRankingData && userRankingData.length > 0 ? (
              <div className="table-wrapper">
                <table>
                  <thead>
                    <tr>
                      <th>#</th>
                      <th>用户</th>
                      <th>请求数</th>
                      <th>Token</th>
                    </tr>
                  </thead>
                  <tbody>
                    {userRankingData.slice(0, 10).map((r, i) => (
                      <tr key={i}>
                        <td style={{ color: 'var(--text-muted)' }}>{i + 1}</td>
                        <td>{r.email || `User#${r.user_id}`}</td>
                        <td>{': getFmtLimit(r.request_count || r.requests || r.count || 0)'}</td>
                        <td>{': getFmtTok(r.total_tokens || r.tokens || 0)'}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            ) : (
              <div className="empty-state" style={{ padding: '22px' }}>
                <p>暂无用户排行数据</p>
              </div>
            )}
          </div>
        </div>
      </div>

      {/* 渠道健康状态 */}
      <div className="section-card" style={{ marginTop: 16 }}>
        <div className="section-card-header">
          <h2>渠道健康状态</h2>
        </div>
        <div className="section-card-body">
          {channelHealthData && channelHealthData.length > 0 ? (
            <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fill, minmax(240px, 1fr))', gap: 12 }}>
              {/* 渠道健康卡片 */}
            </div>
          ) : (
            <div className="empty-state" style={{ padding: '22px' }}>
              <p>暂无渠道健康数据</p>
            </div>
          )}
        </div>
      </div>

      {/* 快捷操作 */}
      <div className="card" style={{ marginTop: 16 }}>
        <div className="card-header">
          <h2>快捷操作</h2>
        </div>
        <div className="card-body quick-actions">
          {/* 操作按钮 */}
        </div>
      </div>
    </div>
  );
}

// 工具函数实现
function getFmtLimit(val: number | null | undefined): string {
  if (val == null) return '—';
  const n = Number(val);
  if (n >= 100000) return (n / 10000).toFixed(1) + 'w';
  if (n >= 1000) return (n / 1000).toFixed(1) + 'K';
  return n.toLocaleString();
}

function getFmtTok(val: number | null | undefined): string {
  if (val == null) return '—';
  const n = Number(val);
  if (n >= 1000000000) return (n / 1000000000).toFixed(1) + 'B';
  if (n >= 1000000) return (n / 1000000).toFixed(1) + 'M';
  if (n >= 1000) return (n / 1000).toFixed(1) + 'K';
  return n.toLocaleString();
}

function getFmtMoney(val: number | null | undefined): string {
  if (val == null) return '—';
  return '¥' + Number(val).toFixed(2);
}

function getTrendVal(d: TrendData): number {
  return d.value ?? d.cost ?? d.tokens ?? 0;
}

// 辅助组件占位
function TrendChart({ data }: TrendChartProps): JSX.Element | null {
  return null;
}

function PieChart({ data }: PieChartProps): JSX.Element | null {
  return null;
}