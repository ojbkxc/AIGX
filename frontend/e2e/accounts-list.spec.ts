import { test, expect, loginAsAdmin } from './fixtures';

/**
 * 账号池列表能力回归（07 报告 A.4 ①②③ 的固化）：
 * - 分组折叠：分组带点击收起/展开，收起记忆写 localStorage
 * - 分组带统计：启用/禁用计数 + ⋯ 菜单（整组启停入口）
 * - 凭证有效期列：未来到期「N 天后」/ 已过期红徽章 / 无到期「—」/ 套餐到期回退
 * - 待生效套餐徽章（last_balance.plans pending）
 * - 按到期排序：已过期最前、无到期最后
 *
 * 桩数据（static-server.mjs）：4 条账号 —— openai_compatible×2（5 天后到期/无到期）
 * + workbuddy×2（套餐 2 小时后到期+待生效/已过期），分组后两组各 2 行。
 */

const dataRows = (page: import('@playwright/test').Page) =>
  page.locator('.as-table tbody tr:not(.as-tr-group):not(.as-panel-row)');

test.describe('账号池列表能力', () => {
  test.beforeEach(async ({ page }) => {
    await loginAsAdmin(page);
    await page.goto('/channels');
    // 清掉列配置/折叠记忆，避免上一个用例的偏好串场（evaluate 只作用于
    // 当前页面，不会像 addInitScript 那样在同用例的 reload 里再清一次）
    await page.evaluate(() => {
      localStorage.removeItem('aigx-as-collapsed');
      localStorage.removeItem('aigx-as-colcfg');
      localStorage.removeItem('aigx-as-colw');
      localStorage.removeItem('aigx-as-grouped');
    });
    // 进「账号池」Tab（Gateways 多 Tab 容器）
    await page.getByRole('tab', { name: '账号池' }).click();
    await expect(page.locator('.as-table')).toBeVisible({ timeout: 10000 });
  });

  test('分组模式：分组带带统计与家名，两组各两行', async ({ page, pageErrors }) => {
    // 打开「分组」开关
    await page.locator('.as-toggle', { hasText: '分组' }).locator('input').check();
    const bands = page.locator('.as-tr-group');
    await expect(bands).toHaveCount(2);

    // 每条分组带：家名 + 计数 + 启用统计
    await expect(bands.first()).toContainText(/2/);
    await expect(page.locator('.as-group-stat').first()).toContainText('启用');

    // 分组后数据行 = 4
    await expect(dataRows(page)).toHaveCount(4);
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('分组折叠：点分组带收起该组，记忆落 localStorage，再点展开', async ({ page, pageErrors }) => {
    await page.locator('.as-toggle', { hasText: '分组' }).locator('input').check();
    await expect(page.locator('.as-tr-group')).toHaveCount(2);
    await expect(dataRows(page)).toHaveCount(4);

    // 点第一条分组带的家名文本（非按钮区，桌面/移动两种视口都在）
    await page.locator('.as-group-band .as-group-label').first().click();
    await expect(dataRows(page)).toHaveCount(2);
    // 折叠箭头转向
    await expect(page.locator('.as-group-fold').first()).toHaveAttribute('aria-expanded', 'false');

    // 记忆落盘：收起的组 id 在 localStorage
    const collapsed = await page.evaluate(() => localStorage.getItem('aigx-as-collapsed'));
    expect(collapsed && JSON.parse(collapsed).length).toBe(1);

    // 再点箭头展开回来
    await page.locator('.as-group-fold').first().click();
    await expect(dataRows(page)).toHaveCount(4);
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('分组带 ⋯ 菜单：整组停用打到 PATCH 端点', async ({ page, pageErrors }) => {
    const patchCalls: string[] = [];
    page.on('request', (r) => {
      if (/\/api\/account-sessions\/[^/]+$/.test(r.url()) && r.method() === 'PATCH') patchCalls.push(r.url());
    });

    await page.locator('.as-toggle', { hasText: '分组' }).locator('input').check();
    // 打开第一组的 ⋯ 菜单
    await page.locator('.as-group-menu-btn').first().click();
    const menu = page.locator('.as-group-menu');
    await expect(menu).toBeVisible();
    await expect(menu).toContainText('全部启用');
    await expect(menu).toContainText('全部停用');

    // 点「全部停用（2）」：该组两条 enabled 账号各一次 PATCH
    await menu.locator('.as-group-menu-item', { hasText: '全部停用' }).click();
    await expect.poll(() => patchCalls.length, { timeout: 5000 }).toBeGreaterThanOrEqual(1);
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('凭证有效期列：未来/已过期/无到期三种形态 + 待生效套餐徽章', async ({ page, pageErrors }) => {
    // 有效期列默认显示
    await expect(page.locator('.as-h-expiry')).toHaveCount(1);

    // 5 天后到期 → 「N 天后」（floor 取整：跨天级剩余，天数随服务器快照
    // 与渲染时刻的秒差在 N-1~N 间浮动，断言形态而非具体数字）
    await expect(dataRows(page).filter({ hasText: 'e2e-账号' })).toContainText(/天后/);
    // 已过期 → 红徽章
    await expect(dataRows(page).filter({ hasText: 'wb-二号' }).locator('.badge-danger')).toBeVisible();
    // 无到期 → 「—」
    await expect(dataRows(page).filter({ hasText: 'e2e-无到期' }).locator('.as-c-expiry')).toContainText('—');
    // 套餐 2 小时后到期（无凭证到期时回退套餐）+ 待生效徽章
    const wb1 = dataRows(page).filter({ hasText: 'wb-一号' });
    await expect(wb1.locator('.as-c-expiry')).toContainText('小时后');
    await expect(wb1.locator('.as-pending-badge')).toBeVisible();
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('按到期排序：已过期最前、无到期最后', async ({ page, pageErrors }) => {
    // 排序改下拉（对齐上游 accounts-sort 三档：调度优先序/按到期/按添加时间）
    await page.locator('select[aria-label="列表排序"]').selectOption('credit');
    const rows = dataRows(page);
    await expect(rows).toHaveCount(4);
    // 第一行 = 已过期（wb-二号），最后一行 = 无到期（e2e-无到期）
    await expect(rows.first()).toContainText('wb-二号');
    await expect(rows.last()).toContainText('e2e-无到期');
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('折叠跨刷新保持（localStorage 记忆）', async ({ page, pageErrors }) => {
    await page.locator('.as-toggle', { hasText: '分组' }).locator('input').check();
    await page.locator('.as-group-band .as-group-label').first().click();
    await expect(dataRows(page)).toHaveCount(2);

    // 刷新页面（URL 带 tab=accounts）：分组开关与折叠态都从 localStorage 恢复
    await page.goto('/channels?tab=accounts');
    await expect(page.locator('.as-table')).toBeVisible({ timeout: 10000 });
    await expect(page.locator('.as-tr-group')).toHaveCount(2);
    await expect(dataRows(page)).toHaveCount(2);
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });
});
