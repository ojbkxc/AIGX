import { test, expect, loginAsAdmin } from './fixtures';

/**
 * 用户工具权限管理面（#65；UserTools.tsx × §5.1 三端点 mock 冒烟）。
 *
 * 后端端点（GET/PUT /api/user-tools/policy + GET /api/user-tools/catalog）
 * 由 be-perm-impl 后续批次交付；本 spec 全部 page.route mock，验证：
 * - 404 降级（后端未就绪空态，不崩）
 * - 正常渲染：全局开关 / 组下拉 / 工具矩阵（HighRisk 与无 user_exec 行禁用）
 * - ["*"] 通配切换与勾选写 allowlist
 * - 保存 PUT body（全量 policy 形状）与 409 超限文案透传
 *
 * 精确路径分流：/api/user-tools/policy(\?|$) 与 /api/user-tools/catalog(\?|$)。
 */

const POLICY = {
  success: true,
  data: {
    enabled: false,
    groups: { vip: { allowlist: ['aigx_checkin_run'] } },
  },
};

const CATALOG = {
  success: true,
  data: {
    tools: [
      { name: 'aigx_checkin_run', label: '执行签到', risk: 'LowRisk', has_user_exec: true, policy_hit: true },
      { name: 'aigx_query_self_usage', label: '查我的用量', risk: 'ReadOnly', has_user_exec: true },
      { name: 'aigx_query_logs', label: '查全部日志', risk: 'ReadOnly', has_user_exec: false },
      { name: 'aigx_channel_delete', label: '删除渠道', risk: 'HighRisk', has_user_exec: false },
    ],
  },
};

const GROUPS = {
  success: true,
  data: [
    { id: 1, name: 'default' },
    { id: 2, name: 'vip' },
  ],
};

async function mockAll(page: import('@playwright/test').Page, opts?: { policyStatus?: number }): Promise<void> {
  await page.route(/\/api\/user-tools\/policy(\?|$)/, async (r) => {
    if (r.request().method() === 'PUT') {
      const body = r.request().postDataJSON() as Record<string, unknown>;
      await r.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify({ success: true, data: body }),
      });
      return;
    }
    const status = opts?.policyStatus ?? 200;
    await r.fulfill({
      status,
      contentType: 'application/json',
      body: JSON.stringify(status === 200 ? POLICY : { success: false }),
    });
  });
  await page.route(/\/api\/user-tools\/catalog(\?|$)/, (r) =>
    r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(CATALOG) }));
  await page.route(/\/api\/groups(\?|$)/, (r) =>
    r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ success: true, data: GROUPS.data }) }));
}

test.describe('用户工具权限管理面', () => {
  test('404 降级：后端未就绪空态，不崩', async ({ page, pageErrors }) => {
    await loginAsAdmin(page);
    await mockAll(page, { policyStatus: 404 });
    await page.goto('/user-tools');
    await expect(page.locator('.empty-state')).toContainText('尚未就绪');
    // mock 404 的资源加载 console 噪音是预期副产物；只断言无 JS 异常
    const jsErrors = pageErrors.filter((e) => !e.startsWith('console: Failed to load resource'));
    expect(jsErrors, jsErrors.join('\n')).toEqual([]);
  });

  test('矩阵渲染：HighRisk/无 user_exec 行禁用，vip 组预置勾选', async ({ page, pageErrors }) => {
    await loginAsAdmin(page);
    await mockAll(page);
    await page.goto('/user-tools');

    // 全局开关默认关（POLICY.enabled=false）
    const switches = page.locator('.ut-switch-title input[type="checkbox"], .ut-switch-title button');
    await expect(page.locator('.ut-hint', { hasText: '逐字节等价' })).toBeVisible();
    void switches;

    // 选 vip 组（预置 allowlist 含 aigx_checkin_run）
    await page.locator('select.ut-group-select').selectOption('vip');
    const rows = page.locator('.ut-matrix-row');
    await expect(rows).toHaveCount(4);
    // checkin：LowRisk + user_exec + 预置命中 → 可勾且已勾
    const checkin = page.locator('.ut-matrix-row', { hasText: 'aigx_checkin_run' });
    await expect(checkin.locator('input[type="checkbox"]')).toBeChecked();
    await expect(checkin.locator('.ut-hit')).toContainText('当前命中');
    // HighRisk 行禁用 + 高危标注
    const high = page.locator('.ut-matrix-row', { hasText: 'aigx_channel_delete' });
    await expect(high.locator('input[type="checkbox"]')).toBeDisabled();
    await expect(high.locator('.ut-flag.danger')).toContainText('永不暴露');
    // 无 user_exec 行禁用
    const noExec = page.locator('.ut-matrix-row', { hasText: 'aigx_query_logs' });
    await expect(noExec.locator('input[type="checkbox"]')).toBeDisabled();

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('通配切换 + 勾选写 allowlist + 保存 PUT body', async ({ page, pageErrors }) => {
    const putBodies: Array<Record<string, unknown>> = [];
    await loginAsAdmin(page);
    await mockAll(page);
    await page.route(/\/api\/user-tools\/policy(\?|$)/, async (r) => {
      if (r.request().method() === 'PUT') {
        const body = r.request().postDataJSON() as Record<string, unknown>;
        putBodies.push(body);
        await r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ success: true, data: body }) });
        return;
      }
      await r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(POLICY) });
    });
    await page.goto('/user-tools');
    await page.locator('select.ut-group-select').selectOption('default');

    // 勾选一个工具 → allowlist 写入
    const usage = page.locator('.ut-matrix-row', { hasText: 'aigx_query_self_usage' });
    await usage.locator('input[type="checkbox"]').check();

    // 开通全局开关 + 保存
    await page.locator('.ut-switch-title input[type="checkbox"], .ut-switch-title button').first().click();
    await page.locator('button', { hasText: '保存策略' }).click();

    await expect.poll(() => putBodies.length).toBeGreaterThanOrEqual(1);
    const body = putBodies[0] as { enabled?: boolean; groups?: Record<string, { allowlist: string[] }> };
    expect(body.enabled).toBe(true);
    expect(body.groups?.default?.allowlist).toContain('aigx_query_self_usage');
    // 保存成功回执
    await expect(page.locator('body')).toContainText('已保存');

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('409 超限：后端文案原样透传', async ({ page, pageErrors }) => {
    await loginAsAdmin(page);
    await mockAll(page);
    await page.route(/\/api\/user-tools\/policy(\?|$)/, async (r) => {
      if (r.request().method() === 'PUT') {
        await r.fulfill({
          status: 409,
          contentType: 'application/json',
          body: JSON.stringify({ success: false, message: '条件组数量超过上限' }),
        });
        return;
      }
      await r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(POLICY) });
    });
    await page.goto('/user-tools');
    await page.locator('select.ut-group-select').selectOption('default');
    await page.locator('button', { hasText: '保存策略' }).click();

    await expect(page.locator('.ut-error')).toContainText('条件组数量超过上限');
    // mock 409 的资源加载 console 噪音是预期副产物
    const jsErrors = pageErrors.filter((e) => !e.startsWith('console: Failed to load resource'));
    expect(jsErrors, jsErrors.join('\n')).toEqual([]);
  });
});
