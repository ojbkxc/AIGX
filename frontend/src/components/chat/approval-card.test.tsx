/**
 * 审批卡状态机（approval-card）—— 复现「先关卡片再发请求」的三个后果。
 *
 * 后端语义（src/agent/runner.rs、src/agent/api.rs）：
 * - 高危工具挂起 → SSE `approval_request`；决策或**超时**后必发
 *   `approval_resolved{approved}`，超时按 Denied 处理；
 * - `POST /api/agent/approvals/:id` 的 `success:false` = request_id 已不在挂起表。
 *
 * 旧实现：`setPending(null)` 在 await 之前 → 提交失败卡片已消失、没有重试对象；
 * 提交中三个按钮全可点 → 连点两次向同一 request_id 发两次 allow；
 * `approval_resolved` 到达时不清 pending → 超时后按钮仍可点（点下去必然
 * success:false，而前端不读 success，看起来像成功）。
 */
import { describe, expect, it } from 'vitest';
import {
  approvalActionsEnabled,
  approvalPhaseLabel,
  approvalVisible,
  approveFailureEvent,
  approveResultEvent,
  IDLE_APPROVAL,
  reduceApproval,
  type ApprovalState,
} from './approval-card';

function pending(): ApprovalState {
  return reduceApproval(IDLE_APPROVAL, {
    type: 'request',
    requestId: 'req-1',
    name: 'update_channel',
    args: '{"id":3}',
  });
}

describe('审批状态机 — 提交与重复提交', () => {
  it('收到 request 进入 idle 且可点', () => {
    const s = pending();
    expect(s.phase).toBe('idle');
    expect(approvalVisible(s)).toBe(true);
    expect(approvalActionsEnabled(s)).toBe(true);
  });

  it('提交后进 submitting 且按钮全禁（旧实现提交中仍可点 → 重复提交）', () => {
    const s = reduceApproval(pending(), { type: 'submit', action: 'allow' });
    expect(s.phase).toBe('submitting');
    expect(approvalActionsEnabled(s)).toBe(false);
  });

  it('submitting 期间再次 submit 是幂等的（连点两次只发一次 allow）', () => {
    const once = reduceApproval(pending(), { type: 'submit', action: 'allow' });
    const twice = reduceApproval(once, { type: 'submit', action: 'allow' });
    expect(twice).toBe(once);
    expect(twice.phase).toBe('submitting');
  });

  it('提交失败保留卡片可重试（旧实现清 pending → 卡片消失）', () => {
    const submitting = reduceApproval(pending(), { type: 'submit', action: 'allow' });
    const failed = reduceApproval(submitting, { type: 'submitFailed', message: '网络错误' });
    expect(failed.phase).toBe('error');
    expect(failed.requestId).toBe('req-1');
    expect(approvalVisible(failed)).toBe(true);
    expect(failed.error).toBe('网络错误');
    // 可重试
    expect(approvalActionsEnabled(failed)).toBe(true);
    expect(failed.lastAction).toBe('allow');
  });

  it('失败后重试同一动作 → 回到 submitting', () => {
    const failed = reduceApproval(
      reduceApproval(pending(), { type: 'submit', action: 'deny' }),
      { type: 'submitFailed', message: 'boom' },
    );
    const retry = reduceApproval(failed, { type: 'submit', action: 'deny' });
    expect(retry.phase).toBe('submitting');
    expect(retry.error).toBeNull();
  });
});

describe('审批状态机 — 终态与超时', () => {
  it('服务端确认允许 → allowed，按钮禁用', () => {
    const s = reduceApproval(
      reduceApproval(pending(), { type: 'submit', action: 'allow' }),
      { type: 'resolve', approved: true },
    );
    expect(s.phase).toBe('allowed');
    expect(approvalActionsEnabled(s)).toBe(false);
  });

  it('本端点了拒绝 → denied', () => {
    const s = reduceApproval(
      reduceApproval(pending(), { type: 'submit', action: 'deny' }),
      { type: 'resolve', approved: false },
    );
    expect(s.phase).toBe('denied');
  });

  it('未提交就收到 approved:false → expired（后端超时按 Denied 处理）', () => {
    // 原缺陷：这种 resolve 只往消息流里推了一条 step，pending 不清，
    // 用户还能对已失效的 request_id 点「允许」
    const s = reduceApproval(pending(), { type: 'resolve', approved: false });
    expect(s.phase).toBe('expired');
    expect(approvalActionsEnabled(s)).toBe(false);
  });

  it('超时后点按钮无效（阶段不是 idle/error，submit 被忽略）', () => {
    const expired = reduceApproval(pending(), { type: 'resolve', approved: false });
    const after = reduceApproval(expired, { type: 'submit', action: 'allow' });
    expect(after).toBe(expired);
  });

  it('迟到的失败回调不覆盖已落终态', () => {
    const allowed = reduceApproval(
      reduceApproval(pending(), { type: 'submit', action: 'allow' }),
      { type: 'resolve', approved: true },
    );
    const after = reduceApproval(allowed, { type: 'submitFailed', message: '迟到的失败' });
    expect(after.phase).toBe('allowed');
  });
});

describe('审批状态机 — 撤销与重置', () => {
  it('切会话/停止时撤销：卡片转 revoked 且按钮禁用（不再指向旧 request_id）', () => {
    const s = reduceApproval(pending(), { type: 'revoke' });
    expect(s.phase).toBe('revoked');
    expect(approvalActionsEnabled(s)).toBe(false);
  });

  it('已撤销后收到 resolve 不再改状态（避免把撤销写成「已允许」）', () => {
    const revoked = reduceApproval(pending(), { type: 'revoke' });
    expect(reduceApproval(revoked, { type: 'resolve', approved: true })).toBe(revoked);
  });

  it('reset 清空 requestId，卡片从界面移除', () => {
    const s = reduceApproval(pending(), { type: 'reset' });
    expect(s.requestId).toBeNull();
    expect(approvalVisible(s)).toBe(false);
  });

  it('同一 request_id 重复下发（重连/重放）不重置已提交状态', () => {
    const submitting = reduceApproval(pending(), { type: 'submit', action: 'allow' });
    const again = reduceApproval(submitting, {
      type: 'request', requestId: 'req-1', name: 'update_channel', args: '{"id":3}',
    });
    expect(again).toBe(submitting);
  });

  it('新 request_id 接管：旧卡片被替换成新的待批', () => {
    const s = reduceApproval(pending(), {
      type: 'request', requestId: 'req-2', name: 'delete_user', args: '{}',
    });
    expect(s.requestId).toBe('req-2');
    expect(s.name).toBe('delete_user');
    expect(s.phase).toBe('idle');
    expect(s.lastAction).toBeNull();
  });
});

describe('后端响应翻译 — success:false 不能被当成成功', () => {
  /**
   * 后端 `POST /api/agent/approvals/:id` 成功失败都是 HTTP 200，差别只在
   * `success`。旧实现只 catch 抛错，从不读 success：超时后才点「允许」时
   * 界面照样关卡片，用户以为批准了，工具其实没执行。
   */
  it('success:true + allow → 已允许', () => {
    const s = reduceApproval(pending(), approveResultEvent('allow', { success: true }));
    expect(s.phase).toBe('allowed');
  });

  it('success:true + deny → 已拒绝', () => {
    const s = reduceApproval(pending(), approveResultEvent('deny', { success: true }));
    expect(s.phase).toBe('denied');
  });

  it('success:false（request_id 已不在挂起表）+ allow → 已过期，不是已允许', () => {
    const submitting = reduceApproval(pending(), { type: 'submit', action: 'allow' });
    const s = reduceApproval(submitting, approveResultEvent('allow', { success: false, data: null }));
    expect(s.phase).toBe('expired');
    expect(approvalActionsEnabled(s)).toBe(false);
  });

  it('success:false + deny → 已拒绝（本端点确实拒绝过，只是没命中挂起项）', () => {
    const submitting = reduceApproval(pending(), { type: 'submit', action: 'deny' });
    const s = reduceApproval(submitting, approveResultEvent('deny', { success: false }));
    expect(s.phase).toBe('denied');
  });

  it('响应缺 success 字段时按成功处理（老后端兼容，不误判过期）', () => {
    const s = reduceApproval(pending(), approveResultEvent('allow', { data: null }));
    expect(s.phase).toBe('allowed');
  });

  it('网络异常 → 保留卡片可重试', () => {
    const submitting = reduceApproval(pending(), { type: 'submit', action: 'remember' });
    const s = reduceApproval(submitting, approveFailureEvent(new Error('Failed to fetch')));
    expect(s.phase).toBe('error');
    expect(s.requestId).toBe('req-1');
    expect(s.error).toBe('Failed to fetch');
  });
});

describe('展示判据', () => {  it('终态都有中文原文 key（缺 key 会回落中文，界面不空）', () => {
    expect(approvalPhaseLabel('submitting')).toBe('处理中');
    expect(approvalPhaseLabel('allowed')).toBe('已允许');
    expect(approvalPhaseLabel('denied')).toBe('已拒绝');
    expect(approvalPhaseLabel('expired')).toBe('已过期');
    expect(approvalPhaseLabel('revoked')).toBe('已失效');
    expect(approvalPhaseLabel('error')).toBe('操作失败');
    expect(approvalPhaseLabel('idle')).toBe('');
  });

  it('没有 requestId 时按钮一律禁用', () => {
    expect(approvalActionsEnabled(IDLE_APPROVAL)).toBe(false);
    expect(approvalVisible(IDLE_APPROVAL)).toBe(false);
  });
});
