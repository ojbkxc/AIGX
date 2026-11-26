import { test, expect, loginAsAdmin } from './fixtures';
import type { Page } from '@playwright/test';

/**
 * 侧栏分组「组内计数 + 隐藏组 + 恢复」（`2c96e573` 的第 5 条）。
 *
 * 这是本轮新增的功能面，此前零 e2e。三条判据必须一起验，少一条就等于没验：
 *  1. 隐藏真的把整组从 DOM 移除（不是 `display:none` 的假隐藏 —— 后者会让
 *     键盘 Tab 仍能走到隐藏的链接）；
 *  2. 隐藏后必须留下「已隐藏 N 组 · 恢复」这条回来的路（否则用户以为菜单没了）；
 *  3. 隐藏的是**组 key** 且落 localStorage：刷新后仍然隐藏（下标方案会在菜单
 *     增删时静默落到别的组上）。
 */

/** 当前可见侧栏（桌面常驻 / 窄屏先唤出抽屉） */
async function openSidebar(page: Page): Promise<void> {
  const isMobile = (page.viewportSize()?.width ?? 1440) <= 768;
  if (isMobile) {
    await page.locator('.mobile-menu-btn').click();
  }
  await expect(page.locator('.sidebar-aside').filter({ visible: true }).first()).toBeVisible();
}

const HIDDEN_KEY = 'aigx-sidebar-hidden-groups';

function hiddenStored(page: Page): Promise<string | null> {
  return page.evaluate((k) => localStorage.getItem(k), HIDDEN_KEY);
}

test.describe('侧栏分组隐藏', () => {
  test('隐藏「管理」组：整组离开 DOM、计数提示出现、恢复后原样回来', async ({ page, pageErrors }) => {
    await loginAsAdmin(page, { sessions: [] });
    await page.goto('/');
    await openSidebar(page);

    // 结构断言一律打在**桌面那一份**侧栏上（`.sidebar-desktop .sidebar-aside`）：
    // 它始终挂载，与抽屉共用同一份 hiddenGroups 状态（见 Sidebar.tsx 的注释），
    // 因此 `toHaveCount` 这类结构判据在窄屏下同样成立 —— 反而比"可见实例"更准，
    // 因为窄屏抽屉会随点击收起/重挂，抓住它断言读到的是动画中间态。
    // 只有「点按钮」这一动作用可见实例（窄屏 = 抽屉里那份）。
    const desktopSide = page.locator('.sidebar-desktop .sidebar-aside');
    const clickableSide = () => page.locator('.sidebar-aside').filter({ visible: true }).first();
    const isMobile = (page.viewportSize()?.width ?? 1440) <= 768;

    // 前置：三组都在，管理组里有 /channels
    await expect(desktopSide.locator('.sidebar-group')).toHaveCount(3);
    await expect(desktopSide.locator('.nav-item[href="/channels"]')).toHaveCount(1);
    // 组内计数：管理组 14 项（本轮新增的「{{n}} 项」）
    const adminGroup = desktopSide
      .locator('.sidebar-group')
      .filter({ has: page.locator('.sidebar-group-label', { hasText: /^管理$/ }) });
    await expect(adminGroup.locator('.sidebar-group-count')).toHaveText('14');

    // 点「隐藏「管理」分组」（按 aria-label 定位：窄屏抽屉里 getByRole 解析不到
    // 这个名字，属可访问名称计算问题，另行登记；这里不靠 role）
    await clickableSide().locator('.sidebar-group-hide[aria-label="隐藏「管理」分组"]').click();

    // 整组**离开 DOM**：组数减一、组内链接从 DOM 消失（不是 display:none，见文件头）
    await expect(desktopSide.locator('.sidebar-group')).toHaveCount(2);
    await expect(desktopSide.locator('.nav-item[href="/channels"]')).toHaveCount(0);
    await expect(desktopSide.locator('.sidebar-group-label', { hasText: /^管理$/ })).toHaveCount(0);

    // 回来的一条路：底部常驻「已隐藏 1 组 · 恢复」
    await expect(desktopSide.locator('.sidebar-restore-groups')).toHaveText(/已隐藏 1 组 · 恢复/);

    // 隐藏状态落的是**组 key**（不是下标）
    expect(await hiddenStored(page)).toBe(JSON.stringify(['admin']));

    // 刷新仍然隐藏（跨会话记住）
    await page.reload();
    await expect(desktopSide.locator('.sidebar-group')).toHaveCount(2);
    await expect(desktopSide.locator('.sidebar-restore-groups')).toHaveText(/已隐藏 1 组/);

    // 恢复：三组都回来，提示条消失，存储清空（恢复条在窄屏抽屉里要可见才点得到）
    await openSidebar(page);
    await clickableSide().locator('.sidebar-restore-groups').click();
    await expect(desktopSide.locator('.sidebar-group')).toHaveCount(3);
    await expect(desktopSide.locator('.nav-item[href="/channels"]')).toHaveCount(1);
    await expect(desktopSide.locator('.sidebar-restore-groups')).toHaveCount(0);
    expect(await hiddenStored(page)).toBe(JSON.stringify([]));
    // 桌面档再补一条「真的能看见」：窄屏下桌面侧栏本身被 CSS 收起，可见性无意义
    if (!isMobile) {
      await expect(desktopSide.locator('.nav-item[href="/channels"]')).toBeVisible();
    }

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('残留的未知组 key 不占位（菜单下线后旧存盘不该继续生效）', async ({ page, pageErrors }) => {
    // 存一个已经不存在的组 key：readHiddenGroups 只收仍在 navGroups 里的 key
    await page.addInitScript((k) => {
      localStorage.setItem(k, JSON.stringify(['admin', 'ghost-group']));
    }, HIDDEN_KEY);
    await loginAsAdmin(page, { sessions: [] });
    await page.goto('/');
    await openSidebar(page);

    const side = page.locator('.sidebar-aside').filter({ visible: true }).first();
    // 只隐藏了 1 组（ghost 被丢弃），不是 2
    await expect(side.locator('.sidebar-group')).toHaveCount(2);
    await expect(side.locator('.sidebar-restore-groups')).toHaveText(/已隐藏 1 组/);

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('普通用户看不到管理组，隐藏计数也不该把它算进来', async ({ page, pageErrors }) => {
    // 普通用户视角：只注入角色=user 的登录态
    await page.addInitScript((k) => {
      localStorage.setItem('token', 'e2e-token');
      localStorage.setItem('expires_at', String(Date.now() + 24 * 3600 * 1000));
      localStorage.setItem('email', 'user@aigx.local');
      localStorage.setItem('username', 'user');
      localStorage.setItem('role', 'user');
      localStorage.setItem('i18n_lang', 'zh');
      localStorage.setItem(k, JSON.stringify(['admin']));
    }, HIDDEN_KEY);
    await page.goto('/');
    await openSidebar(page);

    const side = page.locator('.sidebar-aside').filter({ visible: true }).first();
    // 管理组本来就不可见，因此「已隐藏 N 组」不该冒出来（否则是个点不动的计数）
    await expect(side.locator('.sidebar-restore-groups')).toHaveCount(0);
    await expect(side.locator('.sidebar-group')).toHaveCount(2);

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });
});
