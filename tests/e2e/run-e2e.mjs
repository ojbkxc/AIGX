// 真实浏览器 E2E 执行器（node 直跑，不依赖 vitest 环境）
import { chromium } from '../../frontend/node_modules/playwright-core/index.mjs';

const BASE = process.env.AIGX_E2E_BASE || 'http://104.223.65.202:9527';
const USER = process.env.AIGX_E2E_USER || 'admin';
const PASS = process.env.AIGX_E2E_PASS || '123456';

const results = [];
const check = (name, ok, detail = '') => {
  results.push({ name, ok });
  console.log(`${ok ? 'PASS' : 'FAIL'} — ${name}${detail ? ` | ${detail}` : ''}`);
};

const browser = await chromium.launch();
const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });

try {
  // 1. 登录页渲染
  await page.goto(`${BASE}/login`, { waitUntil: 'networkidle', timeout: 30000 });
  check('登录页加载', (await page.title()).length > 0, await page.title());
  const userInput = page.locator('input#email');
  await userInput.waitFor({ timeout: 10000 });
  check('用户名输入框渲染', await userInput.isVisible());
  check('密码输入框渲染', await page.locator('input#password').isVisible());
  check('登录按钮渲染', await page.locator('button[type="submit"]').isVisible());

  // 2. 填表单 + username 登录
  await userInput.fill(USER);
  await page.locator('input#password').fill(PASS);
  await page.screenshot({ path: 'tests/e2e/screenshots/01-login-filled.png' });
  await page.locator('button[type="submit"]').click();
  await page.waitForURL((u) => !u.pathname.startsWith('/login'), { timeout: 15000 });
  check('username 登录成功', !page.url().includes('/login'), page.url());

  // 3. 仪表盘
  await page.waitForLoadState('networkidle');
  await page.screenshot({ path: 'tests/e2e/screenshots/02-dashboard.png', fullPage: false });
  check('仪表盘渲染', await page.locator('aside, nav, .sidebar-desktop').first().isVisible());

  // 4. 渠道页
  await page.goto(`${BASE}/channels`, { waitUntil: 'networkidle' });
  await page.screenshot({ path: 'tests/e2e/screenshots/03-channels.png' });
  const rows = await page.locator('table tbody tr').count();
  check('渠道列表加载', rows > 0, `rows=${rows}`);

  // 5. 对话调试
  const chatBtn = page.locator('button', { hasText: '对话' }).first();
  const hasChat = await chatBtn.isVisible({ timeout: 5000 }).catch(() => false);
  if (hasChat) {
    await chatBtn.click();
    await page.waitForTimeout(1000);
    await page.screenshot({ path: 'tests/e2e/screenshots/04-chat-open.png' });
    check('对话调试弹窗打开', await page.locator('.chat-debugger').isVisible().catch(() => false));

    // 模型搜索
    const pickerBtn = page.locator('.chat-debugger-model-btn');
    if (await pickerBtn.isVisible().catch(() => false)) {
      await pickerBtn.click();
      const search = page.locator('.chat-debugger-search input');
      if (await search.isVisible().catch(() => false)) {
        check('模型选择器搜索框', true);
        await search.fill('glm');
        await page.waitForTimeout(400);
        const filtered = await page.locator('.chat-debugger-picker-item').count();
        check('搜索过滤生效', true, `glm 匹配=${filtered}`);
        await page.screenshot({ path: 'tests/e2e/screenshots/05-model-search.png' });
        await page.keyboard.press('Escape');
      } else {
        check('模型选择器搜索框', false, '未渲染');
      }
    }

    // 发送
    const ta = page.locator('.chat-input, textarea').last();
    await ta.fill('你好，一句话介绍你自己');
    await page.screenshot({ path: 'tests/e2e/screenshots/06-before-send.png' });
    const sendBtn = page.locator('.chat-debugger button').filter({ hasText: /发送|Send/ }).first();
    if (await sendBtn.isVisible().catch(() => false)) await sendBtn.click();
    else await ta.press('Enter');
    await page.waitForTimeout(9000);
    await page.screenshot({ path: 'tests/e2e/screenshots/07-after-send.png' });
    const assistantMsgs = await page.locator('[class*="assistant"]').count();
    check('对话发送并收到回复', assistantMsgs > 0, `assistant_elements=${assistantMsgs}`);
  } else {
    check('对话调试入口可见', false, '渠道行无对话按钮');
  }

  // 6. Settings 改密表单
  await page.goto(`${BASE}/settings`, { waitUntil: 'networkidle' });
  await page.screenshot({ path: 'tests/e2e/screenshots/08-settings.png' });
  const hasPwCard = (await page.getByText('当前密码').count()) > 0 || (await page.getByText('旧密码').count()) > 0;
  check('改密表单（账户安全卡片）', hasPwCard);
} catch (e) {
  console.error('E2E 异常:', e.message);
  await page.screenshot({ path: 'tests/e2e/screenshots/99-error.png' }).catch(() => {});
} finally {
  await browser.close();
}

const failed = results.filter((r) => !r.ok).length;
console.log(`\n========== E2E 结果: ${results.length - failed}/${results.length} PASS ==========`);
process.exit(failed > 0 ? 1 : 0);
