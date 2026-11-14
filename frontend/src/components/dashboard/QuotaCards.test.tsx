import { describe, expect, it, beforeEach } from 'vitest';
import { render } from '@testing-library/react';
import i18n from 'i18next';
import QuotaCards from './QuotaCards';
import '../../i18n';

// 全量套件里别的测试文件会把 localStorage 的 i18n_lang 污染成 en（jsdom 共享环境），
// 断言依赖中文文案 → 每个用例前钉回 zh
beforeEach(async () => {
  localStorage.setItem('i18n_lang', 'zh');
  await i18n.changeLanguage('zh');
});

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
    // React 18 序列化颜色带空格
    expect(bar?.getAttribute('style')).toContain('rgb(239, 68, 68)');
  });
});
