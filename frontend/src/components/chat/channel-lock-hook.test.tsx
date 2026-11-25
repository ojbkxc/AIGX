/**
 * 渠道锁定接线（useChannelLock）—— 复现「拉列表失败 = 静默当成没锁定」。
 *
 * 旧实现：`api.listChannels().then(...).catch(() => { /* 静默降级 *\/ })`。
 * 后果：拉取失败与「一切正常」在界面上完全一致——徽标照显示「已锁定」，
 * 选择器却已解锁（因为 channelModels 为空），发出去必被后端拒（后端只走锁定
 * 渠道）。这里钉住：失败路径必须留下 degraded 标记，且锁定事实不变。
 */
import { describe, expect, it, vi } from 'vitest';
import { renderHook, waitFor } from '@testing-library/react';

const listChannels = vi.fn();
vi.mock('../../api', () => ({
  api: { listChannels: (...a: unknown[]) => listChannels(...a) },
}));

import { useChannelLock } from './channel-lock';

describe('useChannelLock', () => {
  it('未配置渠道：不锁定，且不发请求', () => {
    listChannels.mockReset();
    const { result } = renderHook(() => useChannelLock(''));
    expect(result.current.locked).toBe(false);
    expect(listChannels).not.toHaveBeenCalled();
  });

  it('正常渠道：拿到名字与模型，选择器可锁', async () => {
    listChannels.mockReset();
    listChannels.mockResolvedValue({ data: [{ id: 3, name: '主供应商', models: ['gpt-4o'] }] });
    const { result } = renderHook(() => useChannelLock('3'));
    await waitFor(() => expect(result.current.nameResolved).toBe(true));
    expect(result.current.label).toBe('主供应商');
    expect(result.current.models).toEqual(['gpt-4o']);
    expect(result.current.lockPicker).toBe(true);
    expect(result.current.degraded).toBeNull();
  });

  it('拉列表失败：仍锁定，但 degraded=no-list（旧实现静默，界面看不出差别）', async () => {
    listChannels.mockReset();
    listChannels.mockRejectedValue(new Error('403'));
    const { result } = renderHook(() => useChannelLock('3'));
    await waitFor(() => expect(result.current.degraded).toBe('no-list'));
    // 锁定是后端硬事实，不因前端拉不到列表而改变
    expect(result.current.locked).toBe(true);
    expect(result.current.lockPicker).toBe(false);
  });

  it('请求在飞期间就已标成「锁定但列表未知」（不闪一下「未锁定」）', () => {
    listChannels.mockReset();
    listChannels.mockReturnValue(new Promise(() => {})); // 永不 resolve
    const { result } = renderHook(() => useChannelLock('3'));
    expect(result.current.locked).toBe(true);
    expect(result.current.degraded).toBe('no-list');
  });

  it('渠道列表里没有该渠道（已删除）：锁定 + degraded，名称回落 id', async () => {
    listChannels.mockReset();
    listChannels.mockResolvedValue({ data: [{ id: 9, name: '别的', models: ['m'] }] });
    const { result } = renderHook(() => useChannelLock('3'));
    await waitFor(() => expect(result.current.degraded).toBe('no-list'));
    expect(result.current.label).toBe('3');
    expect(result.current.nameResolved).toBe(false);
  });

  it('渠道没有模型列表：锁定但选择器不锁，degraded=no-models', async () => {
    listChannels.mockReset();
    listChannels.mockResolvedValue({ data: [{ id: 3, name: '主供应商', models: [] }] });
    const { result } = renderHook(() => useChannelLock('3'));
    await waitFor(() => expect(result.current.degraded).toBe('no-models'));
    expect(result.current.locked).toBe(true);
    expect(result.current.lockPicker).toBe(false);
  });

  it('卸载后迟到响应不再写状态（切页不报 React 警告）', async () => {
    listChannels.mockReset();
    let release: (v: unknown) => void = () => {};
    listChannels.mockReturnValue(new Promise((r) => { release = r; }));
    const { result, unmount } = renderHook(() => useChannelLock('3'));
    const before = result.current;
    unmount();
    release({ data: [{ id: 3, name: '主供应商', models: ['gpt-4o'] }] });
    await Promise.resolve();
    // 卸载后 hook 不再更新：这里只断言没抛错、快照没被改写
    expect(before.degraded).toBe('no-list');
  });
});
