/**
 * useColumnResize —— 拖拽会话的收尾（卸载 / pointercancel）。
 *
 * 复现的原始缺陷：
 * 1) 监听器挂在 window 上却没有句柄。拖到一半组件卸载（切路由、关弹窗）后，
 *    `pointermove`/`pointerup` 一直留在 window 上，松手时 `onUp` 对已卸载组件
 *    `setWidths` + `onCommit` —— 一次没人看见的拖拽结果被写进了存储。
 * 2) 只挂 pointerup。触屏被系统手势接管 / 窗口失焦时浏览器只发 `pointercancel`，
 *    pointerup 永不触发：监听器泄漏，`draggingKey` 卡在「拖拽中」。
 */
import { describe, expect, it, vi } from 'vitest';
import { render, act } from '@testing-library/react';
import { useState } from 'react';
import { useColumnResize } from './useColumnResize';

const COL = 'name';

function Harness({
  onCommit,
  initialWidths = {},
}: {
  onCommit?: (w: Record<string, number>) => void;
  initialWidths?: Record<string, number>;
}) {
  const [widths, setWidths] = useState<Record<string, number>>(initialWidths);
  const hook = useColumnResize({
    widths,
    setWidths,
    defaultWidths: { [COL]: 200 },
    onCommit,
  });
  const grip = hook.gripProps(COL, { isLast: false, defaultWidth: 200 });
  return (
    <div>
      <table>
        <colgroup>
          <col data-col-key={COL} />
        </colgroup>
        <thead>
          <tr>
            <th>
              {/* 把手：onPointerDown / onDoubleClick 由 hook 提供 */}
              <span data-testid="grip" {...grip} />
            </th>
          </tr>
        </thead>
      </table>
    </div>
  );
}

/** 从把手按下开始一次拖拽（clientX 可指定） */
function startDrag(grip: HTMLElement, clientX = 100): void {
  act(() => {
    grip.dispatchEvent(pointerEvent('pointerdown', { clientX, button: 0 }));
  });
}

/**
 * 手造指针事件。
 *
 * jsdom 没有 `PointerEvent`（`window.PointerEvent === undefined`），
 * `fireEvent.pointerDown(el, { clientX })` 只会派发一个普通 `Event`，
 * **clientX 被静默丢掉** —— 拿它写拖拽测试会得到 `dx = NaN` 的假绿/假红。
 * 这里显式把坐标挂到事件对象上（React 的合成事件直接读 nativeEvent 字段）。
 */
function pointerEvent(
  type: 'pointerdown' | 'pointermove' | 'pointerup' | 'pointercancel',
  init: { clientX: number; button?: number } = { clientX: 0 },
): Event {
  const event = new Event(type, { bubbles: true, cancelable: true });
  Object.defineProperty(event, 'clientX', { value: init.clientX });
  Object.defineProperty(event, 'clientY', { value: 0 });
  Object.defineProperty(event, 'button', { value: init.button ?? 0 });
  return event;
}

/** 在 window 上派发（拖拽期间的 move/up/cancel 都挂在 window） */
function fireOnWindow(
  type: 'pointermove' | 'pointerup' | 'pointercancel',
  clientX: number,
): void {
  act(() => {
    window.dispatchEvent(pointerEvent(type, { clientX }));
  });
}

describe('useColumnResize 拖拽会话收尾', () => {
  it('拖拽中卸载：之后 window 上的 pointermove/pointerup 不再落盘', () => {
    const onCommit = vi.fn();
    const { getByTestId, unmount } = render(<Harness onCommit={onCommit} />);
    const grip = getByTestId('grip');

    startDrag(grip, 100);
    // 拖到一半就卸载
    fireOnWindow('pointermove', 160);
    unmount();

    // 卸载后松手：若监听器还在，onUp 会对已卸载组件 setWidths + onCommit
    fireOnWindow('pointerup', 160);

    expect(onCommit).not.toHaveBeenCalled();
  });

  it('pointercancel：不落盘，且被直写过的 DOM 宽度还原', () => {
    const onCommit = vi.fn();
    const { getByTestId } = render(<Harness onCommit={onCommit} />);
    const grip = getByTestId('grip');
    const col = document.querySelector('col[data-col-key="name"]') as HTMLElement;

    startDrag(grip, 100);
    fireOnWindow('pointermove', 180);
    // 直写路径生效：DOM 已变宽（200 + 80）
    expect(col.style.width).toBe('280px');

    fireOnWindow('pointercancel', 180);

    expect(onCommit).not.toHaveBeenCalled();
    // 状态没变，DOM 也不能停在中间值
    expect(col.style.width).toBe('');
  });

  it('正向对照：正常松手仍然落盘一次', () => {
    const onCommit = vi.fn();
    const { getByTestId } = render(<Harness onCommit={onCommit} />);
    const grip = getByTestId('grip');

    startDrag(grip, 100);
    fireOnWindow('pointermove', 150);
    fireOnWindow('pointerup', 150);

    expect(onCommit).toHaveBeenCalledTimes(1);
    expect(onCommit).toHaveBeenCalledWith({ name: 250 });
  });
});
