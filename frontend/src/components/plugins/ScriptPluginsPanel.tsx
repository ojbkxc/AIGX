/**
 * ScriptPluginsPanel — 脚本插件（Magpie）管理区。
 *
 * 契约对齐：
 * - 后端 src/api/admin/script_plugins.rs 8 端点（list/catalog/refresh/install/
 *   enable/uninstall/options/rebuild）；
 * - src/plugin_host/script/catalog（三分类目录：official / recommended /
 *   third-party，前端按 magpieOfficial / magpieRecommended 分组）。
 *
 * 安全口径：脚本不是 OS 沙箱——安装必须经过含「脚本不是沙箱，将以 Bun
 * 进程权限运行」文案的二次确认（confirm 后传 trusted=true，后端 operations
 * 还会再强制校验一次）。
 */

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Copy, ExternalLink, Loader2, RefreshCw, RotateCcw, Search, Star, Trash2 } from 'lucide-react';
import { api } from '../../api';
import { useToast } from '../Toast';
import ConfirmDialog, { type ConfirmState } from '../ConfirmDialog';
import PluginIcon from './PluginIcon';
import { Badge, Button, Card, EmptyState, Loading, Switch, Tooltip } from '../ui';
import type {
  ScriptCatalogEntryView,
  ScriptCatalogPayload,
  ScriptCatalogStatus,
  ScriptPackageView,
  ScriptPackagesPayload,
  ScriptProviderOption,
  ScriptSettingsView,
} from '../../types';
import './ScriptPluginsPanel.css';

/** options 字节上限（对齐后端 `MAX_SETTINGS_BYTES = 256 KiB`，超限后端 400） */
const MAX_OPTIONS_BYTES = 256 * 1024;

/** unix 秒 → 日期（目录时间戳展示用） */
function fmtUnix(value: number | null | undefined): string {
  if (!value || value <= 0) return '—';
  return new Date(value * 1000).toLocaleString();
}

/** 错误文案展示上限：后端已截断到 512 字符，这里再兜一层，避免 toast 被撑爆 */
const MAX_ERROR_DISPLAY = 300;

/**
 * 错误 → 单行可读文案。
 *
 * 宿主启动失败时后端可能带回多行 stderr（已截断），toast/错误条里换行与
 * 超长文本会溢出容器。这里压成单行并限长；完整文案由调用方通过 title
 * 提示（悬停可见）。
 */
function formatError(err: unknown): string {
  const raw = err instanceof Error ? err.message : String(err);
  const oneLine = raw.replace(/\s*\n+\s*/g, ' ⏎ ').trim();
  return oneLine.length > MAX_ERROR_DISPLAY ? `${oneLine.slice(0, MAX_ERROR_DISPLAY)}…` : oneLine;
}

/** 目录刷新是否已有进展：刷新是后台异步任务（refresh 端点只负责唤醒），
 * 阶段时间戳前进即视为本轮已落地。无基线时退化为「两个阶段都不过期」。
 */
function catalogAdvanced(prev: ScriptCatalogStatus | undefined, next: ScriptCatalogStatus): boolean {
  if (!prev) return !next.market_stale && !next.github_stale;
  return next.market_at !== prev.market_at || next.github_at !== prev.github_at;
}

/** 发布者三分类 → 徽标（tone 与原生插件页 badge 色板同源） */
function PublisherBadge({ publisher }: { publisher: string }): JSX.Element {
  const { t } = useTranslation();
  const tone: 'info' | 'success' | 'neutral' =
    publisher === 'official' ? 'info' : publisher === 'recommended' ? 'success' : 'neutral';
  return <Badge tone={tone}>{t(publisher === 'official' ? '官方' : publisher === 'recommended' ? '推荐' : '第三方')}</Badge>;
}

/** 目录条目卡片（商店形态：安装 / 已安装徽标 / 可更新徽标） */
function CatalogCard({
  entry,
  installed,
  installing,
  busy,
  onInstall,
}: {
  entry: ScriptCatalogEntryView;
  installed: ScriptPackageView | undefined;
  installing: boolean;
  busy: boolean;
  onInstall: (entry: ScriptCatalogEntryView) => void;
}): JSX.Element {
  const { t } = useTranslation();
  const repoUrl = entry.repositoryUrl || (entry.repository ? `https://github.com/${entry.repository}` : null);
  const isInstalled = !!installed;
  const updatable = isInstalled && !!entry.version && installed.version !== entry.version;
  return (
    <div className={`pl-card${entry.installable ? '' : ' is-disabled'}`}>
      <div className="pl-card-head">
        <PluginIcon sources={entry.iconSources} label={entry.label || entry.name} />
        <div className="pl-card-title">
          <h3>
            {entry.label || entry.name}
            {entry.version ? <span className="pl-ver">v{entry.version}</span> : null}
          </h3>
          <div className="pl-card-badges">
            <PublisherBadge publisher={entry.publisher} />
            <Badge tone="neutral">{t('来源 {{source}}', { source: entry.source })}</Badge>
            {updatable ? <Badge tone="warning">{t('可更新')}</Badge> : null}
            {isInstalled ? <Badge tone="success">{t('已安装')}</Badge> : null}
          </div>
        </div>
      </div>
      {entry.summary ? <p className="pl-card-desc">{entry.summary}</p> : null}
      <div className="pl-card-meta">
        {entry.stars ? (
          <span className="pls-stars">
            <Star size={11} /> {t('{{count}} 星', { count: entry.stars })}
          </span>
        ) : null}
        {repoUrl ? (
          <a className="pls-repo" href={repoUrl} target="_blank" rel="noreferrer">
            <ExternalLink size={11} /> {t('仓库')}
          </a>
        ) : null}
        {entry.providers.length > 0 ? (
          <span className="pls-providers">
            {entry.providers.slice(0, 5).map((p) => (
              <Badge key={p} tone="neutral">{p}</Badge>
            ))}
            {entry.providers.length > 5 ? (
              <Badge tone="neutral">{t('还有 {{count}} 家', { count: entry.providers.length - 5 })}</Badge>
            ) : null}
          </span>
        ) : null}
      </div>
      {!entry.installable && entry.unavailableReason ? (
        <p className="pl-card-err" title={entry.unavailableReason}>{entry.unavailableReason}</p>
      ) : null}
      <div className="pl-card-actions">
        {isInstalled ? (
          <span className="pl-store-hint">{t('已安装，在下方列表管理')}</span>
        ) : (
          <Button
            variant="primary"
            size="sm"
            disabled={!entry.installable || busy || installing}
            title={entry.unavailableReason ?? undefined}
            onClick={() => onInstall(entry)}
          >
            {installing ? <Loader2 size={13} className="spin" /> : null}
            {installing ? t('安装中…') : t('安装')}
          </Button>
        )}
      </div>
    </div>
  );
}

/** 已安装脚本包卡片（启停 / 卸载 / 配置） */
function InstalledScriptCard({
  pkg,
  busy,
  onToggleEnabled,
  onProviderEnabled,
  onUninstall,
  onEditOptions,
}: {
  pkg: ScriptPackageView;
  busy: boolean;
  onToggleEnabled: (pkg: ScriptPackageView, enabled: boolean) => void;
  onProviderEnabled: (pkg: ScriptPackageView, provider: string, enabled: boolean) => void;
  onUninstall: (pkg: ScriptPackageView) => void;
  onEditOptions: (pkg: ScriptPackageView) => void;
}): JSX.Element {
  const { t } = useTranslation();
  return (
    <div className="pl-card is-installed">
      <div className="pl-card-head">
        <PluginIcon src={pkg.icon} label={pkg.label || pkg.id} />
        <div className="pl-card-title">
          <h3>
            {pkg.label || pkg.id}
            {pkg.version ? <span className="pl-ver">v{pkg.version}</span> : null}
          </h3>
          <div className="pl-card-badges">
            <Badge tone={pkg.enabled ? 'info' : 'neutral'}>{pkg.enabled ? t('已启用') : t('已停用')}</Badge>
            <Badge tone="neutral">{t('来源 {{source}}', { source: pkg.source })}</Badge>
            <Badge tone={pkg.trusted ? 'success' : 'warning'}>{pkg.trusted ? t('受信任') : t('未信任')}</Badge>
            <Badge tone={pkg.providerRouted ? 'success' : 'neutral'}>
              {pkg.providerRouted ? t('已接管') : t('未接管')}
            </Badge>
          </div>
        </div>
      </div>
      <p className="pl-card-meta">
        {pkg.summary ? pkg.summary : null}
      </p>
      {/* 宿主回填的加载错误：解释「已安装但未接管」的原因（完整文本进 title） */}
      {pkg.error ? (
        <p
          className="pl-card-meta"
          style={{ color: 'var(--danger-color)', display: 'flex', alignItems: 'center', gap: 6 }}
          title={pkg.error}
        >
          <Badge tone="warning">{t('加载失败')}</Badge>
          <span style={{ flex: 1, minWidth: 0 }}>{pkg.error}</span>
          <button
            type="button"
            className="btn btn-outline btn-sm"
            style={{ flex: '0 0 auto', display: 'inline-flex', alignItems: 'center', gap: 4 }}
            title={t('复制错误')}
            aria-label={t('复制错误')}
            onClick={() => {
              const text = pkg.error ?? '';
              if (!text) return;
              // 复制失败静默（权限/非安全上下文），不打断页面
              void navigator.clipboard?.writeText(text).catch(() => undefined);
            }}
          >
            <Copy size={11} /> {t('复制错误')}
          </button>
        </p>
      ) : null}
      <p className="pl-card-meta">
        <span>{t('{{count}} 家提供商', { count: pkg.providers.length })}</span>
        <span>{t('安装于 {{time}}', { time: fmtUnix(pkg.installedAt) })}</span>
        {pkg.repository ? <span>{t('仓库 {{repo}}', { repo: pkg.repository })}</span> : null}
      </p>
      {pkg.providers.length > 0 ? (
        <ul className="pl-providers">
          {pkg.providers.map((p) => (
            <li key={p.id} className="pl-provider-row">
              <span className="pl-provider-name" title={p.id}>{p.label || p.id}</span>
              <span className="pl-provider-dot" title={p.id} />
              {/* provider 级开关：包内 disabled_providers（后端补 enabled 前按 true 兜底） */}
              <Switch
                checked={p.enabled ?? true}
                disabled={busy}
                onChange={(v) => onProviderEnabled(pkg, p.id, v)}
                ariaLabel={`${t('启用')} ${p.label || p.id}`}
              />
            </li>
          ))}
        </ul>
      ) : null}
      <div className="pl-card-actions">
        <label className="pl-switch-label">
          {t('启用')}
          <Switch
            checked={pkg.enabled}
            disabled={busy || !pkg.trusted}
            onChange={(v) => onToggleEnabled(pkg, v)}
            ariaLabel={`${t('启用')} ${pkg.label || pkg.id}`}
          />
        </label>
        <Button variant="outline" size="sm" disabled={busy} onClick={() => onEditOptions(pkg)}>
          {t('配置')}
        </Button>
        <Tooltip
          label={
            pkg.source === 'local'
              ? t('卸载仅移除本机目录包的插件登记、配置与元数据，本机文件不会被删除')
              : t('卸载后该脚本包的提供商将停止接管请求，插件包目录会被删除')
          }
        >
          <Button variant="danger" size="sm" disabled={busy} onClick={() => onUninstall(pkg)}>
            <Trash2 size={13} /> {t('卸载')}
          </Button>
        </Tooltip>
      </div>
    </div>
  );
}

/** 选项声明里的多语文案取值（labels/descriptions/confirms 优先，回退单语字段） */
function optionWord(
  option: ScriptProviderOption,
  key: 'label' | 'description' | 'confirm',
  lang: string,
): string {
  const map =
    key === 'label' ? option.labels : key === 'description' ? option.descriptions : option.confirms;
  if (map && typeof map === 'object') {
    for (const candidate of [lang, lang.split('-')[0], 'zh', 'zh-CN', 'en']) {
      const hit = map[candidate];
      if (typeof hit === 'string' && hit.trim()) return hit;
    }
  }
  const single = option[key];
  return typeof single === 'string' ? single : '';
}

/**
 * 配置弹窗（对齐 A2A `ScriptPluginSettings` + `ScriptPluginOptions`）。
 *
 * 数据源是 `POST /api/plugins/scripts/settings`（只带 id = 只读），返回
 * `{options, providerOptions, schema}`：
 * - `schema.providerOptions` 渲染逐 provider 开关（运行中实时生效，不重建宿主）；
 * - 高级参数（options）是包级 JSON，保存后启用中的包会重建宿主；
 * - 设置端点不可用时降级到旧的 `/scripts/options`（仅包级参数），不整块失效。
 */
function OptionsDialog({
  pkg,
  busy,
  onClose,
  onSaved,
}: {
  pkg: ScriptPackageView;
  busy: boolean;
  onClose: () => void;
  onSaved: () => Promise<void> | void;
}): JSX.Element {
  const { t, i18n } = useTranslation();
  const addToast = useToast();
  const lang = i18n.language || 'zh';
  const mounted = useRef(true);
  const [loading, setLoading] = useState(true);
  const [view, setView] = useState<ScriptSettingsView | null>(null);
  const [providerOptions, setProviderOptions] = useState<Record<string, Record<string, boolean>>>({});
  const [draft, setDraft] = useState('{}');
  const [optionsDirty, setOptionsDirty] = useState(false);
  const [settingsAvailable, setSettingsAvailable] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState('');
  /** 需要二次确认的选项（打开前先展示插件声明的 confirm 文案） */
  const [pending, setPending] = useState<{ provider: string; option: ScriptProviderOption } | null>(null);
  /** 兜底 options（设置端点不可用时）：用 ref 固化首值，避免轮询改包对象触发重载。 */
  const fallbackOptions = useRef(pkg.options?.values ?? {});

  useEffect(() => {
    mounted.current = true;
    void (async () => {
      try {
        const res = await api.getScriptSettings(pkg.id, pkg.version);
        if (!mounted.current) return;
        const data = res?.data ?? null;
        setView(data);
        setProviderOptions(data?.providerOptions ?? {});
        setDraft(JSON.stringify(data?.options ?? fallbackOptions.current, null, 2));
        setSettingsAvailable(true);
      } catch {
        if (!mounted.current) return;
        // 设置端点不可用：保留旧的 options 编辑能力，提示用户范围受限
        setError(t('设置接口不可用，仅可编辑包级参数'));
        setDraft(JSON.stringify(fallbackOptions.current, null, 2));
        setSettingsAvailable(false);
      } finally {
        if (mounted.current) setLoading(false);
      }
    })();
    return () => {
      mounted.current = false;
    };
  }, [pkg.id, pkg.version, t]);

  const applyOption = (provider: string, key: string, on: boolean): void => {
    setProviderOptions((prev) => ({ ...prev, [provider]: { ...(prev[provider] ?? {}), [key]: on } }));
  };

  const toggleOption = (provider: string, option: ScriptProviderOption, next: boolean): void => {
    if (next && optionWord(option, 'confirm', lang)) {
      setPending({ provider, option });
      return;
    }
    applyOption(provider, option.key, next);
  };

  const save = async (): Promise<void> => {
    if (saving || busy) return;
    let options: Record<string, unknown>;
    try {
      const parsed: unknown = JSON.parse(draft);
      if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) {
        throw new Error(t('插件参数必须是 JSON 对象'));
      }
      options = parsed as Record<string, unknown>;
    } catch (err) {
      setError(err instanceof SyntaxError ? t('JSON 格式不正确') : formatError(err));
      return;
    }
    // 后端强制 ≤ 256 KiB：本地先量一次，给出可读错误
    const bytes = new TextEncoder().encode(JSON.stringify(options)).length;
    if (bytes > MAX_OPTIONS_BYTES) {
      setError(t('配置过大（{{size}} KiB），上限 256 KiB', { size: Math.ceil(bytes / 1024) }));
      return;
    }
    setSaving(true);
    setError('');
    try {
      if (settingsAvailable) {
        // options 未改动就不回传，避免无谓地重建宿主；providerOptions 始终保存
        await api.saveScriptSettings(
          pkg.id,
          { ...(optionsDirty ? { options } : {}), providerOptions },
          pkg.version,
        );
      } else {
        await api.saveScriptPackageOptions(pkg.id, options, pkg.version);
      }
      addToast(t('配置已保存'));
      await onSaved();
      onClose();
    } catch (err) {
      setError(formatError(err));
    } finally {
      if (mounted.current) setSaving(false);
    }
  };

  const optionGroups = Object.entries(view?.schema?.providerOptions ?? {});
  const disabled = busy || saving;

  return (
    <div className="modal-overlay" role="dialog" aria-modal="true" onClick={onClose}>
      <div className="modal" style={{ maxWidth: 600, width: '92%' }} onClick={(e) => e.stopPropagation()}>
        <div className="modal-header">
          <h3>{t('编辑配置 · {{name}}', { name: pkg.label || pkg.id })}</h3>
          <button className="modal-close" onClick={onClose} aria-label={t('关闭')}>
            &times;
          </button>
        </div>
        <div className="modal-body">
          {loading ? (
            <Loading text={t('读取配置')} />
          ) : (
            <>
              {optionGroups.length > 0 ? (
                <section className="pls-opt-section">
                  <h4 className="pls-opt-heading">{t('插件选项')}</h4>
                  {optionGroups.map(([provider, options]) => (
                    <div key={provider} className="pls-opt-provider">
                      <p className="pls-opt-provider-name">
                        {pkg.providers.find((p) => p.id === provider)?.label || provider}
                      </p>
                      {options.map((option) => {
                        const desc = optionWord(option, 'description', lang);
                        const confirming = pending?.provider === provider && pending.option.key === option.key;
                        return (
                          <div key={option.key} className="pls-opt-switch-row">
                            <div className="pls-opt-switch-main">
                              <span>{optionWord(option, 'label', lang) || option.key}</span>
                              {desc ? <span className="pls-opt-desc">{desc}</span> : null}
                              {confirming ? (
                                <div className="pls-opt-confirm">
                                  <span>{optionWord(option, 'confirm', lang)}</span>
                                  <span className="pls-opt-confirm-actions">
                                    <Button
                                      variant="outline"
                                      size="sm"
                                      onClick={() => setPending(null)}
                                    >
                                      {t('取消')}
                                    </Button>
                                    <Button
                                      variant="primary"
                                      size="sm"
                                      onClick={() => {
                                        applyOption(provider, option.key, true);
                                        setPending(null);
                                      }}
                                    >
                                      {t('确认')}
                                    </Button>
                                  </span>
                                </div>
                              ) : null}
                            </div>
                            <Switch
                              checked={providerOptions[provider]?.[option.key] ?? option.default ?? false}
                              disabled={disabled}
                              onChange={(next) => toggleOption(provider, option, next)}
                              ariaLabel={optionWord(option, 'label', lang) || option.key}
                            />
                          </div>
                        );
                      })}
                    </div>
                  ))}
                </section>
              ) : null}

              <section className="pls-opt-section">
                <h4 className="pls-opt-heading">{t('高级参数（JSON）')}</h4>
                <textarea
                  className="form-input pls-opt-json"
                  rows={6}
                  spellCheck={false}
                  value={draft}
                  disabled={disabled}
                  onChange={(e) => {
                    setDraft(e.target.value);
                    setOptionsDirty(true);
                    setError('');
                  }}
                />
                <p className="pl-store-hint">
                  {t('修改高级参数会等待在途任务结束后重新加载插件；专属开关实时生效。')}
                </p>
              </section>

              {error ? (
                <div className="error-message" style={{ marginTop: 10 }} title={error}>
                  {error}
                </div>
              ) : null}
            </>
          )}
        </div>
        <div className="modal-footer">
          <Button variant="outline" onClick={onClose}>
            {t('取消')}
          </Button>
          <Button variant="primary" disabled={disabled || loading} onClick={() => void save()}>
            {saving ? <Loader2 size={13} className="spin" /> : null}
            {saving ? t('保存中…') : t('保存配置')}
          </Button>
        </div>
      </div>
    </div>
  );
}

/**
 * 脚本插件面板主体。
 *
 * 双数据源：`/api/plugins/scripts`（宿主 + 已装包）与
 * `/api/plugins/scripts/catalog`（目录三分类）。
 */
export default function ScriptPluginsPanel({
  query: pageQuery,
  status: pageStatus,
  installRequest = 0,
}: {
  /** 页面级搜索关键词（A2A plugins-page 把搜索透传给脚本面板） */
  query?: string;
  /** 页面级状态筛选（missing = 未安装） */
  status?: 'all' | 'updatable' | 'installed' | 'missing';
  /** 自增计数器：页头「从 npm / GitHub 安装」请求打开手动安装弹窗 */
  installRequest?: number;
} = {}): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();
  const mounted = useRef(true);
  /**
   * 写操作互斥（ref 而非 state）：防止同一渲染帧内连点两个按钮时，
   * 两个闭包都读到旧的 busy=false 而并发发起写请求。
   */
  const busyRef = useRef(false);
  /**
   * 回读序号：并发 load（首屏 + 写操作后回读 + 刷新目录轮询）可能乱序返回，
   * 只允许最新一次的结果落地，避免旧快照覆盖新状态（竞态）。
   */
  const loadSeq = useRef(0);
  /** 数据快照签名：轮询/回读返回同一份数据时跳过 setState，避免整表重渲染。 */
  const payloadSig = useRef<string | null>(null);
  const catalogSig = useRef<string | null>(null);

  const [payload, setPayload] = useState<ScriptPackagesPayload | null>(null);
  const [catalog, setCatalog] = useState<ScriptCatalogPayload | null>(null);
  const [loading, setLoading] = useState(true);
  const [notReady, setNotReady] = useState(false);
  const [error, setError] = useState('');
  const [busyKey, setBusyKey] = useState<string | null>(null);
  const [confirmState, setConfirmState] = useState<ConfirmState | null>(null);
  const [editingPkg, setEditingPkg] = useState<ScriptPackageView | null>(null);
  /** 手动安装弹窗（npm 包名 / GitHub 地址 / magpie: 前缀） */
  const [manualOpen, setManualOpen] = useState(false);
  const [manualSelector, setManualSelector] = useState('');
  const [manualError, setManualError] = useState('');
  const lastInstallRequest = useRef(installRequest);

  // 页头「从 npm / GitHub 安装」自增计数器 → 打开手动安装弹窗
  useEffect(() => {
    if (installRequest > lastInstallRequest.current) {
      lastInstallRequest.current = installRequest;
      setManualSelector('');
      setManualError('');
      setManualOpen(true);
    }
  }, [installRequest]);

  // 筛选与搜索
  const [query, setQuery] = useState('');
  const [sourceFilter, setSourceFilter] = useState<'all' | 'magpie' | 'npm' | 'github'>('all');
  const [statusFilter, setStatusFilter] = useState<'all' | 'installed' | 'notInstalled' | 'updatable'>('all');
  /** 已装列表「只看失败」：过滤 error 非空或未接管（providerRouted=false）的包 */
  const [onlyFailed, setOnlyFailed] = useState(false);

  // 页面级筛选联动：搜索框与状态筛选由插件页统一驱动（A2A 同口径）
  useEffect(() => {
    if (pageQuery !== undefined) setQuery(pageQuery);
  }, [pageQuery]);
  useEffect(() => {
    if (!pageStatus) return;
    setStatusFilter(pageStatus === 'missing' ? 'notInstalled' : pageStatus);
  }, [pageStatus]);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  const load = useCallback(async (): Promise<void> => {
    const seq = ++loadSeq.current;
    try {
      const results = await Promise.all([api.listScriptPlugins(), api.scriptCatalog()]);
      const listData = results[0]?.data ?? null;
      const catData = results[1]?.data ?? null;
      if (!mounted.current || seq !== loadSeq.current) return;
      const nextPayloadSig = listData ? JSON.stringify(listData) : '';
      const nextCatalogSig = catData ? JSON.stringify(catData) : '';
      if (nextPayloadSig !== payloadSig.current) {
        payloadSig.current = nextPayloadSig;
        setPayload(listData);
      }
      if (nextCatalogSig !== catalogSig.current) {
        catalogSig.current = nextCatalogSig;
        setCatalog(catData);
      }
      setNotReady(false);
      setError('');
    } catch (err) {
      if (!mounted.current || seq !== loadSeq.current) return;
      const msg = err instanceof Error ? err.message : String(err);
      const is404 = /404|not found/i.test(msg);
      setNotReady(is404);
      setError(is404 ? '' : formatError(msg));
      // 404 = 管理面未提供（降级空态）；其它错误保留上次成功数据，
      // 只挂错误条——瞬时网络抖动不该把已加载的面板整块清空。
      if (is404) {
        payloadSig.current = null;
        catalogSig.current = null;
        setPayload(null);
        setCatalog(null);
      }
    } finally {
      if (mounted.current && seq === loadSeq.current) setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const packages = useMemo(() => payload?.packages ?? [], [payload]);
  const host = payload?.host ?? null;

  /** 失败判定：有加载错误，或未接管（providerRouted=false）。「只看失败」据此过滤。 */
  const failedPackageCount = useMemo(
    () => packages.filter((pkg) => pkg.error || !pkg.providerRouted).length,
    [packages],
  );
  const visiblePackages = useMemo(
    () => (onlyFailed ? packages.filter((pkg) => pkg.error || !pkg.providerRouted) : packages),
    [packages, onlyFailed],
  );

  /** 已装包按名（不区分大小写）索引：目录条目「已安装」徽标 / 可更新判定用 */
  const installedByName = useMemo(() => {
    const map = new Map<string, ScriptPackageView>();
    for (const pkg of packages) map.set(pkg.id.toLowerCase(), pkg);
    return map;
  }, [packages]);

  // 目录条目筛选：来源 / 状态 / 搜索（label/name/summary/provider 子串匹配）
  const entries = useMemo(() => catalog?.entries ?? [], [catalog]);
  const filteredEntries = useMemo(() => {
    const q = query.trim().toLowerCase();
    return entries.filter((entry) => {
      if (sourceFilter !== 'all' && entry.source !== sourceFilter) return false;
      const installed = installedByName.get(entry.name.toLowerCase());
      if (statusFilter === 'installed' && !installed) return false;
      if (statusFilter === 'notInstalled' && installed) return false;
      if (statusFilter === 'updatable') {
        if (!installed || !entry.version || installed.version === entry.version) return false;
      }
      if (!q) return true;
      const haystack = `${entry.label} ${entry.name} ${entry.summary ?? ''} ${entry.providers.join(' ')}`.toLowerCase();
      return haystack.includes(q);
    });
  }, [entries, sourceFilter, statusFilter, query, installedByName]);

  // 三分类分组（目录侧已排好序：publisher rank → stars 降序 → label 升序）
  const groups = useMemo(
    () => [
      { title: t('官方'), entries: filteredEntries.filter((e) => e.magpieOfficial) },
      { title: t('推荐'), entries: filteredEntries.filter((e) => !e.magpieOfficial && e.magpieRecommended) },
      { title: t('第三方'), entries: filteredEntries.filter((e) => !e.magpieOfficial && !e.magpieRecommended) },
    ],
    [filteredEntries, t]
  );

  const busy = busyKey != null;

  /**
   * 通用写操作包装：busyRef 互斥 + busyKey 占位 + 无论成败回读 + toast。
   *
   * 失败后也回读：后端存在「已落盘但宿主重建失败」这类半成功
   * （如 options 保存后 rebuild 失败），UI 必须反映真实状态而不是停在
   * 点击前的旧快照。
   *
   * `toastError=false` 供确认弹窗内使用：错误改为抛出，由 ConfirmDialog
   * 就地展示并保持弹窗打开（用户可直接重试），不再叠一条转瞬即逝的 toast。
   */
  const runMutation = async (
    key: string,
    action: () => Promise<void>,
    opts: { toastError?: boolean } = {}
  ): Promise<{ ok: boolean; error?: string }> => {
    if (busyRef.current) return { ok: false, error: 'busy' };
    busyRef.current = true;
    setBusyKey(key);
    let ok = false;
    let error: string | undefined;
    try {
      await action();
      ok = true;
    } catch (err) {
      error = formatError(err);
      if (opts.toastError !== false) addToast(error, 'error');
    } finally {
      await load();
      busyRef.current = false;
      if (mounted.current) setBusyKey(null);
    }
    return { ok, error };
  };

  /** 目录卡「安装」：二次确认（含沙箱警告）后才传 trusted=true */
  const handleInstall = (entry: ScriptCatalogEntryView): void => {
    setConfirmState({
      title: t('安装脚本包'),
      message: t('脚本不是沙箱，将以 Bun 进程权限运行。确认安装「{{name}}」？', { name: entry.label || entry.name }),
      confirmText: t('确认安装'),
      danger: true,
      onConfirm: async () => {
        const res = await runMutation(
          `install:${entry.selector}`,
          async () => {
            await api.installScriptPackage(entry.selector, true);
            addToast(t('脚本包已安装'));
          },
          { toastError: false }
        );
        // 抛出让 ConfirmDialog 就地显示后端错误并保持打开，用户可修正/重试
        if (!res.ok && res.error && res.error !== 'busy') throw new Error(res.error);
      },
    });
  };

  const handleToggleEnabled = (pkg: ScriptPackageView, enabled: boolean): void => {
    void runMutation(`enable:${pkg.id}@${pkg.version}`, async () => {
      // 必须带 version：多版本共存时后端缺省选「最后登记」的版本，
      // 不传就会把另一张卡片的版本切了。
      await api.enableScriptPackage(pkg.id, enabled, pkg.version);
      addToast(enabled ? t('脚本包已启用') : t('脚本包已停用'));
    });
  };

  /**
   * 包内单家 provider 启停（`/plugins/scripts/change` action=provider）。
   *
   * 与内置包的 `/plugins/provider` 分开：脚本包的开关落 `disabled_providers`，
   * 启用中的包会触发宿主重建。错误文案由 runMutation 统一 toast（后端会说
   * 「提供商不属于该脚本插件」这类原因）；成功后 runMutation 的 finally 里
   * 已 `load()` 回读，开关状态以服务端为准。
   */
  const handleProviderEnabled = (pkg: ScriptPackageView, provider: string, enabled: boolean): void => {
    void runMutation(`provider:${pkg.id}@${pkg.version}:${provider}`, async () => {
      await api.setScriptProviderEnabled(pkg.id, provider, enabled, pkg.version);
      addToast(enabled ? t('该提供商已启用') : t('该提供商已停用'));
    });
  };

  const handleUninstall = (pkg: ScriptPackageView): void => {
    const name = pkg.label || pkg.id;
    // Local 来源是「本机目录原地登记」，卸载不删运行者的源码目录；
    // 确认文案必须按来源区分，否则会误导用户以为文件被删。
    const isLocal = pkg.source === 'local';
    setConfirmState({
      title: t('卸载脚本包'),
      message: isLocal
        ? t('确定卸载「{{name}}」吗？该包是「本机目录」来源，仅移除插件登记、配置与元数据，本机文件不会被删除。', { name })
        : t('确定卸载「{{name}}」吗？该脚本包声明的提供商将停止接管请求，插件包目录、包配置与元数据都会被删除。', { name }),
      confirmText: t('卸载'),
      danger: true,
      onConfirm: async () => {
        const res = await runMutation(
          `remove:${pkg.id}@${pkg.version}`,
          async () => {
            // 带 version：不带时后端会删掉同 id 的全部版本，而 UI 只展示单张卡片
            await api.uninstallScriptPackage(pkg.id, pkg.version);
            addToast(t('脚本包已卸载'));
          },
          { toastError: false }
        );
        if (!res.ok && res.error && res.error !== 'busy') throw new Error(res.error);
      },
    });
  };

  /**
   * 手动安装（npm 包名 / GitHub 地址 / `magpie:` 前缀）：与目录卡安装同一条
   * 安全闸门——先弹含「脚本不是沙箱」的确认，确认后传 `trusted=true`。
   */
  const handleManualInstall = (raw: string): void => {
    const selector = raw.trim();
    if (!selector) {
      setManualError(t('请填写安装来源'));
      return;
    }
    setManualOpen(false);
    setConfirmState({
      title: t('安装脚本包'),
      message: t('脚本不是沙箱，将以 Bun 进程权限运行。确认安装「{{name}}」？', { name: selector }),
      confirmText: t('确认安装'),
      danger: true,
      onConfirm: async () => {
        const res = await runMutation(
          `install:${selector}`,
          async () => {
            await api.installScriptPackage(selector, true);
            addToast(t('脚本包已安装'));
          },
          { toastError: false }
        );
        if (!res.ok && res.error && res.error !== 'busy') throw new Error(res.error);
      },
    });
  };

  /**
   * 刷新目录：POST 只唤醒后台任务立即返回，结果落在 catalog.status 上。
   * 这里轮询几次状态并如实汇报——不能无条件 toast「已刷新」（上游失败或
   * 重试闸门拦截时目录根本没变）。
   */
  const handleRefreshCatalog = async (): Promise<void> => {
    if (busyRef.current) return;
    busyRef.current = true;
    setBusyKey('refresh');
    try {
      const prev = catalog?.status;
      await api.refreshScriptCatalog();
      let latest: ScriptCatalogPayload | null = null;
      for (let i = 0; i < 4; i++) {
        await new Promise((resolve) => window.setTimeout(resolve, 2000));
        if (!mounted.current) return;
        const res = await api.scriptCatalog();
        latest = res?.data ?? null;
        if (latest && catalogAdvanced(prev, latest.status)) break;
      }
      const refreshError = latest?.status.market_error || latest?.status.github_error;
      if (refreshError) {
        addToast(t('目录刷新失败：{{err}}', { err: formatError(refreshError) }), 'error');
      } else if (!latest || latest.status.market_stale || latest.status.github_stale) {
        addToast(t('目录刷新尚未完成（上游受限或网络较慢），请稍后刷新查看。'), 'warning');
      } else {
        addToast(t('目录已刷新'));
      }
    } catch (err) {
      addToast(formatError(err), 'error');
    } finally {
      await load();
      busyRef.current = false;
      if (mounted.current) setBusyKey(null);
    }
  };

  const handleRebuildHost = (): void => {
    void runMutation('rebuild', async () => {
      await api.rebuildScriptHost();
      addToast(t('宿主已重启'));
    });
  };

  return (
    <div>
      {notReady ? (
        <EmptyState
          icon="🧩"
          message={t('脚本插件后端尚未就绪')}
          action={<p className="pl-empty-hint">{t('当前后端未提供脚本插件管理面（端点 404），请升级 AIGX 后端后重试。')}</p>}
        />
      ) : error && !payload ? (
        <div className="error-message">
          {error}
          <Button className="pls-error-retry" variant="outline" size="sm" onClick={() => void load()}>
            {t('重试')}
          </Button>
        </div>
      ) : (
        <>
          {/* 局部失败：保留上次成功数据，只挂错误条 + 重试 */}
          {error ? (
            <div className="error-message">
              {error}
              <Button className="pls-error-retry" variant="outline" size="sm" onClick={() => void load()}>
                {t('重试')}
              </Button>
            </div>
          ) : null}

          {/* 宿主状态条 */}
          <Card
            title={t('宿主状态')}
            actions={
              <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap' }}>
                <Button variant="outline" size="sm" onClick={() => void handleRefreshCatalog()} disabled={busy}>
                  {busyKey === 'refresh' ? <Loader2 size={13} className="spin" /> : <RefreshCw size={13} />}
                  {busyKey === 'refresh' ? t('刷新目录中…') : t('刷新目录')}
                </Button>
                <Button variant="outline" size="sm" onClick={handleRebuildHost} disabled={busy}>
                  {busyKey === 'rebuild' ? <Loader2 size={13} className="spin" /> : <RotateCcw size={13} />}
                  {busyKey === 'rebuild' ? t('重启宿主中…') : t('重启宿主')}
                </Button>
              </div>
            }
          >
            {payload ? (
              <>
                <div className="pls-host-bar">
                  <Badge tone={host?.running ? 'success' : 'neutral'}>{host?.running ? t('宿主运行中') : t('宿主未运行')}</Badge>
                  <span>{t('已装载 {{count}} 个包', { count: host?.packages ?? 0 })}</span>
                  <span>{t('接管 {{count}} 家提供商', { count: host?.providers.length ?? 0 })}</span>
                  {host && host.crashes > 0 ? <Badge tone="warning">{t('崩溃 {{count}} 次', { count: host.crashes })}</Badge> : null}
                  {host?.lastError ? <span className="pl-dg-err" title={host.lastError}>{t('最近错误：{{err}}', { err: formatError(host.lastError) })}</span> : null}
                </div>
                {catalog?.status ? (
                  <div className="pls-host-bar pls-cat-bar">
                    <span>{t('目录：官方 {{a}} / 推荐 {{b}} / 第三方 {{c}}', {
                      a: catalog.status.official,
                      b: catalog.status.recommended,
                      c: catalog.status.third_party,
                    })}</span>
                    <span>{t('市场清单更新于 {{time}}', { time: fmtUnix(catalog.status.market_at) })}</span>
                    {catalog.status.market_error ? <span className="pl-dg-err" title={catalog.status.market_error}>{formatError(catalog.status.market_error)}</span> : null}
                    {catalog.status.github_error ? <span className="pl-dg-err" title={catalog.status.github_error}>{formatError(catalog.status.github_error)}</span> : null}
                  </div>
                ) : null}
              </>
            ) : (
              // 首次加载 / 读取失败：不能显示成「宿主未运行 0 个包」——那是两种完全不同的状态
              <p className="pl-empty-hint">
                {loading ? t('正在读取宿主状态…') : t('宿主状态读取失败')}
              </p>
            )}
          </Card>

          {/* 已安装脚本包 */}
          <div style={{ marginTop: 14 }}>
            <Card
              title={`${t('已安装脚本包')} (${onlyFailed ? `${visiblePackages.length}/${packages.length}` : packages.length})`}
              actions={
                <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap', alignItems: 'center' }}>
                  <label
                    className="pl-switch-label"
                    style={{ display: 'inline-flex', alignItems: 'center', gap: 6, fontSize: 12 }}
                  >
                    <Switch checked={onlyFailed} onChange={setOnlyFailed} ariaLabel={t('只看失败')} />
                    <span>
                      {t('只看失败')}
                      {failedPackageCount > 0 ? ` (${failedPackageCount})` : ''}
                    </span>
                  </label>
                  {/* 宿主状态徽章（与上方「宿主状态」卡同源；失败时警示色） */}
                  {host ? (
                    <div style={{ display: 'flex', gap: 6, flexWrap: 'wrap', alignItems: 'center' }}>
                      <Badge tone={host.running ? 'success' : 'warning'}>
                        {host.running ? t('宿主运行中') : t('宿主未运行')}
                      </Badge>
                      {host.crashes > 0 ? (
                        <Badge tone="danger">{t('崩溃 {{count}} 次', { count: host.crashes })}</Badge>
                      ) : null}
                      {host.lastError ? (
                        <span className="pl-dg-err" title={host.lastError}>
                          {t('最近错误：{{err}}', { err: formatError(host.lastError) })}
                        </span>
                      ) : null}
                    </div>
                  ) : null}
                </div>
              }
            >
              {loading && !payload ? (
                <Loading text={t('正在读取脚本插件…')} />
              ) : packages.length === 0 ? (
                <EmptyState
                  icon="🧩"
                  message={t('暂无已安装脚本包')}
                  action={<p className="pl-empty-hint">{t('从下方目录选择脚本包安装后，在这里启停、配置与卸载。')}</p>}
                />
              ) : visiblePackages.length === 0 ? (
                <p className="pl-empty-hint">{t('没有失败或未接管的脚本包')}</p>
              ) : (
                <div className="pl-grid">
                  {visiblePackages.map((pkg) => (
                    <InstalledScriptCard
                      key={`${pkg.id}@${pkg.version}`}
                      pkg={pkg}
                      busy={busy}
                      onToggleEnabled={handleToggleEnabled}
                      onProviderEnabled={handleProviderEnabled}
                      onUninstall={handleUninstall}
                      onEditOptions={(p) => setEditingPkg(p)}
                    />
                  ))}
                </div>
              )}
            </Card>
          </div>

          {/* Magpie 目录（三分类） */}
          <div style={{ marginTop: 14 }}>
            <Card title={`${t('脚本插件目录')} (${filteredEntries.length})`}>
              <div className="pls-toolbar">
                <div className="pls-search">
                  <Search size={13} />
                  <input
                    className="form-input"
                    placeholder={t('搜索脚本插件…')}
                    value={query}
                    onChange={(e) => setQuery(e.target.value)}
                  />
                </div>
                <select
                  className="form-input"
                  value={sourceFilter}
                  onChange={(e) => setSourceFilter(e.target.value as 'all' | 'magpie' | 'npm' | 'github')}
                  aria-label={t('来源筛选')}
                >
                  <option value="all">{t('全部来源')}</option>
                  <option value="magpie">Magpie</option>
                  <option value="npm">npm</option>
                  <option value="github">GitHub</option>
                </select>
                <select
                  className="form-input"
                  value={statusFilter}
                  onChange={(e) => setStatusFilter(e.target.value as 'all' | 'installed' | 'notInstalled' | 'updatable')}
                  aria-label={t('状态筛选')}
                >
                  <option value="all">{t('全部状态')}</option>
                  <option value="installed">{t('已安装')}</option>
                  <option value="notInstalled">{t('未安装')}</option>
                  <option value="updatable">{t('可更新')}</option>
                </select>
              </div>

              {loading && !catalog ? (
                <Loading text={t('正在读取脚本插件…')} />
              ) : entries.length === 0 ? (
                <EmptyState
                  icon="🧩"
                  message={t('暂无目录条目')}
                  action={<p className="pl-empty-hint">{t('目录尚未拉取，点「刷新目录」试试；也可能是网络/上游不可用。')}</p>}
                />
              ) : filteredEntries.length === 0 ? (
                <p className="pl-empty-hint">{t('没有符合筛选条件的条目。')}</p>
              ) : (
                groups.map((group) => (
                  <div className="pls-group" key={group.title}>
                    <div className="pls-group-title">
                      <h4>{group.title}</h4>
                      <span className="pls-group-count">{group.entries.length}</span>
                    </div>
                    {group.entries.length === 0 ? (
                      <p className="pl-empty-hint">{t('暂无条目')}</p>
                    ) : (
                      <div className="pl-grid">
                        {group.entries.map((entry) => (
                          <CatalogCard
                            key={entry.name}
                            entry={entry}
                            installed={installedByName.get(entry.name.toLowerCase())}
                            installing={busyKey === `install:${entry.selector}`}
                            busy={busy}
                            onInstall={handleInstall}
                          />
                        ))}
                      </div>
                    )}
                  </div>
                ))
              )}
            </Card>
          </div>
        </>
      )}

      <ConfirmDialog state={confirmState} onClose={() => setConfirmState(null)} />
      {manualOpen ? (
        <div className="modal-overlay" role="dialog" aria-modal="true" aria-label={t('从 npm / GitHub 安装')} onClick={() => setManualOpen(false)}>
          <div className="modal" style={{ maxWidth: 520, width: '92%' }} onClick={(e) => e.stopPropagation()}>
            <div className="modal-header">
              <h3>{t('从 npm / GitHub 安装')}</h3>
              <button className="modal-close" onClick={() => setManualOpen(false)} aria-label={t('关闭')}>
                &times;
              </button>
            </div>
            <div className="modal-body">
              <label className="pls-opt-field">
                <span>{t('安装来源')}</span>
                <input
                  className="form-input"
                  autoComplete="off"
                  placeholder="@scope/plugin / https://github.com/owner/repo / magpie:包名"
                  value={manualSelector}
                  onChange={(e) => {
                    setManualSelector(e.target.value);
                    setManualError('');
                  }}
                  onKeyDown={(e) => {
                    if (e.key === 'Enter') handleManualInstall(manualSelector);
                  }}
                />
              </label>
              <p className="pl-store-hint">
                {t('仅安装你信任的 npm 包、GitHub 仓库或 Magpie 分发包；完整性校验不等于安全审计。')}
              </p>
              <div className="pl-missing-warn">
                <p>{t('脚本不是安全沙箱，可以访问网关所在电脑的文件和网络。不要安装来历不明的插件。')}</p>
              </div>
              {manualError ? <div className="error-message">{manualError}</div> : null}
            </div>
            <div className="modal-footer">
              <Button variant="outline" onClick={() => setManualOpen(false)}>
                {t('取消')}
              </Button>
              <Button variant="primary" disabled={busy} onClick={() => handleManualInstall(manualSelector)}>
                {t('继续安装')}
              </Button>
            </div>
          </div>
        </div>
      ) : null}
      {editingPkg ? (
        <OptionsDialog
          pkg={editingPkg}
          busy={busy}
          onClose={() => setEditingPkg(null)}
          onSaved={() => load()}
        />
      ) : null}
    </div>
  );
}
