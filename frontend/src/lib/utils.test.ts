import { describe, expect, it, vi, beforeEach } from 'vitest';
import { cn, fmtCompact, isAdmin } from './utils';

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

  it('千/百万/十亿缩写', () => {
    expect(fmtCompact(1500)).toBe('1.5K');
    expect(fmtCompact(2_500_000)).toBe('2.5M');
    expect(fmtCompact(3_000_000_000)).toBe('3.0B');
  });

  it('小于千位使用本地化数字', () => {
    expect(fmtCompact(999)).toBe('999');
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
