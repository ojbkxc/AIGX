/**
 * 提供商卡片（对齐 Agent2API v3.5.1 `routing-provider-card.tsx` 的卡片面）。
 *
 * A2A 的提供商卡是画布节点：把手 · 序号 · 标记 · 名称 + 「N / M 个账号参与」
 * · 参与开关 · 分组设置 · 更多（启用/禁用全部、提高/降低优先级）
 * · 消耗顺序行 · 展开后的账号行（含单账号设置入口）。
 *
 * AIGX 侧这张卡承载档案参与名单（`RoutingScope.providers` /
 * `disabled_providers`），账号目录由 `routing-account-directory.ts`
 * （/api/account-sessions 投影）提供：
 * - 有目录时摘要显示「N / M 个账号参与」，展开体渲染账号行
 *   （名称 / 启用态 / 在途数 / 参与勾选 / 单账号设置入口）；
 * - 账号勾选改的是本层草稿 `excluded_accounts`（A2A 非全局分支同语义）；
 *   「启用/禁用全部账号」是**全局启停**，走 /api/account-sessions 立即保存
 *   （A2A 全局分支同语义），与档案草稿互不改写；
 * - 目录缺失时不编造账号行，展开体给一句白话说明。
 */
import { useTranslation } from 'react-i18next';
import { ArrowDown, ArrowUp, Search, Settings2 } from 'lucide-react';
import { RoutingActions, RoutingProviderMark } from './routing-actions';
import { RoutingAccountRow, type RoutingAccountRef } from './routing-account-row';
import { RoutingConsumptionOrder } from './routing-account-order';
import type { AccountProviderItem, AccountSortRule, AccountSortSettings } from '../../types';

export interface RoutingProviderCardProps {
  id: string;
  /** 提供商展示名（缺省 = id；A2A provider.label） */
  label?: string;
  /** 参与中的序号（排除的传 -1 → 显示「—」） */
  rank: number;
  included: boolean;
  first: boolean;
  last: boolean;
  locked: boolean;
  open: boolean;
  /** 搜索关键字（小写）；命中账号时卡片常开 */
  query?: string;
  /** 该提供商的账号目录（空 = 目录缺失，不渲染账号行） */
  accounts?: RoutingAccountRef[];
  /** 本层账号顺序 / 排除名单（A2A `profile.accountOrder` / `excludedAccounts`） */
  accountOrder?: string[];
  excludedAccounts?: string[];
  /** 消耗规则表 + 余额排序状态（A5） */
  sorting?: AccountSortSettings | null;
  balanceSortStates?: Record<string, string>;
  onSaveSorting?(provider: string, rules: AccountSortRule[]): Promise<void>;
  onOpen(open: boolean): void;
  onJoin(on: boolean): void;
  onMove(direction: number): void;
  onToggleAccount?(id: string): void;
  onMoveAccount?(id: string, direction: number): void;
  /** 单账号设置入口（A7） */
  onAccount?(id: string): void;
  /** 分组设置（完整消耗规则弹窗） */
  onSort?(): void;
  /** 全局启停本组账号（A6；立即保存，不改草稿） */
  onEnableGroup?(enabled: boolean): void;
  /** 目录操作进行中（禁用启停类按钮） */
  controlBusy?: boolean;
}

export function RoutingProviderCard({
  id,
  label,
  rank,
  included,
  first,
  last,
  locked,
  open,
  query = '',
  accounts = [],
  accountOrder = [],
  excludedAccounts = [],
  sorting,
  balanceSortStates,
  onSaveSorting,
  onOpen,
  onJoin,
  onMove,
  onToggleAccount,
  onMoveAccount,
  onAccount,
  onSort,
  onEnableGroup,
  controlBusy = false,
}: RoutingProviderCardProps): JSX.Element {
  const { t } = useTranslation();
  const name = label || id;
  /** A2A：profile.accountOrder 里已排的在前，其余账号按目录顺序补后 */
  const order = [...new Set([...accountOrder, ...accounts.map((account) => account.id)])]
    .filter((key) => accounts.some((account) => account.id === key));
  const joined = (key: string): boolean => !excludedAccounts.includes(key);
  const participants = order.filter(joined);
  const enabledCount = accounts.filter((account) => account.enabled !== false).length;
  const summary = accounts.length
    ? t('{{selected}} / {{total}} 个账号参与', { selected: participants.length, total: accounts.length })
    : included ? t('本层参与') : t('本层排除');
  /** 搜索命中账号名/id 时该卡常开，且只留命中的账号行（A2A 同口径） */
  const matches = (account: RoutingAccountRef): boolean =>
    !query
    || name.toLowerCase().includes(query)
    || id.toLowerCase().includes(query)
    || (account.name || account.id).toLowerCase().includes(query)
    || account.id.toLowerCase().includes(query);
  const expanded = open || Boolean(query);
  return (
    <section
      data-route-node={id}
      data-route-connected={included ? '' : undefined}
      data-route-expanded={expanded ? '' : undefined}
      className={`rt-config-provider${included ? '' : ' is-off'}`}
    >
      <header>
        <span className="rt-config-rank">{included ? String(rank + 1).padStart(2, '0') : '—'}</span>
        <RoutingProviderMark label={name} />
        <button
          type="button"
          className="rt-provider-toggle"
          aria-expanded={expanded}
          onClick={() => onOpen(!open)}
        >
          <span className="rt-provider-name">
            <b title={name}>{name}</b>
            <small>{summary}</small>
          </span>
          <span className="rt-chevron" aria-hidden="true">⌄</span>
        </button>
        <label className="rt-switch" title={included ? t('移出本路由') : t('加入路由')}>
          <input
            type="checkbox"
            checked={included}
            disabled={locked}
            aria-label={included ? t('{{provider}} 参与本路由', { provider: name }) : t('{{provider}} 加入路由', { provider: name })}
            onChange={(event) => onJoin(event.currentTarget.checked)}
          />
        </label>
        {onSort ? (
          <button
            type="button"
            className="rt-link-btn rt-group-settings"
            title={t('分组设置')}
            aria-label={t('{{name}} · 分组设置', { name })}
            onClick={onSort}
          >
            <Settings2 size={14} aria-hidden="true" />
          </button>
        ) : null}
        <RoutingActions
          label={t('提供商路由设置')}
          actions={[
            ...(onEnableGroup ? [
              {
                label: t('启用全部账号'),
                disabled: locked || controlBusy || !accounts.length || enabledCount === accounts.length,
                onClick: () => onEnableGroup(true),
              },
              {
                label: t('禁用全部账号'),
                disabled: locked || controlBusy || !enabledCount,
                onClick: () => onEnableGroup(false),
              },
            ] : []),
            {
              label: t('提高优先级'),
              icon: <ArrowUp size={14} aria-hidden="true" />,
              disabled: locked || !included || first,
              onClick: () => onMove(-1),
            },
            {
              label: t('降低优先级'),
              icon: <ArrowDown size={14} aria-hidden="true" />,
              disabled: locked || !included || last,
              onClick: () => onMove(1),
            },
            {
              label: included ? t('移出本路由') : t('加入路由'),
              disabled: locked,
              onClick: () => onJoin(!included),
            },
          ]}
        />
      </header>
      <RoutingConsumptionOrder
        provider={id}
        sorting={sorting}
        balanceSortStates={balanceSortStates}
        independent={false}
        disabled={controlBusy}
        onSettings={onSort}
        onSaveSorting={onSaveSorting}
      />
      {expanded ? (
        <div className="rt-config-accounts">
          {accounts.length ? (
            <>
              {participants.length ? (
                <div data-route-list>
                  {order.filter((key) => joined(key) && matches(accounts.find((account) => account.id === key) as RoutingAccountRef)).map((key) => {
                    const account = accounts.find((item) => item.id === key) as RoutingAccountRef;
                    const accountIndex = participants.indexOf(key);
                    return (
                      <RoutingAccountRow
                        key={key}
                        id={key}
                        account={account}
                        rank={accountIndex + 1}
                        joined
                        state={account.enabled === false ? 'disabled' : 'enabled'}
                        note={account.inFlight != null
                          ? t('在途 {{n}} / {{limit}}', { n: account.inFlight, limit: account.maxConcurrency || '∞' })
                          : t('参与本路由')}
                        locked={locked || !included}
                        orderLocked={locked || !included}
                        first={accountIndex === 0}
                        last={accountIndex === participants.length - 1}
                        onMove={(direction) => onMoveAccount?.(key, direction)}
                        onToggle={() => onToggleAccount?.(key)}
                        onAccount={onAccount ? () => onAccount(key) : undefined}
                      />
                    );
                  })}
                </div>
              ) : null}
              {order.some((key) => !joined(key)) ? (
                <>
                  <p className="rt-account-section">{t('未参与本路由')}</p>
                  <div data-route-list>
                    {order.filter((key) => !joined(key) && matches(accounts.find((account) => account.id === key) as RoutingAccountRef)).map((key) => {
                      const account = accounts.find((item) => item.id === key) as RoutingAccountRef;
                      return (
                        <RoutingAccountRow
                          key={key}
                          id={key}
                          account={account}
                          rank={-1}
                          joined={false}
                          state={account.enabled === false ? 'disabled' : 'enabled'}
                          note={t('未参与本路由')}
                          locked={locked}
                          orderLocked
                          first={false}
                          last={false}
                          onMove={() => undefined}
                          onToggle={() => onToggleAccount?.(key)}
                          onAccount={onAccount ? () => onAccount(key) : undefined}
                        />
                      );
                    })}
                  </div>
                </>
              ) : null}
            </>
          ) : (
            <p className="rt-config-note">
              {t('该提供商下没有读到账号；账号明细需要账号目录（/api/account-sessions），也可在下方按账号 id 维护参与与顺序。')}
            </p>
          )}
        </div>
      ) : null}
    </section>
  );
}

/** 参与 / 排除两段的卡片列表（A2A 折叠区的等价形态）。 */
export function RoutingProviderList({
  included,
  excluded,
  locked,
  query,
  opened,
  accounts = [],
  accountOrder = [],
  excludedAccounts = [],
  sorting,
  balanceSortStates,
  onSaveSorting,
  onOpen,
  onJoin,
  onMove,
  onToggleAccount,
  onMoveAccount,
  onAccount,
  onSort,
  onEnableGroup,
  controlBusy,
  filterModel = '',
}: {
  included: string[];
  excluded: string[];
  locked: boolean;
  query: string;
  opened: Record<string, boolean>;
  /** 全量账号目录（本组件按 provider 分发） */
  accounts?: RoutingAccountRef[];
  accountOrder?: string[];
  excludedAccounts?: string[];
  sorting?: AccountSortSettings | null;
  balanceSortStates?: Record<string, string>;
  onSaveSorting?(provider: string, rules: AccountSortRule[]): Promise<void>;
  onOpen(id: string, open: boolean): void;
  onJoin(id: string, on: boolean): void;
  onMove(id: string, direction: number): void;
  onToggleAccount?(id: string): void;
  onMoveAccount?(provider: string, id: string, direction: number): void;
  onAccount?(id: string): void;
  onSort?(provider: string): void;
  onEnableGroup?(provider: string, enabled: boolean): void;
  controlBusy?: boolean;
  /** 模型筛选（A3）：只筛展示，不改配置；空串 = 全部 */
  filterModel?: string;
}): JSX.Element {
  const { t } = useTranslation();
  /** 账号是否声明了筛选模型：未声明 models 的账号不因此隐藏（不编造「不支持」） */
  const supportsModel = (account: RoutingAccountRef): boolean => {
    if (!filterModel) return true;
    const models = account.models;
    if (!models || !models.length) return true;
    return models.some((model) => model.toLowerCase() === filterModel.toLowerCase());
  };
  const accountsOf = (id: string): RoutingAccountRef[] =>
    accounts.filter((account) => account.provider === id);
  const visible = (id: string): boolean => {
    if (filterModel) {
      const list = accountsOf(id);
      if (list.length && !list.some(supportsModel)) return false;
    }
    return !query
      || id.toLowerCase().includes(query)
      || accountsOf(id).some((account) => (account.name || account.id).toLowerCase().includes(query));
  };
  const activeVisible = included.filter(visible);
  const excludedVisible = excluded.filter(visible);
  const cardProps = {
    locked,
    query,
    accountOrder,
    excludedAccounts,
    sorting,
    balanceSortStates,
    onSaveSorting,
    onToggleAccount,
    onAccount,
    controlBusy,
  };
  return (
    <div className="rt-config-listcol">
      <div className="rt-config-participants" data-route-list>
        {activeVisible.map((id) => (
          <RoutingProviderCard
            key={id}
            id={id}
            label={accountsOf(id)[0]?.providerLabel}
            rank={included.indexOf(id)}
            included
            first={included[0] === id}
            last={included[included.length - 1] === id}
            open={Boolean(opened[id])}
            accounts={accountsOf(id).filter(supportsModel)}
            onOpen={(open) => onOpen(id, open)}
            onJoin={(on) => onJoin(id, on)}
            onMove={(direction) => onMove(id, direction)}
            onSort={onSort ? () => onSort(id) : undefined}
            onEnableGroup={onEnableGroup ? (enabled) => onEnableGroup(id, enabled) : undefined}
            onMoveAccount={onMoveAccount ? (accountId, direction) => onMoveAccount(id, accountId, direction) : undefined}
            {...cardProps}
          />
        ))}
      </div>
      {!included.length ? (
        <div className="rt-state-note rt-state-inherit">
          <b>{t('还没有参与的提供商')}</b>
          <span>{t('「全部提供商」档下新出现的提供商自动参与；「仅加入的参与」档下需要手动加入。')}</span>
        </div>
      ) : null}
      {excludedVisible.length ? (
        <div className="rt-config-excluded">
          <div className="rt-excluded-trigger">
            <Search size={12} aria-hidden="true" />
            {t('排除的提供商')} · {excludedVisible.length}
          </div>
          <div>
            {excludedVisible.map((id) => (
              <RoutingProviderCard
                key={id}
                id={id}
                label={accountsOf(id)[0]?.providerLabel}
                rank={-1}
                included={false}
                first={false}
                last={false}
                open={Boolean(opened[id])}
                accounts={accountsOf(id).filter(supportsModel)}
                onOpen={(open) => onOpen(id, open)}
                onJoin={(on) => onJoin(id, on)}
                onMove={() => undefined}
                onSort={onSort ? () => onSort(id) : undefined}
                onEnableGroup={onEnableGroup ? (enabled) => onEnableGroup(id, enabled) : undefined}
                onMoveAccount={onMoveAccount ? (accountId, direction) => onMoveAccount(id, accountId, direction) : undefined}
                {...cardProps}
              />
            ))}
          </div>
        </div>
      ) : null}
      {query && !activeVisible.length && !excludedVisible.length ? (
        <div className="rt-state-note rt-state-inherit">{t('没有匹配的提供商或账号')}</div>
      ) : null}
      {filterModel && !activeVisible.length && !excludedVisible.length ? (
        <div className="rt-state-note rt-state-inherit">
          {t('当前模型没有对应的账号，请先添加账号或检查模型绑定。')}
        </div>
      ) : null}
    </div>
  );
}

/** 供调用方构造 AccountSortDialog 的 providers 入参（目录缺失时给 id 兜底行） */
export function providerItemOf(id: string, label: string | undefined): AccountProviderItem {
  return { kind: id, label: label || id, protocol: '', default_base_url: '' };
}
