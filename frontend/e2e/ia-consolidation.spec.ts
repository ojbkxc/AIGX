import { test, expect, loginAsAdmin } from './fixtures';
import type { Page } from '@playwright/test';

/**
 * 信息架构整合（IA v2）—— 验收回归。
 *
 * 覆盖 docs/IA-CONSOLIDATION-SPEC.md §7.2 的落点：
 * - 侧栏整合：上游渠道与账号池合并为「供应商」；签到中心 / 用户工具权限 /
 *   定时任务 / 系统设置「账户」顶级入口下线。
 * - 旧路由重定向：/account-sessions、/checkin、/user-tools、/scheduled-tasks、
 *   /settings?tab=account、/model-manage?tab=meta。
 * - 供应商页：h1 + 4 Tab（供应商/账号池/健康/签到，无「网关」）、旧 tab= 别名归一。
 * - 统一供应商列表：形态列（API Key / 账号池）、账号数列、账号池型行「管理账号」。
 * - 用户管理双 Tab；系统设置分类（有「定时任务」无「账户」）;模型管理双 Tab +
 *   「覆盖管理」弹窗入口；签到嵌入后保留 4 个二级 Tab。
 *
 * 选择器一律走语义（role / title / 稳定文案），不依赖易变 class 名。
 */

// 供应商页 h1（Gateways 的 page-header，embedded 的 Channels 不再出 h1）
const gwTitle = (page: Page) => page.locator('.gw-header h1');
// 侧栏（桌面常驻 + 移动抽屉各一实例，取第一个即可读文案；两态 DOM 均在）
const sidebar = (page: Page) => page.locator('.sidebar-aside').first();

test.describe('IA 整合 · 侧栏与路由', () => {
  test.beforeEach(async ({ page }) => {
    await loginAsAdmin(page, { sessions: [] });
  });

  test('侧栏已整合：无旧菜单项，保留供应商/用户管理/系统设置/模型管理', async ({ page, pageErrors }) => {
    await page.goto('/');
    await expect(page.locator('.main-content')).toBeVisible();

    const nav = sidebar(page);
    for (const gone of ['渠道与账号池', '签到中心', '用户工具权限', '定时任务']) {
      await expect(nav).not.toContainText(gone);
    }
    for (const kept of ['供应商', '用户管理', '系统设置', '模型管理']) {
      await expect(nav).toContainText(kept);
    }

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('旧路由重定向到归并后的落点', async ({ page, pageErrors }) => {
    await page.goto('/account-sessions');
    await expect(page).toHaveURL(/\/channels\?tab=accounts/);

    await page.goto('/checkin');
    await expect(page).toHaveURL(/\/channels\?tab=checkin/);

    await page.goto('/user-tools');
    await expect(page).toHaveURL(/\/users\?tab=tools/);

    await page.goto('/scheduled-tasks');
    await expect(page).toHaveURL(/\/settings\?tab=scheduled/);

    await page.goto('/settings?tab=account');
    await expect(page).toHaveURL(/\/profile$/);

    await page.goto('/model-manage?tab=meta');
    await expect(page).toHaveURL(/\/model-manage\?tab=catalog/);

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });
});

test.describe('IA 整合 · 供应商页', () => {
  test.beforeEach(async ({ page }) => {
    await loginAsAdmin(page, { sessions: [] });
  });

  test('/channels h1 为供应商，恰好 4 个 Tab 且无「网关」', async ({ page, pageErrors }) => {
    await page.goto('/channels');
    await expect(gwTitle(page)).toHaveText('供应商');

    await expect(page.getByRole('tab')).toHaveCount(4);
    for (const name of ['供应商', '账号池', '健康', '签到']) {
      await expect(page.getByRole('tab', { name })).toHaveCount(1);
    }
    await expect(page.getByRole('tab', { name: '网关' })).toHaveCount(0);

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('旧 tab= 别名 upstream / gateway 归一为供应商 Tab', async ({ page, pageErrors }) => {
    for (const alias of ['upstream', 'gateway']) {
      await page.goto(`/channels?tab=${alias}`);
      // 供应商 Tab 选中 + 标题在
      await expect(page.getByRole('tab', { name: '供应商' })).toHaveAttribute('aria-selected', 'true');
      await expect(gwTitle(page)).toHaveText('供应商');
      // URL 归一：tab 参数被清掉（replace），不再残留别名
      await expect(page).toHaveURL(/\/channels(\?|$)/);
      await expect(page).not.toHaveURL(/tab=/);
    }
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('统一列表：形态列 + 账号数，账号池型行有「管理账号」、API Key 型没有', async ({ page, pageErrors }) => {
    // 两条样本：一条 API Key 型 + 一条 channel_type='account'（账号池容器）
    await page.route(/\/api\/channels(\?.*)?$/, (route) => {
      if (route.request().method() !== 'GET') return route.continue();
      const data = [
        {
          id: 'ch-key-1', seq: 1, name: 'e2e-key-supplier', channel_type: 'openai_compatible',
          base_url: 'https://key.example.com/v1', api_key: 'sk-e2e-1', priority: 10, weight: 1,
          status: 'enabled', models: ['glm-4'], model_mapping: {}, response_time: 200, test_time: 0,
          balance_query_interval_secs: null, balance_last_query_at: null, balance_last_error: null,
        },
        {
          id: 'ch-pool-1', seq: 2, name: 'e2e-account-pool', channel_type: 'account',
          base_url: '', api_key: null, priority: 0, weight: 1,
          status: 'enabled', models: [], response_time: 0, test_time: 0,
          balance_query_interval_secs: null, balance_last_query_at: null, balance_last_error: null,
        },
      ];
      return route.fulfill({
        status: 200,
        contentType: 'application/json; charset=utf-8',
        body: JSON.stringify({ success: true, data, total: data.length, page: 1, size: data.length }),
      });
    });
    // 账号数（账号池型行的 enabled/total）
    await page.route(/\/api\/account-sessions\/stats(\?.*)?$/, (route) =>
      route.fulfill({
        status: 200,
        contentType: 'application/json; charset=utf-8',
        body: JSON.stringify({
          success: true,
          data: {
            channels: [
              { channel_id: 'ch-pool-1', channel_name: 'e2e-account-pool', total: 3, enabled: 2, cooling: 0, in_flight: 0, success: 0, error: 0 },
            ],
            total: 1,
          },
        }),
      }),
    );

    await page.goto('/channels');

    // 两行都在（账号池型不再被前端过滤）
    const rows = page.locator('.channels-table tbody tr');
    await expect(rows).toHaveCount(2);
    const poolRow = rows.filter({ hasText: 'e2e-account-pool' });
    const keyRow = rows.filter({ hasText: 'e2e-key-supplier' });
    await expect(poolRow).toHaveCount(1);
    await expect(keyRow).toHaveCount(1);

    // 表头含「形态」「账号数」
    const head = page.locator('.channels-table thead');
    await expect(head).toContainText('形态');
    await expect(head).toContainText('账号数');

    // 形态列文案
    await expect(poolRow.locator('.ch-kind-badge')).toContainText('账号池');
    await expect(keyRow.locator('.ch-kind-badge')).toContainText('API Key');

    // 账号数列：账号池型 2/3，API Key 型占位 —
    await expect(poolRow.locator('.col-accounts')).toHaveText('2/3');
    await expect(keyRow.locator('.col-accounts')).toHaveText('—');

    // 行操作：账号池型有「管理账号」，API Key 型没有
    await expect(poolRow.getByTitle('管理账号')).toHaveCount(1);
    await expect(keyRow.getByTitle('管理账号')).toHaveCount(0);
    await expect(page.getByTitle('管理账号')).toHaveCount(1);

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('签到 Tab 嵌入后保留 4 个二级 Tab', async ({ page, pageErrors }) => {
    await page.goto('/channels?tab=checkin');
    const subTabs = page.locator('.ck-sub-tabs .sub-tab');
    await expect(subTabs).toHaveCount(4);
    for (const name of ['签到中心', '定时签到', '签到记录', '福利通道']) {
      await expect(page.locator('.ck-sub-tabs .sub-tab', { hasText: name })).toHaveCount(1);
    }
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('供应商页无横向溢出（含 4 Tab）', async ({ page }) => {
    await page.goto('/channels');
    await expect(page.getByRole('tab')).toHaveCount(4);
    const overflow = await page.evaluate(() => {
      const de = document.documentElement;
      return de.scrollWidth - de.clientWidth;
    });
    expect(overflow).toBeLessThanOrEqual(1);
  });

  test('证据截图：供应商统一列表（desktop + 390px）', async ({ page }, testInfo) => {
    await page.route(/\/api\/channels(\?.*)?$/, (route) => {
      if (route.request().method() !== 'GET') return route.continue();
      const data = [
        {
          id: 'ch-key-1', seq: 1, name: 'e2e-key-supplier', channel_type: 'openai_compatible',
          base_url: 'https://key.example.com/v1', api_key: 'sk-e2e-1', priority: 10, weight: 1,
          status: 'enabled', models: ['glm-4'], model_mapping: {}, response_time: 200, test_time: 0,
          balance_query_interval_secs: null, balance_last_query_at: null, balance_last_error: null,
        },
        {
          id: 'ch-pool-1', seq: 2, name: 'e2e-account-pool', channel_type: 'account',
          base_url: '', api_key: null, priority: 0, weight: 1,
          status: 'enabled', models: [], response_time: 0, test_time: 0,
          balance_query_interval_secs: null, balance_last_query_at: null, balance_last_error: null,
        },
      ];
      return route.fulfill({
        status: 200,
        contentType: 'application/json; charset=utf-8',
        body: JSON.stringify({ success: true, data, total: data.length, page: 1, size: data.length }),
      });
    });
    await page.goto('/channels');
    await expect(page.locator('.channels-table tbody tr')).toHaveCount(2);
    await page.screenshot({ path: `.e2e-shots/ia-supplier-${testInfo.project.name}.png`, fullPage: true });
  });
});

test.describe('IA 整合 · 用户管理 / 系统设置 / 模型管理', () => {
  test.beforeEach(async ({ page }) => {
    await loginAsAdmin(page, { sessions: [] });
  });

  test('用户管理双 Tab：用户列表 / 工具权限，?tab=tools 直达工具 Tab', async ({ page, pageErrors }) => {
    await page.goto('/users');
    await expect(page.getByRole('tab')).toHaveCount(2);
    await expect(page.getByRole('tab', { name: '用户列表' })).toHaveAttribute('aria-selected', 'true');
    await expect(page.getByRole('tab', { name: '工具权限' })).toHaveCount(1);

    await page.goto('/users?tab=tools');
    await expect(page.getByRole('tab', { name: '工具权限' })).toHaveAttribute('aria-selected', 'true');

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('系统设置左栏含「定时任务」不含「账户」，?tab=scheduled 直达', async ({ page, pageErrors }) => {
    await page.goto('/settings');
    const nav = page.locator('.settings-nav');
    await expect(nav).toBeVisible();
    await expect(nav).toContainText('定时任务');
    await expect(nav).not.toContainText('账户');

    await page.goto('/settings?tab=scheduled');
    await expect(page.locator('.settings-nav-item.active')).toContainText('定时任务');

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('模型管理双 Tab；?tab=meta 回落目录；「覆盖管理」入口可开弹窗', async ({ page, pageErrors }) => {
    await page.goto('/model-manage');
    await expect(page.getByRole('tab')).toHaveCount(2);
    await expect(page.getByRole('tab', { name: '模型目录' })).toHaveCount(1);
    await expect(page.getByRole('tab', { name: '定价倍率' })).toHaveCount(1);
    await expect(page.getByRole('tab', { name: '元信息覆盖' })).toHaveCount(0);

    // 旧 ?tab=meta 回落目录
    await page.goto('/model-manage?tab=meta');
    await expect(page).toHaveURL(/\/model-manage\?tab=catalog/);
    await expect(page.getByRole('tab', { name: '模型目录' })).toHaveAttribute('aria-selected', 'true');

    // 「覆盖管理」入口存在且能打开弹窗
    await page.getByRole('button', { name: '覆盖管理' }).click();
    await expect(page.locator('.mm-overrides-modal')).toBeVisible();

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });
});
