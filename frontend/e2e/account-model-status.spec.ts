import { test, expect, loginAsAdmin } from './fixtures';
import type { Page } from '@playwright/test';

/**
 * 账号池「模型可见性状态」四态渲染（SPEC-1.0.46 §5 / §6.2）。
 *
 * mock 形状契约锚点（防「前端假定形状」假绿）：
 *   GET /api/account-sessions → { success, data:[...], total }
 *     AccountSessionItem 新增可选字段 `models_status`（后端 session_json 显式
 *     snake_case 序列化，见 src/api/admin/account_sessions.rs）：
 *       { state:'whitelist'|'catalog'|'none'|'failed', count:number, error?:string|null }
 *   契约先写、后端未落地时不下发该字段 → 前端不渲染（本用例喂四态，锁渲染口径）。
 *
 * mock 分流纪律（#31 教训）：列表用 /\/api\/account-sessions(\?|$)/ 精确 regex，
 * 绝不写宽 glob —— 否则 providers/stats/connections 子路径被吞、形状错、假绿。
 */

const dataRows = (page: Page) => page.locator('.as-table tbody tr:not(.as-tr-group):not(.as-panel-row)');

/** 账号列表桩的公共字段（形状对齐后端 AccountSessionItem） */
function session(over: Record<string, unknown>): Record<string, unknown> {
  return {
    channel_id: 'ch-e2e', channel_name: 'e2e 池', base_url: 'https://up.example/v1',
    priority: 0, weight: 1, max_concurrency: 0, models: [], extra_headers: {},
    configured_status: 'enabled',
    ...over,
  };
}

/** 四态样本：白名单 / 目录发现 / 不支持发现 / 探测失败 */
const SESSIONS_MODELS = {
  success: true,
  total: 4,
  data: [
    // failed 行放最前：它的行内「重试」按钮要可点（底部固定状态栏 `.statusline`
    // 会盖住视口底部的行，末行按钮常被它拦截 pointer events）。
    session({
      id: 'ms-fail', name: '态-失败', provider: 'workbuddy', status: 'enabled',
      models_status: { state: 'failed', count: 0, error: '上游目录探测失败：HTTP 500' },
    }),
    session({
      id: 'ms-wl', name: '态-白名单', provider: 'openai_compatible', status: 'enabled',
      models_status: { state: 'whitelist', count: 3, error: null },
    }),
    session({
      id: 'ms-cat', name: '态-目录', provider: 'workbuddy', status: 'enabled',
      models_status: { state: 'catalog', count: 5, error: null },
    }),
    session({
      id: 'ms-none', name: '态-无', provider: 'qoder', status: 'enabled',
      models_status: { state: 'none', count: 0, error: null },
    }),
  ],
};

const PROVIDERS = { success: true, data: [], total: 0 };
const STATS = { success: true, data: { channels: [], total: 0 } };
const CONNECTIONS = { success: true, data: { counts: {} } };
const PROXY_POOL = { success: true, total: 0, items: [] };

async function gotoAccounts(page: Page): Promise<void> {
  await page.goto('/channels?tab=accounts');
  await expect(page.locator('.as-table')).toBeVisible({ timeout: 10000 });
}

test.describe('账号池模型状态（SPEC-1.0.46 §5/§6.2）', () => {
  test.beforeEach(async ({ page }) => {
    // 静默桩，避免 console error（形状对齐 static-server.mjs 既有桩）
    await page.route('**/api/channels**', (r) =>
      r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ success: true, data: [] }) }));
    await page.route('**/api/account-sessions/providers', (r) =>
      r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(PROVIDERS) }));
    await page.route(/\/api\/account-sessions\/stats(\?|$)/, (r) =>
      r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(STATS) }));
    await page.route('**/api/account-sessions/connections', (r) =>
      r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(CONNECTIONS) }));
    await page.route('**/api/proxy-pool', (r) =>
      r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(PROXY_POOL) }));
    await page.route(/\/api\/account-sessions(\?|$)/, (r) =>
      r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(SESSIONS_MODELS) }));
    await loginAsAdmin(page, { sessions: [] });
  });

  test('四态渲染：行内紧凑标签 + title 长文案；failed 态带重试按钮', async ({ page, pageErrors }) => {
    await gotoAccounts(page);
    await expect(dataRows(page)).toHaveCount(4);

    // 行内 spy：紧凑形态 —— **短标签与长句两条都要断**（长句迁 title，不是删除）。
    // whitelist → 「白名单 N」+ ok 字形 + title 完整句
    const wl = dataRows(page).filter({ hasText: '态-白名单' }).locator('.as-model-status');
    await expect(wl).toContainText('白名单 3');
    await expect(wl).toHaveAttribute('title', '模型清单：白名单 3 个');
    await expect(wl.locator('.as-model-glyph.ok')).toHaveCount(1);
    // catalog → 「目录 N」+ title 完整句（含「自动发现…写入白名单固化」提示）
    const cat = dataRows(page).filter({ hasText: '态-目录' }).locator('.as-model-status');
    await expect(cat).toContainText('目录 5');
    await expect(cat).toHaveAttribute(
      'title',
      '模型清单：目录发现 5 个 · 模型由上游目录自动发现；可在编辑弹窗写入白名单固化',
    );
    // none → 行内「无模型」，完整解释在 title
    const none = dataRows(page).filter({ hasText: '态-无' }).locator('.as-model-status');
    await expect(none).toContainText('无模型');
    await expect(none).toHaveAttribute('title', '模型清单：无（该供应商不支持自动发现，请手填）');

    // failed → 行内「同步失败」+ title 里 error 原文 + 行内重试按钮
    const fail = dataRows(page).filter({ hasText: '态-失败' });
    const st = fail.locator('.as-model-status-err');
    await expect(st).toContainText('同步失败');
    await expect(st).toHaveAttribute('title', /^模型同步失败：.*上游目录探测失败：HTTP 500/);
    await expect(fail.locator('.as-model-retry')).toBeVisible();

    // 无状态下发时（其它三态）不出现重试按钮
    await expect(dataRows(page).filter({ hasText: '态-白名单' }).locator('.as-model-retry')).toHaveCount(0);

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('failed 态点「重试」→ 打到 POST /api/account-sessions/catalog/sync', async ({ page, pageErrors }) => {
    const syncCalls: string[] = [];
    await page.route('**/api/account-sessions/catalog/sync', (r) => {
      syncCalls.push(r.request().method());
      return r.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify({ success: true, data: { synced: 1, models: [] } }),
      });
    });

    await gotoAccounts(page);
    const fail = dataRows(page).filter({ hasText: '态-失败' });
    await fail.locator('.as-model-retry').click();

    await expect.poll(() => syncCalls.length, { timeout: 5000 }).toBeGreaterThan(0);
    expect(syncCalls[0]).toBe('POST');
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('后端未下发 models_status（可选字段）→ 不渲染、不占位、不崩', async ({ page, pageErrors }) => {
    await page.route(/\/api\/account-sessions(\?|$)/, (r) =>
      r.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify({
          success: true,
          total: 1,
          data: [session({ id: 'no-ms', name: '无状态', provider: 'openai_compatible', status: 'enabled' })],
        }),
      }));

    await gotoAccounts(page);
    await expect(dataRows(page)).toHaveCount(1);
    await expect(dataRows(page).filter({ hasText: '无状态' })).toBeVisible();
    await expect(page.locator('.as-model-status')).toHaveCount(0);

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });
});
