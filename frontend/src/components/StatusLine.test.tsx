/**
 * StatusLine —— 错误率真的接了后端数据。
 *
 * 复现的原始缺陷：组件读 `c.error_rate`，但后端 `handle_channel_health`
 * 的**顶层根本没有这个字段**（只有 `success_rate` 0~100 与 `health.error_rate`
 * 0~1）。于是加权错误率恒为 0：状态行永远显示 `0.0%`、永不标红，
 * 这一整列等于没接。
 */
import { describe, expect, it, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import '../i18n';

const getRealtime = vi.fn();
const getChannelHealth = vi.fn();
const getTodayTokens = vi.fn();

vi.mock('../api', () => ({
  api: {
    getRealtime: () => getRealtime(),
    getChannelHealth: () => getChannelHealth(),
    getTodayTokens: () => getTodayTokens(),
  },
}));

// isAdmin() 读 localStorage.role
function asAdmin(): void {
  localStorage.setItem('role', 'admin');
}

async function renderStatusLine(): Promise<void> {
  const { default: StatusLine } = await import('./StatusLine');
  render(<StatusLine />);
}

describe('StatusLine 错误率口径', () => {
  beforeEach(() => {
    asAdmin();
    getRealtime.mockResolvedValue({ data: { rps: 1.5, avg_latency_ms: 120 } });
    getTodayTokens.mockResolvedValue({ data: { request_count: 1234 } });
  });

  afterEach(() => {
    vi.clearAllMocks();
    localStorage.clear();
  });

  it('后端只给 success_rate 时错误率不再恒为 0（按 100-success_rate 反推）', async () => {
    // 后端真实形状：顶层 success_rate 百分数，没有顶层 error_rate
    getChannelHealth.mockResolvedValue({
      data: [
        { id: 'a', status: 'enabled', total_requests: 100, success_rate: 50, health: null },
        { id: 'b', status: 'enabled', total_requests: 100, success_rate: 50, health: null },
      ],
    });

    await renderStatusLine();

    // 两个渠道各 50% 成功 → 加权错误率 50.0%
    await waitFor(() => {
      expect(screen.getByText(/50\.0%/)).toBeTruthy();
    });
  });

  it('有健康快照时优先用 health.error_rate（0~1 比例 → 百分数展示）', async () => {
    getChannelHealth.mockResolvedValue({
      data: [
        { id: 'a', status: 'enabled', total_requests: 100, success_rate: 99, health: { error_rate: 0.2 } },
      ],
    });

    await renderStatusLine();

    await waitFor(() => {
      expect(screen.getByText(/20\.0%/)).toBeTruthy();
    });
  });

  it('错误率超 5% 时该段变警告色', async () => {
    getChannelHealth.mockResolvedValue({
      data: [
        { id: 'a', status: 'enabled', total_requests: 10, success_rate: 90, health: null },
      ],
    });

    const { container } = await (async () => {
      const { default: StatusLine } = await import('./StatusLine');
      return render(<StatusLine />);
    })();

    await waitFor(() => {
      expect(screen.getByText(/10\.0%/)).toBeTruthy();
    });
    const bad = Array.from(container.querySelectorAll('.statusline-seg-bad')).map((n) => n.textContent);
    expect(bad.some((t) => t?.includes('10.0%'))).toBe(true);
  });

  it('无数据时不崩、错误率 0.0%', async () => {
    getChannelHealth.mockResolvedValue({ data: [] });

    await renderStatusLine();

    await waitFor(() => {
      expect(screen.getByText(/0\.0%/)).toBeTruthy();
    });
  });
});
