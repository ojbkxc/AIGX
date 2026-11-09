import { test, expect, loginAsAdmin, type SeedSession } from './fixtures';
import type { Page } from '@playwright/test';

/**
 * 分页统一控件 E2E（TablePaginationFooter + 公共 Pagination）。
 *
 * 被测页选渠道管理（客户端分页、行数可控）：用 `page.route` 把 `/api/channels`
 * 换成 45 条生成数据（默认每页 20 → 3 页），避免改动 static-server 的全局桩
 * （全局 CHANNELS 只有 6 条，改它会污染其他渠道用例）。
 *
 * 断言全部走语义选择器（role / aria-label / aria-current），不依赖分页 class：
 * - 首/末/上/下一页按钮：aria-label「首页」「上一页」「下一页」「末页」
 * - 数字页码按钮：aria-label「第 N 页」+ 当前页 aria-current="page"
 * - 每页条数 select：aria-label「每页条数」；跳页 input：aria-label「跳至页码」
 */

const TOTAL = 45;
const PER_PAGE = 20;

/** 生成渠道桩：命名带序号，便于断言翻页后内容确实换了 */
function makeChannels(count: number): Array<Record<string, unknown>> {
  return Array.from({ length: count }, (_, i) => {
    const n = String(i + 1).padStart(2, '0');
    return {
      id: `pg-ch-${n}`,
      seq: i + 1,
      name: `分页渠道 ${n}`,
      channel_type: 'openai_compatible',
      base_url: 'https://pg.example.com/v1',
      api_key: `sk-pg-${n}`,
      priority: 0,
      weight: 1,
      status: 'enabled',
      models: ['glm-4'],
      model_mapping: {},
      response_time: 100,
      test_time: 1761900000,
      balance_query_interval_secs: null,
      balance_last_query_at: null,
      balance_last_error: null,
    };
  });
}

/** 打开渠道页并等待首页数据行渲染 */
async function openChannels(page: Page, opts: { sessions?: SeedSession[] } = {}): Promise<void> {
  await loginAsAdmin(page, { sessions: opts.sessions ?? [] });
  await page.route(/\/api\/channels(\?.*)?$/, (route) => {
    if (route.request().method() !== 'GET') return route.continue();
    const data = makeChannels(TOTAL);
    return route.fulfill({
      status: 200,
      contentType: 'application/json; charset=utf-8',
      body: JSON.stringify({ success: true, data, total: data.length, page: 1, size: data.length }),
    });
  });
  await page.goto('/channels');
  await expect(rows(page)).toHaveCount(PER_PAGE);
}

const rows = (page: Page) => page.locator('.channels-table tbody tr');
const firstRowName = (page: Page) => rows(page).first().locator('td.col-name');
const pageBtn = (page: Page, n: number) => page.getByRole('button', { name: `第 ${n} 页` });
const nav = (page: Page) => page.getByRole('navigation', { name: '分页导航' });

/** 断言当前页高亮（aria-current）且首行是期望的序号 */
async function expectPage(page: Page, n: number, firstSeq: number, rowCount: number): Promise<void> {
  await expect(pageBtn(page, n)).toHaveAttribute('aria-current', 'page');
  await expect(rows(page)).toHaveCount(rowCount);
  await expect(firstRowName(page)).toContainText(`分页渠道 ${String(firstSeq).padStart(2, '0')}`);
}

test.describe('分页统一控件（模型管理页）', () => {
  test('非分组视图出现统一页脚；分组视图保持无页脚', async ({ page, pageErrors }) => {
    await loginAsAdmin(page, { sessions: [] });
    await page.goto('/model-manage');
    await expect(page.locator('h1')).toContainText('模型管理');

    // 默认「全部」= 分组视图：无页脚（保持原行为）
    await expect(page.locator('.mm-table')).toBeVisible();
    await expect(page.getByLabel('每页条数')).toHaveCount(0);

    // 点左栏单个渠道 → 非分组视图：统一页脚出现（共 N 条 / 每页条数 / 跳页）
    await page.locator('.mm-rail-item', { hasText: 'lxchat-glm' }).click();
    await expect(page.getByText('共 1 条')).toBeVisible();
    await expect(page.getByLabel('每页条数')).toBeVisible();
    // 单页时跳页框禁用
    await expect(page.getByLabel('跳至页码')).toBeDisabled();

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });
});

test.describe('分页统一控件（渠道页）', () => {
  test('页脚信息与首页边界：共 N 条 / 当前区间 / 首页与上一页禁用', async ({ page, pageErrors }) => {
    await openChannels(page);

    // 总数 + 区间文案
    await expect(page.getByText(`共 ${TOTAL} 条`)).toBeVisible();
    await expect(page.getByText(`当前第 1–${PER_PAGE} 条`)).toBeVisible();

    // 首页：第 1 页渲染 20 行，首行是 01
    await expectPage(page, 1, 1, PER_PAGE);

    // 第 1 页：首页/上一页禁用，下一页/末页可用
    await expect(page.getByRole('button', { name: '首页' })).toBeDisabled();
    await expect(page.getByRole('button', { name: '上一页' })).toBeDisabled();
    await expect(page.getByRole('button', { name: '下一页' })).toBeEnabled();
    await expect(page.getByRole('button', { name: '末页' })).toBeEnabled();

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('点数字页码翻页：内容随页变化，末页按钮到末页后禁用', async ({ page, pageErrors, isMobile }) => {
    // 移动端（<=720px）隐藏非当前页的数字页码，无法点击页码按钮翻页；
    // 移动端的翻页路径由「下一页/上一页」「箭头 + 跳页输入」两组用例覆盖。
    test.skip(isMobile, '移动端隐藏非当前页码，无法点击页码按钮');

    await openChannels(page);

    await pageBtn(page, 2).click();
    await expectPage(page, 2, PER_PAGE + 1, PER_PAGE);

    // 45 条 → 第 3 页只有 5 行
    await pageBtn(page, 3).click();
    await expectPage(page, 3, PER_PAGE * 2 + 1, TOTAL - PER_PAGE * 2);
    await expect(page.getByRole('button', { name: '下一页' })).toBeDisabled();
    await expect(page.getByRole('button', { name: '末页' })).toBeDisabled();

    // 末页区间文案同步
    await expect(page.getByText(`当前第 ${PER_PAGE * 2 + 1}–${TOTAL} 条`)).toBeVisible();

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('上一页 / 下一页按钮翻页', async ({ page, pageErrors }) => {
    await openChannels(page);

    await page.getByRole('button', { name: '下一页' }).click();
    await expectPage(page, 2, PER_PAGE + 1, PER_PAGE);

    await page.getByRole('button', { name: '下一页' }).click();
    await expectPage(page, 3, PER_PAGE * 2 + 1, TOTAL - PER_PAGE * 2);

    await page.getByRole('button', { name: '上一页' }).click();
    await expectPage(page, 2, PER_PAGE + 1, PER_PAGE);

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('首页 / 末页按钮直达首末页', async ({ page, pageErrors }) => {
    await openChannels(page);

    await page.getByRole('button', { name: '末页' }).click();
    await expectPage(page, 3, PER_PAGE * 2 + 1, TOTAL - PER_PAGE * 2);

    await page.getByRole('button', { name: '首页' }).click();
    await expectPage(page, 1, 1, PER_PAGE);

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('切换每页条数后回到第 1 页', async ({ page, pageErrors }) => {
    await openChannels(page);

    // 先翻到第 2 页（用「下一页」以便桌面/移动端通用——移动端隐藏非当前页码）
    await page.getByRole('button', { name: '下一页' }).click();
    await expectPage(page, 2, PER_PAGE + 1, PER_PAGE);

    // 切到 50/页：页码条退场（单页），区间覆盖全部 45 条
    await page.getByLabel('每页条数').selectOption('50');
    await expect(rows(page)).toHaveCount(TOTAL);
    await expect(firstRowName(page)).toContainText('分页渠道 01');
    await expect(page.getByText(`当前第 1–${TOTAL} 条`)).toBeVisible();

    // 切回 20/页：仍是第 1 页（未被留在越界的第 2 页）
    await page.getByLabel('每页条数').selectOption('20');
    await expectPage(page, 1, 1, PER_PAGE);
    await expect(nav(page)).toBeVisible();

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('跳页输入：合法值生效，越界值被忽略', async ({ page, pageErrors }) => {
    await openChannels(page);

    const jump = page.getByLabel('跳至页码');

    // 合法：跳到第 2 页
    await jump.fill('2');
    await jump.press('Enter');
    await expectPage(page, 2, PER_PAGE + 1, PER_PAGE);

    // 越界（超过总页数）：保持当前页
    await jump.fill('999');
    await jump.press('Enter');
    await expectPage(page, 2, PER_PAGE + 1, PER_PAGE);

    // 越界（0）：保持当前页
    await jump.fill('0');
    await jump.press('Enter');
    await expectPage(page, 2, PER_PAGE + 1, PER_PAGE);

    // 合法：回第 1 页
    await jump.fill('1');
    await jump.press('Enter');
    await expectPage(page, 1, 1, PER_PAGE);

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });
});
