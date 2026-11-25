/**
 * 请求日志详情 —— 「供应商」一行的取值口径。
 *
 * 背景（线上现象）：日志页的「供应商」列此前读的是 `channel_name`（渠道/账号池
 * 的**名字**），而「是哪一家上游扛的」这个维度（后端 `provider`：渠道型=协议族，
 * 账号池型=账号的家 trae / xiaohuanxiong …）从来没落过日志，于是这一列在有渠道
 * 名时显示渠道名、没渠道名时一律「—」，管理员无法据此判断「这次是哪一家」。
 *
 * 本测试钉住三件事：
 * 1. 有 `provider` 时**必须显示它**（不能因为缺 `channel_name` 就显示「—」——
 *    「有数据也显示 —」正是要修的那个 bug 形态）；
 * 2. 有 `provider` 时 `channel_name` 仍作为归属信息保留（两个维度都不丢）；
 * 3. 老日志（无 `provider` 键）逐字回退到改造前的显示（无回归）。
 *
 * 语言：测试环境初始语言取自 navigator（jsdom 是 en-US），故标签一律按
 * 「取到的那一行」断言，不依赖「供应商」这个中文标签的翻译结果。
 */
import { describe, expect, it, vi } from 'vitest';
import { render, screen } from '@testing-library/react';
import { RequestLogDetail } from './LogDetailDialogs';
import type { RequestLogItem } from '../pages/Logs';
import '../i18n';

function baseLog(over: Partial<RequestLogItem>): RequestLogItem {
  return {
    id: 'req-1',
    created_at: 1_700_000_000,
    model: 'gpt-4o',
    status_code: 200,
    latency_ms: 120,
    input_tokens: 10,
    output_tokens: 20,
    cost: 5,
    ...over,
  };
}

function renderDetail(log: RequestLogItem): void {
  render(<RequestLogDetail log={log} admin onClose={vi.fn()} />);
}

/** 取「供应商」那一行的值（按 label 找行，再取同行 value 单元格）。 */
function providerRowValue(): string {
  const label = screen
    .getAllByText(/^(供应商|Provider)$/)
    .find((node) => node.className.includes('log-detail-label'));
  expect(label, '详情里应有「供应商」一行').toBeTruthy();
  const value = label!.parentElement?.querySelector('.log-detail-value');
  return (value?.textContent ?? '').trim();
}

describe('RequestLogDetail 供应商行', () => {
  it('账号池型：显示 provider（家名），渠道名作括号归属', () => {
    renderDetail(
      baseLog({ provider: 'trae', channel_name: 'Trae 账号池', channel_id: 'ch-pool' }),
    );
    const value = providerRowValue();
    expect(value).toContain('trae');
    // 两个维度都在：供应商在前，渠道名/ID 作归属
    expect(value).toContain('Trae 账号池');
    expect(value).toContain('ch-pool');
  });

  it('渠道型：显示协议族（不是渠道名），不因渠道名不同而变形', () => {
    renderDetail(
      baseLog({ provider: 'anthropic', channel_name: '主力 Claude', channel_id: 'ch-9' }),
    );
    const value = providerRowValue();
    expect(value.startsWith('anthropic')).toBe(true);
    expect(value).toContain('主力 Claude');
  });

  it('有 provider 但没有 channel_name：仍显示 provider，绝不显示「—」', () => {
    // 这条正是「有数据也显示 —」的回归判据：渠道名缺失不该把供应商也吞掉
    renderDetail(baseLog({ provider: 'xiaohuanxiong', channel_id: 'ch-x' }));
    const value = providerRowValue();
    expect(value).toContain('xiaohuanxiong');
    expect(value).not.toBe('—');
  });

  it('老日志（无 provider 键）：逐字回退到改造前的 channel_name 显示', () => {
    renderDetail(baseLog({ channel_name: '老渠道', channel_id: 'ch-old' }));
    const value = providerRowValue();
    expect(value).toContain('老渠道');
    expect(value).toContain('ch-old');
  });

  it('三者皆无：显示占位符「—」（真的没记录，不编造）', () => {
    renderDetail(baseLog({}));
    expect(providerRowValue()).toBe('—');
  });
});
