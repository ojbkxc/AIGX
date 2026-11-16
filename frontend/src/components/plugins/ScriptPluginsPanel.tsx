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
import { ExternalLink, Loader2, Plus, RefreshCw, RotateCcw, Search, Star, Trash2 } from 'lucide-react';
import { api } from '../../api';
import { useToast } from '../Toast';
import ConfirmDialog, { type ConfirmState } from '../ConfirmDialog';
import { Badge, Button, Card, EmptyState, Loading, Switch, Tooltip } from '../ui';
import type {
  ScriptCatalogEntryView,
  ScriptCatalogPayload,
  ScriptCatalogStatus,
  ScriptPackageView,
  ScriptPackagesPayload,
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

/** 图标：iconSources 按序回退（第一个候选失败 → 首字母兜底） */
function ScriptIcon({ sources, label }: { sources: string[]; label: string }): JSX.Element {
  const [failed, setFailed] = useState(false);
  const src = sources[0];
  if (!src || failed) {
    return <div className="pl-card-icon">{(label || '?').slice(0, 1).toUpperCase()}</div>;
  }
  return (
    <div className="pl-card-icon">
      <img src={src} alt="" onError={() => setFailed(true)} />
    </div>
  );
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
        <ScriptIcon sources={entry.iconSources} label={entry.label || entry.name} />
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
  onUninstall,
  onEditOptions,
}: {
  pkg: ScriptPackageView;
  busy: boolean;
  onToggleEnabled: (pkg: ScriptPackageView, enabled: boolean) => void;
  onUninstall: (pkg: ScriptPackageView) => void;
  onEditOptions: (pkg: ScriptPackageView) => void;
}): JSX.Element {
  const { t } = useTranslation();
  return (
    <div className="pl-card is-installed">
      <div className="pl-card-head">
        <div className="pl-card-icon">{(pkg.label || pkg.id).slice(0, 1).toUpperCase()}</div>
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

/** 配置弹窗：options.values 键值对编辑器（值按 JSON 解析，失败按字符串保存） */
function OptionsDialog({
  pkg,
  busy,
  onClose,
  onSave,
}: {
  pkg: ScriptPackageView;
  busy: boolean;
  onClose: () => void;
  onSave: (pkg: ScriptPackageView, values: Record<string, unknown>) => Promise<void>;
}): JSX.Element {
  const { t } = useTranslation();
  const [rows, setRows] = useState<Array<{ key: string; value: string }>>(() =>
    Object.entries(pkg.options?.values ?? {}).map(([key, value]) => ({
      key,
      value: typeof value === 'string' ? value : JSON.stringify(value),
    }))
  );
  /** 保存失败的就地错误：弹窗不关，用户能看着错误改 */
  const [saveError, setSaveError] = useState('');

  const updateRow = (index: number, patch: Partial<{ key: string; value: string }>): void => {
    setRows((prev) => prev.map((row, i) => (i === index ? { ...row, ...patch } : row)));
  };
  const removeRow = (index: number): void => {
    setRows((prev) => prev.filter((_, i) => i !== index));
  };
  const addRow = (): void => {
    setRows((prev) => [...prev, { key: '', value: '' }]);
  };

  const handleSave = async (): Promise<void> => {
    const values: Record<string, unknown> = {};
    for (const row of rows) {
      if (!row.key) continue;
      try {
        values[row.key] = JSON.parse(row.value);
      } catch {
        values[row.key] = row.value;
      }
    }
    try {
      setSaveError('');
      await onSave(pkg, values);
    } catch (err) {
      // 失败必须可见且弹窗保持打开（onSave 成功时才由父层关闭）
      setSaveError(formatError(err));
    }
  };

  return (
    <div className="modal-overlay" role="dialog" aria-modal="true" onClick={onClose}>
      <div className="modal" style={{ maxWidth: 560, width: '92%' }} onClick={(e) => e.stopPropagation()}>
        <div className="modal-header">
          <h3>{t('编辑配置 · {{name}}', { name: pkg.label || pkg.id })}</h3>
          <button className="modal-close" onClick={onClose} aria-label={t('关闭')}>
            &times;
          </button>
        </div>
        <div className="modal-body">
          {rows.length === 0 ? (
            <p className="pl-store-hint">{t('暂无配置项')}</p>
          ) : (
            rows.map((row, index) => (
              <div className="pls-opt-row" key={index}>
                <input
                  className="form-input pls-opt-key"
                  placeholder={t('键')}
                  value={row.key}
                  onChange={(e) => updateRow(index, { key: e.target.value })}
                />
                <input
                  className="form-input pls-opt-val"
                  placeholder={t('值')}
                  value={row.value}
                  onChange={(e) => updateRow(index, { value: e.target.value })}
                />
                <Button variant="outline" size="sm" onClick={() => removeRow(index)} aria-label={t('删除')}>
                  <Trash2 size={13} />
                </Button>
              </div>
            ))
          )}
          <Button variant="outline" size="sm" onClick={addRow}>
            <Plus size={13} /> {t('添加配置项')}
          </Button>
          <p className="pl-store-hint" style={{ marginTop: 8 }}>
            {t('值按 JSON 解析（数字/布尔/对象直接生效），解析失败按字符串保存。')}
          </p>
          {saveError ? (
            <div className="error-message" style={{ marginTop: 10 }} title={saveError}>
              {saveError}
            </div>
          ) : null}
        </div>
        <div className="modal-footer">
          <Button variant="outline" onClick={onClose}>
            {t('取消')}
          </Button>
          <Button variant="primary" disabled={busy} onClick={() => void handleSave()}>
            {busy ? <Loader2 size={13} className="spin" /> : null}
            {busy ? t('保存中…') : t('保存配置')}
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
export default function ScriptPluginsPanel(): JSX.Element {
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

  const [payload, setPayload] = useState<ScriptPackagesPayload | null>(null);
  const [catalog, setCatalog] = useState<ScriptCatalogPayload | null>(null);
  const [loading, setLoading] = useState(true);
  const [notReady, setNotReady] = useState(false);
  const [error, setError] = useState('');
  const [busyKey, setBusyKey] = useState<string | null>(null);
  const [confirmState, setConfirmState] = useState<ConfirmState | null>(null);
  const [editingPkg, setEditingPkg] = useState<ScriptPackageView | null>(null);

  // 筛选与搜索
  const [query, setQuery] = useState('');
  const [sourceFilter, setSourceFilter] = useState<'all' | 'magpie' | 'npm' | 'github'>('all');
  const [statusFilter, setStatusFilter] = useState<'all' | 'installed' | 'notInstalled' | 'updatable'>('all');

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  const load = useCallback(async (): Promise<void> => {
    const seq = ++loadSeq.current;
    try {
      const [listRes, catRes] = await Promise.all([api.listScriptPlugins(), api.scriptCatalog()]);
      if (!mounted.current || seq !== loadSeq.current) return;
      setPayload(listRes?.data ?? null);
      setCatalog(catRes?.data ?? null);
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

  const handleSaveOptions = async (pkg: ScriptPackageView, values: Record<string, unknown>): Promise<void> => {
    // 后端强制 ≤ 256 KiB（MAX_SETTINGS_BYTES）：本地先量一次，给出可读错误，
    // 而不是等整轮请求被 400 打回。
    const bytes = new TextEncoder().encode(JSON.stringify(values)).length;
    if (bytes > MAX_OPTIONS_BYTES) {
      throw new Error(t('配置过大（{{size}} KiB），上限 256 KiB', { size: Math.ceil(bytes / 1024) }));
    }
    const res = await runMutation(
      `options:${pkg.id}@${pkg.version}`,
      async () => {
        await api.saveScriptPackageOptions(pkg.id, values, pkg.version);
        addToast(t('配置已保存'));
      },
      { toastError: false }
    );
    if (!res.ok) {
      // 抛回 OptionsDialog：弹窗保持打开并就地显示错误，用户改完可直接重试
      if (res.error && res.error !== 'busy') throw new Error(res.error);
      return;
    }
    setEditingPkg(null);
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
            <Card title={`${t('已安装脚本包')} (${packages.length})`}>
              {loading && !payload ? (
                <Loading text={t('正在读取脚本插件…')} />
              ) : packages.length === 0 ? (
                <EmptyState
                  icon="🧩"
                  message={t('暂无已安装脚本包')}
                  action={<p className="pl-empty-hint">{t('从下方目录选择脚本包安装后，在这里启停、配置与卸载。')}</p>}
                />
              ) : (
                <div className="pl-grid">
                  {packages.map((pkg) => (
                    <InstalledScriptCard
                      key={`${pkg.id}@${pkg.version}`}
                      pkg={pkg}
                      busy={busy}
                      onToggleEnabled={handleToggleEnabled}
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
      {editingPkg ? (
        <OptionsDialog
          pkg={editingPkg}
          busy={busy}
          onClose={() => setEditingPkg(null)}
          onSave={handleSaveOptions}
        />
      ) : null}
    </div>
  );
}
