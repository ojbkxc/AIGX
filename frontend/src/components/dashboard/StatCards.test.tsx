import { describe, expect, it } from 'vitest';
import { render } from '@testing-library/react';
import StatCards from './StatCards';
import '../../i18n';

const ov = {
  total_requests: 120,
  success_requests: 100,
  input_tokens: 5000,
  output_tokens: 2000,
  total_cost: 30,
  active_days: 10,
  streak: 3,
};

describe('StatCards — 趋势 delta + sparkline（#11）', () => {
  it('缺省（无 prev/series）时不渲染 delta 与 sparkline，与旧版一致', () => {
    const { container } = render(<StatCards overview={ov} />);
    expect(container.textContent).not.toContain('▲');
    expect(container.textContent).not.toContain('▼');
    expect(container.querySelectorAll('svg')).toHaveLength(0);
  });

  it('有 prev 时给出环比 delta（▲/▼ + 百分点）', () => {
    const { container } = render(<StatCards overview={ov} prev={{ ...ov, total_requests: 100, success_requests: 90, input_tokens: 4000, output_tokens: 1000, total_cost: 40 }} />);
    // 请求 120 vs 100 → ▲ 20.0%
    expect(container.textContent).toContain('▲ 20.0%');
    // 成功率 100/120=83.3% vs 90/100=90.0% → ▼ 6.7pp
    expect(container.textContent).toContain('▼ 6.7pp');
    // token 7000 vs 5000 → ▲ 40.0%
    expect(container.textContent).toContain('▲ 40.0%');
    // 成本 30 vs 40 → ▼ 25.0%
    expect(container.textContent).toContain('▼ 25.0%');
  });

  it('有 series 时渲染小 sparkline（面积+折线），走势方向决定配色', () => {
    const rising = render(<StatCards overview={ov} series={[1, 2, 3, 4]} />);
    const svg = rising.container.querySelector('svg');
    expect(svg).not.toBeNull();
    expect(svg?.getAttribute('width')).toBe('88');
    // 上行用 mint，下行用 coral
    expect(svg?.innerHTML).toContain('var(--mint)');
    const falling = render(<StatCards overview={ov} series={[4, 3, 2, 1]} />);
    expect(falling.container.querySelector('svg')?.innerHTML).toContain('var(--coral)');
  });

  it('series 只有 1 个点时不画 sparkline', () => {
    const { container } = render(<StatCards overview={ov} series={[42]} />);
    expect(container.querySelectorAll('svg')).toHaveLength(0);
  });

  it('连续天数卡无 delta（绝对值指标）', () => {
    const { container } = render(<StatCards overview={ov} prev={{ ...ov, streak: 99 }} />);
    expect(container.textContent).not.toContain('▼ 96.0%');
  });
});
