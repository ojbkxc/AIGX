import { test, expect, loginAsAdmin } from './fixtures';

/**
 * 请求日志 · 「重试」列（`260aaaeb`）。
 *
 * 根因回顾：后端 `RequestLog.attempts` 一直在日志行里（缺省 1），但本页列声明
 * 从 `latency` 直接跳到 `status` —— 「一次就通」还是「试了三家才通」只能点开详情
 * 或去路由回放翻。它加列时顺带给了「>1 上 warning 色 / 1 次次要色」的同列可比性。
 *
 * 这条 spec 之所以必要：`static-server.mjs` 的 REQUEST_LOGS **原先没有 `attempts`
 * 字段**，于是 `l.attempts ?? 1` 让每一行都渲染成 1 —— 「>1 上色」那条臂在 E2E 里
 * 永远不出现。列加上去、CI 全绿，而真正的判据（>1 才上色）从没被执行过。
 * 现在桩给 3 / 2 / 1 三档，两条臂都能断言。
 */

/** 取某一行里「重试」单元格（按表头名定位，不靠列下标 —— 列可被用户拖动/隐藏） */
async function retryCellOfRow(page: import('@playwright/test').Page, rowText: string) {
  const headerCells = await page.locator('.main-content thead th').allInnerTexts();
  const idx = headerCells.map((h) => h.trim()).indexOf('重试');
  expect(idx, `表头里没有「重试」列：${headerCells.join('|')}`).toBeGreaterThanOrEqual(0);
  const row = page.locator('tbody tr', { hasText: rowText }).first();
  return row.locator('td').nth(idx);
}

test.describe('请求日志 · 重试列', () => {
  test('表头有「重试」列；>1 上 warning 徽标、1 次只给数字（同列可比）', async ({ page, pageErrors }) => {
    await loginAsAdmin(page, { sessions: [] });
    await page.goto('/logs');
    await expect(page.locator('.main-content')).toContainText('请求日志');

    // 列在「延迟」与「状态」之间（声明顺序，供列设置面板的插回位用）
    const headers = (await page.locator('.main-content thead th').allInnerTexts()).map((h) => h.trim());
    const iLatency = headers.indexOf('延迟');
    const iRetry = headers.indexOf('重试');
    const iStatus = headers.indexOf('状态');
    expect(iRetry, `表头没有「重试」列：${headers.join('|')}`).toBeGreaterThanOrEqual(0);
    expect(iRetry).toBe(iLatency + 1);
    expect(iStatus).toBe(iRetry + 1);

    // log-1：attempts = 3 → warning 徽标 + title 报出次数
    const cell3 = await retryCellOfRow(page, 'glm-4.7-flash');
    const badge = cell3.locator('.badge-warning');
    await expect(badge).toHaveText('3');
    await expect(badge).toHaveAttribute('title', '共尝试 3 次（含重试）');

    // log-3：attempts = 1 → 不上色（`.badge` 不存在），只给次要色数字
    // 按内容取行别：log-3 是 500 那条（含 error_msg upstream 429），列表里状态徽标为 500
    const cell1 = await retryCellOfRow(page, '500');
    await expect(cell1.locator('.badge')).toHaveCount(0);
    await expect(cell1).toHaveText('1');

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('详情弹窗概览里有「尝试次数」，与列表同源（只给计数，不泄露逐次明细）', async ({ page, pageErrors }) => {
    await loginAsAdmin(page, { sessions: [] });
    await page.goto('/logs');
    await page.locator('tbody tr').first().click();

    // 概览 Tab 默认打开：尝试次数 3 次
    await expect(page.locator('.modal')).toContainText('尝试次数');
    await expect(page.locator('.modal')).toContainText('3 次');
    // 刻意不做的那件事：不给 attempt_details 逐次明细（普通用户会从里面看穿上游拓扑）
    await expect(page.locator('.modal')).not.toContainText('attempt_details');

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });
});
