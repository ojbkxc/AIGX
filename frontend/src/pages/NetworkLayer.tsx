import { useState, useEffect, useCallback } from 'react';

import { useTranslation } from 'react-i18next';
import {
  getNetworkStatus,
  updateNetworkConfig,
  restartNetwork,
  listNetworkAccounts,
  addNetworkAccount,
  removeNetworkAccount,
} from '../api/network';
import type { NetworkStatusRaw, NetworkAccount } from '../types/network';
import { useToast } from '../components/Toast';
import './NetworkLayer.css';

/** 网络层持久化配置（与后端 NetworkLayerConfig 对齐） */
interface NetworkLayerConfig {
  enabled: boolean;
  strategy: string;
  account_pool_min: number;
  account_pool_max: number;
  connection_pool_max: number;
  session_pool_max: number;
}

const DEFAULT_CONFIG: NetworkLayerConfig = {
  enabled: true,
  strategy: 'priority+weighted+circuit',
  account_pool_min: 2,
  account_pool_max: 10,
  connection_pool_max: 10,
  session_pool_max: 50,
};

const STRATEGY_OPTIONS = [
  { value: 'priority+weighted+circuit', labelKey: '优先级+权重+断路器' },
  { value: 'latency-aware', labelKey: '延迟感知' },
  { value: 'weighted', labelKey: '权重均衡' },
  { value: 'random', labelKey: '随机选择' },
  { value: 'least-loaded', labelKey: '最空闲优先' },
];

function fmtLatency(ms: number | null | undefined): string {
  if (ms === undefined || ms === null) return '—';
  return ms.toFixed(1) + 'ms';
}

function fmtPercent(val: number | null | undefined): string {
  if (val === undefined || val === null) return '—';
  const clamped = Math.min(Math.max(val, 0), 1);
  return (clamped * 100).toFixed(1) + '%';
}

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
  const [config, setConfig] = useState<NetworkLayerConfig>(DEFAULT_CONFIG);
  const [showSettings, setShowSettings] = useState(false);
  const [saving, setSaving] = useState(false);
  const [showRestartConfirm, setShowRestartConfirm] = useState(false);
  const [restartPending, setRestartPending] = useState(false);
  const [accounts, setAccounts] = useState<NetworkAccount[]>([]);
  const [showAccountForm, setShowAccountForm] = useState(false);
  const [accountForm, setAccountForm] = useState({ name: '', accountId: '', apiToken: '' });
  const [toggling, setToggling] = useState(false);

  const fetchStatus = useCallback(async (keepOld = false): Promise<void> => {
    try {
      if (!keepOld) setLoading(true);
      const res = await getNetworkStatus();
      const data = res.data ?? null;
      setNetworkStatus(data);
      // 用后端持久化配置初始化表单（真实生效值）
      if (data?.config) {
        setConfig({
          enabled: data.config.enabled,
          strategy: data.config.strategy,
          account_pool_min: data.config.account_pool_min,
          account_pool_max: data.config.account_pool_max,
          connection_pool_max: data.config.connection_pool_max,
          session_pool_max: data.config.session_pool_max,
        });
      }
      try {
        const accRes = await listNetworkAccounts();
        setAccounts(accRes.data ?? []);
      } catch {
        // 账号列表失败不阻塞状态面板
      }
    } catch {
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

  /** 启停开关：立即持久化 enabled，数据面闸门随之后效 */
  const handleToggle = async (): Promise<void> => {
    const next = !config.enabled;
    setToggling(true);
    try {
      await updateNetworkConfig('default', { enabled: next, strategy: config.strategy });
      setConfig((c) => ({ ...c, enabled: next }));
      addToast(next ? t('网络层已启用，转发恢复') : t('网络层已停用，推理转发被拒绝'));
      await fetchStatus(true);
    } catch {
      addToast(t('切换失败，请重试'), 'error');
    } finally {
      setToggling(false);
    }
  };

  /** 保存全部配置（enabled + strategy + 池参数）持久化 */
  const handleUpdateConfig = async (): Promise<void> => {
    setSaving(true);
    try {
      await updateNetworkConfig('default', {
        enabled: config.enabled,
        strategy: config.strategy,
        account_pool_min: config.account_pool_min,
        account_pool_max: config.account_pool_max,
        connection_pool_max: config.connection_pool_max,
        session_pool_max: config.session_pool_max,
      });
      addToast(t('配置已保存并持久化'));
      setShowSettings(false);
      await fetchStatus(true);
    } catch {
      addToast(t('配置保存失败'), 'error');
    } finally {
      setSaving(false);
    }
  };

  const handleRestart = async (): Promise<void> => {
    try {
      setRestartPending(true);
      setShowRestartConfirm(false);
      await restartNetwork();
      addToast(t('网络层已重启（断路器与健康状态已复位）'));
      await fetchStatus(true);
    } catch {
      addToast(t('网络层重启失败'), 'error');
      setShowRestartConfirm(false);
    } finally {
      setRestartPending(false);
    }
  };

  const handleAddAccount = async (): Promise<void> => {
    if (!accountForm.name || !accountForm.accountId || !accountForm.apiToken) {
      addToast(t('名称、账号 ID 与 Token 均必填'), 'error');
      return;
    }
    try {
      await addNetworkAccount(accountForm.accountId, {
        name: accountForm.name,
        accountId: accountForm.accountId,
        apiToken: accountForm.apiToken,
        status: 'active',
        priority: 0,
      });
      addToast(t('账号已添加'));
      setAccountForm({ name: '', accountId: '', apiToken: '' });
      setShowAccountForm(false);
      await fetchStatus(true);
    } catch {
      addToast(t('账号添加失败'), 'error');
    }
  };

  const handleRemoveAccount = async (accountId: string): Promise<void> => {
    try {
      await removeNetworkAccount(accountId);
      addToast(t('账号已删除'));
      await fetchStatus(true);
    } catch {
      addToast(t('账号删除失败'), 'error');
    }
  };

  if (loading) {
    return <div className="loading">{t('加载中...')}</div>;
  }

  const ap = networkStatus?.account_pool;
  const cp = networkStatus?.connection_pool;
  const sp = networkStatus?.session_pool;

  return (
    <div className="space-y-6">
      {networkStatus && (
        <>
          {/* Statistics Cards：整体状态 / 账号池 / 连接池 / 会话池 */}
          <div className="grid grid-cols-1 md:grid-cols-4 gap-4">
              <div className="glass-card p-5">
                <h3 className="text-gray-400 text-sm font-medium mb-3">{t('整体状态')}</h3>
                <div className="space-y-2">
                  <div className="flex items-center justify-between">
                    <span className="text-gray-300">{t('运行开关')}</span>
                    <button
                      type="button"
                      onClick={handleToggle}
                      disabled={toggling}
                      className={`relative inline-flex h-6 w-11 items-center rounded-full transition-colors ${config.enabled ? 'bg-green-500' : 'bg-gray-600'}`}
                      title={config.enabled ? t('点击停用：拒绝所有推理转发') : t('点击启用：恢复推理转发')}
                    >
                      <span
                        className={`inline-block h-4 w-4 transform rounded-full bg-white transition-transform ${config.enabled ? 'translate-x-6' : 'translate-x-1'}`}
                      />
                    </button>
                  </div>
                  <div className="flex items-center justify-between">
                    <span className="text-gray-300">{t('负载均衡策略')}</span>
                    <span className="text-white font-medium">{networkStatus.load_balance_strategy || config.strategy}</span>
                  </div>
                  <div className="flex items-center justify-between">
                    <span className="text-gray-300">{t('最后检查')}</span>
                    <span className="text-green-500 text-sm">{formatTimestamp(networkStatus.last_check_at)}</span>
                  </div>
                </div>
              </div>

              {/* Account Pool */}
              <div className="glass-card p-5">
                <div className="flex items-center justify-between mb-3">
                  <h3 className="text-gray-400 text-sm font-medium">{t('账号池状态')}</h3>
                  <div className="w-2 h-2 bg-green-500 rounded-full animate-pulse"></div>
                </div>
                <div className="grid grid-cols-2 gap-3">
                  <div>
                    <p className="text-2xl font-bold text-white">{ap?.total_accounts ?? 0}</p>
                    <p className="text-xs text-gray-400">{t('总账号数')}</p>
                  </div>
                  <div>
                    <p className="text-2xl font-bold text-green-500">{ap?.available_accounts ?? 0}</p>
                    <p className="text-xs text-gray-400">{t('可用账号')}</p>
                  </div>
                  <div>
                    <p className="text-2xl font-bold text-yellow-500">{ap?.busy_accounts ?? 0}</p>
                    <p className="text-xs text-gray-400">{t('使用中')}</p>
                  </div>
                  <div>
                    <p className="text-2xl font-bold text-red-500">{ap?.error_accounts ?? 0}</p>
                    <p className="text-xs text-gray-400">{t('错误账号')}</p>
                  </div>
                </div>
                <div className="mt-3 pt-3 border-t border-gray-700">
                  <div className="flex items-center justify-between text-xs text-gray-400">
                    <span>{t('今日请求')}</span>
                    <span className="text-green-500">{ap?.total_requests ?? 0}</span>
                  </div>
                </div>
              </div>

              {/* Connection Pool */}
              <div className="glass-card p-5">
                <div className="flex items-center justify-between mb-3">
                  <h3 className="text-gray-400 text-sm font-medium">{t('连接池状态')}</h3>
                  <div className="w-2 h-2 bg-blue-500 rounded-full animate-pulse"></div>
                </div>
                <div className="grid grid-cols-2 gap-3">
                  <div>
                    <p className="text-2xl font-bold text-white">{cp?.total_connections ?? 0}</p>
                    <p className="text-xs text-gray-400">{t('总渠道数')}</p>
                  </div>
                  <div>
                    <p className="text-2xl font-bold text-blue-500">{cp?.active_connections ?? 0}</p>
                    <p className="text-xs text-gray-400">{t('启用渠道')}</p>
                  </div>
                  <div>
                    <p className="text-2xl font-bold text-yellow-500">{cp?.idle_connections ?? 0}</p>
                    <p className="text-xs text-gray-400">{t('停用渠道')}</p>
                  </div>
                  <div>
                    <p className="text-2xl font-bold text-purple-500">{fmtLatency(cp?.avg_latency_ms)}</p>
                    <p className="text-xs text-gray-400">{t('加权 P95 延迟')}</p>
                  </div>
                </div>
                <div className="mt-3 pt-3 border-t border-gray-700">
                  <div className="flex items-center justify-between text-xs text-gray-400">
                    <span>{t('当日建流成功率')}</span>
                    <span className="text-green-500">
                      {fmtPercent(
                        cp && cp.successful_requests + cp.failed_requests > 0
                          ? cp.successful_requests / (cp.successful_requests + cp.failed_requests)
                          : 1,
                      )}
                    </span>
                  </div>
                </div>
              </div>

              {/* Session Pool */}
              <div className="glass-card p-5">
                <div className="flex items-center justify-between mb-3">
                  <h3 className="text-gray-400 text-sm font-medium">{t('会话池状态')}</h3>
                  <div className="w-2 h-2 bg-purple-500 rounded-full animate-pulse"></div>
                </div>
                <div className="grid grid-cols-2 gap-3">
                  <div>
                    <p className="text-2xl font-bold text-white">{sp?.total_sessions ?? 0}</p>
                    <p className="text-xs text-gray-400">{t('总会话数')}</p>
                  </div>
                  <div>
                    <p className="text-2xl font-bold text-purple-500">{sp?.active_sessions ?? 0}</p>
                    <p className="text-xs text-gray-400">{t('活跃用户')}</p>
                  </div>
                  <div>
                    <p className="text-2xl font-bold text-yellow-500">{sp?.idle_sessions ?? 0}</p>
                    <p className="text-xs text-gray-400">{t('空闲会话')}</p>
                  </div>
                  <div></div>
                </div>
                <div className="mt-3 pt-3 border-t border-gray-700">
                  <div className="flex items-center justify-between text-xs text-gray-400">
                    <span>{t('会话 TTL')}</span>
                    <span className="text-blue-500">{sp?.session_ttl_hours ?? 72} {t('小时')}</span>
                  </div>
                </div>
              </div>
            </div>

            {/* Connection Details and Controls */}
            <div className="glass-card">
              <div className="flex items-center justify-between mb-4">
                <h2 className="text-xl font-semibold text-white">{t('高级管理')}</h2>
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
                      disabled={saving}
                      className="px-4 py-2 bg-green-500 text-white rounded-lg hover:bg-green-600 transition font-medium text-sm disabled:opacity-50"
                    >
                      {saving ? t('保存中...') : t('保存配置')}
                    </button>
                  ) : (
                    <button
                      onClick={() => setShowSettings(true)}
                      className="px-4 py-2 bg-blue-500 text-white rounded-lg hover:bg-blue-600 transition font-medium text-sm"
                    >
                      {t('配置参数')}
                    </button>
                  )}
                </div>
              </div>

              {/* Config Settings（持久化配置，重启保留） */}
              {showSettings && (
                <div className="bg-gray-800/50 rounded-xl p-5 mb-4">
                  <h3 className="text-lg font-medium text-white mb-4">{t('网络层配置')}</h3>
                  <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
                    <div>
                      <label className="block text-sm font-medium text-gray-300 mb-2">{t('负载均衡策略')}</label>
                      <select
                        value={config.strategy}
                        onChange={(e) => setConfig({ ...config, strategy: e.target.value })}
                        className="w-full bg-gray-900 border border-gray-700 rounded-lg px-4 py-2 text-white focus:ring-2 focus:ring-blue-500 focus:border-transparent"
                      >
                        {STRATEGY_OPTIONS.map((opt) => (
                          <option key={opt.value} value={opt.value}>{t(opt.labelKey)}</option>
                        ))}
                      </select>
                    </div>
                    <div>
                      <label className="block text-sm font-medium text-gray-300 mb-2">{t('账号池下限')}：{config.account_pool_min}</label>
                      <input
                        type="range"
                        min="1"
                        max="20"
                        value={config.account_pool_min || 2}
                        onChange={(e) => setConfig({ ...config, account_pool_min: parseInt(e.target.value) })}
                        className="w-full"
                      />
                    </div>
                    <div>
                      <label className="block text-sm font-medium text-gray-300 mb-2">{t('账号池上限')}：{config.account_pool_max}</label>
                      <input
                        type="range"
                        min="2"
                        max="50"
                        value={config.account_pool_max || 10}
                        onChange={(e) => setConfig({ ...config, account_pool_max: parseInt(e.target.value) })}
                        className="w-full"
                      />
                    </div>
                    <div>
                      <label className="block text-sm font-medium text-gray-300 mb-2">{t('连接池上限')}：{config.connection_pool_max}</label>
                      <input
                        type="range"
                        min="5"
                        max="100"
                        value={config.connection_pool_max || 10}
                        onChange={(e) => setConfig({ ...config, connection_pool_max: parseInt(e.target.value) })}
                        className="w-full"
                      />
                    </div>
                    <div>
                      <label className="block text-sm font-medium text-gray-300 mb-2">{t('会话池上限')}：{config.session_pool_max}</label>
                      <input
                        type="range"
                        min="10"
                        max="200"
                        value={config.session_pool_max || 50}
                        onChange={(e) => setConfig({ ...config, session_pool_max: parseInt(e.target.value) })}
                        className="w-full"
                      />
                    </div>
                  </div>
                  <p className="text-gray-500 text-xs mt-3">{t('配置保存后持久化存储，服务重启不丢失。账号池下限低于可用账号数时触发告警。')}</p>
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
                      {t('重启中...')}
                    </>
                  ) : (
                    <>
                      <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
                        <path d="M23 4v6h-6"></path>
                        <path d="M1 20v-6h6"></path>
                        <path d="M3.51 9a9 9 0 0 1 14.85-3.36L23 10M1 14l4.64 4.36A9 9 0 0 0 20.49 15"></path>
                      </svg>
                      {t('重启网络层')}
                    </>
                  )}
                </button>
                <button
                  onClick={() => setShowSettings(true)}
                  className="px-6 py-2.5 bg-blue-500/10 text-blue-400 border border-blue-500/50 rounded-lg hover:bg-blue-500/20 transition font-medium text-sm flex items-center gap-2"
                >
                  <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
                    <circle cx="12" cy="12" r="3"></circle>
                    <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1 0 2.83 2 2 0 0 1-2.83 0l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-2 2 2 2 0 0 1-2-2v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83 0 2 2 0 0 1 0-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1-2-2 2 2 0 0 1 2-2h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 0-2.83 2 2 0 0 1 2.83 0l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 2-2 2 2 0 0 1 2 2v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 0 2 2 0 0 1 0 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 2 2 2 2 0 0 1-2 2h-.09a1.65 1.65 0 0 0-1.51 1z"></path>
                  </svg>
                  {t('配置调整')}
                </button>
              </div>
            </div>

            {/* 账号管理（CF 账号池） */}
            <div className="glass-card">
              <div className="flex items-center justify-between mb-4">
                <h2 className="text-xl font-semibold text-white">{t('账号管理')}</h2>
                <button
                  onClick={() => setShowAccountForm((v) => !v)}
                  className="px-4 py-2 bg-blue-500 text-white rounded-lg hover:bg-blue-600 transition font-medium text-sm"
                >
                  {showAccountForm ? t('收起') : t('添加账号')}
                </button>
              </div>
              {showAccountForm && (
                <div className="bg-gray-800/50 rounded-xl p-5 mb-4 grid grid-cols-1 md:grid-cols-3 gap-4">
                  <div>
                    <label className="block text-sm font-medium text-gray-300 mb-2">{t('名称')}</label>
                    <input
                      value={accountForm.name}
                      onChange={(e) => setAccountForm({ ...accountForm, name: e.target.value })}
                      className="w-full bg-gray-900 border border-gray-700 rounded-lg px-4 py-2 text-white"
                      placeholder={t('账号显示名')}
                    />
                  </div>
                  <div>
                    <label className="block text-sm font-medium text-gray-300 mb-2">{t('账号 ID')}</label>
                    <input
                      value={accountForm.accountId}
                      onChange={(e) => setAccountForm({ ...accountForm, accountId: e.target.value })}
                      className="w-full bg-gray-900 border border-gray-700 rounded-lg px-4 py-2 text-white"
                      placeholder="account_id"
                    />
                  </div>
                  <div>
                    <label className="block text-sm font-medium text-gray-300 mb-2">API Token</label>
                    <input
                      value={accountForm.apiToken}
                      onChange={(e) => setAccountForm({ ...accountForm, apiToken: e.target.value })}
                      className="w-full bg-gray-900 border border-gray-700 rounded-lg px-4 py-2 text-white"
                      placeholder="api_token"
                    />
                  </div>
                  <div className="md:col-span-3">
                    <button
                      onClick={handleAddAccount}
                      className="px-6 py-2 bg-green-500 text-white rounded-lg hover:bg-green-600 transition font-medium text-sm"
                    >
                      {t('确认添加')}
                    </button>
                  </div>
                </div>
              )}
              {accounts.length === 0 ? (
                <p className="text-gray-500 text-sm py-4">{t('暂无账号（CF 账号池为空）')}</p>
              ) : (
                <div className="overflow-x-auto">
                  <table className="w-full text-sm">
                    <thead>
                      <tr className="text-gray-400 text-left border-b border-gray-700">
                        <th className="py-2 pr-4">{t('名称')}</th>
                        <th className="py-2 pr-4">{t('账号 ID')}</th>
                        <th className="py-2 pr-4">{t('状态')}</th>
                        <th className="py-2 pr-4">{t('最近使用')}</th>
                        <th className="py-2 pr-4">{t('操作')}</th>
                      </tr>
                    </thead>
                    <tbody>
                      {accounts.map((a) => (
                        <tr key={a.id} className="border-b border-gray-800">
                          <td className="py-2 pr-4 text-white">{a.name}</td>
                          <td className="py-2 pr-4 text-gray-300">{a.account_id}</td>
                          <td className="py-2 pr-4">
                            <span className={a.status === 'active' ? 'text-green-500' : a.status === 'error' ? 'text-red-500' : 'text-yellow-500'}>
                              {a.status}
                            </span>
                          </td>
                          <td className="py-2 pr-4 text-gray-400">{formatTimestamp(a.last_used_at ?? undefined)}</td>
                          <td className="py-2 pr-4">
                            <button
                              onClick={() => handleRemoveAccount(a.account_id)}
                              className="text-red-400 hover:text-red-300 text-sm"
                            >
                              {t('删除')}
                            </button>
                          </td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
              )}
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
              <h3 className="text-xl font-semibold text-white mb-2">{t('确认重启网络层')}</h3>
              <p className="text-gray-400 mb-6">
                {t('重启后将重置所有渠道的断路器和健康追踪状态（不影响在途请求）。确定要继续吗？')}
              </p>
              <div className="flex gap-3 justify-center">
                <button
                  onClick={() => setShowRestartConfirm(false)}
                  disabled={restartPending}
                  className="px-6 py-2.5 bg-gray-700 text-white rounded-lg hover:bg-gray-600 transition font-medium text-sm"
                >
                  {t('取消')}
                </button>
                <button
                  onClick={handleRestart}
                  disabled={restartPending}
                  className="px-6 py-2.5 bg-red-500 text-white rounded-lg hover:bg-red-600 transition font-medium text-sm"
                >
                  {restartPending ? t('重启中...') : t('确认重启')}
                </button>
              </div>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
