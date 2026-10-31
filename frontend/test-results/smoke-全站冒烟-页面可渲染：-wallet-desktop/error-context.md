# Instructions

- Following Playwright test failed.
- Explain why, be concise, respect Playwright best practices.
- Provide a snippet of code with the fix, if possible.

# Test info

- Name: smoke.spec.ts >> 全站冒烟 >> 页面可渲染：/wallet
- Location: e2e\smoke.spec.ts:56:5

# Error details

```
Error: expect(received).toBe(expected) // Object.is equality

Expected: true
Received: false
```

# Page snapshot

```yaml
- generic [ref=e2]:
  - generic [ref=e3]:
    - complementary [ref=e5]:
      - generic [ref=e6]:
        - img "AIGX" [ref=e8]
        - generic [ref=e9]:
          - generic [ref=e10]: AIGX
          - generic [ref=e11]: AI 中转网关
      - navigation [ref=e12]:
        - generic [ref=e13]:
          - generic "通用" [ref=e14]
          - link "仪表盘" [ref=e15] [cursor=pointer]:
            - /url: /
          - link "聊天" [ref=e22] [cursor=pointer]:
            - /url: /chat
          - link "提示词库" [ref=e26] [cursor=pointer]:
            - /url: /prompts
          - link "API 密钥" [ref=e31] [cursor=pointer]:
            - /url: /keys
          - link "用量日志" [ref=e36] [cursor=pointer]:
            - /url: /logs
          - link "模型管理" [ref=e41] [cursor=pointer]:
            - /url: /model-manage
        - generic [ref=e53]:
          - generic "管理" [ref=e54]
          - link "渠道管理" [ref=e55] [cursor=pointer]:
            - /url: /channels
          - link "账号池" [ref=e63] [cursor=pointer]:
            - /url: /account-sessions
          - link "定价倍率" [ref=e69] [cursor=pointer]:
            - /url: /pricing
          - link "用户管理" [ref=e76] [cursor=pointer]:
            - /url: /users
          - link "兑换码" [ref=e83] [cursor=pointer]:
            - /url: /redemptions
          - link "套餐管理" [ref=e87] [cursor=pointer]:
            - /url: /plans
          - link "签到中心" [ref=e93] [cursor=pointer]:
            - /url: /checkin
          - link "系统设置" [ref=e98] [cursor=pointer]:
            - /url: /settings
          - link "AI 运维" [ref=e103] [cursor=pointer]:
            - /url: /agent
        - generic [ref=e108]:
          - generic "个人" [ref=e109]
          - link "钱包" [ref=e110] [cursor=pointer]:
            - /url: /wallet
          - link "工单" [ref=e116] [cursor=pointer]:
            - /url: /tickets
          - link "个人中心" [ref=e120] [cursor=pointer]:
            - /url: /profile
      - button "收起侧边栏" [ref=e125] [cursor=pointer]
      - button "A admin@aigx.local admin" [ref=e133] [cursor=pointer]:
        - generic [ref=e134]: A
        - generic [ref=e135]:
          - generic [ref=e136]: admin@aigx.local
          - generic [ref=e137]: admin
    - main [ref=e140]:
      - generic [ref=e142]:
        - generic [ref=e143]:
          - heading "钱包充值" [level=1] [ref=e144]
          - paragraph [ref=e145]: 通过易支付为账户充值配额
        - generic [ref=e148]:
          - generic [ref=e149]:
            - generic [ref=e150]: 当前账户
            - generic [ref=e151]: admin@aigx.local
            - generic [ref=e152]: admin
          - generic [ref=e153]:
            - generic [ref=e154]: 剩余配额
            - generic [ref=e155]: "0"
          - generic [ref=e156]:
            - generic [ref=e157]: 兑换倍率
            - generic [ref=e158]: 1 元 = 1 配额
        - generic [ref=e159]:
          - heading "充值" [level=2] [ref=e161]
          - generic [ref=e162]:
            - generic [ref=e163]:
              - button "¥10 10 配额" [ref=e164] [cursor=pointer]:
                - generic [ref=e165]: ¥10
                - generic [ref=e166]: 10 配额
              - button "¥50 50 配额" [ref=e167] [cursor=pointer]:
                - generic [ref=e168]: ¥50
                - generic [ref=e169]: 50 配额
              - button "¥100 100 配额" [ref=e170] [cursor=pointer]:
                - generic [ref=e171]: ¥100
                - generic [ref=e172]: 100 配额
              - button "¥500 500 配额" [ref=e173] [cursor=pointer]:
                - generic [ref=e174]: ¥500
                - generic [ref=e175]: 500 配额
            - generic [ref=e176]:
              - generic [ref=e177]:
                - generic [ref=e178]: 充值金额（元）
                - spinbutton "充值金额（元）" [ref=e179]: "10"
                - generic [ref=e180]: 最低 1 元，将获得 10 配额
              - generic [ref=e181]:
                - generic [ref=e182]: 支付方式
                - generic [ref=e183]:
                  - button "支付宝" [ref=e184] [cursor=pointer]
                  - button "微信" [ref=e186] [cursor=pointer]
              - generic [ref=e188]:
                - generic [ref=e189]: 实付金额
                - generic [ref=e190]: ¥10
                - generic [ref=e191]: →
                - generic [ref=e192]: 获得配额
                - generic [ref=e193]: "10"
              - button "立即充值" [ref=e194] [cursor=pointer]
        - generic [ref=e195]:
          - heading "邀请返利" [level=2] [ref=e197]
          - generic [ref=e199]:
            - generic [ref=e200]:
              - generic [ref=e201]: 我的邀请码
              - generic [ref=e202]:
                - code [ref=e203]: —
                - button "复制邀请链接" [disabled] [ref=e204]
            - generic [ref=e205]:
              - generic [ref=e206]: 已邀请人数
              - generic [ref=e207]: "0"
            - generic [ref=e208]:
              - generic [ref=e209]: 待领取奖励
              - generic [ref=e210]: "0"
            - generic [ref=e211]:
              - generic [ref=e212]: 累计邀请获得
              - generic [ref=e213]: "0"
            - button "领取奖励" [disabled] [ref=e214]
        - generic [ref=e215]:
          - heading "兑换码充值" [level=2] [ref=e217]
          - generic [ref=e219]:
            - generic [ref=e220]:
              - generic [ref=e221]: 兑换码
              - textbox "兑换码" [ref=e222]:
                - /placeholder: 输入兑换码直接充值配额
              - generic [ref=e223]: 输入管理员发放的兑换码，即可将对应配额充入账户。
            - button "立即兑换" [ref=e224] [cursor=pointer]
        - generic [ref=e225]:
          - heading "我的订单 (0)" [level=2] [ref=e227]
          - generic [ref=e229]:
            - generic [ref=e230]: 🧾
            - paragraph [ref=e231]: 暂无订单
    - status "网关状态" [ref=e232]:
      - generic [ref=e234]: 今日请求 0
      - generic [ref=e235]: 错误率 0.0%
      - generic [ref=e236]: 熔断渠道 0
      - generic [ref=e237]: RPS 0.0
      - generic [ref=e238]: 平均延迟 —
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
> 43  |   expect(hasContent).toBe(true);
      |                      ^ Error: expect(received).toBe(expected) // Object.is equality
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
  61  |       await expect(page.locator('.sidebar-aside').first()).toBeVisible();
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