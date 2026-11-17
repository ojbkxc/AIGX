/**
 * 路由页 rail 骨架回归。
 *
 * 钉住三件事：
 * 1. **rail 五项**（配置 / 回放 / 压缩 / 标题 / 视觉）都在，且切走再切回
 *    不丢未保存的档案草稿（草稿存页面 state、切 rail 只是不渲染，不是重置）。
 * 2. **时间窗切换**会带新的 minutes 重新请求 /routing/flow。
 * 3. 既有能力面（分层档案 / 三态 / 预演）在 rail 化后仍原样可用。
 *
 * mock 按后端 serde 的真实形状构造（RoutingProfiles 三层、字段全 Option、
 * AccountSortRule 内部标签 {on,dir}），页面必须在该形状下不崩 —— 本仓出过
 * 「界面假定的形状与真实投影不匹配」的事故（#31）。
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
  // 本波新增（rail 工作台 / 特殊面板）
  getRoutingWorkspace: vi.fn(),
  getRoutingFlow: vi.fn(),
  getRoutingTrace: vi.fn(),
  getRoutingTraceRequest: vi.fn(),
  getCompactRoutes: vi.fn(),
  saveCompactRoutes: vi.fn(),
  getTitleRoutes: vi.fn(),
  saveTitleRoutes: vi.fn(),
  getVisionCompatibility: vi.fn(),
  saveVisionCompatibility: vi.fn(),
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
        scope: {
          provider_scope: 'all',
          providers: [],
          disabled_providers: [],
          account_order: [],
          excluded_accounts: [],
        },
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
          members: [
            { provider: 'openai', model: 'gpt-4o-2024-08-06', included: true, accounts: [] },
          ],
        },
      ],
    },
  },
};

const FLOW = {
  success: true,
  data: {
    available: true,
    paths: [],
    pathCount: 0,
    requests: 0,
    running: 0,
    sessions: 0,
    now: 0,
    since: 0,
  },
};

const TRACE = { success: true, data: { available: true, entries: [], matched: 0 } };

describe('Routing 页冒烟', () => {
  beforeEach(() => {
    localStorage.clear();
    Object.values(apiMock).forEach((f) => f.mockReset());
    apiMock.getRoutingPolicy.mockResolvedValue(POLICY);
    apiMock.getRoutingDefaults.mockResolvedValue(POLICY);
    apiMock.getRouteSticky.mockResolvedValue({
      success: true,
      data: { entries: [], total: 0, truncated: false, now: 0, sticky: 'auto' },
    });
    apiMock.getRoutingQueue.mockResolvedValue({
      success: true,
      data: {
        strategy: 'ranked',
        sticky: 'auto',
        model: 'gpt-4o',
        pickedId: 'c1',
        ordered: [{ id: 'c1', name: '渠道一', provider: 'openai', state: 'closed' }],
        blocked: [],
        profilesEnabled: true,
        appliedLayers: ['global', 'model'],
        skipped: { provider_not_allowed: 2, provider_disabled: 0 },
      },
    });
    apiMock.getRoutingWorkspace.mockResolvedValue({
      success: true,
      data: { model: null, customized: false, models: [], configuredModels: [], queue: {} },
    });
    apiMock.getRoutingFlow.mockResolvedValue(FLOW);
    apiMock.getRoutingTrace.mockResolvedValue(TRACE);
  });
  afterEach(() => cleanup());

  const railButton = (container: HTMLElement, label: string): HTMLElement => {
    const found = Array.from(container.querySelectorAll<HTMLElement>('.rt-nav-item')).find(
      (el) => el.textContent?.trim() === label,
    );
    if (!found) throw new Error(`rail item not found: ${label}`);
    return found;
  };

  it('rail 五项在 + 配置页能力保留 + 切 rail 不丢未保存草稿', async () => {
    const { container } = render(<Routing /> as ReactElement);
    await waitFor(() => expect(screen.getByText('分层档案')).toBeTruthy(), { timeout: 3000 });

    // 五项 rail（配置 / 回放 / 压缩 / 标题 / 视觉）
    const labels = Array.from(container.querySelectorAll('.rt-nav-label')).map((el) =>
      el.textContent?.trim(),
    );
    expect(labels).toEqual(['配置', '回放', '压缩', '标题', '视觉']);

    // 总开关提示（enabled=true 走「已启用」分支）
    expect(container.textContent).toContain('多规则已启用');
    // 层栏：全局 + 模型级 gpt-4o + 路由组
    expect(screen.getByText('gpt-4o')).toBeTruthy();

    // 切到模型级层 → ProfileForm 渲染三态控件；改一笔草稿（清空）
    fireEvent.click(screen.getByText('gpt-4o'));
    await waitFor(() => expect(screen.getByText('选路策略')).toBeTruthy());
    expect(container.textContent).toContain('模型名请填客户端的请求名');
    expect(container.querySelectorAll('.rt-seg').length).toBeGreaterThanOrEqual(6);

    const clearBtns = Array.from(container.querySelectorAll('.rt-seg button')).filter(
      (b) => b.textContent === '清空',
    );
    expect(clearBtns.length).toBeGreaterThan(0);
    fireEvent.click(clearBtns[0]);
    await waitFor(() => expect(container.textContent).toContain('有未保存的修改'));

    // 切到「压缩」再切回「配置」：草稿仍在（rail 切换不是重置）
    fireEvent.click(railButton(container, '压缩'));
    await waitFor(() => expect(container.textContent).toContain('压缩上下文'));
    fireEvent.click(railButton(container, '配置'));
    await waitFor(() => expect(container.textContent).toContain('有未保存的修改'));
  });

  it('回放 rail：时间窗切到 30 分钟会用新 minutes 重新请求流向', async () => {
    apiMock.getRoutingFlow.mockClear();
    const { container } = render(<Routing /> as ReactElement);
    await waitFor(() => expect(screen.getByText('分层档案')).toBeTruthy(), { timeout: 3000 });

    fireEvent.click(railButton(container, '回放'));
    await waitFor(() => expect(container.textContent).toContain('请求流向'));
    await waitFor(() => expect(apiMock.getRoutingFlow).toHaveBeenCalled());
    // 默认 10 分钟
    expect(apiMock.getRoutingFlow.mock.calls.some((c) => c[0]?.minutes === 10)).toBe(true);

    apiMock.getRoutingFlow.mockClear();
    const win30 = container.querySelector('[data-window="30"]') as HTMLElement;
    expect(win30).toBeTruthy();
    fireEvent.click(win30);
    await waitFor(() =>
      expect(apiMock.getRoutingFlow.mock.calls.some((c) => c[0]?.minutes === 30)).toBe(true),
    );
  });

  it('预演端点 404 → 明确告知未就绪', async () => {
    apiMock.previewRouting.mockRejectedValue(Object.assign(new Error('nf'), { status: 404 }));
    const { container } = render(<Routing /> as ReactElement);
    await waitFor(() => expect(screen.getByText('分层档案')).toBeTruthy(), { timeout: 3000 });
    fireEvent.change(container.querySelector('input[placeholder="模型名（请求名）"]') as HTMLElement, {
      target: { value: 'gpt-4o' },
    });
    const btns = screen.getAllByText('预演');
    fireEvent.click(btns[btns.length - 1]);
    await waitFor(() => expect(container.textContent).toContain('预演端点尚未就绪'), {
      timeout: 3000,
    });
  });
});