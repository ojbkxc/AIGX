/**
 * 审批卡接线 hook（useAgentApproval）—— 复现三处「挂载顺序写错就坏」的形态。
 *
 * 这一层存在的理由：状态机再正确，挂载方只要写成「先关卡片再发请求」或
 * 「不看 success 一律当成功」，缺陷就原样回来。所以提交顺序由 hook 单点负责，
 * 这里用注入的 submit 逐条钉住：
 * 1. 连点两次只发一次请求（submit 期间按钮/调用被拦）；
 * 2. `success:false`（request_id 已不在挂起表）→ 已过期，不是「已允许」；
 * 3. 抛错 → 卡片保留可重试，并把原因抛给外层错误条。
 */
import { describe, expect, it, vi } from 'vitest';
import { act, renderHook } from '@testing-library/react';

// hook 内部默认走 api.agentApprove；本测一律注入 submit，模块只需可导入
vi.mock('../../api', () => ({
  api: { agentApprove: vi.fn() },
}));

import { useAgentApproval } from './approval-card';

describe('useAgentApproval', () => {
  it('request 进入待批；respond 后进处理中；成功后进已允许', async () => {
    const submit = vi.fn().mockResolvedValue({ success: true, data: null });
    const { result } = renderHook(() => useAgentApproval({ submit }));

    act(() => result.current.request('req-1', 'update_channel', '{}'));
    expect(result.current.state.phase).toBe('idle');

    await act(async () => { result.current.respond('allow'); });
    expect(submit).toHaveBeenCalledWith('req-1', 'allow');
    expect(result.current.state.phase).toBe('allowed');
  });

  it('连点两次只发一次请求（原实现提交中仍可点 → 重复提交）', async () => {
    let release: (v: unknown) => void = () => {};
    const submit = vi.fn(() => new Promise<unknown>((r) => { release = r; }));
    const { result } = renderHook(() => useAgentApproval({ submit }));

    act(() => result.current.request('req-2', 'delete_user', '{}'));
    act(() => { result.current.respond('allow'); result.current.respond('allow'); });
    expect(submit).toHaveBeenCalledTimes(1);

    await act(async () => { release({ success: true }); await Promise.resolve(); });
    expect(result.current.state.phase).toBe('allowed');
  });

  it('success:false + allow → 已过期（旧实现按成功关卡片）', async () => {
    const submit = vi.fn().mockResolvedValue({ success: false, data: null });
    const { result } = renderHook(() => useAgentApproval({ submit }));

    act(() => result.current.request('req-3', 'update_channel', '{}'));
    await act(async () => { result.current.respond('allow'); });
    expect(result.current.state.phase).toBe('expired');
  });

  it('抛错 → 卡片保留在 error 可重试，并回调 onError', async () => {
    const submit = vi.fn().mockRejectedValue(new Error('Failed to fetch'));
    const onError = vi.fn();
    const { result } = renderHook(() => useAgentApproval({ submit, onError }));

    act(() => result.current.request('req-4', 'update_channel', '{}'));
    await act(async () => { result.current.respond('deny'); });

    expect(result.current.state.phase).toBe('error');
    expect(result.current.state.requestId).toBe('req-4');
    expect(onError).toHaveBeenCalledWith('Failed to fetch');
  });

  it('error 后重试能回到提交并成功', async () => {
    const submit = vi.fn()
      .mockRejectedValueOnce(new Error('boom'))
      .mockResolvedValueOnce({ success: true });
    const { result } = renderHook(() => useAgentApproval({ submit }));

    act(() => result.current.request('req-5', 'update_channel', '{}'));
    await act(async () => { result.current.respond('allow'); });
    expect(result.current.state.phase).toBe('error');

    await act(async () => { result.current.respond('allow'); });
    expect(result.current.state.phase).toBe('allowed');
    expect(submit).toHaveBeenCalledTimes(2);
  });

  it('session resolve(false) → 已过期，按钮随即不可点', async () => {
    const { result } = renderHook(() => useAgentApproval({ submit: vi.fn() }));
    act(() => result.current.request('req-6', 'update_channel', '{}'));
    act(() => result.current.resolve(false));
    expect(result.current.state.phase).toBe('expired');
  });

  it('revoke（切会话/停止）→ 已失效，之后的 resolve 不再改写成已允许', () => {
    const { result } = renderHook(() => useAgentApproval({ submit: vi.fn() }));
    act(() => result.current.request('req-7', 'update_channel', '{}'));
    act(() => result.current.revoke());
    expect(result.current.state.phase).toBe('revoked');
    act(() => result.current.resolve(true));
    expect(result.current.state.phase).toBe('revoked');
  });

  it('reset 清空，卡片从界面移除', () => {
    const { result } = renderHook(() => useAgentApproval({ submit: vi.fn() }));
    act(() => result.current.request('req-8', 'update_channel', '{}'));
    act(() => result.current.reset());
    expect(result.current.state.requestId).toBeNull();
  });

  it('卸载后不再 setState（审批请求可能比组件活得久）', async () => {
    let release: (v: unknown) => void = () => {};
    const submit = vi.fn(() => new Promise<unknown>((r) => { release = r; }));
    const onError = vi.fn();
    const { result, unmount } = renderHook(() => useAgentApproval({ submit, onError }));

    act(() => result.current.request('req-9', 'update_channel', '{}'));
    act(() => { result.current.respond('allow'); });
    unmount();
    // 卸载后才回来的响应/错误：不应再触发状态写入（这里只断言不抛、不回调）
    await act(async () => { release({ success: true }); await Promise.resolve(); });
    expect(onError).not.toHaveBeenCalled();
  });
});
