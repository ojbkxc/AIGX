/**
 * 卡片拖拽排序（对齐 Agent2API v3.5.1 `routing-drag.tsx`，含落点预览浮层）。
 *
 * 契约（与 A2A 逐条一致，改动会同时打断两处调用点）：
 * - 只在把手 `onPointerDown` 起拖（`start(event, kind, provider, id)`）；
 * - 起拖元素必须落在 `[data-route-sort]` 行内、且该行最近的 `[data-route-list]`
 *   里至少有两个同级行，否则不起拖（单行不需要排序）；
 * - 落点按同级行 `header` 的中线计算 before/after，拖动过程中同级行的
 *   `data-route-sort` 序列一旦变化就取消（列表被别的动作改过 = 落点语义失效）；
 * - 松手、`pointercancel`、窗口失焦、Esc、`buttons === 0` 都取消；
 * - `enabled` 为 false（锁定 / 搜索中 / 只读）时整体禁用，且进拖的中途关掉也立即取消。
 *
 * 用途：成员卡之间（kind='member'）与组内账号行之间（kind='account'）。
 * 提供商卡（kind='provider'）的落点语义属于配置面板，暂未接（AIGX 侧
 * 提供商顺序由档案 `scope.providers` 维护）。
 */
import type React from 'react';
import { useCallback, useEffect, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { useTranslation } from 'react-i18next';

export type RoutingDrop = {
  kind: 'member' | 'account';
  /** account 落点用：所属成员键；member 落点为空串 */
  provider: string;
  id: string;
  target: string;
  after: boolean;
};

type Drag = Omit<RoutingDrop, 'target' | 'after'> & {
  pointer: number;
  x: number;
  y: number;
  active: boolean;
  list: HTMLElement;
  ids: string[];
  label: string;
};

type Position = RoutingDrop & { index: number; x: number; y: number; label: string };

/** 只在把手起拖，落点按同级列表计算；取消与松手是两条独立路径。 */
export function useRoutingDrag(enabled: boolean, scope: string, onDrop: (drop: RoutingDrop) => void) {
  const drag = useRef<Drag | null>(null);
  const position = useRef<Position | null>(null);
  const [state, setState] = useState<Position | null>(null);
  const live = useRef({ enabled, onDrop });
  live.current = { enabled, onDrop };
  const cancel = useCallback(() => {
    drag.current = null;
    position.current = null;
    setState(null);
  }, []);
  useEffect(() => {
    if (!enabled) cancel();
  }, [enabled, cancel]);
  useEffect(() => cancel, [scope, cancel]);
  useEffect(() => {
    const rows = (current: Drag): HTMLElement[] => [...current.list.querySelectorAll<HTMLElement>('[data-route-sort]')]
      .filter((row) => row.closest('[data-route-list]') === current.list);
    let frame = 0;
    let lastY = 0;
    const locate = (x: number, y: number): void => {
      const current = drag.current;
      if (!current?.active || !current.list.isConnected || !live.current.enabled) {
        cancel();
        return;
      }
      const list = rows(current);
      if (list.map((row) => row.dataset.routeSort).join('\0') !== current.ids.join('\0')) {
        cancel();
        return;
      }
      const others = list.filter((row) => row.dataset.routeSort !== current.id);
      const bounds = current.list.getBoundingClientRect();
      const scrollerBounds = current.list.closest<HTMLElement>('[data-route-scroll]')?.getBoundingClientRect() || bounds;
      if (
        !others.length
        || x < bounds.left - 24
        || x > bounds.right + 24
        || y < scrollerBounds.top - 48
        || y > scrollerBounds.bottom + 48
      ) {
        position.current = null;
        setState(null);
        return;
      }
      const found = others.findIndex((row) => {
        const rect = (current.kind !== 'account' ? row.querySelector('header') || row : row).getBoundingClientRect();
        return y < rect.top + rect.height / 2;
      });
      const index = found < 0 ? others.length : found;
      const target = others[Math.min(index, others.length - 1)].dataset.routeSort as string;
      const next: Position = {
        kind: current.kind,
        provider: current.provider,
        id: current.id,
        target,
        after: index === others.length,
        index,
        x,
        y,
        label: current.label,
      };
      position.current = next;
      setState(next);
    };
    const scroll = (): void => {
      const current = drag.current;
      if (!current?.active) return;
      const scroller = current.list.closest<HTMLElement>('[data-route-scroll]');
      if (scroller) {
        const rect = scroller.getBoundingClientRect();
        const delta = lastY < rect.top + 48 ? -12 : lastY > rect.bottom - 48 ? 12 : 0;
        if (delta) {
          scroller.scrollTop += delta;
          locate(position.current?.x ?? current.x, lastY);
        }
      }
      frame = requestAnimationFrame(scroll);
    };
    const move = (event: PointerEvent): void => {
      const current = drag.current;
      if (!current || event.pointerId !== current.pointer) return;
      if (event.buttons === 0 || !live.current.enabled) {
        cancel();
        return;
      }
      if (!current.active && Math.hypot(event.clientX - current.x, event.clientY - current.y) < 7) return;
      event.preventDefault();
      if (!current.active) {
        current.active = true;
        frame = requestAnimationFrame(scroll);
      }
      lastY = event.clientY;
      locate(event.clientX, event.clientY);
    };
    const finish = (event: PointerEvent): void => {
      const current = drag.current;
      if (!current || event.pointerId !== current.pointer) return;
      if (!current.active) {
        cancel();
        return;
      }
      locate(event.clientX, event.clientY);
      const drop = position.current;
      cancelAnimationFrame(frame);
      cancel();
      if (current.active && drop && live.current.enabled) {
        live.current.onDrop({ kind: current.kind, provider: current.provider, id: current.id, target: drop.target, after: drop.after });
      }
    };
    const abort = (): void => {
      cancelAnimationFrame(frame);
      cancel();
    };
    const key = (event: KeyboardEvent): void => {
      if (event.key === 'Escape' && drag.current) {
        event.preventDefault();
        abort();
      }
    };
    window.addEventListener('pointermove', move, { passive: false });
    window.addEventListener('pointerup', finish);
    window.addEventListener('pointercancel', abort);
    window.addEventListener('blur', abort);
    window.addEventListener('keydown', key);
    return () => {
      cancelAnimationFrame(frame);
      window.removeEventListener('pointermove', move);
      window.removeEventListener('pointerup', finish);
      window.removeEventListener('pointercancel', abort);
      window.removeEventListener('blur', abort);
      window.removeEventListener('keydown', key);
      drag.current = null;
    };
  }, [cancel]);
  const start = (event: React.PointerEvent, kind: RoutingDrop['kind'], provider: string, id: string): void => {
    if (!live.current.enabled || event.button !== 0 || drag.current) return;
    const row = event.currentTarget.closest<HTMLElement>('[data-route-sort]');
    const list = row?.closest<HTMLElement>('[data-route-list]');
    if (!list || !row) return;
    const ids = [...list.querySelectorAll<HTMLElement>('[data-route-sort]')]
      .filter((item) => item.closest('[data-route-list]') === list)
      .map((item) => item.dataset.routeSort as string);
    if (ids.length < 2) return;
    if (event.pointerType === 'mouse') event.preventDefault();
    event.currentTarget.setPointerCapture(event.pointerId);
    drag.current = {
      kind,
      provider,
      id,
      pointer: event.pointerId,
      x: event.clientX,
      y: event.clientY,
      active: false,
      list,
      ids,
      label: row.querySelector('b')?.textContent || id,
    };
  };
  return { start, state, cancel };
}

/** 拖动中的浮层（A2A `RoutingDragOverlay`：名字 + 「放到第 N 位」）。 */
export function RoutingDragOverlay({ state }: { state: Position | null }): JSX.Element | null {
  const { t } = useTranslation();
  return state
    ? createPortal(
      <div className="rt-drag-preview" style={{ left: state.x + 18, top: state.y + 16 }}>
        <b>{state.label}</b>
        <span>{t('放到第 {{n}} 位', { n: state.index + 1 })}</span>
      </div>,
      document.body,
    )
    : null;
}

/** 把 `id` 移动到 `target` 之前/之后（原地改数组，A2A 同名函数同语义）。 */
export function reorderRoute(ids: string[], id: string, target: string, after: boolean): void {
  const from = ids.indexOf(id);
  if (from < 0 || id === target || !ids.includes(target)) return;
  ids.splice(from, 1);
  ids.splice(ids.indexOf(target) + (after ? 1 : 0), 0, id);
}