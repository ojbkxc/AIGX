import { describe, expect, it } from 'vitest';
import {
  AGENT_EFFORT_LEVELS,
  modelAvailability,
  modelNames,
  modelSupportsImages,
  normalizeEffort,
  reconcileEffort,
  resolveEffortOptions,
} from './asst-model';

describe('asst-model — 思考等级候选', () => {
  it('模型声明的档位优先，且按后端顺序重排', () => {
    const resolved = resolveEffortOptions({ declared: ['high', 'off', 'medium'] });
    expect(resolved).toEqual({ options: ['off', 'medium', 'high'], source: 'model', dropped: [] });
  });

  it('模型声明了本网关不认的档位：如实丢弃并报出来，不静默降级', () => {
    const resolved = resolveEffortOptions({ declared: ['minimal', 'xhigh'] });
    expect(resolved.options).toEqual([]);
    expect(resolved.source).toBe('model');
    expect(resolved.dropped).toEqual(['minimal', 'xhigh']);
  });

  it('部分可认时只留可认的（不因为有一个不认就整表丢掉）', () => {
    const resolved = resolveEffortOptions({ declared: ['off', 'max', 'high'] });
    expect(resolved.options).toEqual(['off', 'high']);
    expect(resolved.dropped).toEqual(['max']);
  });

  it('模型未声明 → 网关候选 → 本地四档（与 Agent 后端值域一致）', () => {
    expect(resolveEffortOptions({ gateway: ['low', 'high'] })).toEqual({
      options: ['low', 'high'],
      source: 'gateway',
      dropped: [],
    });
    expect(resolveEffortOptions()).toEqual({
      options: [...AGENT_EFFORT_LEVELS],
      source: 'fallback',
      dropped: [],
    });
  });

  it('任意档位串 → 最近的合法档；未知值落到 off 而不是 high', () => {
    expect(normalizeEffort('HIGH')).toBe('high');
    expect(normalizeEffort('none')).toBe('off');
    expect(normalizeEffort('minimal')).toBe('low');
    expect(normalizeEffort('max')).toBe('high');
    expect(normalizeEffort('weird')).toBe('off');
    expect(normalizeEffort(undefined)).toBe('off');
  });

  it('切换模型后收敛：当前档不在候选里就回 off（候选没有 off 时取第一项）', () => {
    expect(reconcileEffort('high', ['off', 'low'])).toBe('off');
    expect(reconcileEffort('high', ['off', 'high'])).toBe('high');
    expect(reconcileEffort('high', ['low'])).toBe('low');
    expect(reconcileEffort('high', [])).toBe('off');
  });
});

describe('asst-model — 模型列表降级', () => {
  it('去空、去重、保序', () => {
    expect(modelNames([{ id: 'a' }, { id: 'a' }, { id: '' }, { id: ' b ' }, {}])).toEqual(['a', 'b']);
    expect(modelNames(undefined)).toEqual([]);
  });

  it('列表为空 = 未知（不判不可用，避免一刷新就把已选模型说成失效）', () => {
    expect(modelAvailability([], 'x')).toEqual({ available: true, unknown: true });
    expect(modelAvailability(undefined, 'x')).toEqual({ available: true, unknown: true });
  });

  it('列表有值时才判可用', () => {
    expect(modelAvailability(['a', 'b'], 'b')).toEqual({ available: true, unknown: false });
    expect(modelAvailability(['a'], 'z')).toEqual({ available: false, unknown: false });
    expect(modelAvailability(['a'], '')).toEqual({ available: false, unknown: false });
  });

  it('读图能力：三态能力位优先，其次能力数组，都没有 = 不知道', () => {
    expect(modelSupportsImages({ capability_flags: { supportsImages: false } })).toBe(false);
    expect(modelSupportsImages({ capability_flags: { supportsImages: null }, capabilities: ['vision'] })).toBe(true);
    expect(modelSupportsImages({ capabilities: ['text'] })).toBeUndefined();
    expect(modelSupportsImages(null)).toBeUndefined();
  });
});
