import { describe, expect, it } from 'vitest';
import { fmtCost } from './format';

describe('fmtCost（货币金额统一两位小数）', () => {
  it('固定两位 + 千分位', () => {
    expect(fmtCost(1234.5)).toBe('¥1,234.50');
    expect(fmtCost(0)).toBe('¥0.00');
    expect(fmtCost(1)).toBe('¥1.00');
    expect(fmtCost(1234567.891)).toBe('¥1,234,567.89');
  });

  it('负数额号在币号外（-¥1.00）', () => {
    expect(fmtCost(-1.5)).toBe('-¥1.50');
    expect(fmtCost(-1234.5)).toBe('-¥1,234.50');
  });

  it('null / undefined → —', () => {
    expect(fmtCost(null)).toBe('—');
    expect(fmtCost(undefined)).toBe('—');
  });

  it('英文档同样千分位两位小数', () => {
    expect(fmtCost(1234.5, 'en')).toBe('¥1,234.50');
  });
});
