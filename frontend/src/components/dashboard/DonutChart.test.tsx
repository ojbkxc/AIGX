import { describe, expect, it } from 'vitest';
import { render, fireEvent } from '@testing-library/react';
import DonutChart from './DonutChart';
import '../../i18n';

const data = [
  { name: 'gpt-4o', value: 60, labelValue: '6.0万' },
  { name: 'claude', value: 30, labelValue: '3.0万' },
  { name: 'gemini', value: 10, labelValue: '1.0万' },
];
const colors = new Map(data.map((d) => [d.name, '#123456']));
const baseProps = { data, colors, totalText: '10万', totalSub: 'tokens' };

describe('DonutChart — 跟随式 tooltip + 图例联动（#8/#9）', () => {
  it('扇区 hover 显示自定义 tooltip（不再依赖原生 title）', () => {
    const { container } = render(<DonutChart {...baseProps} />);
    const path = container.querySelector('path') as SVGPathElement;
    expect(path.querySelector('title')).toBeNull();
    expect(container.querySelector('.donut-tip')).toBeNull();
    const svg = container.querySelector('svg') as SVGSVGElement;
    // tooltip 需要鼠标位置（纯 state + CSS 定位）
    fireEvent.mouseMove(svg, { clientX: 100, clientY: 100 });
    fireEvent.mouseEnter(path);
    const tip = container.querySelector('.donut-tip');
    expect(tip?.textContent).toBe('gpt-4o · 6.0万 · 60.0%');
  });

  it('图例 hover 联动高亮扇区 + 中心换成该扇区读数', () => {
    const { container } = render(<DonutChart {...baseProps} />);
    const paths = Array.from(container.querySelectorAll('path')) as SVGPathElement[];
    fireEvent.mouseEnter(container.querySelectorAll('.donut-legend-row')[1]);
    // 非目标扇区降透明度，目标扇区保持 1
    expect(paths[0].getAttribute('style')).toContain('opacity: 0.35');
    expect(paths[1].getAttribute('style')).not.toContain('opacity: 0.35');
    expect(paths[2].getAttribute('style')).toContain('opacity: 0.35');
    // 中心读数切到 claude 的 30.0%
    const texts = Array.from(container.querySelectorAll('svg text')).map((t) => t.textContent);
    expect(texts).toContain('30.0%');
  });

  it('超过 maxSlices 折叠时图例后半段指向「其他」扇区', () => {
    const many = Array.from({ length: 12 }, (_, i) => ({ name: `m${i}`, value: 10 + i }));
    const { container } = render(<DonutChart data={many} colors={new Map()} totalText="t" totalSub="s" maxSlices={4} />);
    const paths = Array.from(container.querySelectorAll('path')) as SVGPathElement[];
    expect(paths).toHaveLength(5); // 4 头部 + 其他
    const rows = Array.from(container.querySelectorAll('.donut-legend-row'));
    expect(rows).toHaveLength(12); // 图例列全量
    fireEvent.mouseEnter(rows[11]); // 第 12 行 → 其他（最后扇区）
    expect(paths[4].getAttribute('style')).not.toContain('opacity: 0.35');
    expect(paths[0].getAttribute('style')).toContain('opacity: 0.35');
  });

  it('响应式：svg 按容器宽缩放，不再固定 220px', () => {
    const { container } = render(<DonutChart {...baseProps} />);
    const svg = container.querySelector('svg') as SVGSVGElement;
    expect(svg.getAttribute('style')).toContain('width: 100%');
    expect(svg.getAttribute('style')).toContain('height: auto');
    expect(svg.parentElement?.getAttribute('style')).toContain('max-width: 100%');
  });
});
