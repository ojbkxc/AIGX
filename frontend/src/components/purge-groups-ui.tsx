import { createContext, memo, useCallback, useContext, useMemo, useRef } from 'react';
import type { ReactNode } from 'react';
import type { TFunction } from 'i18next';
import type { PurgeGroupId, PurgeGroupMeta } from './purge-sections';
import {
  FORCED_COLLAPSE_WHEN_OFF,
  PURGE_GROUPS,
  groupForAnchor,
  isGroupOpen,
} from './purge-sections';
import './purge-groups-ui.css';

const ALL_GROUP_IDS: PurgeGroupId[] = PURGE_GROUPS.map((group) => group.id);

/**
 * 可折叠分组（受控）。
 *
 * 为什么是受控而不是自己存 state：折叠状态要跨刷新记住、要能一键全展开、
 * 还要被「体检告警跳转到某字段」强行打开——三件事都在父组件里，组件自己存
 * 一份就成了第二真相源（跳过去时明明展开着，组件内部却以为是收起的）。
 */
export interface PurgeSectionProps {
  meta: PurgeGroupMeta;
  open: boolean;
  onToggle: (id: PurgeGroupId) => void;
  /** 折叠头右侧的「N 项已启用」等摘要（父组件按数据面语义算好） */
  status?: ReactNode;
  /** 摘要是否点亮（>0 项已启用） */
  statusActive?: boolean;
  /** 标题行右侧的动作（按钮组）；折叠时不渲染，避免「看得见点不着」 */
  actions?: ReactNode;
  /** 总开关关闭时的灰化（true = 灰化） */
  mutedWhenOff?: boolean;
  children: ReactNode;
  t: TFunction;
}

/**
 * 折叠头是**整行可点**的 <button>：键盘 Tab 一次就到、回车/空格切换，
 * 而不是只有右侧那个小三角能点（触屏上 44×44 的触摸目标也靠它满足）。
 *
 * 收起用 `hidden` 属性而不是条件渲染：条件渲染会让收起的分组**整段卸载**，
 * 词表里正在输入的字符、下拉的展开态、滚动位置全部丢失；而 `hidden` 只是不显示，
 * DOM 里字段还在，`aria-controls` 指向的 id 也始终存在（读屏不迷路）。
 *
 * 代价是收起时元素仍在 DOM 里——首屏**元素总数**不会降到 0，降下来的是
 * 「首屏高度 / 一屏塞不下」的视觉噪音与布局代价（实测对照见交付说明）。
 *
 * 注意：本组件**不**渲染 `data-purge-fix`——组内已有真正的锚点（页内导航与告警
 * 跳转都指向那些具体控件）。组上再挂一个同名锚点会让 `querySelector` 先命中组，
 * 于是「跳到字段」变成「跳到组头」，闪烁高亮也落在整块上。
 */
export const PurgeSection = memo(function PurgeSection({
  meta,
  open,
  onToggle,
  status,
  statusActive,
  actions,
  mutedWhenOff,
  children,
  t,
}: PurgeSectionProps): JSX.Element {
  const bodyId = `purge-group-${meta.id}`;
  return (
    <section
      className={`purge-group${open ? ' is-open' : ' is-closed'}${mutedWhenOff ? ' is-muted' : ''}`}
      data-purge-group={meta.id}
    >
      <div className="purge-group-head">
        {/*
          无障碍名称**只取组名**（`aria-label`），不含 hint：
          1. 读屏念「统计」而不是「统计 命中与失败计数（只读）」——后者把提示当名字；
          2. hint 里含「试跑」「死区词改写」这类词，若进了名称，页内 `getByRole('button',
             { name: /死区词改写/ })` 这类既有查询会命中两个按钮（strict mode 冲突，
             实测 e2e 的「词表行的试」与「生效项跳转」两条都因此挂掉）。
          hint 仍在视觉上显示（对看得见的用户有用），只是 `aria-hidden`。
        */}
        <button
          type="button"
          className="purge-group-toggle"
          aria-expanded={open}
          aria-controls={bodyId}
          aria-label={t(meta.label)}
          data-testid={`purge-group-toggle-${meta.id}`}
          onClick={() => onToggle(meta.id)}
        >
          <span className="purge-group-caret" aria-hidden="true">
            {open ? '▾' : '▸'}
          </span>
          <span className="purge-group-title">{t(meta.label)}</span>
          <span className="purge-group-hint" aria-hidden="true">
            {t(meta.hint)}
          </span>
        </button>
        {status ? (
          <span
            className={`purge-group-status${statusActive ? ' is-active' : ''}`}
            data-testid={`purge-group-status-${meta.id}`}
          >
            {status}
          </span>
        ) : null}
        {open && actions ? <div className="purge-group-actions">{actions}</div> : null}
      </div>
      <div className="purge-group-body" id={bodyId} hidden={!open}>
        {children}
      </div>
    </section>
  );
});

/** 折叠上下文：唯一传递通道（见 `PurgeGroupWrap` 的头注释）。 */
export interface PurgeGroupCtxValue {
  /** 组是否展开（含总开关关闭时强制收起的派生规则） */
  isOpen: (id: PurgeGroupId) => boolean;
  toggle: (id: PurgeGroupId) => void;
}

const PurgeGroupCtx = createContext<PurgeGroupCtxValue | null>(null);

export const PurgeGroupProvider = PurgeGroupCtx.Provider;

export function usePurgeGroupCtx(): PurgeGroupCtxValue {
  const value = useContext(PurgeGroupCtx);
  if (!value) {
    throw new Error('PurgeGroupWrap 必须包在 PurgeGroupProvider 之内');
  }
  return value;
}

/**
 * 折叠状态 + 「跳转前先展开目标组」的唯一实现。
 *
 * 跳转为什么也要挂在这里：页内 `focusFixTarget` 会先摘掉目标组的 `hidden`
 * （同步改 DOM，不等 React 渲染），这条路与「用户点组头」共用同一个 collapsed
 * 集合——两处各写一份就会漂移（点开又跳转，跳转后组又收起）。
 */
export function usePurgeGroups({
  enabled,
  collapsed,
  setCollapsed,
}: {
  enabled: boolean;
  collapsed: readonly PurgeGroupId[];
  setCollapsed: (next: PurgeGroupId[]) => void;
}): {
  toggle: (id: PurgeGroupId) => void;
  isOpen: (id: PurgeGroupId) => boolean;
  openCount: number;
  ctx: PurgeGroupCtxValue;
} {
  // 回调都读「当前」折叠集合：做成 ref 兜住，回调引用才稳定。
  // `PurgeSection` 是 memo 组件，回调不稳会让每组每次渲染都重建 ——
  // 组多了以后每次输入都重画 9 个组头。
  const collapsedRef = useRef(collapsed);
  collapsedRef.current = collapsed;
  const setRef = useRef(setCollapsed);
  setRef.current = setCollapsed;
  const enabledRef = useRef(enabled);
  enabledRef.current = enabled;

  const toggle = useCallback((id: PurgeGroupId) => {
    const current = collapsedRef.current;
    setRef.current(
      current.includes(id) ? current.filter((item) => item !== id) : [...current, id],
    );
  }, []);

  const isOpen = useCallback(
    (id: PurgeGroupId) => isGroupOpen(id, collapsedRef.current, enabledRef.current),
    [],
  );

  const openCount = useMemo(
    () => ALL_GROUP_IDS.filter((id) => isGroupOpen(id, collapsed, enabled)).length,
    [collapsed, enabled],
  );

  const ctx = useMemo<PurgeGroupCtxValue>(() => ({ isOpen, toggle }), [isOpen, toggle]);

  return { toggle, isOpen, openCount, ctx };
}

/** 锚点 → 目标组（跳转前先展开用）。纯函数转发，保持「锚点表只有一份」。 */
export function groupOfAnchor(anchor: string): PurgeGroupId | null {
  return groupForAnchor(anchor);
}

/**
 * 「跳转前先展开」的**状态级**入口（模块级单槽，由页面在 effect 里注册）。
 *
 * 为什么不能只在 `focusFixTarget` 里改 DOM：那一下确实摘掉了 `hidden`，但
 * 跳转动作通常伴随 `addToast`/`patch` 之类的 setState，React 下一轮渲染会把
 * `hidden` 按 `collapsed` 原样写回去——用户看到「闪了一下就没了」（实测过：
 * 告警锚点拿到 `.purge-fix-flash`，但组体 `hidden=true`）。
 *
 * 所以计划跳转时必须同时**改状态**（并顺手落盘），DOM 那一下只是让滚动量到
 * 正确高度、不等一帧渲染。单槽而不是 context：`focusFixTarget` 是模块级函数，
 * 被纯函数与旧调用点共用，走 context 就得把它改成 hook，牵动十几处调用。
 */
let openGroupHandler: ((id: PurgeGroupId) => void) | null = null;

export function registerOpenGroupHandler(handler: ((id: PurgeGroupId) => void) | null): void {
  openGroupHandler = handler;
}

export function requestOpenGroup(id: PurgeGroupId): void {
  const body = typeof document === 'undefined' ? null : document.getElementById(`purge-group-${id}`);
  // 同步摘掉 `hidden`：让紧随其后的 scrollIntoView 量到展开后的高度
  if (body && body.hidden) body.hidden = false;
  openGroupHandler?.(id);
}

/** 总开关关闭时会被强制收起的组（页面用它判断「重新打开总开关要不要恢复」）。 */
export const FORCED_WHEN_OFF: readonly PurgeGroupId[] = FORCED_COLLAPSE_WHEN_OFF;

/**
 * 折叠头工具条：全部展开 / 全部收起。
 *
 * 用户原话「不要为了看起来清爽把功能藏到找不到的地方」——折叠收益必须能一键撤回。
 */
export const PurgeGroupToolbar = memo(function PurgeGroupToolbar({
  openCount,
  total,
  onExpandAll,
  onCollapseAll,
  t,
}: {
  openCount: number;
  total: number;
  onExpandAll: () => void;
  onCollapseAll: () => void;
  t: TFunction;
}): JSX.Element {
  return (
    <div className="purge-grouptools" data-testid="purge-group-tools">
      <span className="purge-hint">
        {t('已展开 {{open}} / {{total}} 组', { open: openCount, total })}
      </span>
      <button
        type="button"
        className="purge-grouptools-btn"
        onClick={onExpandAll}
        data-testid="purge-groups-expand-all"
      >
        {/* 文案刻意**不含「全部」**：预设库的厂商筛选 chip 里本来就有「全部」，
            两个同名按钮会让 `getByRole('button', { name: '全部' })` 变成 strict
            mode 冲突（实测 e2e 的厂商过滤用例因此挂掉）。「展开所有组」同样清楚。 */}
        {t('展开所有组')}
      </button>
      <button
        type="button"
        className="purge-grouptools-btn"
        onClick={onCollapseAll}
        data-testid="purge-groups-collapse-all"
      >
        {t('收起所有组')}
      </button>
    </div>
  );
});

/**
 * 「N 项已启用」摘要文案（折叠头右侧）。
 *
 * 0 项也照常显示——折叠头若什么都不显示，看起来就像「这组没配过」；而「未启用」
 * 恰好是最该被看见的信息（用户配了内容但开关关着，正是这个数）。且不只显示数字：
 * `0` 一个字符在折叠头上没有任何含义。
 */
export function groupStatusText(
  count: number,
  t: TFunction,
  opts: { active: boolean } = { active: true },
): string {
  if (count <= 0) return t('未启用');
  return opts.active ? t('{{n}} 项已启用', { n: count }) : t('{{n}} 项已配置', { n: count });
}

/**
 * **裹一层分组**（读 context，不传 props）。
 *
 * 用法：在要折进某组的那一段 JSX 外面包一层 `<PurgeGroupWrap id="governance">`。
 * `wrap={false}` 用于「已经在同一组里」的后续卡片——只让第一张出组头。
 *
 * 为什么读 context 而不是收 props：`PurgeConsole` 的渲染体是一个上千行的返回
 * 表达式，逐处传 props 意味着几十处签名改动；context 让「包一层」成为唯一要写的
 * 代码。而且失败模式是**构建期就抛错**（没有 provider 直接 throw），不会静默
 * 退化成「没有折叠」——那是最坏的失败：改了半天，页面一点没变。
 */
export const PurgeGroupWrap = memo(function PurgeGroupWrap({
  id,
  wrap = true,
  status,
  statusActive,
  actions,
  children,
  t,
}: {
  id: PurgeGroupId;
  wrap?: boolean;
  status?: ReactNode;
  statusActive?: boolean;
  actions?: ReactNode;
  children: ReactNode;
  t: TFunction;
}): JSX.Element {
  // hooks 必须在早退之前调用（wrap=false 也走一次 useContext，规则才稳定）
  const ctx = usePurgeGroupCtx();
  const group = PURGE_GROUPS.find((item) => item.id === id);
  if (!group) {
    throw new Error(`PurgeGroupWrap: 未登记的分组 id「${id}」`);
  }
  if (!wrap) return <>{children}</>;
  return (
    <PurgeSection
      meta={group}
      open={ctx.isOpen(id)}
      onToggle={ctx.toggle}
      status={status}
      statusActive={statusActive}
      actions={actions}
      mutedWhenOff={group.forceCollapseWhenOff}
      t={t}
    >
      {children}
    </PurgeSection>
  );
});