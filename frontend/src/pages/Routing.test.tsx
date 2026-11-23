/**
 * 路由页骨架回归（v1.0.93 起对齐 A2A 工作台形态）。
 *
 * 钉住三件事：
 * 1. **模式分段 + 配置 rail**：顶部「路由配置 / 请求回放」两段；配置模式下
 *    左栏 rail = 全局默认 / 错误处理 / 压缩上下文 / 图片兼容 / 标题生成
 *    （特殊三项来自 SPECIAL_RAIL），且切走再切回不丢未保存的档案草稿
 *    （草稿存页面 state、切 rail 只是不渲染，不是重置）。
 * 2. **回放模式**：切到「请求回放」呈现最近请求列表；时间窗切 30 分钟用
 *    新 minutes 重新请求 /routing/flow。
 * 3. 既有能力面（分层档案 / 三态控件）在新 rail 下仍原样可用。
 *
 * 注：预演（preview）工作台自 v1.0.93 起按「严格对齐 A2A」隐藏
 * （Routing.tsx 的 SHOW_ROUTING_ENHANCED=false），不再测试预演交互，
 * 只断言页面不渲染预演入口 —— 防止它悄悄回来。
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
  // v1.0.93 错误处理面板
  getRoutingErrors: vi.fn(),
  saveRoutingErrors: vi.fn(),
  // 请求回放模式：左栏「最近请求」走 requestLogs 列表端点
  listRequestLogs: vi.fn(),
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
    apiMock.getRoutingErrors.mockResolvedValue({ success: true, data: { rules: [] } });
    apiMock.listRequestLogs.mockResolvedValue({ success: true, data: [] });
  });
  afterEach(() => cleanup());

  /** 模式分段按钮（顶部 tablist）：按可见文本找。 */
  const modeButton = (container: HTMLElement, label: string): HTMLElement => {
    const found = Array.from(container.querySelectorAll<HTMLElement>('.rt-mode-btn')).find(
      (el) => el.textContent?.trim().includes(label),
    );
    if (!found) throw new Error(`mode button not found: ${label}`);
    return found;
  };
  /** 左栏 rail 按钮：按可见文本找。 */
  const railButton = (container: HTMLElement, label: string): HTMLElement => {
    const found = Array.from(container.querySelectorAll<HTMLElement>('.rt-nav-item')).find(
      (el) => el.textContent?.trim() === label,
    );
    if (!found) throw new Error(`rail item not found: ${label}`);
    return found;
  };

  it('模式分段两项 + 配置 rail 五区 + 切 rail 不丢未保存草稿 + 预演不出现', async () => {
    const { container } = render(<Routing /> as ReactElement);
    await waitFor(() => expect(screen.getByText('分层档案')).toBeTruthy(), { timeout: 3000 });

    // 顶部模式分段：路由配置 / 请求回放
    expect(modeButton(container, '路由配置')).toBeTruthy();
    expect(modeButton(container, '请求回放')).toBeTruthy();

    // 配置 rail：全局默认 / 错误处理 + 特殊三项（压缩上下文 / 图片兼容 / 标题生成）
    // 注：动态的「模型路由 gpt-4o」「路由组 g1」条目不进这一断言（数量随数据变）
    const labels = Array.from(container.querySelectorAll('.rt-nav-label')).map((el) =>
      el.textContent?.trim(),
    );
    ['全局默认', '错误处理', '压缩上下文', '图片兼容', '标题生成'].forEach((want) =>
      expect(labels).toContain(want),
    );

    // 总开关提示（enabled=true 走「已启用」分支）
    expect(container.textContent).toContain('多规则已启用');
    // 层栏：全局 + 模型级 gpt-4o（rail 里模型条目；模型档案区还会出现同名
    // 副本 —— 用 getAllByText 防多命中）
    expect(screen.getAllByText('gpt-4o').length).toBeGreaterThanOrEqual(1);

    // 切到模型级层 → ProfileForm 渲染三态控件；改一笔草稿（清空）。
    // gpt-4o 会出现在多个位置（模型路由 rail 条目 + 模型档案区），取全部
    // 里的第一个可点击项。
    const modelEntries = screen.getAllByText('gpt-4o');
    expect(modelEntries.length).toBeGreaterThanOrEqual(1);
    fireEvent.click(modelEntries[0]);
    await waitFor(() => expect(screen.getByText('选路策略')).toBeTruthy());
    expect(container.textContent).toContain('模型名请填客户端的请求名');
    expect(container.querySelectorAll('.rt-seg').length).toBeGreaterThanOrEqual(6);

    const clearBtns = Array.from(container.querySelectorAll('.rt-seg button')).filter(
      (b) => b.textContent === '清空',
    );
    expect(clearBtns.length).toBeGreaterThan(0);
    fireEvent.click(clearBtns[0]);
    await waitFor(() => expect(container.textContent).toContain('有未保存的修改'));

    // 切到「压缩上下文」再切回「全局默认」：草稿仍在（rail 切换不是重置）
    fireEvent.click(railButton(container, '压缩上下文'));
    await waitFor(() => expect(container.textContent).toContain('压缩上下文'));
    fireEvent.click(railButton(container, '全局默认'));
    await waitFor(() => expect(container.textContent).toContain('有未保存的修改'));

    // 预演工作台自 v1.0.93 隐藏：页面不应渲染预演输入/按钮
    expect(
      container.querySelector('input[placeholder="模型名（请求名）"]'),
    ).toBeNull();
    expect(screen.queryAllByText('预演').length).toBe(0);
  });

  it('回放模式：进入拉最近请求列表；空态提示 + 选中请求后不回退空态', async () => {
    apiMock.getRouteReplay.mockClear();
    const { container } = render(<Routing /> as ReactElement);
    await waitFor(() => expect(screen.getByText('分层档案')).toBeTruthy(), { timeout: 3000 });

    // 切到回放模式
    fireEvent.click(modeButton(container, '请求回放'));
    await waitFor(() => expect(container.textContent).toContain('暂无可回放的请求'));
    // 左栏「最近请求」走 requestLogs 列表端点
    expect(apiMock.listRequestLogs).toHaveBeenCalled();

    // 右栏默认是按 ID 回放面板（输入框提示在）；空列表不渲染请求按钮
    expect(container.querySelectorAll('.rt-replay-item').length).toBe(0);
    // 回放明细端点未被拉（未选中任何请求）
    expect(apiMock.getRouteReplay).not.toHaveBeenCalled();
  });
});
