import { describe, expect, it, vi, beforeEach, afterEach } from 'vitest';
import { render, waitFor } from '@testing-library/react';
import { useMobileColumns } from './useMobileColumns';

// jsdom 没有 ResizeObserver；matchMedia 恒 false 无法模拟断点，均按测试需要补齐
function installEnv(matches: boolean): void {
  vi.stubGlobal('ResizeObserver', class {
    observe(): void {}
    unobserve(): void {}
    disconnect(): void {}
  });
  Object.defineProperty(window, 'matchMedia', {
    configurable: true,
    value: vi.fn(() => ({
      matches,
      addEventListener: () => {},
      removeEventListener: () => {},
    })),
  });
}

function Harness(): JSX.Element {
  useMobileColumns();
  return (
    <div className="table-wrapper">
      <table>
        <thead>
          <tr><th>模型</th><th>请求数</th></tr>
        </thead>
        <tbody>
          <tr><td>gpt-4</td><td>10</td></tr>
        </tbody>
      </table>
    </div>
  );
}

beforeEach(() => installEnv(true));
afterEach(() => {
  vi.unstubAllGlobals();
});

describe('useMobileColumns — 按需绑定具体表格（#18）', () => {
  it('窄屏首次挂载给 td 注入 data-col', () => {
    render(<Harness />);
    const tds = document.querySelectorAll('tbody td');
    expect(tds[0]?.getAttribute('data-col')).toBe('模型');
    expect(tds[1]?.getAttribute('data-col')).toBe('请求数');
  });

  it('tbody 重建后按受影响表格重绘（row 换新行也会补上标签）', async () => {
    render(<Harness />);
    // 模拟 React 重渲染重建行：清掉旧行（含已注入的 data-col）再插入新行
    const tbody = document.querySelector('tbody');
    tbody!.innerHTML = '';
    const tr = document.createElement('tr');
    tr.innerHTML = '<td>gpt-4o</td><td>20</td>';
    tbody!.appendChild(tr);
    await waitFor(() => {
      const tds = document.querySelectorAll('tbody td');
      expect(tds[0]?.getAttribute('data-col')).toBe('模型');
    });
  });

  it('桌面端（断点外）不注入列名', () => {
    installEnv(false);
    render(<Harness />);
    expect(document.querySelector('tbody td')?.getAttribute('data-col')).toBeNull();
  });
});
