import { test, expect, loginAsAdmin } from './fixtures';
import type { Page } from '@playwright/test';

/**
 * 账号池 1.0.44 验收面（docs/SPEC-1.0.44-ACCOUNT-POOL.md §7 的 7 条）。
 *
 * ┌─────────────────────────────────────────────────────────────────────┐
 * │ mock 形状契约锚点（防「前端假定形状」假绿——逐字段出处）：           │
 * │  账号列表 GET /api/account-sessions → {success, data:[...], total}  │
 * │    新增字段形状见 SPEC §4（后端 session_json，account_sessions.rs）：│
 * │      proxy_state?: { mode:'direct'|'url'|'pool'|'rotate',            │
 * │                       ok:boolean, error?:string|null }              │
 * │      limiter_state?: { account_skip:boolean,                         │
 * │                        skip_models:string[], disable:boolean }       │
 * │  代理池 GET /api/proxy-pool → {success, items:[{id,name,enabled,…}]} │
 * │  行内代理 PATCH /api/account-sessions/:id → body 含 proxy（三态：    │
 * │    缺省保持 / 空串清空 / 非空设置；SPEC §2.11）                      │
 * │  mock 分流纪律（#31 教训）：列表用 /\/api\/account-sessions(\?|$)/   │
 * │    精确 regex，绝不写宽 glob——否则 providers/stats/connections 等    │
 * │    子路径被吞、形状错、假绿。                                        │
 * └─────────────────────────────────────────────────────────────────────┘
 *
 * 说明：1.0.44 前端改动尚未 build，本 spec 只做静态校验；全量回归由 lead
 * 在 build 之后统一跑（E2E_PORT=4174 npx playwright test --trace=off）。
 */

/** 数据行（排除分组带 / 面板行） */
const dataRows = (page: Page) => page.locator('.as-table tbody tr:not(.as-tr-group):not(.as-panel-row)');

/** 进账号池 Tab 并等表格渲染 */
async function gotoAccounts(page: Page): Promise<void> {
  await page.goto('/channels?tab=accounts');
  await expect(page.locator('.as-table')).toBeVisible({ timeout: 10000 });
}

/** 账号列表桩的公共字段（列宽/默认态无关紧要，形状对齐后端 AccountSessionItem） */
function session(over: Record<string, unknown>): Record<string, unknown> {
  return {
    channel_id: 'ch-e2e', channel_name: 'e2e 池', base_url: 'https://up.example/v1',
    priority: 0, weight: 1, max_concurrency: 0, models: [], extra_headers: {},
    configured_status: 'enabled',
    ...over,
  };
}

/** §2.4 限额分段桩：A 整号跳过（已限流）/ B 仅 Token 跳过（仍正常）/ C 干净（正常）/ D 禁用 */
const SESSIONS_LIMIT = {
  success: true,
  total: 4,
  data: [
    session({ id: 'lim-a', name: '额-a', provider: 'openai_compatible', status: 'enabled',
      limiter_state: { account_skip: true, skip_models: [], disable: false } }),
    session({ id: 'lim-b', name: '额-b', provider: 'openai_compatible', status: 'enabled',
      limiter_state: { account_skip: false, skip_models: ['glm-4'], disable: false } }),
    session({ id: 'lim-c', name: '额-c', provider: 'openai_compatible', status: 'enabled',
      limiter_state: { account_skip: false, skip_models: [], disable: false } }),
    session({ id: 'lim-d', name: '额-d', provider: 'openai_compatible', status: 'disabled',
      configured_status: 'disabled',
      limiter_state: { account_skip: true, skip_models: [], disable: false } }),
  ],
};

/** §2.11 行内代理下拉桩：一条直连账号（proxy 空串 + proxy_state.ok） */
const SESSIONS_PROXY = {
  success: true,
  total: 1,
  data: [
    session({ id: 'px-a', name: '代-a', provider: 'openai_compatible', status: 'enabled',
      proxy: '', proxy_state: { mode: 'direct', ok: true, error: null } }),
  ],
};

const PROXY_POOL = {
  success: true,
  total: 1,
  items: [{ id: 'px_e2e_1', name: '香港节点', enabled: true, protocol: 'socks5', host: '127.0.0.1', port: 1080, username: '', has_password: false, usedBy: [] }],
};

const PROVIDERS = { success: true, data: [], total: 0 };
const STATS = { success: true, data: { channels: [], total: 0 } };
const CONNECTIONS = { success: true, data: { counts: {} } };

/** 读账号伪元素列的索引（按表头文案定位，避免依赖新增列的具体 td class 名） */
async function proxyColIndex(page: Page): Promise<number> {
  const texts = await page.locator('.as-table thead th').allInnerTexts();
  return texts.findIndex((t) => t.includes('代理'));
}

test.describe('账号池 1.0.44 · §7 验收面', () => {
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
    await loginAsAdmin(page, { sessions: [] });
  });

  // 1 §1 表头 sticky + 12.5px 字号（断言 computed style）
  test('1 · 账号池表头 sticky 且字号 12.5px/10.5px、内边距 7px 10px', async ({ page, pageErrors }) => {
    await gotoAccounts(page);

    // 滚动容器：.as-scroll 可滚动 + 有 max-height（= 表头 sticky 生效的前提）
    const scroll = await page.locator('.as-scroll').evaluate((el) => {
      const s = getComputedStyle(el);
      return { overflowY: s.overflowY, maxHeight: s.maxHeight };
    });
    expect(scroll.overflowY).toBe('auto');
    expect(scroll.maxHeight).not.toBe('none');

    // 表头 th：sticky + 10.5px + padding 7px 10px
    const thStyle = await page.locator('.as-table thead th').first().evaluate((el) => {
      const s = getComputedStyle(el);
      return { position: s.position, top: s.top, fontSize: s.fontSize, paddingTop: s.paddingTop, paddingLeft: s.paddingLeft };
    });
    expect(thStyle.position).toBe('sticky');
    expect(thStyle.top).toBe('0px');
    expect(thStyle.fontSize).toBe('10.5px');
    expect(thStyle.paddingTop).toBe('7px');
    expect(thStyle.paddingLeft).toBe('10px');

    // 数据格 td：继承 12.5px + padding 7px 10px
    const tdStyle = await page.locator('td.as-c-name').first().evaluate((el) => {
      const s = getComputedStyle(el);
      return { fontSize: s.fontSize, paddingTop: s.paddingTop, paddingLeft: s.paddingLeft };
    });
    expect(tdStyle.fontSize).toBe('12.5px');
    expect(tdStyle.paddingTop).toBe('7px');
    expect(tdStyle.paddingLeft).toBe('10px');

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  // 2 §1 列设置「对齐」修复：右对齐 → th/td 真的挂 ta-right
  test('2 · 列设置改右对齐 → 该列 th/td 出现 ta-right', async ({ page, pageErrors }) => {
    await gotoAccounts(page);

    // 账号池的列设置按钮被 Tooltip 包成 <span class="glass-tooltip-wrapper">，
    // 按钮是孙节点；用后代选择器（Channels 页是直子，才有 .ch-colset > button）
    await page.locator('.as-colset button').click();
    const panel = page.locator('.tbl-colset');
    await expect(panel).toBeVisible();

    // provider 列（默认左对齐）→ 点「右」
    await page.locator('[data-col-key="provider"] .tbl-colset-align button').filter({ hasText: '右' }).click();

    await expect(page.locator('th.as-h-provider')).toHaveClass(/ta-right/);
    await expect(page.locator('td.as-c-provider').first()).toHaveClass(/ta-right/);

    // 落盘：公共列设置 v2 里 provider.align = 'right'
    const align = await page.evaluate(() => {
      const raw = localStorage.getItem('aigx-tbl-cols.accounts');
      const items = (JSON.parse(raw || '{}') as { items?: Array<{ key: string; align: string }> }).items ?? [];
      return items.find((c) => c.key === 'provider')?.align;
    });
    expect(align).toBe('right');

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  // 3 §2.4 限额分段：点「已限流」只剩 limiter_state 命中的行；禁用账号不计入正常/已限流
  test('3 · 限额分段「已限流/正常」按 limiter_state 过滤，禁用账号不落两档', async ({ page, pageErrors }) => {
    await page.route(/\/api\/account-sessions(\?|$)/, (r) =>
      r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(SESSIONS_LIMIT) }));

    await gotoAccounts(page);
    await expect(dataRows(page)).toHaveCount(4);

    // 已限流 → 仅 A（整号跳过）
    await page.locator('.seg-filter-btn', { hasText: '已限流' }).click();
    await expect(dataRows(page)).toHaveCount(1);
    await expect(dataRows(page).first()).toContainText('额-a');

    // 正常 → B（仅 Token 跳过仍算正常）+ C；禁用账号 D 不属于任何一档
    await page.locator('.seg-filter-btn', { hasText: '正常' }).click();
    await expect(dataRows(page)).toHaveCount(2);
    await expect(dataRows(page).filter({ hasText: '额-b' })).toHaveCount(1);
    await expect(dataRows(page).filter({ hasText: '额-c' })).toHaveCount(1);
    await expect(dataRows(page).filter({ hasText: '额-d' })).toHaveCount(0);

    // 全部 → 4 行
    await page.locator('.seg-filter-btn', { hasText: '全部' }).first().click();
    await expect(dataRows(page)).toHaveCount(4);

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  // 4 §2.2 折叠全部 / 展开全部（仅分组模式可见）
  test('4 · 折叠全部/展开全部仅分组模式可见，点击整体收起/展开', async ({ page, pageErrors }) => {
    await gotoAccounts(page);

    // 未分组 → 两按钮不渲染
    await expect(page.getByRole('button', { name: '折叠全部' })).toHaveCount(0);
    await expect(page.getByRole('button', { name: '展开全部' })).toHaveCount(0);

    // 开分组（内置桩 4 行 = openai_compatible / workbuddy 两组）
    await page.locator('.as-toggle', { hasText: '分组' }).locator('input').check();
    await expect(page.locator('.as-tr-group')).toHaveCount(2);
    await expect(dataRows(page)).toHaveCount(4);

    await expect(page.getByRole('button', { name: '折叠全部' })).toBeVisible();
    await page.getByRole('button', { name: '折叠全部' }).click();
    await expect(dataRows(page)).toHaveCount(0);

    await expect(page.getByRole('button', { name: '展开全部' })).toBeVisible();
    await page.getByRole('button', { name: '展开全部' }).click();
    await expect(dataRows(page)).toHaveCount(4);

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  // 5 §2.11 行内代理下拉「选中即存」→ 发出 PATCH(body.proxy)
  test('5 · 行内代理下拉改变值 → 发出 PATCH 且请求体含 proxy', async ({ page, pageErrors }) => {
    await page.route(/\/api\/account-sessions(\?|$)/, (r) =>
      r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(SESSIONS_PROXY) }));

    const patches: Array<Record<string, unknown>> = [];
    await page.route(/\/api\/account-sessions\/[^/]+$/, (r) => {
      const method = r.request().method();
      if (method === 'PATCH') {
        patches.push(r.request().postDataJSON() as Record<string, unknown>);
        return r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ success: true, data: {} }) });
      }
      return r.fallback();
    });

    await gotoAccounts(page);
    await expect(dataRows(page)).toHaveCount(1);

    // 代理列（按表头文案定位）里的 <select>
    const idx = await proxyColIndex(page);
    expect(idx, '代理列未渲染').toBeGreaterThanOrEqual(0);
    const select = dataRows(page).first().locator('td').nth(idx).locator('select');
    await expect(select).toBeVisible();

    // 选「轮询全部启用」（SPEC §2.11 定义的 rotate 选项）
    await select.selectOption({ label: '轮询全部启用' });

    await expect.poll(() => patches.length, { timeout: 5000 }).toBeGreaterThanOrEqual(1);
    const body = patches[patches.length - 1];
    expect(body).toHaveProperty('proxy');
    expect(typeof body.proxy).toBe('string');
    expect((body.proxy as string).length).toBeGreaterThan(0);

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  // 6 §5 定价列点击 → 「编辑定价」弹窗出现
  test('6 · 点定价列 → 「编辑定价」弹窗出现', async ({ page, pageErrors }) => {
    await page.goto('/model-manage');
    await expect(page.locator('h1')).toContainText('模型管理');

    // 模型目录 Tab 数据行里的定价单元格（.mm-c-price，已实现）
    const priceCell = page.locator('td.mm-c-price').first();
    await expect(priceCell).toBeVisible();
    await priceCell.click();

    await expect(page.locator('.modal-overlay h3', { hasText: '编辑定价' })).toBeVisible();

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  // 7 §3.1 侧边栏宽度 = 160px
  test('7 · 侧边栏展开宽度 = 160px', async ({ page, pageErrors }) => {
    test.skip((page.viewportSize()?.width ?? 0) < 900, '移动端侧栏为抽屉形态，宽度口径不同');
    await page.goto('/');
    const sidebar = page.locator('.sidebar-aside').first();
    await expect(sidebar).toBeVisible();
    const width = await sidebar.evaluate((el) => getComputedStyle(el).width);
    expect(width).toBe('160px');
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });
});
