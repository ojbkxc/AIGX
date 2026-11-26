import { test, expect, loginAsAdmin } from './fixtures';
import type { Page } from '@playwright/test';

/**
 * 侧栏布局：整组隐藏 / 恢复（服务端权威副本 + localStorage 首帧缓存）。
 *
 * 数据模型见 `src/lib/sidebar-layout.ts`：
 *   `{ groups: [{ id, label, custom, hidden, items: [menuKey] }], hidden: [menuKey] }`
 * 权威副本在 `GET/PUT /api/sidebar-layout`，localStorage `aigx-sidebar-layout-v1`
 * 是首帧缓存。E2E 里 `/api/sidebar-layout` 落到静态服务器默认桩（无 `layout`
 * 字段）⇒ 前端沿用缓存，因此断言打缓存即可。
 *
 * 判据：
 *  1. 隐藏**整组**后该组离开 DOM（不是 `display:none` 假隐藏）；
 *  2. 底部出现「已隐藏 N 项 · 恢复」这条回来的路；
 *  3. 含固定项（系统设置）的分组**不渲染**隐藏按钮 —— 否则点了没反应；
 *  4. 存盘里残留的未知菜单键在规整时被丢弃，不占位、不计数。
 */

/** 当前可见侧栏（桌面常驻 / 窄屏先唤出抽屉） */
async function openSidebar(page: Page): Promise<void> {
  const isMobile = (page.viewportSize()?.width ?? 1440) <= 768;
  if (isMobile) {
    await page.locator('.mobile-menu-btn').click();
  }
  await expect(page.locator('.sidebar-aside').filter({ visible: true }).first()).toBeVisible();
}

const LAYOUT_KEY = 'aigx-sidebar-layout-v1';

interface StoredGroup {
  id: string;
  hidden: boolean;
  items: string[];
}
interface StoredLayout {
  groups: StoredGroup[];
  hidden: string[];
}

function layoutStored(page: Page): Promise<StoredLayout | null> {
  return page.evaluate((k) => {
    const text = localStorage.getItem(k);
    return text ? (JSON.parse(text) as StoredLayout) : null;
  }, LAYOUT_KEY);
}

test.describe('侧栏布局隐藏', () => {
  test('隐藏「通用」组：整组离开 DOM、计数提示出现、恢复后原样回来', async ({ page, pageErrors }) => {
    await loginAsAdmin(page, { sessions: [] });
    await page.goto('/');
    await openSidebar(page);

    // 结构断言打在**桌面那一份**侧栏上（`.sidebar-desktop .sidebar-aside`）：
    // 它始终挂载，与抽屉共用同一份布局状态，窄屏下同样成立。
    const desktopSide = page.locator('.sidebar-desktop .sidebar-aside');
    const clickableSide = () => page.locator('.sidebar-aside').filter({ visible: true }).first();
    const isMobile = (page.viewportSize()?.width ?? 1440) <= 768;

    // 前置：三组都在（通用 / 管理 / 个人）
    await expect(desktopSide.locator('.sidebar-group')).toHaveCount(3);
    await expect(desktopSide.locator('.nav-item[href="/chat"]')).toHaveCount(1);

    // 「通用」组不含固定项 ⇒ 有隐藏按钮
    const generalGroup = desktopSide
      .locator('.sidebar-group')
      .filter({ has: page.locator('.sidebar-group-label', { hasText: /^通用$/ }) });
    await expect(generalGroup.locator('.sidebar-group-count')).toHaveText('5');

    await clickableSide().locator('.sidebar-group-hide[aria-label="隐藏「通用」分组"]').click();

    // 整组离开 DOM：组数减一、组内链接消失
    await expect(desktopSide.locator('.sidebar-group')).toHaveCount(2);
    await expect(desktopSide.locator('.nav-item[href="/chat"]')).toHaveCount(0);
    await expect(desktopSide.locator('.sidebar-group-label', { hasText: /^通用$/ })).toHaveCount(0);

    // 回来的路：底部常驻「已隐藏 1 项 · 恢复」
    await expect(desktopSide.locator('.sidebar-restore-groups')).toHaveText(/已隐藏 1 项 · 恢复/);

    // 落盘的是组隐藏位（g-general.hidden = true）
    const stored = await layoutStored(page);
    expect(stored?.groups.find((g) => g.id === 'g-general')?.hidden).toBe(true);

    // 刷新仍然隐藏（读缓存，服务端桩无布局不覆盖）
    await page.reload();
    await expect(desktopSide.locator('.sidebar-group')).toHaveCount(2);
    await expect(desktopSide.locator('.sidebar-restore-groups')).toHaveText(/已隐藏 1 项/);

    // 恢复：三组都回来，提示条消失，存储清空
    await openSidebar(page);
    await clickableSide().locator('.sidebar-restore-groups').click();
    await expect(desktopSide.locator('.sidebar-group')).toHaveCount(3);
    await expect(desktopSide.locator('.nav-item[href="/chat"]')).toHaveCount(1);
    await expect(desktopSide.locator('.sidebar-restore-groups')).toHaveCount(0);
    const restored = await layoutStored(page);
    expect(restored?.groups.find((g) => g.id === 'g-general')?.hidden).toBe(false);
    // 桌面档再补一条「真的能看见」：窄屏下桌面侧栏被 CSS 收起，可见性无意义
    if (!isMobile) {
      await expect(desktopSide.locator('.nav-item[href="/chat"]')).toBeVisible();
    }

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('含固定项的「管理」组不渲染隐藏按钮（点了没反应的路不给）', async ({ page, pageErrors }) => {
    await loginAsAdmin(page, { sessions: [] });
    await page.goto('/');
    await openSidebar(page);

    const side = page.locator('.sidebar-aside').filter({ visible: true }).first();
    // 管理组含固定项「系统设置」，不可整组隐藏 ⇒ 不应存在该组的隐藏按钮
    await expect(side.locator('.sidebar-group-hide[aria-label="隐藏「管理」分组"]')).toHaveCount(0);
    // 其他无固定项的组仍有按钮
    await expect(side.locator('.sidebar-group-hide[aria-label="隐藏「个人」分组"]')).toHaveCount(1);
    await expect(side.locator('.nav-item[href="/settings"]')).toHaveCount(1);

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('残留的未知菜单键被丢弃（菜单下线后旧存盘不该继续生效）', async ({ page, pageErrors }) => {
    // 存一个已不存在的菜单键：normalizeLayout 只收 SIDEBAR_MENUS 里还认得的 key
    await page.addInitScript((k) => {
      localStorage.setItem(
        k,
        JSON.stringify({
          groups: [
            { id: 'g-general', label: '通用', custom: false, hidden: false, items: ['/', '/chat', '/ghost-menu'] },
          ],
          hidden: ['/ghost-menu'],
        }),
      );
    }, LAYOUT_KEY);
    await loginAsAdmin(page, { sessions: [] });
    await page.goto('/');
    await openSidebar(page);

    const side = page.locator('.sidebar-aside').filter({ visible: true }).first();
    // ghost 不进渲染、也不计入隐藏数
    await expect(side.locator('.nav-item[href="/ghost-menu"]')).toHaveCount(0);
    await expect(side.locator('.sidebar-restore-groups')).toHaveCount(0);

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('普通用户看不到管理组，隐藏计数也不该把它算进来', async ({ page, pageErrors }) => {
    // 普通用户视角：只注入角色=user 的登录态，并预先「隐藏」管理组
    await page.addInitScript((k) => {
      localStorage.setItem('token', 'e2e-token');
      localStorage.setItem('expires_at', String(Date.now() + 24 * 3600 * 1000));
      localStorage.setItem('email', 'user@aigx.local');
      localStorage.setItem('username', 'user');
      localStorage.setItem('role', 'user');
      localStorage.setItem('i18n_lang', 'zh');
      localStorage.setItem(
        k,
        JSON.stringify({
          groups: [
            {
              id: 'g-general',
              label: '通用',
              custom: false,
              hidden: false,
              items: ['/', '/chat', '/prompts', '/keys', '/logs'],
            },
            {
              id: 'g-admin',
              label: '管理',
              custom: false,
              hidden: true,
              items: ['/model-manage', '/channels', '/users'],
            },
            {
              id: 'g-personal',
              label: '个人',
              custom: false,
              hidden: false,
              items: ['/wallet', '/tickets', '/profile'],
            },
          ],
          hidden: [],
        }),
      );
    }, LAYOUT_KEY);
    await page.goto('/');
    await openSidebar(page);

    const side = page.locator('.sidebar-aside').filter({ visible: true }).first();
    // 管理组本来就不可见，因此「已隐藏 N 项」不该冒出来（否则是个点不动的计数）
    await expect(side.locator('.sidebar-restore-groups')).toHaveCount(0);
    await expect(side.locator('.sidebar-group')).toHaveCount(2);
    await expect(side.locator('.nav-item[href="/channels"]')).toHaveCount(0);

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });
});
