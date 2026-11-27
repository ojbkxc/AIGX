import { useState, useEffect } from 'react';
import { useNavigate, useSearchParams } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import { SUPPORTED_LANGS, setLanguage } from '../i18n';
import {
  Monitor, Sun, Moon,
  SlidersHorizontal, Network, ShieldCheck, Download,
  MessageSquare, Info, Database, Bell, Layers, Timer, UserRound, KeyRound,
} from 'lucide-react';
import { api } from '../api';
import { useToast } from '../components/Toast';
import ConfirmDialog, { type ConfirmState } from '../components/ConfirmDialog';
import { SkeletonList, Switch } from '../components/ui';
import { getThemeMode, applyTheme, type ThemeMode } from '../lib/theme';
import { getZoom, applyZoom, ZOOM_LEVELS } from '../lib/zoom';
import Security from './Security';
import IpManagement from './IpManagement';
import NetworkLayer from './NetworkLayer';
import LogRetention from './LogRetention';
import UpdatePanel from '../components/UpdatePanel';
import GatewayPanel from '../components/GatewayPanel';
import SanitizePanel from '../components/SanitizePanel';
import ClineHeadersPanel from '../components/ClineHeadersPanel';
import ClientIdPanel from '../components/ClientIdPanel';
import GuardrailPanel from '../components/GuardrailPanel';
import ZcodeCaptchaPanel from '../components/ZcodeCaptchaPanel';
import SecurityPolicyPanel from '../components/SecurityPolicyPanel';
import PromptPanel from '../components/PromptPanel';
import PromptRulesPanel from '../components/PromptRulesPanel';
import WebToolsPanel from '../components/WebToolsPanel';
import BrandingSettingsPanel from '../components/BrandingSettingsPanel';
import DebugTrafficPanel from '../components/DebugTrafficPanel';
import ClientCompatPanel from '../components/ClientCompatPanel';
import QueuePanel from '../components/QueuePanel';
import ModelMetadataPanel from '../components/ModelMetadataPanel';
import SystemInfoPanel from '../components/SystemInfoPanel';
import DataManagement from '../components/DataManagement';
import CloudSyncPanel from '../components/CloudSyncPanel';
import SidebarLayoutPanel from '../components/SidebarLayoutPanel';
import ImageCompressPanel from '../components/ImageCompressPanel';
import OAuthConfigPanel from '../components/OAuthConfigPanel';
import SettingsOverview from '../components/SettingsOverview';
import './Settings.css';

interface LimitsForm {
  daily_limit: string;
  monthly_limit: string;
  threshold: string;
  max_request_body_mb: string;
}

interface DataResponse<T> {
  data?: T;
}

/**
 * 「超时」分类的四个字段（A2A v3.5.1 TIMEOUT_FIELDS 逐项对齐：连接中 / 等待响应 /
 * 流式空闲 / 非流式响应体）。
 *
 * 取值范围跟 **AIGX 后端**走：`settings.rs::handle_update_limits` 对
 * connect 钳在 1..=300、其余三项 1..=600（A2A 是统一 1..=3600）。这里按后端区间
 * 校验并在提示里写明真实上限——写 3600 会被后端静默钳到 600，界面若声称 3600
 * 就是撒谎。
 */
const TIMEOUT_FIELDS: Array<{ field: TimeoutField; labelKey: string; hintKey: string; max: number }> = [
  {
    field: 'connect_timeout_secs',
    labelKey: '连接中超时',
    hintKey: '建立上游 TCP/TLS 连接或代理隧道的最大等待时间（秒），范围 1~300。',
    max: 300,
  },
  {
    field: 'headers_timeout_secs',
    labelKey: '等待响应超时',
    hintKey: '请求发出后等待上游响应头的最大时间（秒），范围 1~600。',
    max: 600,
  },
  {
    field: 'stream_idle_secs',
    labelKey: '流式响应空闲超时',
    hintKey: '流式响应相邻数据之间允许的最大空闲时间（秒），收到新数据后重新计时，范围 1~600。',
    max: 600,
  },
  {
    field: 'body_timeout_secs',
    labelKey: '非流式响应超时',
    hintKey: '读取完整非流式响应体允许的最大时间（秒），范围 1~600。',
    max: 600,
  },
];

type TimeoutField =
  | 'connect_timeout_secs'
  | 'headers_timeout_secs'
  | 'stream_idle_secs'
  | 'body_timeout_secs';

/**
 * 汇率（`GET/PUT /api/pricing/exchange-rates`，扁平对象 `{USD, CNY, ...}`）。
 *
 * A2A v3.5.1 在「设置 → 通用 → 计量单位」里加了计价货币与汇率刷新；AIGX 的
 * 汇率编辑随计费页迁出后**没有任何调用方**（`getExchangeRates` 全库零消费），
 * 等于停在一个配不出来的状态。这里按可落地的子集接回来：USD 是基准（后端
 * 显式忽略写入），只编辑 CNY，写前按后端口径要求正数。
 */
interface ExchangeRatesShape {
  USD?: number;
  CNY?: number;
  [key: string]: unknown;
}

/**
 * 设置分区：对齐 Agent2API settings-page 的 CATEGORIES（通用 / 显示 / 网关 /
 * 安全 / 数据 / 反馈与需求 / 版本信息 / 更新）。v1.0.93 大块迁移：
 * AI 助手/聊天/报表/重试/超时/限流与缓存/计费/通知 八分类迁往顶级页或
 * 路由页（入口见各自页面），此处不再保留分类。账户不建（对应物 /profile
 * 个人中心已完整），提供商不建（已是顶级 /channels）。
 */
type SettingsTab =
  | 'account'
  | 'notifications'
  | 'general'
  | 'display'
  | 'gateway'
  | 'providers'
  | 'timeout'
  | 'security'
  | 'oauth'
  | 'data'
  | 'feedback'
  | 'system'
  | 'update';

const THEME_OPTIONS: Array<{ mode: ThemeMode; labelKey: string; hintKey: string; icon: typeof Sun }> = [
  { mode: 'light', labelKey: '浅色模式', hintKey: 'settingsThemeLightHint', icon: Sun },
  { mode: 'dark', labelKey: '深色模式', hintKey: 'settingsThemeDarkHint', icon: Moon },
  { mode: 'system', labelKey: '跟随系统', hintKey: 'settingsThemeSystemHint', icon: Monitor },
];

const LIMIT_FIELDS: Array<{ field: 'daily_limit' | 'monthly_limit' | 'threshold'; labelKey: string; hintKey: string; min?: number; max?: number }> = [
  { field: 'daily_limit', labelKey: '每日 Token 限额', hintKey: '每天允许的最大 Token 数。0 或空 = 无限制。', min: 0 },
  { field: 'monthly_limit', labelKey: '每月 Token 限额', hintKey: '每月允许的最大 Token 数。0 或空 = 无限制。', min: 0 },
  { field: 'threshold', labelKey: '告警阈值 (%)', hintKey: '触发告警的限额使用百分比（0-100）。', min: 0, max: 100 },
];

/** 设置分类：左栏导航，对齐 Agent2API v3.5.1 settings-model 的 CATEGORIES（顺序逐项一致）。 */
const SETTINGS_CATEGORIES: Array<{ key: SettingsTab; label: string; icon: typeof Sun }> = [
  { key: 'account', label: '账户', icon: UserRound },
  { key: 'notifications', label: '通知', icon: Bell },
  { key: 'general', label: '通用', icon: SlidersHorizontal },
  { key: 'display', label: '显示', icon: Monitor },
  { key: 'gateway', label: '网关', icon: Network },
  { key: 'providers', label: '提供商', icon: Layers },
  { key: 'timeout', label: '超时', icon: Timer },
  { key: 'security', label: '安全', icon: ShieldCheck },
  // OAuth 登录（GitHub / Google / LinuxDO）对接参数：后端 /api/oauth/config 一直存在，
  // 前端此前没有入口，管理员在线上找不到这三家的配置（v1.0.124 补）。
  { key: 'oauth', label: 'OAuth 登录', icon: KeyRound },
  { key: 'data', label: '数据', icon: Database },
  { key: 'feedback', label: '反馈与需求', icon: MessageSquare },
  { key: 'system', label: '版本信息', icon: Info },
  { key: 'update', label: '更新', icon: Download },
];

/** URL ?tab= 只认已注册分类，避免手改 URL 把设置页置空 */
const VALID_SETTINGS_TABS = new Set<string>(SETTINGS_CATEGORIES.map((c) => c.key));

export default function Settings() {
  const [limits, setLimits] = useState<LimitsForm>({
    daily_limit: '',
    monthly_limit: '',
    threshold: '',
    max_request_body_mb: '',
  });
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState('');
  // 超时四字段（独立保存：改超时不该顺带重提交每日/每月限额）
  const [timeouts, setTimeoutValues] = useState<Record<TimeoutField, string>>({
    connect_timeout_secs: '',
    headers_timeout_secs: '',
    stream_idle_secs: '',
    body_timeout_secs: '',
  });
  const [timeoutSaving, setTimeoutSaving] = useState(false);
  const [timeoutError, setTimeoutError] = useState('');
  const addToast = useToast();
  const { t, i18n } = useTranslation();

  // 当前激活的配置分类：左侧导航（对齐 Agent2API 的 CATEGORIES），
  // 每类只渲染自己那块，不再靠子标签层层下钻。
  // Tab 状态走 URL ?tab=（旧 /scheduled-tasks 与 ?tab=scheduled 重定向到「自动维护」页；
  // billing 的子标签仍为内部 state，不进 URL）。
  const [searchParams, setSearchParams] = useSearchParams();
  const navigate = useNavigate();
  // beta.7：定时任务分类已升级为顶级「自动维护」页，旧书签 ?tab=scheduled 重定向过去。
  useEffect(() => {
    if (searchParams.get('tab') === 'scheduled') navigate('/maintenance', { replace: true });
  }, [searchParams, navigate]);
  // 已知历史 tab 值的重定向表：这些分类后来迁去了独立页/别的 tab，旧书签不该静默落到
  // 「通用」——用户会得到「这个分类搬去哪了」的明确去向，而不是一个对不上的页面。
  // account 由 App.tsx 的 SettingsRoute 处理（→/profile），这里不再重复。
  useEffect(() => {
    const want = searchParams.get('tab');
    if (!want) return;
    const legacy: Record<string, string> = {
      billing: '/products?tab=billing',
      limit: '/limit',
      network: '/settings?tab=gateway',
      'network-layer': '/settings?tab=gateway',
    };
    if (legacy[want]) {
      navigate(legacy[want], { replace: true });
      return;
    }
    // 非法 tab（手改 URL / 已被删除的历史值）：URL 与界面状态要对得上——
    // 界面落到「通用」，URL 也替换成 ?tab=general，否则刷新/分享这条 URL 的人
    // 看到的是通用面板、地址栏却还是个无效值。
    if (!VALID_SETTINGS_TABS.has(want)) {
      const next = new URLSearchParams(searchParams);
      next.set('tab', 'general');
      setSearchParams(next, { replace: true });
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [searchParams, navigate]);
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

  // 界面分区：主题三态（system/light/dark），与登录页/侧边栏切换共享同一份持久化
  const [theme, setTheme] = useState<ThemeMode>(() => getThemeMode());

  // 界面缩放（80%–130%，localStorage 持久化；对照 Agent2API「设置 → 显示」）
  const [zoom, setZoom] = useState<number>(() => getZoom());

  // ── 邮箱白名单（注册限制：仅允许指定域名后缀的邮箱注册；随 /limits 读写）──
  // 编辑形态是**一个逗号分隔的字符串**（「用 , 隔开代表下一个」），提交时才拆成数组；
  // 逐行增删的编辑器对几个域名来说太啰嗦。
  const [emailWlEnabled, setEmailWlEnabled] = useState(false);
  const [emailWlText, setEmailWlText] = useState('');
  const [emailWlLoading, setEmailWlLoading] = useState(false);
  const [emailWlSaving, setEmailWlSaving] = useState(false);

  // ── 通用确认弹窗（用于清空缓存等危险操作）──
  const [confirmState, setConfirmState] = useState<ConfirmState | null>(null);

  // ── 汇率（USD 基准，CNY 可编辑）──
  const [fxRate, setFxRate] = useState('');
  const [fxSaved, setFxSaved] = useState('');
  const [fxLoading, setFxLoading] = useState(false);
  const [fxSaving, setFxSaving] = useState(false);
  const [fxError, setFxError] = useState('');

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
    loadEmailWhitelist();
    void loadExchangeRate();
  }, []);

  /** 读汇率：后端对未配置的币种回 0（「空 = 未配置」），这里渲染成空串 */
  const loadExchangeRate = async (): Promise<void> => {
    setFxLoading(true);
    setFxError('');
    try {
      const res = await api.getExchangeRates();
      const rates = (res?.data ?? {}) as ExchangeRatesShape;
      const cny = typeof rates.CNY === 'number' && rates.CNY > 0 ? String(rates.CNY) : '';
      setFxRate(cny);
      setFxSaved(cny);
    } catch (err) {
      setFxError(err instanceof Error ? err.message : String(err));
    } finally {
      setFxLoading(false);
    }
  };

  /** 存汇率：USD 是基准（后端忽略），只提交 CNY；<=0 后端会 400，前端先挡 */
  const handleSaveExchangeRate = async (): Promise<void> => {
    const raw = fxRate.trim();
    if (raw !== '' && (Number.isNaN(Number(raw)) || Number(raw) <= 0)) {
      setFxError(t('汇率必须是大于 0 的数字'));
      return;
    }
    setFxSaving(true);
    setFxError('');
    try {
      const res = await api.updateExchangeRates(raw === '' ? {} : { CNY: Number(raw) });
      const rates = (res?.data ?? {}) as ExchangeRatesShape;
      const next = typeof rates.CNY === 'number' && rates.CNY > 0 ? String(rates.CNY) : '';
      setFxRate(next);
      setFxSaved(next);
      addToast(t('汇率已保存'));
    } catch (err) {
      setFxError(err instanceof Error ? err.message : String(err));
    } finally {
      setFxSaving(false);
    }
  };
  const fxDirty = fxRate.trim() !== fxSaved.trim();

  // （限流 / 缓存 / 价格同步 / 汇率 的加载与保存已随 顶级「限流」页 与「套餐与兑换码」计费 Tab 迁出，v1.0.93）

  // ── 邮箱白名单（读：email_whitelist_enable + email_whitelist_suffix，随 /limits）──
  // 数组按 `,` / `，` / 换行 / 空白 连成一行展示，用户看到的即「逗号分隔」。
  const loadEmailWhitelist = async () => {
    setEmailWlLoading(true);
    try {
      const res = await api.getLimits();
      const d = res?.data as Record<string, unknown> | undefined;
      const suffix = Array.isArray(d?.email_whitelist_suffix)
        ? (d.email_whitelist_suffix as string[]).map((s) => String(s).trim()).filter((s) => s !== '')
        : [];
      setEmailWlEnabled(Boolean(d?.email_whitelist_enable));
      setEmailWlText(suffix.join(','));
    } catch {
      // 邮箱白名单未启用或加载失败，静默处理（区块照常渲染，默认关闭）
    } finally {
      setEmailWlLoading(false);
    }
  };

  // 保存：把逗号分隔文本拆成数组（`,` `，` 空白 换行都算分隔符），去空/去重/小写。
  // 域名大小写不敏感，统一小写后再提交，避免 user@163.COM 与白名单对不上。
  const handleEmailWhitelistSave = async () => {
    const seen = new Set<string>();
    const domains: string[] = [];
    for (const raw of emailWlText.split(/[,，\s]+/)) {
      const d = raw.trim().toLowerCase();
      // 容忍用户按习惯写 `@163.com`，去掉前导 @
      const norm = d.replace(/^@+/, '');
      if (norm === '' || seen.has(norm)) continue;
      seen.add(norm);
      domains.push(norm);
    }
    setEmailWlSaving(true);
    setError('');
    try {
      await api.updateLimits({
        email_whitelist_enable: emailWlEnabled,
        email_whitelist_suffix: domains,
      });
      // 回填规范化后的文本，让用户看到最终生效的形态（去重、去 @、小写）
      setEmailWlText(domains.join(','));
      addToast(t('邮箱白名单已保存，下一个注册立即生效'));
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setEmailWlSaving(false);
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
        max_request_body_mb: data.max_request_body_mb != null ? String(data.max_request_body_mb) : '',
      });
      // 超时四字段同源（同一份 /limits）：缺省/0 一律渲染成空串，空 = 跟随网关默认
      const raw = (res.data ?? res) as Record<string, unknown>;
      const pick = (key: TimeoutField): string => {
        const value = raw?.[key];
        return typeof value === 'number' && value > 0 ? String(value) : '';
      };
      setTimeoutValues({
        connect_timeout_secs: pick('connect_timeout_secs'),
        headers_timeout_secs: pick('headers_timeout_secs'),
        stream_idle_secs: pick('stream_idle_secs'),
        body_timeout_secs: pick('body_timeout_secs'),
      });
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  };

  /** 保存超时：只提交非空且落在后端区间内的字段，越界先在前端挡 */
  const handleSaveTimeouts = async (): Promise<void> => {
    const payload: Record<string, number> = {};
    for (const item of TIMEOUT_FIELDS) {
      const raw = timeouts[item.field].trim();
      if (raw === '') continue;
      const value = Number(raw);
      if (!Number.isInteger(value) || value < 1 || value > item.max) {
        setTimeoutError(t('{{label}}需在 1~{{max}} 秒之间', { label: t(item.labelKey), max: item.max }));
        return;
      }
      payload[item.field] = value;
    }
    if (!Object.keys(payload).length) {
      setTimeoutError(t('没有可保存的超时设置'));
      return;
    }
    setTimeoutSaving(true);
    setTimeoutError('');
    try {
      await api.updateLimits(payload);
      addToast(t('超时设置已保存'));
      await loadLimits();
    } catch (err) {
      setTimeoutError(err instanceof Error ? err.message : String(err));
    } finally {
      setTimeoutSaving(false);
    }
  };

  const handleChange = (field: keyof LimitsForm, value: string) => {
    setLimits({ ...limits, [field]: value });
  };

  const handleSave = async () => {
    // NaN 兜底：任一数值字段填入非数字内容时直接报错，不提交
    const numericFields = [
      limits.daily_limit,
      limits.monthly_limit,
      limits.threshold,
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
    if (limits.max_request_body_mb !== '') payload.max_request_body_mb = Number(limits.max_request_body_mb);

    // 阈值按原始输入（0-100 百分比）校验；payload.threshold 已除 100，不再用于范围比较
    if (payload.daily_limit < 0 || payload.monthly_limit < 0 || (limits.threshold !== '' && (Number(limits.threshold) < 0 || Number(limits.threshold) > 100))) {
      setError(t('请输入有效值。日/月限额必须 >= 0。阈值必须在 0-100 之间。'));
      return;
    }
    if (payload.max_request_body_mb != null && payload.max_request_body_mb <= 0) {
      setError(t('请求体大小上限必须大于 0'));
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
      {/* 账户（A2A v3.5.1 AccountPane）：AIGX 的账户面完整落在 /profile 个人中心，
          这里只落**结构占位**——分类、标题、入口与文案按 A2A 摆好，点进去是真的，
          但不在设置里重复建一套表单（重复建必然两处漂移）。 */}
      {activeTab === 'account' && (
        <div className="card">
          <div className="card-header">
            <h2>{t('账户')}</h2>
            <p className="card-subtitle">{t('账户资料、密码与登录态在「个人中心」管理。')}</p>
          </div>
          <div className="card-body">
            <div className="settings-actions">
              <button type="button" className="btn btn-primary" onClick={() => navigate('/profile')}>
                {t('前往个人中心')}
              </button>
            </div>
          </div>
        </div>
      )}

      {/* 通知（A2A v3.5.1 notifications 分类）：AIGX 的通知配置在顶级「通知」页 */}
      {activeTab === 'notifications' && (
        <div className="card">
          <div className="card-header">
            <h2>{t('通知')}</h2>
            <p className="card-subtitle">{t('余额预警、签到提醒与投递渠道在「通知」页配置。')}</p>
          </div>
          <div className="card-body">
            <div className="settings-actions">
              <button type="button" className="btn btn-primary" onClick={() => navigate('/notify')}>
                {t('前往通知设置')}
              </button>
            </div>
          </div>
        </div>
      )}

      {activeTab === 'general' && (
        <>
          {/* 配置总览：设置页默认落点的第一张卡，回答「有哪些配置、各自在哪一页」。
              加它的原因见 components/SettingsOverview.tsx 头注释——易支付 / OAuth 都
              不在本页，旧版这里只有「使用限额」，管理员一进来看到的就是一张没有
              易支付的页面。不新增左栏分类（导航项数量由 e2e 钉死为当前值，且侧栏
              正在单独精简），所以挂在默认分类里。 */}
          <SettingsOverview />

          {/* 通用 = 使用限额 + 界面（主题 / 缩放） */}
          <div className="card" style={{ marginTop: 16 }}>
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

          {/* 汇率：A2A v3.5.1 的「计量单位」面板里带计价货币与汇率刷新；
              AIGX 侧等价物是 USD→CNY 汇率（后端 USD 为基准、写入忽略 USD）。 */}
          <div className="card" style={{ marginTop: 16 }}>
            <div className="card-header">
              <h2>{t('汇率与计价')}</h2>
              <p className="card-subtitle">
                {t('模型价格与账单按美元计价，这里配置 1 美元折算多少人民币，用于人民币口径的展示与结算。')}
              </p>
            </div>
            <div className="card-body">
              <div className="settings-form">
                <div className="form-group" style={{ maxWidth: 320 }}>
                  <label htmlFor="fxCnyRate">{t('人民币汇率（1 USD = ? CNY）')}</label>
                  <input
                    id="fxCnyRate"
                    className="form-input"
                    type="number"
                    min="0"
                    step="0.0001"
                    placeholder={t('未配置')}
                    value={fxRate}
                    disabled={fxLoading || fxSaving}
                    onChange={(e) => setFxRate(e.target.value)}
                  />
                  <span className="form-hint">
                    {fxSaved
                      ? t('当前生效：1 USD ≈ ¥{{rate}}', { rate: fxSaved })
                      : t('尚未配置汇率，人民币口径会按 0 处理。')}
                  </span>
                </div>
                {fxError ? (
                  <div className="form-group">
                    <span className="form-hint" style={{ color: 'var(--danger-color, #dc2626)' }}>
                      {fxError}
                    </span>
                  </div>
                ) : null}
                <div className="settings-actions">
                  <button
                    className="btn btn-primary"
                    onClick={() => void handleSaveExchangeRate()}
                    disabled={fxSaving || fxLoading || !fxDirty}
                  >
                    {fxSaving ? t('保存中...') : t('保存更改')}
                  </button>
                  <button
                    className="btn btn-secondary"
                    style={{ marginLeft: 8 }}
                    onClick={() => void loadExchangeRate()}
                    disabled={fxLoading || fxSaving}
                  >
                    {t('刷新汇率')}
                  </button>
                </div>
              </div>
            </div>
          </div>

          {/* 邮箱白名单（注册限制：仅允许指定域名后缀的邮箱注册；保存后下一个注册立即生效） */}
          <div className="card" style={{ marginTop: 16 }}>
            <div className="card-header">
              <h2>{t('邮箱白名单')}</h2>
              <p className="card-subtitle">{t('settingsEmailWhitelistHint')}</p>
            </div>
            <div className="card-body">
              {emailWlLoading ? (
                <SkeletonList rows={3} />
              ) : (
                <div className="settings-form">
                  <div className="form-group">
                    <label style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
                      <Switch
                        checked={emailWlEnabled}
                        onChange={setEmailWlEnabled}
                      />
                      {t('启用邮箱白名单')}
                    </label>
                    <span className="form-hint">{t('settingsEmailWhitelistEnableHint')}</span>
                  </div>

                  <div className="form-group">
                    <label htmlFor="emailWhitelistDomains">{t('允许的邮箱域名')}</label>
                    <input
                      id="emailWhitelistDomains"
                      className="form-input"
                      value={emailWlText}
                      placeholder="qq.com,163.com,gmail.com"
                      onChange={(e) => setEmailWlText(e.target.value)}
                      spellCheck={false}
                      autoComplete="off"
                    />
                    <span className="form-hint">{t('settingsEmailWhitelistDomainsHint')}</span>
                  </div>

                  <div className="settings-actions">
                    <button
                      className="btn btn-primary"
                      onClick={handleEmailWhitelistSave}
                      disabled={emailWlSaving}
                    >
                      {emailWlSaving ? t('保存中...') : t('保存更改')}
                    </button>
                  </div>
                </div>
              )}
            </div>
          </div>

          {/* 品牌自定义（面板名称 / Logo / favicon） */}
          <BrandingSettingsPanel />
        </>
      )}

      {/* 显示 = 主题 / 缩放 / 语言（纯前端偏好；对照 Agent2API「设置 → 显示」） */}
      {activeTab === 'display' && (
        <>
          <div className="card">
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

          {/* 语言（A2A 显示分类里的语言设置；侧栏底部切换保留为快捷入口） */}
          <div className="card" style={{ marginTop: 16 }}>
            <div className="card-header">
              <h2>{t('语言')}</h2>
              <p className="card-subtitle">{t('界面语言，切换后立即生效并保存在本机。')}</p>
            </div>
            <div className="card-body">
              <div className="form-group" style={{ maxWidth: 280 }}>
                <select
                  className="form-input"
                  value={i18n.language || 'zh'}
                  onChange={(e) => setLanguage(e.target.value as (typeof SUPPORTED_LANGS)[number]['code'])}
                >
                  {SUPPORTED_LANGS.map((l) => (
                    <option key={l.code} value={l.code}>
                      {l.label}
                    </option>
                  ))}
                </select>
              </div>
            </div>
          </div>

          {/* 侧边栏菜单：逐项显隐 / 拖拽排序 / 分组重命名 / 自定义分组（落库 + 跨端同步） */}
          <SidebarLayoutPanel />
        </>
      )}

      {/* 反馈与需求（对照 Agent2API「设置 → 反馈与需求」：issue 表单入口 + 工单通道） */}
      {activeTab === 'feedback' && (
        <div className="card">
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
              <button
                type="button"
                className="btn btn-secondary"
                onClick={() => navigate('/tickets')}
              >
                {t('提交工单（站内通道）')}
              </button>
            </div>
          </div>
        </div>
      )}

      {/* 限流与缓存 → 顶级「限流」页（v1.0.93 迁出；RuntimeCapacityPanel 同迁） */}

      {/* 计费 → 「套餐与兑换码」页新 Tab（v1.0.93 迁出：易支付 / 用户分组 / 按次计费 / 价格同步 / 汇率） */}

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
            {/* 客户端识别（对照 Agent2API；规则保存后下一个请求生效） */}
            <ClientIdPanel />
          </div>
          <div style={{ marginTop: 16 }}>
            {/* 内容护栏（输入/输出黑名单 + 200 假答复门控；保存即热生效） */}
            <GuardrailPanel />
          </div>
          <div style={{ marginTop: 16 }}>
            {/* ZCode 验证码令牌池（对照 Agent2API；令牌由管理页后台铸造） */}
            <ZcodeCaptchaPanel />
          </div>
          <div style={{ marginTop: 16 }}>
            <PromptPanel />
          </div>
          <div style={{ marginTop: 16 }}>
            {/* 系统提示词规则引擎（beta.2：按规则匹配决定 system 的处理方式；后端 /api/prompt/rules 未就绪时降级） */}
            <PromptRulesPanel />
          </div>
          <div style={{ marginTop: 16 }}>
            {/* 网页工具调用模拟（beta.2：为声明支持的网页模型转换工具调用；后端 /api/web-tool-emulation 未就绪时降级） */}
            <WebToolsPanel />
          </div>
          <div style={{ marginTop: 16 }}>
            <DebugTrafficPanel />
          </div>
          <div style={{ marginTop: 16 }}>
            <ClientCompatPanel />
          </div>
          <div style={{ marginTop: 16 }}>
            <QueuePanel />
          </div>
          <div style={{ marginTop: 16 }}>
            {/* 转发前压缩对话内嵌大图（开关 + 长边/质量/阈值；真正的压缩在后端） */}
            <ImageCompressPanel />
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


      {/* 提供商（A2A v3.5.1 settings-providers）：AIGX 的提供商管理是顶级「渠道」页 */}
      {activeTab === 'providers' && (
        <div className="card">
          <div className="card-header">
            <h2>{t('提供商')}</h2>
            <p className="card-subtitle">{t('提供商渠道、密钥与模型映射在「渠道」页维护。')}</p>
          </div>
          <div className="card-body">
            <div className="settings-actions">
              <button type="button" className="btn btn-primary" onClick={() => navigate('/channels')}>
                {t('前往渠道管理')}
              </button>
            </div>
          </div>
        </div>
      )}

      {/* 超时（A2A v3.5.1 TimeoutPane）：四项字段逐个对齐，取值上限按 AIGX 后端区间 */}
      {activeTab === 'timeout' && (
        <div className="card">
          <div className="card-header">
            <h2>{t('请求超时')}</h2>
            <p className="card-subtitle">
              {t('数据面转发到上游时的各阶段等待上限；留空 = 跟随网关默认值。保存后下一个请求生效。')}
            </p>
          </div>
          <div className="card-body">
            <div className="settings-form">
              {TIMEOUT_FIELDS.map((item) => (
                <div key={item.field} className="form-group">
                  <label htmlFor={`timeout-${item.field}`}>{t(item.labelKey)}</label>
                  <input
                    id={`timeout-${item.field}`}
                    className="form-input"
                    type="number"
                    min={1}
                    max={item.max}
                    placeholder={t('跟随默认')}
                    value={timeouts[item.field]}
                    onChange={(e) =>
                      setTimeoutValues((prev) => ({ ...prev, [item.field]: e.target.value }))
                    }
                  />
                  <span className="form-hint">{t(item.hintKey)}</span>
                </div>
              ))}
              {timeoutError ? (
                <div className="form-group">
                  <span className="form-hint" style={{ color: 'var(--danger-color, #dc2626)' }}>
                    {timeoutError}
                  </span>
                </div>
              ) : null}
              <div className="settings-actions">
                <button
                  className="btn btn-primary"
                  onClick={() => void handleSaveTimeouts()}
                  disabled={timeoutSaving}
                >
                  {timeoutSaving ? t('保存中...') : t('保存更改')}
                </button>
              </div>
            </div>
          </div>
        </div>
      )}

      {/* AI 助手 → 助手页自带设置抽屉（v1.0.93 迁出）；聊天 → /chat 页；报表 → 仪表盘 Tab；
          图片兼容（ImageCompatPanel）→ 路由「特殊请求路由 → 图片兼容」 */}

      {activeTab === 'oauth' && <OAuthConfigPanel />}

      {activeTab === 'data' && (
        <>
          {/* 数据 = 存储/备份 + 云同步 + 日志保留（原 ops 的 data / logs 子标签） */}
          <DataManagement />
          <div style={{ marginTop: 16 }}>
            <CloudSyncPanel />
          </div>
          <div style={{ marginTop: 16 }}>
            <LogRetention />
          </div>
        </>
      )}

      {/* 系统信息（对照 Agent2API 设置页「版本信息」：一屏看全 + 一键复制） */}
      {activeTab === 'system' && <SystemInfoPanel />}

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