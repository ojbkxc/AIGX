import { describe, expect, it, beforeEach } from 'vitest';
import { clearFilter, getFilter, setFilter } from './filterMemory';

const KEY = 'test-filter-memory';
const DEFAULTS = { state: 'all', sort: 'default', keyword: '' };

beforeEach(() => {
  localStorage.clear();
});

describe('getFilter', () => {
  it('无存盘时返回默认值', () => {
    expect(getFilter(KEY, DEFAULTS)).toEqual(DEFAULTS);
  });

  it('存盘值按默认键收窄合并', () => {
    setFilter(KEY, { state: 'on', keyword: 'gpt' });
    expect(getFilter(KEY, DEFAULTS)).toEqual({ state: 'on', sort: 'default', keyword: 'gpt' });
  });

  it('非字符串存盘值回落默认', () => {
    localStorage.setItem(KEY, JSON.stringify({ state: 123, sort: 'priority' }));
    expect(getFilter(KEY, DEFAULTS)).toEqual({ state: 'all', sort: 'priority', keyword: '' });
  });

  it('存坏 JSON 回落全默认', () => {
    localStorage.setItem(KEY, '{not json');
    expect(getFilter(KEY, DEFAULTS)).toEqual(DEFAULTS);
  });

  it('数组 / 标量存盘值视为存坏', () => {
    localStorage.setItem(KEY, JSON.stringify([1, 2]));
    expect(getFilter(KEY, DEFAULTS)).toEqual(DEFAULTS);
  });

  it('不给 defaults 返回存盘原值，缺失返回 null', () => {
    localStorage.setItem(KEY, JSON.stringify({ state: 'on' }));
    expect(getFilter<{ state: string }>(KEY)).toEqual({ state: 'on' });
    clearFilter(KEY);
    expect(getFilter(KEY)).toBeNull();
  });
});

describe('setFilter', () => {
  it('合并写回：覆盖存量键，保留其余维度', () => {
    setFilter(KEY, { state: 'on', sort: 'priority' });
    setFilter(KEY, { state: 'off' });
    expect(getFilter(KEY, { state: 'all', sort: 'default' })).toEqual({ state: 'off', sort: 'priority' });
  });

  it('存量存坏时以 patch 为准', () => {
    localStorage.setItem(KEY, '{broken');
    setFilter(KEY, { state: 'on' });
    expect(getFilter(KEY, { state: 'all' })).toEqual({ state: 'on' });
  });

  it('写空 patch 不破坏存量', () => {
    setFilter(KEY, { state: 'on' });
    setFilter(KEY, {});
    expect(getFilter(KEY, { state: 'all' })).toEqual({ state: 'on' });
  });
});

describe('clearFilter', () => {
  it('清空后回落默认', () => {
    setFilter(KEY, { state: 'on' });
    clearFilter(KEY);
    expect(getFilter(KEY, DEFAULTS)).toEqual(DEFAULTS);
  });

  it('清空不存在的键不抛错', () => {
    expect(() => clearFilter('test-filter-memory-absent')).not.toThrow();
  });
});
