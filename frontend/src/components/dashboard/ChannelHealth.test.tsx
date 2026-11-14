import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render, waitFor } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import i18n from 'i18next';
import ChannelHealth from './ChannelHealth';
import { getSchedulerStatus } from '../../api';
import '../../i18n';

vi.mock('../../api', () => ({
  getSchedulerStatus: vi.fn(),
}));

// 全量套件里别的测试文件会把 localStorage 的 i18n_lang 污染成 en（jsdom 共享环境），
// 断言依赖中文文案 → 每个用例前钉回 zh
beforeEach(async () => {
  localStorage.setItem('i18n_lang', 'zh');
  await i18n.changeLanguage('zh');
  vi.mocked(getSchedulerStatus).mockReset();
});

const rows = [
  { id: 'c1', name: 'Alpha', status: 'enabled', success_rate: 99, total_requests: 100, error_rate: 1, avg_latency_ms: 50 },
  { id: 'c2', name: 'Beta', status: 'enabled', success_rate: 70, total_requests: 10, error_rate: 30, avg_latency_ms: 800 },
];

function renderWith(): { container: HTMLElement } {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return render(
    <QueryClientProvider client={client}>
      <ChannelHealth data={rows} />
    </QueryClientProvider>,
  );
}

describe('ChannelHealth — P95 列（#22，scheduler_status archive join）', () => {
  it('调度档案有 p95 时显示 P95 列并按渠道 id 对上读数', async () => {
    vi.mocked(getSchedulerStatus).mockResolvedValue({
      success: true,
      data: {
        scheduler: { health_weight: 1, cost_weight: 1, rpm_weight: 1 },
        channels: [
          { id: 'c1', archive: { p95_ms: 123 } },
          { id: 'c2', archive: null },
        ],
      },
    } as never);
    const { container } = renderWith();
    await waitFor(() => {
      expect(container.textContent).toContain('P95');
    });
    // 默认排序：严重度升序（异常在前健康在后）→ 行序 Beta、Alpha；
    // num 列序：成功率/错误率/延迟/P95。Beta 无 archive → —；Alpha → 123ms
    await waitFor(() => {
      const cells = Array.from(container.querySelectorAll('tbody tr')).map(
        (tr) => tr.querySelectorAll('td.num')[3]?.textContent,
      );
      expect(cells[0]).toBe('—');
      expect(cells[1]).toBe('123ms');
    });
  });

  it('调度状态不可用时整列隐藏（不造假数据）', async () => {
    vi.mocked(getSchedulerStatus).mockResolvedValue({ success: true, data: null } as never);
    const { container } = renderWith();
    await waitFor(() => {
      expect(getSchedulerStatus).toHaveBeenCalled();
    });
    expect(container.textContent).not.toContain('P95');
  });
});
