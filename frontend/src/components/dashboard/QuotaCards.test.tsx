import { describe, expect, it } from 'vitest';
import { render } from '@testing-library/react';
import QuotaCards from './QuotaCards';
import '../../i18n';

describe('QuotaCards — 三卡合并（#17）', () => {
  it('单卡 + 进度条承载 总/已用/剩余，不再三卡冗余', () => {
    const { container } = render(<QuotaCards monthlyUsed={30} monthlyLimit={100} />);
    expect(container.querySelectorAll('.stat-card')).toHaveLength(1);
    const text = container.textContent ?? '';
    expect(text).toContain('30'); // 已用
    expect(text).toContain('100'); // 总配额
    expect(text).toContain('70'); // 剩余 = 总 − 已用
    expect(text).toContain('30.0%'); // 已消耗
    expect(text).toContain('已消耗');
  });

  it('未设限额时显示未设限额，不画进度条', () => {
    const { container } = render(<QuotaCards monthlyUsed={5} monthlyLimit={null} />);
    const text = container.textContent ?? '';
    expect(text).toContain('未设限额');
    expect(text).not.toContain('已消耗');
    expect(container.querySelector('div[style*="overflow: hidden"]')).toBeNull();
  });

  it('已耗 >80% 进度条走红色档', () => {
    const { container } = render(<QuotaCards monthlyUsed={85} monthlyLimit={100} />);
    const bar = container.querySelector('div[style*="overflow: hidden"] div');
    expect(bar?.getAttribute('style')).toContain('rgb(239,68,68)');
  });
});
