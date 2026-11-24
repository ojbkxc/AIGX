/**
 * 账号行（对齐 Agent2API v3.5.1 `RoutingAccountRow`，见 canvas-controls.tsx）。
 *
 * 一行 = 把手 + 序号 + 首字母头像 + 名字/说明 + 状态 + 参与勾选 + 更多（上下移）。
 * A2A 行尾还有「账号设置」按钮（打开该账号的提供商路由设置弹窗）与
 * 「全局账号启停」开关；AIGX 的路由页没有账号设置入口（账号在账号页维护），
 * 故这两项不渲染 —— 结构位保留在 `RoutingAccountRowProps` 的注释里，接入时补。
 *
 * 数据缺口：AIGX 没有账号目录（名字/启用态/在途/积分到期都取不到），
 * 传进来只有 id —— 名字位置直接显示 id，状态位在没有 `state` 时**不渲染**
 * （渲染「未就绪」等于编造一个后端没给的状态）。
 */
import type React from 'react';
import { useTranslation } from 'react-i18next';
import { ArrowDown, ArrowUp } from 'lucide-react';
import { RoutingActions, routingOrderKey } from './routing-actions';
import { RoutingStateText } from './routing-state-text';

export interface RoutingAccountRef {
  id: string;
  enabled?: boolean;
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
}: RoutingAccountRowProps): JSX.Element {
  const { t } = useTranslation();
  const name = account ? account.id : id;
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