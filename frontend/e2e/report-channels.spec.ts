import { test, expect, loginAsAdmin } from './fixtures';

/**
 * 仪表盘报表区 + 渠道页模型摘要 —— 功能级回归。
 *
 * 报表：时间档位切换会重新请求 report 端点；Top 供应商 / Top 模型表渲染；
 * 热力图有格子。
 * 渠道摘要：编辑渠道时不再有模型输入框，只有只读摘要 + 跳转模型管理。
 */

test.describe('仪表盘报表', () => {
  test('时间档位切换会重新请求报表端点', async ({ page, pageErrors }) => {
    await loginAsAdmin(page, { sessions: [] });
    await page.goto('/');

    // 等首屏报表加载完（Top 供应商表出现）
    await expect(page.locator('.range-switcher')).toBeVisible();
    await expect(page.locator('.main-content')).toContainText('Top 供应商');

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

  test('Top 供应商与 Top 模型表渲染数据（默认档位，行数即「前 N 名」档）', async ({ page, pageErrors }) => {
    await loginAsAdmin(page, { sessions: [] });
    await page.goto('/');
    await expect(page.locator('.main-content')).toContainText('Top 供应商');

    // Top 供应商表：桩给 10 行，**默认档位是「前 5 名」**（TopSources 初值 count=5）。
    // 这里断言 5 而不是「大于 0」：档位默认值一旦被改成「全部」，行数必须跟着变红
    // ——只断「有行」的话，档位控件坏了也照样绿。
    const sourceCard = page.locator('.section-card', { hasText: 'Top 供应商' });
    await expect(sourceCard.locator('tbody tr')).toHaveCount(5);
    await expect(sourceCard).toContainText('glm-4.7-flash');
    await expect(sourceCard).toContainText('wb-coder');
    // 第 6 行（kimi-k2）在「前 5 名」档下不该出现，切「全部」后才出现（见 report-enhance）
    await expect(sourceCard.locator('tbody tr', { hasText: 'kimi-k2' })).toHaveCount(0);

    // Top 模型表：桩给 12 行，该组件默认档位是「前 10 名」（TopModelsTable 初值 count=10）
    const modelCard = page.locator('.section-card', { hasText: 'Top 模型' });
    await expect(modelCard.locator('tbody tr')).toHaveCount(10);

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('热力图渲染格子', async ({ page, pageErrors }) => {
    await loginAsAdmin(page, { sessions: [] });
    await page.goto('/');
    await expect(page.locator('.main-content')).toContainText('Top 供应商');
    // 热力图（GitHub 贡献图布局）：一天一个 svg rect，stub 给满 365 天
    const heatCard = page.locator('.section-card', { hasText: '消费热力图' });
    await expect(heatCard.locator('svg')).toBeVisible();
    const cells = await heatCard.locator('svg rect').count();
    expect(cells).toBeGreaterThan(300);
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });
});

test.describe('渠道页模型摘要', () => {
  test('编辑渠道时不再有模型输入框，只有只读摘要与跳转链接', async ({ page, pageErrors }) => {
    await loginAsAdmin(page, { sessions: [] });
    // 旧版这条是「渠道列表为空 ⇒ 页面上不该出现某段文案」——空页面上任何
    // `not.toContainText` 都恒成立，等于没验。现在真打开一条渠道的编辑弹窗：
    // 桩 ch-1 有 2 个已配置模型，摘要必须报数并列出 chip，链接必须带 channel_id。
    await page.goto('/channels');
    const row = page.locator('tbody tr', { hasText: 'lxchat-glm' }).first();
    await expect(row).toBeVisible();
    // 行菜单用 fixed 挂在 body 上，并在 `window` 的 scroll(capture) 上自我关闭：
    // Playwright 的 `locator.click()` 会先做「滚动到可见」，那次滚动会把刚打开的
    // 菜单立刻收掉（实测点完 count=0，面板从未留在 DOM 里）。这里用真实的
    // move → down → up 鼠标序（不触发滚动），菜单才留得住 —— 这也是用户手上的顺序。
    const moreBtn = row.locator('button[title="更多操作"]');
    await moreBtn.scrollIntoViewIfNeeded();
    const box = await moreBtn.boundingBox();
    expect(box).not.toBeNull();
    await page.mouse.move(box!.x + box!.width / 2, box!.y + box!.height / 2);
    await page.mouse.down();
    await page.mouse.up();
    const menu = page.locator('.ch-row-menu-panel');
    await expect(menu).toBeVisible();
    await menu.getByRole('button', { name: '编辑供应商' }).click();

    const modal = page.locator('.modal');
    await expect(modal).toBeVisible();
    // 摘要区：计数 + 模型 chip
    const summary = modal.locator('.ch-model-summary');
    await expect(summary).toBeVisible();
    await expect(summary.locator('.ch-model-summary-counts')).toContainText('已配置 2 个模型');
    await expect(summary.locator('.ch-model-badge')).toHaveText(['glm-4.7-flash', 'glm-4-plus']);
    // 跳转入口指向模型管理并带 channel_id（模型清单的维护点唯一）
    await expect(summary.locator('a.btn')).toHaveAttribute('href', /\/model-manage\?channel_id=ch-1$/);
    // 逗号分隔的模型输入框确已下线（旧控件）
    await expect(modal.locator('input[placeholder*="逗号"]')).toHaveCount(0);
    await expect(modal).not.toContainText('支持的模型（逗号分隔');

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });
});
