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
  ScriptPackageView,
  ScriptPackagesPayload,
} from '../../types';
import './ScriptPluginsPanel.css';

/** unix 秒 → 日期（目录时间戳展示用） */
function fmtUnix(value: number | null | undefined): string {
  if (!value || value <= 0) return '—';
  return new Date(value * 1000).toLocaleString();
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
            <Star size={11} /> {entry.stars} 星
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
        <Tooltip label={t('卸载后该脚本包的提供商将停止接管请求')}>
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
    await onSave(pkg, values);
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
    try {
      const [listRes, catRes] = await Promise.all([api.listScriptPlugins(), api.scriptCatalog()]);
      if (!mounted.current) return;
      setPayload(listRes?.data ?? null);
      setCatalog(catRes?.data ?? null);
      setNotReady(false);
      setError('');
    } catch (err) {
      if (!mounted.current) return;
      const msg = err instanceof Error ? err.message : String(err);
      const is404 = /404|not found/i.test(msg);
      setNotReady(is404);
      setError(is404 ? '' : msg);
      setPayload(null);
      setCatalog(null);
    } finally {
      if (mounted.current) setLoading(false);
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

  /** 通用写操作包装：busyKey 占位 + 完成回读 + toast */
  const runMutation = async (key: string, action: () => Promise<void>): Promise<void> => {
    if (busy) return;
    setBusyKey(key);
    try {
      await action();
      await load();
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    } finally {
      if (mounted.current) setBusyKey(null);
    }
  };

  /** 目录卡「安装」：二次确认（含沙箱警告）后才传 trusted=true */
  const handleInstall = (entry: ScriptCatalogEntryView): void => {
    setConfirmState({
      title: t('安装脚本包'),
      message: t('脚本不是沙箱，将以 Bun 进程权限运行。确认安装「{{name}}」？', { name: entry.label || entry.name }),
      confirmText: t('确认安装'),
      danger: true,
      onConfirm: async () => {
        await runMutation(`install:${entry.selector}`, async () => {
          await api.installScriptPackage(entry.selector, true);
          addToast(t('脚本包已安装'));
        });
      },
    });
  };

  const handleToggleEnabled = (pkg: ScriptPackageView, enabled: boolean): void => {
    void runMutation(`enable:${pkg.id}`, async () => {
      await api.enableScriptPackage(pkg.id, enabled);
      addToast(enabled ? t('脚本包已启用') : t('脚本包已停用'));
    });
  };

  const handleUninstall = (pkg: ScriptPackageView): void => {
    setConfirmState({
      title: t('卸载脚本包'),
      message: t('确定卸载「{{name}}」吗？该脚本包声明的提供商将停止接管请求，包配置与元数据会被删除。', {
        name: pkg.label || pkg.id,
      }),
      confirmText: t('卸载'),
      danger: true,
      onConfirm: async () => {
        await runMutation(`remove:${pkg.id}`, async () => {
          await api.uninstallScriptPackage(pkg.id);
          addToast(t('脚本包已卸载'));
        });
      },
    });
  };

  const handleSaveOptions = async (pkg: ScriptPackageView, values: Record<string, unknown>): Promise<void> => {
    try {
      await api.saveScriptPackageOptions(pkg.id, values);
      addToast(t('配置已保存'));
      setEditingPkg(null);
      await load();
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    }
  };

  const handleRefreshCatalog = async (): Promise<void> => {
    if (busy) return;
    setBusyKey('refresh');
    try {
      await api.refreshScriptCatalog();
      // 后台任务异步刷新：等一小段再回读，让新条目可见
      await new Promise((resolve) => window.setTimeout(resolve, 2500));
      await load();
      addToast(t('目录已刷新'));
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    } finally {
      if (mounted.current) setBusyKey(null);
    }
  };

  const handleRebuildHost = async (): Promise<void> => {
    if (busy) return;
    setBusyKey('rebuild');
    try {
      await api.rebuildScriptHost();
      await load();
      addToast(t('宿主已重启'));
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    } finally {
      if (mounted.current) setBusyKey(null);
    }
  };

  return (
    <div>
      {/* 宿主状态条 */}
      <Card
        title={t('宿主状态')}
        actions={
          <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap' }}>
            <Button variant="outline" size="sm" onClick={() => void handleRefreshCatalog()} disabled={busy}>
              {busyKey === 'refresh' ? <Loader2 size={13} className="spin" /> : <RefreshCw size={13} />}
              {busyKey === 'refresh' ? t('刷新目录中…') : t('刷新目录')}
            </Button>
            <Button variant="outline" size="sm" onClick={() => void handleRebuildHost()} disabled={busy}>
              {busyKey === 'rebuild' ? <Loader2 size={13} className="spin" /> : <RotateCcw size={13} />}
              {busyKey === 'rebuild' ? t('重启宿主中…') : t('重启宿主')}
            </Button>
          </div>
        }
      >
        <div className="pls-host-bar">
          <Badge tone={host?.running ? 'success' : 'neutral'}>{host?.running ? t('宿主运行中') : t('宿主未运行')}</Badge>
          <span>{t('已装载 {{count}} 个包', { count: host?.packages ?? 0 })}</span>
          <span>{t('接管 {{count}} 家提供商', { count: host?.providers.length ?? 0 })}</span>
          {host && host.crashes > 0 ? <Badge tone="warning">{t('崩溃 {{count}} 次', { count: host.crashes })}</Badge> : null}
          {host?.lastError ? <span className="pl-dg-err" title={host.lastError}>{t('最近错误：{{err}}', { err: host.lastError })}</span> : null}
        </div>
        {catalog?.status ? (
          <div className="pls-host-bar pls-cat-bar">
            <span>{t('目录：官方 {{a}} / 推荐 {{b}} / 第三方 {{c}}', {
              a: catalog.status.official,
              b: catalog.status.recommended,
              c: catalog.status.third_party,
            })}</span>
            <span>{t('市场清单更新于 {{time}}', { time: fmtUnix(catalog.status.market_at) })}</span>
            {catalog.status.market_error ? <span className="pl-dg-err">{catalog.status.market_error}</span> : null}
            {catalog.status.github_error ? <span className="pl-dg-err">{catalog.status.github_error}</span> : null}
          </div>
        ) : null}
      </Card>

      {/* 已安装脚本包 */}
      <div style={{ marginTop: 14 }}>
      <Card title={`${t('已安装脚本包')} (${packages.length})`}>
        {packages.length === 0 ? (
          <EmptyState icon="🧩" message={t('暂无已安装脚本包')} />
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

        {notReady ? (
          <EmptyState
            icon="🧩"
            message={t('脚本插件后端尚未就绪')}
            action={<p className="pl-empty-hint">{t('AIGX 的脚本插件管理面正在建设中；后端上线本区块即自动可用。')}</p>}
          />
        ) : error ? (
          <div className="error-message">{error}</div>
        ) : loading ? (
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
