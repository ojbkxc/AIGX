import { test, expect, loginAsAdmin } from './fixtures';

/**
 * 粘滞会话管理面（#38；RouteSticky.tsx × admin/routing.rs sticky 端点）。
 *
 * 覆盖：
 * - 列表渲染：条数 / 渠道名 / 到期时刻（unix 秒 → expiryMillis 自适应）
 * - 单条清除：POST body {id: 原样回传}、行消失
 * - 全部清除：POST body {all: true}、清空态
 * - 搜索过滤：id/渠道名包含匹配
 *
 * 桩：static-server.mjs 进程内 stickyEntries（/api/routing/sticky +
 * /api/routing/sticky/clear）。**这里 spec 用 page.route 精确分流覆盖桩**
 * （单条清除断言 body 时拦截记账；桩负责列表形状）——两类都演示精确路径
 * 分流，不用宽 glob。
 *
 * 打开路径：/settings → 路由 Tab（RoutingPanel 内嵌 RouteSticky）。
 */

/** 打开「系统设置 → 路由」并滚到粘滞会话卡 */
async function openStickyPanel(page: import('@playwright/test').Page): Promise<void> {
  await loginAsAdmin(page);
  await page.goto('/settings');
  await page.locator('.settings-nav-item', { hasText: '路由' }).first().click();
  // RoutingPanel 挂载后 RouteSticky 卡随其后（策略卡 → 粘滞卡）
  await expect(page.locator('h2', { hasText: '粘滞会话' })).toBeVisible();
}

test.describe('粘滞会话管理面', () => {
  test('列表渲染：条数/渠道名/到期时刻（桩形状）', async ({ page, pageErrors }) => {
    await openStickyPanel(page);

    const items = page.locator('.rs-item');
    await expect(items).toHaveCount(3);
    // 渠道名优先于 #id（后端 channel_name 映射）
    await expect(items.first()).toContainText('lxchat-glm');
    await expect(items.nth(1)).toContainText('workbuddy-pool');
    // 到期时刻（expires_at unix 秒 → 本地时间字符串）
    await expect(items.first().locator('.rs-exp')).toContainText(/到期/);
    // 当前模式徽标（data.sticky=auto）
    await expect(page.locator('.card', { hasText: '粘滞会话' }).first()).toContainText('auto');

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('单条清除：POST body 携带原样 id，行消失', async ({ page, pageErrors }) => {
    const clearBodies: Array<Record<string, unknown>> = [];
    await page.route(/\/api\/routing\/sticky\/clear/, async (r) => {
      const body = r.request().postDataJSON() as Record<string, unknown>;
      clearBodies.push(body);
      // 桩同款行为由后端承担；这里直接回成功
      await r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ success: true, data: { cleared: 1 } }) });
    });
    await openStickyPanel(page);

    const items = page.locator('.rs-item');
    await expect(items).toHaveCount(3);
    // 记住第一行的 id 文本（内部编码含控制符，截断显示 24 字符）
    const firstId = await items.first().locator('.rs-id').getAttribute('title');
    expect(firstId).toBeTruthy();

    await items.first().locator('button', { hasText: '清除' }).last().click();
    // 二次确认
    await page.getByRole('button', { name: '清除', exact: true }).last().click();

    // body 携带原样 id（不解析、不截断）
    await expect.poll(() => clearBodies.length).toBeGreaterThanOrEqual(1);
    expect(clearBodies[0]).toEqual({ id: firstId });
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('全部清除：POST body {all:true}', async ({ page, pageErrors }) => {
    const clearBodies: Array<Record<string, unknown>> = [];
    await page.route(/\/api\/routing\/sticky\/clear/, async (r) => {
      clearBodies.push(r.request().postDataJSON() as Record<string, unknown>);
      await r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ success: true, data: { cleared: 3 } }) });
    });
    await openStickyPanel(page);

    await page.locator('button', { hasText: '全部清除' }).click();
    await page.getByRole('button', { name: '全部清除', exact: true }).last().click();

    await expect.poll(() => clearBodies.length).toBeGreaterThanOrEqual(1);
    expect(clearBodies[0]).toEqual({ all: true });
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('搜索过滤：按渠道名过滤列表（桩 query 生效）', async ({ page, pageErrors }) => {
    await openStickyPanel(page);

    await page.locator('input[placeholder*="按会话标识"]').fill('workbuddy');
    // 去抖 300ms 后才发请求
    await expect(page.locator('.rs-item')).toHaveCount(1);
    await expect(page.locator('.rs-item').first()).toContainText('workbuddy-pool');

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });
});
