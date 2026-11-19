import { test, expect, loginAsAdmin } from './fixtures';

/**
 * 插件管理页回归（A2A 双源形态）。
 *
 * 桩（static-server.mjs /api/plugins）：
 * - 一个已装包（两家提供商，一加载一未加载）；
 * - 目录商店两条（已装包自身 + 未装的 `aigx-web-pack`，含清单 providers）；
 * - `hiddenAccounts`：`web-glm` 的 2 个账号因插件未安装而隐藏；
 * - 脚本插件端点显式 404（本 spec 不覆盖 Magpie 面板）。
 *
 * 覆盖：已装卡渲染、提供商展开、整包启停、卸载二次确认、商店下载
 * （`install-from-release` 后台 Operation）、诊断信息、来源/状态筛选计数、
 * 搜索与清除筛选、缺失插件横幅与补全弹窗。
 */

test.describe('插件管理页', () => {
  test.beforeEach(async ({ page }) => {
    await loginAsAdmin(page);
    // 恢复插件桩预置态（启停/卸载/安装是写操作，跨用例残留隔离）
    await page.request.post('/api/__e2e/reset-plugins');
    await page.goto('/plugins');
    await expect(page.locator('h1')).toContainText('插件管理');
    // 首屏加载完成（已装包出现）
    await expect(page.locator('.pl-card.is-installed')).toHaveCount(1);
  });

  test('已装包卡片渲染：徽章 / 提供商计数 / 宿主运行态', async ({ page, pageErrors }) => {
    const card = page.locator('.pl-card.is-installed');
    await expect(card).toContainText('提供商增强包');
    await expect(card).toContainText('v1.2.0');
    await expect(card.locator('.pl-card-badges')).toContainText('已启用');
    await expect(card).toContainText('2 家提供商');
    // 宿主运行态 + 已加载 1 家
    await expect(card.locator('.pl-card-state')).toContainText('宿主运行中');
    await expect(card.locator('.pl-card-state')).toContainText('已加载 1 家');
    // 分区标题（双源结构的第一块）
    await expect(page.locator('.card-header h2').first()).toContainText('Agent2API 社区');
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('提供商设置展开：单家启停开关 + 加载态圆点', async ({ page, pageErrors }) => {
    const card = page.locator('.pl-card.is-installed');
    // 卡片上的「提供商（n）」折叠区
    await card.locator('button', { hasText: /^提供商/ }).click();
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
      if (new URL(r.url()).pathname === '/api/plugins/enable') enableCalls.push(r.method());
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
      if (new URL(r.url()).pathname === '/api/plugins/uninstall') uninstallCalls.push(r.method());
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

  test('商店条目：未装包显示下载，点击走 install-from-release（后台 Operation）', async ({ page, pageErrors }) => {
    const installCalls: string[] = [];
    page.on('request', (r) => {
      if (new URL(r.url()).pathname === '/api/plugins/install-from-release') installCalls.push(r.method());
    });
    const storeCard = page.locator('.pl-card.is-store');
    await expect(storeCard).toHaveCount(1);
    await expect(storeCard).toContainText('网页提供商包');
    const dl = storeCard.locator('button', { hasText: '下载' });
    await expect(dl).toBeEnabled();
    await dl.click();
    await expect.poll(() => installCalls.length, { timeout: 5000 }).toBe(1);
    expect(installCalls[0]).toBe('POST');
    await expect(page.locator('.toast-success')).toContainText('插件已下载', { timeout: 5000 });
    // 安装完成后商店卡消失（桩把目录条目摘掉）
    await expect(page.locator('.pl-card.is-store')).toHaveCount(0);
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('诊断信息：工具栏按钮展开诊断卡（目录 / 平台 / 协议版本）', async ({ page, pageErrors }) => {
    const tools = page.locator('.pl-toolbar-actions');
    await tools.getByRole('button', { name: '诊断信息' }).click();
    const diag = page.locator('.pl-diagnostics-body');
    await expect(diag).toBeVisible();
    await expect(diag).toContainText('插件目录');
    await expect(diag).toContainText('/data/aigx/plugins');
    await expect(diag).toContainText('x86_64-unknown-linux-gnu');
    await expect(diag).toContainText('协议版本');
    // 关闭后隐藏
    await page.locator('.card-header').filter({ hasText: '诊断信息' }).getByRole('button', { name: '关闭' }).click();
    await expect(diag).toHaveCount(0);
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('来源 / 状态筛选：带计数且驱动列表', async ({ page, pageErrors }) => {
    const sourceGroup = page.locator('.pl-filter-group').nth(0);
    const statusGroup = page.locator('.pl-filter-group').nth(1);

    // 计数：原生 = 已装 1 + 商店 1；脚本端点 404 → Magpie 0
    await expect(sourceGroup.getByRole('button', { name: /Agent2API 社区/ })).toContainText('2');
    await expect(sourceGroup.getByRole('button', { name: /Magpie 插件/ })).toContainText('0');
    await expect(statusGroup.getByRole('button', { name: /可更新/ })).toContainText('0');
    await expect(statusGroup.getByRole('button', { name: /已安装/ })).toContainText('1');
    await expect(statusGroup.getByRole('button', { name: /未安装/ })).toContainText('1');

    // 来源切到 Magpie：原生两块都不渲染
    await sourceGroup.getByRole('button', { name: /Magpie 插件/ }).click();
    await expect(page.locator('.pl-card.is-installed')).toHaveCount(0);
    await expect(page.locator('.pl-card.is-store')).toHaveCount(0);
    await sourceGroup.getByRole('button', { name: /Agent2API 社区/ }).click();
    await expect(page.locator('.pl-card.is-installed')).toHaveCount(1);

    // 状态切到「未安装」：只剩商店条目
    await statusGroup.getByRole('button', { name: /未安装/ }).click();
    await expect(page.locator('.pl-card.is-installed')).toHaveCount(0);
    await expect(page.locator('.pl-card.is-store')).toHaveCount(1);
    // 状态切到「已安装」：只剩已装卡
    await statusGroup.getByRole('button', { name: /已安装/ }).click();
    await expect(page.locator('.pl-card.is-installed')).toHaveCount(1);
    await expect(page.locator('.pl-card.is-store')).toHaveCount(0);
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('搜索与清除筛选', async ({ page, pageErrors }) => {
    const search = page.locator('.pl-search');
    await search.fill('网页');
    await expect(page.locator('.pl-card.is-store')).toHaveCount(1);
    await expect(page.locator('.pl-card.is-installed')).toHaveCount(0);

    await search.fill('zzzz-无此插件');
    await expect(page.locator('.pl-card.is-store')).toHaveCount(0);
    await expect(page.locator('.pl-card.is-installed')).toHaveCount(0);
    await expect(page.getByText('没有符合筛选条件的插件')).toBeVisible();

    await page.getByRole('button', { name: '清除筛选' }).click();
    await expect(page.locator('.pl-card.is-installed')).toHaveCount(1);
    await expect(page.locator('.pl-card.is-store')).toHaveCount(1);
    await expect(search).toHaveValue('');
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('缺失插件横幅与补全弹窗', async ({ page, pageErrors }) => {
    const banner = page.locator('.pl-missing-banner');
    await expect(banner).toBeVisible();
    await expect(banner).toContainText('1 个插件缺失');
    await expect(banner).toContainText('2 个账号暂不可用');

    await banner.getByRole('button', { name: '补全缺失插件' }).click();
    const dialog = page.locator('.ps-overlay[aria-label="补全缺失的插件"]');
    await expect(dialog).toBeVisible();
    await expect(dialog).toContainText('恢复现有账号');
    await expect(dialog).toContainText('网页提供商包');
    await expect(dialog).toContainText('2 个账号等待插件恢复');

    await dialog.getByRole('button', { name: '稍后' }).click();
    await expect(dialog).toHaveCount(0);
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });
});
