import { test, expect, loginAsAdmin } from './fixtures';
import type { Page, Request } from '@playwright/test';

/**
 * API 密钥页 · 可用提供商白名单（`54d1f22d`）+ 密钥值长度下限（`8fe0581d`）
 * + 提供商列（同笔提交）。
 *
 * 为什么这四件事必须单独有 e2e：`static-server.mjs` 此前**没有 `/api/tokens` 桩**，
 * `/keys` 落到通用 EMPTY_LIST → 令牌表恒空。smoke 只证明「页面能渲染」，于是
 * 「提供商列渲染 / 白名单多选 / 勾了提供商后模型候选收窄 / 把白名单清空真的发
 * `[]` / 短密钥被拦」五条判据在 CI 里一条都没有。这一笔把桩补上，并按下述
 * **能区分新旧**的方式断言：
 *
 *  · 候选联动：勾 anthropic 后模型候选只能是 `claude-sonnet-4`（全量候选里有
 *    `gpt-4o`；不联动的实现会把它一起列出来 → 红）；
 *  · 提交体：创建时 `allowed_providers` 必须在 POST body 里（不是只在界面上选中）；
 *  · 清空：编辑时删光标签 → PUT 里必须出现 `allowed_providers: []`
 *    （后端 Some 即覆盖；「空就少发」会让用户以为清掉了、实际限制还在）；
 *  · 长度下限：`custom_key` 短于 8 个字符时**不得发出 POST**，并给出可读错误
 *    （改前是「创建看着成功、之后每次调用被上游当非法 key」）。
 */

/** 打开「创建令牌」弹窗 */
async function openCreate(page: Page): Promise<void> {
  await page.getByRole('button', { name: '+ 创建令牌' }).click();
  await expect(page.locator('.modal')).toBeVisible();
}

/** 弹窗里按标签取 MultiSelect（可用提供商 / 模型白名单两个同构控件） */
function multiSelectByLabel(page: Page, label: RegExp) {
  return page.locator('.form-group').filter({ has: page.locator('label', { hasText: label }) }).locator('.ms-wrap');
}

/** 往 MultiSelect 里选一个候选（点开 → 点候选按钮） */
async function pickOption(page: Page, ms: ReturnType<typeof multiSelectByLabel>, option: string): Promise<void> {
  await ms.locator('.ms-box').click();
  await ms.locator('.ms-item', { hasText: new RegExp(`^${option}$`) }).click();
}

/** 读某个 MultiSelect 的已选标签文本 */
async function chipsOf(ms: ReturnType<typeof multiSelectByLabel>): Promise<string[]> {
  return ms.locator('.ms-chip-label').allInnerTexts();
}

test.describe('API 密钥 · 提供商白名单', () => {
  // 令牌种子是进程内可变态，desktop / mobile 两个 project 串行共用同一个桩进程：
  // 「编辑把白名单删空」那条会把 tk-seed-1 的 allowed_providers 清成 null，
  // 后跑的 project 再断言「两个 provider chip」就红（实测 mobile 全量跑挂在
  // 第 46 行）。每条用例前复位预置态，与 proxies.spec 的 reset-proxy-pool 同口径。
  test.beforeEach(async ({ page }) => {
    await page.request.post('/api/__e2e/reset-tokens');
  });

  test('列表「提供商」列：空白名单显示「全部」，超过 2 家折叠成 +N', async ({ page, pageErrors }) => {
    await loginAsAdmin(page, { sessions: [] });
    await page.goto('/keys');
    await expect(page.locator('.main-content')).toContainText('API 密钥');

    // 桩三条：两家白名单 / null（不限）/ 三家白名单
    const row1 = page.locator('tbody tr', { hasText: 'CrewAI 生产' }).first();
    const row2 = page.locator('tbody tr', { hasText: 'LiteLLM 网关' }).first();
    const row3 = page.locator('tbody tr', { hasText: '三方混跑' }).first();
    await expect(row1).toBeVisible();

    await expect(row1.locator('.col-providers .tk-provider-badge')).toHaveText(['openai', 'anthropic']);
    await expect(row1.locator('.col-providers .tk-models-more')).toHaveCount(0);

    // null 白名单 = 不限：显示「全部」，不是空白也不是 0 项
    await expect(row2.locator('.col-providers .tk-models-all')).toHaveText('全部');

    // 三家 → 前 2 个 chip + “+1”，title 给完整清单（能看到第三家是谁）
    await expect(row3.locator('.col-providers .tk-provider-badge')).toHaveText(['openai', 'anthropic']);
    await expect(row3.locator('.col-providers .tk-models-more')).toHaveText('+1');
    await expect(row3.locator('.col-providers .tk-models')).toHaveAttribute('title', 'openai, anthropic, google');

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('创建：勾提供商后模型候选收窄到该家，提交体带 allowed_providers', async ({ page, pageErrors }) => {
    await loginAsAdmin(page, { sessions: [] });
    await page.goto('/keys');
    await openCreate(page);

    const providers = multiSelectByLabel(page, /可用提供商/);
    const models = multiSelectByLabel(page, /模型白名单/);

    // 未勾提供商：模型候选是网关全量（桩 /api/models/available 给 glm-4）
    await expect(models.locator('.ms-input')).toHaveAttribute('placeholder', /glm-5\.2/);
    await models.locator('.ms-box').click();
    await expect(models.locator('.ms-item')).toHaveText(['glm-4']);

    // 勾 anthropic：候选必须收窄到 modelsByProvider.anthropic（claude-sonnet-4），
    // 且**不得**再出现全量候选里的 gpt-4o（不联动的实现会两者都列）
    await pickOption(page, providers, 'anthropic');
    expect(await chipsOf(providers)).toEqual(['anthropic']);
    await expect(models.locator('.ms-box')).toBeVisible();
    await models.locator('.ms-box').click();
    await expect(models.locator('.ms-item')).toHaveText(['claude-sonnet-4']);
    await expect(models.locator('.ms-item', { hasText: 'gpt-4o' })).toHaveCount(0);
    await page.keyboard.press('Escape');

    // 填名称并提交，抓 POST body
    await page.locator('.modal input[placeholder="例如：开发环境令牌"]').fill('E2E 白名单令牌');
    const [post] = await Promise.all([
      page.waitForRequest((r: Request) => r.url().endsWith('/api/tokens') && r.method() === 'POST'),
      page.getByRole('button', { name: '创建', exact: true }).click(),
    ]);
    const body = post.postDataJSON() as Record<string, unknown>;
    expect(body.allowed_providers, '创建时白名单没进请求体').toEqual(['anthropic']);
    expect(body.name).toBe('E2E 白名单令牌');
    // 创建响应不带 plain_key 时不该弹一次性密钥框（桩回的是脱敏 key，页面留在表单）
    await expect(page.locator('.toast', { hasText: '令牌创建成功' })).toBeVisible();

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('编辑：把白名单删空 → PUT 里必须出现 allowed_providers: []（否则限制清不掉）', async ({ page, pageErrors }) => {
    await loginAsAdmin(page, { sessions: [] });
    await page.goto('/keys');
    const row = page.locator('tbody tr', { hasText: 'CrewAI 生产' }).first();
    await row.locator('button[title="编辑令牌"]').click();
    await expect(page.locator('.modal')).toBeVisible();

    const providers = multiSelectByLabel(page, /可用提供商/);
    // 回读到原白名单两家
    expect(await chipsOf(providers)).toEqual(['openai', 'anthropic']);

    // 逐个移除（点 chip 上的 ×）
    await providers.locator('.ms-chip-x').first().click();
    await providers.locator('.ms-chip-x').first().click();
    expect(await chipsOf(providers)).toEqual([]);

    const [put] = await Promise.all([
      page.waitForRequest((r: Request) => /\/api\/tokens\/tk-seed-1$/.test(r.url()) && r.method() === 'PUT'),
      page.getByRole('button', { name: '保存' }).click(),
    ]);
    const body = put.postDataJSON() as Record<string, unknown>;
    // 关键：空数组必须**显式**发出去。缺键会被后端当成「不修改」，白名单还在生效。
    expect(body, '清空白名单没有发 allowed_providers: []').toHaveProperty('allowed_providers');
    expect(body.allowed_providers).toEqual([]);

    // 桩侧回读同一份写入（R1：直打桩不如当场读回写侧留痕，确认落到了桩里）
    const lastWrite = await page.evaluate(async () => {
      const res = await fetch('/api/__e2e/token-last-write');
      return (await res.json()) as { data?: { method?: string; body?: Record<string, unknown> } };
    });
    expect(lastWrite.data?.method).toBe('PUT');
    expect(lastWrite.data?.body?.allowed_providers).toEqual([]);

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('密钥值短于 8 个字符：拦在提交前，不发 POST 且给可读错误', async ({ page, pageErrors }) => {
    await loginAsAdmin(page, { sessions: [] });
    await page.goto('/keys');

    const posts: string[] = [];
    page.on('request', (r) => {
      if (r.url().endsWith('/api/tokens') && r.method() === 'POST') posts.push(r.url());
    });

    await openCreate(page);
    await page.locator('.modal input[placeholder="例如：开发环境令牌"]').fill('短密钥令牌');
    // 标签里写明「至少 8 个字符」；这里填 7 个字符
    const keyInput = page.locator('.modal input[placeholder="sk-..."]');
    await expect(keyInput).toBeVisible();
    await keyInput.fill('sk-1234');
    await page.getByRole('button', { name: '创建', exact: true }).click();

    await expect(page.locator('.toast', { hasText: '密钥值至少需要 8 个字符' })).toBeVisible();
    // 弹窗留着（用户可以改），且没有请求发出
    await expect(page.locator('.modal')).toBeVisible();
    expect(posts, '短密钥被拦住了却仍然发了 POST').toEqual([]);

    // 反例对照：改成 8 个字符就能发出（证明拦住的是长度，不是把创建整个弄坏了）
    await keyInput.fill('sk-12345');
    const [post] = await Promise.all([
      page.waitForRequest((r: Request) => r.url().endsWith('/api/tokens') && r.method() === 'POST'),
      page.getByRole('button', { name: '创建', exact: true }).click(),
    ]);
    expect((post.postDataJSON() as Record<string, unknown>).custom_key).toBe('sk-12345');

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });
});
