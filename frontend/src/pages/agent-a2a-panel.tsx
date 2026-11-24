/**
 * A2A（Agent2API v3.5.1）Agent 页客户端配置管理区。
 *
 * 结构严格对齐 `agent-page.tsx` + `agent-editor.tsx` + `agent-fields.tsx`：
 * 顶部工具条（搜索 / 分段筛选 / 重新检测）→ 左栏客户端清单 → 右栏客户端详情
 * （客户端头 + 四个页签 + 配置列表 + 配置位置与说明折叠区）。
 *
 * AIGX 后端缺「本机客户端检测」数据源：字段与控件先照 A2A 落结构，
 * 空态与占位照 A2A 文案，数据源缺失记入交付清单。
 */

import { useEffect, useMemo, useState, useSyncExternalStore, type CSSProperties } from 'react';
import { ChevronDown, ChevronRight, Plus, RotateCw, Search } from 'lucide-react';
import { Button } from '../components/ui';
import {
  A2A_TEXT,
  A2A_TAB_KEYS,
  type A2aClient,
  type A2aTabKey,
} from './agent-a2a-types';
import {
  clearFilters,
  counts,
  detectionLabel,
  formatDetectedTime,
  getA2aStore,
  loadPanel,
  kindLabel,
  protocolLabel,
  quickBlockReason,
  selectClient,
  selectedClient,
  setFilter,
  setQuery,
  subscribeA2aStore,
  visibleClients,
} from './agent-a2a-state';
import { A2aPathDialog, A2aProfileDialog, A2aQuickConnectDialog } from './agent-a2a-dialogs';

const A2A_FILTER_OPTIONS: Array<{ key: 'all' | 'via' | 'direct' | 'missing'; label: string }> = [
  { key: 'all', label: A2A_TEXT.filterAll },
  { key: 'via', label: A2A_TEXT.filterVia },
  { key: 'direct', label: A2A_TEXT.filterDirect },
  { key: 'missing', label: A2A_TEXT.filterMissing },
];

/** 客户端头像（对齐 A2A ClientAvatar：有图标用图标，否则首字母 + hue 底色） */
function A2aAvatar({ client, large = false }: { client: A2aClient; large?: boolean }): JSX.Element {
  const icon = Boolean(client.icon) && client.iconFull;
  return (
    <span
      className={`a2a-avatar${large ? ' is-lg' : ''}${icon ? ' has-icon' : ''}`}
      style={{ '--a2a-hue': String(client.hue) } as CSSProperties}
      aria-hidden="true"
    >
      {icon ? <img src={client.icon} alt="" loading="lazy" /> : client.letter}
    </span>
  );
}

/** 状态点（对齐 A2A StatusDot 的四种变体） */
function A2aStatusDot({ variant }: { variant: 'success' | 'warning' | 'destructive' | 'muted' }): JSX.Element {
  return <span className={`a2a-dot a2a-dot-${variant}`} aria-hidden="true" />;
}

function ClientRow({ client, active }: { client: A2aClient; active: boolean }): JSX.Element {
  const live = client.configurations.find((profile) => profile.status === 'active' || profile.status === 'enabled');
  const dotVariant = client.readError
    ? 'destructive'
    : client.applied.pendingRecovery
      ? 'warning'
      : client.viaGateway
        ? 'success'
        : 'muted';
  const subtitle = client.readError
    ? A2A_TEXT.readingFailed
    : client.applied.pendingRecovery
      ? A2A_TEXT.pendingRecovery
      : live
        ? client.viaGateway ? `${live.name} · ${A2A_TEXT.connecting}` : live.name
        : client.configurations.length > 0
          ? A2A_TEXT.notEnabled
          : detectionLabel(client);

  return (
    <button
      type="button"
      className={`a2a-client${active ? ' is-active' : ''}${client.detected ? '' : ' is-missing'}`}
      onClick={() => selectClient(client.id)}
    >
      <A2aAvatar client={client} />
      <span className="a2a-client-main">
        <span className="a2a-client-name">
          {client.name}
          {client.version && <span className="a2a-client-ver">{client.version}</span>}
        </span>
        <span className="a2a-client-sub">{subtitle}</span>
      </span>
      <A2aStatusDot variant={dotVariant} />
    </button>
  );
}

/** 配置列表行（对齐 agent-editor.tsx 的 profile 行：徽标 / 名称 / 地址 / 模型 / 操作） */
function ConfigRow({ client, profile }: { client: A2aClient; profile: A2aProfileLike }): JSX.Element {
  const active = profile.status === 'active' || profile.status === 'enabled';
  const statusText = profile.status === 'active' ? A2A_TEXT.current : profile.status === 'enabled' ? A2A_TEXT.enabled : profile.status === 'disabled' ? A2A_TEXT.disabled : '';
  const name = profile.name || profile.id;
  const duplicateName = client.configurations.some(
    (other) => other !== profile && (other.name || other.id) === name,
  );
  const models = profile.models.length
    ? profile.models.map((model) => model.id)
    : profile.model
      ? [profile.model]
      : [];
  const actionLabel = profile.status === 'disabled' ? A2A_TEXT.toggleOn : client.supportsDefault ? A2A_TEXT.useConfig : A2A_TEXT.addToClient;

  return (
    <div className={`a2a-config-row${active ? ' is-current' : ''}`}>
      <span className="a2a-config-mark" aria-hidden="true">
        {profile.kind === 'gateway' ? 'A2' : name.slice(0, 2).toUpperCase()}
      </span>
      <div className="a2a-config-main">
        <div className="a2a-config-title">
          <span title={name}>{name}</span>
          {duplicateName && <code>{profile.sourceId || profile.id}</code>}
        </div>
        <div className="a2a-config-meta">
          <span className="a2a-config-url" title={profile.baseUrl}>{profile.baseUrl || '客户端默认地址'}</span>
          <span>{kindLabel(profile.kind)}</span>
          <span>{protocolLabel(profile.protocol)}</span>
        </div>
        {models.length > 0 && (
          <p className="a2a-config-models" title={models.join(' · ')}>
            {models.slice(0, 3).join(' · ')}
            {models.length > 3 ? ` · +${models.length - 3}` : ''}
          </p>
        )}
        {profile.note && <p className="a2a-config-note">{profile.note}</p>}
        {profile.validationError && (
          <p className="a2a-config-error">{A2A_TEXT.missingFields} · {profile.validationError}</p>
        )}
      </div>
      <div className="a2a-config-actions">
        {statusText && (
          <span className={`a2a-config-status${active ? ' is-current' : ''}`} title={active ? A2A_TEXT.statusActive : undefined}>
            <A2aStatusDot variant={active ? 'success' : 'muted'} />
            {statusText}
          </span>
        )}
        {!active && profile.editable && (
          <Button size="sm" variant="outline" disabled={Boolean(client.storeError)}>
            {actionLabel}
          </Button>
        )}
        {profile.editable && (
          <button type="button" className="a2a-icon-btn" title="编辑" aria-label={`编辑：${name}`}>
            <A2aEditIcon />
          </button>
        )}
        {profile.deletable && (
          <button type="button" className="a2a-icon-btn a2a-danger" title="配置操作" aria-label="配置操作">
            <ChevronDown size={15} />
          </button>
        )}
      </div>
    </div>
  );
}

function A2aEditIcon(): JSX.Element {
  return (
    <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.5"
      strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      <path d="m16 3 5 5M4 20l4-1L21 6a2 2 0 0 0-3-3L5 16l-1 4Z" />
    </svg>
  );
}

/** A2A 的 bolt 图标（一键接入） */
function BoltIcon(): JSX.Element {
  return (
    <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.5"
      strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      <path d="M13 2 4 14h7l-1 8 9-12h-7l1-8Z" />
    </svg>
  );
}

/** A2A 的 more 图标（客户端选项菜单） */
function MoreIcon(): JSX.Element {
  return (
    <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="3"
      strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      <path d="M4 12h.01M12 12h.01M20 12h.01" />
    </svg>
  );
}

type A2aProfileLike = A2aClient['configurations'][number];

/** 非「模型配置」页签的结构占位（A2A 用 AgentResourcesPanel 渲染，AIGX 缺该数据源） */
function ResourceTabPlaceholder({ tab, client }: { tab: A2aTabKey; client: A2aClient }): JSX.Element {
  return (
    <section className="a2a-resource-panel" aria-label={A2A_TEXT.tabs[tab]}>
      <div className="a2a-resource-note">
        <span>
          {A2A_TEXT.machineNote}
          {getA2aStore().hostName ? ` · ${getA2aStore().hostName}` : ''}
        </span>
      </div>
      <div className="a2a-resource-empty">
        <div className="a2a-resource-title">{A2A_TEXT.tabs[tab]}</div>
        <p className="a2a-resource-hint">
          {`A2A v3.5.1 在「${A2A_TEXT.tabs[tab]}」页签下渲染 ${client.name} 的客户端资源；AIGX 后端暂无该数据源，结构与入口已对齐，取数待补。`}
        </p>
      </div>
    </section>
  );
}

function ClientDetail({ client, tab, onTabChange }: {
  client: A2aClient;
  tab: A2aTabKey;
  onTabChange: (next: A2aTabKey) => void;
}): JSX.Element {
  const store = getA2aStore();
  const [pathOpen, setPathOpen] = useState(false);
  const [creating, setCreating] = useState(false);
  const [quickOpen, setQuickOpen] = useState(false);
  const [menuOpen, setMenuOpen] = useState(false);
  const [detailsOpen, setDetailsOpen] = useState(false);
  const busy = Boolean(store.busy);
  const addReason = client.readError || client.storeError || (!client.writable ? client.supportNote : '');
  const canQuickConnect = client.writable && quickBlockReason(client) === '';
  const showHeaderQuick = canQuickConnect && !client.viaGateway && client.configurations.length > 0;

  return (
    <>
      <div className="a2a-dhead">
        <A2aAvatar client={client} large />
        <div className="a2a-dtitle">
          <h3>{client.name}</h3>
          <div className="a2a-meta">
            <span>{client.vendor}</span>
            {client.version ? <span>{client.version}</span> : <span>版本未识别</span>}
            {!client.writable && <span>{detectionLabel(client)}</span>}
          </div>
        </div>
        <div className="a2a-dhead-actions">
          {showHeaderQuick && (
            <Button size="sm" disabled={busy} onClick={() => setQuickOpen(true)}>
              <BoltIcon />
              {A2A_TEXT.quickConnect}
            </Button>
          )}
          <Button
            size="sm"
            variant={showHeaderQuick || (canQuickConnect && !client.configurations.length) ? 'outline' : 'primary'}
            disabled={busy || Boolean(addReason)}
            title={addReason || undefined}
            onClick={() => setCreating(true)}
          >
            <Plus size={14} />
            {A2A_TEXT.addConfig}
          </Button>
          <div className="a2a-client-menu">
            <button
              type="button"
              className="a2a-icon-btn"
              title={A2A_TEXT.clientOptions}
              aria-label={A2A_TEXT.clientOptions}
              disabled={busy}
              onClick={() => setMenuOpen((value) => !value)}
              aria-expanded={menuOpen}
            >
              <MoreIcon />
            </button>
            {menuOpen && (
              <div className="a2a-action-menu" role="menu">
                {canQuickConnect && (
                  <button type="button" role="menuitem" onClick={() => { setMenuOpen(false); setQuickOpen(true); }}>
                    {A2A_TEXT.quickConnectGateway}
                  </button>
                )}
                <button type="button" role="menuitem" onClick={() => { setMenuOpen(false); void loadPanel({ force: true }); }}>
                  {A2A_TEXT.redetect}
                </button>
                <button type="button" role="menuitem" onClick={() => { setMenuOpen(false); setPathOpen(true); }}>
                  {A2A_TEXT.choosePath}
                </button>
                {client.applied.canUndo && (
                  <button type="button" role="menuitem" onClick={() => { setMenuOpen(false); setPathOpen(true); }}>
                    {A2A_TEXT.undoLast}
                  </button>
                )}
                {client.applied.canDisconnect && (
                  <button type="button" role="menuitem" className="is-destructive" onClick={() => { setMenuOpen(false); setPathOpen(true); }}>
                    {A2A_TEXT.disconnectGateway}
                  </button>
                )}
              </div>
            )}
          </div>
        </div>
      </div>

      <div className="a2a-tabs" role="tablist" aria-label="客户端管理分类">
        {A2A_TAB_KEYS.map((key) => (
          <button
            key={key}
            type="button"
            role="tab"
            aria-selected={tab === key}
            className={`a2a-tab${tab === key ? ' is-active' : ''}`}
            onClick={() => onTabChange(key)}
          >
            {A2A_TEXT.tabs[key]}
          </button>
        ))}
      </div>

      {tab !== 'config' ? (
        <ResourceTabPlaceholder tab={tab} client={client} />
      ) : (
        <>
          {!client.detected && (
            <p className="a2a-detection-hint">
              {A2A_TEXT.notDetected}
              {client.writable ? ` · ${A2A_TEXT.notDetectedWritable}` : ''}
            </p>
          )}
          {!client.writable && !client.readError && (
            <div className="a2a-alert a2a-alert-warning">{client.supportNote}</div>
          )}
          <div className="a2a-configurations" aria-label={A2A_TEXT.panelConfig}>
            {client.readError && (
              <div className="a2a-alert a2a-alert-destructive">
                <div className="a2a-alert-head">
                  <span className="a2a-alert-title">配置文件读取失败</span>
                  <div className="a2a-alert-actions">
                    <Button size="sm" variant="outline" disabled={busy} onClick={() => void loadPanel({ force: true })}>
                      {A2A_TEXT.redetect}
                    </Button>
                    <Button size="sm" variant="outline" disabled={busy} onClick={() => setPathOpen(true)}>
                      指定配置文件
                    </Button>
                  </div>
                </div>
                <div className="a2a-alert-detail">{client.readError}</div>
              </div>
            )}
            {client.storeError && <div className="a2a-alert a2a-alert-destructive">{client.storeError}</div>}
            {!client.readError && !client.storeError && !client.configurations.length && (
              <div className="a2a-config-empty">
                <div className="a2a-config-empty-title">{A2A_TEXT.noConfigs}</div>
                <p className="a2a-config-empty-sub">
                  {client.writable
                    ? canQuickConnect
                      ? '可以直接一键接入本网关，或手动添加其他服务配置'
                      : '手动新增一套配置后即可应用到客户端'
                    : A2A_TEXT.noConfigsHint}
                </p>
                {client.writable && (
                  <div className="a2a-config-empty-actions">
                    {canQuickConnect && (
                      <Button size="sm" variant="primary" disabled={busy} onClick={() => setQuickOpen(true)}>
                        <BoltIcon />
                        {A2A_TEXT.quickConnectGateway}
                      </Button>
                    )}
                    <Button size="sm" variant={canQuickConnect ? 'outline' : 'primary'} disabled={busy} onClick={() => setCreating(true)}>
                      <Plus size={14} />
                      {A2A_TEXT.addConfigManual}
                    </Button>
                  </div>
                )}
              </div>
            )}
            {client.configurations.map((profile) => (
              <ConfigRow key={`${profile.liveId || 'saved'}:${profile.id}`} client={client} profile={profile} />
            ))}
          </div>

          {client.applied.error && <div className="a2a-alert a2a-alert-warning">{client.applied.error}</div>}
          {client.applied.pendingRecovery && (
            <div className="a2a-alert a2a-alert-warning">
              <span>上次应用未完整结束，请先查看恢复预览。</span>
              <Button size="sm" variant="outline" disabled={busy} onClick={() => setPathOpen(true)}>
                {A2A_TEXT.undoLast}
              </Button>
            </div>
          )}

          <div className={`a2a-details${detailsOpen ? ' is-open' : ''}`}>
            <button
              type="button"
              className="a2a-details-trigger"
              onClick={() => setDetailsOpen((v) => !v)}
              aria-expanded={detailsOpen}
            >
              <span className="a2a-chevron">{detailsOpen ? <ChevronDown size={14} /> : <ChevronRight size={14} />}</span>
              {A2A_TEXT.configLocation}
            </button>
            {detailsOpen && (
              <div className="a2a-details-body">
                <div className="a2a-machine-note">
                  {A2A_TEXT.machineNote}
                  {store.hostName ? ` · ${store.hostName}` : ''}
                </div>
                <code className="a2a-resolved-path">{client.resolvedPath}</code>
                {client.evidence.length > 0 && (
                  <ul className="a2a-evidence">
                    {client.evidence.map((evidence, index) => (
                      <li key={index}>{evidence}</li>
                    ))}
                  </ul>
                )}
                <p className="a2a-details-note">{client.supportNote}</p>
                <p className="a2a-details-note">{A2A_TEXT.statusLoadNote}</p>
                {client.applied.modifiedExternally && (
                  <p className="a2a-details-note a2a-warning">{A2A_TEXT.externalModified}</p>
                )}
              </div>
            )}
          </div>
        </>
      )}

      {pathOpen && <A2aPathDialog client={client} onClose={() => setPathOpen(false)} />}
      {quickOpen && (
        <A2aQuickConnectDialog
          client={client}
          catalog={store.catalog}
          gatewayKeys={store.gatewayKeys}
          defaultModel={store.defaultModel}
          onClose={() => setQuickOpen(false)}
        />
      )}
      {creating && (
        <A2aProfileDialog
          client={client}
          catalog={store.catalog}
          profile={{
            id: `p-${client.id}-new`,
            name: '',
            kind: client.supportsGateway ? 'gateway' : 'relay',
            protocol: client.protocols[0] || '',
            baseUrl: '',
            model: '',
            provider: '',
            effort: '',
            enabled: true,
            default: false,
            note: '',
            models: [],
            claudeModels: { opus: '', sonnet: '', haiku: '', subagent: '' },
            authMode: 'gateway',
            apiKey: '',
            envKey: '',
            gatewayKeyId: '',
          }}
          creating
          onClose={() => setCreating(false)}
        />
      )}
    </>
  );
}

export function A2aAgentPanel(): JSX.Element {
  const store = useSyncExternalStore(subscribeA2aStore, getA2aStore, getA2aStore);
  const [tab, setTab] = useState<A2aTabKey>('config');
  const client = selectedClient();
  const list = useMemo(() => visibleClients(store), [store]);
  const stats = useMemo(() => counts(store), [store]);
  const detected = list.filter((item) => item.detected);
  const missing = list.filter((item) => !item.detected);
  const initialLoading = store.loading && store.clients.length === 0;
  const isDetecting = store.loading || store.refreshing;
  const detectedTime = formatDetectedTime(store.detectedAt);
  const statusHint = store.refreshing && !initialLoading
    ? A2A_TEXT.updating
    : detectedTime
      ? A2A_TEXT.detectedAt.replace('{time}', detectedTime)
      : '';

  useEffect(() => {
    void loadPanel({});
  }, []);

  return (
    <section className="a2a-panel">
      <div className="a2a-toolbar">
        <div className="a2a-search">
          <Search size={14} className="a2a-search-icon" />
          <input
            type="search"
            className="a2a-search-input"
            placeholder={A2A_TEXT.search}
            aria-label={A2A_TEXT.searchAria}
            value={store.query}
            autoComplete="off"
            onChange={(event) => setQuery(event.currentTarget.value)}
          />
        </div>
        <div className="a2a-filter" role="group" aria-label="按状态筛选">
          {A2A_FILTER_OPTIONS.map((option) => (
            <button
              key={option.key}
              type="button"
              aria-pressed={store.filter === option.key}
              className={`a2a-filter-item${store.filter === option.key ? ' is-active' : ''}`}
              onClick={() => setFilter(option.key)}
            >
              {option.label}
              {!initialLoading && (
                <span className="a2a-filter-count">
                  {option.key === 'all' ? stats.all : option.key === 'via' ? stats.via : option.key === 'direct' ? stats.direct : stats.missing}
                </span>
              )}
            </button>
          ))}
        </div>
        <div className="a2a-toolbar-right">
          {statusHint && <span className="a2a-toolbar-hint">{statusHint}</span>}
          <Button size="sm" variant="outline" disabled={isDetecting || Boolean(store.busy)} onClick={() => void loadPanel({ force: true })}>
            {isDetecting ? <RotateCw size={14} className="a2a-spin" /> : <RotateCw size={14} />}
            {isDetecting ? A2A_TEXT.detecting : A2A_TEXT.redetect}
          </Button>
        </div>
      </div>

      {store.error && (
        <div className="a2a-alert a2a-alert-destructive a2a-alert-row">
          <span className="a2a-alert-text">{store.error}</span>
          <Button size="sm" variant="outline" disabled={isDetecting || Boolean(store.busy)} onClick={() => void loadPanel({ force: true })}>
            {A2A_TEXT.redetect}
          </Button>
        </div>
      )}

      <div className="a2a-body">
        <aside className="a2a-rail" aria-label={A2A_TEXT.railClients}>
          {initialLoading ? (
            <div className="a2a-skeleton-list">
              {Array.from({ length: 8 }, (_, index) => (
                <div key={index} className="a2a-skeleton-row" aria-hidden="true">
                  <div className="a2a-skeleton a2a-skeleton-box" />
                  <div className="a2a-skeleton-lines">
                    <div className="a2a-skeleton a2a-skeleton-line" />
                    <div className="a2a-skeleton a2a-skeleton-line short" />
                  </div>
                  <div className="a2a-skeleton a2a-skeleton-dot" />
                </div>
              ))}
            </div>
          ) : (
            <>
              {detected.length > 0 && (
                <>
                  <div className="a2a-rail-label">{`${A2A_TEXT.railDetected}（${detected.length}）`}</div>
                  {detected.map((item) => <ClientRow key={item.id} client={item} active={item.id === store.selected} />)}
                </>
              )}
              {missing.length > 0 && (
                <>
                  <div className="a2a-rail-label">{`${A2A_TEXT.railMissing}（${missing.length}）`}</div>
                  {missing.map((item) => <ClientRow key={item.id} client={item} active={item.id === store.selected} />)}
                </>
              )}
              {!list.length && (
                <div className="a2a-rail-empty">
                  <span>{A2A_TEXT.noMatchClients}</span>
                  {(store.query || store.filter !== 'all') && (
                    <Button size="sm" variant="outline" onClick={clearFilters}>{A2A_TEXT.clearFilters}</Button>
                  )}
                </div>
              )}
            </>
          )}
        </aside>

        <div className="a2a-detail">
          {initialLoading ? (
            <div className="a2a-detail-center">
              <div className="a2a-spinner" />
              <p className="a2a-detail-loading">{A2A_TEXT.detailLoading}</p>
            </div>
          ) : store.clients.length > 0 && !list.length ? (
            <div className="a2a-detail-center">
              <div className="a2a-detail-empty-title">{A2A_TEXT.noMatchDetail}</div>
              <p className="a2a-detail-empty-sub">{A2A_TEXT.noMatchDetailHint}</p>
              <Button size="sm" variant="outline" onClick={clearFilters}>{A2A_TEXT.clearFilters}</Button>
            </div>
          ) : client ? (
            <ClientDetail key={client.id} client={client} tab={tab} onTabChange={setTab} />
          ) : (
            <div className="a2a-detail-center">
              <div className="a2a-detail-empty-title">{A2A_TEXT.noClientData}</div>
              <Button size="sm" variant="outline" disabled={isDetecting || Boolean(store.busy)} onClick={() => void loadPanel({ force: true })}>
                {A2A_TEXT.redetect}
              </Button>
            </div>
          )}
        </div>
      </div>
    </section>
  );
}
