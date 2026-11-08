import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Loader2, RefreshCw, Trash2 } from 'lucide-react';
import { api } from '../api';
import { useToast } from '../components/Toast';
import ConfirmDialog, { type ConfirmState } from '../components/ConfirmDialog';
import { Badge, Button, Card, EmptyState, Loading, Switch, Tooltip } from '../components/ui';
import type { PluginOperation, PluginPackSummary, PluginsPayload, StorePlugin } from '../types';
import './Plugins.css';

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

/** 已安装包的卡片（启停/卸载/单家提供商启停） */
function InstalledPackCard({
  pack,
  operation,
  busy,
  onEnabled,
  onRemove,
  onProviderEnabled,
}: {
  pack: PluginPackSummary;
  operation?: PluginOperation;
  busy: boolean;
  onEnabled: (enabled: boolean) => void;
  onRemove: () => void;
  onProviderEnabled: (provider: string, enabled: boolean) => void;
}): JSX.Element {
  const { t } = useTranslation();
  const [providersOpen, setProvidersOpen] = useState(false);
  const loadedCount = pack.providers.filter((p) => p.loaded).length;
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
            <Badge tone={pack.signed ? 'success' : 'warning'}>
              {pack.signed ? t('已签名') : t('未签名')}
            </Badge>
            {pack.version.includes('-') ? <Badge tone="warning">{t('beta 版')}</Badge> : null}
            {pack.updateRequired ? <Badge tone="danger">{t('需要补齐插件')}</Badge> : null}
            {!pack.entryPresent ? <Badge tone="danger">{t('文件缺失')}</Badge> : null}
          </div>
        </div>
      </div>
      {pack.description ? <p className="pl-card-desc">{pack.description}</p> : null}
      <p className="pl-card-meta">
        {t('{{count}} 家提供商 · 安装于 {{time}}', { count: pack.providers.length, time: fmtInstalledAt(pack.installedAt) })}
      </p>
      <div className="pl-card-state">
        <span>{pack.host?.running ? t('运行中') : t('未运行')}</span>
        <span>{t('已加载 {{count}} 家', { count: loadedCount })}</span>
      </div>
      {pack.compatibilityError ? (
        <p className="pl-card-err" title={pack.compatibilityError}>{pack.compatibilityError}</p>
      ) : null}
      {!pack.entryPresent ? (
        <p className="pl-card-err">{t('插件包的可执行文件不在磁盘上（可能被手工删除或移动），请重新安装。')}</p>
      ) : null}
      {operation?.error ? <p className="pl-card-err" title={operation.error}>{operation.error}</p> : null}
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
              {providersOpen ? t('收起提供商') : t('提供商设置（{{n}}）', { n: pack.providers.length })}
            </Button>
          )}
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
              <Switch
                checked={p.enabled}
                disabled={busy}
                onChange={(v) => onProviderEnabled(p.id, v)}
                ariaLabel={`${t('启用')} ${p.label}`}
              />
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

/** 商店（发布目录）条目的卡片（在线安装暂未开放——AIGX 无插件商店分发渠道） */
function StorePackCard({ item, target }: { item: StorePlugin; target: string }): JSX.Element {
  const { t } = useTranslation();
  const platform = item.platforms.find((p) => p.target === target);
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
      <div className="pl-card-actions">
        <span className="pl-store-hint" title={t('AIGX 暂未接入插件商店分发渠道；本地安装与后端管理面就绪后开放')}>
          {t('暂不支持在线安装')}
        </span>
      </div>
    </div>
  );
}

/**
 * Plugins — 插件管理页（管理员专属）。
 *
 * 契约对齐 Agent2API api/plugins.rs（list/enable/provider/uninstall/update/release/progress）。
 * AIGX 后端插件管理面建设中（be-impl 任务 #3）：端点 404 时整页降级为
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

  useEffect(() => {
    void load();
  }, [load]);

  // 有在途操作时 2 秒轮询进度（对齐上游操作期间的轮询；完成后回读完整列表）
  const hasActiveOps = (payload?.operations ?? []).some((op) => op.active);
  useEffect(() => {
    if (!hasActiveOps) return undefined;
    const timer = window.setInterval(() => void load(), 2000);
    return () => window.clearInterval(timer);
  }, [hasActiveOps, load]);

  const packs = useMemo(() => payload?.packs ?? [], [payload]);
  const host = payload?.host ?? null;
  const catalog = payload?.catalog;
  const installedIds = useMemo(() => new Set(packs.map((p) => p.id)), [packs]);
  const storePlugins = useMemo(() => {
    const store = catalog?.plugins ?? [];
    return store.filter((item) => !installedIds.has(item.id));
  }, [catalog, installedIds]);
  const operations = payload?.operations ?? [];
  const busy = busyId != null;

  /** 包级写操作：跑完回读（启用/禁用/卸载共享同一条路径） */
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

  return (
    <div>
      <div className="page-header">
        <div>
          <h1>{t('插件管理')}</h1>
          <p>{t('按需安装插件扩展提供商能力；启停、状态与来源统一管理。')}</p>
        </div>
        <div style={{ display: 'flex', gap: 8 }}>
          <Button variant="outline" onClick={() => void load()} disabled={loading}>
            {loading ? <Loader2 size={13} className="spin" /> : <RefreshCw size={13} />}
            {t('刷新')}
          </Button>
        </div>
      </div>

      {error && <div className="error-message">{error}</div>}

      {loading ? (
        <Loading text={t('读取插件列表')} />
      ) : notReady ? (
        <EmptyState
          icon="🧩"
          message={t('插件管理后端尚未就绪')}
          action={<p className="pl-empty-hint">{t('AIGX 的插件机制正在建设中（契约对齐 Agent2API：安装 / 启停 / 目录 / 进度）。后端上线本页即自动可用；当前版本的提供商全部内置于网关，无需插件即可使用。')}</p>}
        />
      ) : (
        <Card
          title={`${t('已安装插件')} (${packs.length})`}
          actions={
            <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap' }}>
              <Badge tone={host?.running ? 'success' : 'neutral'}>
                {host?.running ? t('宿主运行中') : packs.length ? t('宿主未运行') : t('未安装')}
              </Badge>
              <Button variant="outline" size="sm" onClick={() => void handleRefreshStore()} disabled={busy}>
                {busyId === 'refresh' ? <Loader2 size={13} className="spin" /> : null}
                {t('刷新目录')}
              </Button>
              <Button variant="outline" size="sm" onClick={() => void handleCheckUpdates()} disabled={busy}>
                {busyId === 'check' || catalog?.checking ? t('检查中…') : t('检查更新')}
              </Button>
            </div>
          }
        >
          {packs.length === 0 && storePlugins.length === 0 ? (
            <EmptyState
              icon="🧩"
              message={t('暂无已安装的插件')}
              action={<p className="pl-empty-hint">{t('AIGX 当前所有提供商均内置于网关（注册表静态表达），无需插件即可使用。插件包用于扩展后续的第三方提供商。')}</p>}
            />
          ) : (
            <div className="pl-grid">
              {packs.map((pack) => (
                <InstalledPackCard
                  key={pack.id}
                  pack={pack}
                  operation={operations.find((op) => op.id === pack.id)}
                  busy={busy}
                  onEnabled={(v) => handleEnabled(pack, v)}
                  onRemove={() => handleUninstall(pack)}
                  onProviderEnabled={(provider, v) => handleProviderEnabled(pack, provider, v)}
                />
              ))}
              {storePlugins.map((item) => (
                <StorePackCard key={item.id} item={item} target={payload?.target || ''} />
              ))}
            </div>
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
        </Card>
      )}

      <ConfirmDialog state={confirmState} onClose={() => setConfirmState(null)} />
    </div>
  );
}
