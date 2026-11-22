// 插件管理页（原生插件包 + Magpie 脚本插件）。
//
// 形态对齐 Agent2API v3.5.0-beta.2 的 plugins-page.tsx / plugins-toolbar.tsx /
// plugins-page-tools.tsx / plugins-cards.tsx：
// - 双源结构：`Agent2API 社区`（原生插件包 + 商店条目）/ `Magpie 插件`（脚本插件）；
// - 来源筛选（全部 / Agent2API 社区 / Magpie 插件）+ 状态筛选（全部 / 可更新 /
//   已安装 / 未安装）+ 搜索 + 清除筛选，计数随筛选实时变化；
// - 工具栏：检查更新 / 刷新 / 插件设置 / 本地安装 / 从 npm/GitHub 安装 /
//   补全缺失插件 / 诊断信息；
// - 缺失插件横幅 + 弹窗（数据源：`hiddenAccounts` + 文件缺失的已装包）；
// - 批量生命周期（安装/更新/启停/卸载，逐项进度走 /api/plugins/progress）。
//
// AIGX 原生 Rust 提供商（后端内置目录包 id=`builtin`）**不是** A2A 的插件源，
// 本页不渲染它（仍保留在 GET /api/plugins 供渠道表单等既有消费方使用）。

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import {
  ArrowDownToLine, Download, Loader2, MoreHorizontal, RotateCcw, Search, Settings2, Trash2,
} from 'lucide-react';
import { api } from '../../api';
import ScriptPluginsPanel from './ScriptPluginsPanel';
import ProviderSettingsDialog from './ProviderSettingsDialog';
import FileInstallDialog from './FileInstallDialog';
import PluginIcon from './PluginIcon';
import { useToast } from '../Toast';
import ConfirmDialog, { type ConfirmState } from '../ConfirmDialog';
import { Badge, Button, Card, EmptyState, Loading, Switch, Tooltip } from '../ui';
import type {
  PluginOperation,
  PluginPackSummary,
  PluginProviderSettingsRow,
  PluginsPayload,
  ScriptCatalogEntryView,
  ScriptHostView,
  ScriptPackageView,
  StorePlugin,
} from '../../types';
import './PluginManagerPage.css';

type SourceFilter = 'all' | 'agent2' | 'magpie';
type StatusFilter = 'all' | 'updatable' | 'installed' | 'missing';

interface FilterItem {
  installed: boolean;
  updatable: boolean;
}

function matchesStatus(item: FilterItem, status: StatusFilter): boolean {
  if (status === 'all') return true;
  if (status === 'updatable') return item.installed && item.updatable;
  return item.installed === (status === 'installed');
}

function fmtInstalledAt(value: unknown): string {
  const n = Number(value);
  if (!Number.isFinite(n) || n <= 0) return '—';
  const ms = n < 1e11 ? n * 1000 : n;
  return new Date(ms).toLocaleDateString();
}

function fmtSize(bytes: number): string {
  if (!bytes) return '';
  if (bytes >= 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  if (bytes >= 1024) return `${Math.round(bytes / 1024)} KB`;
  return `${bytes} B`;
}

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

interface MoreAction {
  key: string;
  label: string;
  onSelect: () => void;
  disabled?: boolean;
}

/**
 * 页头「更多操作」三点菜单（对齐 A2A PluginPageTools 的 Popover）。
 *
 * 用原生按钮实现（AIGX ui 无 Popover 组件）：点击外部 / 按 Esc 关闭，
 * 每个条目沿用页面既有 handler，disabled 态由调用方按现有 busy 逻辑传入。
 * 菜单为绝对定位且右对齐，窄屏下可点击、不撑破页头。
 */
function MoreMenu({ label, actions }: { label: string; actions: MoreAction[] }): JSX.Element {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return undefined;
    const onDocDown = (event: MouseEvent): void => {
      if (ref.current && !ref.current.contains(event.target as Node)) setOpen(false);
    };
    const onKey = (event: KeyboardEvent): void => {
      if (event.key === 'Escape') setOpen(false);
    };
    document.addEventListener('mousedown', onDocDown);
    document.addEventListener('keydown', onKey);
    return () => {
      document.removeEventListener('mousedown', onDocDown);
      document.removeEventListener('keydown', onKey);
    };
  }, [open]);
  return (
    <div className="pl-more" ref={ref}>
      <button
        type="button"
        className="pl-more-btn"
        aria-label={label}
        title={label}
        aria-haspopup="menu"
        aria-expanded={open}
        onClick={() => setOpen((value) => !value)}
      >
        <MoreHorizontal size={18} />
      </button>
      {open ? (
        <div className="pl-more-menu" role="menu" aria-label={label}>
          {actions.map((action) => (
            <button
              key={action.key}
              type="button"
              role="menuitem"
              className="pl-more-item"
              disabled={action.disabled}
              onClick={() => {
                setOpen(false);
                action.onSelect();
              }}
            >
              {action.label}
            </button>
          ))}
        </div>
      ) : null}
    </div>
  );
}

/** 已安装原生插件卡（A2A InstalledPluginCard 的 AIGX 形态）。 */
function InstalledPackCard({
  pack, operation, busy, canUpdate, latestVersion, hasUpdateNotes, selectable, checked,
  onToggleSelect, onEnabled, onReload, onRemove, onProviderEnabled, onOpenSettings, onUpdate, onNotes,
}: {
  pack: PluginPackSummary;
  operation?: PluginOperation;
  busy: boolean;
  canUpdate: boolean;
  latestVersion?: string | null;
  hasUpdateNotes?: boolean;
  selectable?: boolean;
  checked?: boolean;
  onToggleSelect?: () => void;
  onEnabled: (enabled: boolean) => void;
  onReload: () => void;
  onRemove: () => void;
  onProviderEnabled: (provider: string, enabled: boolean) => void;
  onOpenSettings: () => void;
  onUpdate: () => void;
  onNotes: () => void;
}): JSX.Element {
  const { t } = useTranslation();
  const [providersOpen, setProvidersOpen] = useState(false);
  const loadedCount = pack.providers.filter((p) => p.loaded).length;
  const crashes = pack.host?.crashes ?? 0;
  return (
    <div className="pl-card is-installed">
      <div className="pl-card-head">
        {selectable ? (
          <input
            type="checkbox"
            className="pl-batch-check"
            checked={checked}
            aria-label={t('选择 {{name}}', { name: pack.name || pack.id })}
            onChange={() => onToggleSelect?.()}
          />
        ) : null}
        <PluginIcon src={pack.icon} label={pack.name || pack.id} />
        <div className="pl-card-title">
          <h3>
            {pack.name || pack.id}
            {pack.version ? <span className="pl-ver">v{pack.version}</span> : null}
          </h3>
          <div className="pl-card-badges">
            <Badge tone={pack.enabled ? 'info' : 'neutral'}>{pack.enabled ? t('已启用') : t('已停用')}</Badge>
            {pack.signed ? null : <Badge tone="warning">{t('未校验签名')}</Badge>}
            {pack.version.includes('-') ? <Badge tone="warning">{t('beta 版')}</Badge> : null}
            {!pack.entryPresent ? <Badge tone="danger">{t('文件缺失')}</Badge> : null}
            {canUpdate && latestVersion ? (
              <Badge tone="info">{t('可更新到 v{{v}}', { v: latestVersion })}</Badge>
            ) : null}
          </div>
        </div>
      </div>
      {pack.description ? <p className="pl-card-desc">{pack.description}</p> : null}
      <p className="pl-card-meta">
        {t('{{count}} 家提供商 · 安装于 {{time}}', {
          count: pack.providers.length,
          time: fmtInstalledAt(pack.installedAt),
        })}
      </p>
      <div className="pl-card-state">
        <span>{pack.host?.running ? t('宿主运行中') : t('宿主未运行')}</span>
        <span>{t('已加载 {{count}} 家', { count: loadedCount })}</span>
        {crashes > 0 && <span className="pl-crash-count">{t('崩溃 {{n}} 次', { n: crashes })}</span>}
      </div>
      {pack.compatibilityError ? (
        <p className="pl-card-err" title={pack.compatibilityError}>{pack.compatibilityError}</p>
      ) : null}
      {!pack.entryPresent ? (
        <p className="pl-card-err">{t('插件包的可执行文件不在磁盘上（可能被手工删除或移动），请重新安装。')}</p>
      ) : null}
      {pack.host?.lastError ? <p className="pl-card-err" title={pack.host.lastError}>{pack.host.lastError}</p> : null}
      {operation?.error ? <p className="pl-card-err" title={operation.error}>{operation.error}</p> : null}
      {operation?.hostError ? <p className="pl-card-err" title={operation.hostError}>{operation.hostError}</p> : null}
      {operation?.active ? (
        <OperationProgress op={operation} />
      ) : (
        <div className="pl-card-actions">
          <label className="pl-switch-label">
            {t('启用')}
            <Switch
              checked={pack.enabled}
              disabled={busy}
              onChange={(v) => onEnabled(v)}
              ariaLabel={`${t('启用')} ${pack.name}`}
            />
          </label>
          {pack.providers.length > 0 && (
            <Button variant="outline" size="sm" onClick={() => setProvidersOpen((v) => !v)}>
              {providersOpen ? t('收起提供商') : t('提供商（{{n}}）', { n: pack.providers.length })}
            </Button>
          )}
          <Tooltip label={t('插件声明的可编辑设置（settings_schema）')}>
            <Button variant="outline" size="sm" disabled={busy} onClick={onOpenSettings}>
              <Settings2 size={13} /> {t('设置')}
            </Button>
          </Tooltip>
          <Tooltip label={t('重启本包宿主进程')}>
            <Button variant="outline" size="sm" disabled={busy} onClick={onReload}>
              <RotateCcw size={13} /> {t('重载')}
            </Button>
          </Tooltip>
          {canUpdate && (
            <Button variant="primary" size="sm" disabled={busy} onClick={onUpdate}>
              <ArrowDownToLine size={13} /> {t('更新')}
            </Button>
          )}
          {hasUpdateNotes ? (
            <Button variant="outline" size="sm" disabled={busy} onClick={onNotes}>
              {t('查看更新日志')}
            </Button>
          ) : null}
          <Tooltip label={t('卸载后账号数据保留，重新安装可继续使用')}>
            <Button variant="danger" size="sm" disabled={busy} onClick={onRemove}>
              <Trash2 size={13} /> {t('卸载')}
            </Button>
          </Tooltip>
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

/** 商店条目卡（A2A StorePluginCard 的 AIGX 形态）。 */
function StorePackCard({
  item, target, catalogAvailable, catalogReason, busy, operation, selectable, checked,
  onToggleSelect, onInstall, onNotes,
}: {
  item: StorePlugin;
  target: string;
  catalogAvailable: boolean;
  catalogReason?: string | null;
  busy: boolean;
  operation?: PluginOperation;
  selectable?: boolean;
  checked?: boolean;
  onToggleSelect?: () => void;
  onInstall: (item: StorePlugin) => void;
  onNotes: () => void;
}): JSX.Element {
  const { t } = useTranslation();
  const platform = item.platforms.find((p) => p.target === target);
  const installable = catalogAvailable && !!platform && !!item.version;
  // Wasm 沙箱包：条目分类或标签声明（后端 index.json 原样透传）
  const isWasm = item.category === 'wasm' || (item.tags ?? []).some((tag) => tag.toLowerCase() === 'wasm');
  const hasNotes = Boolean(item.notes || item.page || item.homepage);
  return (
    <div className="pl-card is-store">
      <div className="pl-card-head">
        {selectable ? (
          <input
            type="checkbox"
            className="pl-batch-check"
            checked={checked}
            disabled={!installable}
            aria-label={t('选择 {{name}}', { name: item.name || item.id })}
            onChange={() => onToggleSelect?.()}
          />
        ) : null}
        <PluginIcon src={item.icon} label={item.name || item.id} />
        <div className="pl-card-title">
          <h3>
            {item.name || item.id}
            {item.version ? <span className="pl-ver">v{item.version}</span> : null}
          </h3>
          <div className="pl-card-badges">
            {item.official ? <Badge tone="info">{t('官方')}</Badge> : null}
            {isWasm ? <Badge tone="neutral">{t('Wasm')}</Badge> : null}
            {item.requireSigned ? <Badge tone="warning">{t('需签名')}</Badge> : null}
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
          {hasNotes ? (
            <Button variant="outline" size="sm" disabled={busy} onClick={onNotes}>
              {t('更新日志')}
            </Button>
          ) : null}
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

export default function PluginManagerPage(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();

  const [payload, setPayload] = useState<PluginsPayload | null>(null);
  const [loading, setLoading] = useState(true);
  const [notReady, setNotReady] = useState(false);
  const [error, setError] = useState('');
  const [busyId, setBusyId] = useState<string | null>(null);
  const [confirmState, setConfirmState] = useState<ConfirmState | null>(null);
  const [source, setSource] = useState<SourceFilter>('all');
  const [status, setStatus] = useState<StatusFilter>('all');
  const [search, setSearch] = useState('');
  const [batchMode, setBatchMode] = useState(false);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [batchRunning, setBatchRunning] = useState(false);
  const [batchOps, setBatchOps] = useState<PluginOperation[]>([]);
  const [settingsProviders, setSettingsProviders] = useState<string[] | null>(null);
  const [settingsRows, setSettingsRows] = useState<PluginProviderSettingsRow[] | null>(null);
  const [fileOpen, setFileOpen] = useState(false);
  const [missingOpen, setMissingOpen] = useState(false);
  const [diagnosticsOpen, setDiagnosticsOpen] = useState(false);
  /** 更新日志 / 版本详情弹窗（目录条目与已装包更新共用） */
  const [notes, setNotes] = useState<{
    name: string;
    version?: string | null;
    notes?: string | null;
    page?: string | null;
    homepage?: string | null;
  } | null>(null);
  const [scriptPackages, setScriptPackages] = useState<ScriptPackageView[]>([]);
  const [scriptEntries, setScriptEntries] = useState<ScriptCatalogEntryView[]>([]);
  const [scriptHost, setScriptHost] = useState<ScriptHostView | null>(null);
  const [scriptsAvailable, setScriptsAvailable] = useState(false);
  /** 自增计数器：页头「从 npm / GitHub 安装」触发脚本面板的手动安装弹窗 */
  const [scriptInstallRequest, setScriptInstallRequest] = useState(0);
  const mounted = useRef(true);
  /** 上次成功落地列表的快照签名：轮询返回同一份数据时跳过 setState，
   *  避免在途操作期间每 2s 把整张卡片表重渲染一遍（图标是 data URI 时最贵）。 */
  const payloadSig = useRef<string | null>(null);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  const load = useCallback(async (): Promise<void> => {
    try {
      const res = await api.listPlugins();
      if (!mounted.current) return;
      const data = res?.data ?? null;
      const sig = data ? JSON.stringify(data) : '';
      if (sig !== payloadSig.current) {
        payloadSig.current = sig;
        setPayload(data);
      }
      setNotReady(false);
      setError('');
    } catch (err) {
      if (!mounted.current) return;
      const msg = err instanceof Error ? err.message : String(err);
      const is404 = /404|not found/i.test(msg);
      setNotReady(is404);
      setError(is404 ? '' : msg);
      payloadSig.current = null;
      setPayload(null);
    } finally {
      if (mounted.current) setLoading(false);
    }
  }, []);

  /** Magpie 统计（来源/状态筛选的计数与缺失映射用；后端未就绪时静默降级）。 */
  const loadScripts = useCallback(async (): Promise<void> => {
    try {
      const [listRes, catRes] = await Promise.all([api.listScriptPlugins(), api.scriptCatalog()]);
      if (!mounted.current) return;
      setScriptPackages(listRes?.data?.packages ?? []);
      setScriptEntries(catRes?.data?.entries ?? []);
      setScriptHost(listRes?.data?.host ?? null);
      setScriptsAvailable(true);
    } catch {
      if (!mounted.current) return;
      setScriptPackages([]);
      setScriptEntries([]);
      setScriptHost(null);
      setScriptsAvailable(false);
    }
  }, []);

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
    void loadScripts();
    void loadProviderSettings();
  }, [load, loadScripts, loadProviderSettings]);

  const hasActiveOps = (payload?.operations ?? []).some((op) => op.active);
  useEffect(() => {
    if (!hasActiveOps) return undefined;
    const timer = window.setInterval(() => {
      void load();
      void loadScripts();
    }, 2000);
    return () => window.clearInterval(timer);
  }, [hasActiveOps, load, loadScripts]);

  const packs = useMemo(() => (payload?.packs ?? []).filter((p) => p.id !== 'builtin'), [payload]);
  const catalog = payload?.catalog;
  const store = useMemo(() => catalog?.plugins ?? [], [catalog]);
  const installedIds = useMemo(() => new Set(packs.map((p) => p.id)), [packs]);
  const storePlugins = useMemo(() => store.filter((item) => !installedIds.has(item.id)), [store, installedIds]);
  const operations = payload?.operations ?? [];
  const updates = useMemo(() => payload?.updates ?? [], [payload]);
  const target = payload?.target || '';
  // 顶层 host 恒为 null（列表接口没有「全局宿主」概念），宿主错误挂在每个
  // 包的 host.lastError 上——诊断弹窗汇总这里，避免永远显示「—」。
  const packHostErrors = packs
    .filter((p) => p.host?.lastError)
    .map((p) => `${p.id}: ${p.host?.lastError}`);
  const busy = busyId != null;

  const updateOf = useCallback(
    (id: string) => updates.find((u) => u.id === id && u.available === true),
    [updates],
  );

  const installedScriptIds = useMemo(() => new Set(scriptPackages.map((p) => p.id)), [scriptPackages]);
  const scriptEntriesNotInstalled = useMemo(
    () => scriptEntries.filter((entry) => !installedScriptIds.has(entry.name)),
    [scriptEntries, installedScriptIds],
  );

  const keyword = search.trim().toLocaleLowerCase();
  const searchedPacks = useMemo(
    () =>
      packs.filter((pack) => {
        if (!keyword) return true;
        const haystack = [
          pack.id,
          pack.name,
          pack.description ?? '',
          ...pack.providers.map((provider) => `${provider.id} ${provider.label}`),
        ]
          .join(' ')
          .toLocaleLowerCase();
        return haystack.includes(keyword);
      }),
    [packs, keyword],
  );
  const searchedStore = useMemo(
    () =>
      storePlugins.filter((item) =>
        !keyword
          ? true
          : `${item.id} ${item.name} ${item.description}`.toLocaleLowerCase().includes(keyword),
      ),
    [storePlugins, keyword],
  );
  const searchedScripts = useMemo(
    () =>
      scriptPackages.filter((pack) =>
        !keyword ? true : `${pack.id} ${pack.label} ${pack.version}`.toLocaleLowerCase().includes(keyword),
      ),
    [scriptPackages, keyword],
  );
  const searchedEntries = useMemo(
    () =>
      scriptEntriesNotInstalled.filter((entry) =>
        !keyword
          ? true
          : `${entry.label} ${entry.name} ${entry.summary ?? ''}`.toLocaleLowerCase().includes(keyword),
      ),
    [scriptEntriesNotInstalled, keyword],
  );

  // 计数：搜索后、按另一维筛选（A2A `countMatching` 口径）
  const nativeItems: FilterItem[] = [
    ...searchedPacks.map((pack) => ({ installed: true, updatable: !!updateOf(pack.id) })),
    ...searchedStore.map(() => ({ installed: false, updatable: false })),
  ];
  const magpieItems: FilterItem[] = [
    ...searchedScripts.map(() => ({ installed: true, updatable: false })),
    ...searchedEntries.map(() => ({ installed: false, updatable: false })),
  ];
  const itemsFor = (value: SourceFilter): FilterItem[] =>
    value === 'agent2' ? nativeItems : value === 'magpie' ? magpieItems : [...nativeItems, ...magpieItems];
  const countMatching = (value: SourceFilter, next: StatusFilter): number =>
    itemsFor(value).filter((item) => matchesStatus(item, next)).length;

  const installedRows = searchedPacks.filter((pack) =>
    matchesStatus({ installed: true, updatable: !!updateOf(pack.id) }, status),
  );
  const storeRows = searchedStore.filter(() => matchesStatus({ installed: false, updatable: false }, status));
  const showAgent2 = source !== 'magpie' && (installedRows.length > 0 || storeRows.length > 0);
  const showMagpie = scriptsAvailable && source !== 'agent2';

  const hasAny =
    packs.length + storePlugins.length + scriptPackages.length + scriptEntriesNotInstalled.length > 0;
  const filtersActive = source !== 'all' || status !== 'all' || keyword.length > 0;

  // ── 缺失插件（A2A `missingPluginItems` 的 AIGX 投影）─────────────────
  const hiddenProviders = (payload?.hiddenAccounts?.providers ?? []).filter(
    (item) => item.reason === 'not_installed' || item.reason === 'legacy_unsupported',
  );
  const storeByProvider = useMemo(() => {
    const map = new Map<string, StorePlugin>();
    for (const item of storePlugins) {
      for (const provider of item.providers ?? []) {
        if (provider?.id) map.set(provider.id, item);
      }
    }
    return map;
  }, [storePlugins]);
  const installableOf = (item: StorePlugin): boolean =>
    Boolean(item.version && !item.error && item.platforms.some((p) => p.target === target));
  interface MissingRow {
    key: string;
    storeId: string;
    name: string;
    version?: string | null;
    accounts: number;
    installable: boolean;
    reason?: string | null;
  }
  const requiredByStore = new Map<string, MissingRow>();
  for (const provider of hiddenProviders) {
    const item = storeByProvider.get(provider.id);
    if (!item) continue;
    const row = requiredByStore.get(item.id) ?? {
      key: `pack:${item.id}`,
      storeId: item.id,
      name: item.name || item.id,
      version: item.version,
      accounts: 0,
      installable: installableOf(item),
      reason: item.reason,
    };
    row.accounts += provider.count;
    requiredByStore.set(item.id, row);
  }
  for (const pack of packs.filter((p) => p.enabled && p.entryPresent === false)) {
    const item = store.find((i) => i.id === pack.id);
    if (item && !requiredByStore.has(item.id)) {
      requiredByStore.set(item.id, {
        key: `pack:${item.id}`,
        storeId: item.id,
        name: item.name || item.id,
        version: item.version,
        accounts: 0,
        installable: installableOf(item),
        reason: item.reason,
      });
    }
  }
  const requiredRows = [...requiredByStore.values()];
  const unresolved = hiddenProviders.filter((provider) => !storeByProvider.has(provider.id));
  const optionalStores = storePlugins.filter(
    (item) => !requiredByStore.has(item.id) && installableOf(item),
  );
  const missingCount = requiredRows.length + unresolved.length;
  const missingAccounts =
    requiredRows.reduce((total, row) => total + row.accounts, 0) +
    unresolved.reduce((total, row) => total + row.count, 0);

  // ── 选择与批量 ─────────────────────────────────────────────────────
  const toggleSelect = (key: string): void => {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  };
  const exitBatch = (): void => {
    setBatchMode(false);
    setSelected(new Set());
    setBatchOps([]);
  };
  const selectedPacks = packs.filter((p) => selected.has(`pack:${p.id}`));
  const selectedStores = storePlugins.filter((item) => selected.has(`store:${item.id}`));
  const batchCount = selectedPacks.length + selectedStores.length;
  const anyPackEnabled = selectedPacks.some((p) => p.enabled);
  const anyPackDisabled = selectedPacks.some((p) => !p.enabled);
  const anyUpdatable = selectedPacks.some((p) => !!updateOf(p.id));
  const selectableKeys = [
    ...installedRows.map((p) => `pack:${p.id}`),
    ...storeRows
      .filter((s) => catalog?.available !== false && s.platforms.some((p) => p.target === target))
      .map((s) => `store:${s.id}`),
  ];
  const allSelected = selectableKeys.length > 0 && selectableKeys.every((key) => selected.has(key));
  const toggleAll = (): void => {
    setSelected(allSelected ? new Set() : new Set(selectableKeys));
  };

  const pollBatchUntilDone = async (watched: PluginOperation[]): Promise<void> => {
    const ids = new Set(watched.map((op) => op.id));
    let latest = watched;
    for (let i = 0; i < 300; i += 1) {
      const res = await api.getPluginProgress();
      const ops = res?.data?.operations ?? [];
      latest = ops.filter((op) => ids.has(op.id));
      setBatchOps(latest);
      if (latest.length > 0 && latest.every((op) => !op.active)) break;
      await new Promise((resolve) => setTimeout(resolve, 1500));
    }
    setBatchRunning(false);
    setBusyId(null);
    await load();
    await loadScripts();
    const failed = latest.filter((op) => op.stage === 'failed' || op.error || op.hostError);
    if (failed.length) {
      addToast(
        t('批量操作完成，{{n}} 项失败：{{detail}}', {
          n: failed.length,
          detail: failed
            .slice(0, 3)
            .map((op) => `${op.id}: ${op.error || op.hostError || op.stage}`)
            .join('；'),
        }),
        'error',
      );
    } else {
      addToast(t('批量操作完成'));
      setSelected(new Set());
    }
  };

  const runBatch = async (
    action: 'install' | 'update' | 'enable' | 'disable' | 'uninstall',
    overrideIds?: string[],
  ): Promise<void> => {
    if (batchRunning || busy) return;
    let body: Parameters<typeof api.batchPlugins>[0];
    if (action === 'install') {
      const items = selectedStores
        .map((item) => {
          const platform = item.platforms.find((p) => p.target === target);
          return platform
            ? { id: item.id, action: 'install', url: platform.url, sha256: platform.sha256 || undefined }
            : null;
        })
        .filter((row): row is { id: string; action: string; url: string; sha256: string | undefined } => row !== null);
      if (!items.length) {
        addToast(t('所选项没有可执行该操作的插件'), 'error');
        return;
      }
      body = { items };
    } else {
      let ids: string[];
      if (action === 'enable') ids = selectedPacks.filter((p) => !p.enabled).map((p) => p.id);
      else if (action === 'disable') ids = selectedPacks.filter((p) => p.enabled).map((p) => p.id);
      else if (action === 'update') ids = selectedPacks.filter((p) => !!updateOf(p.id)).map((p) => p.id);
      else ids = selectedPacks.map((p) => p.id);
      if (overrideIds) ids = overrideIds;
      if (!ids.length) {
        addToast(t('所选项没有可执行该操作的插件'), 'error');
        return;
      }
      body = { action, ids };
    }
    setBatchRunning(true);
    setBusyId(`batch:${action}`);
    setBatchOps([]);
    try {
      const res = await api.batchPlugins(body);
      const started = res?.data?.operations ?? [];
      setBatchOps(started);
      await pollBatchUntilDone(started);
    } catch (err) {
      setBatchRunning(false);
      setBusyId(null);
      addToast(err instanceof Error ? err.message : String(err), 'error');
    }
  };

  /** 一键更新全部可更新包（对齐 A2A「全部更新」；进度走每张卡片的 Operation）。 */
  const runUpdateAll = (): void => {
    const ids = packs.filter((p) => !!updateOf(p.id)).map((p) => p.id);
    if (!ids.length) {
      addToast(t('当前没有可更新的插件'), 'error');
      return;
    }
    addToast(t('已触发 {{n}} 个插件的更新', { n: ids.length }));
    void runBatch('update', ids);
  };

  const confirmBatchUninstall = (): void => {
    setConfirmState({
      title: t('批量卸载插件包'),
      message: t(
        '确定卸载所选的 {{n}} 个插件包吗？卸载后这些包里的提供商将无法转发，也无法添加账号。账号数据本身不会被删除，重新安装后可以继续使用。',
        { n: selectedPacks.length },
      ),
      confirmText: t('卸载'),
      danger: true,
      onConfirm: async () => {
        await runBatch('uninstall');
      },
    });
  };

  // ── 单包写操作 ─────────────────────────────────────────────────────
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
      message: t(
        '确定卸载「{{name}}」吗？卸载后这个包里的提供商将无法转发，也无法添加账号。账号数据本身不会被删除，重新安装后可以继续使用。',
        { name: pack.name || pack.id },
      ),
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
  const handleInstall = (item: StorePlugin): void => {
    void runMutation(`install:${item.id}`, async () => {
      await api.runPluginInstallFromRelease({ id: item.id, action: 'install' });
      addToast(t('插件已下载'));
    });
  };
  const handleUpdate = (pack: PluginPackSummary): void => {
    void runMutation(`update:${pack.id}`, async () => {
      await api.runPluginInstallFromRelease({ id: pack.id, action: 'update' });
      addToast(t('插件已更新'));
    });
  };
  const installMissing = (storeId: string): void => {
    void runMutation(`missing:${storeId}`, async () => {
      await api.runPluginInstallFromRelease({ id: storeId, action: 'install' });
      addToast(t('已开始安装'));
    });
  };

  const handleCheckUpdates = async (): Promise<void> => {
    if (busy) return;
    setBusyId('check');
    try {
      // 原生包与 Magpie 脚本包各有检查链路，一起问，别只查一半。
      const [res] = await Promise.all([
        api.checkPluginUpdates(),
        scriptsAvailable ? api.checkScriptUpdates().catch(() => undefined) : Promise.resolve(undefined),
      ]);
      const data = res?.data as { error?: boolean; reason?: string } | undefined;
      if (data?.error) addToast(String(data.reason || t('检查更新失败')), 'error');
      else addToast(t('已检查更新'));
      await load();
      if (scriptsAvailable) await loadScripts();
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
      if (scriptsAvailable) await loadScripts();
      addToast(t('已刷新插件目录'));
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    } finally {
      setBusyId(null);
    }
  };

  const openPackSettings = (pack: PluginPackSummary): void => {
    // settingsRows === null = 挂载时读取失败（后端 404/网络错），给出明确提示；
    // [] = 后端就绪但没有声明任何设置——仍然打开弹窗（弹窗自己会刷新并显示空态），
    // 避免「点设置毫无反应」。
    if (settingsRows === null) {
      addToast(t('插件设置接口读取失败，请稍后重试'), 'error');
      return;
    }
    setSettingsProviders(pack.providers.map((p) => p.id));
  };

  const focusMagpie = (): void => {
    setSource('magpie');
    // 打开脚本面板的手动安装弹窗（npm 包名 / GitHub 地址 / magpie: 前缀）
    setScriptInstallRequest((value) => value + 1);
    window.setTimeout(() => {
      document.getElementById('pl-magpie')?.scrollIntoView({ behavior: 'smooth', block: 'start' });
    }, 0);
  };

  const sourceOptions: Array<{ value: SourceFilter; label: string }> = [
    { value: 'all', label: t('全部') },
    { value: 'agent2', label: t('Agent2API 社区') },
    { value: 'magpie', label: t('Magpie 插件') },
  ];
  const statusOptions: Array<{ value: StatusFilter; label: string }> = [
    { value: 'all', label: t('全部') },
    { value: 'updatable', label: t('可更新') },
    { value: 'installed', label: t('已安装') },
    { value: 'missing', label: t('未安装') },
  ];
  const clearFilters = (): void => {
    setSource('all');
    setStatus('all');
    setSearch('');
  };

  // 页头「更多操作」清单（A2A PluginPageTools）：全部沿用既有 handler，
  // disabled/busy 与旧工具栏一致；未接通的动作不出现。
  const updatableCount = packs.filter((pack) => !!updateOf(pack.id)).length;
  const moreActions: MoreAction[] = [
    {
      key: 'update-all',
      label: t('全部更新（{{n}}）', { n: updatableCount }),
      disabled: busy || batchRunning || updatableCount === 0,
      onSelect: runUpdateAll,
    },
    {
      key: 'check',
      label: busyId === 'check' ? t('检查中…') : t('检查更新'),
      disabled: busy,
      onSelect: () => void handleCheckUpdates(),
    },
    {
      key: 'refresh',
      label: t('刷新'),
      disabled: busy,
      onSelect: () => void handleRefreshStore(),
    },
    {
      key: 'settings',
      label: t('插件设置'),
      disabled: busy,
      onSelect: () => setSettingsProviders(settingsRows?.map((row) => row.provider) ?? []),
    },
    {
      key: 'file',
      label: t('从文件安装…'),
      disabled: batchRunning,
      onSelect: () => setFileOpen(true),
    },
    {
      key: 'script',
      label: t('从 npm / GitHub 安装'),
      disabled: !scriptsAvailable || busy,
      onSelect: focusMagpie,
    },
    {
      key: 'missing',
      label: t('补全缺失插件'),
      disabled: missingCount === 0,
      onSelect: () => setMissingOpen(true),
    },
    {
      key: 'diagnostics',
      label: t('诊断信息'),
      onSelect: () => setDiagnosticsOpen(true),
    },
  ];

  return (
    <div>
      <div className="page-header">
        <div>
          <h1>{t('插件管理')}</h1>
          <p>{t('按需安装插件扩展提供商能力；启停、状态与来源统一管理。')}</p>
        </div>
        <div style={{ display: 'flex', gap: 8, alignItems: 'center' }}>
          <Button
            variant={batchMode ? 'primary' : 'outline'}
            size="sm"
            disabled={batchRunning}
            onClick={() => (batchMode ? exitBatch() : setBatchMode(true))}
          >
            {batchMode ? t('退出批量管理') : t('批量管理')}
          </Button>
          <MoreMenu label={t('更多操作')} actions={moreActions} />
        </div>
      </div>

      {error && <div className="error-message">{error}</div>}

      {notReady ? (
        <EmptyState
          icon="🧩"
          message={t('插件管理后端尚未就绪')}
          action={
            <p className="pl-empty-hint">
              {t('插件管理接口返回 404，请升级 AIGX 后端后重试。当前版本的提供商全部内置于网关，无需插件即可使用。')}
            </p>
          }
        />
      ) : loading ? (
        <Loading text={t('读取插件列表')} />
      ) : (
        <>
          {/* 筛选器 + 工具栏（A2A plugins-toolbar） */}
          <div className="pl-toolbar2" role="region" aria-label={t('插件筛选和操作')}>
            <div className="pl-filter-group">
              <span className="pl-filter-label">{t('来源')}</span>
              <div className="pl-seg" role="group" aria-label={t('来源')}>
                {sourceOptions.map((option) => (
                  <button
                    key={option.value}
                    type="button"
                    className={`pl-seg-btn${source === option.value ? ' is-active' : ''}`}
                    onClick={() => setSource(option.value)}
                  >
                    {option.label}
                    <em>{countMatching(option.value, status)}</em>
                  </button>
                ))}
              </div>
            </div>
            <div className="pl-filter-group">
              <span className="pl-filter-label">{t('状态')}</span>
              <div className="pl-seg" role="group" aria-label={t('状态')}>
                {statusOptions.map((option) => (
                  <button
                    key={option.value}
                    type="button"
                    className={`pl-seg-btn${status === option.value ? ' is-active' : ''}`}
                    onClick={() => setStatus(option.value)}
                  >
                    {option.label}
                    <em>{countMatching(source, option.value)}</em>
                  </button>
                ))}
              </div>
            </div>
            <label className="pl-search-wrap">
              <Search size={13} />
              <input
                type="search"
                className="form-input pl-search"
                value={search}
                placeholder={t('搜索插件')}
                aria-label={t('搜索插件')}
                onChange={(event) => setSearch(event.currentTarget.value)}
              />
            </label>
          </div>

          {/* 插件目录拉取异常提示条（beta.7：A2A PluginCatalogStatus 的轻量版）。
              网络失败不在每张卡片上刷屏，只在板块顶部提醒一次；已装插件照常可用。 */}
          {source !== 'magpie' && (catalog?.error || catalog?.available === false) ? (
            <div className="pl-missing-banner" role="alert">
              <span className="pl-missing-icon" aria-hidden="true">⚠️</span>
              <p>
                {t('暂未连上插件仓库，当前仅影响检查更新与目录刷新；已安装插件可正常使用。')}
                {catalog?.reason ? ` ${catalog.reason}` : ''}
              </p>
              <Button size="sm" disabled={busy} onClick={() => void handleRefreshStore()}>
                {busyId === 'refresh' ? t('重试中…') : t('重试')}
              </Button>
            </div>
          ) : null}

          {/* 缺失插件横幅（A2A PluginMissingBanner） */}
          {missingCount > 0 ? (
            <div className="pl-missing-banner" role="status">
              <span className="pl-missing-icon" aria-hidden="true">⚠️</span>
              <p>
                {missingAccounts
                  ? t('{{count}} 个插件缺失，{{accounts}} 个账号暂不可用', {
                      count: missingCount,
                      accounts: missingAccounts,
                    })
                  : t('{{count}} 个插件文件缺失', { count: missingCount })}
              </p>
              <Button size="sm" onClick={() => setMissingOpen(true)}>
                {t('补全缺失插件')}
              </Button>
            </div>
          ) : null}

          {/* 批量工具条 */}
          {batchMode ? (
            <div className="pl-batch-bar" role="region" aria-label={t('批量管理')}>
              <label className="pl-batch-all">
                <input
                  type="checkbox"
                  checked={allSelected}
                  disabled={batchRunning || selectableKeys.length === 0}
                  onChange={toggleAll}
                />
                {t('全选当前结果')}
              </label>
              <span className="pl-batch-count">
                {t('已选 {{count}} 个插件', { count: batchCount })} ·{' '}
                {t('{{installed}} 已安装 · {{missing}} 未安装', {
                  installed: selectedPacks.length,
                  missing: selectedStores.length,
                })}
              </span>
              <div className="pl-batch-actions">
                {selectedStores.length > 0 ? (
                  <Button variant="primary" size="sm" disabled={batchRunning || busy} onClick={() => void runBatch('install')}>
                    {t('安装所选（{{n}}）', { n: selectedStores.length })}
                  </Button>
                ) : null}
                {selectedPacks.length > 0 && anyUpdatable ? (
                  <Button variant="primary" size="sm" disabled={batchRunning || busy} onClick={() => void runBatch('update')}>
                    {t('批量更新')}
                  </Button>
                ) : null}
                {selectedPacks.length > 0 && anyPackDisabled ? (
                  <Button variant="outline" size="sm" disabled={batchRunning || busy} onClick={() => void runBatch('enable')}>
                    {t('批量启用')}
                  </Button>
                ) : null}
                {selectedPacks.length > 0 && anyPackEnabled ? (
                  <Button variant="outline" size="sm" disabled={batchRunning || busy} onClick={() => void runBatch('disable')}>
                    {t('批量停用')}
                  </Button>
                ) : null}
                {selectedPacks.length > 0 ? (
                  <Button variant="danger" size="sm" disabled={batchRunning || busy} onClick={confirmBatchUninstall}>
                    <Trash2 size={13} /> {t('批量卸载（{{n}}）', { n: selectedPacks.length })}
                  </Button>
                ) : null}
                {batchRunning ? <Loader2 size={13} className="pl-spin" /> : null}
                <Button variant="outline" size="sm" disabled={batchRunning} onClick={exitBatch}>
                  {t('退出')}
                </Button>
              </div>
              {batchOps.length > 0 ? (
                <ul className="pl-batch-ops" aria-live="polite">
                  {batchOps.map((op, i) => (
                    <li
                      key={`${op.id}:${i}`}
                      className={`pl-batch-op is-${!op.active && (op.stage === 'failed' || op.error || op.hostError) ? 'failed' : op.stage}`}
                    >
                      <span className="pl-batch-op-id">{op.id}</span>
                      <span className="pl-batch-op-stage">
                        {op.active
                          ? `${stageLabel(t, op.stage)}${op.stage === 'downloading' && op.total > 0 ? ` ${Math.round(op.percent || 0)}%` : ''}`
                          : op.stage === 'failed'
                            ? `${t('失败')}${op.error ? `：${op.error}` : ''}`
                            : op.hostError
                              ? `${t('失败')}：${op.hostError}`
                              : t('完成')}
                      </span>
                    </li>
                  ))}
                </ul>
              ) : null}
            </div>
          ) : null}

          {/* 双源分区：Agent2API 社区 / Magpie 插件 */}
          {showAgent2 ? (
            <Card
              title={`${t('Agent2API 社区')} (${installedRows.length + storeRows.length})`}
              actions={<span className="pl-section-hint">{t('独立插件，来自项目插件仓库')}</span>}
            >
              <div className="pl-grid">
                {installedRows.map((pack) => {
                  const op = operations.find((o) => o.id === pack.id);
                  const update = updateOf(pack.id);
                  return (
                    <InstalledPackCard
                      key={pack.id}
                      pack={pack}
                      operation={op}
                      busy={busy || op?.active === true}
                      canUpdate={!!update}
                      latestVersion={update?.latest}
                      hasUpdateNotes={Boolean(update?.notes || update?.page)}
                      selectable={batchMode}
                      checked={selected.has(`pack:${pack.id}`)}
                      onToggleSelect={() => toggleSelect(`pack:${pack.id}`)}
                      onEnabled={(v) => handleEnabled(pack, v)}
                      onReload={() => handleReload(pack)}
                      onRemove={() => handleUninstall(pack)}
                      onProviderEnabled={(provider, v) => handleProviderEnabled(pack, provider, v)}
                      onOpenSettings={() => openPackSettings(pack)}
                      onUpdate={() => handleUpdate(pack)}
                      onNotes={() =>
                        setNotes({
                          name: pack.name || pack.id,
                          version: update?.latest || pack.version,
                          notes: update?.notes,
                          page: update?.page,
                          homepage: pack.homepage,
                        })
                      }
                    />
                  );
                })}
                {storeRows.map((item) => (
                  <StorePackCard
                    key={item.id}
                    item={item}
                    target={target}
                    catalogAvailable={catalog?.available !== false}
                    catalogReason={catalog?.reason}
                    busy={busy}
                    operation={operations.find((op) => op.id === item.id)}
                    selectable={batchMode}
                    checked={selected.has(`store:${item.id}`)}
                    onToggleSelect={() => toggleSelect(`store:${item.id}`)}
                    onInstall={handleInstall}
                    onNotes={() =>
                      setNotes({
                        name: item.name || item.id,
                        version: item.version,
                        notes: item.notes,
                        page: item.page,
                        homepage: item.homepage,
                      })
                    }
                  />
                ))}
              </div>
            </Card>
          ) : null}

          {showMagpie ? (
            <div id="pl-magpie">
              <ScriptPluginsPanel query={search} status={status} installRequest={scriptInstallRequest} />
            </div>
          ) : null}

          {!showAgent2 && !showMagpie ? (
            <Card>
              <EmptyState
                icon="🧩"
                message={hasAny ? t('没有符合筛选条件的插件') : t('暂无可安装的插件')}
                action={
                  hasAny && filtersActive ? (
                    <Button variant="outline" size="sm" onClick={clearFilters}>
                      {t('清除筛选')}
                    </Button>
                  ) : (
                    <p className="pl-empty-hint">
                      {t('可以点击「刷新」读取插件仓库，或从本地 zip 安装插件。')}
                    </p>
                  )
                }
              />
            </Card>
          ) : null}

          {diagnosticsOpen ? (
            <div
              className="modal-overlay"
              role="dialog"
              aria-modal="true"
              aria-label={t('诊断信息')}
              onClick={() => setDiagnosticsOpen(false)}
            >
              <div
                className="modal"
                style={{ maxWidth: 560, width: '92%' }}
                onClick={(event) => event.stopPropagation()}
              >
                <div className="modal-header">
                  <h3>{t('诊断信息')}</h3>
                  <button className="modal-close" aria-label={t('关闭')} onClick={() => setDiagnosticsOpen(false)}>
                    &times;
                  </button>
                </div>
                <div className="modal-body">
                  <div className="pl-diagnostics-body">
                    <span>{t('插件目录')} <code>{payload?.pluginsDir || '—'}</code></span>
                    <span>{t('当前平台')} <code>{target || '—'}</code></span>
                    <span>{t('协议版本')} {payload?.protocol ?? '—'}</span>
                    <span>{t('已安装包')} {packs.length}</span>
                    <span>{t('在途操作')} {operations.filter((op) => op.active).length}</span>
                    <span>{t('可选更新')} {updatableCount}</span>
                    <span>{t('隐藏账号')} {payload?.hiddenAccounts?.total ?? 0}</span>
                    <span>{t('安装包体积上限')} {fmtSize(payload?.maxPackBytes ?? 0) || '—'}</span>
                    <span>
                      {t('宿主 lastError')}{' '}
                      <code>
                        {packHostErrors.length > 0
                          ? packHostErrors.join('；')
                          : payload?.host?.lastError || '—'}
                      </code>
                    </span>
                    <span>
                      {t('脚本运行时')} {scriptsAvailable
                        ? `${scriptHost?.running ? t('宿主运行中') : t('宿主未运行')} · ${t('已装载 {{count}} 个包', { count: scriptHost?.packages ?? 0 })} · ${t('崩溃 {{count}} 次', { count: scriptHost?.crashes ?? 0 })}`
                        : t('脚本插件后端尚未就绪')}
                    </span>
                    {scriptHost?.lastError ? (
                      <span className="pl-dg-err">{scriptHost.lastError}</span>
                    ) : null}
                    {catalog?.reason ? <span className="pl-dg-err">{catalog.reason}</span> : null}
                  </div>
                </div>
              </div>
            </div>
          ) : null}

          <p className="pl-page-foot">
            {t('插件独立安装与更新。卸载后账号数据仍会保留；脚本插件共用一个按需下载的运行时。')}
          </p>
        </>
      )}

      {notes ? (
        <div
          className="modal-overlay"
          role="dialog"
          aria-modal="true"
          aria-label={t('更新日志')}
          onClick={() => setNotes(null)}
        >
          <div
            className="modal"
            style={{ maxWidth: 560, width: '92%' }}
            onClick={(event) => event.stopPropagation()}
          >
            <div className="modal-header">
              <h3>
                {t('更新日志')}
                {notes.version ? <span className="pl-ver"> v{notes.version}</span> : null}
              </h3>
              <button className="modal-close" aria-label={t('关闭')} onClick={() => setNotes(null)}>
                &times;
              </button>
            </div>
            <div className="modal-body">
              <p className="pl-missing-name">{notes.name}</p>
              {notes.notes ? (
                <p style={{ whiteSpace: 'pre-wrap', lineHeight: 1.6, margin: '8px 0' }}>{notes.notes}</p>
              ) : (
                <p className="pl-empty-hint">{t('暂无更新日志')}</p>
              )}
              {notes.page || notes.homepage ? (
                <a
                  className="pl-page-foot"
                  href={notes.page || notes.homepage || '#'}
                  target="_blank"
                  rel="noreferrer"
                >
                  {t('打开更新页面')}
                </a>
              ) : null}
            </div>
          </div>
        </div>
      ) : null}

      <ConfirmDialog state={confirmState} onClose={() => setConfirmState(null)} />
      <ProviderSettingsDialog
        open={settingsProviders !== null}
        providers={settingsProviders ?? undefined}
        onClose={() => setSettingsProviders(null)}
        onSaved={() => void loadProviderSettings()}
      />
      <FileInstallDialog
        open={fileOpen}
        maxPackBytes={payload?.maxPackBytes ?? 24 * 1024 * 1024}
        onClose={() => setFileOpen(false)}
        onInstalled={() => {
          void load();
          void loadScripts();
        }}
      />
      {missingOpen ? (
        <div className="ps-overlay" role="dialog" aria-modal="true" aria-label={t('补全缺失的插件')}>
          <div className="ps-panel">
            <div className="ps-head">
              <div>
                <h2>{t('补全缺失的插件')}</h2>
                <p>{t('安装并成功加载后，现有账号和原有计划会自动恢复。')}</p>
              </div>
              <div className="ps-head-actions">
                <Button variant="outline" size="sm" onClick={() => setMissingOpen(false)}>
                  {t('稍后')}
                </Button>
              </div>
            </div>
            <div className="ps-body">
              {requiredRows.length > 0 ? (
                <section>
                  <h3 className="pl-missing-section">{t('恢复现有账号')}</h3>
                  <ul className="pl-missing-rows">
                    {requiredRows.map((row) => (
                      <li key={row.key} className="pl-missing-row">
                        <div>
                          <p className="pl-missing-name">{row.name}</p>
                          <p className="pl-missing-meta">
                            {t('Agent2API 社区')} · {row.version || t('暂未发布')}
                            {row.accounts
                              ? ` · ${t('{{count}} 个账号等待插件恢复', { count: row.accounts })}`
                              : ` · ${t('插件文件缺失，请重新安装')}`}
                          </p>
                          {row.reason ? <p className="pl-missing-meta">{row.reason}</p> : null}
                        </div>
                        <Button
                          size="sm"
                          disabled={busy || !row.installable}
                          onClick={() => installMissing(row.storeId)}
                        >
                          {t('重新安装')}
                        </Button>
                      </li>
                    ))}
                  </ul>
                </section>
              ) : null}
              {unresolved.length > 0 ? (
                <div className="pl-missing-warn">
                  <p>
                    {t('以下提供商暂未找到对应的可安装插件：{{names}}', {
                      names: unresolved.map((item) => item.label).join(t('、')),
                    })}
                  </p>
                  <p>{t('请刷新目录，或从本地 zip 安装对应插件包。')}</p>
                </div>
              ) : null}
              {optionalStores.length > 0 ? (
                <section>
                  <h3 className="pl-missing-section">{t('可选安装')}</h3>
                  <ul className="pl-missing-rows">
                    {optionalStores.map((item) => (
                      <li key={item.id} className="pl-missing-row">
                        <div>
                          <p className="pl-missing-name">{item.name || item.id}</p>
                          <p className="pl-missing-meta">
                            {t('Agent2API 社区')} · {item.version || t('暂未发布')}
                          </p>
                        </div>
                        <Button size="sm" variant="outline" disabled={busy} onClick={() => installMissing(item.id)}>
                          {t('下载')}
                        </Button>
                      </li>
                    ))}
                  </ul>
                </section>
              ) : null}
              {requiredRows.length === 0 && unresolved.length === 0 && optionalStores.length === 0 ? (
                <p className="pl-empty-hint">{t('没有缺失的插件')}</p>
              ) : null}
            </div>
          </div>
        </div>
      ) : null}
    </div>
  );
}
