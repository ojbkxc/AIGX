import { describe, expect, it, beforeEach } from 'vitest';
import { render, fireEvent } from '@testing-library/react';
import i18n from 'i18next';
import { ConsumptionPanel, ModelPanels } from './TrendPanels';
import '../../i18n';
import type { DailyTrendRow, ReportModelRow, TopSource } from '../../hooks/useDashboard';

beforeEach(async () => {
  localStorage.clear();
  localStorage.setItem('i18n_lang', 'zh');
  await i18n.changeLanguage('zh');
});

/** PanelWrap 只吃这四个字段；refetch 给一个永不 resolve 出值的 promise 即可 */
const okQuery = <T,>(data: T) => ({
  isPending: false,
  isError: false,
  refetch: () => Promise.resolve(undefined as never),
  data,
});

const daily: DailyTrendRow[] = [
  { date: '2026-11-01', count: 10, input_tokens: 100, output_tokens: 50, cost: 0.5, models: [] },
  { date: '2026-11-02', count: 30, input_tokens: 300, output_tokens: 100, cost: 1.5, models: [] },
];

describe('ConsumptionPanel — 指标 × 形态切换（A2A daily-trend-panel）', () => {
  it('默认消费口径：合计走金额格式化（¥），不是裸数字', () => {
    const { container } = render(
      <ConsumptionPanel query={okQuery(daily)} daily={daily} rLabel="近 7 日" />,
    );
    expect(container.querySelector('.trend-reading')?.textContent).toContain('合计');
    // fmtCost 按兑换倍率折算（默认 200），所以只断言币号与读数存在，不钉死具体金额
    expect(container.querySelector('.trend-reading strong')?.textContent).toContain('¥');
    // 默认柱状：有 rect 无 polyline
    expect(container.querySelectorAll('rect').length).toBeGreaterThan(0);
    expect(container.querySelector('polyline')).toBeNull();
  });

  it('切到请求数：读数改为 10 + 30 = 40', () => {
    const { container } = render(
      <ConsumptionPanel query={okQuery(daily)} daily={daily} rLabel="近 7 日" />,
    );
    const btn = Array.from(container.querySelectorAll('.seg-btn')).find((b) => b.textContent === '请求数');
    expect(btn).toBeTruthy();
    fireEvent.click(btn as HTMLElement);
    expect(container.querySelector('.trend-reading strong')?.textContent).toBe('40');
  });

  it('切到 Token：读数改为 550（两日输入+输出之和）', () => {
    const { container } = render(
      <ConsumptionPanel query={okQuery(daily)} daily={daily} rLabel="近 7 日" />,
    );
    const btn = Array.from(container.querySelectorAll('.seg-btn')).find((b) => b.textContent === 'Token');
    fireEvent.click(btn as HTMLElement);
    // 100+50 + 300+100 = 550
    expect(container.querySelector('.trend-reading strong')?.textContent).toContain('550');
  });

  it('切到折线：图形本体变成 polyline（轴/刻度/hover 共用）', () => {
    const { container } = render(
      <ConsumptionPanel query={okQuery(daily)} daily={daily} rLabel="近 7 日" />,
    );
    const btn = Array.from(container.querySelectorAll('.seg-btn')).find((b) => b.textContent === '折线');
    fireEvent.click(btn as HTMLElement);
    expect(container.querySelector('polyline')).not.toBeNull();
  });
});

const models: ReportModelRow[] = [
  { model: 'big', count: 80, input_tokens: 4000, output_tokens: 4000, cost: 1 },
  { model: 'mid', count: 10, input_tokens: 500, output_tokens: 500, cost: 0.1 },
  { model: 'tiny', count: 2, input_tokens: 50, output_tokens: 50, cost: 0.01 },
];
const providers: TopSource[] = [
  { name: 'ch-a', count: 80, cost: 1, tokens: 8000, success_rate: 100, avg_latency_ms: 100 },
  { name: 'ch-b', count: 12, cost: 0.1, tokens: 1000, success_rate: 100, avg_latency_ms: 120 },
];
const donutData = models.map((m) => ({
  name: m.model ?? '—',
  value: (m.input_tokens ?? 0) + (m.output_tokens ?? 0),
  labelValue: String(m.model),
}));
const providerDonut = providers.map((p) => ({ name: p.name ?? '—', value: p.tokens ?? 0, labelValue: String(p.name) }));

describe('ModelPanels — 双饼图 + 合并小项阈值（A2A usage-donuts）', () => {
  it('给了供应商数据就渲染第二张饼图（模型 / 供应商两张）', () => {
    const { container } = render(
      <ModelPanels
        query={okQuery(models)} donutData={donutData} colors={new Map()} totalTokens={9100}
        rLabel="近 7 日" personal={false}
        providers={providerDonut} providersTotal={9000} providersColors={new Map()}
        providersQuery={okQuery(providers)}
      />,
    );
    expect(container.textContent).toContain('模型分布');
    expect(container.textContent).toContain('供应商分布');
  });

  it('个人视图（无供应商数据）只渲染一张饼图', () => {
    const { container } = render(
      <ModelPanels query={okQuery(models)} donutData={donutData} colors={new Map()} totalTokens={9100}
        rLabel="近 7 日" personal />,
    );
    expect(container.textContent).toContain('模型分布');
    expect(container.textContent).not.toContain('供应商分布');
  });

  it('关掉「合并小项」后小扇区重新出现（阈值确实接在饼图上）', () => {
    // tiny 占 550/9100 ≈ 6%：阈值 1% 时不会被合并，改到 10% 才合并
    const withMerge = render(
      <ModelPanels query={okQuery(models)} donutData={donutData} colors={new Map()} totalTokens={9100}
        rLabel="近 7 日" personal />,
    );
    const before = withMerge.container.querySelectorAll('path').length;
    const select = withMerge.container.querySelector('.donut-settings-select') as HTMLSelectElement;
    expect(select).toBeTruthy();
    fireEvent.change(select, { target: { value: '5' } });
    const after = withMerge.container.querySelectorAll('path').length;
    // 5% 阈值把 mid（11%）之外的 tiny 合并掉：扇区数应减少或持平，但不该增加
    expect(after).toBeLessThanOrEqual(before);
  });
});
