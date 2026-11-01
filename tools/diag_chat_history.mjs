// 诊断 /chat 历史会话在生产上的真实行为（临时脚本，不入库）
// 用法: node tools/diag_chat_history.mjs <token>
import { chromium } from 'file:///C:/GitHub/AIGX/frontend/node_modules/playwright-core/index.mjs';

const TOKEN = process.argv[2];
if (!TOKEN) { console.error('missing token'); process.exit(1); }
const BASE = 'https://api.lxseek.com';

const browser = await chromium.launch({
  executablePath: 'C:/Program Files/Google/Chrome/Application/chrome.exe',
  args: ['--ignore-certificate-errors'],
});
const page = await browser.newPage({ viewport: { width: 1280, height: 800 } });

// 预置登录态（token/role/expires_at 是 Login.tsx 写的三个键）
await page.addInitScript((tk) => {
  localStorage.setItem('token', tk);
  localStorage.setItem('role', 'admin');
  localStorage.setItem('expires_at', String(Date.now() + 86400000));
}, TOKEN);

const reqLog = [];
page.on('request', (r) => {
  const u = r.url();
  if (u.includes('/api/chat') || u.includes('chat_test')) reqLog.push(`${r.method()} ${u}`);
});
page.on('response', (r) => {
  const u = r.url();
  if (u.includes('/api/chat/sessions')) reqLog.push(`  <- ${r.status()} ${r.request().method()} ${u.split('api.lxseek.com')[1]}`);
});
const consoleErrs = [];
page.on('console', (m) => { if (m.type() === 'error') consoleErrs.push(m.text()); });
page.on('pageerror', (e) => consoleErrs.push(`PAGEERROR: ${e.message}`));

await page.goto(`${BASE}/chat`, { waitUntil: 'networkidle', timeout: 45000 });
await page.waitForTimeout(1500);
console.log('== 进入 /chat 后的网络请求 ==');
reqLog.forEach((l) => console.log(' ', l));

// 页面结构检查
const structure = await page.evaluate(() => ({
  pgShell: !!document.querySelector('.pg-shell'),
  drawerBtn: !!Array.from(document.querySelectorAll('button')).find((b) => /历史会话/.test(b.textContent || '')),
  msgArea: !!document.querySelector('.chat-debugger-messages'),
  msgCount: document.querySelectorAll('.chat-debugger-messages .msg-row, .chat-debugger-messages > *').length,
}));
console.log('== 页面结构 ==', JSON.stringify(structure));

await page.screenshot({ path: 'C:/GitHub/AIGX/.e2e-shots/diag-chat-1-loaded.png' });

// 输入一条消息发送（会触发 chat_test 或渠道直连；不依赖真实模型，看请求是否发出 + busy 守卫）
const ta = page.locator('.pg-shell textarea').first();
await ta.fill('diag-probe');
await ta.press('Enter');
await page.waitForTimeout(2500);
console.log('== 发送后 2.5s 的网络请求 ==');
reqLog.forEach((l) => console.log(' ', l));
await page.screenshot({ path: 'C:/GitHub/AIGX/.e2e-shots/diag-chat-2-sent.png' });

// 再等 debounce(500ms) + 网络往返，看 PUT 是否发出
await page.waitForTimeout(3000);
console.log('== 再等 3s 后（debounce 落盘窗口）==');
reqLog.forEach((l) => console.log(' ', l));

// 打开历史抽屉
const histBtn = page.getByRole('button', { name: '历史会话' }).first();
if (await histBtn.count()) {
  await histBtn.click();
  await page.waitForTimeout(1200);
  const drawer = await page.evaluate(() => {
    const root = document.querySelector('.sd-root');
    const items = Array.from(document.querySelectorAll('.sd-item')).map((el) => ({
      title: el.querySelector('.sd-item-title')?.textContent?.trim(),
      meta: el.querySelector('.sd-item-meta')?.textContent?.trim(),
      active: el.className.includes('active'),
    }));
    return { open: root?.className ?? '(无 .sd-root)', itemCount: items.length, items };
  });
  console.log('== 抽屉状态 ==', JSON.stringify(drawer, null, 2));
  await page.screenshot({ path: 'C:/GitHub/AIGX/.e2e-shots/diag-chat-3-drawer.png' });
} else {
  console.log('!! 找不到「历史会话」按钮');
}

console.log('== 控制台错误 ==');
consoleErrs.slice(0, 10).forEach((e) => console.log(' ', e));
await browser.close();
