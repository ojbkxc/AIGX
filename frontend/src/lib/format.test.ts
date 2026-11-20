import { describe, expect, it, afterEach } from 'vitest';
import { fmtCost } from './format';
import { setQuotaPerYuan } from './quotaRate';

describe('fmtCost（分 → 元，统一两位小数）', () => {
  it('按 200 额度 = 1 元折算 + 千分位两位', () => {
    expect(fmtCost(246900)).toBe('¥1,234.50');
    expect(fmtCost(0)).toBe('¥0.00');
    expect(fmtCost(200)).toBe('¥1.00');
    expect(fmtCost(1)).toBe('¥0.01');
    expect(fmtCost(246913578)).toBe('¥1,234,567.89');
  });

  it('负数额号在币号外（-¥1.50）', () => {
    expect(fmtCost(-300)).toBe('-¥1.50');
    expect(fmtCost(-246900)).toBe('-¥1,234.50');
  });

  it('null / undefined → —', () => {
    expect(fmtCost(null)).toBe('—');
    expect(fmtCost(undefined)).toBe('—');
  });

  it('英文档同样千分位两位小数', () => {
    expect(fmtCost(246900, 'en')).toBe('¥1,234.50');
  });

  it('随兑换倍率动态折算', () => {
    // 后台把倍率改成 1 元 = 100 额度后，同样的额度数应折成两倍金额
    setQuotaPerYuan(100);
    expect(fmtCost(123450)).toBe('¥1,234.50');
    expect(fmtCost(200)).toBe('¥2.00');
    setQuotaPerYuan(200);
  });
});

afterEach(() => {
  // 全局倍率是模块级状态，用例之间复位，避免顺序依赖
  setQuotaPerYuan(200);
});
