import { test, expect, loginAsAdmin } from './fixtures';

/**
 * 顶级页迁移冒烟（v1.0.93 设置菜单迁移）。
 *
 * 迁移口径（docs/ROUTING-MENU-PARITY.md §9）：
 * - 通知：从「系统设置」迁出为顶级页 /notify（侧栏「通知」菜单项）
 * - 其余迁出目标（/limit、/products 计费设置 Tab、仪表盘报表设置 Tab、
 *   /chat 聊天设置、/agent 助手设置抽屉）由源码迁移完成后在此追加冒烟。
 *
 * 本文件只做「入口接线」级断言：侧栏项存在、直达 URL 渲染出对应面板骨架。
 * 面板内部功能交互由各自既有 spec 覆盖（Notify 面板原在设置页时已有覆盖路径，
 * 迁移只换挂载点不换实现，此处不重复断言表单细节）。
 */

test.describe('设置迁移 · 顶级页冒烟', () => {
  test.beforeEach(async ({ page }) => {
    await loginAsAdmin(page, { sessions: [] });
  });

  test('通知已独立成顶级页：侧栏有「通知」项，/notify 渲染通知配置', async ({ page, pageErrors }) => {
    await page.goto('/');
    await expect(page.locator('.main-content')).toBeVisible();

    // 侧栏出现「通知」菜单项（adminOnly）；mobile 侧栏收在抽屉里，默认不可见，故仅桌面断言
    if (test.info().project.name !== 'mobile') {
      await expect(page.getByRole('link', { name: '通知', exact: true })).toBeVisible();
    }

    // 直达顶级页：渲染通知配置骨架（总开关 + Telegram 卡）
    await page.goto('/notify');
    await expect(page.locator('.notify-shell')).toBeVisible();
    await expect(page.locator('.notify-shell h2', { hasText: '通知配置' })).toBeVisible();
    await expect(page.locator('.notify-shell')).toContainText('启用通知');
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('旧 /settings?tab=notify 不再是通知入口（旧键失效回退通用，通知分类只是跳板）', async ({ page, pageErrors }) => {
    // A2A v3.5.1 把 notifications 恢复成合法设置分类，但**键名是 notifications**，
    // 旧书签用的 notify 不在 VALID_SETTINGS_TABS（Settings.tsx:158）里 →
    // 手改 URL 不会把设置页置空，也不该落到通知面，而是回退「通用」。
    await page.goto('/settings?tab=notify');
    await expect(page.locator('.settings-layout')).toBeVisible();
    await expect(page.locator('.settings-nav-item.active')).toHaveText('通用');
    await expect(page.locator('.settings-pane')).toContainText('使用限额');

    // 「通知」分类确实回来了（12 分类之一），但它只是跳板：不承载通知表单，
    // 真实配置面在顶级 /notify（.notify-shell 只在那边挂载）。
    const notifyItem = page.locator('.settings-nav-item', { hasText: '通知' });
    await expect(notifyItem).toHaveCount(1);
    await notifyItem.click();
    await expect(page.locator('.settings-nav-item.active')).toHaveText('通知');
    const pane = page.locator('.settings-pane');
    await expect(pane.locator('h2')).toHaveText('通知');
    // 通知表单本体（总开关）不在设置页内
    await expect(pane).not.toContainText('启用通知');
    await expect(page.locator('.notify-shell')).toHaveCount(0);

    // 跳板入口可达：点「前往通知设置」落到顶级 /notify
    await pane.getByRole('button', { name: '前往通知设置' }).click();
    await expect(page).toHaveURL(/\/notify$/);
    await expect(page.locator('.notify-shell')).toBeVisible();
    await expect(page.locator('.notify-shell h2', { hasText: '通知配置' })).toBeVisible();

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });
});
