import { describe, expect, it } from 'vitest';
import {
  PER_MILLION_SCALE,
  discountRatio,
  formatDiscount,
  formatPerMillion,
  formatPriceNumber,
  fromPerMillion,
  parsePerMillionInput,
  toPerMillion,
} from './pricingUnits';

/**
 * 定价单位换算的单测 —— 守卫「界面按每百万 Token、后端按每 1K token」这条口径。
 *
 * 核心约束：写回后端的必须是**每 1K** 值，一个字节都不能变（`price:xxx` KV 与
 * 计费公式都依赖它）。所以除正向换算外，这里还钉住反向换算的**逐位相等**。
 */
describe('toPerMillion（存储值 → 界面每百万）', () => {
  it('换算系数是 1000', () => {
    expect(PER_MILLION_SCALE).toBe(1000);
  });

  it('0.0003 → 0.3（线上基准：deepseek-v4.1-flash 输入）', () => {
    expect(toPerMillion(0.0003)).toBe(0.3);
  });

  it('0.0006 → 0.6（线上基准：输出）', () => {
    expect(toPerMillion(0.0006)).toBe(0.6);
  });

  it('0.0000025 → 0.0025（缓存读，浮点尾巴必须被规整）', () => {
    expect(toPerMillion(0.0000025)).toBe(0.0025);
  });

  it('存量值放大 1000 倍：0.01 → 10', () => {
    expect(toPerMillion(0.01)).toBe(10);
  });

  it('不裁整数的零：1 每 1K → 1000 每百万，不是 1', () => {
    expect(toPerMillion(1)).toBe(1000);
  });

  it('非有限值原样透传（不做静默归零）', () => {
    expect(toPerMillion(NaN)).toBeNaN();
    expect(toPerMillion(Infinity)).toBe(Infinity);
  });
});

describe('fromPerMillion（界面每百万 → 写回后端每 1K）', () => {
  it('0.3 → 0.0003，写回值与原存储值逐位相等', () => {
    expect(fromPerMillion(0.3)).toBe(0.0003);
  });

  it('0.0025 → 0.0000025，缓存读写回不漂移', () => {
    expect(fromPerMillion(0.0025)).toBe(0.0000025);
  });

  it('10 → 0.01（存量 0.01 在新界面显示 10，保存后仍存 0.01）', () => {
    expect(fromPerMillion(10)).toBe(0.01);
  });

  it('往返往返恒等：界面值 = 存储值 × 1000，写回后再转回不变', () => {
    const storages = [0.0003, 0.0006, 0.0000025, 0.01, 0.5, 0, 1234.5];
    for (const storage of storages) {
      const ui = toPerMillion(storage);
      expect(fromPerMillion(ui)).toBe(storage);
    }
  });

  it('非有限值原样透传', () => {
    expect(fromPerMillion(NaN)).toBeNaN();
  });
});

describe('formatPriceNumber（展示串去浮点尾巴）', () => {
  it('1000 保留为 1000，不会被裁成 1', () => {
    expect(formatPriceNumber(1000)).toBe('1000');
  });

  it('只在有小数位时裁零：1.50 → 1.5、2 → 2', () => {
    expect(formatPriceNumber(1.5)).toBe('1.5');
    expect(formatPriceNumber(2)).toBe('2');
    expect(formatPriceNumber(12.25)).toBe('12.25');
  });

  it('0.30000000000000004 → 0.3，0.0025000000000000005 → 0.0025', () => {
    expect(formatPriceNumber(0.30000000000000004)).toBe('0.3');
    expect(formatPriceNumber(0.0025000000000000005)).toBe('0.0025');
  });

  it('0 → 0，负数按数值展示', () => {
    expect(formatPriceNumber(0)).toBe('0');
    expect(formatPriceNumber(-1.5)).toBe('-1.5');
  });

  it('空值与非有限值 → 占位符', () => {
    expect(formatPriceNumber(null)).toBe('—');
    expect(formatPriceNumber(undefined)).toBe('—');
    expect(formatPriceNumber(NaN)).toBe('—');
  });
});

describe('formatPerMillion（存储值 → 每百万展示串）', () => {
  it('0.0003 → "0.3"', () => {
    expect(formatPerMillion(0.0003)).toBe('0.3');
  });

  it('0.0000025 → "0.0025"（无浮点尾巴）', () => {
    expect(formatPerMillion(0.0000025)).toBe('0.0025');
  });

  it('未定价（undefined）→ "—"，不显示成 0 元', () => {
    expect(formatPerMillion(undefined)).toBe('—');
  });
});

describe('parsePerMillionInput（界面输入串 → 后端存储值）', () => {
  it('空串按 0（沿用既有编辑器留空即 0 的口径）', () => {
    expect(parsePerMillionInput('')).toBe(0);
  });

  it('"0.3" → 0.0003，写回的仍是每 1K', () => {
    expect(parsePerMillionInput('0.3')).toBe(0.0003);
  });

  it('"0.0025" → 0.0000025', () => {
    expect(parsePerMillionInput('0.0025')).toBe(0.0000025);
  });

  it('"10" → 0.01', () => {
    expect(parsePerMillionInput('10')).toBe(0.01);
  });

  it('非数字输入 → NaN（让上层拦下来，不静默存 0）', () => {
    expect(parsePerMillionInput('abc')).toBeNaN();
  });
});

describe('discountRatio（我们价 / 官方价）', () => {
  it('0.3 对 3.0 → 0.1（1 折）', () => {
    expect(discountRatio(0.3, 3.0)).toBeCloseTo(0.1, 10);
  });

  it('0.15 对 3.0 → 0.05（0.5 折）', () => {
    expect(discountRatio(0.15, 3.0)).toBeCloseTo(0.05, 10);
  });

  it('官方价缺失 → null（界面显示未登记，不凭空算折扣）', () => {
    expect(discountRatio(0.3, null)).toBeNull();
    expect(discountRatio(0.3, undefined)).toBeNull();
  });

  it('官方价为 0 或负数 → null', () => {
    expect(discountRatio(0.3, 0)).toBeNull();
    expect(discountRatio(0.3, -1)).toBeNull();
  });

  it('我们价缺失 → null', () => {
    expect(discountRatio(null, 3.0)).toBeNull();
  });

  it('我们价高于官方价 → 比率 ≥ 1，由调用方按「无折扣」处理', () => {
    expect(discountRatio(3.0, 3.0)).toBeCloseTo(1, 10);
    expect(discountRatio(6.0, 3.0)).toBeCloseTo(2, 10);
  });
});

describe('formatDiscount（比率 → 中文「折」）', () => {
  it('0.1 → 1 折', () => {
    expect(formatDiscount(0.1)).toBe('1 折');
  });

  it('0.25 → 2.5 折', () => {
    expect(formatDiscount(0.25)).toBe('2.5 折');
  });

  it('0.05 → 0.5 折', () => {
    expect(formatDiscount(0.05)).toBe('0.5 折');
  });

  it('0.5 → 5 折（不显示成 5.0 折）', () => {
    expect(formatDiscount(0.5)).toBe('5 折');
  });

  it('1 → 10 折（比率 ≥ 1 时由界面改用「无折扣」文案）', () => {
    expect(formatDiscount(1)).toBe('10 折');
  });
});
