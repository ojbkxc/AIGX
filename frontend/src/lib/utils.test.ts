import { describe, expect, it, vi, beforeEach } from 'vitest';
import { cn, fmtCompact, fmtQuota, isAdmin } from './utils';

describe('cn', () => {
  it('合并普通类名', () => {
    expect(cn('a', 'b')).toBe('a b');
  });

  it('过滤 false/null/undefined', () => {
    expect(cn('a', false, null, undefined, 'b')).toBe('a b');
  });

  it('tailwind 冲突后类胜出', () => {
    expect(cn('px-2', 'px-4')).toBe('px-4');
  });

  it('空输入返回空串', () => {
    expect(cn()).toBe('');
  });
});

describe('fmtCompact', () => {
  it('null/undefined 显示破折号', () => {
    expect(fmtCompact(null)).toBe('—');
    expect(fmtCompact(undefined)).toBe('—');
  });

  it('非有限值显示破折号', () => {
    expect(fmtCompact(NaN)).toBe('—');
  });

  it('万/亿中文单位', () => {
    expect(fmtCompact(15_000)).toBe('1.5万');
    expect(fmtCompact(20_000)).toBe('2万');
    expect(fmtCompact(2_500_000)).toBe('250万');
    expect(fmtCompact(300_000_000)).toBe('3亿');
  });

  it('小于万位使用本地化数字', () => {
    expect(fmtCompact(9999)).toBe('9,999');
    expect(fmtCompact(999)).toBe('999');
  });
});

describe('fmtQuota', () => {
  it('空值按 0 元展示', () => {
    expect(fmtQuota(null)).toBe('0.00');
    expect(fmtQuota(undefined)).toBe('0.00');
  });

  it('按 200 额度 = 1 元折算两位小数', () => {
    expect(fmtQuota(2_400_000)).toBe('12,000.00');
    expect(fmtQuota(24690)).toBe('123.45');
    expect(fmtQuota(1)).toBe('0.01');
  });
});

describe('isAdmin', () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });

  it('role=admin 时返回 true', () => {
    vi.spyOn(Storage.prototype, 'getItem').mockReturnValue('admin');
    expect(isAdmin()).toBe(true);
  });

  it('其他角色返回 false', () => {
    vi.spyOn(Storage.prototype, 'getItem').mockReturnValue('user');
    expect(isAdmin()).toBe(false);
  });

  it('localStorage 抛异常时返回 false', () => {
    vi.spyOn(Storage.prototype, 'getItem').mockImplementation(() => {
      throw new Error('denied');
    });
    expect(isAdmin()).toBe(false);
  });
});
