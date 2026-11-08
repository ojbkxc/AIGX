import { test, expect, loginAsAdmin } from './fixtures';

/**
 * 系统设置 —— 分类导航回归。
 *
 * 对齐 Agent2API 的 settings-page 分类：超时与重试从原来塞在「站点 → API 配置」
 * 一张卡里拆成独立分类，管理员不必翻长卡片找参数。
 */

/** 左栏分类导航 */
const nav = (page: import('@playwright/test').Page) => page.locator('.settings-nav');

test.describe('系统设置分类', () => {
  test.beforeEach(async ({ page }) => {
    await loginAsAdmin(page, { sessions: [] });
    await page.goto('/settings');
    await expect(page.locator('.settings-layout')).toBeVisible();
  });

  test('左栏渲染全部分类，默认落在通用', async ({ page, pageErrors }) => {
    await expect(nav(page)).toBeVisible();
    // 「路由」已独立成页 /routing（#40 分层档案 + 预演 + 回放 + 粘滞），不再在此列
    for (const label of ['通用', 'AI 助手', '网关', '重试', '超时', '计费', '数据', '通知', '安全', '更新', '账户']) {
      await expect(nav(page).locator('.settings-nav-item', { hasText: label })).toHaveCount(1);
    }
    // 默认分类高亮，右栏是通用（使用限额 + 主题）
    await expect(page.locator('.settings-nav-item.active')).toContainText('通用');
    await expect(page.locator('.settings-pane')).toContainText('使用限额');
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('超时分类承载五个超时窗口，不再挤在通用里', async ({ page, pageErrors }) => {
    // 通用分类里不应再出现超时/重试字段
    await expect(page.locator('.settings-pane')).not.toContainText('请求总超时');

    await nav(page).locator('.settings-nav-item', { hasText: '超时' }).click();
    const pane = page.locator('.settings-pane');
    await expect(pane).toContainText('请求总超时');
    await expect(pane).toContainText('连接超时');
    await expect(pane).toContainText('响应头等待');
    await expect(pane).toContainText('流空闲超时');
    await expect(pane).toContainText('非流式响应体超时');
    // 五个数值输入框
    await expect(pane.locator('input[type=number]')).toHaveCount(5);
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('重试分类四个字段，含状态码文本框', async ({ page, pageErrors }) => {
    await nav(page).locator('.settings-nav-item', { hasText: '重试' }).click();
    const pane = page.locator('.settings-pane');
    await expect(pane).toContainText('最大重试次数（渠道切换）');
    await expect(pane).toContainText('同账号重试次数');
    await expect(pane).toContainText('重试间隔');
    await expect(pane).toContainText('直接换号/换渠道状态码');
    // 状态码是文本输入（逗号分隔），不是数字框
    await expect(pane.locator('input[type=text]')).toHaveCount(1);
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('重试分类保存打到 /api/limits', async ({ page, pageErrors }) => {
    const calls: string[] = [];
    page.on('request', (r) => {
      if (r.url().includes('/api/limits')) calls.push(r.method());
    });

    await nav(page).locator('.settings-nav-item', { hasText: '重试' }).click();
    await page.locator('.settings-pane input[type=number]').first().fill('3');
    await page.locator('.settings-pane .btn-primary', { hasText: '保存更改' }).click();

    await expect.poll(() => calls.filter((m) => m === 'PUT' || m === 'POST').length, { timeout: 5000 }).toBeGreaterThan(0);
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('网关 / 数据 / 计费分类各自渲染内容', async ({ page, pageErrors }) => {
    await nav(page).locator('.settings-nav-item', { hasText: '网关' }).click();
    await expect(page.locator('.settings-pane')).toContainText('请求体大小上限');

    // 「数据」在导航里唯一，但「数据」二字也会命中侧边栏分组名，
    // 用 exact 精确匹配导航项自身的文本
    await nav(page).getByRole('button', { name: '数据', exact: true }).click();
    await expect(page.locator('.settings-pane')).not.toContainText('请求体大小上限');
    const text = await page.locator('.settings-pane').innerText();
    expect(text.length).toBeGreaterThan(10);

    await nav(page).getByRole('button', { name: '计费', exact: true }).click();
    await expect(page.locator('.settings-pane')).toContainText('按次计费');
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });
});
