/**
 * RoutingCanvas —— 路由配置的画布外壳（对齐 A2A v3.5.1 的 `routing-canvas-layout.tsx`）。
 *
 * 三层，逐层对应：
 *   · `RoutingConfigShell`  → A2A `RoutingConfigShell`：点阵底 + 连线层容器。
 *                             `plain` = 特殊请求路由/错误处理：去点阵、不画连线。
 *   · `RoutingCanvasLayout` → A2A `RoutingCanvasLayout`：左列（配置/源卡）+ 右列（节点）
 *                             + 底行三句图例（`grid-column: 1/-1`）。
 *   · `RoutingCanvasGroups` → A2A 的 `groups` 列内容（它那边是 provider 卡 + 账号行，
 *                             本仓映射为「路由组卡 + 成员槽位行」）。
 *
 * 一栏还是两栏、连不连线，都交给 CSS（`RoutingCanvas.css`）与
 * `RoutingCanvasEdges`（实测位置），这里只落结构。
 */
import { useRef } from 'react';
import { Info, Pencil, Route as RouteIcon } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import RoutingCanvasEdges from './RoutingCanvasEdges';
import type { RoutingGroup } from '../../types/routing-profile';
import './RoutingCanvas.css';

/**
 * 画布外壳：点阵底 + SVG 连线层。
 *
 * `plain`（特殊请求路由 / 错误处理）时**不挂连线层**——A2A 是靠「没有
 * data-route-source 就一条都画不出」实现的，本仓显式不渲染，省掉空 SVG 节点。
 */
export function RoutingConfigShell({
  plain = false,
  children,
}: {
  plain?: boolean;
  children: React.ReactNode;
}): JSX.Element {
  const root = useRef<HTMLDivElement>(null);
  return (
    <div className={`routing-config-shell${plain ? ' is-plain' : ''}`} ref={root}>
      {plain ? null : <RoutingCanvasEdges root={root} />}
      {children}
    </div>
  );
}

/**
 * 画布布局：左列配置、右列节点、整行图例。
 *
 * 图例三句照 A2A `routing-canvas-layout.tsx:31-33`：
 *   `<i/>` + 连线表示参与本路由 ｜ `<svg width=22 height=8><path d='M 0 4 H 22'/></svg>` +
 *   流动：有请求在途 ｜ 保存后才会影响真实请求
 */
export function RoutingCanvasLayout({
  title,
  note,
  children,
  groups,
}: {
  title: string;
  note: string;
  children: React.ReactNode;
  groups: React.ReactNode;
}): JSX.Element {
  const { t } = useTranslation();
  return (
    <div className="routing-canvas">
      <section className="routing-config-column">
        <div className="routing-column-head">
          <RouteIcon size={14} aria-hidden="true" />
          <b>{t('路由配置')}</b>
          <span>{title}</span>
        </div>
        {children}
        <p className="routing-canvas-note">
          <Info size={13} aria-hidden="true" />
          <span>{note}</span>
        </p>
      </section>
      {groups}
      <div className="routing-canvas-legend">
        <span>
          <i />
          {t('连线表示参与本路由')}
        </span>
        <span>
          <svg width={22} height={8} aria-hidden="true">
            <path d="M 0 4 H 22" />
          </svg>
          {t('流动：有请求在途')}
        </span>
        <span>{t('保存后才会影响真实请求')}</span>
      </div>
    </div>
  );
}

/** 成员槽位行在画布上的稳定键（组 id + 序号，序号即成员真实下标） */
function memberKey(groupId: string, index: number): string {
  return `${groupId}:${index}`;
}

/**
 * 画布右列：路由组节点。
 *
 * 只做**投影 + 跳转**，不在画布里再放一份编辑器（组编辑仍在左列的 GroupEditor，
 * 点节点 = 选中该组）。与 A2A 一致：它的 provider 卡也是同一种投影，
 * 点进右栏面板才编辑。
 *
 * 连线判据（`data-route-connected`）由调用方算好传进来，本组件不猜：
 *   · 节点连通 = 当前层（或其引用链）真的用到了这个组
 *   · 成员行连通 = 该组连通 且 这个槽位 `included`
 */
export function RoutingCanvasGroups({
  groups,
  connectedIds,
  runningIds,
  activeGroupId,
  onOpen,
}: {
  groups: RoutingGroup[];
  connectedIds: ReadonlySet<string>;
  runningIds: ReadonlySet<string>;
  activeGroupId: string | null;
  onOpen: (id: string) => void;
}): JSX.Element {
  const { t } = useTranslation();
  return (
    <section className="routing-groups-column" data-route-scroll aria-label={t('路由组')}>
      <div className="routing-column-head">
        <b>{t('路由组')}</b>
        <span className="rt-count">{groups.length}</span>
      </div>
      <div className="routing-groups-list">
        {groups.length === 0 ? (
          <p className="routing-canvas-empty">
            {t('还没有路由组。没有组时请求只走 global 与模型级档案 —— 需要「某模型专走某批供应商/账号」时才建组。')}
          </p>
        ) : null}
        {groups.map((group) => {
          const connected = connectedIds.has(group.id);
          const running = runningIds.has(group.id);
          // 聚焦某个组时只展开它；未聚焦（全局/模型级）时全展开，
          // 让「这一层到底连到哪些槽位」一眼可数
          const expanded = activeGroupId ? group.id === activeGroupId : true;
          return (
            <article
              key={group.id}
              className="routing-canvas-node"
              data-route-node={group.id}
              data-route-connected={connected ? '' : undefined}
              data-route-running={running ? '' : undefined}
              data-route-expanded={expanded ? '' : undefined}
              aria-current={activeGroupId === group.id ? 'true' : undefined}
            >
              <header>
                <b title={group.id}>{group.name || group.id || t('（未命名 id）')}</b>
                <span>{t('{{n}} 个成员槽位', { n: group.members.length })}</span>
                {group.model ? <span>{`· ${group.model}`}</span> : null}
                <span className="right">
                  <button
                    type="button"
                    className="btn btn-secondary btn-sm"
                    onClick={() => onOpen(group.id)}
                    title={t('编辑此组')}
                  >
                    <Pencil size={12} aria-hidden="true" />
                  </button>
                </span>
              </header>
              {expanded
                ? group.members.map((member, index) => {
                    const slotConnected = connected && member.included;
                    return (
                      <div
                        key={memberKey(group.id, index)}
                        className="routing-canvas-account"
                        data-route-account={memberKey(group.id, index)}
                        data-route-connected={slotConnected ? '' : undefined}
                      >
                        <b title={member.provider || undefined}>{member.provider || '—'}</b>
                        <small>{member.model || '—'}</small>
                        <small>
                          {member.accounts.length
                            ? t('{{n}} 个账号', { n: member.accounts.length })
                            : '—'}
                        </small>
                        <small>{member.included ? t('参与') : t('未参与')}</small>
                      </div>
                    );
                  })
                : null}
              {expanded && group.members.length === 0 ? (
                <p className="routing-canvas-empty">{t('还没有参与模型')}</p>
              ) : null}
            </article>
          );
        })}
      </div>
    </section>
  );
}
