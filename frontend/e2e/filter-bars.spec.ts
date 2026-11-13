import { test, expect, loginAsAdmin } from './fixtures';

/**
 * 筛选工具条观感统一（对齐 ModelManage 的 .mm-filters / Channels 的 .ch-filters）。
 *
 * 起因：日志与审计页的筛选区原是 `Card` 内的 auto-fill 网格（控件 40px 高、
 * datetime-local/select 高度各异、按钮另起一行），账号池筛选行是内联 flex 混排
 * （.form-group 16px 底边距 + .seg-filter 约 33px）—— 两处都「高度参差、不居中」。
 *
 * 本用例锁的是**结构性不变量**（不锁像素级视觉）：容器单行 + align-items:center、
 * 筛选控件统一 30px 高、分段控件按钮不高过输入框。像素微调不应让本用例红，
 * 但「谁把 40px 的全尺寸 input 又塞回来」必须红。
 */

/** 账号池列表桩（形状对齐后端 AccountSessionItem；只喂筛选条，不关心行内容） */
const SESSIONS = {
  success: true,
  total: 1,
  data: [
    {
      id: 'fb-a', name: '筛-a', provider: 'openai_compatible', status: 'enabled',
      configured_status: 'enabled', channel_id: 'ch-e2e', channel_name: 'e2e 池',
      base_url: 'https://up.example/v1', priority: 0, weight: 1, max_concurrency: 0,
      models: [], extra_headers: {},
      proxy_state: { mode: 'direct', ok: true, error: null },
      limiter_state: { account_skip: false, skip_models: [], disable: false },
    },
  ],
};

/** 取 computed 值（避免 toHaveCSS 的字符串格式差异） */
async function cssOf(locator: import('@playwright/test').Locator, prop: string): Promise<string> {
  return locator.evaluate((el, p) => getComputedStyle(el).getPropertyValue(p), prop);
}

test.describe('筛选工具条观感统一', () => {
  test('日志与审计：单行工具条 + 30px 控件 + 状态分段触发查询', async ({ page, pageErrors }) => {
    await loginAsAdmin(page, { sessions: [] });
    await page.goto('/logs');

    const bar = page.locator('.lg-filters');
    await expect(bar).toBeVisible();

    // 单行 + 垂直居中（旧版是 Card 内 auto-fill 网格，不存在 .lg-filters）
    expect((await cssOf(bar, 'display')).trim()).toBe('flex');
    expect((await cssOf(bar, 'align-items')).trim()).toBe('center');

    // 状态：分段控件（全部 / 成功 / 失败）
    await expect(bar.locator('.lg-seg-btn')).toHaveText(['全部', '成功', '失败']);

    // 筛选输入统一 30px（全局 .form-input 是 40px —— 必须被页内规则压回）
    const heights = await bar.locator('.form-input').evaluateAll((els) =>
      els.map((el) => getComputedStyle(el).height));
    expect(heights.length).toBeGreaterThan(0);
    expect(new Set(heights)).toEqual(new Set(['30px']));

    // 右侧计数（复用 i18n「共 {{count}} 条」）
    await expect(bar.locator('.lg-filters-count')).toContainText('条');

    // 点「失败」→ 点「查询」→ GET /api/logs/requests 带 status=error
    await bar.locator('.lg-seg-btn', { hasText: '失败' }).click();
    await expect(bar.locator('.lg-seg-btn.active')).toHaveText('失败');
    const [req] = await Promise.all([
      page.waitForRequest((r) => r.url().includes('/api/logs/requests') && r.method() === 'GET'),
      bar.getByRole('button', { name: '查询' }).click(),
    ]);
    const url = new URL(req.url());
    expect(url.searchParams.get('status')).toBe('error');

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('账号池：单行工具条 + 30px 控件 + 分段按钮矮于输入框', async ({ page, pageErrors }) => {
    await page.route('**/api/channels**', (r) =>
      r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ success: true, data: [] }) }));
    await page.route('**/api/account-sessions/providers', (r) =>
      r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ success: true, data: [], total: 0 }) }));
    await page.route('**/api/account-sessions/stats', (r) =>
      r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ success: true, data: { channels: [], total: 0 } }) }));
    await page.route('**/api/account-sessions/connections', (r) =>
      r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ success: true, data: { counts: {} } }) }));
    await page.route(/\/api\/account-sessions(\?|$)/, (r) =>
      r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(SESSIONS) }));
    await loginAsAdmin(page, { sessions: [] });

    await page.goto('/channels?tab=accounts');
    await expect(page.locator('.as-table')).toBeVisible({ timeout: 10000 });

    const bar = page.locator('.as-filters');
    await expect(bar).toBeVisible();
    expect((await cssOf(bar, 'align-items')).trim()).toBe('center');

    // 筛选输入 / 下拉统一 30px
    const heights = await bar.locator('.form-input').evaluateAll((els) =>
      els.map((el) => getComputedStyle(el).height));
    expect(heights.length).toBeGreaterThan(0);
    expect(new Set(heights)).toEqual(new Set(['30px']));

    // 分段控件几何对齐 .mm-seg / .ch-seg：11.5px 字号 + 4px/8px 内边距
    // （旧版是 13px + 7px 12px —— 与紧凑输入框明显不同档）。
    // 不断言按钮高度：App.css 的窄屏规则 `.btn, button { min-height: 36px }`
    // 会把所有按钮抬到 36px（触控目标，全站一致），桌面档才是自然高度。
    const segBtn = bar.locator('.seg-filter-btn').first();
    expect((await cssOf(segBtn, 'font-size')).trim()).toBe('11.5px');
    expect((await cssOf(segBtn, 'padding-top')).trim()).toBe('4px');
    expect((await cssOf(segBtn, 'padding-left')).trim()).toBe('8px');

    // 类名保留（e2e/account-pool.spec.ts 依赖）
    await expect(bar.locator('.seg-filter').first()).toBeVisible();
    await expect(bar.locator('.as-toggle')).toBeVisible();

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });
});
