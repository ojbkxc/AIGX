import { test, expect, loginAsAdmin } from './fixtures';

/**
 * 模型管理页 —— 功能级回归。
 *
 * 覆盖「渠道 + 账号池模型合流」这个核心改造：
 * - 左栏按 provider 分组与计数（含账号池来源标记）
 * - 来源 chips 区分渠道 / 账号
 * - 启停开关可点且请求打到 /api/models/state
 * - 未定价模型高亮 + 顶部统计
 * - 搜索与状态筛选真的过滤行
 */

test.describe('模型管理', () => {
  test.beforeEach(async ({ page }) => {
    await loginAsAdmin(page, { sessions: [] });
    await page.goto('/model-manage');
    await expect(page.locator('h1')).toContainText('模型管理');
  });

  test('左栏按 provider 分组并显示计数', async ({ page, pageErrors }) => {
    const rail = page.locator('.mm-rail');
    await expect(rail).toBeVisible();

    // 「全部」+ 两个 provider 分组
    await expect(rail.locator('.mm-rail-item')).toHaveCount(3);
    await expect(rail).toContainText('全部');
    await expect(rail).toContainText('OpenAI 兼容');
    await expect(rail).toContainText('WorkBuddy 国内版');

    // 账号池来源的分组带 account 标记点
    await expect(rail.locator('.mm-rail-dot.account')).toHaveCount(1);

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('表格展示模型行、来源 chips 与未定价标记', async ({ page, pageErrors }) => {
    const rows = page.locator('.mm-table tbody tr');
    await expect(rows).toHaveCount(3);

    // 渠道来源与账号来源的 chip 文本不同
    await expect(page.locator('.mm-source-chip', { hasText: 'lxchat-glm' })).toBeVisible();
    await expect(page.locator('.mm-source-chip.account', { hasText: 'workbuddy-1' })).toBeVisible();

    // 未定价模型显示占位
    await expect(page.locator('.mm-price-empty')).toHaveCount(1);
    await expect(rows.filter({ hasText: 'unpriced-model' })).toContainText('未定价');

    // 已定价模型显示输入/输出价
    await expect(rows.filter({ hasText: 'glm-4.7-flash' })).toContainText('0.001');

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('点击左栏分组只显示该组模型', async ({ page, pageErrors }) => {
    await page.locator('.mm-rail-item', { hasText: 'WorkBuddy 国内版' }).click();
    await expect(page.locator('.mm-table tbody tr')).toHaveCount(1);
    await expect(page.locator('.mm-table')).toContainText('wb-coder');

    await page.locator('.mm-rail-item', { hasText: '全部' }).click();
    await expect(page.locator('.mm-table tbody tr')).toHaveCount(3);

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('搜索过滤模型行', async ({ page, pageErrors }) => {
    const search = page.locator('.mm-toolbar input.form-input').first();
    await search.fill('glm');
    await expect(page.locator('.mm-table tbody tr')).toHaveCount(1);
    await expect(page.locator('.mm-table')).toContainText('glm-4.7-flash');

    await search.fill('不存在的模型');
    // 无匹配：表格空或显示空态
    const rowCount = await page.locator('.mm-table tbody tr').count();
    expect(rowCount).toBe(0);

    await search.fill('');
    await expect(page.locator('.mm-table tbody tr')).toHaveCount(3);
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('启停开关可用，请求打到 /api/models/state', async ({ page, pageErrors }) => {
    const stateCalls: string[] = [];
    page.on('request', (r) => {
      if (r.url().includes('/api/models/state')) stateCalls.push(r.method());
    });

    const row = page.locator('.mm-table tbody tr', { hasText: 'unpriced-model' });
    // 注意：行内第一个复选框是「批量选择」，启停是 role=switch 的独立控件
    const toggle = row.getByRole('switch');
    await expect(toggle).toBeVisible();
    await toggle.click();

    await expect.poll(() => stateCalls.length, { timeout: 5000 }).toBeGreaterThan(0);
    expect(stateCalls[0]).toBe('POST');
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('元信息覆盖 Tab 可切换并渲染', async ({ page, pageErrors }) => {
    await page.getByRole('tab', { name: '元信息覆盖' }).click();
    const text = await page.locator('.main-content').innerText();
    expect(text.length).toBeGreaterThan(10);
    // 目录 Tab 的表格不应再显示
    await expect(page.locator('.mm-table')).toHaveCount(0);
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);

    await page.getByRole('tab', { name: '模型目录' }).click();
    await expect(page.locator('.mm-table')).toBeVisible();
  });
});
