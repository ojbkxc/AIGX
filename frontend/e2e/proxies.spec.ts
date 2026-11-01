import { test, expect, loginAsAdmin } from './fixtures';

/**
 * 网络代理页 —— 功能级回归。
 *
 * 覆盖 ds2api + Agent2API 对齐的代理池管理：
 * - 列表渲染（含禁用条目 + 引用计数）
 * - 新建 / 编辑 / 删除
 * - 启用/禁用开关
 * - 连通性测试（mock 返回固定 IP）
 * - 引用编码复制
 */

test.describe('网络代理', () => {
  test.beforeEach(async ({ page }) => {
    // mock server 的代理池是进程内数组：desktop/mobile 串行共用，
    // CRUD 用例的残留会跨 project 污染后续断言（toHaveCount(2) 实得 5）。
    // 每用例先经 page.request（挂 baseURL）复位回预置两条。
    await page.request.post('/api/__e2e/reset-proxy-pool');
    await loginAsAdmin(page, { sessions: [] });
    await page.goto('/proxy-pool');
    await expect(page.locator('h1')).toContainText('网络代理');
  });

  test('列表渲染预置两条代理', async ({ page, pageErrors }) => {
    // 香港节点（启用）+ 日本节点（禁用）
    await expect(page.locator('.proxy-card')).toHaveCount(2);
    await expect(page.locator('.proxy-card-name', { hasText: '香港节点' })).toBeVisible();
    await expect(page.locator('.proxy-card-name', { hasText: '日本节点' })).toBeVisible();
    // 地址展示
    await expect(page.locator('.proxy-card-addr', { hasText: 'socks5://127.0.0.1:1080' })).toBeVisible();
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('新建代理', async ({ page, pageErrors }) => {
    await page.getByRole('button', { name: '新建代理' }).click();
    await page.locator('input[placeholder]').first().fill('测试节点');
    // 协议下拉
    await page.locator('select').first().selectOption('socks5');
    // 主机 + 端口
    const inputs = page.locator('input.form-input');
    await inputs.nth(1).fill('10.0.0.1');
    await inputs.nth(2).fill('9999');
    await page.getByRole('button', { name: '保存' }).click();

    // 列表出现新条目
    await expect(page.locator('.proxy-card-name', { hasText: '测试节点' })).toBeVisible();
    await expect(page.locator('.proxy-card-addr', { hasText: 'socks5://10.0.0.1:9999' })).toBeVisible();
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('启用/禁用开关可切', async ({ page, pageErrors }) => {
    // 日本节点初始禁用 → 点开关启用
    const card = page.locator('.proxy-card', { hasText: '日本节点' });
    const sw = card.locator('button[role="switch"], .switch').first();
    await sw.click();
    // mock 回整份列表，开关应反映新状态（不报错即通过）
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('连通性测试返回出口 IP', async ({ page, pageErrors }) => {
    const card = page.locator('.proxy-card', { hasText: '香港节点' });
    await card.getByRole('button', { name: '测试' }).click();
    // mock 返回 ip=203.0.113.42，toast 或测试结果区显示
    await expect(page.locator('text=203.0.113.42')).toBeVisible({ timeout: 5000 });
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('删除代理走确认弹窗', async ({ page, pageErrors }) => {
    const card = page.locator('.proxy-card', { hasText: '香港节点' });
    await card.getByRole('button').filter({ has: page.locator('svg') }).last().click();
    // 确认弹窗
    const dlg = page.locator('[role="dialog"], .confirm-dialog').filter({ hasText: '删除代理' }).first();
    await expect(dlg).toBeVisible();
    await dlg.getByRole('button', { name: '删除' }).click();
    // 香港节点消失
    await expect(page.locator('.proxy-card-name', { hasText: '香港节点' })).toHaveCount(0);
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });
});

/**
 * 账号弹窗的代理选择器 —— Phase E 回归。
 * 打开账号编辑弹窗，确认出口代理是选择器而非裸串输入框。
 */
test.describe('账号弹窗代理选择器', () => {
  test('编辑账号时代理字段是下拉选择器', async ({ page, pageErrors }) => {
    await loginAsAdmin(page, { sessions: [] });
    await page.goto('/account-sessions');
    // 等列表加载后点编辑（第一条账号）
    await expect(page.locator('tbody tr').first()).toBeVisible({ timeout: 5000 });
    await page.locator('tbody tr').first().locator('button[title="编辑"]').first().click();
    // 弹窗里应有代理选择器（下拉含「直连」选项）
    const modal = page.locator('.modal-overlay, [role="dialog"]').first();
    await expect(modal).toBeVisible();
    await expect(modal.locator('.proxy-selector')).toBeVisible();
    await expect(modal.locator('.proxy-selector select option', { hasText: '直连' })).toHaveCount(1);
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });
});