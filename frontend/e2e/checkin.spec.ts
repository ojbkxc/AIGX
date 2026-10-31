import { test, expect, loginAsAdmin } from './fixtures';

/**
 * 签到中心 —— 功能级回归。
 *
 * 这是「代账号池里的上游账号去签到」的管理面（与钱包页的用户签到不是一回事）：
 * - 顶部统计（今日完成 / 可签到账号 / 支持的上游 / 定时签到）
 * - 按 provider 分组列账号，已签/未签徽章
 * - 「立即全部签到」「签到本组」「单账号签到」都打对应的运行端点
 * - 签到记录 Tab 渲染台账，结果按 outcome 着色
 * - 定时签到 Tab 有总开关与间隔
 */

test.describe('签到中心', () => {
  test.beforeEach(async ({ page }) => {
    await loginAsAdmin(page, { sessions: [] });
    await page.goto('/checkin');
    await expect(page.locator('h1')).toContainText('签到中心');
  });

  test('顶部统计与分组账号表渲染', async ({ page, pageErrors }) => {
    // 四张统计卡：今日完成 1/2、可签到账号 2、支持上游 2、定时签到 1
    await expect(page.locator('.ck-stat')).toHaveCount(4);
    const statsText = await page.locator('.ck-stats').innerText();
    expect(statsText).toContain('今日完成');
    expect(statsText).toContain('可签到账号');
    expect(statsText).toContain('支持的上游');

    // provider 分组标题带 已签/总数
    await expect(page.locator('.ck-table')).toHaveCount(1);
    await expect(page.locator('.main-content')).toContainText('WorkBuddy 国内版');

    // 两个账号行，一个已签一个未签
    const rows = page.locator('.ck-table tbody tr');
    await expect(rows).toHaveCount(2);
    await expect(rows.nth(0)).toContainText('已签');
    await expect(rows.nth(1)).toContainText('未签');
    await expect(rows.nth(0)).toContainText('签到成功 +100');

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('单账号签到请求打到 /api/checkin-center/run', async ({ page, pageErrors }) => {
    const calls: Array<{ url: string; body: string }> = [];
    page.on('request', (r) => {
      if (r.url().includes('/api/checkin-center/run')) {
        calls.push({ url: r.url(), body: r.postData() ?? '' });
      }
    });

    const row = page.locator('.ck-table tbody tr', { hasText: 'workbuddy-2' });
    await row.getByRole('button', { name: '签到' }).click();

    await expect.poll(() => calls.length, { timeout: 5000 }).toBeGreaterThan(0);
    // 单账号签到的请求体里应带该账号 id
    expect(calls[0].body).toContain('acc-2');
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('立即全部签到请求可发出', async ({ page, pageErrors }) => {
    const calls: string[] = [];
    page.on('request', (r) => {
      if (r.url().includes('/api/checkin-center/run')) calls.push(r.postData() ?? '');
    });

    await page.getByRole('button', { name: /立即全部签到/ }).click();
    await expect.poll(() => calls.length, { timeout: 5000 }).toBeGreaterThan(0);
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('签到记录 Tab 渲染台账并按结果着色', async ({ page, pageErrors }) => {
    await page.locator('.sub-tab', { hasText: '签到记录' }).click();

    const rows = page.locator('.ck-table tbody tr');
    await expect(rows).toHaveCount(2);
    // 成功与「今日已签到」两条
    await expect(page.locator('.main-content')).toContainText('签到成功 +100');
    await expect(page.locator('.main-content')).toContainText('今日已签到');
    // 来源列区分 手动 / 定时
    await expect(rows.nth(0)).toContainText('手动');
    await expect(rows.nth(1)).toContainText('定时');

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('定时签到 Tab 有总开关与间隔设置', async ({ page, pageErrors }) => {
    await page.locator('.sub-tab', { hasText: '定时签到' }).click();
    await expect(page.locator('.ck-setting-row').first()).toBeVisible();
    await expect(page.locator('.main-content')).toContainText('启用定时签到');
    await expect(page.locator('.main-content')).toContainText('账号间隔');
    // 至少一个开关控件
    await expect(page.getByRole('switch').first()).toBeVisible();
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });
});
