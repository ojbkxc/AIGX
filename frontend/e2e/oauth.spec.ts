import { test, expect, loginAsAdmin } from './fixtures';

/**
 * 个人中心「第三方账号」绑定 + OAuth 登录回跳 ticket 消费。
 *
 * 覆盖本轮修复：
 * - OAuth 登录 callback 改为重定向回 SPA（#oauth_ticket），不再把 JSON 甩给浏览器；
 *   登录页 mount 时消费 ticket 完成登录。
 * - 个人中心新增第三方账号卡片：已绑可解绑、未绑可发起绑定（整页跳授权 URL）。
 *
 * 所有后端交互都用 page.route 本地桩化 —— mock server 是进程内全局态，
 * 满套件并发下单线程 server 会被其它 spec 拖慢/串扰。自包含桩让本用例与
 * 共享状态、worker 数、执行顺序全部解耦，稳定可重复。
 */

type Bindings = { oauth_github: boolean; oauth_google: boolean; oauth_linuxdo: boolean };

/** 每个用例独立的“已绑定”态，随解绑变化；闭包持有，不碰共享 server。 */
function stubOAuth(page: import('@playwright/test').Page, initial: Bindings): Bindings {
  const state = { ...initial };
  const meBody = {
    id: 'u-admin', email: 'admin@aigx.local', username: 'admin', role: 'admin',
    status: 'active', quota: null, used_quota: 0, password_set: true,
  };
  void page.route('**/api/users/me', (route) =>
    route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({ success: true, data: { ...meBody, ...state } }),
    }),
  );
  return state;
}

const SEED: Bindings = { oauth_github: true, oauth_google: false, oauth_linuxdo: false };

test.describe('OAuth 登录与绑定', () => {
  test('个人中心展示第三方账号卡片，GitHub 已绑定', async ({ page, pageErrors }) => {
    await loginAsAdmin(page);
    const state = stubOAuth(page, SEED);
    void state;
    await page.goto('/profile');
    await expect(page.locator('h1')).toContainText('个人中心');
    const card = page.locator('.card', { hasText: '第三方账号' }).first();
    await expect(card).toBeVisible();
    for (const name of ['GitHub', 'Google', 'LinuxDO']) {
      await expect(card.getByText(name, { exact: true })).toBeVisible();
    }
    // 绑定态（exact 排除提示语里的「已绑定」子串）：GitHub=已绑 / 其余=未绑
    await expect(card.getByText('已绑定', { exact: true })).toHaveCount(1);
    await expect(card.getByText('未绑定', { exact: true })).toHaveCount(2);
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('未绑定项提供可用绑定入口，bind/start 返回授权 URL', async ({ page }) => {
    await loginAsAdmin(page);
    stubOAuth(page, SEED);
    await page.goto('/profile');
    const card = page.locator('.card', { hasText: '第三方账号' }).first();
    // 未绑定项渲染出「绑定」按钮且可用（Google/LinuxDO 各一）
    const bindBtns = card.getByRole('button', { name: '绑定' });
    await expect(bindBtns).toHaveCount(2);
    await expect(bindBtns.first()).toBeEnabled();
    // 端点契约：POST bind/start 返回可用于整页跳转的第三方授权 URL
    const resp = await page.request.post('/api/auth/google/bind/start', { data: {} });
    expect(resp.ok()).toBeTruthy();
    const body = await resp.json();
    expect(body.success).toBe(true);
    expect(String(body?.data?.url || '')).toMatch(/oauth\/authorize|github\.com|connect\.linux\.do|accounts\.google/);
  });

  test('已绑定项可解绑，解绑后转为未绑定', async ({ page, pageErrors }) => {
    await loginAsAdmin(page);
    const state = stubOAuth(page, SEED);
    // 解绑桩：把对应 provider 的布尔位翻 false，后续 /api/users/me 即反映新态
    await page.route('**/api/auth/oauth/unbind', (route) => {
      const body = JSON.parse(route.request().postData() || '{}');
      if (body.provider === 'github') state.oauth_github = false;
      if (body.provider === 'google') state.oauth_google = false;
      if (body.provider === 'linuxdo') state.oauth_linuxdo = false;
      return route.fulfill({
        status: 200, contentType: 'application/json',
        body: JSON.stringify({ success: true, data: { provider: body.provider } }),
      });
    });
    await page.goto('/profile');
    const card = page.locator('.card', { hasText: '第三方账号' }).first();
    await expect(card.getByText('已绑定', { exact: true })).toHaveCount(1);
    await card.getByRole('button', { name: '解绑' }).click();
    await expect(card.getByText('已绑定', { exact: true })).toHaveCount(0, { timeout: 5000 });
    await expect(card.getByText('未绑定', { exact: true })).toHaveCount(3);
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('登录页消费 #oauth_ticket 完成登录并跳首页', async ({ page, pageErrors }) => {
    await page.route('**/api/auth/oauth/consume', (route) =>
      route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify({
          success: true,
          data: {
            token: 'e2e-oauth-token', email: 'admin@aigx.local', username: 'admin',
            role: 'admin', expires_at: Math.floor(Date.now() / 1000) + 86400, must_change_password: false,
          },
        }),
      }),
    );
    await page.goto('/login#oauth_ticket=e2e-ticket');
    await expect(page).toHaveURL(/\/$/, { timeout: 8000 });
    await expect(page.locator('.main-content')).toBeVisible();
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('登录页对 OAuth 错误码给出可读提示', async ({ page }) => {
    await page.goto('/login#oauth_error=need_bind');
    await expect(page.locator('.error-message')).toContainText('个人中心');
    // fragment 已清理，刷新不会重复提示
    await expect.poll(() => page.evaluate(() => window.location.hash), { timeout: 5000 }).toBe('');
  });
});
