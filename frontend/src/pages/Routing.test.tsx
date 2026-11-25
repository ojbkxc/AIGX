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
  // 画布「流动」的数据源（GET /api/logs/inflight）
  listInflightRequests: vi.fn(),
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
    apiMock.listInflightRequests.mockResolvedValue({ success: true, total: 0, data: [] });
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
  /** 左栏 rail 按钮：按可见文本找（计数徽标在 .rt-nav-count，不参与匹配） */
  const railButton = (container: HTMLElement, label: string): HTMLElement => {
    const found = Array.from(container.querySelectorAll<HTMLElement>('.rt-nav-item')).find(
      (el) => el.querySelector('.rt-nav-label')?.textContent?.trim() === label,
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

    // 左栏头（对齐 A2A RoutingRail 的 SidebarHeader）：标题 + 条目总数 + 新增
    const railHead = container.querySelector('.rt-rail-head');
    expect(railHead).toBeTruthy();
    expect(railHead?.textContent).toContain('路由配置');
    // 2 个默认规则项 + 特殊三项 + 1 个模型档案 + 1 个路由组 = 7
    expect(railHead?.querySelector('.rt-count')?.textContent).toBe('7');
    expect(railHead?.querySelector('button')?.getAttribute('aria-label')).toBe('新增路由配置');

    // 路由组条目显示组名（不是组 id），id 留在 title 里供定位
    expect(labels).toContain('快速组');
    const groupItem = railButton(container, '快速组');
    expect(groupItem.getAttribute('title')).toBe('g1');

    // 页头策略摘要（对齐 A2A routing-page-policy）：策略 · 粘滞 模式
    expect(container.querySelector('.rt-pagebar-policy')?.textContent).toContain('加权随机');
    expect(container.querySelector('.rt-pagebar-policy')?.textContent).toContain('粘滞 自动');

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
    // 切到模型级层 → ProfileForm 渲染三态控件（「路由策略」会同时出现在基础策略卡
    // 与档案表单，故用 getAllByText 防多命中）
    await waitFor(() => expect(screen.getAllByText('路由策略').length).toBeGreaterThanOrEqual(1));
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

  it('回放左栏：列结构 + 筛选项 + 服务端过滤参数 + 翻页（对齐 A2A RoutingRequestList）', async () => {
    apiMock.listRequestLogs.mockResolvedValue({
      success: true,
      total: 45,
      data: [
        {
          id: 'req-aaa',
          request_id: 'chatcmpl-aaa',
          model: 'gpt-4o',
          status_code: 200,
          latency_ms: 820,
          created_at: 1_700_000_000,
          channel_id: 'ch-1',
        },
        {
          id: 'req-bbb',
          request_id: 'chatcmpl-bbb',
          model: 'claude-3',
          status_code: 500,
          latency_ms: 1200,
          created_at: 1_700_000_600,
          channel_id: 'ch-2',
        },
        {
          // status_code=0 = 后端未记录（u16 + serde(default) 的中断/未完成日志）：
          // 必须落「未记录」而不是「失败」，与 Logs.tsx 的 statusKind 同一口径
          id: 'req-ccc',
          request_id: 'chatcmpl-ccc',
          model: 'claude-3',
          status_code: 0,
          latency_ms: 0,
          created_at: 1_700_000_900,
          channel_id: 'ch-3',
        },
      ],
    });
    const { container } = render(<Routing /> as ReactElement);
    await waitFor(() => expect(screen.getByText('分层档案')).toBeTruthy(), { timeout: 3000 });

    fireEvent.click(modeButton(container, '请求回放'));
    await waitFor(() => expect(container.querySelectorAll('.rt-replay-item').length).toBe(3));

    /** 最近一次列表请求的参数（target 是 ES2020，不用 Array.prototype.at） */
    const lastParams = (): Record<string, unknown> | undefined => {
      const calls = apiMock.listRequestLogs.mock.calls;
      return calls.length ? (calls[calls.length - 1][0] as Record<string, unknown>) : undefined;
    };

    // 头：匹配数来自服务端 total（不是本页条数）
    expect(container.querySelector('.rt-replay-rail-title .rt-count')?.textContent).toBe('45');

    // 行结构：模型 + 状态点；副行 = 时间 · 渠道 · 请求 id
    const rows = container.querySelectorAll('.rt-replay-item');
    expect(rows[0].querySelector('.rt-replay-state.is-success')).toBeTruthy();
    expect(rows[1].querySelector('.rt-replay-state.is-failed')).toBeTruthy();
    // status_code=0 → 未记录（中性），不是失败：旧实现在这里渲染成 is-failed
    expect(rows[2].querySelector('.rt-replay-state.is-unknown')).toBeTruthy();
    expect(rows[2].querySelector('.rt-replay-state.is-failed')).toBeNull();
    expect(rows[2].textContent).toContain('未记录');
    expect(rows[0].textContent).toContain('chatcmpl-aaa');
    expect(rows[0].textContent).toContain('#ch-1');

    // 首次请求参数：page 1 / size 30（A2A 每页 30 条）
    expect(apiMock.listRequestLogs).toHaveBeenLastCalledWith({ page: 1, size: 30 });

    // 搜索 → 服务端 keyword，且页码归零
    const search = container.querySelector<HTMLInputElement>(
      'input[aria-label="搜索请求、模型或账号"]',
    );
    expect(search).toBeTruthy();
    fireEvent.change(search as HTMLInputElement, { target: { value: 'aaa' } });
    await waitFor(() =>
      expect(apiMock.listRequestLogs).toHaveBeenLastCalledWith({ page: 1, size: 30, keyword: 'aaa' }),
    );

    // 请求模型下拉：候选取本页出现过的模型
    const modelSelect = container.querySelector<HTMLSelectElement>('select[aria-label="请求模型"]');
    expect(Array.from(modelSelect?.options ?? []).map((o) => o.value)).toEqual([
      '',
      'claude-3',
      'gpt-4o',
    ]);
    fireEvent.change(modelSelect as HTMLSelectElement, { target: { value: 'gpt-4o' } });
    await waitFor(() =>
      expect(apiMock.listRequestLogs).toHaveBeenLastCalledWith({
        page: 1,
        size: 30,
        keyword: 'aaa',
        model: 'gpt-4o',
      }),
    );

    // 时间范围：60 分钟档 → start = now - 60min（unix 秒）
    const before = Math.floor((Date.now() - 60 * 60_000) / 1000);
    const windowSelect = container.querySelector<HTMLSelectElement>(
      'select[aria-label="已完成请求时间范围"]',
    );
    expect(Array.from(windowSelect?.options ?? []).map((o) => o.value)).toEqual([
      '0',
      '5',
      '10',
      '30',
      '60',
    ]);
    fireEvent.change(windowSelect as HTMLSelectElement, { target: { value: '60' } });
    await waitFor(() => {
      const last = lastParams() as { start?: number } | undefined;
      expect(last?.start).toBeGreaterThanOrEqual(before);
      expect(last?.start).toBeLessThanOrEqual(before + 5);
    });

    // 仅失败 → status=error
    fireEvent.click(screen.getByText('仅失败'));
    await waitFor(() => expect(lastParams()).toMatchObject({ status: 'error' }));

    // 翻页：total=45 → 2 页；下一页 → page 2
    expect(container.querySelector('.rt-replay-foot')?.textContent).toContain('1 / 2');
    const next = Array.from(container.querySelectorAll<HTMLElement>('.rt-replay-foot button')).find(
      (b) => b.textContent === '下一页',
    );
    expect(next).toBeTruthy();
    fireEvent.click(next as HTMLElement);
    await waitFor(() => expect(lastParams()).toMatchObject({ page: 2 }));
  });

  it('有未保存修改时「重新读取」先确认（对齐 A2A reloadConfig，不静默丢草稿）', async () => {
    const { container } = render(<Routing /> as ReactElement);
    await waitFor(() => expect(screen.getByText('分层档案')).toBeTruthy(), { timeout: 3000 });

    // 先制造一处草稿修改：切到模型级层（gpt-4o），清空一个三态字段
    const modelNav = railButton(container, 'gpt-4o');
    expect(modelNav).toBeTruthy();
    fireEvent.click(modelNav as HTMLElement);
    await waitFor(() => expect(screen.getAllByText('路由策略').length).toBeGreaterThanOrEqual(1));

    const clearBtns = Array.from(container.querySelectorAll('.rt-seg button')).filter(
      (b) => b.textContent === '清空',
    );
    expect(clearBtns.length).toBeGreaterThan(0);
    fireEvent.click(clearBtns[0]);
    await waitFor(() => expect(container.textContent).toContain('有未保存的修改'));

    // 点页头「重新读取」→ 先出确认，不直接重读
    const reload = Array.from(container.querySelectorAll<HTMLElement>('.rt-pagebar-actions button')).find(
      (b) => b.textContent?.includes('重新读取'),
    );
    expect(reload).toBeTruthy();
    apiMock.getRoutingPolicy.mockClear();
    fireEvent.click(reload as HTMLElement);
    await waitFor(() =>
      expect(container.textContent).toContain('重新读取会丢弃未保存的修改'),
    );
    // 未确认前不发新请求（草稿还在）
    expect(apiMock.getRoutingPolicy).not.toHaveBeenCalled();
    expect(container.textContent).toContain('有未保存的修改');
  });

  /**
   * 配置画布（对齐 A2A RoutingConfigShell / RoutingCanvasLayout）。
   *
   * 钉住四件事，缺一条这套结构就会退化成「看起来像画布」：
   * 1. 账号路由视图 = 点阵外壳 + 连线层 + 三句图例（文案与图标形态照 A2A）
   * 2. 连线的 data 判据真的落在 DOM 上（源卡 / 组节点 / 成员槽位 / 作用域条目）
   * 3. 没有在途数据时**不画流动**（不许静态线冒充实时线）
   * 4. 特殊请求路由走 plain：去点阵、不挂连线层、不显示图例
   */
  it('画布外壳：点阵 + 连线层 + 三句图例，连线的 data 判据落在 DOM 上', async () => {
    const { container } = render(<Routing /> as ReactElement);
    await waitFor(() => expect(screen.getByText('分层档案')).toBeTruthy(), { timeout: 3000 });

    // ① 外壳与画布布局
    const shell = container.querySelector('.routing-config-shell');
    expect(shell).toBeTruthy();
    expect(shell?.classList.contains('is-plain')).toBe(false);
    const canvas = container.querySelector('.routing-canvas');
    expect(canvas).toBeTruthy();
    // 点阵只挂在 shell 上（A2A 点径 .8px / 间距 20px 在 CSS 里，这里钉结构归属）
    expect(shell?.querySelector('.routing-canvas-edges')).toBeTruthy();

    // ② 三句图例：连线表示参与本路由 / 流动：有请求在途 / 保存后才会影响真实请求
    const legend = container.querySelector('.routing-canvas-legend');
    expect(legend).toBeTruthy();
    expect(legend?.textContent).toContain('连线表示参与本路由');
    expect(legend?.textContent).toContain('流动：有请求在途');
    expect(legend?.textContent).toContain('保存后才会影响真实请求');
    // 图例三种形态：<i/> 静态线段、<svg><path d='M 0 4 H 22'/> 流动示例
    expect(legend?.querySelector('i')).toBeTruthy();
    expect(legend?.querySelector('svg path')?.getAttribute('d')).toBe('M 0 4 H 22');

    // ③ 源卡 = 左列配置（连线的起点），组节点列 = 右列
    expect(container.querySelector('[data-route-source]')).toBeTruthy();
    const nodes = Array.from(container.querySelectorAll<HTMLElement>('[data-route-node]'));
    expect(nodes.map((n) => n.dataset.routeNode)).toEqual(['g1']);
    // 全局层没有 default_group → 组不连通；成员槽位行也标了 data-route-account
    expect(nodes[0].hasAttribute('data-route-connected')).toBe(false);
    expect(nodes[0].hasAttribute('data-route-expanded')).toBe(true);
    expect(
      Array.from(nodes[0].querySelectorAll<HTMLElement>('[data-route-account]')).map(
        (a) => a.dataset.routeAccount,
      ),
    ).toEqual(['g1:0']);
    // 作用域条目（rail 上被选中的层）带 data-active，连线起点靠它
    const scope = container.querySelector('[data-route-scope][data-active]');
    expect(scope?.getAttribute('data-route-scope')).toBe('global');

    // ④ 没有在途数据 → 一条流动线都不许有
    expect(container.querySelector('.routing-canvas-edges .is-running')).toBeNull();
  });

  it('画布连通判据：模型级层引用了 g1（default_group）→ 组节点与成员槽位连通', async () => {
    const { container } = render(<Routing /> as ReactElement);
    await waitFor(() => expect(screen.getByText('分层档案')).toBeTruthy(), { timeout: 3000 });

    // 切到模型级 gpt-4o（它的 default_group = g1）
    fireEvent.click(railButton(container, 'gpt-4o'));
    await waitFor(() =>
      expect(container.querySelector('[data-route-scope][data-active]')?.getAttribute('data-route-scope')).toBe('m:gpt-4o'),
    );

    const node = container.querySelector<HTMLElement>('[data-route-node="g1"]');
    expect(node).toBeTruthy();
    expect(node?.hasAttribute('data-route-connected')).toBe(true);
    // 成员槽位 included=true → 槽位行也连通
    expect(
      container.querySelector('[data-route-account="g1:0"]')?.hasAttribute('data-route-connected'),
    ).toBe(true);
    // 仍然没有在途 → 静态线
    expect(container.querySelector('.routing-canvas-edges .is-running')).toBeNull();
  });

  it('画布流动态：有在途请求落到该组时才 is-running（数据缺失就保持静态）', async () => {
    // 在途模型 = gpt-4o，且模型级档案的 default_group = g1 → 组与源卡都在途
    apiMock.listInflightRequests.mockResolvedValue({
      success: true,
      total: 1,
      data: [{ id: 'inflight-1', model: 'gpt-4o', started_at: 1, elapsed_ms: 1000, stream: true }],
    });
    const { container } = render(<Routing /> as ReactElement);
    await waitFor(() => expect(screen.getByText('分层档案')).toBeTruthy(), { timeout: 3000 });

    fireEvent.click(railButton(container, 'gpt-4o'));
    await waitFor(() =>
      expect(container.querySelector('[data-route-node="g1"]')?.hasAttribute('data-route-running')).toBe(
        true,
      ),
    );
    // 源卡同样在途（A2A 的 data-route-running → is-running 连线）
    expect(container.querySelector('[data-route-source]')?.hasAttribute('data-route-running')).toBe(
      true,
    );
    // 成员槽位行不置 running：日志里没有账号↔成员的归因依据（宁可静态）
    expect(
      container.querySelector('[data-route-account="g1:0"]')?.hasAttribute('data-route-running'),
    ).toBe(false);
  });

  it('特殊请求路由走 plain：去点阵、不挂连线层、不显示图例', async () => {
    const { container } = render(<Routing /> as ReactElement);
    await waitFor(() => expect(screen.getByText('分层档案')).toBeTruthy(), { timeout: 3000 });

    fireEvent.click(railButton(container, '压缩上下文'));
    await waitFor(() =>
      expect(container.querySelector('.routing-config-shell')?.classList.contains('is-plain')).toBe(
        true,
      ),
    );
    expect(container.querySelector('.routing-canvas-edges')).toBeNull();
    expect(container.querySelector('.routing-canvas-legend')).toBeNull();
    expect(container.querySelector('.routing-canvas')).toBeNull();
  });
});
