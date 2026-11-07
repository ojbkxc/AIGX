import { test, expect, loginAsAdmin } from './fixtures';

/**
 * 插件管理页回归（契约对齐 Agent2API api/plugins.rs）。
 *
 * 桩（static-server.mjs /api/plugins）：一个已装包（两家提供商，一加载一未加载）
 * + 目录商店两条（其中一条是已装包自己的版本，只显示未装的另一条）。
 * 覆盖：已装卡渲染（徽章/提供商计数/宿主态）、提供商设置展开、整包启停开关
 * 打到 /api/plugins/enable、卸载二次确认、商店卡「暂不支持在线安装」、诊断折叠区。
 */

test.describe('插件管理页', () => {
  test.beforeEach(async ({ page }) => {
    await loginAsAdmin(page);
    // 恢复插件桩预置态（启停/卸载是写操作，跨用例残留隔离）
    await page.request.post('/api/__e2e/reset-plugins');
    await page.goto('/plugins');
    await expect(page.locator('h1')).toContainText('插件管理');
  });

  test('已装包卡片渲染：徽章 / 提供商计数 / 宿主运行态', async ({ page, pageErrors }) => {
    const card = page.locator('.pl-card.is-installed');
    await expect(card).toHaveCount(1);
    await expect(card).toContainText('提供商增强包');
    await expect(card).toContainText('v1.2.0');
    await expect(card.locator('.pl-card-badges', { hasText: '已启用' })).toBeVisible();
    await expect(card.locator('.pl-card-badges', { hasText: '已签名' })).toBeVisible();
    await expect(card).toContainText('2 家提供商');
    // 宿主运行态 + 已加载 1 家
    await expect(card.locator('.pl-card-state')).toContainText('运行中');
    await expect(card.locator('.pl-card-state')).toContainText('已加载 1 家');
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('提供商设置展开：单家启停开关 + 加载态圆点', async ({ page, pageErrors }) => {
    const card = page.locator('.pl-card.is-installed');
    await card.locator('button', { hasText: /提供商设置/ }).click();
    const rows = page.locator('.pl-provider-row');
    await expect(rows).toHaveCount(2);
    await expect(rows.filter({ hasText: 'MonkeyCode' })).toBeVisible();
    await expect(rows.filter({ hasText: 'MonkeyCode' }).locator('.pl-provider-dot.on')).toBeVisible();
    await expect(rows.filter({ hasText: 'OrcaTerm' }).locator('.pl-provider-dot:not(.on)')).toBeVisible();
    // 每行有启停开关
    await expect(rows.first().getByRole('switch')).toBeVisible();
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('整包启停开关打到 /api/plugins/enable', async ({ page, pageErrors }) => {
    const enableCalls: string[] = [];
    page.on('request', (r) => {
      if (r.url().includes('/api/plugins/enable')) enableCalls.push(r.method());
    });
    const card = page.locator('.pl-card.is-installed');
    await card.getByRole('switch').click();
    await expect.poll(() => enableCalls.length, { timeout: 5000 }).toBeGreaterThan(0);
    expect(enableCalls[0]).toBe('POST');
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('卸载走二次确认，确认后请求打到 /api/plugins/uninstall', async ({ page, pageErrors }) => {
    const uninstallCalls: string[] = [];
    page.on('request', (r) => {
      if (r.url().includes('/api/plugins/uninstall')) uninstallCalls.push(r.method());
    });
    const card = page.locator('.pl-card.is-installed');
    await card.locator('button', { hasText: '卸载' }).click();

    // 确认弹窗（ConfirmDialog）：说明文案含「账号数据」
    const dialog = page.locator('.modal-overlay').last();
    await expect(dialog).toContainText('卸载插件包');
    await expect(dialog).toContainText('账号数据本身不会被删除');
    // 未确认前不打请求
    expect(uninstallCalls.length).toBe(0);
    await dialog.locator('button', { hasText: '卸载', exact: true }).last().click();
    await expect.poll(() => uninstallCalls.length, { timeout: 5000 }).toBe(1);
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('商店条目：未装的包显示卡片并标注暂不支持在线安装', async ({ page, pageErrors }) => {
    // 桩目录两条，其中 aigx-provider-pack 已装 → 只显示 aigx-web-pack
    const storeCard = page.locator('.pl-card.is-store');
    await expect(storeCard).toHaveCount(1);
    await expect(storeCard).toContainText('网页提供商包');
    await expect(storeCard.locator('.pl-store-hint')).toContainText('暂不支持在线安装');
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('诊断信息折叠区：目录 / 平台 / 协议版本', async ({ page, pageErrors }) => {
    const diag = page.locator('.pl-diagnostics');
    await expect(diag).toBeVisible();
    // details 默认收起，展开后可见目录与平台
    await diag.locator('summary').click();
    await expect(diag).toContainText('插件目录');
    await expect(diag).toContainText('/data/aigx/plugins');
    await expect(diag).toContainText('x86_64-unknown-linux-gnu');
    await expect(diag).toContainText('协议版本');
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });
});
