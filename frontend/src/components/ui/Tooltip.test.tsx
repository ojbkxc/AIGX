/**
 * Tooltip —— 显示计时器与 hide 的配对。
 *
 * 复现的原始缺陷：`show()` 直接覆盖 `timer.current`。键盘用户点进一个被
 * Tooltip 包住的控件时，`mouseenter` 与 `focus` 会接连各排一个计时器；
 * 前一个失去句柄，`hide()` 只清得掉最后一个 —— 残留的那个在鼠标与焦点都
 * 离开之后把 `visible` 置回 true，提示卡住不消失。
 */
import { describe, expect, it, vi, beforeEach, afterEach } from 'vitest';
import { render, fireEvent, act } from '@testing-library/react';
import Tooltip from './Tooltip';

const DELAY = 300;

function setup() {
  const view = render(
    <Tooltip label="提示内容" delay={DELAY}>
      <button type="button">目标</button>
    </Tooltip>,
  );
  const target = view.getByRole('button', { name: '目标' });
  return { ...view, target };
}

describe('Tooltip 计时器配对', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it('enter 后再 focus，两者都离开：提示最终不可见（残留计时器不得把它拉回来）', () => {
    const { target, queryByRole } = setup();

    // 键盘用户点进控件：mouseenter 与 focus 接连触发两次 show()
    fireEvent.mouseEnter(target);
    fireEvent.focus(target);
    // 立刻离开（还没到 delay）
    fireEvent.mouseLeave(target);
    fireEvent.blur(target);

    act(() => {
      vi.advanceTimersByTime(DELAY * 3);
    });

    expect(queryByRole('tooltip')).toBeNull();
  });

  it('正向对照：只 enter 不离开，delay 到点后提示可见', () => {
    const { target, queryByRole } = setup();

    fireEvent.mouseEnter(target);
    act(() => {
      vi.advanceTimersByTime(DELAY);
    });

    const tip = queryByRole('tooltip');
    expect(tip).not.toBeNull();
    expect(tip?.textContent).toBe('提示内容');
  });

  it('正向对照：可见后离开立即隐藏', () => {
    const { target, queryByRole } = setup();

    fireEvent.mouseEnter(target);
    act(() => {
      vi.advanceTimersByTime(DELAY);
    });
    expect(queryByRole('tooltip')).not.toBeNull();

    fireEvent.mouseLeave(target);
    expect(queryByRole('tooltip')).toBeNull();
  });
});
