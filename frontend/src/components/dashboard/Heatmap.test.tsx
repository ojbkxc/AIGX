import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render } from '@testing-library/react';
import Heatmap from './Heatmap';
import '../../i18n';

// jsdom 没有 ResizeObserver（Heatmap 用 useMeasuredWidth 量容器宽）
beforeEach(() => {
  vi.stubGlobal('ResizeObserver', class {
    observe(): void {}
    unobserve(): void {}
    disconnect(): void {}
  });
});

const cells = [
  { date: '2026-01-01', count: 5, cost: 1, tokens: 0 }, // 有请求但 token=0 → 最低「有活动」档（#12）
  { date: '2026-01-02', count: 3, cost: 1, tokens: 1000 }, // 正常分档
  { date: '2026-01-03', count: 0, cost: 0, tokens: 0 }, // 真零 → 底色
];

describe('Heatmap — 零值档位（#12）', () => {
  it('token=0 但有请求的日子给最低「有活动」档，不画成空白', () => {
    const { container } = render(<Heatmap data={cells} />);
    const rects = Array.from(container.querySelectorAll('rect')) as SVGRectElement[];
    // 2026-01-01 是周四，前 3 格补空 → out[0]=01-01、out[1]=01-02、out[2]=01-03
    expect(rects[0].getAttribute('fill')).toBe('rgba(47,111,237,0.25)');
    // 正常 token 的日子按阈值分档（peak=1000 → step=250 → 1000 落在最深档）
    expect(rects[1].getAttribute('fill')).toBe('rgb(47,111,237)');
    // 真零（无请求）保持底色
    expect(rects[2].getAttribute('fill')).toBe('var(--bg-color)');
  });

  it('全部请求都失败（token=0）时不出现空视图，活动日仍有色', () => {
    const failed = cells.map((c) => ({ ...c, tokens: 0 }));
    const { container } = render(<Heatmap data={failed} />);
    const rects = Array.from(container.querySelectorAll('rect')) as SVGRectElement[];
    expect(rects.length).toBeGreaterThan(0);
    expect(rects[0].getAttribute('fill')).toBe('rgba(47,111,237,0.25)');
    expect(rects[2].getAttribute('fill')).toBe('var(--bg-color)');
  });
});
