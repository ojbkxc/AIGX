import { test, expect, loginAsAdmin } from './fixtures';

/**
 * 路由回放 —— 功能级回归（#14 RouteReplay + #26 有界形状 + #31 契约对齐）。
 *
 * ┌─────────────────────────────────────────────────────────────────────────┐
 * │ 形状契约锚点（#31）：mock 必须按后端投影的**扁平形状**构造，             │
 * │ 防止「前端假定形状」假绿。字段清单与出处：                              │
 * │   handler: src/api/admin/logs.rs:692-716 handle_get_route_replay        │
 * │   字段:    src/log/mod.rs                                               │
 * │     candidate_channels: string[]      —— 只有渠道 id，无 name/provider   │
 * │     filtered_channels: {channel_id, reason}[]   (mod.rs:391)            │
 * │     selected_channel: string|null                                     │
 * │     attempts: number                 —— 计数！非逐次明细 (mod.rs:116)   │
 * │     phase: string                    (mod.rs:124)                       │
 * │     route_config: {strategy, sticky_enabled}|null  (mod.rs:401)         │
 * │     route_truncated: number                                            │
 * │     account_skips: {channel_id, skipped:{account_id,account_name,      │
 * │       reason}[], truncated}[]        (mod.rs:438)                       │
 * │ ── 逐次尝试明细（#32 落库，7de5d392）──                                 │
 * │   attempt_details: {index, channel_id, channel_name, provider,         │
 * │     account, model, stickyHit, startedAt, finishedAt, status, error,   │
 * │     notice}[]                        (mod.rs:491-515，serde camelCase) │
 * │   attempt_details_truncated: number  (mod.rs:145；上限 64，:480)        │
 * │   投影：logs.rs:719-720；开关关闭/旧日志不序列化该字段（mod.rs:2286）   │
 * │   → 前端形状守卫：缺省 = 整节不渲染；断言口径 details.length ≤ attempts │
 * │   且 index 连续 1..=n（be-fix 契约）。                                 │
 * │ 注意：无逐次尝试明细时「执行路径」整节不渲染。                         │
 * │ ── 日志列表（用例 4 的 mock 依据，同样钉死）──                          │
 * │   /api/logs/requests → {success, data: [...], total, page, size}       │
 * │   （src/api/admin/logs.rs:192-196；前端 Logs.tsx:185 读 Array.isArray  │
 * │   (res.data)——该对当前正确，勿改坏）                                   │
 * │   行内可见列：时间/状态/模型/渠道/用量/费用/耗时（Logs.tsx:775-808）—— │
 * │   l.id 只作 React key 不进 DOM（:750），定位行须用可见字段（模型/渠道）。│
 * └─────────────────────────────────────────────────────────────────────────┘
 *
 * 新端点用 page.route 层 mock，不动 static-server.mjs（桩文件属 fe-lists-fix 域）。
 */

const FLAT_SNAP = {
  success: true,
  data: {
    log_id: 77,
    request_id: 'req-77',
    model: 'gpt-4o',
    origin_model: 'gpt-4o-raw',
    created_at: 1762000000,
    status_code: 200,
    attempts: 2,
    phase: 'stream',
    candidate_channels: ['ch-1', 'ch-2', 'ch-3'],
    filtered_channels: [
      { channel_id: 'ch-9', reason: '断路器打开（连续失败 5 次）' },
    ],
    selected_channel: 'ch-2',
    route_config: { strategy: 'ranked', sticky_enabled: true },
    route_truncated: 3,
    account_skips: [
      {
        channel_id: 'ch-1',
        skipped: [
          { account_id: 'acc-1', account_name: '号一', reason: '余额不足' },
          { account_id: 'acc-2', account_name: '号二', reason: '冷却中' },
        ],
        truncated: 5,
      },
      {
        channel_id: 'ch-2',
        skipped: [{ account_id: 'acc-3', account_name: '号三', reason: '并发已满' }],
        truncated: 0,
      },
    ],
    // #32 逐次尝试明细（形状 = mod.rs:491-515 camelCase；index 连续 1..=2，
    // ≤ attempts 计数 2——be-fix 断言口径）
    attempt_details: [
      {
        index: 1, channel_id: 'ch-1', channel_name: '渠道一', provider: 'openai',
        account: '', model: 'gpt-4o-2024', stickyHit: false,
        startedAt: 1762000000, finishedAt: 1762000002, status: 502,
        error: '上游 502 Bad Gateway', notice: null,
      },
      {
        index: 2, channel_id: 'ch-2', channel_name: '渠道二', provider: 'openai',
        account: '', model: 'gpt-4o', stickyHit: true,
        startedAt: 1762000002, finishedAt: 1762000003, status: 200,
        error: null, notice: null,
      },
    ],
    attempt_details_truncated: 0,
  },
};

/** 打开「系统设置 → 路由」并提交一个请求 ID 回放 */
async function openReplayPanel(page: import('@playwright/test').Page, requestId: string): Promise<void> {
  await loginAsAdmin(page);
  await page.goto('/settings');
  await page.locator('.settings-nav-item', { hasText: '路由' }).first().click();
  await expect(page.locator('.rr-panel h2', { hasText: '路由回放' })).toBeVisible();
  await page.getByPlaceholder('粘贴请求日志里的请求 ID').fill(requestId);
  await page.getByRole('button', { name: '回放', exact: true }).click();
}

test.describe('路由回放', () => {
  test('回放面板：扁平快照渲染（候选序/被跳过/逐账号分组/策略卡）', async ({ page, pageErrors }) => {
    await page.route('**/api/routing/replay/req-77*', (r) =>
      r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(FLAT_SNAP) }));
    await openReplayPanel(page, 'req-77');

    // 徽标行：策略 / 粘性启用 / 最终承载（扁平形状只有 id → 显示 id）
    const chips = page.locator('.rr-chips');
    await expect(chips).toContainText('多因子调度');
    await expect(chips).toContainText('最终承载：ch-2');

    // 四格指标：状态码 / 尝试次数（计数 2）/ 候选 3 / 跳过 1
    const cells = page.locator('.rr-metrics > div');
    await expect(cells).toHaveCount(4);
    await expect(cells.nth(0)).toContainText('200');
    await expect(cells.nth(1)).toContainText('2');
    await expect(cells.nth(2)).toContainText('3');
    await expect(cells.nth(3)).toContainText('1');

    // 候选顺序：id 可见（无名字来源）、ch-2 高亮选中；ch-1 在序
    const ordered = page.locator('.rr-ordered > li');
    await expect(ordered).toHaveCount(3);
    await expect(ordered.nth(0)).toContainText('ch-1');
    await expect(ordered.nth(1)).toHaveClass(/is-picked/);
    await expect(ordered.nth(1)).toContainText('选中');

    // 被跳过渠道：id + 原因；截断提示（route_truncated=3）
    await expect(page.locator('.rr-blocked')).toContainText('ch-9');
    await expect(page.locator('.rr-blocked')).toContainText('断路器打开');
    await expect(page.locator('.rr-note', { hasText: '候选明细超过记录上限' })).toContainText('3');

    // 逐账号跳过分组（#26）：按渠道折叠 + 截断计数；展开第一组可见账号与原因
    const groups = page.locator('.rr-askip-group');
    await expect(groups).toHaveCount(2);
    const g0 = groups.nth(0);
    await expect(g0.locator('summary')).toContainText('ch-1');
    await expect(g0.locator('summary')).toContainText('还有 5 条未记录');
    await g0.locator('summary').click();
    await expect(g0).toContainText('余额不足');
    await expect(g0).toContainText('冷却中');

    // 策略/粘性卡（route_config）
    await expect(page.locator('.rr-kv')).toContainText('多因子调度');
    await expect(page.locator('.rr-kv')).toContainText('启用');

    // 执行路径时间线（#32）：请求进入 + 2 次尝试；断言口径 details.length(2)
    // ≤ attempts 计数(2) 且 index 连续 1..=n（be-fix 契约）
    await expect(page.locator('.rr-timeline .rr-step')).toHaveCount(3);
    await expect(page.locator('.rr-timeline').getByText('请求进入')).toBeVisible();
    const failedStep = page.locator('.rr-timeline .rr-step-failed');
    await expect(failedStep).toHaveCount(1);
    await expect(failedStep).toContainText('渠道一');
    await expect(failedStep).toContainText('502');
    await expect(failedStep).toContainText('上游 502 Bad Gateway');
    // 成功步：粘滞命中徽章 + 耗时（finishedAt-startedAt = 1s）
    const okStep = page.locator('.rr-timeline .rr-step').nth(2);
    await expect(okStep).toContainText('渠道二');
    await expect(okStep).toContainText('粘滞命中');
    await expect(okStep).toContainText('1.0s');
    await expect(okStep).toContainText('gpt-4o');
    // truncated=0 → 无溢出提示
    await expect(page.locator('.rr-note', { hasText: '尝试明细超过记录上限' })).toHaveCount(0);

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('回放面板：无回放字段空态（旧记录）', async ({ page, pageErrors }) => {
    // 旧记录/开关关闭：后端投影全部缺省（空数组/null）——与真实缺省形状一致
    await page.route('**/api/routing/replay/req-empty*', (r) =>
      r.fulfill({
        status: 200, contentType: 'application/json',
        body: JSON.stringify({
          success: true,
          data: {
            log_id: 1, request_id: 'req-empty', model: 'gpt-4o',
            attempts: 1, phase: 'forward',
            candidate_channels: [], filtered_channels: [],
            selected_channel: null, route_config: null,
            route_truncated: 0, account_skips: [],
          },
        }),
      }));
    await openReplayPanel(page, 'req-empty');
    // rr-panel 域内断言：RouteSticky 的空态同时挂载在相邻卡片，不能裸查 .rr-degraded 全局唯一性以外的语义——这里直接限定面板域
    await expect(page.locator('.rr-panel .rr-degraded')).toContainText('该请求没有保存选路快照');
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('回放面板：端点 404 降级（后端未就绪）', async ({ page, pageErrors }) => {
    await page.route('**/api/routing/replay/req-404*', (r) =>
      r.fulfill({ status: 404, contentType: 'application/json', body: JSON.stringify({ success: false, error: 'not found' }) }));
    await openReplayPanel(page, 'req-404');
    // 限定在回放面板卡片内断言（RouteSticky 的 EmptyState 兄弟挂载，避免 strict 歧义）
    await expect(page.locator('.rr-panel .empty-state').first()).toContainText('路由回放快照端点尚未就绪');
    // 本用例**故意**让端点返回 404，浏览器必然记录一条
    // `console: Failed to load resource: ... 404`（fixtures.ts:72-74 收 console error）。
    // 「自己制造 404 又要求零报错」是自相矛盾的断言，所以只排除这一条预期内的
    // 404 噪声，其余任何 console/page error 仍然拦截。
    const unexpected = pageErrors.filter((e) => !/404/.test(e));
    expect(unexpected, unexpected.join('\n')).toEqual([]);
  });

  test('日志详情：路由回放 Tab 渲染逐账号跳过分组（#26 形状）', async ({ page, pageErrors }) => {
    // 覆盖请求日志列表：一行带 account_skips / route_truncated（其余字段对齐桩形状）
    await page.route('**/api/logs/requests**', (r) =>
      r.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify({
          success: true,
          data: [{
            id: 'log-77',
            created_at: 1762000000,
            user_id: 'u-admin',
            user_email: 'admin@aigx.local',
            model: 'gpt-4o',
            channel_name: '渠道二',
            channel_id: 'ch-2',
            input_tokens: 10,
            output_tokens: 5,
            cost: 1,
            latency_ms: 120,
            status_code: 200,
            candidate_channels: ['ch-1', 'ch-2'],
            filtered_channels: [{ channel_id: 'ch-9', reason: '熔断打开' }],
            account_skips: [
              {
                channel_id: 'ch-1',
                skipped: [{ account_id: 'acc-1', account_name: '号一', reason: '余额不足' }],
                truncated: 2,
              },
            ],
            route_truncated: 1,
            selected_channel: 'ch-2',
          }],
          total: 1,
          page: 1,
          size: 20,
        }),
      }));
    await loginAsAdmin(page);
    await page.goto('/logs');
    // 打开详情：按「可见字段」定位行——表格不渲染 l.id（只作 React key，
    // Logs.tsx:750），行内可见文本是时间/状态/模型/渠道/用量/费用/耗时
    const row = page.locator('tbody tr').filter({ hasText: 'gpt-4o' }).first();
    // 点**模型列**而不是首列：`Logs.tsx:815` 的详情开关挂在 `<tr onClick>`，
    // 而 `Logs.tsx:820` 对首列（勾选框）`td` 做了 `stopPropagation`
    // ——点 `td.first()` 只会切换选中、永远不开详情弹窗（原断言因此超时）。
    await row.getByText('gpt-4o').click();
    await expect(page.locator('.log-detail-modal')).toBeVisible({ timeout: 10_000 });
    await page.locator('.log-detail-tabs button', { hasText: '路由回放' }).click();

    // 基础字段仍在：候选 chips + 被过滤原因
    await expect(page.locator('.log-detail-route')).toContainText('熔断打开');
    // 逐账号跳过分组（形状守卫：无字段的老日志不渲染该节）
    const groups = page.locator('.log-detail-modal .rr-askip-group');
    await expect(groups).toHaveCount(1);
    await expect(groups.first().locator('summary')).toContainText('ch-1');
    await expect(groups.first().locator('summary')).toContainText('还有 2 条未记录');
    await groups.first().locator('summary').click();
    await expect(groups.first()).toContainText('余额不足');

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });
});
