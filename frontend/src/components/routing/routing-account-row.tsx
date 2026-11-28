/**
 * 账号行（对齐 Agent2API v3.5.1 `RoutingAccountRow`，见 canvas-controls.tsx）。
 *
 * 一行 = 把手 + 序号 + 首字母头像 + 名字/说明 + 状态 + 参与勾选 + 更多（上下移）
 * + 「账号设置」入口（A2A 行尾的 settings 按钮：从路由页跳到该账号的设置）。
 *
 * 数据来自 `routing-account-directory.ts`（/api/account-sessions 投影）：
 * 有目录时名字位显示账号名、状态位显示启用/禁用；没有目录数据时名字位显示 id，
 * 状态位在没有 `state` 时**不渲染**（渲染「未就绪」等于编造后端没给的状态）。
 */
import type React from 'react';
import { useTranslation } from 'react-i18next';
import { ArrowDown, ArrowUp, Settings2 } from 'lucide-react';
import { RoutingActions, routingOrderKey } from './routing-actions';
import { RoutingStateText } from './routing-state-text';

export interface RoutingAccountRef {
  id: string;
  /** 展示名（缺省 = id） */
  name?: string;
  provider?: string;
  providerLabel?: string;
  enabled?: boolean;
  /** 在途连接数 / 并发上限（有值才进 note，缺省不编造） */
  inFlight?: number;
  maxConcurrency?: number;
  /** 账号声明的模型（空/缺 = 未声明，模型筛选不因此隐藏） */
  models?: string[];
}

export interface RoutingAccountRowProps {
  id: string;
  account?: RoutingAccountRef;
  rank: number;
  joined: boolean;
  /** 后端给的状态码；缺省 = 没有该账号的状态数据（不渲染状态位） */
  state?: string;
  stateText?: string;
  note: string;
  locked: boolean;
  orderLocked: boolean;
  orderHint?: string;
  first: boolean;
  last: boolean;
  dragging?: boolean;
  drop?: 'before' | 'after';
  onDrag?(event: React.PointerEvent): void;
  onMove(direction: number): void;
  onToggle(): void;
  /** 单账号设置入口（A2A 行尾 settings 按钮）；缺省 = 不渲染 */
  onAccount?(): void;
}

export function RoutingAccountRow({
  id,
  account,
  rank,
  joined,
  state,
  stateText,
  note,
  locked,
  orderLocked,
  orderHint,
  first,
  last,
  dragging,
  drop,
  onDrag,
  onToggle,
  onMove,
  onAccount,
}: RoutingAccountRowProps): JSX.Element {
  const { t } = useTranslation();
  const name = account?.name || account?.id || id;
  return (
    <div
      data-route-sort={joined ? id : undefined}
      data-route-account={id}
      data-route-connected={joined ? '' : undefined}
      className={`rt-config-account${joined ? '' : ' is-excluded'}${dragging ? ' is-dragging' : ''}${
        drop ? ` drop-${drop}` : ''
      }`}
    >
      <button
        type="button"
        className="rt-grip"
        disabled={orderLocked || !joined}
        title={orderHint || t('拖动或按 Alt + 方向键调整顺序')}
        aria-label={t('拖动调整账号顺序')}
        onPointerDown={onDrag}
        onKeyDown={(event) => routingOrderKey(event, onMove)}
      >
        ⠿
      </button>
      <span className="rt-account-rank">{joined ? rank : '—'}</span>
      <span className="rt-account-avatar" aria-hidden="true">
        {name.slice(0, 1).toUpperCase()}
      </span>
      <span className="rt-config-account-name">
        <b title={name}>{name}</b>
        {note ? <small title={note}>{note}</small> : null}
      </span>
      {state ? <RoutingStateText state={state}>{stateText}</RoutingStateText> : null}
      <label className="rt-account-toggle" title={joined ? t('移出本路由') : t('加入路由')}>
        <input
          type="checkbox"
          checked={joined}
          disabled={locked}
          aria-label={t('{{account}} 参与本路由', { account: name })}
          onChange={onToggle}
        />
      </label>
      {onAccount ? (
        <button
          type="button"
          className="rt-link-btn rt-account-settings"
          title={t('账号设置')}
          aria-label={t('{{name}} · 账号设置', { name })}
          onClick={onAccount}
        >
          <Settings2 size={14} aria-hidden="true" />
        </button>
      ) : null}
      <RoutingActions
        label={t('账号操作')}
        actions={[
          {
            label: t('提高优先级'),
            icon: <ArrowUp size={14} aria-hidden="true" />,
            disabled: orderLocked || !joined || first,
            onClick: () => onMove(-1),
          },
          {
            label: t('降低优先级'),
            icon: <ArrowDown size={14} aria-hidden="true" />,
            disabled: orderLocked || !joined || last,
            onClick: () => onMove(1),
          },
        ]}
      />
    </div>
  );
}