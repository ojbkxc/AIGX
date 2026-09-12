import { useState, useEffect, type FormEvent } from 'react';
import { useTranslation } from 'react-i18next';
import { Monitor, Sun, Moon } from 'lucide-react';
import { api } from '../api';
import { useToast } from '../components/Toast';
import ConfirmDialog, { type ConfirmState } from '../components/ConfirmDialog';
import { Tabs } from '../components/ui';
import { getThemeMode, applyTheme, type ThemeMode } from '../lib/theme';
import Pricing from './Pricing';
import Epay from './Epay';
import Groups from './Groups';
import Notify from './Notify';
import Security from './Security';
import IpManagement from './IpManagement';
import './Settings.css';

interface LimitsForm {
  daily_limit: string;
  monthly_limit: string;
  threshold: string;
  api_timeout_secs: string;
  max_retries: string;
}

interface RateLimitConfig {
  enabled?: boolean;
  per_key_rpm?: number | null;
  per_key_tpm?: number | null;
  per_model_rpm?: number | null;
  per_user_rpm?: number | null;
  per_user_tpm?: number | null;
  per_ip_rpm?: number | null;
  global_rpm?: number | null;
  global_tpm?: number | null;
}

interface CacheStats {
  entries?: number;
  count?: number;
  hit_rate?: number;
  memory_bytes?: number;
  size_bytes?: number;
}

interface PriceSyncConfig {
  enabled?: boolean;
  sync_url?: string;
  interval_secs?: number | null;
}

type ExchangeRates = Record<string, string | number>;

interface DataResponse<T> {
  data?: T;
}

type SettingsTab = 'site' | 'billing' | 'ops' | 'security' | 'account';
/** billing 分区子标签（定价倍率 / 易支付 / 用户分组） */
type BillingSubTab = 'pricing' | 'epay' | 'groups';
/** ops 分区子标签（通知 / 限流 / 缓存 / 价格同步 / 汇率） */
type OpsSubTab = 'notify' | 'ratelimit' | 'cache' | 'pricesync' | 'exchange';

// 格式化字节数为人类可读单位
function fmtBytes(bytes: number | null | undefined): string {
  const n = Number(bytes || 0);
  if (n >= 1073741824) return (n / 1073741824).toFixed(2) + ' GB';
  if (n >= 1048576) return (n / 1048576).toFixed(2) + ' MB';
  if (n >= 1024) return (n / 1024).toFixed(2) + ' KB';
  return n + ' B';
}

const THEME_OPTIONS: Array<{ mode: ThemeMode; labelKey: string; hintKey: string; icon: typeof Sun }> = [
  { mode: 'light', labelKey: '浅色模式', hintKey: 'settingsThemeLightHint', icon: Sun },
  { mode: 'dark', labelKey: '深色模式', hintKey: 'settingsThemeDarkHint', icon: Moon },
  { mode: 'system', labelKey: '跟随系统', hintKey: 'settingsThemeSystemHint', icon: Monitor },
];

type RateLimitNumericField = Exclude<keyof RateLimitConfig, 'enabled'>;

const RL_FIELDS: Array<{ field: RateLimitNumericField; labelKey: string; hintKey: string }> = [
  { field: 'per_key_rpm', labelKey: '每 Key RPM', hintKey: '单个 API Key 每分钟最大请求数' },
  { field: 'per_key_tpm', labelKey: '每 Key TPM', hintKey: '单个 API Key 每分钟最大 Token 数' },
  { field: 'per_model_rpm', labelKey: '每模型 RPM', hintKey: '单个模型每分钟最大请求数' },
  { field: 'per_user_rpm', labelKey: '每用户 RPM', hintKey: '单个用户每分钟最大请求数' },
  { field: 'per_user_tpm', labelKey: '每用户 TPM', hintKey: '单个用户每分钟最大 Token 数' },
  { field: 'per_ip_rpm', labelKey: '每 IP RPM', hintKey: '单个 IP 每分钟最大请求数' },
  { field: 'global_rpm', labelKey: '全局 RPM', hintKey: '全系统每分钟最大请求数' },
  { field: 'global_tpm', labelKey: '全局 TPM', hintKey: '全系统每分钟最大 Token 数' },
];

const LIMIT_FIELDS: Array<{ field: 'daily_limit' | 'monthly_limit' | 'threshold'; labelKey: string; hintKey: string; min?: number; max?: number }> = [
  { field: 'daily_limit', labelKey: '每日 Token 限额', hintKey: '每天允许的最大 Token 数。0 或空 = 无限制。', min: 0 },
  { field: 'monthly_limit', labelKey: '每月 Token 限额', hintKey: '每月允许的最大 Token 数。0 或空 = 无限制。', min: 0 },
  { field: 'threshold', labelKey: '告警阈值 (%)', hintKey: '触发告警的限额使用百分比（0-100）。', min: 0, max: 100 },
];

export default function Settings() {
  const [limits, setLimits] = useState<LimitsForm>({
    daily_limit: '',
    monthly_limit: '',
    threshold: '',
    api_timeout_secs: '',
    max_retries: '',
  });
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState('');
  const addToast = useToast();
  const { t } = useTranslation();

  // 当前激活的配置分区：四 tab 信息架构对齐 new-api System Settings
  // 站点（原通用+界面）/ 计费（定价+易支付+用户分组）/ 运维（通知+限流+缓存+价格同步）/ 安全（安全监控+IP管理）
  const [activeTab, setActiveTab] = useState<SettingsTab>('site');
  // 分区内子标签：避免 billing/ops 长页面垂直堆叠（易支付曾被压到最底）
  const [billingSub, setBillingSub] = useState<BillingSubTab>('pricing');
  const [opsSub, setOpsSub] = useState<OpsSubTab>('notify');

  // 界面分区：主题三态（system/light/dark），与登录页/侧边栏切换共享同一份持久化
  const [theme, setTheme] = useState<ThemeMode>(() => getThemeMode());

  // 限流配置
  const [rlConfig, setRlConfig] = useState<RateLimitConfig | null>(null);
  const [rlLoading, setRlLoading] = useState(false);
  const [rlSaving, setRlSaving] = useState(false);

  // ── 缓存管理 ──
  const [cacheStats, setCacheStats] = useState<CacheStats | null>(null);
  const [cacheLoading, setCacheLoading] = useState(false);
  const [cacheClearing, setCacheClearing] = useState(false);

  // ── 价格同步配置 ──
  const [priceSyncConfig, setPriceSyncConfig] = useState<PriceSyncConfig | null>(null);
  const [priceSyncLoading, setPriceSyncLoading] = useState(false);
  const [priceSyncSaving, setPriceSyncSaving] = useState(false);
  const [priceSyncTriggering, setPriceSyncTriggering] = useState(false);

  // ── 汇率配置 ──
  const [exchangeRates, setExchangeRates] = useState<ExchangeRates | null>(null);
  const [exchangeRatesLoading, setExchangeRatesLoading] = useState(false);
  const [exchangeRatesSaving, setExchangeRatesSaving] = useState(false);

  // ── 通用确认弹窗（用于清空缓存等危险操作）──
  const [confirmState, setConfirmState] = useState<ConfirmState | null>(null);

  // ── 账户安全（修改密码）──
  const [oldPw, setOldPw] = useState('');
  const [newPw, setNewPw] = useState('');
  const [confirmPw, setConfirmPw] = useState('');
  const [pwSaving, setPwSaving] = useState(false);
  const [pwError, setPwError] = useState('');

  const handleChangePassword = async (e?: FormEvent) => {
    e?.preventDefault();
    setPwError('');
    if (!oldPw) { setPwError(t('请输入当前密码')); return; }
    if (newPw.length < 6) { setPwError(t('新密码至少 6 位')); return; }
    if (newPw !== confirmPw) { setPwError(t('两次输入的新密码不一致')); return; }
    setPwSaving(true);
    try {
      await api.changePassword(oldPw, newPw);
      addToast(t('密码已修改，下次登录请使用新密码'));
      setOldPw(''); setNewPw(''); setConfirmPw('');
    } catch (err) {
      setPwError(err instanceof Error ? err.message : String(err));
    } finally {
      setPwSaving(false);
    }
  };

  const selectTheme = (mode: ThemeMode) => {
    setTheme(mode);
    applyTheme(mode);
  };

  useEffect(() => {
    loadLimits();
    loadRateLimitConfig();
    loadCacheStats();
    loadPriceSyncConfig();
    loadExchangeRates();
  }, []);

  const loadRateLimitConfig = async () => {
    setRlLoading(true);
    try {
      const res = (await api.getRateLimitConfig()) as DataResponse<RateLimitConfig> | RateLimitConfig;
      setRlConfig((res as DataResponse<RateLimitConfig>).data || (res as RateLimitConfig));
    } catch (err) {
      // 限流配置可能未启用，静默处理
    } finally {
      setRlLoading(false);
    }
  };

  const handleRlChange = (field: keyof RateLimitConfig, value: string) => {
    setRlConfig({ ...(rlConfig as RateLimitConfig), [field]: value === '' ? null : Number(value) });
  };

  const handleRlSave = async () => {
    setRlSaving(true);
    setError('');
    try {
      await api.updateRateLimitConfig(rlConfig as unknown as Record<string, unknown>);
      addToast(t('限流配置更新成功'));
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setRlSaving(false);
    }
  };

  // ── 缓存管理 ──
  const loadCacheStats = async () => {
    setCacheLoading(true);
    try {
      const res = (await api.getCacheStats()) as DataResponse<CacheStats> | CacheStats;
      setCacheStats((res as DataResponse<CacheStats>).data || (res as CacheStats));
    } catch {
      // 缓存统计可能未启用，静默处理
    } finally {
      setCacheLoading(false);
    }
  };

  const handleClearCache = () => {
    setConfirmState({
      title: t('清空缓存'),
      message: t('确定清空所有缓存？此操作不可撤销，可能短暂影响性能。'),
      confirmText: t('清空'),
      danger: true,
      onConfirm: async () => {
        setCacheClearing(true);
        setError('');
        try {
          await api.clearCache();
          addToast(t('缓存已清空'));
          loadCacheStats();
        } catch (err) {
          setError(err instanceof Error ? err.message : String(err));
        } finally {
          setCacheClearing(false);
        }
      },
    });
  };

  // ── 价格同步配置 ──
  const loadPriceSyncConfig = async () => {
    setPriceSyncLoading(true);
    try {
      const res = (await api.getPriceSyncConfig()) as DataResponse<PriceSyncConfig> | PriceSyncConfig;
      setPriceSyncConfig((res as DataResponse<PriceSyncConfig>).data || (res as PriceSyncConfig));
    } catch {
      // 价格同步可能未启用，静默处理
    } finally {
      setPriceSyncLoading(false);
    }
  };

  const handlePriceSyncSave = async () => {
    setPriceSyncSaving(true);
    setError('');
    try {
      await api.updatePriceSyncConfig(priceSyncConfig as unknown as Record<string, unknown>);
      addToast(t('价格同步配置已更新'));
      // P1：保存后立即刷新 last_sync 等后端派生字段
      await loadPriceSyncConfig();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setPriceSyncSaving(false);
    }
  };

  const handlePriceSyncTrigger = async () => {
    setPriceSyncTriggering(true);
    setError('');
    try {
      await api.triggerPriceSync();
      addToast(t('价格同步已触发，请稍后查看同步结果'));
      await loadPriceSyncConfig();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setPriceSyncTriggering(false);
    }
  };

  // ── 汇率配置 ──
  const loadExchangeRates = async () => {
    setExchangeRatesLoading(true);
    try {
      const res = (await api.getExchangeRates()) as DataResponse<ExchangeRates> | ExchangeRates;
      setExchangeRates((res as DataResponse<ExchangeRates>).data || (res as ExchangeRates));
    } catch {
      // 汇率配置可能未启用，静默处理
    } finally {
      setExchangeRatesLoading(false);
    }
  };

  const handleExchangeRateChange = (currency: string, value: string) => {
    setExchangeRates({ ...(exchangeRates as ExchangeRates), [currency]: value === '' ? '' : Number(value) });
  };

  const handleExchangeRatesSave = async () => {
    setExchangeRatesSaving(true);
    setError('');
    try {
      await api.updateExchangeRates(exchangeRates as unknown as Record<string, unknown>);
      addToast(t('汇率配置已更新'));
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setExchangeRatesSaving(false);
    }
  };

  const loadLimits = async () => {
    setLoading(true);
    setError('');
    try {
      const res = (await api.getLimits()) as DataResponse<Partial<LimitsForm> & { threshold?: number }>;
      const data = res.data ?? (res as unknown as Partial<LimitsForm> & { threshold?: number });
      setLimits({
        daily_limit: data.daily_limit ?? '',
        monthly_limit: data.monthly_limit ?? '',
        threshold: data.threshold != null ? String(data.threshold * 100) : '',
        api_timeout_secs: data.api_timeout_secs ?? '',
        max_retries: data.max_retries ?? '',
      });
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  };

  // ── 通知配置已迁移至独立「通知设置」页面（/notify），此处只提供入口与说明 ──

  const handleChange = (field: keyof LimitsForm, value: string) => {
    setLimits({ ...limits, [field]: value });
  };

  const handleSave = async () => {
    // NaN 兜底：任一数值字段填入非数字内容时直接报错，不提交
    const numericFields = [
      limits.daily_limit,
      limits.monthly_limit,
      limits.threshold,
      limits.api_timeout_secs,
      limits.max_retries,
    ];
    if (numericFields.some((v) => v !== '' && Number.isNaN(Number(v)))) {
      setError(t('请输入有效数字'));
      return;
    }

    const payload: Record<string, number> = {};
    if (limits.daily_limit !== '') payload.daily_limit = Number(limits.daily_limit);
    if (limits.monthly_limit !== '') payload.monthly_limit = Number(limits.monthly_limit);
    if (limits.threshold !== '') payload.threshold = Number(limits.threshold) / 100;
    if (limits.api_timeout_secs !== '') payload.api_timeout_secs = Number(limits.api_timeout_secs);
    if (limits.max_retries !== '') payload.max_retries = Number(limits.max_retries);

    // 阈值按原始输入（0-100 百分比）校验；payload.threshold 已除 100，不再用于范围比较
    if (payload.daily_limit < 0 || payload.monthly_limit < 0 || (limits.threshold !== '' && (Number(limits.threshold) < 0 || Number(limits.threshold) > 100))) {
      setError(t('请输入有效值。日/月限额必须 >= 0。阈值必须在 0-100 之间。'));
      return;
    }
    if (payload.api_timeout_secs != null && payload.api_timeout_secs < 5) {
      setError(t('API 超时时间至少为 5 秒'));
      return;
    }
    if (payload.max_retries != null && (payload.max_retries < 0 || payload.max_retries > 10)) {
      setError(t('最大重试次数必须在 0-10 之间'));
      return;
    }

    setSaving(true);
    setError('');
    try {
      await api.updateLimits(payload);
      addToast(t('设置更新成功'));
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  };

  if (loading) return <div className="loading">{t('加载设置')}</div>;

  return (
    <div>
      <div className="page-header">
        <h1>{t('系统设置')}</h1>
        <p>{t('settingsPageSubtitle')}</p>
      </div>

      {error && <div className="error-message">{error}</div>}

      <Tabs<SettingsTab>
        items={[
          { key: 'site', label: t('站点') },
          { key: 'billing', label: t('计费/易支付') },
          { key: 'ops', label: t('运维') },
          { key: 'security', label: t('安全') },
          { key: 'account', label: t('账户') },
        ]}
        active={activeTab}
        onChange={setActiveTab}
        ariaLabel={t('设置分区')}
        className="settings-tabs"
      />

      {activeTab === 'site' && (
        <>
          {/* 站点 = 原通用 + 界面：使用限额 + API 策略 + 主题 */}
          <div className="card">
            <div className="card-header">
              <h2>{t('使用限额')}</h2>
              <p className="card-subtitle">{t('settingsGeneralHint')}</p>
            </div>
            <div className="card-body">
              <div className="settings-form">
                {LIMIT_FIELDS.map((f) => (
                  <div key={f.field} className="form-group">
                    <label>{t(f.labelKey)}</label>
                    <input
                      className="form-input"
                      type="number"
                      min={f.min}
                      max={f.max}
                      placeholder={t('settingsPlaceholderThreshold')}
                      value={limits[f.field]}
                      onChange={(e) => handleChange(f.field, e.target.value)}
                    />
                    <span className="form-hint">{t(f.hintKey)}</span>
                  </div>
                ))}
              </div>
            </div>
          </div>

          <div className="card" style={{ marginTop: 16 }}>
            <div className="card-header">
              <h2>{t('API 配置')}</h2>
            </div>
            <div className="card-body">
              <div className="settings-form">
                <div className="form-group">
                  <label>{t('API 超时时间 (秒)')}</label>
                  <input className="form-input" type="number" min="5" max="300" placeholder={t('settingsPlaceholderApiTimeout')} value={limits.api_timeout_secs} onChange={(e) => handleChange('api_timeout_secs', e.target.value)} />
                  <span className="form-hint">{t('向 Cloudflare API 发送请求的超时时间，默认 120 秒。')}</span>
                </div>
                <div className="form-group">
                  <label>{t('最大重试次数')}</label>
                  <input className="form-input" type="number" min="0" max="10" placeholder={t('settingsPlaceholderMaxRetries')} value={limits.max_retries} onChange={(e) => handleChange('max_retries', e.target.value)} />
                  <span className="form-hint">{t('API 请求失败时的最大重试次数，0 表示不重试。')}</span>
                </div>
                <div className="settings-actions">
                  <button className="btn btn-primary" onClick={handleSave} disabled={saving}>
                    {saving ? t('保存中...') : t('保存更改')}
                  </button>
                </div>
              </div>
            </div>
          </div>

          <div className="card" style={{ marginTop: 16 }}>
            <div className="card-header">
              <h2>{t('主题')}</h2>
              <p className="card-subtitle">{t('settingsThemeSubtitle')}</p>
            </div>
            <div className="card-body">
              <div className="theme-options" role="radiogroup" aria-label={t('主题')}>
                {THEME_OPTIONS.map((opt) => {
                  const Icon = opt.icon;
                  const active = theme === opt.mode;
                  return (
                    <button
                      key={opt.mode}
                      type="button"
                      role="radio"
                      aria-checked={active}
                      className={`theme-option ${active ? 'active' : ''}`}
                      onClick={() => selectTheme(opt.mode)}
                    >
                      <Icon size={18} className="theme-option-icon" />
                      <span className="theme-option-label">{t(opt.labelKey)}</span>
                      <span className="theme-option-hint">{t(opt.hintKey)}</span>
                    </button>
                  );
                })}
              </div>
            </div>
          </div>
        </>
      )}

      {activeTab === 'billing' && (
        <>
          {/* 计费 = 定价倍率 + 易支付 + 用户分组（子标签切换，不再垂直堆叠） */}
          <Tabs<BillingSubTab>
            items={[
              { key: 'pricing', label: t('定价倍率') },
              { key: 'epay', label: t('易支付配置') },
              { key: 'groups', label: t('用户分组') },
            ]}
            active={billingSub}
            onChange={setBillingSub}
            ariaLabel={t('计费设置分区')}
          />
          <div style={{ marginTop: 16 }}>
            {billingSub === 'pricing' && <Pricing />}
            {billingSub === 'epay' && <Epay />}
            {billingSub === 'groups' && <Groups />}
          </div>
        </>
      )}

      {activeTab === 'ops' && (
        <>
          {/* 运维 = 通知设置 + 限流配置 + 缓存管理 + 价格同步 + 汇率（子标签切换） */}
          <Tabs<OpsSubTab>
            items={[
              { key: 'notify', label: t('通知设置') },
              { key: 'ratelimit', label: t('限流配置') },
              { key: 'cache', label: t('缓存管理') },
              { key: 'pricesync', label: t('价格同步') },
              { key: 'exchange', label: t('汇率配置') },
            ]}
            active={opsSub}
            onChange={setOpsSub}
            ariaLabel={t('运维设置分区')}
          />
          <div style={{ marginTop: 16, display: opsSub === 'notify' ? 'block' : 'none' }}>
            <Notify />
          </div>
          <div style={{ marginTop: 16, display: opsSub === 'ratelimit' ? 'block' : 'none' }}>
            <div className="card">
            <div className="card-header">
              <h2>{t('限流配置')}</h2>
            </div>
            <div className="card-body">
              {rlLoading ? (
                <div className="loading">{t('加载限流配置')}</div>
              ) : rlConfig ? (
                <div className="settings-form">
                  <p style={{ fontSize: 13, color: 'var(--text-muted)', marginBottom: 16 }}>
                    {t('配置多维度 RPM（每分钟请求数）/ TPM（每分钟 Token 数）限流。留空或 0 = 不限制。')}
                  </p>
                  {/* 启用限流总开关：不开启时任何限流维度都不生效 */}
                  <div className="form-group">
                    <label style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
                      <input
                        type="checkbox"
                        checked={rlConfig.enabled ?? false}
                        onChange={(e) => setRlConfig({ ...rlConfig, enabled: e.target.checked })}
                      />
                      {t('启用限流总开关')}
                    </label>
                    <span className="form-hint">{t('开启后限流规则才会生效')}</span>
                  </div>
                  <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fill, minmax(220px, 1fr))', gap: 16 }}>
                    {RL_FIELDS.map((f) => (
                      <div key={f.field} className="form-group">
                        <label>{t(f.labelKey)}</label>
                        <input className="form-input" type="number" min="0" placeholder="0"
                          value={rlConfig[f.field] ?? ''}
                          onChange={(e) => handleRlChange(f.field, e.target.value)} />
                        <span className="form-hint">{t(f.hintKey)}</span>
                      </div>
                    ))}
                  </div>
                  <div className="settings-actions" style={{ marginTop: 16 }}>
                    <button className="btn btn-primary" onClick={handleRlSave} disabled={rlSaving}>
                      {rlSaving ? t('保存中...') : t('保存限流配置')}
                    </button>
                  </div>
                </div>
              ) : (
                <div className="empty-state">
                  <p>{t('限流配置未启用或加载失败')}</p>
                </div>
              )}
            </div>
          </div>
          </div>

          <div style={{ marginTop: 16, display: opsSub === 'cache' ? 'block' : 'none' }}>
          <div className="card">
            <div className="card-header">
              <h2>{t('缓存管理')}</h2>
            </div>
            <div className="card-body">
              {cacheLoading ? (
                <div className="loading">{t('加载缓存统计')}</div>
              ) : cacheStats ? (
                <div className="settings-form">
                  <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fill, minmax(200px, 1fr))', gap: 16, marginBottom: 16 }}>
                    <div className="form-group">
                      <label>{t('缓存条目数')}</label>
                      <div style={{ fontSize: 20, fontWeight: 600, color: 'var(--text-main)' }}>
                        {Number(cacheStats.entries || cacheStats.count || 0).toLocaleString()}
                      </div>
                    </div>
                    <div className="form-group">
                      <label>{t('命中率')}</label>
                      <div style={{ fontSize: 20, fontWeight: 600, color: 'var(--text-main)' }}>
                        {Number(cacheStats.hit_rate || 0).toFixed(1)}%
                      </div>
                    </div>
                    <div className="form-group">
                      <label>{t('内存占用')}</label>
                      <div style={{ fontSize: 20, fontWeight: 600, color: 'var(--text-main)' }}>
                        {fmtBytes(cacheStats.memory_bytes || cacheStats.size_bytes || 0)}
                      </div>
                    </div>
                  </div>
                  <div className="settings-actions">
                    <button
                      className="btn btn-danger"
                      onClick={handleClearCache}
                      disabled={cacheClearing}
                    >
                      {cacheClearing ? t('清空中...') : t('清空缓存')}
                    </button>
                  </div>
                </div>
              ) : (
                <div className="empty-state">
                  <p>{t('缓存统计未启用或加载失败')}</p>
                </div>
              )}
            </div>
          </div>
          </div>

          <div style={{ marginTop: 16, display: opsSub === 'pricesync' ? 'block' : 'none' }}>
          <div className="card">
            <div className="card-header">
              <h2>{t('价格同步')}</h2>
            </div>
            <div className="card-body">
              {priceSyncLoading ? (
                <div className="loading">{t('加载价格同步配置')}</div>
              ) : priceSyncConfig ? (
                <div className="settings-form">
                  <div className="form-group">
                    <label style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
                      <input
                        type="checkbox"
                        checked={Boolean(priceSyncConfig.enabled)}
                        onChange={(e) => setPriceSyncConfig({ ...priceSyncConfig, enabled: e.target.checked })}
                      />
                      {t('启用自动同步')}
                    </label>
                    <span className="form-hint">{t('开启后按间隔自动从同步 URL 拉取最新价格')}</span>
                  </div>
                  <div className="form-group">
                    <label>{t('同步 URL')}</label>
                    <input
                      className="form-input"
                      placeholder="https://example.com/prices.json"
                      value={priceSyncConfig.sync_url || ''}
                      onChange={(e) => setPriceSyncConfig({ ...priceSyncConfig, sync_url: e.target.value })}
                    />
                    <span className="form-hint">{t('价格数据源的 JSON URL')}</span>
                  </div>
                  <div className="form-group">
                    <label>{t('同步间隔（秒）')}</label>
                    <input
                      className="form-input"
                      type="number"
                      min="60"
                      placeholder="3600"
                      value={priceSyncConfig.interval_secs ?? ''}
                      onChange={(e) => setPriceSyncConfig({ ...priceSyncConfig, interval_secs: e.target.value === '' ? null : Number(e.target.value) })}
                    />
                    <span className="form-hint">{t('自动同步间隔，建议 >= 300 秒')}</span>
                  </div>
                  <div className="settings-actions" style={{ display: 'flex', gap: 12 }}>
                    <button className="btn btn-primary" onClick={handlePriceSyncSave} disabled={priceSyncSaving}>
                      {priceSyncSaving ? t('保存中...') : t('保存配置')}
                    </button>
                    <button className="btn btn-outline" onClick={handlePriceSyncTrigger} disabled={priceSyncTriggering}>
                      {priceSyncTriggering ? t('同步中...') : t('立即同步')}
                    </button>
                  </div>
                </div>
              ) : (
                <div className="empty-state">
                  <p>{t('价格同步未启用或加载失败')}</p>
                </div>
              )}
            </div>
          </div>
          </div>

          <div style={{ marginTop: 16, display: opsSub === 'exchange' ? 'block' : 'none' }}>
          <div className="card">
            <div className="card-header">
              <h2>{t('汇率配置')}</h2>
            </div>
            <div className="card-body">
              {exchangeRatesLoading ? (
                <div className="loading">{t('加载汇率配置')}</div>
              ) : exchangeRates ? (
                <div className="settings-form">
                  <p style={{ fontSize: 13, color: 'var(--text-muted)', marginBottom: 16 }}>
                    {t('各币种相对于 USD 的汇率。例如 CNY=7.2 表示 1 USD = 7.2 CNY。')}
                  </p>
                  <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fill, minmax(200px, 1fr))', gap: 16 }}>
                    {Object.keys(exchangeRates).map((currency) => (
                      <div key={currency} className="form-group">
                        <label>{currency} / USD</label>
                        <input
                          className="form-input"
                          type="number"
                          step="0.0001"
                          min="0"
                          placeholder="1.0"
                          value={exchangeRates[currency] ?? ''}
                          disabled={currency.toUpperCase() === 'USD'}
                          onChange={(e) => handleExchangeRateChange(currency, e.target.value)}
                        />
                        {currency.toUpperCase() === 'USD' && (
                          <span className="form-hint">{t('USD 为基准货币，不可编辑')}</span>
                        )}
                      </div>
                    ))}
                  </div>
                  <div className="settings-actions">
                    <button className="btn btn-primary" onClick={handleExchangeRatesSave} disabled={exchangeRatesSaving}>
                      {exchangeRatesSaving ? t('保存中...') : t('保存汇率')}
                    </button>
                  </div>
                </div>
              ) : (
                <div className="empty-state">
                  <p>{t('汇率配置未启用或加载失败')}</p>
                </div>
              )}
            </div>
          </div>
          </div>
        </>
      )}

      {activeTab === 'security' && (
        <>
          {/* 安全 = 安全监控 + IP 管理 */}
          <Security />
          <div style={{ marginTop: 16 }}><IpManagement /></div>
        </>
      )}

      {activeTab === 'account' && (
        <div className="card">
          <div className="card-header">
            <h2>{t('账户安全')}</h2>
          </div>
          <div className="card-body">
            <form className="settings-form" onSubmit={(e) => void handleChangePassword(e)}>
              <div className="form-group">
                <label>{t('当前密码')}</label>
                <input className="form-input" type="password" value={oldPw}
                  onChange={(e) => setOldPw(e.target.value)} autoComplete="current-password" />
              </div>
              <div className="form-group">
                <label>{t('新密码')}</label>
                <input className="form-input" type="password" value={newPw}
                  onChange={(e) => setNewPw(e.target.value)} autoComplete="new-password" />
                <span className="form-hint">{t('至少 6 位，建议混合字母与数字')}</span>
              </div>
              <div className="form-group">
                <label>{t('确认新密码')}</label>
                <input className="form-input" type="password" value={confirmPw}
                  onChange={(e) => setConfirmPw(e.target.value)} autoComplete="new-password" />
              </div>
              {pwError && <div className="error-message">{pwError}</div>}
              <div className="settings-actions">
                <button type="submit" className="btn btn-primary" disabled={pwSaving}>
                  {pwSaving ? t('修改中...') : t('修改密码')}
                </button>
              </div>
            </form>
          </div>
        </div>
      )}

      <ConfirmDialog state={confirmState} onClose={() => setConfirmState(null)} />

    </div>
  );
}