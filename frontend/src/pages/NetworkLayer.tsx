import { useState, useEffect, useCallback } from 'react';

import { useTranslation } from 'react-i18next';
import { getNetworkStatus, updateNetworkConfig, restartNetwork } from '../api/network';
import type { NetworkStatusRaw, NetworkConfigRequest } from '../types/network';
import { useToast } from '../components/Toast';
import './NetworkLayer.css';

interface NetworkLayerConfig {
  enabled: boolean;
  strategy: string;
  minAccounts: number;
  maxConnections: number;
  maxSessions: number;
}

function fmtLatency(ms: number | null | undefined): string {
  if (ms === undefined || ms === null) return '—';
  return ms.toFixed(1) + 'ms';
}

function fmtPercent(val: number | null | undefined): string {
  if (val === undefined || val === null) return '—';
  const clamped = Math.min(Math.max(val, 0), 1);
  return (clamped * 100).toFixed(1) + '%';
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


// 时间戳格式化（此前缺失，运行时崩溃点）
function formatTimestamp(ts: number | undefined): string {
  if (!ts) return '—';
  return new Date(ts * 1000).toLocaleString();
}

export default function NetworkLayer(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();
  const [loading, setLoading] = useState(true);
  const [refreshing, setRefreshing] = useState(false);
  const [networkStatus, setNetworkStatus] = useState<NetworkStatusRaw | null>(null);
  const [config, setConfig] = useState<NetworkLayerConfig>({
    enabled: true,
    strategy: 'latency-aware',
    minAccounts: 2,
    maxConnections: 10,
    maxSessions: 50,
  });
  const [showSettings, setShowSettings] = useState(false);
  const [showRestartConfirm, setShowRestartConfirm] = useState(false);
  const [restartPending, setRestartPending] = useState(false);

  const fetchStatus = useCallback(async (keepOld = false): Promise<void> => {
    try {
      if (!keepOld) setLoading(true);
      const res = await getNetworkStatus();
      setNetworkStatus(res.data ?? null);
    } catch (error) {
      addToast(t('获取网络状态失败'), 'error');
    } finally {
      setLoading(false);
    }
  }, [addToast, t]);

  useEffect(() => {
    void fetchStatus();
  }, [fetchStatus]);

  const handleRefresh = async (): Promise<void> => {
    try {
      setRefreshing(true);
      await fetchStatus(true);
    } finally {
      setRefreshing(false);
    }
  };

  const handleUpdateConfig = async (): Promise<void> => {
    try {
      setShowSettings(false);
      const payload: NetworkConfigRequest & Record<string, string | number | boolean> = {
        enabled: config.enabled,
        strategy: config.strategy,
        account_pool_min: config.minAccounts,
        account_pool_max: config.minAccounts + 10,
        connection_pool_max: config.maxConnections,
        session_pool_max: config.maxSessions,
      };
      await updateNetworkConfig('default', payload);
      addToast(t('配置已保存'));
      await fetchStatus(true);
    } catch (error) {
      addToast(t('配置保存失败'), 'error');
      setShowSettings(true);
    }
  };

  const handleRestart = async (): Promise<void> => {
    try {
      setRestartPending(true);
      setShowRestartConfirm(false);
      await restartNetwork();
      addToast(t('网络层已重启'));
      await fetchStatus(true);
    } catch (error) {
      addToast(t('网络层重启失败'), 'error');
      setShowRestartConfirm(false);
    } finally {
      setRestartPending(false);
    }
  };

  if (loading) {
    return <div className="loading">加载中...</div>;
  }

  return (
    <div className="space-y-6">
      {networkStatus && (
        <>
          {/* Statistics Cards：Status Summary / 账号池 / 连接池 / 会话池 */}
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
                    <span className="text-green-500">{fmtPercent(networkStatus.account_pool.total_requests > 0 ? (1 - networkStatus.account_pool.failed_requests / networkStatus.account_pool.total_requests) : 1)}</span>
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
                    <span className="text-green-500">{fmtPercent(networkStatus.connection_pool.successful_requests + networkStatus.connection_pool.failed_requests > 0 ? networkStatus.connection_pool.successful_requests / (networkStatus.connection_pool.successful_requests + networkStatus.connection_pool.failed_requests) : 1)}</span>
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
                    <span className="text-blue-500">{networkStatus.session_pool.session_ttl_hours ?? 72} 小时</span>
                  </div>
                </div>
              </div>
            </div>

            {/* Connection Details and Controls */}
            <div className="glass-card">
              <div className="flex items-center justify-between mb-4">
                <h2 className="text-xl font-semibold text-white">高级管理</h2>
                <div className="flex gap-2">
                  <button
                    onClick={handleRefresh}
                    disabled={refreshing}
                    className="p-2 text-gray-400 hover:text-white border border-gray-700 rounded-lg hover:bg-gray-700/50 transition"
                    title={t('刷新')}
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
          </>
        )}

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
