// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//
// Plugins — 插件管理页（管理员专属）。
// 信息结构对照 A2A ui-islands plugins-page.tsx / plugins-pack-settings.tsx：
//   内置目录 / 外置包分区（providerCategory = agent / web / wasm）、下载进度、
//   安装 / 卸载 / 启停、每 provider 开关、provider 设置（settings_schema）、
//   包图标、宿主状态（running / crashes / lastError）、更新检查。
// 注意：交互与布局按 AIGX 自研组件重写，不整段拷 A2A 的 islands TSX。

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import {
  ArrowDownToLine, Download, Loader2, RefreshCw, RotateCcw, Settings2, Trash2,
} from 'lucide-react';
import { api } from '../api';
import ScriptPluginsPanel from '../components/plugins/ScriptPluginsPanel';
import ProviderSettingsDialog from '../components/plugins/ProviderSettingsDialog';
import { useToast } from '../components/Toast';
import ConfirmDialog, { type ConfirmState } from '../components/ConfirmDialog';
import { Badge, Button, Card, EmptyState, Loading, Switch, Tabs, Tooltip } from '../components/ui';
import type {
  PluginOperation,
  PluginPackSummary,
  PluginProviderSettingsRow,
  PluginsPayload,
  StorePlugin,
} from '../types';
import './Plugins.css';

/** 插件页分组：原生插件包 / 脚本插件（Magpie） */
type PluginTab = 'native' | 'scripts';

/** 外置包的 provider 形态分区（A2A providerCategory 三态 + 内置目录） */
type PackSection = 'builtin' | 'agent' | 'web' | 'wasm';

const SECTION_ORDER: PackSection[] = ['builtin', 'agent', 'web', 'wasm'];

/** 安装时间（unix 毫秒/秒归一）→ 人话 */
function fmtInstalledAt(value: unknown): string {
  const n = Number(value);
  if (!Number.isFinite(n) || n <= 0) return '—';
  const ms = n < 1e11 ? n * 1000 : n;
  return new Date(ms).toLocaleDateString();
}

/** 字节数 → KB/MB */
function fmtSize(bytes: number): string {
  if (!bytes) return '';
  if (bytes >= 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  if (bytes >= 1024) return `${Math.round(bytes / 1024)} KB`;
  return `${bytes} B`;
}

/** 操作阶段 → 中文（对齐上游 OperationProgress 的阶段文案） */
function stageLabel(t: (s: string) => string, stage: string): string {
  switch (stage) {
    case 'downloading': return t('正在下载…');
    case 'verifying': return t('正在校验包内容…');
    case 'waiting': return t('等待当前任务结束…');
    case 'loading': return t('正在加载新插件…');
    case 'restoring': return t('正在恢复旧版本插件…');
    case 'installing': return t('正在安装…');
    default: return stage;
  }
}

/** 包归属分区：内置目录（id=builtin）单列；外置按 providerCategory */
function sectionOf(pack: PluginPackSummary): PackSection {
  if (pack.id === 'builtin') return 'builtin';
  if (pack.providerCategory === 'wasm') return 'wasm';
  if (pack.providerCategory === 'web') return 'web';
  return 'agent';
}

/** 在途操作的进度条（下载中显示百分比，其余阶段显示不定进度） */
function OperationProgress({ op }: { op: PluginOperation }): JSX.Element {
  const { t } = useTranslation();
  const download = op.stage === 'downloading';
  return (
    <div className="pl-op-progress" aria-live="polite">
      <div className="pl-op-line">
        <span className="pl-op-stage">{stageLabel(t, op.stage)}</span>
        <span className="pl-op-bytes">
          {download
            ? op.total > 0
              ? `${fmtSize(op.received)} / ${fmtSize(op.total)} · ${Math.round(op.percent || 0)}%`
              : fmtSize(op.received)
            : t('请稍候')}
        </span>
      </div>
      <div className={`pl-op-bar${download ? ' determinate' : ''}`} role="progressbar">
        {download && <span style={{ width: `${Math.max(2, Math.round(op.percent || 0))}%` }} />}
      </div>
    </div>
  );
}

/** 已安装包的卡片（启停/卸载/单家 provider 启停/包级设置/更新） */
function InstalledPackCard({
  pack,
  operation,
  busy,
  canUpdate,
  latestVersion,
  onEnabled,
  onReload,
  onRemove,
  onProviderEnabled,
  onOpenSettings,
  onUpdate,
}: {
  pack: PluginPackSummary;
  operation?: PluginOperation;
  busy: boolean;
  canUpdate: boolean;
  latestVersion?: string | null;
  onEnabled: (enabled: boolean) => void;
  onReload: () => void;
  onRemove: () => void;
  onProviderEnabled: (provider: string, enabled: boolean) => void;
  onOpenSettings: () => void;
  onUpdate: () => void;
}): JSX.Element {
  const { t } = useTranslation();
  const [providersOpen, setProvidersOpen] = useState(false);
  const loadedCount = pack.providers.filter((p) => p.loaded).length;
  const crashes = pack.host?.crashes ?? 0;
  const isBuiltin = pack.id === 'builtin';
  return (
    <div className="pl-card is-installed">
      <div className="pl-card-head">
        <div className="pl-card-icon">
          {pack.icon ? <img src={pack.icon} alt="" /> : (pack.name || pack.id).slice(0, 1).toUpperCase()}
        </div>
        <div className="pl-card-title">
          <h3>
            {pack.name || pack.id}
            {pack.version ? <span className="pl-ver">v{pack.version}</span> : null}
          </h3>
          <div className="pl-card-badges">
            <Badge tone={pack.enabled ? 'info' : 'neutral'}>
              {pack.enabled ? t('已启用') : t('已停用')}
            </Badge>
            {/* AIGX 不做签名验签（对齐 M2 安装链路口径）：两种形态都不声称「可信」 */}
            {!isBuiltin && (
              <Badge tone="warning">
                {pack.signed ? t('带签名（未校验）') : t('未校验签名')}
              </Badge>
            )}
            {pack.version.includes('-') ? <Badge tone="warning">{t('beta 版')}</Badge> : null}
            {pack.updateRequired ? <Badge tone="danger">{t('需要补齐插件')}</Badge> : null}
            {!pack.entryPresent ? <Badge tone="danger">{t('文件缺失')}</Badge> : null}
            {canUpdate && latestVersion ? (
              <Badge tone="info">{t('可更新到 v{{v}}', { v: latestVersion })}</Badge>
            ) : null}
          </div>
        </div>
      </div>
      {pack.description ? <p className="pl-card-desc">{pack.description}</p> : null}
      <p className="pl-card-meta">
        {isBuiltin
          ? t('编译进网关的内置目录 · {{count}} 家提供商', { count: pack.providers.length })
          : t('{{count}} 家提供商 · 安装于 {{time}}', { count: pack.providers.length, time: fmtInstalledAt(pack.installedAt) })}
      </p>
      <div className="pl-card-state">
        {isBuiltin ? (
          <span>{t('内置实现')}</span>
        ) : (
          <span>{pack.host?.running ? t('宿主运行中') : t('宿主未运行')}</span>
        )}
        <span>{t('已加载 {{count}} 家', { count: loadedCount })}</span>
        {crashes > 0 && <span className="pl-crash-count">{t('崩溃 {{n}} 次', { n: crashes })}</span>}
      </div>
      {pack.compatibilityError ? (
        <p className="pl-card-err" title={pack.compatibilityError}>{pack.compatibilityError}</p>
      ) : null}
      {!pack.entryPresent ? (
        <p className="pl-card-err">{t('插件包的可执行文件不在磁盘上（可能被手工删除或移动），请重新安装。')}</p>
      ) : null}
      {pack.host?.lastError ? (
        <p className="pl-card-err" title={pack.host.lastError}>{pack.host.lastError}</p>
      ) : null}
      {operation?.error ? <p className="pl-card-err" title={operation.error}>{operation.error}</p> : null}
      {operation?.active ? (
        <OperationProgress op={operation} />
      ) : (
        <div className="pl-card-actions">
          {!isBuiltin && (
            <label className="pl-switch-label">
              {t('启用')}
              <Switch
                checked={pack.enabled}
                disabled={busy}
                onChange={(v) => onEnabled(v)}
                ariaLabel={`${t('启用')} ${pack.name}`}
              />
            </label>
          )}
          {pack.providers.length > 0 && (
            <Button variant="outline" size="sm" onClick={() => setProvidersOpen((v) => !v)}>
              {providersOpen ? t('收起提供商') : t('提供商（{{n}}）', { n: pack.providers.length })}
            </Button>
          )}
          {!isBuiltin && (
            <Tooltip label={t('插件声明的可编辑设置（settings_schema）')}>
              <Button variant="outline" size="sm" disabled={busy} onClick={onOpenSettings}>
                <Settings2 size={13} /> {t('设置')}
              </Button>
            </Tooltip>
          )}
          {!isBuiltin && (
            <Tooltip label={t('重启本包宿主进程')}>
              <Button variant="outline" size="sm" disabled={busy} onClick={onReload}>
                <RotateCcw size={13} /> {t('重载')}
              </Button>
            </Tooltip>
          )}
          {canUpdate && (
            <Button variant="primary" size="sm" disabled={busy} onClick={onUpdate}>
              <ArrowDownToLine size={13} /> {t('更新')}
            </Button>
          )}
          {!isBuiltin && (
            <Tooltip label={t('卸载后账号数据保留，重新安装可继续使用')}>
              <Button variant="danger" size="sm" disabled={busy} onClick={onRemove}>
                <Trash2 size={13} /> {t('卸载')}
              </Button>
            </Tooltip>
          )}
        </div>
      )}
      {providersOpen && (
        <ul className="pl-providers">
          {pack.providers.map((p) => (
            <li key={p.id} className="pl-provider-row">
              <span className="pl-provider-name" title={p.id}>{p.label || p.id}</span>
              <span className={`pl-provider-dot${p.loaded ? ' on' : ''}`} title={p.loaded ? t('已加载') : t('未加载')} />
              {p.overriddenBy ? (
                <Badge tone="neutral">{t('由其他插件接管')}</Badge>
              ) : (p.toggle ?? 'global') === 'none' ? (
                // 原生插件包没有 provider 级开关（PackMeta 无该字段，后端会 400）：
                // 不渲染 Switch，改为「随包启停」说明，避免点了才报错。
                <Badge tone="neutral">{t('随包启停')}</Badge>
              ) : (
                <Switch
                  checked={p.enabled}
                  disabled={busy}
                  onChange={(v) => onProviderEnabled(p.id, v)}
                  ariaLabel={`${t('启用')} ${p.label}`}
                />
              )}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

/** 商店（发布目录）条目的卡片：从当前平台包直接下载安装 */
function StorePackCard({
  item,
  target,
  catalogAvailable,
  catalogReason,
  busy,
  operation,
  onInstall,
}: {
  item: StorePlugin;
  target: string;
  catalogAvailable: boolean;
  catalogReason?: string | null;
  busy: boolean;
  operation?: PluginOperation;
  onInstall: (item: StorePlugin) => void;
}): JSX.Element {
  const { t } = useTranslation();
  const platform = item.platforms.find((p) => p.target === target);
  // 目录不可用 = 无分发渠道/拉取失败，下载无从谈起（原因 title 提示）
  const installable = catalogAvailable && !!platform;
  return (
    <div className="pl-card is-store">
      <div className="pl-card-head">
        <div className="pl-card-icon">
          {item.icon ? <img src={item.icon} alt="" /> : (item.name || item.id).slice(0, 1).toUpperCase()}
        </div>
        <div className="pl-card-title">
          <h3>
            {item.name || item.id}
            {item.version ? <span className="pl-ver">v{item.version}</span> : null}
          </h3>
          <div className="pl-card-badges">
            {item.official ? <Badge tone="info">{t('官方')}</Badge> : null}
            {item.prerelease ? <Badge tone="warning">{t('beta 版')}</Badge> : null}
            {!item.version ? <Badge tone="neutral">{t('暂未发布')}</Badge> : null}
          </div>
        </div>
      </div>
      {item.description ? <p className="pl-card-desc">{item.description}</p> : null}
      <div className="pl-card-state">
        <span>{target || '—'}</span>
        <span>{platform ? fmtSize(platform.size) : t('当前平台暂无包')}</span>
      </div>
      {operation?.active ? (
        <OperationProgress op={operation} />
      ) : operation?.error ? (
        <p className="pl-card-err" title={operation.error}>{operation.error}</p>
      ) : (
        <div className="pl-card-actions">
          <Button
            variant="primary"
            size="sm"
            disabled={!installable || busy}
            title={!catalogAvailable && catalogReason ? catalogReason : undefined}
            onClick={() => onInstall(item)}
          >
            <Download size={13} /> {t('下载')}
          </Button>
          {!installable && (
            <span
              className="pl-store-hint"
              title={catalogAvailable ? t('当前平台暂无包') : (catalogReason || t('目录暂不可用'))}
            >
              {platform ? (catalogReason || t('目录暂不可用')) : t('当前平台暂无包')}
            </span>
          )}
        </div>
      )}
    </div>
  );
}

/**
 * Plugins — 插件管理页（管理员专属）。
 *
 * 契约对齐 Agent2API api/plugins.rs（list/enable/provider/uninstall/update/
 * release/progress + provider-settings/reload）。
 * **安装与更新走 `/api/plugins/install`（url 直装）**：AIGX 没有 A2A 的
 * `install-from-release` 后台任务端点（`/api/plugins/update` 也只有 GET 的
 * 更新检查），所以不存在「先起任务、失败回落」的两段路径。
 * AIGX 后端插件管理面在原契约上继续补齐（T1/T2 任务）；端点 404 时整页降级为
 * 「后端未就绪」空态说明，不报错不崩；后端就绪后零改动生效。
 */
export default function Plugins(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();

  const [payload, setPayload] = useState<PluginsPayload | null>(null);
  const [loading, setLoading] = useState(true);
  /** 404 → 后端未就绪降级态（区别于普通错误：不给重试以外的引导） */
  const [notReady, setNotReady] = useState(false);
  const [error, setError] = useState('');
  const [busyId, setBusyId] = useState<string | null>(null);
  const [confirmState, setConfirmState] = useState<ConfirmState | null>(null);
  /** 插件页分组（原生插件包 / 脚本插件（Magpie）） */
  const [tab, setTab] = useState<PluginTab>('native');
  /** provider 设置弹层：null = 关闭；否则给该包声明的 provider 清单 */
  const [settingsProviders, setSettingsProviders] = useState<string[] | null>(null);
  /** provider-settings 端点是否有内容（未就绪时「设置」按钮也给替换提示） */
  const [settingsRows, setSettingsRows] = useState<PluginProviderSettingsRow[] | null>(null);
  const mounted = useRef(true);

  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; };
  }, []);

  const load = useCallback(async (): Promise<void> => {
    try {
      const res = await api.listPlugins();
      if (!mounted.current) return;
      setPayload(res?.data ?? null);
      setNotReady(false);
      setError('');
    } catch (err) {
      if (!mounted.current) return;
      const msg = err instanceof Error ? err.message : String(err);
      // 404 = 插件管理端点未上线（后端建设中）；其余按普通错误显示
      const is404 = /404|not found/i.test(msg);
      setNotReady(is404);
      setError(is404 ? '' : msg);
      setPayload(null);
    } finally {
      if (mounted.current) setLoading(false);
    }
  }, []);

  /** provider-settings 清单（有内容才给「设置」按钮详见；404 时不阻塞主流程） */
  const loadProviderSettings = useCallback(async (): Promise<void> => {
    try {
      const res = await api.getPluginProviderSettings();
      if (!mounted.current) return;
      setSettingsRows(res?.data?.settings ?? []);
    } catch {
      if (mounted.current) setSettingsRows(null);
    }
  }, []);

  useEffect(() => {
    void load();
    void loadProviderSettings();
  }, [load, loadProviderSettings]);

  // 有在途操作时 2 秒轮询进度（对齐上游操作期间的轮询；完成后回读完整列表）
  const hasActiveOps = (payload?.operations ?? []).some((op) => op.active);
  useEffect(() => {
    if (!hasActiveOps) return undefined;
    const timer = window.setInterval(() => void load(), 2000);
    return () => window.clearInterval(timer);
  }, [hasActiveOps, load]);

  const packs = useMemo(() => payload?.packs ?? [], [payload]);
  const host = payload?.host ?? null;
  // 外置包（排除内置目录）——只看它们的宿主状态
  const externalPacks = packs.filter((p) => p.id !== 'builtin');
  const anyHostRunning = externalPacks.some((p) => p.host?.running);
  const catalog = payload?.catalog;
  const installedIds = useMemo(() => new Set(packs.map((p) => p.id)), [packs]);
  const storePlugins = useMemo(() => {
    const store = catalog?.plugins ?? [];
    return store.filter((item) => !installedIds.has(item.id));
  }, [catalog, installedIds]);
  const operations = payload?.operations ?? [];
  const updates = useMemo(() => payload?.updates ?? [], [payload]);
  const busy = busyId != null;

  /** 每个包可更新的最新版本（updates[] 里 available=true 的那条） */
  const updateOf = useCallback(
    (id: string) => updates.find((u) => u.id === id && u.available === true),
    [updates],
  );

  /** 按分区聚合已装包（保持后端顺序） */
  const groupedPacks = useMemo(() => {
    const map = new Map<PackSection, PluginPackSummary[]>();
    for (const pack of packs) {
      const section = sectionOf(pack);
      const arr = map.get(section) ?? [];
      arr.push(pack);
      map.set(section, arr);
    }
    return map;
  }, [packs]);

  /** 包级写操作：跑完回读（启用/禁用/卸载/重载共享同一条路径） */
  const runMutation = async (key: string, action: () => Promise<void>): Promise<void> => {
    if (busy) return;
    setBusyId(key);
    try {
      await action();
      await load();
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    } finally {
      if (mounted.current) setBusyId(null);
    }
  };

  const handleEnabled = (pack: PluginPackSummary, enabled: boolean): void => {
    void runMutation(`enable:${pack.id}`, async () => {
      await api.setPluginEnabled(pack.id, enabled);
      addToast(enabled ? t('插件已启用') : t('插件已停用'));
    });
  };

  const handleProviderEnabled = (pack: PluginPackSummary, provider: string, enabled: boolean): void => {
    void runMutation(`provider:${provider}`, async () => {
      await api.setPluginProviderEnabled(pack.id, provider, enabled);
      addToast(enabled ? t('提供商已启用') : t('提供商已停用'));
    });
  };

  const handleReload = (pack: PluginPackSummary): void => {
    void runMutation(`reload:${pack.id}`, async () => {
      await api.reloadPlugin(pack.id);
      addToast(t('插件已重载'));
    });
  };

  const handleUninstall = (pack: PluginPackSummary): void => {
    setConfirmState({
      title: t('卸载插件包'),
      message: t('确定卸载「{{name}}」吗？卸载后这个包里的提供商将无法转发，也无法添加账号。账号数据本身不会被删除，重新安装后可以继续使用。', { name: pack.name || pack.id }),
      confirmText: t('卸载'),
      danger: true,
      onConfirm: async () => {
        await runMutation(`remove:${pack.id}`, async () => {
          await api.uninstallPlugin(pack.id);
          addToast(t('插件已卸载'));
        });
      },
    });
  };

  /**
   * 商店卡「下载」：目录声明的 url 直装。
   *
   * AIGX 后端没有 `install-from-release` 这类后台任务端点（`/api/plugins/install`
   * 才是安装入口，直接带 url），所以这里不再试任务路径——先前那层 catch 回落
   * 只是把恒 404 的调用兜住，功能可用但代码是死的。
   */
  const handleInstall = (item: StorePlugin): void => {
    const target = payload?.target || '';
    void runMutation(`install:${item.id}`, async () => {
      const platform = item.platforms.find((p) => p.target === target);
      if (!platform) {
        throw new Error(t('该插件不提供当前平台（{{target}}）的安装包', { target }));
      }
      // 目录声明了 sha256 就传给后端复核（挡传输损坏 / 上游资产被换）
      await api.installPlugin(platform.url, platform.sha256 || undefined);
      addToast(t('插件已下载'));
    });
  };

  /**
   * 包「更新」：同样走 url 直装（后端 `/api/plugins/update` 只有 GET 的更新检查）。
   *
   * 保留原语义：**没有目录条目就不更新**（目录里查不到 = 不知道该装哪个版本）。
   */
  const handleUpdate = (pack: PluginPackSummary, item?: StorePlugin): void => {
    const target = payload?.target || '';
    void runMutation(`update:${pack.id}`, async () => {
      if (!item) throw new Error(t('该插件不在发布目录中，无法更新'));
      const platform = item.platforms.find((p) => p.target === target);
      if (!platform) {
        throw new Error(t('该插件不提供当前平台（{{target}}）的安装包', { target }));
      }
      await api.installPlugin(platform.url, platform.sha256 || undefined);
      addToast(t('插件已更新'));
    });
  };

  const handleCheckUpdates = async (): Promise<void> => {
    if (busy) return;
    setBusyId('check');
    try {
      const res = await api.checkPluginUpdates();
      const data = res?.data as { error?: boolean; reason?: string } | undefined;
      if (data?.error) {
        addToast(String(data.reason || t('检查更新失败')), 'error');
      } else {
        addToast(t('已检查更新'));
      }
      await load();
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    } finally {
      setBusyId(null);
    }
  };

  const handleRefreshStore = async (): Promise<void> => {
    if (busy) return;
    setBusyId('refresh');
    try {
      await api.refreshPluginRelease();
      await load();
      addToast(t('已刷新插件目录'));
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    } finally {
      setBusyId(null);
    }
  };

  /** 点「设置」：把该包声明的 provider 交给弹层（弹层内按 provider 过滤） */
  const openPackSettings = (pack: PluginPackSummary): void => {
    if (!settingsRows || settingsRows.length === 0) {
      addToast(t('后端插件设置接口尚未就绪，或该包未声明可编辑设置'), 'error');
      return;
    }
    setSettingsProviders(pack.providers.map((p) => p.id));
  };

  const sectionTitle = (section: PackSection): { title: string; hint: string } => {
    switch (section) {
      case 'builtin':
        return { title: t('内置目录'), hint: t('编译进网关的提供商目录（无需安装）') };
      case 'web':
        return { title: t('网页提供商插件'), hint: t('网页形态的 provider（登录态 / 网页会话）') };
      case 'wasm':
        return { title: t('Wasm 插件'), hint: t('沙箱执行的 Wasm 包') };
      default:
        return { title: t('Agent 提供商插件'), hint: t('客户端登录态型的 provider') };
    }
  };

  return (
    <div>
      <div className="page-header">
        <div>
          <h1>{t('插件管理')}</h1>
          <p>{t('按需安装插件扩展提供商能力；启停、状态与来源统一管理。')}</p>
        </div>
        <div style={{ display: 'flex', gap: 8 }}>
          <Button variant="outline" onClick={() => void load()} disabled={loading}>
            {loading ? <Loader2 size={13} className="pl-spin" /> : <RefreshCw size={13} />}
            {t('刷新')}
          </Button>
        </div>
      </div>

      {/* 原生插件 / 脚本插件（Magpie）两个分组 */}
      <Tabs<PluginTab>
        className="pl-tabs"
        ariaLabel={t('插件管理')}
        items={[
          { key: 'native', label: t('原生插件') },
          { key: 'scripts', label: t('脚本插件（Magpie）') },
        ]}
        active={tab}
        onChange={(k) => setTab(k)}
      />

      {tab === 'scripts' ? (
        <ScriptPluginsPanel />
      ) : (
        <div hidden={tab !== 'native'}>
          {error && <div className="error-message">{error}</div>}

          {loading ? (
            <Loading text={t('读取插件列表')} />
          ) : notReady ? (
            <EmptyState
              icon="🧩"
              message={t('插件管理后端尚未就绪')}
              action={<p className="pl-empty-hint">{t('AIGX 的插件机制正在建设中（契约对齐 Agent2API：安装 / 启停 / 目录 / 进度 / provider 设置）。后端上线本页即自动可用；当前版本的提供商全部内置于网关，无需插件即可使用。')}</p>}
            />
          ) : (
            <>
              {/* 顶部状态条：宿主状态 + 目录操作 */}
              <div className="pl-toolbar">
                <div className="pl-toolbar-state">
                  <Badge tone={anyHostRunning ? 'success' : 'neutral'}>
                    {anyHostRunning ? t('宿主运行中') : externalPacks.length ? t('宿主未运行') : t('内置实现')}
                  </Badge>
                  <span className="pl-toolbar-meta">
                    {t('{{packs}} 个已装包 · {{providers}} 家插件提供商', {
                      packs: externalPacks.length,
                      providers: externalPacks.reduce((n, p) => n + p.providers.length, 0),
                    })}
                  </span>
                  {host?.lastError ? <span className="pl-toolbar-err" title={host.lastError}>{host.lastError}</span> : null}
                </div>
                <div className="pl-toolbar-actions">
                  <Button variant="outline" size="sm" onClick={() => void handleRefreshStore()} disabled={busy}>
                    {busyId === 'refresh' ? <Loader2 size={13} className="pl-spin" /> : null}
                    {t('刷新目录')}
                  </Button>
                  <Button variant="outline" size="sm" onClick={() => void handleCheckUpdates()} disabled={busy}>
                    {busyId === 'check' || catalog?.checking ? t('检查中…') : t('检查更新')}
                  </Button>
                  <Button variant="outline" size="sm" onClick={() => setSettingsProviders(settingsRows?.map((r) => r.provider) ?? [])} disabled={busy}>
                    <Settings2 size={13} /> {t('全部插件设置')}
                  </Button>
                </div>
              </div>

              {/* 分区渲染：内置目录 / Agent / Web / Wasm（空分区不渲染；商店条目归外置区的「可下载」组） */}
              {SECTION_ORDER.map((section) => {
                const sectionPacks = groupedPacks.get(section) ?? [];
                const isStoreSection = section !== 'builtin';
                // 商店条目归到 Agent 分区（A2A 的「Agent2API 社区」口径）；Web 空分区不动
                const sectionStore = isStoreSection && section === 'agent' ? storePlugins : [];
                if (sectionPacks.length === 0 && sectionStore.length === 0) return null;
                const meta = sectionTitle(section);
                return (
                  <Card
                    key={section}
                    title={`${meta.title} (${sectionPacks.length + sectionStore.length})`}
                    actions={
                      <Tooltip label={meta.hint}>
                        <span className="pl-section-hint">{meta.hint}</span>
                      </Tooltip>
                    }
                  >
                    <div className="pl-grid">
                      {sectionPacks.map((pack) => {
                        const op = operations.find((o) => o.id === pack.id);
                        const update = updateOf(pack.id);
                        const storeItem = catalog?.plugins?.find((i) => i.id === pack.id);
                        return (
                          <InstalledPackCard
                            key={pack.id}
                            pack={pack}
                            operation={op}
                            busy={busy || op?.active === true}
                            canUpdate={!!update}
                            latestVersion={update?.latest}
                            onEnabled={(v) => handleEnabled(pack, v)}
                            onReload={() => handleReload(pack)}
                            onRemove={() => handleUninstall(pack)}
                            onProviderEnabled={(provider, v) => handleProviderEnabled(pack, provider, v)}
                            onOpenSettings={() => openPackSettings(pack)}
                            onUpdate={() => handleUpdate(pack, storeItem)}
                          />
                        );
                      })}
                      {sectionStore.map((item) => (
                        <StorePackCard
                          key={item.id}
                          item={item}
                          target={payload?.target || ''}
                          catalogAvailable={catalog?.available !== false}
                          catalogReason={catalog?.reason}
                          busy={busy}
                          operation={operations.find((op) => op.id === item.id)}
                          onInstall={handleInstall}
                        />
                      ))}
                    </div>
                  </Card>
                );
              })}

              {packs.length === 0 && storePlugins.length === 0 && (
                <Card>
                  <EmptyState
                    icon="🧩"
                    message={t('暂无已安装的插件')}
                    action={<p className="pl-empty-hint">{t('AIGX 当前所有提供商均内置于网关（注册表静态表达），无需插件即可使用。插件包用于扩展后续的第三方提供商。')}</p>}
                  />
                </Card>
              )}

              {/* 诊断信息（对齐上游：目录 / 平台 / 协议版本折叠区） */}
              {payload && (
                <details className="pl-diagnostics">
                  <summary>{t('诊断信息')}</summary>
                  <div>
                    <span>{t('插件目录')} <code>{payload.pluginsDir || '—'}</code></span>
                    <span>{t('当前平台')} <code>{payload.target || '—'}</code></span>
                    <span>{t('协议版本')} {payload.protocol || '—'}</span>
                    {catalog?.reason ? <span className="pl-dg-err">{catalog.reason}</span> : null}
                    {host?.lastError ? <span className="pl-dg-err">{host.lastError}</span> : null}
                  </div>
                </details>
              )}
            </>
          )}

          <ConfirmDialog state={confirmState} onClose={() => setConfirmState(null)} />
        </div>
      )}

      <ProviderSettingsDialog
        open={settingsProviders !== null}
        providers={settingsProviders ?? undefined}
        onClose={() => setSettingsProviders(null)}
        onSaved={() => void loadProviderSettings()}
      />
    </div>
  );
}