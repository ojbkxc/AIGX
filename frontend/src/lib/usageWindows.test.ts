import { describe, expect, it } from 'vitest';
import { mergeUsageWindows } from './usageWindows';
import type { AccountUsageWindow } from '../types';

function win(over: Partial<AccountUsageWindow>): AccountUsageWindow {
  return { ...over };
}

describe('mergeUsageWindows', () => {
  it('普通时间窗口原样透传（不误合并）', () => {
    const input = [
      win({ group: 'Pro', window: '5h', remaining_percent: 80, resets_at: 100 }),
      win({ group: 'Pro', window: 'weekly', remaining_percent: 40, resets_at: 200 }),
    ];
    const out = mergeUsageWindows(input);
    expect(out).toHaveLength(2);
    expect(out[0]).toMatchObject({ window: '5h', remaining_percent: 80 });
    expect(out[1]).toMatchObject({ window: 'weekly', remaining_percent: 40 });
  });

  it('同一基础模型的不同思考等级合并为一条，取最紧剩余比例', () => {
    const input = [
      win({ label: 'gpt-5-high', window: 'weekly', remaining_percent: 70, resets_at: 500 }),
      win({ label: 'gpt-5-low', window: 'weekly', remaining_percent: 20, resets_at: 400 }),
      win({ label: 'gpt-5-medium', window: 'weekly', remaining_percent: 45, resets_at: 600 }),
    ];
    const out = mergeUsageWindows(input);
    expect(out).toHaveLength(1);
    expect(out[0].label).toBe('gpt-5');
    expect(out[0].remaining_percent).toBe(20);
    // 最紧那行的更早有效重置时间被保留
    expect(out[0].resets_at).toBe(400);
  });

  it('并列最紧时保留更早的重置时间', () => {
    const input = [
      win({ label: 'claude-xhigh', window: 'weekly', remaining_percent: 30, resets_at: 900 }),
      win({ label: 'claude-low', window: 'weekly', remaining_percent: 30, resets_at: 300 }),
    ];
    const out = mergeUsageWindows(input);
    expect(out).toHaveLength(1);
    expect(out[0].resets_at).toBe(300);
  });

  it('缺失重置时间由另一行补齐', () => {
    const input = [
      win({ label: 'mx-high', window: 'daily', remaining_percent: 10, resets_at: null }),
      win({ label: 'mx-low', window: 'daily', remaining_percent: 50, resets_at: 1234 }),
    ];
    const out = mergeUsageWindows(input);
    expect(out).toHaveLength(1);
    expect(out[0].remaining_percent).toBe(10);
    expect(out[0].resets_at).toBe(1234);
  });

  it('括号形态思考等级也能识别', () => {
    const input = [
      win({ group: 'Team', label: 'sonnet (high)', window: '5h', remaining_percent: 60 }),
      win({ group: 'Team', label: 'sonnet（low）', window: '5h', remaining_percent: 15 }),
    ];
    const out = mergeUsageWindows(input);
    expect(out).toHaveLength(1);
    expect(out[0].label).toBe('sonnet');
    expect(out[0].remaining_percent).toBe(15);
  });

  it('不同基础模型互不合并', () => {
    const input = [
      win({ label: 'gpt-5-high', window: 'weekly', remaining_percent: 60 }),
      win({ label: 'gpt-4o-low', window: 'weekly', remaining_percent: 50 }),
    ];
    expect(mergeUsageWindows(input)).toHaveLength(2);
  });

  it('无标识行不合并，按顺序保留', () => {
    const input: AccountUsageWindow[] = [
      { remaining_percent: 90 },
      { remaining_percent: 10 },
    ];
    expect(mergeUsageWindows(input)).toHaveLength(2);
  });

  it('不修改入参', () => {
    const input = [win({ label: 'gpt-5-high', window: 'weekly', remaining_percent: 70 })];
    mergeUsageWindows(input);
    expect(input[0].label).toBe('gpt-5-high');
  });
});
