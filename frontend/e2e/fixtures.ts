import { test as base, expect, type Page } from '@playwright/test';

/**
 * 预注入登录态与本地会话数据。
 *
 * 前端把会话完全存在 localStorage（`aigx_playground_sessions` /
 * `aigx_playground_active`），所以不需要任何后端就能构造多会话场景。
 */
export const SESSIONS_KEY = 'aigx_playground_sessions';
export const ACTIVE_KEY = 'aigx_playground_active';

export interface SeedSession {
  id: string;
  title: string;
  model: string;
  messages: Array<{ role: 'user' | 'assistant'; content: string }>;
  updated_at: number;
}

export function makeSessions(count: number, messageCount = 2): SeedSession[] {
  return Array.from({ length: count }, (_, i) => ({
    id: `s-${i + 1}`,
    title: `会话 ${i + 1}`,
    model: 'glm-4',
    messages: Array.from({ length: messageCount }, (_, j) => ({
      role: (j % 2 === 0 ? 'user' : 'assistant') as 'user' | 'assistant',
      content: `第 ${i + 1} 个会话的第 ${j + 1} 条消息`,
    })),
    updated_at: Date.UTC(2026, 9, 31) - i * 1000,
  }));
}

/**
 * 注入管理员登录态。
 *
 * 注意：默认**不**写 `must_change_password` —— 该标记会触发路由级强制
 * 改密拦截（除 /profile 外全部重定向），绝大多数页面用例应跳过它。
 */
export async function loginAsAdmin(
  page: Page,
  opts: { sessions?: SeedSession[]; mustChangePassword?: boolean } = {},
): Promise<void> {
  const { sessions, mustChangePassword = false } = opts;
  await page.addInitScript(
    ({ sessionsKey, activeKey, sess, mustChange }) => {
      localStorage.setItem('token', 'e2e-token');
      localStorage.setItem('expires_at', String(Date.now() + 24 * 3600 * 1000));
      localStorage.setItem('email', 'admin@aigx.local');
      localStorage.setItem('username', 'admin');
      localStorage.setItem('role', 'admin');
      // 固定语言，避免 navigator.language + geo-lang 造成文案抖动
      localStorage.setItem('i18n_lang', 'zh');
      if (mustChange) localStorage.setItem('must_change_password', '1');
      else localStorage.removeItem('must_change_password');
      if (sess) {
        localStorage.setItem(sessionsKey, JSON.stringify(sess));
        localStorage.setItem(activeKey, sess[0].id);
      }
    },
    {
      sessionsKey: SESSIONS_KEY,
      activeKey: ACTIVE_KEY,
      sess: sessions ?? null,
      mustChange: mustChangePassword,
    },
  );
}

/** 收集控制台错误与未捕获异常，用例结尾断言为空 */
export function collectPageErrors(page: Page): string[] {
  const errors: string[] = [];
  page.on('console', (m) => {
    if (m.type() === 'error') errors.push(`console: ${m.text()}`);
  });
  page.on('pageerror', (e) => errors.push(`pageerror: ${e.message}`));
  return errors;
}

export const test = base.extend<{ pageErrors: string[] }>({
  pageErrors: async ({ page }, use) => {
    const errors = collectPageErrors(page);
    await use(errors);
  },
});

export { expect };
