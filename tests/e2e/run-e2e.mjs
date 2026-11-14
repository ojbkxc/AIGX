// 真实浏览器 E2E 执行器（node 直跑，不依赖 vitest 环境）
//
// 目标与凭据一律 env 注入（AIGX_E2E_BASE / AIGX_E2E_USER / AIGX_E2E_PASS），
// 缺任一即 fail-fast——历史默认值（生产 IP 104.223.65.202:9527 + 出厂口令
// admin/123456）已移除：默认直连生产属于凭据入库 + 生产误击风险，CI 由
// run-browser-e2e.mjs 起本地真后端 + mock 上游后注入 env。
import { chromium } from '../../frontend/node_modules/playwright-core/index.mjs';

const BASE = process.env.AIGX_E2E_BASE;
const USER = process.env.AIGX_E2E_USER;
const PASS = process.env.AIGX_E2E_PASS;
if (!BASE || !USER || !PASS) {
  console.error(
    '[e2e] 缺 AIGX_E2E_BASE / AIGX_E2E_USER / AIGX_E2E_PASS env —— 本脚本不再内嵌默认目标与凭据；本地可用 `node tests/e2e/run-browser-e2e.mjs` 一键起真后端+mock 上游',
  );
  process.exit(2);
}

const results = [];
const check = (name, ok, detail = '') => {
  results.push({ name, ok });
  console.log(`${ok ? 'PASS' : 'FAIL'} — ${name}${detail ? ` | ${detail}` : ''}`);
};

const browser = await chromium.launch({ executablePath: process.env.CHROME_PATH || undefined });
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
  // 渠道行的对话按钮是图标按钮（无可见文本，title 承载文字），选择器用
  // button.ch-icon-btn[title="对话调试"]（重构后 MessageSquare 图标按钮）
  const chatBtn = page.locator('button.ch-icon-btn[title="对话调试"]').first();
  const hasChat = await chatBtn.isVisible({ timeout: 5000 }).catch(() => false);
  if (hasChat) {
    await chatBtn.click();
    await page.waitForTimeout(1000);
    await page.screenshot({ path: 'tests/e2e/screenshots/04-chat-open.png' });
    check('对话调试弹窗打开', await page.locator('.chat-debugger').isVisible().catch(() => false));

    // 模型搜索（ModelPicker 统一组件：.model-picker-btn 打开弹层，
    // .model-picker-search input 过滤，.model-picker-item 为选项）
    const pickerBtn = page.locator('.model-picker-btn').first();
    if (await pickerBtn.isVisible().catch(() => false)) {
      await pickerBtn.click();
      const search = page.locator('.model-picker-search input');
      if (await search.isVisible().catch(() => false)) {
        check('模型选择器搜索框', true);
        // 搜索第一个渠道的模型片段（DeepSeek官网 → deepseek-*），
        // 断言过滤生效而非固定搜 glm（该渠道不含 glm 模型）
        await search.fill('deepseek');
        await page.waitForTimeout(400);
        const filtered = await page.locator('.model-picker-item').count();
        check('搜索过滤生效', filtered > 0, `deepseek 匹配=${filtered}`);
        await page.screenshot({ path: 'tests/e2e/screenshots/05-model-search.png' });
        await page.keyboard.press('Escape');
      } else {
        check('模型选择器搜索框', false, '未渲染');
      }
    }

    // 发送（chat-debugger-input 输入框 + chat-debugger-send-fab 发送按钮）
    const ta = page.locator('.chat-debugger-input').last();
    await ta.fill('你好，一句话介绍你自己');
    await page.screenshot({ path: 'tests/e2e/screenshots/06-before-send.png' });
    const sendBtn = page.locator('.chat-debugger-send-fab').first();
    if (await sendBtn.isVisible().catch(() => false)) await sendBtn.click();
    else await ta.press('Enter');
    await page.waitForTimeout(9000);
    await page.screenshot({ path: 'tests/e2e/screenshots/07-after-send.png' });
    const assistantMsgs = await page.locator('[class*="assistant"]').count();
    check('对话发送并收到回复', assistantMsgs > 0, `assistant_elements=${assistantMsgs}`);
  } else {
    check('对话调试入口可见', false, '渠道行无对话按钮');
  }

  // 6. Settings 改密表单（五分区后位于「账户」Tabs）
  await page.goto(`${BASE}/settings`, { waitUntil: 'networkidle' });
  await page.screenshot({ path: 'tests/e2e/screenshots/08-settings.png' });
  await page.getByRole('tab', { name: '账户' }).click();
  await page.waitForTimeout(300);
  const hasPwCard = (await page.getByText('当前密码').count()) > 0 || (await page.getByText('旧密码').count()) > 0;
  check('改密表单（账户安全卡片）', hasPwCard);

  // 7. 模型管理（原 /model-presets 目录页已并入 /model-manage，App.tsx 里
  //    /model-presets 是重定向路由——导航锚点用 a[href="/model-manage"]）
  await page.locator('a[href="/model-manage"]').first().click();
  await page.waitForTimeout(1500);
  await page.screenshot({ path: 'tests/e2e/screenshots/09-models.png' });
  const modelHeading = await page.getByRole('heading', { name: /模型管理/ }).count();
  const modelSearchVisible = await page.locator('.models-search input').isVisible().catch(() => false);
  check('模型管理页面渲染', modelHeading > 0 || modelSearchVisible, 'heading=' + modelHeading);

  // 8. 提示词库（Prompts workspace）— SPA 内导航
  await page.locator('a[href="/prompts"]').first().click();
  await page.waitForTimeout(1200);
  await page.screenshot({ path: 'tests/e2e/screenshots/10-prompts.png' });
  const promptsHeading = await page.getByRole('heading', { name: /提示词库|Prompts/ }).count();
  const promptsNewBtn = await page.locator('button', { hasText: /新建提示词|New Prompt/ }).first().isVisible().catch(() => false);
  check('提示词库页面渲染', promptsHeading > 0 || promptsNewBtn, 'heading/新按钮可见');

  // 9. 会话撤销：登出后旧 token 立即失效（P1 收尾验收项）
  const tokenBefore = await page.evaluate(() => localStorage.getItem('token'));
  await page.evaluate(async () => {
    await fetch('/api/auth/logout', { method: 'POST', headers: { Authorization: 'Bearer ' + localStorage.getItem('token') } });
  });
  const revokedStatus = await page.evaluate(async () => {
    const token = localStorage.getItem('token');
    const res = await fetch('/api/users/me', { headers: { Authorization: 'Bearer ' + token } });
    return res.status;
  });
  check('登出后旧 token 立即失效', revokedStatus === 401, 'status=' + revokedStatus + ', token=' + String(tokenBefore && tokenBefore.slice(0, 12)) + '...');
} catch (e) {
  console.error('E2E 异常:', e.message);
  await page.screenshot({ path: 'tests/e2e/screenshots/99-error.png' }).catch(() => {});
} finally {
  await browser.close();
}

const failed = results.filter((r) => !r.ok).length;
console.log(`\n========== E2E 结果: ${results.length - failed}/${results.length} PASS ==========`);
process.exit(failed > 0 ? 1 : 0);
