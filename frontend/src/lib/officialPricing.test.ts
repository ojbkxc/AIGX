import { beforeEach, describe, expect, it } from 'vitest';
import {
  OFFICIAL_PRICE_SEED,
  OFFICIAL_PRICE_STORAGE_KEY,
  clearOfficialPrice,
  getLocalOverrides,
  getOfficialPrice,
  getOfficialPrices,
  normalizeModelName,
  saveOfficialPrice,
} from './officialPricing';

/**
 * 官方参考价的单测 —— 守卫「没有官方价就显示未登记，绝不凭空造折扣」这条判据。
 *
 * 背景：后端 `ModelPrice` 没有 official 字段、models.dev 快照也没有价格键，
 * 所以官方价是本机可选的参考表。这些用例钉住三件事：
 * 1. 种子条目按归一化名字能取到；
 * 2. 覆盖表优先于种子，清除后回到「未登记」；
 * 3. 存盘坏 / 存储不可用时不抛、不伪造值。
 */
describe('normalizeModelName', () => {
  it('大小写与空白归一：DeepSeek-V4.1-Flash 与 deepseek-v4.1-flash 同一个键', () => {
    expect(normalizeModelName(' DeepSeek-V4.1-Flash ')).toBe('deepseek-v4.1-flash');
    expect(normalizeModelName('DEEPSEEK-V4.1-FLASH')).toBe(normalizeModelName('deepseek-v4.1-flash'));
  });
});

describe('内置种子', () => {
  beforeEach(() => {
    localStorage.clear();
  });

  it('deepseek-v4.1-flash 的官方输入价是 3.0（用户口径：官方 ¥3.00，我们 1 折）', () => {
    const entry = getOfficialPrice('deepseek-v4.1-flash');
    expect(entry?.input).toBe(3.0);
  });

  it('大小写变体命中同一条种子', () => {
    expect(getOfficialPrice('DeepSeek-V4.1-Flash')?.input).toBe(3.0);
  });

  it('种子不臆造未给的维度：输出与缓存读是 null', () => {
    const entry = getOfficialPrice('deepseek-v4.1-flash');
    expect(entry?.output).toBeNull();
    expect(entry?.cache).toBeNull();
  });

  it('未登记的模型返回 null（界面显示「未登记」，不显示成 0 元）', () => {
    expect(getOfficialPrice('some-unlisted-model')).toBeNull();
  });

  it('种子表里的键都是归一化形态（防止写死大写导致查不到）', () => {
    for (const key of Object.keys(OFFICIAL_PRICE_SEED)) {
      expect(key).toBe(normalizeModelName(key));
    }
  });
});

describe('本机覆盖表', () => {
  beforeEach(() => {
    localStorage.clear();
  });

  it('保存后按模型名能取到，且覆盖种子值', () => {
    saveOfficialPrice('deepseek-v4.1-flash', { input: 4.0, output: 8.0, cache: 0.5 });
    const entry = getOfficialPrice('deepseek-v4.1-flash');
    expect(entry).toEqual({ input: 4.0, output: 8.0, cache: 0.5 });
  });

  it('保存用大写名，读取用小写名也能命中（同一把归一化刀）', () => {
    saveOfficialPrice('Qwen3.8-Flash', { input: 2.0, output: null, cache: null });
    expect(getOfficialPrice('qwen3.8-flash')?.input).toBe(2.0);
  });

  it('清除后回到「未登记」：种子里本来没有这条，清除后就是 null', () => {
    saveOfficialPrice('custom-model', { input: 1.5, output: null, cache: null });
    expect(getOfficialPrice('custom-model')?.input).toBe(1.5);
    clearOfficialPrice('custom-model');
    expect(getOfficialPrice('custom-model')).toBeNull();
  });

  it('getLocalOverrides 只含管理员填的条目，不含内置种子', () => {
    saveOfficialPrice('custom-model', { input: 1.5, output: null, cache: null });
    const overrides = getLocalOverrides();
    expect(overrides['custom-model']?.input).toBe(1.5);
    expect(overrides['deepseek-v4.1-flash']).toBeUndefined();
  });

  it('存盘坏 JSON 时回落种子，不抛', () => {
    localStorage.setItem(OFFICIAL_PRICE_STORAGE_KEY, '{not json');
    expect(getOfficialPrice('deepseek-v4.1-flash')?.input).toBe(3.0);
    expect(getLocalOverrides()).toEqual({});
  });

  it('脏值（负数 / 字符串垃圾）被规整成 null，整条等同「未登记」', () => {
    localStorage.setItem(
      OFFICIAL_PRICE_STORAGE_KEY,
      JSON.stringify({ 'dirty-model': { input: -5, output: 'abc', cache: null } }),
    );
    // 三项都非法 → 规整成全 null → 与「没填」同义，返回 null 而不是一个空壳对象
    expect(getOfficialPrice('dirty-model')).toBeNull();
    // 覆盖层仍留着这条（管理员填过，只是值非法），读回时被规整成全 null
    expect(getLocalOverrides()['dirty-model']).toEqual({ input: null, output: null, cache: null });
  });

  it('部分合法时保留合法项、非法项归 null', () => {
    localStorage.setItem(
      OFFICIAL_PRICE_STORAGE_KEY,
      JSON.stringify({ 'half-model': { input: 2.5, output: 'abc', cache: -1 } }),
    );
    expect(getOfficialPrice('half-model')).toEqual({ input: 2.5, output: null, cache: null });
  });

  it('getOfficialPrices 返回的键是归一化形态', () => {
    saveOfficialPrice('MiXeD-Case', { input: 1, output: null, cache: null });
    const table = getOfficialPrices();
    expect(table['mixed-case']?.input).toBe(1);
    expect(table['MiXeD-Case']).toBeUndefined();
  });
});