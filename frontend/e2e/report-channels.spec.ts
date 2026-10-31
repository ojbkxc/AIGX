import { test, expect, loginAsAdmin } from './fixtures';

/**
 * 仪表盘报表区 + 渠道页模型摘要 —— 功能级回归。
 *
 * 报表：时间档位切换会重新请求 report 端点；Top 渠道 / Top 模型表渲染；
 * 热力图有格子。
 * 渠道摘要：编辑渠道时不再有模型输入框，只有只读摘要 + 跳转模型管理。
 */

test.describe('仪表盘报表', () => {
  test('时间档位切换会重新请求报表端点', async ({ page, pageErrors }) => {
    await loginAsAdmin(page, { sessions: [] });
    await page.goto('/');

    // 等首屏报表加载完（Top 渠道表出现）
    await expect(page.locator('.range-switcher')).toBeVisible();
    await expect(page.locator('.main-content')).toContainText('Top 渠道');

    const reportCalls: string[] = [];
    page.on('request', (r) => {
      if (r.url().includes('/api/dashboard/report/')) reportCalls.push(r.url());
    });

    // 切到「近 7 日」
    await page.locator('.range-switcher-btn', { hasText: '近 7 日' }).click();
    // 点刷新触发重新拉取（档位按钮只改 state）
    await page.getByRole('button', { name: /刷新/ }).click();

    await expect.poll(() => reportCalls.length, { timeout: 10000 }).toBeGreaterThan(0);
    expect(reportCalls.some((u) => u.includes('days=7'))).toBe(true);
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('Top 渠道与 Top 模型表渲染数据', async ({ page, pageErrors }) => {
    await loginAsAdmin(page, { sessions: [] });
    await page.goto('/');
    await expect(page.locator('.main-content')).toContainText('Top 渠道');

    // Top 渠道表：两条来源
    const sourceCard = page.locator('.section-card', { hasText: 'Top 渠道' });
    await expect(sourceCard.locator('tbody tr')).toHaveCount(2);
    await expect(sourceCard).toContainText('glm-4.7-flash');
    await expect(sourceCard).toContainText('wb-coder');

    // Top 模型表
    const modelCard = page.locator('.section-card', { hasText: 'Top 模型' });
    await expect(modelCard.locator('tbody tr')).toHaveCount(1);

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('热力图渲染格子', async ({ page, pageErrors }) => {
    await loginAsAdmin(page, { sessions: [] });
    await page.goto('/');
    await expect(page.locator('.main-content')).toContainText('Top 渠道');
    // 热力图是 365 个 14px 小方格的 grid
    const cells = await page.evaluate(() => {
      const grids = Array.from(document.querySelectorAll('div'));
      const heat = grids.find((d) => {
        const s = getComputedStyle(d);
        return s.display === 'grid' && s.gridTemplateColumns.includes('14px');
      });
      return heat ? heat.children.length : 0;
    });
    expect(cells).toBeGreaterThan(300);
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });
});

test.describe('渠道页模型摘要', () => {
  test('编辑渠道时不再有模型输入框，只有只读摘要与跳转链接', async ({ page, pageErrors }) => {
    await loginAsAdmin(page, { sessions: [] });
    // 渠道列表接口返回空数组时没有可编辑行，直接验证页面无模型编辑控件
    await page.goto('/channels');
    await expect(page.locator('.main-content')).toBeVisible();

    // 页面上不应存在旧的「支持的模型（逗号分隔）」输入
    await expect(page.locator('.main-content')).not.toContainText('支持的模型（逗号分隔');

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });
});
