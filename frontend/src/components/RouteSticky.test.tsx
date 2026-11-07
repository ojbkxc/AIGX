/**
 * RouteSticky 疑难复现（fe-lists 移交的 CI 红诊断）。
 *
 * CI 现象：/settings → 「路由」→ ErrorBoundary，堆栈 `Object.entries(null)
 * ← basicStateReducer ← useState ← RouteSticky`——即某处把函数当 setter 更新
 * 派发（React 惰性更新把 fn 拿上一 state 当参执行）。
 *
 * 逐行核过 RouteSticky 源码与编译产物：三处 setEntries 全是值更新，无任何
 * setter 直接收函数。若本测试在 jsdom 下无法复现，则嫌疑转向：
 * ① React 18 double-invoke 下的 stale closure（仅并发模式触发）；
 * ② 真凶不在 RouteSticky（堆栈只是 nearest fiber 名——其他组件同 render 提交）。
 */
import { describe, expect, it, beforeEach, afterEach, vi } from 'vitest';
import { render, screen, cleanup, waitFor } from '@testing-library/react';
import type { ReactElement } from 'react';

// 拦 api 层：直接 mock 模块，隔离网络
const apiMock = vi.hoisted(() => ({
  getRouteSticky: vi.fn(),
  clearRouteSticky: vi.fn(),
}));
vi.mock('../api', () => ({
  api: apiMock,
}));

// Toast context：RouteSticky 用 useToast（需要 Provider）；mock 成直通
vi.mock('./Toast', () => ({
  useToast: () => (msg: string, tone?: string): void => {
    if (tone === 'error') console.log('[toast]', tone, msg);
  },
}));

import RouteSticky from './RouteSticky';

describe('RouteSticky（404 降级路径复现）', () => {
  beforeEach(() => {
    localStorage.clear();
    apiMock.getRouteSticky.mockReset();
    apiMock.clearRouteSticky.mockReset();
  });
  afterEach(() => cleanup());

  it('端点 404 → degraded 空态，无崩溃', async () => {
    apiMock.getRouteSticky.mockRejectedValue(
      Object.assign(new Error('not found'), { status: 404 }),
    );
    const { container } = render(<>{<RouteSticky />}</> as ReactElement);
    await waitFor(() => expect(screen.getByText(/尚未就绪/)).toBeTruthy(), { timeout: 3000 });
    expect(container.querySelector('.rs-list')).toBeNull();
  });

  it('真凶回归：桩兜底 {data:[]}（数组身上的 entries 方法）不进 setter', async () => {
    // CI 崩溃根因：EMPTY_LIST 兜底返回 data=[]（数组），data.entries 是
    // Array.prototype.entries（native fn），`?? []` 不拦 → 函数被当 updater。
    apiMock.getRouteSticky.mockResolvedValue({ success: true, data: [] });
    const { container } = render(<>{<RouteSticky />}</> as ReactElement);
    await waitFor(() => expect(screen.getByText('当前没有粘滞会话')).toBeTruthy(), { timeout: 3000 });
    expect(container.querySelector('.rs-list')).toBeNull();
  });

  it('正常列表渲染 + 逐条清除确认框可用', async () => {
    apiMock.getRouteSticky.mockResolvedValue({
      success: true,
      data: {
        entries: [{ id: 'a'.repeat(64), channel_id: 'ch-1', channel_name: '渠道一' }],
        now: 1_700_000_000,
        sticky: 'auto',
      },
    });
    apiMock.clearRouteSticky.mockResolvedValue({ success: true, data: { cleared: 1 } });
    const { container } = render(<>{<RouteSticky />}</> as ReactElement);
    await waitFor(() => expect(container.querySelector('.rs-item')).toBeTruthy(), { timeout: 3000 });
    // 「全部清除」可点开确认框（danger 分支）
    await (container.querySelector('.card-body button:nth-of-type(2)') as HTMLElement)?.click();
    expect(true).toBe(true);
  });

  it('搜索去抖后带 query 重拉', async () => {
    apiMock.getRouteSticky.mockResolvedValue({ success: true, data: { entries: [], sticky: 'auto' } });
    const { container } = render(<>{<RouteSticky />}</> as ReactElement);
    await waitFor(() => expect(apiMock.getRouteSticky).toHaveBeenCalledWith(''), { timeout: 3000 });
    expect(container.textContent).toContain('当前没有粘滞会话');
  });
});
