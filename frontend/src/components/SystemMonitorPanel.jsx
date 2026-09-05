import React, { useState, useEffect } from 'react';
import { api as networkApi } from '../api/network';
import { Metrics } from '../types/network';
import SystemMonitorPanel.css';
import { Signal, Activity, Server, Database, Cloud, Percent, Zap } from 'lucide-react';

function MetricCard({ title, value, extra, color }) {
  return (
    <div className="metric-card" style={{ borderColor: color }}>
      <div className="metric-icon" style={{ color }}>
        <Signal size={20} />
      </div>
      <div className="metric-content">
        <div className="metric-title">{title}</div>
        <div className="metric-value">{value}</div>
        {extra && <div className="metric-extra">{extra}</div>}
      </div>
    </div>
  );
}

function TrendChart({ value, max, label }) {
  if (value === null || value === undefined || isNaN(value)) {
    return <span className="text-gray-500">—</span>;
  }

  const percentage = Math.min((value / (max || value * 1.5)) * 100, 100);
  const barClass = percentage < 50 ? 'bg-green' : percentage < 80 ? 'bg-yellow' : 'bg-red';

  return (
    <div className="metric-bar-wrapper">
      <span className="metric-label">{label}</span>
      <div className="metric-bar-container">
        <div className={`metric-bar ${barClass}`} style={{ width: `${percentage}%` }}></div>
      </div>
      <span className="metric-value">{value}</span>
    </div>
  );
}

function StatusIndicator({ status }) {
  const styles = {
    online: { bg: 'bg-green-500', text: 'text-green-500', pulse: 'pulse-green' },
    offline: { bg: 'bg-gray-500', text: 'text-gray-500', pulse: '' },
    syncing: { bg: 'bg-yellow-500', text: 'text-yellow-500', pulse: 'pulse-yellow' },
  };
  const style = styles[status] || styles.offline;

  return (
    <div className={`flex items-center gap-2 ${style.pulse}`}>
      <div className={`w-2 h-2 rounded-full ${style.bg} ${style.pulse === 'pulse-green' ? 'animate-pulse' : ''}`} />
      <span className={`text-xs font-medium ${style.text}`}>{status}</span>
    </div>
  );
}

export default function SystemMonitorPanel() {
  const [loading, setLoading] = useState(true);
  const [refreshing, setRefreshing] = useState(false);
  const [metrics, setMetrics] = useState<Metrics | null>(null);
  const [currentTime, setCurrentTime] = useState(new Date());

  useEffect(() => {
    const timer = setInterval(() => setCurrentTime(new Date()), 1000);
    fetchMetrics();
    return () => clearInterval(timer);
  }, []);

  const fetchMetrics = async () => {
    try {
      setLoading(true);
      const data = await networkApi.getNetworkMetrics();
      setMetrics(data);
    } catch (error) {
      console.error('Failed to fetch metrics:', error);
    } finally {
      setLoading(false);
    }
  };

  const handleRefresh = async () => {
    try {
      setRefreshing(true);
      await fetchMetrics();
    } finally {
      setRefreshing(false);
    }
  };

  if (loading) {
    return (
      <div className="system-monitor-panel">
        <div className="skeleton-loader">
          <div className="skeleton-spinner" />
          <div className="skeleton-bar" style={{ width: '60%' }} />
          <div className="skeleton-bar" style={{ width: '40%' }} />
          <div className="skeleton-bar" style={{ width: '80%' }} />
        </div>
      </div>
    );
  }

  if (!metrics) {
    return null;
  }

  return (
    <div className="system-monitor-panel">
      <div className="panel-header">
        <div className="header-left">
          <Activity className="panel-icon" />
          <h2>系统监控</h2>
        </div>
        <div className="header-right">
          <span className="current-time">{currentTime.toLocaleTimeString()}</span>
          <button
            onClick={handleRefresh}
            className={`refresh-button ${refreshing ? 'refreshing' : ''}`}
            disabled={refreshing}
          >
            <Signal size={16} className={refreshing ? 'animate-spin' : ''} />
          </button>
        </div>
      </div>

      <div className="panel-content">
        {/* CPU & Memory */}
        <div className="metric-row">
          <div className="metric-row-half">
            <MetricCard
              title="CPU 使用率"
              value={`${metrics.cpuUsage}%`}
              color={metrics.cpuUsage > 80 ? '#ef4444' : metrics.cpuUsage > 50 ? '#f59e0b' : '#22c55e'}
              extra={`运行时间: ${formatDuration(metrics.uptime)}`}
            />
          </div>
          <div className="metric-row-middle">
            <MetricCard
              title="内存使用率"
              value={`${metrics.memoryUsage}%`}
              color={metrics.memoryUsage > 80 ? '#ef4444' : metrics.memoryUsage > 50 ? '#f59e0b' : '#22c55e'}
            />
          </div>
          <div className="metric-row-half">
            <MetricCard
              title="磁盘使用率"
              value={`${metrics.diskUsage}%`}
              color={metrics.diskUsage > 80 ? '#ef4444' : metrics.diskUsage > 60 ? '#f59e0b' : '#22c55e'}
            />
          </div>
        </div>

        {/* Network & I/O */}
        <div className="metric-row">
          <div className="metric-row-other">
            <div className="metric-item">
              <div className="metric-with-trend">
                <Database className="metric-icon-bg" size={24} />
                <div className="metric-details">
                  <div className="metric-title">网络吞吐量</div>
                  <div className="metric-value-large">{formatThroughput(metrics.networkTx + metrics.networkRx)}</div>
                  <div className="metric-trend">
                    <TrendChart value={metrics.networkTx + metrics.networkRx} max={1000} label="Tx" />
                    <TrendChart value={metrics.networkRx} max={1000} label="Rx" />
                  </div>
                </div>
              </div>
            </div>
          </div>
          <div className="metric-row-other">
            <MetricCard title="活动连接" value={metrics.activeConnections} extra="总连接数: -" color="#3b82f6" />
          </div>
          <div className="metric-row-other">
            <MetricCard title="吞吐量" value={metrics.throughput} extra="请求/秒" color="#a855f7" />
          </div>
        </div>

        {/* Performance Metrics */}
        <div className="metric-section">
          <div className="section-title">
            <Zap className="section-icon" />
            <h3>性能指标</h3>
          </div>
          <div className="performance-grid">
            {[
              { title: "平均延迟", value: formatLatency(metrics.avgLatency), unit: "ms", color: "#22c55e" },
              { title: "请求成功率", value: formatPercent(metrics.successRate), unit: "%", color: "#3b82f6" },
              { title: "错误率", value: formatPercent(metrics.errorRate), unit: "%", color: "#ef4444" },
              { title: "吞吐量", value: metrics.throughput, unit: "req/s", color: "#a855f7" },
            ].map((metric, index) => (
              <div key={index} className="performance-item">
                <div className="metric-bars">
                  <div className="bar-vertical" style={{ height: `${metric.value}%`, borderColor: metric.color }}>
                    <span className="bar-value">{metric.value}</span>
                  </div>
                </div>
                <div className="metric-info">
                  <div className="metric-title-small">{metric.title}</div>
                  <div className="metric-value-mini">{metric.value} {metric.unit}</div>
                </div>
              </div>
            ))}
          </div>
        </div>

        {/* Nodes Status */}
        <div className="metric-section">
          <div className="section-title">
            <Activity className="section-icon" />
            <h3>分布式节点状态</h3>
          </div>
          <div className="nodes-grid">
            <div className="node-card">
              <div className="node-header">
                <Server className="node-icon" size={20} />
                <span className="node-name">主节点 (Leader)</span>
                <StatusIndicator status="online" />
              </div>
              <div className="node-metrics">
                <div className="node-metric">
                  <span className="metric-label">健康分数</span>
                  <span className="metric-value">98</span>
                </div>
                <div className="node-metric">
                  <span className="metric-label">CPU</span>
                  <span className="metric-value">42%</span>
                </div>
                <div className="node-metric">
                  <span className="metric-label">内存</span>
                  <span className="metric-value">64%</span>
                </div>
              </div>
            </div>
            <div className="node-card">
              <div className="node-header">
                <Cloud className="node-icon" size={20} />
                <span className="node-name">计算节点 A</span>
                <StatusIndicator status="syncing" />
              </div>
              <div className="node-metrics">
                <div className="node-metric">
                  <span className="metric-label">健康分数</span>
                  <span className="metric-value">95</span>
                </div>
                <div className="node-metric">
                  <span className="metric-label">CPU</span>
                  <span className="metric-value">28%</span>
                </div>
                <div className="node-metric">
                  <span className="metric-label">内存</span>
                  <span className="metric-value">52%</span>
                </div>
              </div>
            </div>
            <div className="node-card">
              <div className="node-header">
                <Cloud className="node-icon" size={20} />
                <span className="node-name">计算节点 B</span>
                <StatusIndicator status="online" />
              </div>
              <div className="node-metrics">
                <div className="node-metric">
                  <span className="metric-label">健康分数</span>
                  <span className="metric-value">92</span>
                </div>
                <div className="node-metric">
                  <span className="metric-label">CPU</span>
                  <span className="metric-value">35%</span>
                </div>
                <div className="node-metric">
                  <span className="metric-label">内存</span>
                  <span className="metric-value">78%</span>
                </div>
              </div>
            </div>
          </div>
        </div>

        {/* Scaling Status */}
        <div className="metric-section">
          <div className="section-title">
            <Activity className="section-icon" />
            <h3>自动扩缩容状态</h3>
          </div>
          <div className="scaling-status">
            <div className="scaling-avatar" style={{ background: '#22c55e' }}>
              <Activity size={24} />
            </div>
            <div className="scaling-content">
              <div className="scaling-title">
                <span className="scaling-mode">智能扩缩容</span>
                <span className={`scaling-status-badge primary`}>正常运转</span>
              </div>
              <div className="scaling-details">
                <div className="scaling-stat">
                  <span className="stat-label">扩缩容节点数</span>
                  <span className="stat-value">3/10</span>
                </div>
                <div className="scaling-stat">
                  <span className="stat-label">当前负载</span>
                  <span className="stat-value primary">{metrics.currentLoad}%</span>
                </div>
                <div className="scaling-stat">
                  <span className="stat-label">理想负载</span>
                  <span className="stat-value">100%</span>
                </div>
              </div>
            </div>
            <div className="scaling-bar-background" style={{ width: `${metrics.currentLoad}%` }}>
              <div className="scaling-bar-indicator" style={{ left: `${metrics.currentLoad}%`, borderColor: metrics.currentLoad > 90 ? '#ef4444' : '#22c55e' }} />
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}

function formatDuration(seconds) {
  const days = Math.floor(seconds / 86400);
  const hours = Math.floor((seconds % 86400) / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);

  const parts = [];
  if (days > 0) parts.push(`${days}天`);
  if (hours > 0) parts.push(`${hours}小时`);
  if (minutes > 0) parts.push(`${minutes}分`);

  return parts.length > 0 ? parts.join(' ') : '0秒';
}

function formatLatency(ms) {
  if (ms === null || ms === undefined || isNaN(ms)) return '—';

  if (ms < 1000) {
    return `${ms}ms`;
  }
  return `${(ms / 1000).toFixed(2)}s`;
}

function formatPercent(value) {
  if (value === null || value === undefined || isNaN(value)) return '0%';

  const percent = value * 100;
  if (percent >= 100) return '100%';
  if (percent <= 0) return '0%';

  return `${percent.toFixed(2)}%`;
}

function formatThroughput(bytes) {
  if (bytes === null || bytes === undefined || isNaN(bytes)) return '—';

  if (bytes > 1024 * 1024 * 1024) {
    return `${(bytes / (1024 * 1024 * 1024)).toFixed(2)} TB`;
  }
  if (bytes >= 1024 * 1024) {
    return `${(bytes / (1024 * 1024)).toFixed(2)} MB`;
  }
  if (bytes >= 1024) {
    return `${(bytes / 1024).toFixed(2)} KB`;
  }
  return `${bytes} B/s`;
}