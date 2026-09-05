import React, { useState, useEffect } from 'react';
import { api as networkApi } from '../api/network';
import { NetworkStatus as NetworkStatusType } from '../types/network';
import { getNetworkStatus, updateNetworkConfig, restartNetwork } from '../api/network';
import './NetworkLayer.css';

function fmtLatency(ms) {
  if (ms === undefined || ms === null) return '—';
  return ms.toFixed(1) + 'ms';
}

function fmtPercent(val) {
  if (val === undefined || val === null) return '—';
  if (val > 1) return (val / 100).toFixed(2) + '%';
  return val.toFixed(2) + '%';
}

function RebootIcon() {
  return (
    <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
      <path d="M23 4v6h-6"></path>
      <path d="M1 20v-6h6"></path>
      <path d="M3.51 9a9 9 0 0 1 14.85-3.36L23 10M1 14l4.64 4.36A9 9 0 0 0 20.49 15"></path>
    </svg>
  );
}

function GearIcon() {
  return (
    <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
      <path d="M12 20a8 8 0 1 0 0-16 8 8 0 0 0 0 16Z"></path>
      <path d="M12 14a2 2 0 1 0 0-4 2 2 0 0 0 0 4Z"></path>
      <path d="M12 2v2"></path>
      <path d="M12 22v-2"></path>
      <path d="M17 20H19"></path>
      <path d="M5 20H3"></path>
      <path d="M17 4H19"></path>
      <path d="M5 4H3"></path>
      <path d="m16.95 7.05 1.41-1.41"></path>
      <path d="m6.95 16.95 1.41-1.41"></path>
      <path d="m6.95 7.05 1.41-1.41"></path>
      <path d="m16.95 16.95 1.41 1.41"></path>
    </svg>
  );
}

function HealthCheckIcon({ status }) {
  if (status === 'healthy') {
    return (
      <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="#22c55e" strokeWidth="2">
        <path d="m9 12 2 2 4-4"></path>
        <circle cx="12" cy="12" r="10"></circle>
      </svg>
    );
  } else if (status === 'warning') {
    return (
      <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="#f59e0b" strokeWidth="2">
        <path d="M10.29 3.86L1.82 18a2 2 0 0 0 1.71 3h16.94a2 2 0 0 0 1.71-3L13.71 3.86a2 2 0 0 0-3.42 0Z"></path>
        <path d="m12 9 1 4"></path>
        <path d="m12 17h.01"></path>
      </svg>
    );
  } else if (status === 'error') {
    return (
      <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="#ef4444" strokeWidth="2">
        <path d="m9 12 2 2 4-4"></path>
        <circle cx="12" cy="12" r="10"></circle>
      </svg>
    );
  }
  return null;
}

function StatusBadge({ status }) {
  const styles = {
    enabled: { bg: 'bg-green-500/10', text: 'text-green-500', border: 'border-green-500' },
    disabled: { bg: 'bg-gray-500/10', text: 'text-gray-500', border: 'border-gray-500' },
  };
  const style = styles[status] || styles.enabled;

  return (
    <span className={`inline-flex items-center gap-1 px-2 py-0.5 rounded-full text-xs font-medium border-2 ${style.bg} ${style.text} ${style.border}`}>
      {status === 'enabled' ? '启用中' : '已禁用'}
    </span>
  );
}

function LatencyBars({ latency }) {
  if (latency === undefined || latency === null || isNaN(latency)) {
    return <span className="text-gray-500">—</span>;
  }

  const maxLatency = 150;
  const widthpercent = Math.min((latency / maxLatency) * 100, 100);
  const barClass = latency < 50 ? 'bg-green-500' : latency < 100 ? 'bg-yellow-500' : 'bg-red-500';

  return (
    <div className="flex items-center gap-2">
      <span className="text-xs text-gray-600 w-12">{formatTime(latency)}</span>
      <div className="flex-1 h-1.5 bg-gray-200 rounded-full overflow-hidden">
        <div className={`h-full ${barClass}`} style={{ width: `${widthpercent}%` }}></div>
      </div>
    </div>
  );
}

function formatTime(ms) {
  return ms < 1000 ? ms.toFixed(0) + 'ms' : (ms / 1000).toFixed(2) + 's';
}

export default function NetworkLayer() {
  const [loading, setLoading] = useState(true);
  const [refreshing, setRefreshing] = useState(false);
  const [networkStatus, setNetworkStatus] = useState(null);
  const [config, setConfig] = useState({ enabled: true, strategy: 'latency-aware' });
  const [showSettings, setShowSettings] = useState(false);
  const [showRestartConfirm, setShowRestartConfirm] = useState(false);
  const [restartPending, setRestartPending] = useState(false);

  useEffect(() => {
    fetchStatus();
  }, []);

  const fetchStatus = async () => {
    try {
      setLoading(true);
      const data = await getNetworkStatus();
      setNetworkStatus(data);
    } catch (error) {
      console.error('Failed to fetch network status:', error);
    } finally {
      setLoading(false);
    }
  };

  const handleRefresh = async () => {
    try {
      setRefreshing(true);
      await fetchStatus();
    } finally {
      setRefreshing(false);
    }
  };

  const handleUpdateConfig = async () => {
    try {
      setShowSettings(false);
      await updateNetworkConfig('default', config);
      await fetchStatus();
    } catch (error) {
      console.error('Failed to update config:', error);
      setShowSettings(true);
    }
  };

  const handleRestart = async () => {
    try {
      setRestartPending(true);
      await restartNetwork();
      await fetchStatus();
      setShowRestartConfirm(false);
    } catch (error) {
      console.error('Failed to restart network:', error);
      setShowRestartConfirm(false);
    } finally {
      setRestartPending(false);
    }
  };

  if (loading) {
    return (
      <div className="min-h-screen bg-gradient-to-br from-slate-900 via-slate-800 to-slate-900 p-6 flex flex-col items-center justify-center">
        <div className="w-16 h-16 border-4 border-green-500/30 border-t-green-500 rounded-full animate-spin"></div>
        <p className="mt-4 text-gray-400">加载中...</p>
      </div>
    );
  }

  return (
    <div className="min-h-screen bg-gradient-to-br from-slate-900 via-slate-800 to-slate-900 p-6">
      <div className="max-w-7xl mx-auto space-y-6">
        {/* Header */}
        <div className="flex items-center justify-between">
          <div>
            <h1 className="text-3xl font-bold text-white mb-2">AI网关网络层管理</h1>
            <p className="text-gray-400">账号池 · 连接池 · 会话池 · 智能路由</p>
          </div>
          <div className="flex items-center gap-3">
            <StatusBadge status="enabled" />
            <button
              onClick={handleRefresh}
              disabled={refreshing}
              className="p-2 text-gray-400 hover:text-white border border-gray-700 rounded-lg hover:bg-gray-700/50 transition"
              title="刷新"
            >
              {refreshing ? (
                <svg className="animate-spin h-5 w-5 text-green-500" xmlns="http://www.w3.org/2000/svg" fill="none" viewBox="0 0 24 24">
                  <circle className="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" strokeWidth="4"></circle>
                  <path className="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4zm2 5.291A7.962 7.962 0 014 12H0c0 3.042 1.135 5.824 3 7.938l3-2.647z"></path>
                </svg>
              ) : (
                <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
                  <path d="M23 4v6h-6"></path>
                  <path d="M1 20v-6h6"></path>
                  <path d="M3.51 9a9 9 0 0 1 14.85-3.36L23 10M1 14l4.64 4.36A9 9 0 0 0 20.49 15"></path>
                </svg>
              )}
            </button>
          </div>
        </div>

        {/* Statistics Cards */}
        {networkStatus && (
          <>
            {/* Status Summary */}
            <div className="grid grid-cols-1 md:grid-cols-4 gap-4">
              <div className="glass-card p-5">
                <h3 className="text-gray-400 text-sm font-medium mb-3">整体状态</h3>
                <div className="space-y-2">
                  <div className="flex items-center justify-between">
                    <span className="text-gray-300">负载均衡策略</span>
                    <span className="text-white font-medium">{networkStatus.load_balance_strategy || 'latency-aware'}</span>
                  </div>
                  <div className="flex items-center justify-between">
                    <span className="text-gray-300">最后检查</span>
                    <span className="text-green-500 text-sm">{formatTimestamp(networkStatus.last_check_at)}</span>
                  </div>
                </div>
              </div>

              {/* Account Pool */}
              <div className="glass-card p-5">
                <div className="flex items-center justify-between mb-3">
                  <h3 className="text-gray-400 text-sm font-medium">账号池状态</h3>
                  <div className="w-2 h-2 bg-green-500 rounded-full animate-pulse"></div>
                </div>
                <div className="grid grid-cols-2 gap-3">
                  <div>
                    <p className="text-2xl font-bold text-white">{networkStatus.account_pool.total_accounts}</p>
                    <p className="text-xs text-gray-400">总账号数</p>
                  </div>
                  <div>
                    <p className="text-2xl font-bold text-green-500">{networkStatus.account_pool.available_accounts}</p>
                    <p className="text-xs text-gray-400">可用账号</p>
                  </div>
                  <div>
                    <p className="text-2xl font-bold text-yellow-500">{networkStatus.account_pool.busy_accounts}</p>
                    <p className="text-xs text-gray-400">使用中</p>
                  </div>
                  <div>
                    <p className="text-2xl font-bold text-red-500">{networkStatus.account_pool.error_accounts}</p>
                    <p className="text-xs text-gray-400">错误账号</p>
                  </div>
                </div>
                <div className="mt-3 pt-3 border-t border-gray-700">
                  <div className="flex items-center justify-between text-xs text-gray-400">
                    <span>请求成功率</span>
                    <span className="text-green-500">{fmtPercent(networkStatus.total_requests > 0 ? (1 - networkStatus.failed_requests / networkStatus.total_requests) : 1)}</span>
                  </div>
                </div>
              </div>

              {/* Connection Pool */}
              <div className="glass-card p-5">
                <div className="flex items-center justify-between mb-3">
                  <h3 className="text-gray-400 text-sm font-medium">连接池状态</h3>
                  <div className="w-2 h-2 bg-blue-500 rounded-full animate-pulse"></div>
                </div>
                <div className="grid grid-cols-2 gap-3">
                  <div>
                    <p className="text-2xl font-bold text-white">{networkStatus.connection_pool.total_connections}</p>
                    <p className="text-xs text-gray-400">总连接数</p>
                  </div>
                  <div>
                    <p className="text-2xl font-bold text-blue-500">{networkStatus.connection_pool.active_connections}</p>
                    <p className="text-xs text-gray-400">活跃连接</p>
                  </div>
                  <div>
                    <p className="text-2xl font-bold text-yellow-500">{networkStatus.connection_pool.idle_connections}</p>
                    <p className="text-xs text-gray-400">空闲连接</p>
                  </div>
                  <div>
                    <p className="text-2xl font-bold text-purple-500">{fmtLatency(networkStatus.connection_pool.avg_latency_ms)}</p>
                    <p className="text-xs text-gray-400">平均延迟</p>
                  </div>
                </div>
                <div className="mt-3 pt-3 border-t border-gray-700">
                  <div className="flex items-center justify-between text-xs text-gray-400">
                    <span>请求成功率</span>
                    <span className="text-green-500">{fmtPercent(networkStatus.connection_pool.successful_requests / networkStatus.connection_pool.total_requests)}</span>
                  </div>
                </div>
              </div>

              {/* Session Pool */}
              <div className="glass-card p-5">
                <div className="flex items-center justify-between mb-3">
                  <h3 className="text-gray-400 text-sm font-medium">会话池状态</h3>
                  <div className="w-2 h-2 bg-purple-500 rounded-full animate-pulse"></div>
                </div>
                <div className="grid grid-cols-2 gap-3">
                  <div>
                    <p className="text-2xl font-bold text-white">{networkStatus.session_pool.total_sessions}</p>
                    <p className="text-xs text-gray-400">总会话数</p>
                  </div>
                  <div>
                    <p className="text-2xl font-bold text-purple-500">{networkStatus.session_pool.active_sessions}</p>
                    <p className="text-xs text-gray-400">活跃会话</p>
                  </div>
                  <div>
                    <p className="text-2xl font-bold text-yellow-500">{networkStatus.session_pool.idle_sessions}</p>
                    <p className="text-xs text-gray-400">空闲会话</p>
                  </div>
                  <div></div>
                </div>
                <div className="mt-3 pt-3 border-t border-gray-700">
                  <div className="flex items-center justify-between text-xs text-gray-400">
                    <span>会话 TTL</span>
                    <span className="text-blue-500">{networkStatus.session_pool.session_ttl_hours} 小时</span>
                  </div>
                </div>
              </div>
            </div>

            {/* Connection Details and Controls */}
            <div className="glass-card">
              <div className="flex items-center justify-between mb-4">
                <h2 className="text-xl font-semibold text-white">高级管理</h2>
                <div className="flex gap-2">
                  {showSettings ? (
                    <button
                      onClick={handleUpdateConfig}
                      className="px-4 py-2 bg-green-500 text-white rounded-lg hover:bg-green-600 transition font-medium text-sm"
                    >
                      保存配置
                    </button>
                  ) : (
                    <button
                      onClick={() => setShowSettings(true)}
                      className="px-4 py-2 bg-blue-500 text-white rounded-lg hover:bg-blue-600 transition font-medium text-sm"
                    >
                     配置参数
                    </button>
                  )}
                </div>
              </div>

              {/* Config Settings */}
              {showSettings && (
                <div className="bg-gray-800/50 rounded-xl p-5 mb-4">
                  <h3 className="text-lg font-medium text-white mb-4">网络层配置</h3>
                  <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
                    <div>
                      <label className="block text-sm font-medium text-gray-300 mb-2">负载均衡策略</label>
                      <select
                        value={config.strategy}
                        onChange={(e) => setConfig({ ...config, strategy: e.target.value })}
                        className="w-full bg-gray-900 border border-gray-700 rounded-lg px-4 py-2 text-white focus:ring-2 focus:ring-blue-500 focus:border-transparent"
                      >
                        <option value="latency-aware">延迟感知</option>
                        <option value="weighted">权重均衡</option>
                        <option value="random">随机选择</option>
                        <option value="least-loaded">最空闲优先</option>
                      </select>
                    </div>
                    <div>
                      <label className="block text-sm font-medium text-gray-300 mb-2">账号池大小</label>
                      <input
                        type="range"
                        min="2"
                        max="20"
                        value={config.minAccounts || 2}
                        onChange={(e) => setConfig({ ...config, minAccounts: parseInt(e.target.value) })}
                        className="w-full"
                      />
                      <span className="text-gray-400 text-sm">{config.minAccounts} - {config.minAccounts + 10}</span>
                    </div>
                    <div>
                      <label className="block text-sm font-medium text-gray-300 mb-2">连接池大小</label>
                      <input
                        type="range"
                        min="5"
                        max="50"
                        value={config.maxConnections || 10}
                        onChange={(e) => setConfig({ ...config, maxConnections: parseInt(e.target.value) })}
                        className="w-full"
                      />
                      <span className="text-gray-400 text-sm">{config.maxConnections}</span>
                    </div>
                    <div>
                      <label className="block text-sm font-medium text-gray-300 mb-2">会话池大小</label>
                      <input
                        type="range"
                        min="10"
                        max="100"
                        value={config.maxSessions || 50}
                        onChange={(e) => setConfig({ ...config, maxSessions: parseInt(e.target.value) })}
                        className="w-full"
                      />
                      <span className="text-gray-400 text-sm">{config.maxSessions}</span>
                    </div>
                  </div>
                </div>
              )}

              {/* Action Buttons */}
              <div className="flex items-center gap-4">
                <button
                  onClick={() => setShowRestartConfirm(true)}
                  disabled={restartPending}
                  className="px-6 py-2.5 bg-red-500/10 text-red-400 border border-red-500/50 rounded-lg hover:bg-red-500/20 transition font-medium text-sm flex items-center gap-2"
                >
                  {restartPending ? (
                    <>
                      <svg className="animate-spin h-4 w-4 mr-2" xmlns="http://www.w3.org/2000/svg" fill="none" viewBox="0 0 24 24">
                        <circle className="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" strokeWidth="4"></circle>
                        <path className="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4zm2 5.291A7.962 7.962 0 014 12H0c0 3.042 1.135 5.824 3 7.938l3-2.647z"></path>
                      </svg>
                      重启中...
                    </>
                  ) : (
                    <>
                      <RebootIcon />
                      重启网络层
                    </>
                  )}
                </button>
                <button
                  onClick={() => setShowSettings(true)}
                  className="px-6 py-2.5 bg-blue-500/10 text-blue-400 border border-blue-500/50 rounded-lg hover:bg-blue-500/20 transition font-medium text-sm flex items-center gap-2"
                >
                  <GearIcon />
                  配置调整
                </button>
              </div>
            </div>

            {/* Connection Performance Overview */}
            <div className="grid grid-cols-1 lg:grid-cols-2 gap-6">
              {/* Protocol Distribution */}
              <div className="glass-card">
                <h3 className="text-lg font-semibold text-white mb-4">协议使用分布</h3>
                <div className="space-y-4">
                  {[
                    { name: 'TCP', icon: '🔗', count: networkStatus.connection_pool.active_connections, total: networkStatus.connection_pool.total_connections },
                    { name: 'WebSocket', icon: '📡', count: Math.floor(networkStatus.connection_pool.active_connections * 0.4), total: networkStatus.connection_pool.total_connections },
                    { name: 'KCP', icon: '🚀', count: Math.floor(networkStatus.connection_pool.active_connections * 0.3), total: networkStatus.connection_pool.total_connections },
                  ].map((protocol, index) => (
                    <div key={index} className="flex items-center gap-3">
                      <span className="text-2xl">{protocol.icon}</span>
                      <div className="flex-1">
                        <div className="flex items-center justify-between mb-1">
                          <span className="text-gray-300 font-medium">{protocol.name}</span>
                          <span className="text-gray-400 text-sm">{protocol.count}/{protocol.total}</span>
                        </div>
                        <div className="w-full h-2 bg-gray-800 rounded-full overflow-hidden">
                          <div
                            className="h-full bg-gradient-to-r from-blue-500 to-cyan-400 transition-all"
                            style={{ width: `${(protocol.count / protocol.total) * 100}%` }}
                          ></div>
                        </div>
                      </div>
                    </div>
                  ))}
                </div>
              </div>

              {/* Connection Quality */}
              <div className="glass-card">
                <h3 className="text-lg font-semibold text-white mb-4">连接质量分析</h3>
                <div className="grid grid-cols-2 gap-4">
                  <div className="bg-gray-800/30 rounded-xl p-4">
                    <p className="text-gray-400 text-sm mb-2">吞吐量</p>
                    <p className="text-2xl font-bold text-white">245.3</p>
                    <p className="text-xs text-green-500">请求/秒</p>
                  </div>
                  <div className="bg-gray-800/30 rounded-xl p-4">
                    <p className="text-gray-400 text-sm mb-2">平均延迟</p>
                    <p className="text-2xl font-bold text-white">{fmtLatency(networkStatus.connection_pool.avg_latency_ms)}</p>
                    <p className="text-xs text-yellow-500">目标: <100ms</p>
                  </div>
                  <div className="bg-gray-800/30 rounded-xl p-4">
                    <p className="text-gray-400 text-sm mb-2">成功率</p>
                    <p className="text-2xl font-bold text-white">{fmtPercent(networkStatus.connection_pool.successful_requests / networkStatus.connection_pool.total_requests)}</p>
                    <p className="text-xs text-green-500">99.9%+</p>
                  </div>
                  <div className="bg-gray-800/30 rounded-xl p-4">
                    <p className="text-gray-400 text-sm mb-2">资源利用率</p>
                    <p className="text-2xl font-bold text-white">67%</p>
                    <p className="text-xs text-yellow-500">正常范围: 50-80%</p>
                  </div>
                </div>
              </div>
            </div>

            {/* Recent Alerts and Health Status */}
            <div className="grid grid-cols-1 lg:grid-cols-3 gap-6">
              {/* Health Status */}
              <div className="glass-card lg:col-span-2">
                <h3 className="text-lg font-semibold text-white mb-4">系统健康状态</h3>
                <div className="grid grid-cols-2 gap-4">
                  <div className="bg-green-500/10 border border-green-500/50 rounded-xl p-4 flex items-center gap-4">
                    <div className="w-12 h-12 bg-green-500/20 rounded-lg flex items-center justify-center">
                      <svg className="w-6 h-6 text-green-500" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                        <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M5 13l4 4L19 7" />
                      </svg>
                    </div>
                    <div>
                      <p className="text-white font-medium">整体健康</p>
                      <p className="text-green-500 text-sm">优秀</p>
                    </div>
                  </div>
                  <div className="bg-blue-500/10 border border-blue-500/50 rounded-xl p-4 flex items-center gap-4">
                    <div className="w-12 h-12 bg-blue-500/20 rounded-lg flex items-center justify-center">
                      <svg className="w-6 h-6 text-blue-500" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                        <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M9 12l2 2 4-4m6 2a9 9 0 11-18 0 9 9 0 0118 0z" />
                      </svg>
                    </div>
                    <div>
                      <p className="text-white font-medium">连接状态</p>
                      <p className="text-blue-500 text-sm">正常</p>
                    </div>
                  </div>
                  <div className="bg-purple-500/10 border border-purple-500/50 rounded-xl p-4 flex items-center gap-4">
                    <div className="w-12 h-12 bg-purple-500/20 rounded-lg flex items-center justify-center">
                      <svg className="w-6 h-6 text-purple-500" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                        <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M13 10V3L4 14h7v7l9-11h-7z" />
                      </svg>
                    </div>
                    <div>
                      <p className="text-white font-medium">处理能力</p>
                      <p className="text-purple-500 text-sm">高效</p>
                    </div>
                  </div>
                  <div className="bg-cyan-500/10 border border-cyan-500/50 rounded-xl p-4 flex items-center gap-4">
                    <div className="w-12 h-12 bg-cyan-500/20 rounded-lg flex items-center justify-center">
                      <svg className="w-6 h-6 text-cyan-500" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                        <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M9 5H7a2 2 0 00-2 2v12a2 2 0 002 2h10a2 2 0 002-2V7a2 2 0 00-2-2h-2M9 5a2 2 0 002 2h2a2 2 0 002-2M9 5a2 2 0 012-2h2a2 2 0 012 2" />
                      </svg>
                    </div>
                    <div>
                      <p className="text-white font-medium">负载均衡</p>
                      <p className="text-cyan-500 text-sm">智能</p>
                    </div>
                  </div>
                </div>
              </div>

              {/* Quick Actions */}
              <div className="glass-card">
                <h3 className="text-lg font-semibold text-white mb-4">快捷操作</h3>
                <div className="space-y-3">
                  <button className="w-full p-3 bg-gray-800/50 border border-gray-700 rounded-lg hover:bg-gray-700/50 transition text-left flex items-center justify-between group">
                    <span className="text-gray-300 group-hover:text-white">查看详细指标</span>
                    <svg width="18" height="18" className="text-gray-400 group-hover:text-white" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                      <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M9 5l7 7-7 7" />
                    </svg>
                  </button>
                  <button className="w-full p-3 bg-gray-800/50 border border-gray-700 rounded-lg hover:bg-gray-700/50 transition text-left flex items-center justify-between group">
                    <span className="text-gray-300 group-hover:text-white">查看日志</span>
                    <svg width="18" height="18" className="text-gray-400 group-hover:text-white" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                      <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M9 5l7 7-7 7" />
                    </svg>
                  </button>
                  <button className="w-full p-3 bg-gray-800/50 border border-gray-700 rounded-lg hover:bg-gray-700/50 transition text-left flex items-center justify-between group">
                    <span className="text-gray-300 group-hover:text-white">查看告警</span>
                    <svg width="18" height="18" className="text-gray-400 group-hover:text-white" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                      <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M9 5l7 7-7 7" />
                    </svg>
                  </button>
                  <button className="w-full p-3 bg-gray-800/50 border border-gray-700 rounded-lg hover:bg-gray-700/50 transition text-left flex items-center justify-between group">
                    <span className="text-gray-300 group-hover:text-white">配置查看</span>
                    <svg width="18" height="18" className="text-gray-400 group-hover:text-white" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                      <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M9 5l7 7-7 7" />
                    </svg>
                  </button>
                </div>
              </div>
            </div>
          </>
        )}
      </div>

      {/* Restart Confirmation Dialog */}
      {showRestartConfirm && (
        <div className="fixed inset-0 bg-black/50 flex items-center justify-center p-4 z-50">
          <div className="bg-gray-900 border border-gray-700 rounded-2xl p-6 max-w-md w-full">
            <div className="text-center">
              <div className="w-16 h-16 mx-auto mb-4 bg-red-500/20 rounded-full flex items-center justify-center">
                <svg className="w-8 h-8 text-red-500" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                  <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M12 9v2m0 4h.01m-6.938 4h13.856c1.54 0 2.502-1.667 1.732-3L13.732 4c-.77-1.333-2.694-1.333-3.464 0L3.34 16c-.77 1.333.192 3 1.732 3z" />
                </svg>
              </div>
              <h3 className="text-xl font-semibold text-white mb-2">确认重启网络层</h3>
              <p className="text-gray-400 mb-6">
                重启后将重置所有渠道的断路器和健康追踪状态，并可能会短暂中断服务。确定要继续吗？
              </p>
              <div className="flex gap-3 justify-center">
                <button
                  onClick={() => setShowRestartConfirm(false)}
                  disabled={restartPending}
                  className="px-6 py-2.5 bg-gray-700 text-white rounded-lg hover:bg-gray-600 transition font-medium text-sm"
                >
                  取消
                </button>
                <button
                  onClick={handleRestart}
                  disabled={restartPending}
                  className="px-6 py-2.5 bg-red-500 text-white rounded-lg hover:bg-red-600 transition font-medium text-sm"
                >
                  {restartPending ? '重启中...' : '确认重启'}
                </button>
              </div>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}