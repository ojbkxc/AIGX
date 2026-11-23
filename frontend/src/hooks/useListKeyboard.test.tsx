import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render } from '@testing-library/react';
import { useListKeyboard } from './useListKeyboard';

function Harness({ total, onEnter }: { total: number; onEnter: (i: number) => void }): JSX.Element {
  const { searchRef } = useListKeyboard(total, onEnter);
  return (
    <div>
      <input ref={searchRef} data-testid="search" type="text" />
      <table data-testid="rows">
        {Array.from({ length: total }, (_, i) => (
          <tr key={i} data-testid={`row-${i}`} />
        ))}
      </table>
    </div>
  );
}

describe('useListKeyboard', () => {
  afterEach(() => {
    cleanup();
  });

  it('非输入态 j/k 在行间移动（基本导航仍可用）', () => {
    const onEnter = vi.fn();
    const { getByTestId } = render(<Harness total={3} onEnter={onEnter} />);
    // 先点表格把焦点放到非输入元素之外（body 即非输入态）
    getByTestId('rows').focus();
    fireEvent.keyDown(document, { key: 'j' });
    // 无高亮时 j -> 高亮到 0（activeIndex -1 -> 0）
    fireEvent.keyDown(document, { key: 'k' });
    fireEvent.keyDown(document, { key: 'Enter' });
    // j 一次 + k 一次 + Enter：j 落在 0，k 回 total-1（wrap）
    expect(onEnter).toHaveBeenCalled();
  });

  it('搜索框内输入 j/k 不被导航吞掉（回归：ojbkxc 输入到 j 断掉）', () => {
    const onEnter = vi.fn();
    const { getByTestId } = render(<Harness total={3} onEnter={onEnter} />);
    const input = getByTestId('search') as HTMLInputElement;
    input.focus();
    // 模拟在搜索框内按 j：监听器不得 preventDefault，否则字符打不进输入框
    let prevented = false;
    const probe = (e: KeyboardEvent): void => { if (e.defaultPrevented) prevented = true; };
    document.addEventListener('keydown', probe);
    try {
      fireEvent.keyDown(input, { key: 'j', bubbles: true });
      fireEvent.keyDown(input, { key: 'k', bubbles: true });
    } finally {
      document.removeEventListener('keydown', probe);
    }
    expect(prevented).toBe(false);
    expect(onEnter).not.toHaveBeenCalled();
  });

  it('搜索框内 Enter 不触发行打开（onEnter 只在非输入态）', () => {
    const onEnter = vi.fn();
    const { getByTestId } = render(<Harness total={3} onEnter={onEnter} />);
    const input = getByTestId('search') as HTMLInputElement;
    input.focus();
    fireEvent.keyDown(input, { key: 'Enter', bubbles: true });
    expect(onEnter).not.toHaveBeenCalled();
  });
});
