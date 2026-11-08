import { test, expect, loginAsAdmin } from './fixtures';

/**
 * 限制器条件组编辑面（#29）：编辑弹窗「限制器」段的条件组区块。
 *
 * ┌─────────────────────────────────────────────────────────────────────┐
 * │ mock 形状契约锚点（防「前端假定形状」假绿——逐字段出处）：           │
 * │  账号列表 GET /api/account-sessions                                │
 * │    → {success, data: [...], total}；账号项含 limiters（扁平规则，   │
 * │      后端 model::AccountLimiterRule）与 limiter_groups（条件组，     │
 * │      camelCase 线格式 = src/account_session/limiter.rs:118-159）：   │
 * │      { enabled, action: 'skip'|'disable', target: 'models'|'account',│
 * │        conditions: [{ join, sourceId, models, metric, comparison,    │
 * │          threshold, window: {reset, seconds?,…}|null }] }            │
 * │  条件 PATCH /api/account-sessions/:id                               │
 * │    → body 键 `limiter_groups`；三态：缺省=保持、空数组=清空回落      │
 * │      扁平双读、校验失败 400（account_sessions.rs:959-965）           │
 * │  mock 分流纪律（#31 教训）：列表用 /\/api\/account-sessions(\?|$)/   │
 * │    精确 regex，绝不写宽 glob——否则 providers/connections 等子路径    │
 * │    被吞、形状错、假绿。                                              │
 * └─────────────────────────────────────────────────────────────────────┘
 */

/** 账号列表桩（含一条旧扁平规则账号，供「从旧规则导入」入口） */
const SESSIONS_FLAT = {
  success: true,
  data: [
    {
      id: 'acc-lg', channel_id: 'ch-lg', channel_name: 'e2e 账号池', name: 'e2e-limiter',
      provider: 'openai_compatible', base_url: 'https://lg.example.com/v1',
      status: 'enabled', configured_status: 'enabled',
      priority: 0, weight: 1, max_concurrency: 0, models: [], extra_headers: {},
      // 扁平规则（AccountLimiterRule）：余额 < 5 跳过
      limiters: [{ type: 'balance', action: 'skip', threshold: 5, enabled: true }],
    },
  ],
  total: 1,
};

/** 条件组桩（形状 = limiter.rs:118-159 camelCase；or 开新分支 + and 同分支） */
const GROUP_TWO_COND = {
  enabled: true,
  action: 'skip',
  target: 'account',
  conditions: [
    { join: 'or', sourceId: '', models: [], metric: 'remaining', comparison: 'lte', threshold: 5, window: null },
    { join: 'and', sourceId: '', models: [], metric: 'used_percent', comparison: 'gte', threshold: 80, window: null },
  ],
};

const SESSIONS_WITH_GROUP = {
  success: true,
  data: [
    {
      id: 'acc-lg2', channel_id: 'ch-lg', channel_name: 'e2e 账号池', name: 'e2e-groups',
      provider: 'openai_compatible', base_url: 'https://lg.example.com/v1',
      status: 'enabled', configured_status: 'enabled',
      priority: 0, weight: 1, max_concurrency: 0, models: [], extra_headers: {},
      limiters: [{ type: 'balance', action: 'skip', threshold: 5, enabled: true }],
      limiter_groups: [GROUP_TWO_COND],
    },
  ],
  total: 1,
};

const CHANNELS = {
  success: true,
  data: [{ id: 9002, name: 'e2e 账号池', type: 'account', status: 'enabled' }],
};

const PROVIDERS = { success: true, data: [], total: 0 };

const STATS = { success: true, data: { channels: [], total: 0 } };

/** 打开账号池列表并进入第一行的编辑弹窗 */
async function openEditDialog(page: import('@playwright/test').Page, name: string): Promise<void> {
  await page.goto('/channels?tab=accounts');
  await expect(page.locator('.as-table')).toBeVisible({ timeout: 10000 });
  const row = page.locator('.as-table tbody tr').filter({ hasText: name });
  await row.locator('button[aria-label="编辑"]').click();
  await expect(page.locator('.modal-header h3').first()).toContainText('账号设置');
  await page.locator('.as-dlg-nav', { hasText: '限制器' }).click();
  await expect(page.locator('.as-lg-title')).toBeVisible();
}

test.describe('限制器条件组编辑面', () => {
  test.beforeEach(async ({ page }) => {
    await page.route('**/api/channels**', (r) =>
      r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(CHANNELS) }));
    await page.route('**/api/account-sessions/providers', (r) =>
      r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(PROVIDERS) }));
    await page.route(/\/api\/account-sessions\/stats(\?|$)/, (r) =>
      r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(STATS) }));
    // 连接数轮询（静默失败也无碍，桩掉免得刷 console error）
    await page.route('**/api/account-sessions/connections', (r) =>
      r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ success: true, data: { counts: {} } }) }));
    await loginAsAdmin(page);
  });

  test('新增条件组：tokens 标「暂未生效」；保存 PATCH 携带 camelCase 条件组', async ({ page, pageErrors }) => {
    await page.route(/\/api\/account-sessions(\?|$)/, (r) =>
      r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(SESSIONS_FLAT) }));

    // 主表单 PUT 与条件组 PATCH 都打到 /api/account-sessions/:id：PATCH 记账，
    // PUT 返回成功；其余 GET（providers/stats/connections 等）fallback 给
    // 更早注册的桩——后注册的 route 优先，宽 regex 会把 providers 吞成
    // data:{}（对象而非数组），页面渲染即崩（z.find is not a function）。
    const patches: Array<Record<string, unknown>> = [];
    await page.route(/\/api\/account-sessions\/[^/]+$/, (r) => {
      const method = r.request().method();
      if (method === 'PATCH') {
        patches.push(r.request().postDataJSON() as Record<string, unknown>);
        return r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ success: true, data: {} }) });
      }
      if (method === 'PUT') {
        return r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ success: true, data: {} }) });
      }
      return r.fallback();
    });

    await openEditDialog(page, 'e2e-limiter');

    // 空态：条件组区块空提示 + 旧规则导入入口 + 旧规则分隔可见
    await expect(page.locator('.as-limiter-empty')).toContainText('未添加时按下方旧规则判定');
    await expect(page.getByRole('button', { name: '从旧规则导入' })).toBeVisible();
    await expect(page.locator('.as-lg-divider')).toContainText('旧版限制规则');

    // 新增条件组 → 选本地计数指标 → 必须显式「暂未生效」警示（#29 硬性要求）
    await page.getByRole('button', { name: '新增条件组' }).click();
    await expect(page.locator('.as-lim-overlay .modal-header h3')).toContainText('新增条件组');
    await page.locator('.as-lg-field', { hasText: '读数指标' }).locator('select').selectOption({ label: 'Token 消耗（暂未生效）' });
    await expect(page.locator('.as-lg-warning')).toContainText('本地计数暂未生效');

    // 切回余额类指标，阈值 10，确定
    await page.locator('.as-lg-field', { hasText: '读数指标' }).locator('select').selectOption({ label: '剩余额度' });
    await page.locator('.as-lg-field', { hasText: '阈值' }).locator('input').fill('10');
    await page.locator('.as-lim-overlay .modal-footer button', { hasText: '确定' }).click();

    // 组卡片摘要（describeGroup）+ 导入入口随条件组存在而隐藏
    await expect(page.locator('.as-limiter-rule', { hasText: '跳过本轮 · 限定模型 · 1 条件' })).toBeVisible();
    await expect(page.getByRole('button', { name: '从旧规则导入' })).toHaveCount(0);
    await expect(page.locator('.as-lg-hint')).toContainText('调度判定以条件组为准');

    await page.getByRole('button', { name: '保存修改' }).click();
    await expect.poll(() => patches.length).toBe(1);
    // 形状锚点：camelCase 条件组整包断言（窗口未配置 = null；无旧规则键）
    expect(patches[0]).toEqual({
      limiter_groups: [
        {
          enabled: true,
          action: 'skip',
          target: 'models',
          conditions: [
            { join: 'or', sourceId: '', models: [], metric: 'remaining', comparison: 'lte', threshold: 10, window: null },
          ],
        },
      ],
    });
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('条件组回读：组卡片 + 优先级提示 + 旧规则并排展示', async ({ page, pageErrors }) => {
    await page.route(/\/api\/account-sessions(\?|$)/, (r) =>
      r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(SESSIONS_WITH_GROUP) }));

    await openEditDialog(page, 'e2e-groups');

    // 提交后详情可见：组卡片摘要（describeGroup：动作 · 目标 · 条件数）
    await expect(page.locator('.as-limiter-rule', { hasText: '跳过本轮 · 整号 · 2 条件' })).toBeVisible();
    await expect(page.locator('.as-lg-hint')).toContainText('下方旧规则不再参与');
    // 旧规则并排（双读兜底展示），但导入入口不再出现
    await expect(page.locator('.as-limiter-rule', { hasText: '余额 < 5' })).toBeVisible();
    await expect(page.getByRole('button', { name: '从旧规则导入' })).toHaveCount(0);
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('删除全部条件组：保存 PATCH 以空数组清空（回落扁平双读）', async ({ page, pageErrors }) => {
    await page.route(/\/api\/account-sessions(\?|$)/, (r) =>
      r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(SESSIONS_WITH_GROUP) }));
    const patches: Array<Record<string, unknown>> = [];
    await page.route(/\/api\/account-sessions\/[^/]+$/, (r) => {
      const method = r.request().method();
      if (method === 'PATCH') {
        patches.push(r.request().postDataJSON() as Record<string, unknown>);
        return r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ success: true, data: {} }) });
      }
      if (method === 'PUT') {
        return r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ success: true, data: {} }) });
      }
      return r.fallback();
    });

    await openEditDialog(page, 'e2e-groups');
    await page.locator('.as-limiter-rule button[title="删除"]').first().click();
    await expect(page.locator('.as-limiter-empty')).toBeVisible();

    await page.getByRole('button', { name: '保存修改' }).click();
    await expect.poll(() => patches.length).toBe(1);
    // 三态语义：空数组 = 清空（后端回落扁平 limiters 双读）
    expect(patches[0]).toEqual({ limiter_groups: [] });
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });
});
