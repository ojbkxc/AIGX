/**
 * 路由页（#40）渲染回归。
 *
 * 两件事值得钉住：
 * 1. **形状**：mock 按后端 serde 的真实形状构造（`RoutingProfiles` 三层、
 *    字段全 `Option`、`AccountSortRule` 的内部标签 `{on,dir}`），页面必须在
 *    该形状下不崩 —— 本仓出过「界面假定的形状与真实投影不匹配」的事故（#31）。
 * 2. **降级路径**：预演端点 404 时必须明确告知「未就绪」，不得静默谎报。
 */
import { describe, expect, it, beforeEach, afterEach, vi } from 'vitest';
import { render, screen, cleanup, waitFor, fireEvent } from '@testing-library/react';
import type { ReactElement } from 'react';

const apiMock = vi.hoisted(() => ({
  getRoutingPolicy: vi.fn(),
  saveRoutingEnabled: vi.fn(),
  saveRoutingProfiles: vi.fn(),
  saveRoutingPolicy: vi.fn(),
  getRoutingDefaults: vi.fn(),
  getRoutingQueue: vi.fn(),
  previewRouting: vi.fn(),
  getRouteReplay: vi.fn(),
  getRouteSticky: vi.fn(),
  clearRouteSticky: vi.fn(),
}));
vi.mock('../api', () => ({ api: apiMock }));
vi.mock('../components/Toast', () => ({
  useToast: () => (): void => undefined,
  ToastProvider: ({ children }: { children: ReactElement }) => children,
}));

import Routing from './Routing';

const POLICY = {
  success: true,
  data: {
    strategy: 'weighted',
    sticky: 'auto',
    breaker: { enabled: false, threshold: 5, cooldown_secs: 30, success_threshold: 2 },
    scheduler_weights: { health_weight: 0.4, cost_weight: 0.4, rpm_weight: 0.2 },
    enabled: true,
    profiles: {
      global: {
        scope: { provider_scope: 'all', providers: [], disabled_providers: [], account_order: [], excluded_accounts: [] },
        strategy: null,
        sticky: null,
        needs: null,
        sort_rules: null,
        default_group: null,
      },
      models: {
        'gpt-4o': {
          scope: null,
          strategy: 'ranked',
          sticky: null,
          needs: { tools: true, images: false, video: false },
          sort_rules: [{ on: 'balance', dir: 'asc' }],
          default_group: 'g1',
        },
      },
      groups: [
        {
          id: 'g1',
          name: '快速组',
          model: '',
          strategy: 'round_robin',
          sticky: 'session',
          members: [{ provider: 'openai', model: 'gpt-4o-2024-08-06', included: true, accounts: [] }],
        },
      ],
    },
  },
};

describe('Routing 页冒烟', () => {
  beforeEach(() => {
    localStorage.clear();
    Object.values(apiMock).forEach((f) => f.mockReset());
    apiMock.getRoutingPolicy.mockResolvedValue(POLICY);
    apiMock.getRoutingDefaults.mockResolvedValue(POLICY);
    apiMock.getRouteSticky.mockResolvedValue({ success: true, data: { entries: [], total: 0, truncated: false, now: 0, sticky: 'auto' } });
    apiMock.getRoutingQueue.mockResolvedValue({
      success: true,
      data: { strategy: 'ranked', sticky: 'auto', model: 'gpt-4o', pickedId: 'c1', ordered: [{ id: 'c1', name: '渠道一', provider: 'openai', state: 'closed' }], blocked: [], profilesEnabled: true, appliedLayers: ['global', 'model'], skipped: { provider_not_allowed: 2, provider_disabled: 0 } },
    });
  });
  afterEach(() => cleanup());

  it('渲染配置页 + 切层 + 切三态 + 预演', async () => {
    const { container } = render(<Routing /> as ReactElement);
    await waitFor(() => expect(screen.getByText('分层档案')).toBeTruthy(), { timeout: 3000 });

    // 总开关提示（enabled=true 走「已启用」分支）
    expect(container.textContent).toContain('多规则已启用');
    // 层栏：全局 + 模型级 gpt-4o + 路由组
    expect(screen.getByText('gpt-4o')).toBeTruthy();

    // 切到模型级层 → ProfileForm 渲染三态控件
    fireEvent.click(screen.getByText('gpt-4o'));
    await waitFor(() => expect(screen.getByText('选路策略')).toBeTruthy());
    expect(container.textContent).toContain('模型名请填客户端的请求名');
    // 三档控件存在
    expect(container.querySelectorAll('.rt-seg').length).toBeGreaterThanOrEqual(6);

    // 切「清空」档不崩
    const segs = container.querySelectorAll('.rt-seg');
    const clearBtns = Array.from(segs).flatMap((s) => Array.from(s.querySelectorAll('button'))).filter((b) => b.textContent === '清空');
    expect(clearBtns.length).toBeGreaterThan(0);
    fireEvent.click(clearBtns[0]);

    // 切到路由组层
    fireEvent.click(screen.getByText('路由组'));
    await waitFor(() => expect(screen.getByText('成员槽位')).toBeTruthy());

    // 预演
    fireEvent.change(container.querySelector('input[placeholder="输入模型名（请求名），如 gpt-4o"]') as HTMLElement, { target: { value: 'gpt-4o' } });
    fireEvent.click(screen.getAllByRole('button', { name: '预演' })[0]);
    await waitFor(() => expect(container.textContent).toContain('命中层'), { timeout: 3000 });
    // 命中层徽章（global / model）与候选；被裁计数文案含 {{a}}/{{b}} 插值，
    // 在新键落进 zh.json 前 i18next 不插值，故此处不断言该行文案。
    expect(container.querySelectorAll('.rt-chip-model').length).toBeGreaterThan(0);
    expect(screen.getByText(/渠道一/)).toBeTruthy();
  });

  it('预演端点 404 → 明确告知未就绪', async () => {
    apiMock.previewRouting.mockRejectedValue(Object.assign(new Error('nf'), { status: 404 }));
    const { container } = render(<Routing /> as ReactElement);
    await waitFor(() => expect(screen.getByText('分层档案')).toBeTruthy(), { timeout: 3000 });
    fireEvent.change(container.querySelector('input[placeholder="模型名（请求名）"]') as HTMLElement, { target: { value: 'gpt-4o' } });
    const btns = screen.getAllByText('预演');
    fireEvent.click(btns[btns.length - 1]);
    await waitFor(() => expect(container.textContent).toContain('预演端点尚未就绪'), { timeout: 3000 });
  });
});
