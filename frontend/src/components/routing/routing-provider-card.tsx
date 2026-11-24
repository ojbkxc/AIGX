/**
 * 提供商卡片（对齐 Agent2API v3.5.1 `routing-provider-card.tsx` 的卡片面）。
 *
 * A2A 的提供商卡是画布节点：把手 · 序号 · 标记 · 名称 + 「N / M 个账号参与」
 * · 参与开关 · 分组设置 · 更多（启用/禁用全部、提高/降低优先级）· 展开后的账号行。
 *
 * AIGX 侧这张卡承载的是**档案参与名单**（`RoutingScope.providers` /
 * `disabled_providers`）—— 名单里只有提供商 id，没有账号目录，所以：
 * - 摘要位置显示「本层参与 / 本层排除」，不编造账号数；
 * - 展开体不渲染账号行（账号顺序在名单区下方按 id 维护），给一句白话说明；
 * - 「分组设置」「启用/禁用全部账号」依赖账号目录，不渲染。
 */
import { useTranslation } from 'react-i18next';
import { ArrowDown, ArrowUp, Search } from 'lucide-react';
import { RoutingActions, RoutingProviderMark } from './routing-actions';

export function RoutingProviderCard({
  id,
  rank,
  included,
  first,
  last,
  locked,
  open,
  onOpen,
  onJoin,
  onMove,
}: {
  id: string;
  /** 参与中的序号（排除的传 -1 → 显示「—」） */
  rank: number;
  included: boolean;
  first: boolean;
  last: boolean;
  locked: boolean;
  open: boolean;
  onOpen(open: boolean): void;
  onJoin(on: boolean): void;
  onMove(direction: number): void;
}): JSX.Element {
  const { t } = useTranslation();
  return (
    <section
      data-route-node={id}
      data-route-connected={included ? '' : undefined}
      data-route-expanded={open ? '' : undefined}
      className={`rt-config-provider${included ? '' : ' is-off'}`}
    >
      <header>
        <span className="rt-config-rank">{included ? String(rank + 1).padStart(2, '0') : '—'}</span>
        <RoutingProviderMark label={id} />
        <button
          type="button"
          className="rt-provider-toggle"
          aria-expanded={open}
          onClick={() => onOpen(!open)}
        >
          <span className="rt-provider-name">
            <b title={id}>{id}</b>
            <small>{included ? t('本层参与') : t('本层排除')}</small>
          </span>
          <span className="rt-chevron" aria-hidden="true">⌄</span>
        </button>
        <label className="rt-switch" title={included ? t('移出本路由') : t('加入路由')}>
          <input
            type="checkbox"
            checked={included}
            disabled={locked}
            aria-label={included ? t('{{provider}} 参与本路由', { provider: id }) : t('{{provider}} 加入路由', { provider: id })}
            onChange={(event) => onJoin(event.currentTarget.checked)}
          />
        </label>
        <RoutingActions
          label={t('提供商路由设置')}
          actions={[
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
      {open ? (
        <div className="rt-config-accounts">
          <p className="rt-config-note">
            {t('账号明细需账号目录（本期未接入）：参与账号与顺序在下方「账号优先顺序 / 排除的账号」按账号 id 维护。')}
          </p>
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
  onOpen,
  onJoin,
  onMove,
}: {
  included: string[];
  excluded: string[];
  locked: boolean;
  query: string;
  opened: Record<string, boolean>;
  onOpen(id: string, open: boolean): void;
  onJoin(id: string, on: boolean): void;
  onMove(id: string, direction: number): void;
}): JSX.Element {
  const { t } = useTranslation();
  const visible = (id: string): boolean => !query || id.toLowerCase().includes(query);
  const activeVisible = included.filter(visible);
  const excludedVisible = excluded.filter(visible);
  return (
    <div className="rt-config-listcol">
      <div className="rt-config-participants" data-route-list>
        {activeVisible.map((id) => (
          <RoutingProviderCard
            key={id}
            id={id}
            rank={included.indexOf(id)}
            included
            first={included[0] === id}
            last={included[included.length - 1] === id}
            locked={locked}
            open={Boolean(opened[id])}
            onOpen={(open) => onOpen(id, open)}
            onJoin={(on) => onJoin(id, on)}
            onMove={(direction) => onMove(id, direction)}
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
                rank={-1}
                included={false}
                first={false}
                last={false}
                locked={locked}
                open={Boolean(opened[id])}
                onOpen={(open) => onOpen(id, open)}
                onJoin={(on) => onJoin(id, on)}
                onMove={() => undefined}
              />
            ))}
          </div>
        </div>
      ) : null}
      {query && !activeVisible.length && !excludedVisible.length ? (
        <div className="rt-state-note rt-state-inherit">{t('没有匹配的提供商')}</div>
      ) : null}
    </div>
  );
}