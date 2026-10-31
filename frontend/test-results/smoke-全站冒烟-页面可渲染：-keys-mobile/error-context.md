# Instructions

- Following Playwright test failed.
- Explain why, be concise, respect Playwright best practices.
- Provide a snippet of code with the fix, if possible.

# Test info

- Name: smoke.spec.ts >> 全站冒烟 >> 页面可渲染：/keys
- Location: e2e\smoke.spec.ts:56:5

# Error details

```
Error: expect(locator).toBeVisible() failed

Locator:  locator('.sidebar-aside').first()
Expected: visible
Received: hidden
Timeout:  5000ms

Call log:
  - Expect "toBeVisible" locator('.sidebar-aside').first() with timeout 5000ms
  - waiting for locator('.sidebar-aside').first()
    14 × locator resolved to <aside class="sidebar-aside  ">…</aside>
       - unexpected value "hidden"

```

```yaml
- button "打开菜单":
  - img
- main:
  - heading "API 密钥" [level=1]
  - paragraph: 创建与管理 API 密钥
  - heading "我的令牌 (0)" [level=2]
  - img
  - textbox "搜索令牌"
  - button "查看全部用户"
  - button "+ 创建令牌"
  - text: 🔑
  - paragraph: 暂无 API 令牌
  - button "创建第一个令牌"
- status "网关状态": 今日请求 0 错误率 0.0% 熔断渠道 0 RPS 0.0 平均延迟 —
- region "通知"
```

# Test source

```ts
  1   | import { test, expect, loginAsAdmin, makeSessions } from './fixtures';
  2   | import type { Page } from '@playwright/test';
  3   | 
  4   | /**
  5   |  * 全站冒烟：核心页面能渲染、不报错、不白屏。
  6   |  *
  7   |  * 目的不是覆盖业务细节，而是兜住「路由/懒加载/守卫/接口解包」层面的
  8   |  * 整体性回归 —— 这类问题往往一改就整页白屏，且只在真实浏览器里暴露。
  9   |  */
  10  | 
  11  | /** 需要管理员权限的页面（AdminRoute 包裹） */
  12  | const ADMIN_PAGES: Array<[path: string, marker: string]> = [
  13  |   ['/channels', '渠道'],
  14  |   ['/users', '用户'],
  15  |   ['/settings', '设置'],
  16  |   ['/pricing', '定价'],
  17  |   ['/plans', '套餐'],
  18  |   ['/redemptions', '兑换'],
  19  |   ['/account-sessions', '账号'],
  20  | ];
  21  | 
  22  | /** 普通登录即可访问的页面 */
  23  | const USER_PAGES: Array<[path: string, marker: string]> = [
  24  |   ['/', '仪表盘'],
  25  |   ['/keys', '密钥'],
  26  |   ['/wallet', '钱包'],
  27  |   ['/logs', '日志'],
  28  |   ['/chat', '聊天'],
  29  |   ['/profile', '个人'],
  30  |   ['/tickets', '工单'],
  31  | ];
  32  | 
  33  | /** 页面主体已渲染（不是加载骨架、也不是空白） */
  34  | async function expectRendered(page: Page): Promise<void> {
  35  |   await expect(page.locator('.main-content')).toBeVisible();
  36  |   // 有内容：出现标题或任意交互控件
  37  |   const hasContent = await page.evaluate(() => {
  38  |     const main = document.querySelector('.main-content');
  39  |     if (!main) return false;
  40  |     const text = (main.textContent || '').trim();
  41  |     return text.length > 10 || main.querySelectorAll('button, input, table').length > 0;
  42  |   });
  43  |   expect(hasContent).toBe(true);
  44  | }
  45  | 
  46  | test.describe('全站冒烟', () => {
  47  |   test('登录页可渲染且表单可用', async ({ page, pageErrors }) => {
  48  |     await page.goto('/login');
  49  |     await expect(page.locator('input#email')).toBeVisible();
  50  |     await expect(page.locator('input#password')).toBeVisible();
  51  |     await expect(page.locator('button[type="submit"]')).toBeVisible();
  52  |     expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  53  |   });
  54  | 
  55  |   for (const [path, marker] of USER_PAGES) {
  56  |     test(`页面可渲染：${path}`, async ({ page, pageErrors }) => {
  57  |       await loginAsAdmin(page, { sessions: makeSessions(1) });
  58  |       await page.goto(path);
  59  |       await expectRendered(page);
  60  |       // 导航栏保持可用（页面级 ErrorBoundary 未兜底触发）
> 61  |       await expect(page.locator('.sidebar-aside').first()).toBeVisible();
      |                                                            ^ Error: expect(locator).toBeVisible() failed
  62  |       expect(pageErrors.join('\n')).not.toMatch(/ErrorBoundary|page-fade/);
  63  |       expect(pageErrors, `${path} 控制台报错：\n${pageErrors.join('\n')}`).toEqual([]);
  64  |       void marker;
  65  |     });
  66  |   }
  67  | 
  68  |   for (const [path] of ADMIN_PAGES) {
  69  |     test(`管理页可渲染：${path}`, async ({ page, pageErrors }) => {
  70  |       await loginAsAdmin(page, { sessions: makeSessions(1) });
  71  |       await page.goto(path);
  72  |       await expectRendered(page);
  73  |       // 非管理员不应看到 PermissionDenied（我们是 admin，应看到真实页面）
  74  |       await expect(page.locator('.permission-denied')).toHaveCount(0);
  75  |       expect(pageErrors, `${path} 控制台报错：\n${pageErrors.join('\n')}`).toEqual([]);
  76  |     });
  77  |   }
  78  | 
  79  |   test('未登录访问受保护页面被重定向到登录页', async ({ page }) => {
  80  |     await page.goto('/channels');
  81  |     await expect(page).toHaveURL(/\/login$/);
  82  |     await expect(page.locator('input#email')).toBeVisible();
  83  |   });
  84  | 
  85  |   test('首登弱口令标记触发强制改密，且无法用侧边栏绕过', async ({ page, pageErrors }) => {
  86  |     await loginAsAdmin(page, { mustChangePassword: true });
  87  |     await page.goto('/');
  88  |     // 路由级拦截：任何非 /profile 页面都应被送回改密页
  89  |     await expect(page).toHaveURL(/\/profile\?force_password=1/);
  90  | 
  91  |     // 直接改地址到别的页面，同样被拦
  92  |     await page.goto('/channels');
  93  |     await expect(page).toHaveURL(/\/profile\?force_password=1/);
  94  |     expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  95  |   });
  96  | 
  97  |   test('未知路由显示 404 页而非白屏', async ({ page, pageErrors }) => {
  98  |     await loginAsAdmin(page, { sessions: makeSessions(1) });
  99  |     await page.goto('/this-route-does-not-exist');
  100 |     // 404 是 catch-all 路由，不走 ProtectedLayout（因此没有 .main-content /
  101 |     // 侧边栏），由 RouteErrorPage 独立成页 —— 断言它自己的容器即可。
  102 |     await expect(page.locator('.route-error-page')).toBeVisible();
  103 |     await expect(page.locator('.route-error-page')).toContainText('页面不存在');
  104 |     const body = await page.locator('body').innerText();
  105 |     expect(body.length).toBeGreaterThan(10);
  106 |     expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  107 |   });
  108 | 
  109 |   test('侧边栏导航可切换页面', async ({ page, pageErrors }) => {
  110 |     await loginAsAdmin(page, { sessions: makeSessions(1) });
  111 |     await page.goto('/');
  112 |     await expect(page.locator('.main-content')).toBeVisible();
  113 | 
  114 |     await page.locator('.sidebar-aside').first().getByText('渠道管理', { exact: false }).click();
  115 |     await expect(page).toHaveURL(/\/channels$/);
  116 |     await expectRendered(page);
  117 |     expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  118 |   });
  119 | });
  120 | 
```