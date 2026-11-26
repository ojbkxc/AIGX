import { describe, expect, it, beforeEach } from 'vitest';
import { render, fireEvent } from '@testing-library/react';
import { MemoryRouter, useLocation } from 'react-router-dom';
import i18n from 'i18next';
import TopClients from './TopClients';
import '../../i18n';
import type { TopClient } from '../../hooks/useDashboard';

beforeEach(async () => {
  localStorage.clear();
  localStorage.setItem('i18n_lang', 'zh');
  await i18n.changeLanguage('zh');
});

function LocationProbe(): JSX.Element {
  const loc = useLocation();
  return <div data-testid="loc">{`${loc.pathname}${loc.search}`}</div>;
}

const data: TopClient[] = [
  { name: 'Cline', count: 10, cost: 0.1, tokens: 900, success_rate: 100, avg_latency_ms: 120 },
  { name: 'Claude Code', count: 60, cost: 0.6, tokens: 300, success_rate: 100, avg_latency_ms: 200 },
];

/** 客户端名单元格里的链接（下钻列也有 .row-link，取第二列才唯一） */
const nameLinks = (container: HTMLElement): Array<string | null> =>
  Array.from(container.querySelectorAll('tbody tr td:nth-child(2) .row-link')).map((b) => b.textContent);

function mount(extra: Partial<Parameters<typeof TopClients>[0]> = {}) {
  return render(
    <MemoryRouter initialEntries={['/']}>
      <TopClients data={data} drill={{ start: '2026-11-01', end: '2026-11-07' }} {...extra} />
      <LocationProbe />
    </MemoryRouter>,
  );
}

describe('TopClients — 排序与下钻（A2A source-stats）', () => {
  it('默认按请求数排序', () => {
    const { container } = mount();
    expect(nameLinks(container)).toEqual(['Claude Code', 'Cline']);
  });

  it('切到「按 Token」后按 token 重排（不是原地不动）', () => {
    const { container } = mount();
    const tokenBtn = Array.from(container.querySelectorAll('.seg-btn')).find((b) => b.textContent === '按 Token');
    expect(tokenBtn).toBeTruthy();
    fireEvent.click(tokenBtn as HTMLElement);
    expect(nameLinks(container)).toEqual(['Cline', 'Claude Code']);
  });

  it('个人视图（tokens 全 null）时「按 Token」禁用', () => {
    const personal = data.map((c) => ({ ...c, tokens: null }));
    const { container } = mount({ data: personal });
    const tokenBtn = Array.from(container.querySelectorAll('.seg-btn')).find((b) => b.textContent === '按 Token');
    expect((tokenBtn as HTMLButtonElement).disabled).toBe(true);
  });

  it('点客户端名 → 写入 Logs 筛选记忆并带 `?` 参数跳到 /logs', () => {
    const { container, getByTestId } = mount();
    const first = container.querySelector('tbody tr td:nth-child(2) .row-link');
    expect(first).toBeTruthy();
    fireEvent.click(first as HTMLElement);
    const loc = getByTestId('loc').textContent || '';
    expect(loc.startsWith('/logs?')).toBe(true);
    expect(loc).toContain('client=Claude+Code');
    expect(loc).toContain('start=2026-11-01');
    expect(loc).toContain('end=2026-11-07');
    // 只带 URL 参数不够：Logs 启动时从筛选记忆恢复，这里必须也写了
    const saved = JSON.parse(localStorage.getItem('aigx-logs-filters') || '{}');
    expect(saved.client).toBe('Claude Code');
    expect(saved.range).toBe('custom');
  });

  it('没给 drill 窗口时不渲染请求日志列，行也不可点', () => {
    const { container } = mount({ drill: null });
    expect(container.textContent).not.toContain('请求日志');
    expect(container.querySelector('tbody tr')?.className ?? '').not.toContain('clickable-row');
  });
});
