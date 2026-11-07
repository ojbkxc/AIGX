import { test, expect, loginAsAdmin } from './fixtures';

/**
 * 添加账号五档弹窗常规回归（固化 06-verify 的移交项 #4）。
 *
 * 上一轮交付五档重构时，static-server.mjs 桩里 /api/account-sessions/providers
 * 落 EMPTY_LIST，五档弹窗无常规回归用例（当轮验收用临时 spec，已删）。本文件
 * 固化那批用例：providers 桩带 category 三值（agent/web/api），渠道桩带 account 型。
 *
 * 覆盖：
 * - 五档 tab（全部/Agent/Web/API 模板/已建 API）与「全部」档分组带渲染
 * - Web 档按 category==='web' 渲染 deepseek-web / chatglm 卡
 * - 「已建 API」档列出 account 型渠道卡 + 常驻「新建自定义提供商」卡
 * - 协议下拉单 Select 三选项；切 Anthropic 后 Base URL 提示变「不要带 /v1」
 */

test.describe('添加账号五档弹窗', () => {
  test.beforeEach(async ({ page }) => {
    await loginAsAdmin(page);
    await page.goto('/channels');
    await page.getByRole('tab', { name: '账号池' }).click();
    await expect(page.locator('.as-table')).toBeVisible({ timeout: 10000 });
    // 打开添加账号弹窗（第 1 步 = 选供应商分档）
    await page.locator('button', { hasText: '添加账号' }).first().click();
    const pick = page.locator('.add-modal-wide');
    await expect(pick).toBeVisible();
  });

  test('五档 tab 与「全部」档分组带渲染', async ({ page, pageErrors }) => {
    const tabs = page.locator('.add-modal-wide .ui-tabs');
    for (const label of ['全部', 'Agent', 'Web', 'API 模板', '已建 API']) {
      await expect(tabs.getByRole('tab', { name: label, exact: true })).toBeVisible();
    }

    // 「全部」档：四个分组带（Agent / Web / API 模板 / 已建 API）
    const bands = page.locator('.add-card-band');
    await expect(bands).toHaveCount(4);
    await expect(page.locator('.add-card-band-name').filter({ hasText: 'Agent' })).toBeVisible();
    await expect(page.locator('.add-card-band-name').filter({ hasText: 'Web' })).toBeVisible();
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('Web 档按 category=web 渲染 deepseek-web / chatglm 卡', async ({ page, pageErrors }) => {
    await page.getByRole('tab', { name: 'Web', exact: true }).click();
    const cards = page.locator('.add-cards .add-card');
    // 桩里 web 形态两家
    await expect(cards).toHaveCount(2);
    await expect(page.locator('.add-card-name', { hasText: 'DeepSeek 网页版' })).toBeVisible();
    await expect(page.locator('.add-card-name', { hasText: '智谱清言' })).toBeVisible();
    // 单档视图无分组带
    await expect(page.locator('.add-card-band')).toHaveCount(0);
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('Agent 档按 category=agent 渲染登录态家（带登录态徽章）', async ({ page, pageErrors }) => {
    await page.getByRole('tab', { name: 'Agent', exact: true }).click();
    const cards = page.locator('.add-cards .add-card');
    // 桩里 agent 形态两家（WorkBuddy 国内版 / Qoder）
    await expect(cards).toHaveCount(2);
    await expect(page.locator('.add-card-name', { hasText: 'WorkBuddy 国内版' })).toBeVisible();
    await expect(page.locator('.add-card-badge', { hasText: '登录态' }).first()).toBeVisible();
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('「已建 API」档列出账号池卡 + 常驻「新建自定义提供商」卡', async ({ page, pageErrors }) => {
    await page.getByRole('tab', { name: '已建 API', exact: true }).click();
    // 常驻新建卡
    await expect(page.locator('.add-card.is-new', { hasText: '新建自定义提供商' })).toBeVisible();
    // 桩账号池里 API 型渠道卡（lxchat-glm 渠道承载 openai_compatible 账号）
    await expect(page.locator('.add-card', { hasText: 'lxchat-glm' })).toBeVisible();
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('搜索跨档命中与「看全部匹配」提示', async ({ page, pageErrors }) => {
    // 在「Agent」档搜 web 家的名字：本档无命中，提示其它分类有匹配
    await page.getByRole('tab', { name: 'Agent', exact: true }).click();
    await page.locator('.add-search input').fill('deepseek');
    await expect(page.locator('.add-cross-hint')).toBeVisible();
    await expect(page.locator('.add-cross-hint')).toContainText('其它分类');
    // 点「看全部匹配」切到全部档
    await page.locator('.add-cross-hint button', { hasText: '看全部匹配' }).click();
    await expect(page.locator('.add-card-name', { hasText: 'DeepSeek 网页版' })).toBeVisible();
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('新建自定义提供商：协议下拉三选项，切 Anthropic 后 Base URL 提示变化', async ({ page, pageErrors }) => {
    // 「已建 API」档第一张虚线卡进表单
    await page.getByRole('tab', { name: '已建 API', exact: true }).click();
    await page.locator('.add-card.is-new', { hasText: '新建自定义提供商' }).click();
    const form = page.locator('.add-modal-form');
    await expect(form).toBeVisible();

    // 协议下拉（aria-label=协议）：三选项
    const protocolSelect = form.getByLabel('协议');
    await expect(protocolSelect).toBeVisible();
    await expect(protocolSelect.locator('option', { hasText: 'OpenAI 兼容' })).toHaveCount(1);
    await expect(protocolSelect.locator('option', { hasText: 'Anthropic 兼容' })).toHaveCount(1);
    await expect(protocolSelect.locator('option', { hasText: 'Gemini 兼容' })).toHaveCount(1);

    // 切 Anthropic：Base URL 提示变「不要带 /v1」（根地址口径）
    await protocolSelect.selectOption('anthropic');
    await expect(form).toContainText('不要带 /v1');
    // 切回 OpenAI：提示回到「填到 /v1」的口径
    await protocolSelect.selectOption('openai_compatible');
    await expect(form).toContainText('/v1');
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });
});
