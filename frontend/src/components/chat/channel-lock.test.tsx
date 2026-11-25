/**
 * 渠道锁定（channel-lock）—— 复现「徽标说已锁定、选择器实际解锁」的自相矛盾。
 *
 * 旧实现（pages/Agent.tsx）：
 *   lockToChannel={!!(agentChannel && channelModels.length > 0)}
 *   .catch(() => { /* 渠道列表失败静默降级 *\/ })
 * 拉列表失败或渠道无模型列表时，徽标仍渲染「已锁定」，但选择器解锁且
 * **没有任何提示**；发出去必被后端拒（后端只走锁定渠道）。
 */
import { describe, expect, it } from 'vitest';
import { channelLockBadge, resolveChannelLock } from './channel-lock';

const CHANNELS = [
  { id: 3, name: '主供应商', models: ['gpt-4o', 'claude-3'], discovered_models: ['gpt-4o', 'o3'] },
  { id: 7, name: '备用', models: [] },
];

describe('resolveChannelLock', () => {
  it('未配置渠道：不锁定', () => {
    const s = resolveChannelLock('', CHANNELS);
    expect(s.locked).toBe(false);
    expect(channelLockBadge(s)).toBeNull();
  });

  it('空白字符串也算未配置（避免 " " 触发锁定）', () => {
    expect(resolveChannelLock('   ', CHANNELS).locked).toBe(false);
  });

  it('正常锁定：取渠道名、模型去重保序、选择器可锁', () => {
    const s = resolveChannelLock('3', CHANNELS);
    expect(s.locked).toBe(true);
    expect(s.label).toBe('主供应商');
    expect(s.nameResolved).toBe(true);
    expect(s.models).toEqual(['gpt-4o', 'claude-3', 'o3']);
    expect(s.lockPicker).toBe(true);
    expect(s.degraded).toBeNull();
  });

  it('渠道列表拉取失败（null）：仍锁定，但显式降级不静默', () => {
    const s = resolveChannelLock('3', null);
    expect(s.locked).toBe(true);
    expect(s.lockPicker).toBe(false);
    expect(s.degraded).toBe('no-list');
    expect(channelLockBadge(s)?.titleKey).toBe('已锁定该供应商（渠道列表加载失败，模型列表未知）');
  });

  it('列表里找不到该渠道（已删/被禁用）：锁定但降级，名称回落 id 且标明', () => {
    const s = resolveChannelLock('99', CHANNELS);
    expect(s.locked).toBe(true);
    expect(s.label).toBe('99');
    expect(s.nameResolved).toBe(false);
    expect(s.degraded).toBe('no-list');
  });

  it('渠道没有模型列表：锁定但选择器不锁，给出 no-models 提示', () => {
    const s = resolveChannelLock('7', CHANNELS);
    expect(s.locked).toBe(true);
    expect(s.lockPicker).toBe(false);
    expect(s.degraded).toBe('no-models');
    expect(channelLockBadge(s)?.titleKey).toBe('已锁定该供应商（该渠道未配置模型列表，选择器不锁定）');
  });

  it('模型列表含空白项时被丢弃（不产生空选项）', () => {
    const s = resolveChannelLock('5', [{ id: '5', name: 'x', models: ['', '  ', 'm1'] }]);
    expect(s.models).toEqual(['m1']);
  });

  it('数字 id 与字符串配置值互通（后端下发数字 id）', () => {
    expect(resolveChannelLock('3', CHANNELS).nameResolved).toBe(true);
  });
});

describe('channelLockBadge', () => {
  it('正常锁定给出原文提示 key', () => {
    const s = resolveChannelLock('3', CHANNELS);
    expect(channelLockBadge(s)).toEqual({
      text: '主供应商',
      titleKey: 'Agent 已锁定供应商，只走该供应商的请求',
    });
  });

  it('未锁定时不渲染徽标', () => {
    expect(channelLockBadge(resolveChannelLock(null, CHANNELS))).toBeNull();
  });
});
