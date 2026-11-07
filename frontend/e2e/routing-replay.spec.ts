import { test, expect, loginAsAdmin } from './fixtures';

/**
 * 路由回放 —— 功能级回归（#14 RouteReplay + #26 有界形状）。
 *
 * 覆盖：
 * - 回放面板快照渲染：候选顺序（选中高亮/优先级权重/原因）、被跳过渠道
 *   （状态徽标）、执行时间线（进入 + 逐次尝试的阶段/耗时/错误）、逐账号跳过
 *   分组（#26：按渠道折叠 + 截断计数）、配置快照卡、四格指标
 * - 空态降级：data=null（旧记录无快照）
 * - 端点 404 降级：后端未就绪提示
 * - 日志详情「路由回放」Tab：基础字段 + 逐账号跳过分组同构渲染
 *
 * 全部新端点用 page.route 层 mock，不动 static-server.mjs（桩文件属 fe-lists-fix 域）。
 */

const SNAP = {
  success: true,
  data: {
    request_id: 'req-77',
    model: 'gpt-4o',
    status: 200,
    duration_ms: 2345,
    first_response_ms: 410,
    selected: 'ch-2',
    route: {
      strategy: 'ranked',
      sticky: 'auto',
      sticky_hit: true,
      session_id: 'sess-abcdef1234567890',
      ordered: [
        { id: 'ch-1', name: '渠道一', provider: 'openai', priority: 1, weight: 100 },
        { id: 'ch-2', name: '渠道二', provider: 'openai', priority: 1, weight: 60 },
        { id: 'ch-3', name: '渠道三', provider: 'anthropic', priority: 2, weight: 10, reason: '多因子评分第 3' },
      ],
      blocked: [
        { id: 'ch-9', name: '熔断渠道', provider: 'openai', state: 'open', reason: '连续失败 5 次熔断' },
      ],
      config_snapshot: { captured_at: 1762000000, provider_count: 2, account_count: 3 },
    },
    attempts: [
      { index: 1, channel_id: 'ch-1', channel_name: '渠道一', phase: 'forward', status: 502, duration_ms: 1800, error: '上游 502 Bad Gateway' },
      { index: 2, channel_id: 'ch-2', channel_name: '渠道二', phase: 'stream', status: 200, duration_ms: 545 },
    ],
    account_skips: [
      {
        channel_id: 'ch-1',
        skipped: [
          { account_id: 'acc-1', account_name: '号一', reason: '余额不足' },
          { account_id: 'acc-2', account_name: '号二', reason: '冷却中' },
        ],
        truncated: 5,
      },
      { channel_id: 'ch-2', skipped: [{ account_id: 'acc-3', account_name: '号三', reason: '并发已满' }] },
    ],
    route_truncated: 3,
  },
};

/** 打开「系统设置 → 路由」并提交一个请求 ID 回放 */
async function openReplayPanel(page: import('@playwright/test').Page, requestId: string): Promise<void> {
  await loginAsAdmin(page);
  await page.goto('/settings');
  await page.locator('.settings-nav-item', { hasText: '路由' }).first().click();
  await expect(page.locator('h2', { hasText: '路由回放' })).toBeVisible();
  await page.getByPlaceholder('粘贴请求日志里的请求 ID').fill(requestId);
  await page.getByRole('button', { name: '回放', exact: true }).click();
}

test.describe('路由回放', () => {
  test('回放面板：快照渲染（候选/时间线/逐账号跳过/截断计数）', async ({ page, pageErrors }) => {
    await page.route('**/api/routing/replay/req-77*', (r) =>
      r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(SNAP) }));
    await openReplayPanel(page, 'req-77');

    // 徽标行：策略 / 粘滞命中 / 最终承载
    const chips = page.locator('.rr-chips');
    await expect(chips).toContainText('多因子调度');
    await expect(chips).toContainText('命中会话粘滞');
    await expect(chips).toContainText('最终承载：渠道二');

    // 四格指标：总耗时 / 首帧 / 尝试次数 2 / 跳过候选 1（单元格内无嵌套 div，按下标取）
    const cells = page.locator('.rr-metrics > div');
    await expect(cells).toHaveCount(4);
    await expect(cells.nth(0)).toContainText('2.35s');
    await expect(cells.nth(1)).toContainText('410ms');
    await expect(cells.nth(2)).toContainText('2');
    await expect(cells.nth(3)).toContainText('1');

    // 执行时间线：请求进入 + 两次尝试（失败步骤带错误与红色标记）
    await expect(page.locator('.rr-timeline .rr-step')).toHaveCount(3);
    await expect(page.locator('.rr-timeline').getByText('请求进入')).toBeVisible();
    const failedStep = page.locator('.rr-step-failed');
    await expect(failedStep).toContainText('渠道一');
    await expect(failedStep).toContainText('502');
    await expect(failedStep).toContainText('上游 502 Bad Gateway');

    // 候选顺序：选中行高亮 + 权重/原因徽注
    const ordered = page.locator('.rr-ordered > li');
    await expect(ordered).toHaveCount(3);
    await expect(ordered.nth(1)).toHaveClass(/is-picked/);
    await expect(ordered.nth(1)).toContainText('选中');
    await expect(ordered.nth(2)).toContainText('多因子评分第 3');

    // 被跳过渠道：状态徽标 + 原因
    await expect(page.locator('.rr-blocked')).toContainText('熔断渠道');
    await expect(page.locator('.rr-blocked')).toContainText('连续失败 5 次熔断');

    // 逐账号跳过分组（#26）：按渠道折叠 + 截断计数；展开第一组可见账号与原因
    const groups = page.locator('.rr-askip-group');
    await expect(groups).toHaveCount(2);
    const g0 = groups.nth(0);
    await expect(g0.locator('summary')).toContainText('渠道一');
    await expect(g0.locator('summary')).toContainText('还有 5 条未记录');
    await g0.locator('summary').click();
    await expect(g0).toContainText('余额不足');
    await expect(g0).toContainText('冷却中');
    // 全局截断提示（filtered 溢出计数）
    await expect(page.locator('.rr-note', { hasText: '候选明细超过记录上限' })).toContainText('3');

    // 配置快照卡
    await expect(page.locator('.rr-kv')).toContainText('2 个提供商 / 3 个账号');

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('回放面板：无快照空态（旧记录）', async ({ page, pageErrors }) => {
    await page.route('**/api/routing/replay/req-empty*', (r) =>
      r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ success: true, data: null }) }));
    await openReplayPanel(page, 'req-empty');
    await expect(page.locator('.rr-degraded')).toContainText('该请求没有保存选路快照');
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('回放面板：端点 404 降级（后端未就绪）', async ({ page, pageErrors }) => {
    await page.route('**/api/routing/replay/req-404*', (r) =>
      r.fulfill({ status: 404, contentType: 'application/json', body: JSON.stringify({ success: false, error: 'not found' }) }));
    await openReplayPanel(page, 'req-404');
    await expect(page.locator('.empty-state')).toContainText('路由回放快照端点尚未就绪');
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
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
    await page.locator('tr', { hasText: 'log-77' }).first().click();
    await expect(page.locator('.log-detail-modal')).toBeVisible();
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
