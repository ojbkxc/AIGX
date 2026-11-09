/**
 * useServerPaging —— 全站服务端分页统一 hook 的单测。
 *
 * 覆盖三类此前各页普遍缺失、由本 hook 统一提供的自愈能力：
 * 1) 越界钳位：total 变小（删空末页 / 筛选命中变少）→ 当前页收敛到最后一页；
 * 2) 每页条数变更 → 回第 1 页；
 * 3) 筛选依赖（resetDeps）变化 → 回第 1 页。
 */
import { describe, expect, it, beforeEach } from 'vitest';
import { renderHook, act } from '@testing-library/react';
import { useServerPaging, SERVER_PAGE_SIZES } from './TableShell';

const KEY = 'aigx-test-pagesize';

beforeEach(() => {
  localStorage.clear();
});

describe('useServerPaging', () => {
  it('默认第 1 页 / 无数据时 totalPages 恒为 1', () => {
    const { result } = renderHook(() =>
      useServerPaging({ key: KEY, sizes: SERVER_PAGE_SIZES, fallback: 20 }));
    expect(result.current.page).toBe(1);
    expect(result.current.size).toBe(20);
    expect(result.current.totalPages).toBe(1);
  });

  it('越界钳位：total 变小后当前页收敛到最后一页', () => {
    const { result } = renderHook(() =>
      useServerPaging({ key: KEY, sizes: SERVER_PAGE_SIZES, fallback: 20 }));
    act(() => result.current.setTotal(100)); // 100/20 = 5 页
    expect(result.current.totalPages).toBe(5);
    act(() => result.current.setPage(5));
    expect(result.current.page).toBe(5);

    // 删空末页：total 掉到 30（2 页）→ 第 5 页越界，自愈到第 2 页
    act(() => result.current.setTotal(30));
    expect(result.current.totalPages).toBe(2);
    expect(result.current.page).toBe(2);
  });

  it('total 归零（删空全部）后回第 1 页且不出现第 0 页', () => {
    const { result } = renderHook(() =>
      useServerPaging({ key: KEY, sizes: SERVER_PAGE_SIZES, fallback: 20 }));
    act(() => result.current.setTotal(100));
    act(() => result.current.setPage(4));
    act(() => result.current.setTotal(0));
    expect(result.current.page).toBe(1);
    expect(result.current.totalPages).toBe(1);
  });

  it('每页条数变更 → 回第 1 页并落盘/换挡', () => {
    const { result } = renderHook(() =>
      useServerPaging({ key: KEY, sizes: SERVER_PAGE_SIZES, fallback: 20 }));
    act(() => result.current.setTotal(1000));
    act(() => result.current.setPage(5));
    expect(result.current.page).toBe(5);

    act(() => result.current.setSize(100));
    expect(result.current.size).toBe(100);
    expect(result.current.page).toBe(1);
    expect(localStorage.getItem(KEY)).toBe('100');
  });

  it('筛选依赖（resetDeps）变化 → 回第 1 页；不影响 page 时不动', () => {
    const { result, rerender } = renderHook(
      ({ kw }: { kw: string }) =>
        useServerPaging({ key: KEY, sizes: SERVER_PAGE_SIZES, fallback: 20, resetDeps: [kw] }),
      { initialProps: { kw: 'a' } },
    );
    act(() => result.current.setTotal(1000));
    act(() => result.current.setPage(6));
    expect(result.current.page).toBe(6);

    // 筛选词变化 → 回第 1 页
    rerender({ kw: 'b' });
    expect(result.current.page).toBe(1);

    // 翻到第 3 页后重渲染但筛选词不变 → 页码保持
    act(() => result.current.setPage(3));
    rerender({ kw: 'b' });
    expect(result.current.page).toBe(3);
  });

  it('脏存储档位被夹回兜底档', () => {
    localStorage.setItem(KEY, '999');
    const { result } = renderHook(() =>
      useServerPaging({ key: KEY, sizes: SERVER_PAGE_SIZES, fallback: 50 }));
    expect(result.current.size).toBe(50);
  });
});
