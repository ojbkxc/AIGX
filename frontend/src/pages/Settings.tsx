import { useState, useEffect } from 'react';
import { useSearchParams } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import {
  Monitor, Sun, Moon,
  SlidersHorizontal, Sparkles, Network, RotateCcw,
  Timer, Gauge, CreditCard, Database, Bell, ShieldCheck, Download,
  MessageSquare, CalendarClock,
} from 'lucide-react';
import { api } from '../api';
import { useToast } from '../components/Toast';
import ConfirmDialog, { type ConfirmState } from '../components/ConfirmDialog';
import { Tabs, SkeletonList } from '../components/ui';
import { getThemeMode, applyTheme, type ThemeMode } from '../lib/theme';
import { getZoom, applyZoom, ZOOM_LEVELS } from '../lib/zoom';
import Epay from './Epay';
import Groups from './Groups';
import Notify from './Notify';
import Security from './Security';
import IpManagement from './IpManagement';
import NetworkLayer from './NetworkLayer';
import LogRetention from './LogRetention';
import UpdatePanel from '../components/UpdatePanel';
import AgentSettingsPanel from '../components/AgentSettingsPanel';
import ChatSettingsPanel from '../components/ChatSettingsPanel';
import ReportSettingsPanel from '../components/ReportSettingsPanel';
import GatewayPanel from '../components/GatewayPanel';
import SanitizePanel from '../components/SanitizePanel';
import ClineHeadersPanel from '../components/ClineHeadersPanel';
import ZcodeCaptchaPanel from '../components/ZcodeCaptchaPanel';
import SecurityPolicyPanel from '../components/SecurityPolicyPanel';
import PromptPanel from '../components/PromptPanel';
import DebugTrafficPanel from '../components/DebugTrafficPanel';
import StreamCompatPanel from '../components/StreamCompatPanel';
import QueuePanel from '../components/QueuePanel';
import AutoContinuePanel from '../components/AutoContinuePanel';
import ModelMetadataPanel from '../components/ModelMetadataPanel';
import DataManagement from '../components/DataManagement';
import ScheduledTasks from './ScheduledTasks';
import './Settings.css';

interface LimitsForm {
  daily_limit: string;
  monthly_limit: string;
  threshold: string;
  api_timeout_secs: string;
  connect_timeout_secs: string;
  headers_timeout_secs: string;
  stream_idle_secs: string;
  body_timeout_secs: string;
  max_retries: string;
  retry_self_count: string;
  retry_interval_ms: string;
  retry_on_status: string;
  billing_flat_quota: string;
  max_request_body_mb: string;
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

/**
 * 设置分区：对齐 Agent2API settings-page 的 CATEGORIES（general / assistant /
 * gateway / retry / timeout / security / data …），把原先埋在
 * 「运维 → 子标签」和「站点 → API 配置」里的超时、重试拆成独立分类。
 *
 * 「路由」已升级为独立页 `/routing`（#40 分层档案 + 预演 + 回放 + 粘滞需要
 * 同屏上下文），不再作为此处的分类 —— 上游也是独立的路由域。
 */
type SettingsTab =
  | 'general'
  | 'assistant'
  | 'chat'
  | 'report'
  | 'gateway'
  | 'retry'
  | 'timeout'
  | 'perf'
  | 'billing'
  | 'data'
  | 'scheduled'
  | 'notify'
  | 'security'
  | 'update';

/** 计费分区子标签（易支付 / 用户分组） */
type BillingSubTab = 'epay' | 'groups';

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

/** 超时分类：五个时间窗，对齐 Agent2API 的「超时」pane（TIMEOUT_FIELDS）。 */
const TIMEOUT_FIELDS: Array<{
  field: 'api_timeout_secs' | 'connect_timeout_secs' | 'headers_timeout_secs' | 'stream_idle_secs' | 'body_timeout_secs';
  label: string;
  hint: string;
  min: number;
  max: number;
  placeholder: string;
}> = [
  { field: 'api_timeout_secs', label: '请求总超时 (秒)', hint: '单次上游请求的总时长上限，默认 300 秒（修改后重启生效）。', min: 5, max: 3600, placeholder: '300' },
  { field: 'connect_timeout_secs', label: '连接超时 (秒)', hint: '与上游建立 TCP/TLS 连接的上限，默认 10 秒（修改后重启生效）。', min: 1, max: 300, placeholder: '10' },
  { field: 'headers_timeout_secs', label: '响应头等待 / 首事件上限 (秒)', hint: '等待上游响应头与流式首个事件的上限，默认 60 秒（保存后立即生效）。', min: 1, max: 600, placeholder: '60' },
  { field: 'stream_idle_secs', label: '流空闲超时 (秒)', hint: '流式响应两次读取之间的最长静默，默认 30 秒（修改后重启生效）。', min: 1, max: 600, placeholder: '30' },
  { field: 'body_timeout_secs', label: '非流式响应体超时 (秒)', hint: '非流式请求读完整响应体的上限，默认 300 秒（保存后立即生效）。流式请求不受此项约束。', min: 1, max: 3600, placeholder: '300' },
];

/** 重试分类：渠道切换 / 同账号原地重试 / 间隔 / 直接换号状态码。 */
const RETRY_FIELDS: Array<{
  field: 'max_retries' | 'retry_self_count' | 'retry_interval_ms' | 'retry_on_status';
  label: string;
  hint: string;
  type: 'number' | 'text';
  min?: number;
  max?: number;
  placeholder: string;
}> = [
  { field: 'max_retries', label: '最大重试次数（供应商切换）', hint: '供应商级失败切换的最大额外尝试次数（0 = 只尝试首选供应商，最多 10）；保存后立即生效。', type: 'number', min: 0, max: 10, placeholder: '2' },
  { field: 'retry_self_count', label: '同账号重试次数', hint: '账号池内同一账号原地重试的额外次数（0 = 失败即换号，最多 10）；保存后立即生效。换号次数在账号池「账号路由」里配置。', type: 'number', min: 0, max: 10, placeholder: '1' },
  { field: 'retry_interval_ms', label: '重试间隔 (毫秒)', hint: '账号池换号重试之间的等待时间，0 = 立即换号（保存后立即生效）。', type: 'number', min: 0, max: 10000, placeholder: '0' },
  { field: 'retry_on_status', label: '直接换号/换供应商状态码（逗号分隔）', hint: '命中这些上游状态码时不做指数退避冷却（只给基础冷却）并直接换号，同时在供应商级重试中视为可重试；默认 402（余额不足）。', type: 'text', placeholder: '402' },
];

/** 设置分类：左栏导航，对齐 Agent2API settings-page 的 CATEGORIES。 */
const SETTINGS_CATEGORIES: Array<{ key: SettingsTab; label: string; icon: typeof Sun }> = [
  { key: 'general', label: '通用', icon: SlidersHorizontal },
  { key: 'assistant', label: 'AI 助手', icon: Sparkles },
  { key: 'chat', label: '聊天', icon: MessageSquare },
  { key: 'report', label: '报表', icon: CalendarClock },
  { key: 'gateway', label: '网关', icon: Network },
  { key: 'retry', label: '重试', icon: RotateCcw },
  { key: 'timeout', label: '超时', icon: Timer },
  { key: 'perf', label: '限流与缓存', icon: Gauge },
  { key: 'billing', label: '计费', icon: CreditCard },
  { key: 'data', label: '数据', icon: Database },
  { key: 'scheduled', label: '定时任务', icon: CalendarClock },
  { key: 'notify', label: '通知', icon: Bell },
  { key: 'security', label: '安全', icon: ShieldCheck },
  { key: 'update', label: '更新', icon: Download },
];

/** URL ?tab= 只认已注册分类，避免手改 URL 把设置页置空 */
const VALID_SETTINGS_TABS = new Set<string>(SETTINGS_CATEGORIES.map((c) => c.key));

export default function Settings() {
  const [limits, setLimits] = useState<LimitsForm>({
    daily_limit: '',
    monthly_limit: '',
    threshold: '',
    api_timeout_secs: '',
    connect_timeout_secs: '',
    headers_timeout_secs: '',
    stream_idle_secs: '',
    body_timeout_secs: '',
    max_retries: '',
    retry_self_count: '',
    retry_interval_ms: '',
    retry_on_status: '',
    billing_flat_quota: '',
    max_request_body_mb: '',
  });
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState('');
  const addToast = useToast();
  const { t } = useTranslation();

  // 当前激活的配置分类：左侧导航（对齐 Agent2API 的 CATEGORIES），
  // 每类只渲染自己那块，不再靠子标签层层下钻。
  // Tab 状态走 URL ?tab=（旧 /scheduled-tasks 重定向到 ?tab=scheduled 后由 URL 驱动；
  // billing 的子标签仍为内部 state，不进 URL）。
  const [searchParams, setSearchParams] = useSearchParams();
  const [activeTab, setActiveTab] = useState<SettingsTab>(() => {
    const want = searchParams.get('tab');
    return want && VALID_SETTINGS_TABS.has(want) ? (want as SettingsTab) : 'general';
  });
  useEffect(() => {
    const want = searchParams.get('tab');
    if (want && VALID_SETTINGS_TABS.has(want)) setActiveTab(want as SettingsTab);
  }, [searchParams]);
  const selectSettingsTab = (key: SettingsTab): void => {
    setActiveTab(key);
    const next = new URLSearchParams(searchParams);
    next.set('tab', key);
    setSearchParams(next, { replace: true });
  };
  // 计费分区子标签（易支付 / 用户分组）
  const [billingSub, setBillingSub] = useState<BillingSubTab>('epay');

  // 界面分区：主题三态（system/light/dark），与登录页/侧边栏切换共享同一份持久化
  const [theme, setTheme] = useState<ThemeMode>(() => getThemeMode());

  // 界面缩放（80%–130%，localStorage 持久化；对照 Agent2API「设置 → 显示」）
  const [zoom, setZoom] = useState<number>(() => getZoom());

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

  const selectTheme = (mode: ThemeMode) => {
    setTheme(mode);
    applyTheme(mode);
  };

  const selectZoom = (scale: number) => {
    setZoom(scale);
    applyZoom(scale);
  };

  /** 仓库地址（反馈入口跳 GitHub issue 模板用）。 */
  const REPO_URL = 'https://github.com/ojbkxc/AIGX';

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
        connect_timeout_secs: data.connect_timeout_secs ?? '',
        headers_timeout_secs: data.headers_timeout_secs ?? '',
        stream_idle_secs: data.stream_idle_secs ?? '',
        body_timeout_secs: data.body_timeout_secs ?? '',
        max_retries: data.max_retries ?? '',
        retry_self_count: data.retry_self_count ?? '',
        retry_interval_ms: data.retry_interval_ms ?? '',
        retry_on_status: Array.isArray((data as { retry_on_status?: number[] }).retry_on_status)
          ? ((data as { retry_on_status?: number[] }).retry_on_status || []).join(', ')
          : '',
        billing_flat_quota: data.billing_flat_quota != null ? String(data.billing_flat_quota) : '',
        max_request_body_mb: data.max_request_body_mb != null ? String(data.max_request_body_mb) : '',
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
      limits.connect_timeout_secs,
      limits.headers_timeout_secs,
      limits.stream_idle_secs,
      limits.body_timeout_secs,
      limits.max_retries,
      limits.retry_self_count,
      limits.retry_interval_ms,
      limits.billing_flat_quota,
      limits.max_request_body_mb,
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
    if (limits.connect_timeout_secs !== '') payload.connect_timeout_secs = Number(limits.connect_timeout_secs);
    if (limits.headers_timeout_secs !== '') payload.headers_timeout_secs = Number(limits.headers_timeout_secs);
    if (limits.stream_idle_secs !== '') payload.stream_idle_secs = Number(limits.stream_idle_secs);
    if (limits.body_timeout_secs !== '') payload.body_timeout_secs = Number(limits.body_timeout_secs);
    if (limits.max_retries !== '') payload.max_retries = Number(limits.max_retries);
    if (limits.retry_self_count !== '') payload.retry_self_count = Number(limits.retry_self_count);
    if (limits.retry_interval_ms !== '') payload.retry_interval_ms = Number(limits.retry_interval_ms);
    if (limits.billing_flat_quota !== '') payload.billing_flat_quota = Number(limits.billing_flat_quota);
    if (limits.max_request_body_mb !== '') payload.max_request_body_mb = Number(limits.max_request_body_mb);

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
    if (payload.connect_timeout_secs != null && (payload.connect_timeout_secs < 1 || payload.connect_timeout_secs > 300)) {
      setError(t('连接超时必须在 1-300 秒之间'));
      return;
    }
    if (payload.headers_timeout_secs != null && (payload.headers_timeout_secs < 1 || payload.headers_timeout_secs > 600)) {
      setError(t('响应头等待上限必须在 1-600 秒之间'));
      return;
    }
    if (payload.stream_idle_secs != null && (payload.stream_idle_secs < 1 || payload.stream_idle_secs > 600)) {
      setError(t('流空闲超时必须在 1-600 秒之间'));
      return;
    }
    if (payload.body_timeout_secs != null && (payload.body_timeout_secs < 1 || payload.body_timeout_secs > 3600)) {
      setError(t('非流式响应体超时必须在 1-3600 秒之间'));
      return;
    }
    if (payload.retry_self_count != null && (payload.retry_self_count < 0 || payload.retry_self_count > 10)) {
      setError(t('同账号重试次数必须在 0-10 之间'));
      return;
    }
    if (payload.retry_interval_ms != null && (payload.retry_interval_ms < 0 || payload.retry_interval_ms > 10000)) {
      setError(t('重试间隔必须在 0-10000 毫秒之间'));
      return;
    }
    if (payload.billing_flat_quota != null && payload.billing_flat_quota <= 0) {
      setError(t('按次计费固定额度必须大于 0'));
      return;
    }
    if (payload.max_request_body_mb != null && payload.max_request_body_mb <= 0) {
      setError(t('请求体大小上限必须大于 0'));
      return;
    }

    // 直接换号/换渠道状态码：逗号/空格分隔，只接受 100–599
    const retryCodes = limits.retry_on_status
      .split(/[,\s]+/)
      .map((v) => v.trim())
      .filter((v) => v.length > 0)
      .map((v) => Number(v));
    if (retryCodes.some((code) => !Number.isInteger(code) || code < 100 || code > 599)) {
      setError(t('直接换号/换供应商状态码必须在 100-599 之间'));
      return;
    }
    const body: Record<string, unknown> = { ...payload, retry_on_status: retryCodes };

    setSaving(true);
    setError('');
    try {
      await api.updateLimits(body);
      addToast(t('设置更新成功'));
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  };

  if (loading) return <SkeletonList rows={6} />;

  return (
    <div>
      <div className="page-header">
        <h1>{t('系统设置')}</h1>
        <p>{t('settingsPageSubtitle')}</p>
      </div>

      {error && <div className="error-message">{error}</div>}

      <div className="settings-layout">
        {/* 左栏分类导航：对齐 Agent2API 的 CATEGORIES（超时/重试从「API 配置」里拆出来） */}
        <nav className="settings-nav" aria-label={t('设置分区')}>
          {SETTINGS_CATEGORIES.map((c) => {
            const Icon = c.icon;
            const active = c.key === activeTab;
            return (
              <button
                key={c.key}
                type="button"
                className={`settings-nav-item ${active ? 'active' : ''}`}
                aria-current={active ? 'page' : undefined}
                onClick={() => selectSettingsTab(c.key)}
              >
                <Icon size={15} />
                <span>{t(c.label)}</span>
              </button>
            );
          })}
        </nav>

        <div className="settings-pane">
      {activeTab === 'general' && (
        <>
          {/* 通用 = 使用限额 + 界面（主题 / 缩放） */}
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
              <div className="form-group" style={{ marginTop: 16, maxWidth: 280 }}>
                <label>{t('界面缩放')}</label>
                <select
                  className="form-input"
                  value={String(zoom)}
                  onChange={(e) => selectZoom(Number(e.target.value))}
                >
                  {ZOOM_LEVELS.map((level) => (
                    <option key={level} value={String(level)}>
                      {Math.round(level * 100)}%
                      {level === 1 ? t('（默认）') : ''}
                    </option>
                  ))}
                </select>
                <span className="form-hint">{t('整页等比缩放，保存在本机浏览器。')}</span>
              </div>
            </div>
          </div>

          {/* 反馈与需求（原「账户」分类迁入；对照 Agent2API「设置 → 反馈与需求」：跳 GitHub issue 模板） */}
          <div className="card" style={{ marginTop: 16 }}>
            <div className="card-header">
              <h2>{t('反馈与需求')}</h2>
              <p className="card-subtitle">
                {t('问题反馈、功能建议与提供商/模型支持请求都走 GitHub issue，便于跟踪进度。')}
              </p>
            </div>
            <div className="card-body">
              <div className="settings-actions" style={{ display: 'flex', gap: 12, flexWrap: 'wrap' }}>
                <a
                  className="btn btn-secondary"
                  href={`${REPO_URL}/issues/new?template=bug_report.yml`}
                  target="_blank"
                  rel="noreferrer"
                >
                  {t('问题反馈')}
                </a>
                <a
                  className="btn btn-secondary"
                  href={`${REPO_URL}/issues/new?template=feature_request.yml`}
                  target="_blank"
                  rel="noreferrer"
                >
                  {t('功能建议')}
                </a>
                <a
                  className="btn btn-secondary"
                  href={`${REPO_URL}/issues/new?template=provider_request.yml`}
                  target="_blank"
                  rel="noreferrer"
                >
                  {t('请求提供商/模型支持')}
                </a>
                <a
                  className="btn btn-secondary"
                  href={`${REPO_URL}/issues`}
                  target="_blank"
                  rel="noreferrer"
                >
                  {t('查看已有 issue')}
                </a>
              </div>
            </div>
          </div>
        </>
      )}

      {activeTab === 'timeout' && (
        <div className="card">
          <div className="card-header">
            <h2>{t('超时')}</h2>
            <p className="card-subtitle">{t('各阶段等待上游的最长时间，防止悬挂请求长期占用连接。')}</p>
          </div>
          <div className="card-body">
            <div className="settings-form">
              {TIMEOUT_FIELDS.map((f) => (
                <div key={f.field} className="form-group">
                  <label>{t(f.label)}</label>
                  <input
                    className="form-input"
                    type="number"
                    min={f.min}
                    max={f.max}
                    placeholder={f.placeholder}
                    value={limits[f.field]}
                    onChange={(e) => handleChange(f.field, e.target.value)}
                  />
                  <span className="form-hint">{t(f.hint)}</span>
                </div>
              ))}
              <div className="settings-actions">
                <button className="btn btn-primary" onClick={handleSave} disabled={saving}>
                  {saving ? t('保存中...') : t('保存更改')}
                </button>
              </div>
            </div>
          </div>
        </div>
      )}

      {activeTab === 'retry' && (
        <div className="card">
          <div className="card-header">
            <h2>{t('重试')}</h2>
            <p className="card-subtitle">{t('上游失败后换供应商 / 换账号的策略。')}</p>
          </div>
          <div className="card-body">
            <div className="settings-form">
              {RETRY_FIELDS.map((f) => (
                <div key={f.field} className="form-group">
                  <label>{t(f.label)}</label>
                  <input
                    className="form-input"
                    type={f.type}
                    min={f.min}
                    max={f.max}
                    placeholder={f.placeholder}
                    value={limits[f.field]}
                    onChange={(e) => handleChange(f.field, e.target.value)}
                  />
                  <span className="form-hint">{t(f.hint)}</span>
                </div>
              ))}
              <div className="settings-actions">
                <button className="btn btn-primary" onClick={handleSave} disabled={saving}>
                  {saving ? t('保存中...') : t('保存更改')}
                </button>
              </div>
            </div>
          </div>
        </div>
      )}

      {activeTab === 'perf' && (
        <>
          <div className="card">
            <div className="card-header">
              <h2>{t('限流配置')}</h2>
            </div>
            <div className="card-body">
              {rlLoading ? (
                <SkeletonList rows={4} />
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

          <div className="card" style={{ marginTop: 16 }}>
            <div className="card-header">
              <h2>{t('缓存管理')}</h2>
            </div>
            <div className="card-body">
              {cacheLoading ? (
                <SkeletonList rows={3} />
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
        </>
      )}

      {activeTab === 'billing' && (
        <>
          {/* 计费 = 易支付 + 用户分组（子标签切换） */}
          <Tabs<BillingSubTab>
            items={[
              { key: 'epay', label: t('易支付配置') },
              { key: 'groups', label: t('用户分组') },
            ]}
            active={billingSub}
            onChange={setBillingSub}
            ariaLabel={t('计费设置分区')}
          />
          <div style={{ marginTop: 16 }}>
            {billingSub === 'epay' && <Epay />}
            {billingSub === 'groups' && <Groups />}
          </div>

          <div className="card" style={{ marginTop: 16 }}>
            <div className="card-header">
              <h2>{t('按次计费')}</h2>
            </div>
            <div className="card-body">
              <div className="settings-form">
                <div className="form-group">
                  <label>{t('按次计费固定额度')}</label>
                  <input className="form-input" type="number" min="1" placeholder="2" value={limits.billing_flat_quota} onChange={(e) => handleChange('billing_flat_quota', e.target.value)} />
                  <span className="form-hint">{t('订阅计费模式为「按次」时，每次请求固定扣减的额度（全局统一，默认 2）。')}</span>
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
              <h2>{t('价格同步')}</h2>
            </div>
            <div className="card-body">
              {priceSyncLoading ? (
                <SkeletonList rows={3} />
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

          <div className="card" style={{ marginTop: 16 }}>
            <div className="card-header">
              <h2>{t('汇率配置')}</h2>
            </div>
            <div className="card-body">
              {exchangeRatesLoading ? (
                <SkeletonList rows={3} />
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
        </>
      )}

      {activeTab === 'gateway' && (
        <>
          {/* 网关 = 监听/CORS + 网络层 + 请求侧伪装与兼容开关（原 ops 子标签合流） */}
          <GatewayPanel />
          <div style={{ marginTop: 16 }}>
            <NetworkLayer />
          </div>
          <div style={{ marginTop: 16 }}>
            <SanitizePanel />
          </div>
          <div style={{ marginTop: 16 }}>
            {/* Cline 伪装头覆盖表（对照 Agent2API；保存后下一个 Cline 请求生效） */}
            <ClineHeadersPanel />
          </div>
          <div style={{ marginTop: 16 }}>
            {/* ZCode 验证码令牌池（对照 Agent2API；令牌由管理页后台铸造） */}
            <ZcodeCaptchaPanel />
          </div>
          <div style={{ marginTop: 16 }}>
            <PromptPanel />
          </div>
          <div style={{ marginTop: 16 }}>
            <DebugTrafficPanel />
          </div>
          <div style={{ marginTop: 16 }}>
            <StreamCompatPanel />
          </div>
          <div style={{ marginTop: 16 }}>
            <QueuePanel />
          </div>
          <div style={{ marginTop: 16 }}>
            <AutoContinuePanel />
          </div>
          <div style={{ marginTop: 16 }}>
            <ModelMetadataPanel />
          </div>

          <div className="card" style={{ marginTop: 16 }}>
            <div className="card-header">
              <h2>{t('请求体大小上限')}</h2>
              <p className="card-subtitle">{t('数据面入站请求的硬上限，超出直接拒绝，防止超大请求打爆内存。')}</p>
            </div>
            <div className="card-body">
              <div className="settings-form">
                <div className="form-group">
                  <label>{t('请求体大小上限 (MB)')}</label>
                  <input className="form-input" type="number" min="1" placeholder="25" value={limits.max_request_body_mb} onChange={(e) => handleChange('max_request_body_mb', e.target.value)} />
                  <span className="form-hint">{t('数据面接口单次请求体最大大小（含音频上传），超出返回 413，防止超大请求打爆内存。默认 25MB。')}</span>
                </div>
                <div className="settings-actions">
                  <button className="btn btn-primary" onClick={handleSave} disabled={saving}>
                    {saving ? t('保存中...') : t('保存更改')}
                  </button>
                </div>
              </div>
            </div>
          </div>
        </>
      )}


      {activeTab === 'assistant' && <AgentSettingsPanel />}

      {activeTab === 'chat' && <ChatSettingsPanel />}
      {activeTab === 'report' && <ReportSettingsPanel />}

      {activeTab === 'data' && (
        <>
          {/* 数据 = 存储/备份 + 日志保留（原 ops 的 data / logs 子标签） */}
          <DataManagement />
          <div style={{ marginTop: 16 }}>
            <LogRetention />
          </div>
        </>
      )}

      {/* 定时任务（原独立菜单 /scheduled-tasks 并入；embedded 隐藏自带 h1） */}
      {activeTab === 'scheduled' && <ScheduledTasks embedded />}

      {activeTab === 'notify' && <Notify />}

      {activeTab === 'update' && <UpdatePanel />}

      {activeTab === 'security' && (
        <>
          {/* 安全 = 安全策略（可配门槛）+ 安全监控 + IP 管理 */}
          <SecurityPolicyPanel />
          <div style={{ marginTop: 16 }}><Security /></div>
          <div style={{ marginTop: 16 }}><IpManagement /></div>
        </>
      )}

      <ConfirmDialog state={confirmState} onClose={() => setConfirmState(null)} />
        </div>
      </div>
    </div>
  );
}