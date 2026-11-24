import { test, expect, loginAsAdmin } from './fixtures';

/**
 * 签到中心 —— 功能级回归。
 *
 * v3.5.1 对齐后页面是 A2A 的四任务分区打头（每日签到/新手福利/活动福利/
 * 成长任务）+ 三个二级页（定时签到/定时补签/签到记录）+ AIGX 保留的福利通道：
 * - 顶部四张统计卡（今日签到 / 定时签到 / 待领新手福利 / 累计签到记录）
 * - 分区内左 rail 列 provider（含「全部提供商」），右 main 是行列表
 * - 状态三档筛选（全部/已签到/未签到，带计数）
 * - 「立即全部签到」「单账号签到」都打 /api/checkin-center/run
 * - 签到记录页仍是表格（.ck-table），结果按 outcome 着色
 */

test.describe('签到中心', () => {
  test.beforeEach(async ({ page }) => {
    await loginAsAdmin(page, { sessions: [] });
    await page.goto('/checkin');
    // 新 IA：/checkin 重定向到供应商页「签到」Tab（embedded 无 h1），
    // 用签到页自己的二级 Tab 作为就绪信号（standalone 与 embedded 通用）。
    // v3.5.1 起首 Tab 由「签到中心」改为「每日签到」（四任务分区打头）。
    await expect(page.locator('.ck-sub-tabs .sub-tab', { hasText: '每日签到' })).toBeVisible();
  });

  test('顶部统计与分区行列表渲染', async ({ page, pageErrors }) => {
    // 四张统计卡（v3.5.1 口径：今日签到 / 定时签到 / 待领新手福利 / 累计签到记录）
    await expect(page.locator('.ck-stat')).toHaveCount(4);
    const statsText = await page.locator('.ck-stats').innerText();
    expect(statsText).toContain('今日签到');
    expect(statsText).toContain('定时签到');
    expect(statsText).toContain('累计签到记录');

    // 左 rail：除「全部提供商」外应有 WorkBuddy 国内版这一项
    const rail = page.locator('.ck-provider-rail');
    await expect(rail).toBeVisible();
    await expect(rail.locator('.ck-rail-item', { hasText: 'WorkBuddy 国内版' })).toHaveCount(1);

    // 右 main 渲染两个账号行，一个已签一个待签（行内文案来自 row 的状态徽标）
    const main = page.locator('.ck-center-main');
    await expect(main).toBeVisible();
    await expect(main).toContainText('workbuddy-1');
    await expect(main).toContainText('workbuddy-2');

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('单账号签到请求打到 /api/checkin-center/run', async ({ page, pageErrors }) => {
    const calls: Array<{ url: string; body: string }> = [];
    page.on('request', (r) => {
      if (r.url().includes('/api/checkin-center/run')) {
        calls.push({ url: r.url(), body: r.postData() ?? '' });
      }
    });

    // v3.5.1 起分区默认展示全部账号，不需要先展开 provider 分组
    const main = page.locator('.ck-center-main');
    const row = main.locator('div', { hasText: 'workbuddy-2' }).last();
    await row.getByRole('button', { name: '签到' }).first().click();

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
    // v3.5.1 起定时页是「总开关卡 + 计划卡 + 执行规则卡」，不再是单张设置行
    await expect(page.locator('.main-content')).toContainText('启用定时签到');
    await expect(page.locator('.main-content')).toContainText('账号间隔');
    // 至少一个开关控件
    await expect(page.getByRole('switch').first()).toBeVisible();
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });
});
