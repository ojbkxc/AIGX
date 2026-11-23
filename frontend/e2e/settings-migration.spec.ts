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

  test('旧 /settings?tab=notify 不再是通知入口（分类已删，重定向或回退通用）', async ({ page, pageErrors }) => {
    // 通知分类删除后，settings 左栏不应再含「通知」项
    await page.goto('/settings');
    await expect(page.locator('.settings-layout')).toBeVisible();
    await expect(page.locator('.settings-nav')).not.toContainText('通知');
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });
});
