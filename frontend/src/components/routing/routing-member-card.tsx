/**
 * 成员卡片（对齐 Agent2API v3.5.1 `routing-member-card.tsx`）。
 *
 * 一个成员 = 组里的一条 (provider, model) 槽位。卡片结构照 A2A：
 *
 *   header: 把手 · 序号 · 提供商标记 · 名称/副标题(provider · model) · 参与开关 · 更多
 *   消耗顺序行
 *   展开体: 「N 个账号参与」+ 选择账号 · 参与账号行 · 空注记
 *
 * AIGX 差异（如实登记，不静默变形）：
 * - 成员没有独立 id，身份键 = `routeModelKey(member)`（provider + model）；
 * - 没有账号目录：`member.accounts` 是账号 id 列表，**留空 = 该槽位用该商家
 *   的全部账号**（后端口径）。A2A 的「N / M 个账号参与」需要 M，AIGX 拿不到
 *   M，故只给 N，并在空列表时给出该字段的白话含义；
 * - 「分组设置」按钮（A2A 打开 provider 级消耗排序）在 AIGX 路由侧没有
 *   对应能力，不渲染；只保留消耗顺序行里的说明与（可选）入口。
 */
import { useTranslation } from 'react-i18next';
import { ArrowDown, ArrowUp, Settings2, Users } from 'lucide-react';
import type { RoutingMember } from '../../types/routing-profile';
import type { useRoutingDrag } from './routing-drag';
import { RoutingAccountRow, type RoutingAccountRef } from './routing-account-row';
import { RoutingConsumptionOrder } from './routing-account-order';
import { DragHandle, RoutingActions, RoutingProviderMark, routingOrderKey } from './routing-actions';
import { routeModelKey } from './routing-group-form';
import type { AccountSortRule, AccountSortSettings } from '../../types';

export function RoutingMemberCard({
  member,
  label,
  name,
  index,
  first,
  last,
  open,
  locked,
  query,
  drag,
  accounts,
  sorting,
  balanceSortStates,
  onSaveSorting,
  onAccount,
  onOpen,
  onJoin,
  onMove,
  onAccounts,
  onToggleAccount,
  onMoveAccount,
  onSort,
}: {
  member: RoutingMember;
  /** 提供商展示名（缺省 = provider id） */
  label: string;
  /** 模型展示名（缺省 = model id） */
  name: string;
  index: number;
  first: boolean;
  last: boolean;
  open: boolean;
  locked: boolean;
  /** 搜索关键字（小写）：命中时卡片常开，且禁止排序（A2A 同口径） */
  query: string;
  drag: ReturnType<typeof useRoutingDrag>;
  /** 该商家的账号目录（来自 routing-account-directory；空 = 目录缺失） */
  accounts: RoutingAccountRef[];
  /** 消耗规则（A5：与提供商卡同一数据源） */
  sorting?: AccountSortSettings | null;
  balanceSortStates?: Record<string, string>;
  onSaveSorting?(provider: string, rules: AccountSortRule[]): Promise<void>;
  /** 单账号设置入口（A7） */
  onAccount?(id: string): void;
  onOpen(open: boolean): void;
  onJoin(on: boolean): void;
  onMove(direction: number): void;
  onAccounts(): void;
  onToggleAccount(id: string): void;
  onMoveAccount(id: string, direction: number): void;
  onSort?(): void;
}): JSX.Element {
  const { t } = useTranslation();
  const key = routeModelKey(member);
  const searchingMember = `${label} ${name} ${member.model}`.toLowerCase().includes(query);
  const orderLocked = locked || Boolean(query);
  const accountDrag = drag.state?.kind === 'account' && drag.state.provider === key ? drag.state : null;
  const matchedAccounts = accounts.filter((account) => member.accounts.includes(account.id));
  /** 搜索时只留命中的账号行（成员自身命中则全留）——A2A 同口径 */
  const visibleAccounts = !query || searchingMember
    ? member.accounts
    : member.accounts.filter((id) => id.toLowerCase().includes(query));
  return (
    <section
      data-route-sort={member.included ? key : undefined}
      data-route-node={key}
      data-route-connected={member.included ? '' : undefined}
      data-route-expanded={open ? '' : undefined}
      className={`rt-config-provider${member.included ? '' : ' is-off'}${
        drag.state?.id === key ? ' is-dragging' : ''
      }${drag.state?.target === key ? (drag.state.after ? ' drop-after' : ' drop-before') : ''}`}
    >
      <header>
        <DragHandle
          disabled={orderLocked || !member.included}
          ariaLabel={t('调整模型顺序')}
          onPointerDown={(event) => drag.start(event, 'member', key, key)}
          onKeyDown={(event) => routingOrderKey(event, onMove)}
        />
        <span className="rt-config-rank">{member.included ? String(index + 1).padStart(2, '0') : '—'}</span>
        <RoutingProviderMark label={label} />
        <button
          type="button"
          className="rt-provider-toggle"
          aria-expanded={open}
          onClick={() => onOpen(!open)}
        >
          <span className="rt-provider-name">
            <b title={name}>{name}</b>
            <small title={`${label} · ${member.model}`}>{label} · {member.model}</small>
          </span>
          <span className="rt-chevron" aria-hidden="true">⌄</span>
        </button>
        <label className="rt-switch" title={member.included ? t('移出本路由') : t('加入路由')}>
          <input
            type="checkbox"
            checked={member.included}
            disabled={locked}
            aria-label={t('{{model}} 参与本路由', { model: name })}
            onChange={(event) => onJoin(event.currentTarget.checked)}
          />
        </label>
        {onSort ? (
          <button
            type="button"
            className="rt-link-btn rt-group-settings"
            title={t('分组设置')}
            aria-label={t('{{name}} · 分组设置', { name: label })}
            onClick={onSort}
          >
            <Settings2 size={14} aria-hidden="true" />
          </button>
        ) : null}
        <RoutingActions
          label={t('路由成员操作')}
          actions={[
            {
              label: t('提高优先级'),
              icon: <ArrowUp size={14} aria-hidden="true" />,
              disabled: orderLocked || !member.included || first,
              onClick: () => onMove(-1),
            },
            {
              label: t('降低优先级'),
              icon: <ArrowDown size={14} aria-hidden="true" />,
              disabled: orderLocked || !member.included || last,
              onClick: () => onMove(1),
            },
            {
              label: t('参与账号'),
              icon: <Users size={14} aria-hidden="true" />,
              disabled: locked,
              onClick: onAccounts,
            },
          ]}
        />
      </header>
      <RoutingConsumptionOrder
        provider={member.provider}
        sorting={sorting}
        balanceSortStates={balanceSortStates}
        independent={Boolean(query) || locked}
        disabled={locked}
        onSettings={onSort}
        onSaveSorting={onSaveSorting}
      />
      {open ? (
        <div className="rt-config-accounts">
          <div className="rt-member-summary">
            <span>
              {accounts.length
                ? t('{{selected}} / {{total}} 个账号参与', { selected: matchedAccounts.length, total: accounts.length })
                : t('{{n}} 个账号参与', { n: member.accounts.length })}
            </span>
            <button type="button" className="rt-link-btn" disabled={locked} onClick={onAccounts}>
              {t('选择账号')}
            </button>
          </div>
          {member.accounts.length ? (
            <div data-route-list>
              {visibleAccounts.map((id) => {
                const info = accounts.find((account) => account.id === id);
                return (
                <RoutingAccountRow
                  key={id}
                  id={id}
                  account={info ?? { id }}
                  rank={member.accounts.indexOf(id) + 1}
                  joined
                  state={info ? (info.enabled === false ? 'disabled' : 'enabled') : undefined}
                  note={info?.inFlight != null
                    ? t('在途 {{n}} / {{limit}}', { n: info.inFlight, limit: info.maxConcurrency || '∞' })
                    : t('参与本路由')}
                  locked={locked || !member.included}
                  orderLocked={orderLocked || !member.included}
                  first={member.accounts.indexOf(id) === 0}
                  last={member.accounts.indexOf(id) === member.accounts.length - 1}
                  dragging={accountDrag?.id === id}
                  drop={accountDrag?.target === id ? (accountDrag.after ? 'after' : 'before') : undefined}
                  onDrag={(event) => drag.start(event, 'account', key, id)}
                  onMove={(direction) => onMoveAccount(id, direction)}
                  onToggle={() => onToggleAccount(id)}
                  onAccount={onAccount ? () => onAccount(id) : undefined}
                />
                );
              })}
            </div>
          ) : (
            <p className="rt-config-note">
              {t('未点名账号 = 该槽位使用此提供商下的全部账号；点「选择账号」可逐个加入 / 移出。')}
            </p>
          )}
        </div>
      ) : null}
    </section>
  );
}