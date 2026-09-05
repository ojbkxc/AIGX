import { useState, useEffect, useCallback } from 'react';
import { api } from '../api';

interface NetworkStatus {
  enabled: boolean;
  account_pool: AccountPoolStatus;
  connection_pool: ConnectionPoolStatus;
  session_pool: SessionPoolStats;
  load_balance_strategy: string;
  last_check_at: number;
}

interface AccountPoolStatus {
  total_accounts: number;
  available_accounts: number;
  busy_accounts: number;
  error_accounts: number;
  invalid_accounts: number;
  total_requests: number;
  failed_requests: number;
}

interface ConnectionPoolStatus {
  total_connections: number;
  active_connections: number;
  idle_connections: number;
  total_connections_created: number;
  total_connections_closed: number;
  successful_requests: number;
  failed_requests: number;
  avg_latency_ms: number;
}

interface SessionPoolStats {
  total_sessions: number;
  active_sessions: number;
  idle_sessions: number;
  session_ttl_hours: number;
}

interface NetworkConfig {
  enabled: boolean;
  strategy: string;
  account_pool_min: number;
  account_pool_max: number;
  connection_pool_max: number;
  session_pool_max: number;
}

export default function NetworkLayer() {
  const [status, setStatus] = useState<NetworkStatus | null>(null);
  const [config, setConfig] = useState<NetworkConfig>({
    enabled: true,
    strategy: 'priority+weighted+circuit',
    account_pool_min: 2,
    account_pool_max: 10,
    connection_pool_max: 10,
    session_pool_max: 50,
  });
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const fetchStatus = useCallback(async () => {
    try {
      const res = await api.network.getStatus();
      setStatus((res.data as unknown) as NetworkStatus);
      setError(null);
    } catch (err) {
      setError('获取网络层状态失败');
      console.error('获取网络层状态失败:', err);
    } finally {
      setLoading(false);
    }
  }, []);

  const fetchConfig = useCallback(async () => {
    try {
      const res = await api.network.getConfig('default');
      const data = (res.data as unknown) as Partial<NetworkConfig>;
      setConfig((prev) => ({ ...prev, ...data }));
    } catch (err) {
      console.error('获取网络层配置失败:', err);
    }
  }, []);

  useEffect(() => {
    void fetchStatus();
    void fetchConfig();
  }, [fetchStatus, fetchConfig]);

  const handleUpdateConfig = async () => {
    try {
      await api.network.updateConfig('default', config.enabled, config.strategy);
      alert('配置更新成功！');
    } catch (err) {
      alert('更新配置失败，请检查网络层状态');
      console.error('更新配置失败:', err);
    }
  };

  const handleRestartNetwork = async () => {
    if (!window.confirm('确定要重启网络层吗？')) return;

    try {
      const res = await api.network.restart();
      const data = (res.data as unknown) as { message?: string } | undefined;
      alert(data?.message || '网络层重启成功');
      void fetchStatus();
    } catch (err) {
      console.error('重启失败:', err);
    }
  };

  if (loading) {
    return <div className="loading">加载中...</div>;
  }

  if (!status) {
    return <div className="empty-state">暂无网络层状态数据{error ? `（${error}）` : ''}</div>;
  }

  return (
    <div>
      <div className="page-header">
        <h1>AIGX 网络层管理</h1>
        <p>管理 AIGX Net 网络层（账号池 / 连接池 / 会话池）</p>
      </div>

      {/* 状态概览 */}
      <div className="status-grid">
        <div className="status-card">
          <h3>网络层状态</h3>
          <div className="status-value">
            {status.enabled ? '🟢 已启用' : '🔴 未启用'}
          </div>
        </div>

        <div className="status-card">
          <h3>账号池</h3>
          <div className="status-item">
            <span>总账号数:</span>
            <strong>{status.account_pool.total_accounts}</strong>
          </div>
          <div className="status-item">
            <span>可用:</span>
            <strong className="text-green">{status.account_pool.available_accounts}</strong>
          </div>
          <div className="status-item">
            <span>使用中:</span>
            <strong className="text-blue">{status.account_pool.busy_accounts}</strong>
          </div>
          <div className="status-item">
            <span>错误:</span>
            <strong className="text-red">{status.account_pool.error_accounts}</strong>
          </div>
        </div>

        <div className="status-card">
          <h3>连接池</h3>
          <div className="status-item">
            <span>总连接数:</span>
            <strong>{status.connection_pool.total_connections}</strong>
          </div>
          <div className="status-item">
            <span>活跃:</span>
            <strong className="text-green">{status.connection_pool.active_connections}</strong>
          </div>
          <div className="status-item">
            <span>空闲:</span>
            <strong>{status.connection_pool.idle_connections}</strong>
          </div>
          <div className="status-item">
            <span>平均延迟:</span>
            <strong>{status.connection_pool.avg_latency_ms.toFixed(1)}ms</strong>
          </div>
        </div>

        <div className="status-card">
          <h3>会话池</h3>
          <div className="status-item">
            <span>总会话:</span>
            <strong>{status.session_pool.total_sessions}</strong>
          </div>
          <div className="status-item">
            <span>活跃:</span>
            <strong className="text-green">{status.session_pool.active_sessions}</strong>
          </div>
          <div className="status-item">
            <span>空闲:</span>
            <strong>{status.session_pool.idle_sessions}</strong>
          </div>
          <div className="status-item">
            <span>TTL设置:</span>
            <strong>{status.session_pool.session_ttl_hours}小时</strong>
          </div>
        </div>
      </div>

      {/* 配置面板 */}
      <div className="glass-card">
        <h3>网络层配置</h3>
        <div className="form-group">
          <label>启用网络层</label>
          <select
            value={config.enabled ? 'enabled' : 'disabled'}
            onChange={(e) => setConfig({
              ...config,
              enabled: e.target.value === 'enabled'
            })}
          >
            <option value="disabled">已禁用</option>
            <option value="enabled">已启用</option>
          </select>
        </div>

        <div className="form-group">
          <label>负载均衡策略</label>
          <select
            value={config.strategy}
            onChange={(e) => setConfig({
              ...config,
              strategy: e.target.value
            })}
          >
            <option value="priority+weighted+circuit">优先级+权重+断路器 (推荐)</option>
            <option value="latency_aware">延迟感知</option>
            <option value="weighted">权重均衡</option>
            <option value="random">随机选择</option>
          </select>
        </div>

        <div className="action-buttons">
          <button onClick={handleUpdateConfig} className="btn-primary">
            更新配置
          </button>
          <button onClick={handleRestartNetwork} className="btn-secondary">
            重启网络层
          </button>
        </div>
      </div>
    </div>
  );
}
