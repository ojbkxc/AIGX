import { describe, expect, it } from 'vitest';
import {
  appendStep,
  finalizeItems,
  itemsFromSteps,
  messageStateFor,
  phaseOverrides,
  stepFromEvent,
  stepsFromTrace,
  toToolCallStates,
} from './tool-steps';

const trace = [
  { type: 'tool_call', name: 'list_accounts', arguments: '{"limit":5}' },
  { type: 'tool_result', name: 'list_accounts', ok: true, text: '共 2 个' },
  { type: 'tool_call', name: 'create_key', arguments: '{"name":"x"}' },
  { type: 'approval_request', name: 'create_key', request_id: 'r1' },
  { type: 'approval_resolved', name: 'create_key', approved: true },
  { type: 'tool_result', name: 'create_key', ok: false, text: '{"ok":false,"error":"quota"}' },
];

describe('tool-steps — 轨迹解析', () => {
  it('落库的 tool_calls（含审批事件）逐条还原', () => {
    const steps = stepsFromTrace(trace);
    expect(steps.map((s) => s.kind)).toEqual(['call', 'result', 'call', 'approval', 'approval', 'result']);
    expect(steps[3]).toEqual({ kind: 'approval', name: 'create_key', approved: undefined, requestId: 'r1' });
    expect(steps[4]).toEqual({ kind: 'approval', name: 'create_key', approved: true });
  });

  it('坏条目跳过，不让一条脏数据吞掉整段轨迹', () => {
    const steps = stepsFromTrace([null, 1, {}, { type: 'tool_call' }, { type: 'tool_call', name: 'x' }]);
    expect(steps).toHaveLength(1);
    expect(steps[0]).toEqual({ kind: 'call', name: 'x', args: '' });
  });

  it('SSE 事件与落库轨迹同一套语义', () => {
    expect(stepFromEvent({ type: 'tool_call', name: 'a', arguments: '{}' }))
      .toEqual({ kind: 'call', name: 'a', args: '{}' });
    expect(stepFromEvent({ type: 'tool_result', name: 'a', ok: false, text: 'x' }))
      .toEqual({ kind: 'result', name: 'a', ok: false, text: 'x' });
    expect(stepFromEvent({ type: 'final', content: 'hi' })).toBeNull();
  });
});

describe('tool-steps — 调用与结果配对', () => {
  it('同名先进先出：两次调用各自拿到自己的结果', () => {
    const items = itemsFromSteps([
      { kind: 'call', name: 'f', args: '1' },
      { kind: 'call', name: 'f', args: '2' },
      { kind: 'result', name: 'f', ok: true, text: 'r1' },
      { kind: 'result', name: 'f', ok: false, text: 'r2' },
    ]);
    expect(items).toHaveLength(2);
    expect(items[0].result).toBe('r1');
    expect(items[0].phase).toBe('done');
    expect(items[1].result).toBe('r2');
    expect(items[1].phase).toBe('failed');
  });

  it('审批挂到对应调用上；拒绝即取消', () => {
    const items = itemsFromSteps([
      { kind: 'call', name: 'create_key', args: '{}' },
      { kind: 'approval', name: 'create_key', approved: undefined, requestId: 'r1' },
    ]);
    expect(items).toHaveLength(1);
    expect(items[0].phase).toBe('pending');
    expect(items[0].approvals).toEqual([{ approved: undefined, requestId: 'r1' }]);

    const denied = itemsFromSteps([
      { kind: 'call', name: 'create_key', args: '{}' },
      { kind: 'approval', name: 'create_key', approved: false, requestId: 'r2' },
    ]);
    expect(denied[0].phase).toBe('cancelled');
  });

  it('结果先到（轨迹被截断）也如实成条，不硬塞给别的调用', () => {
    const items = itemsFromSteps([{ kind: 'result', name: 'ghost', ok: true, text: 'x' }]);
    expect(items).toHaveLength(1);
    expect(items[0]).toMatchObject({ name: 'ghost', result: 'x', phase: 'done', args: '' });
  });

  it('索引与 ToolCallState.index 同义且连续', () => {
    const items = itemsFromSteps([
      { kind: 'call', name: 'a', args: '' },
      { kind: 'call', name: 'b', args: '' },
    ]);
    expect(items.map((i) => i.index)).toEqual([0, 1]);
  });
});

describe('tool-steps — 增量与收尾', () => {
  it('appendStep 与批量配对结果一致（同一条判据）', () => {
    const step1 = { kind: 'call', name: 'a', args: '{"x":1}' } as const;
    const step2 = { kind: 'result', name: 'a', ok: true, text: 'ok' } as const;
    const merged = appendStep(appendStep(appendStep([], step1), step2), { kind: 'call', name: 'b', args: '' });
    const batched = itemsFromSteps([step1, step2, { kind: 'call', name: 'b', args: '' }]);
    expect(merged).toEqual(batched);
  });

  it('收尾：仍在跑的按「完成」或「已取消」定性', () => {
    const items = itemsFromSteps([{ kind: 'call', name: 'a', args: '' }]);
    expect(finalizeItems(items)[0].phase).toBe('done');
    expect(finalizeItems(items, { cancelled: true })[0].phase).toBe('cancelled');
    const done = itemsFromSteps([{ kind: 'call', name: 'a', args: '' }, { kind: 'result', name: 'a', ok: true }]);
    expect(finalizeItems(done, { cancelled: true })[0].phase).toBe('done');
  });

  it('回灌给 ToolCallCard：calls + 阶段覆盖 + 消息级状态', () => {
    const items = itemsFromSteps(stepsFromTrace(trace));
    const calls = toToolCallStates(items);
    expect(calls.map((c) => c.index)).toEqual([0, 1]);
    expect(calls[1]).toEqual({ index: 1, name: 'create_key', arguments: '{"name":"x"}', result: '{"ok":false,"error":"quota"}' });
    expect(phaseOverrides(items)).toEqual({ 0: 'done', 1: 'failed' });
    expect(messageStateFor({ busy: true })).toBe('streaming');
    expect(messageStateFor({ cancelled: true })).toBe('interrupted');
    expect(messageStateFor({ failed: true })).toBe('error');
    expect(messageStateFor()).toBe('done');
  });
});
