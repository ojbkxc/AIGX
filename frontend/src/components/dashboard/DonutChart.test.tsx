import { describe, expect, it, beforeEach } from 'vitest';
import { render, fireEvent } from '@testing-library/react';
import i18n from 'i18next';
import DonutChart from './DonutChart';
import '../../i18n';

// 全量套件里别的测试文件会把 localStorage 的 i18n_lang 污染成 en（jsdom 共享环境），
// 断言依赖中文文案 → 每个用例前钉回 zh
beforeEach(async () => {
  localStorage.setItem('i18n_lang', 'zh');
  await i18n.changeLanguage('zh');
});

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
    // 非目标扇区降透明度（opacity 是 SVG 属性，非 style 内联样式）
    expect(paths[0].getAttribute('opacity')).toBe('0.35');
    expect(paths[1].getAttribute('opacity')).toBe('1');
    expect(paths[2].getAttribute('opacity')).toBe('0.35');
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
    expect(paths[4].getAttribute('opacity')).toBe('1');
    expect(paths[0].getAttribute('opacity')).toBe('0.35');
  });

  it('响应式：svg 按容器宽缩放，不再固定 220px', () => {
    const { container } = render(<DonutChart {...baseProps} />);
    const svg = container.querySelector('svg') as SVGSVGElement;
    expect(svg.getAttribute('style')).toContain('width: 100%');
    expect(svg.getAttribute('style')).toContain('height: auto');
    expect(svg.parentElement?.getAttribute('style')).toContain('max-width: 100%');
  });
});

describe('DonutChart — 合并小项阈值（A2A donutView threshold）', () => {
  // 占比 60% / 30% / 10%：阈值 5% 时 10% 仍在饼图上，阈值 15% 时它该被并进「其他」
  it('minPercent 低于阈值的扇区并入「其他」，图例仍列全量', () => {
    const { container } = render(<DonutChart {...baseProps} minPercent={15} />);
    const paths = Array.from(container.querySelectorAll('path')) as SVGPathElement[];
    expect(paths).toHaveLength(3); // gpt-4o / claude / 其他
    const legend = Array.from(container.querySelectorAll('.donut-legend-row')).map((r) => r.textContent);
    expect(legend).toHaveLength(3); // 图例一行不少
    expect(legend.some((x) => x?.includes('gemini'))).toBe(true);
    // 合并出来的扇区名是「其他」（A2A「其他（N 项）」同口径，单项时省略计数）
    const svg = container.querySelector('svg') as SVGSVGElement;
    fireEvent.mouseMove(svg, { clientX: 100, clientY: 100 });
    fireEvent.mouseEnter(paths[2]);
    expect(container.querySelector('.donut-tip')?.textContent).toContain('其他');
  });

  it('阈值不触发时不产生「其他」扇区', () => {
    const { container } = render(<DonutChart {...baseProps} minPercent={5} />);
    expect(container.querySelectorAll('path')).toHaveLength(3);
  });

  it('多个小项并进「其他」时名字带成员数（A2A「其他（N 项）」）', () => {
    const data = [
      { name: 'big', value: 80 },
      { name: 's1', value: 8 },
      { name: 's2', value: 6 },
      { name: 's3', value: 6 },
    ];
    const { container } = render(
      <DonutChart data={data} colors={new Map()} totalText="t" totalSub="s" minPercent={10} />,
    );
    const paths = Array.from(container.querySelectorAll('path')) as SVGPathElement[];
    expect(paths).toHaveLength(2); // big + 其他
    const svg = container.querySelector('svg') as SVGSVGElement;
    fireEvent.mouseMove(svg, { clientX: 100, clientY: 100 });
    fireEvent.mouseEnter(paths[1]);
    expect(container.querySelector('.donut-tip')?.textContent).toContain('其他（3 项）');
  });

  it('关掉阈值（minPercent=0）时行为与改造前一致：只受 maxSlices 约束', () => {
    const many = Array.from({ length: 12 }, (_, i) => ({ name: `m${i}`, value: 10 + i }));
    const { container } = render(
      <DonutChart data={many} colors={new Map()} totalText="t" totalSub="s" maxSlices={4} minPercent={0} />,
    );
    expect(container.querySelectorAll('path')).toHaveLength(5);
  });
});

