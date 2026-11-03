import { test, expect, loginAsAdmin, makeSessions } from './fixtures';
import type { Page } from '@playwright/test';

/**
 * 全站冒烟：核心页面能渲染、不报错、不白屏。
 *
 * 目的不是覆盖业务细节，而是兜住「路由/懒加载/守卫/接口解包」层面的
 * 整体性回归 —— 这类问题往往一改就整页白屏，且只在真实浏览器里暴露。
 */

/** 需要管理员权限的页面（AdminRoute 包裹） */
const ADMIN_PAGES: Array<[path: string, marker: string]> = [
  ['/channels', '渠道'],
  ['/users', '用户'],
  ['/settings', '设置'],
  ['/pricing', '定价'],
  ['/plans', '套餐'],
  ['/redemptions', '兑换'],
  ['/account-sessions', '账号'],
  ['/proxy-pool', '代理'],
  ['/scheduled-tasks', '定时'],
];

/** 普通登录即可访问的页面 */
const USER_PAGES: Array<[path: string, marker: string]> = [
  ['/', '仪表盘'],
  ['/keys', '密钥'],
  ['/wallet', '钱包'],
  ['/logs', '日志'],
  ['/chat', '聊天'],
  ['/profile', '个人'],
  ['/tickets', '工单'],
];

/** 页面主体已渲染（不是加载骨架、也不是空白） */
async function expectRendered(page: Page): Promise<void> {
  await expect(page.locator('.main-content')).toBeVisible();
  // 有内容：出现标题或任意交互控件
  const hasContent = await page.evaluate(() => {
    const main = document.querySelector('.main-content');
    if (!main) return false;
    const text = (main.textContent || '').trim();
    return text.length > 10 || main.querySelectorAll('button, input, table').length > 0;
  });
  expect(hasContent).toBe(true);
}

test.describe('全站冒烟', () => {
  test('登录页可渲染且表单可用', async ({ page, pageErrors }) => {
    await page.goto('/login');
    await expect(page.locator('input#email')).toBeVisible();
    await expect(page.locator('input#password')).toBeVisible();
    await expect(page.locator('button[type="submit"]')).toBeVisible();
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  for (const [path, marker] of USER_PAGES) {
    test(`页面可渲染：${path}`, async ({ page, pageErrors }) => {
      await loginAsAdmin(page, { sessions: makeSessions(1) });
      await page.goto(path);
      await expectRendered(page);
      // 导航保持可用：桌面端侧栏常驻；移动端侧栏收进抽屉（由汉堡按钮唤出）。
      // 注意页面上有**两个** `.sidebar-aside`（桌面常驻 + 抽屉内各一），
      // 窄屏下前者被 CSS 隐藏，不能只取 first() 判可见性。
      const sidebars = page.locator('.sidebar-aside');
      await expect(sidebars.first()).toHaveCount(1);
      const isMobile = (page.viewportSize()?.width ?? 1440) <= 768;
      if (isMobile) {
        await page.locator('.mobile-menu-btn').click();
      }
      // 任一实例可见即说明导航结构在且可用
      await expect(sidebars.filter({ visible: true }).first()).toBeVisible({ timeout: 5000 });
      expect(pageErrors.join('\n')).not.toMatch(/ErrorBoundary|page-fade/);
      expect(pageErrors, `${path} 控制台报错：\n${pageErrors.join('\n')}`).toEqual([]);
      void marker;
    });
  }

  for (const [path] of ADMIN_PAGES) {
    test(`管理页可渲染：${path}`, async ({ page, pageErrors }) => {
      await loginAsAdmin(page, { sessions: makeSessions(1) });
      await page.goto(path);
      await expectRendered(page);
      // 非管理员不应看到 PermissionDenied（我们是 admin，应看到真实页面）
      await expect(page.locator('.permission-denied')).toHaveCount(0);
      expect(pageErrors, `${path} 控制台报错：\n${pageErrors.join('\n')}`).toEqual([]);
    });
  }

  test('未登录访问受保护页面被重定向到登录页', async ({ page }) => {
    await page.goto('/channels');
    await expect(page).toHaveURL(/\/login$/);
    await expect(page.locator('input#email')).toBeVisible();
  });

  test('首登弱口令标记触发强制改密，且无法用侧边栏绕过', async ({ page, pageErrors }) => {
    await loginAsAdmin(page, { mustChangePassword: true });
    await page.goto('/');
    // 路由级拦截：任何非 /profile 页面都应被送回改密页
    await expect(page).toHaveURL(/\/profile\?force_password=1/);

    // 直接改地址到别的页面，同样被拦
    await page.goto('/channels');
    await expect(page).toHaveURL(/\/profile\?force_password=1/);
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('未知路由显示 404 页而非白屏', async ({ page, pageErrors }) => {
    await loginAsAdmin(page, { sessions: makeSessions(1) });
    await page.goto('/this-route-does-not-exist');
    // 404 是 catch-all 路由，不走 ProtectedLayout（因此没有 .main-content /
    // 侧边栏），由 RouteErrorPage 独立成页 —— 断言它自己的容器即可。
    await expect(page.locator('.route-error-page')).toBeVisible();
    await expect(page.locator('.route-error-page')).toContainText('页面不存在');
    const body = await page.locator('body').innerText();
    expect(body.length).toBeGreaterThan(10);
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('侧边栏导航可切换页面', async ({ page, pageErrors }) => {
    await loginAsAdmin(page, { sessions: makeSessions(1) });
    await page.goto('/');
    await expect(page.locator('.main-content')).toBeVisible();

    // 移动端侧栏在抽屉里，先唤出再点
    if ((page.viewportSize()?.width ?? 1440) <= 768) {
      await page.locator('.mobile-menu-btn').click();
    }
    await page
      .locator('.sidebar-aside')
      .filter({ visible: true })
      .first()
      .getByText('渠道管理', { exact: false })
      .click();
    await expect(page).toHaveURL(/\/channels$/);
    await expectRendered(page);
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });
});
