import { test, expect, loginAsAdmin } from './fixtures';
import type { Page } from '@playwright/test';

/**
 * 报表增强（`2c96e573` 那一笔：Top 榜数量选择 / 榜尾占比口径 / 双饼图阈值 /
 * 趋势「指标 × 形态」/ 客户端排序与下钻）。
 *
 * 为什么单独成文件：这五处此前**一条 e2e 都没有**，而 `report-channels.spec.ts`
 * 只断「Top 供应商表有 2 行」（旧桩只有 2 行数据）。桩一改行数那条就红，改回
 * 「有行即绿」又失去了区分能力 —— 所以这里按**行为**写断言：
 *
 *   · 档位切换要真的改变行数（5 / 10 两档在 10 行数据上可区分）；
 *   · 榜尾占比的**分母**是概览总量（120）而不是榜内合计（83）：
 *     前 5 行 74 请求 → 74/120 = 61.7%（正确） vs 74/83 = 89.2%（错口径）；
 *   · 排序切换要真的换行序（请求数与 Token 排序刻意不同）；
 *   · 点客户端行要真的跳到 /logs 并把筛选**写进去**（只跳不带筛选 = 死链接）；
 *   · 饼图阈值要真的改变扇区数（1% → 6 片、5% → 5 片、关掉 → 9 片），
 *     两张饼图共用同一份设置，图例始终列全量；
 *   · 趋势的指标与形态各自独立生效（读数换、图形本体换）。
 *
 * 桩的数据形状见 `static-server.mjs` 的 REPORT_TOP_SOURCES / REPORT_MODELS /
 * REPORT_TOP_CLIENTS / daily_trend —— 那些数字是**故意**挑成能区分正确与错误
 * 实现的（同序、同量、单点都会让断言失去意义）。
 */

/** 按标题定位报表面板（PanelWrap 的 `.section-card`，标题里带窗口文案） */
function panel(page: Page, title: string) {
  return page.locator('.section-card', { hasText: title });
}

async function gotoDashboard(page: Page): Promise<void> {
  await loginAsAdmin(page, { sessions: [] });
  await page.goto('/');
  await expect(page.locator('.main-content')).toContainText('Top 供应商');
}

test.describe('报表增强 · Top 榜档位与榜尾占比', () => {
  test('供应商榜：前 5 / 前 10 / 全部 三档行数不同，榜尾占比用概览总量当分母', async ({ page, pageErrors }) => {
    await gotoDashboard(page);
    const card = panel(page, 'Top 供应商（');
    const rows = card.locator('tbody tr');
    // 档位控件（aria-label 区分供应商 / 客户端 / 模型三个榜）
    const counts = card.locator('[role="group"][aria-label="供应商显示数量"]');

    // 默认「前 5 名」：桩 10 行 → 只画 5 行
    await expect(rows).toHaveCount(5);
    await expect(card.locator('.rank-footer')).toContainText('前 5 名占全部请求');

    // 榜尾占比：前 5 行请求数 30+18+12+8+6 = 74，分母取概览 total_requests = 120
    // → 61.7%。若实现拿「榜内合计 83」当分母会得到 89.2% —— 这条断言专门杀它。
    await expect(card.locator('.rank-footer')).toContainText('61.7%');
    await expect(card.locator('.rank-footer')).not.toContainText('89.2%');

    // 切「全部」→ 10 行，榜尾占比跟着变成 83/120 = 69.2%
    await counts.getByRole('button', { name: '全部' }).click();
    await expect(counts.getByRole('button', { name: '全部' })).toHaveAttribute('aria-pressed', 'true');
    await expect(rows).toHaveCount(10);
    await expect(card.locator('.rank-footer')).toContainText('69.2%');

    // 切「前 10 名」→ 桩只有 10 行，行数回到 10 但**读数标签**必须是「前 10 名」
    // （后端 truncate(10)，所以「前 10」与「全部」在数据上等价；这里断的是控件
    //  状态真的跟上了点击，不是又变回「全部」）
    await counts.getByRole('button', { name: '前 10 名' }).click();
    await expect(card.locator('.rank-footer')).toContainText('前 10 名占全部请求');
    await expect(rows).toHaveCount(10);

    // 回到「前 5 名」：行数与标签都要回退（单向后无法证明控件真的受控）
    await counts.getByRole('button', { name: '前 5 名' }).click();
    await expect(rows).toHaveCount(5);
    await expect(card.locator('.rank-footer')).toContainText('前 5 名占全部请求');

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('模型榜：默认前 10 名，切「前 5 名」只画 5 行', async ({ page, pageErrors }) => {
    await gotoDashboard(page);
    // 「Top 模型」卡与「模型分布」「我的 Top 模型」都不是同一个标题，按完整标题取
    const card = page.locator('.section-card').filter({ has: page.locator('h2', { hasText: /^Top 模型（/ }) });
    const rows = card.locator('tbody tr');
    await expect(rows).toHaveCount(10);

    const counts = card.locator('[role="group"][aria-label="模型显示数量"]');
    await counts.getByRole('button', { name: '前 5 名' }).click();
    await expect(rows).toHaveCount(5);
    // 占比分母是**全表**合计（不随档位变），切档后第一行读数不变
    await expect(card.locator('.rank-footer')).toContainText('前 5 名占全部请求');

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });
});

test.describe('报表增强 · 客户端排序与下钻', () => {
  test('按请求数 / 按 Token 切换真的改变行序', async ({ page, pageErrors }) => {
    await gotoDashboard(page);
    const card = panel(page, 'Top 客户端（');
    const firstRowName = card.locator('tbody tr').first().locator('td').nth(1);
    const sort = card.locator('[role="group"][aria-label="客户端排序"]');

    // 默认按请求数：cursor(50) 在最前
    await expect(firstRowName).toHaveText('cursor');
    // 桩里 cursor 请求最多但 Token 最少（500），cline Token 最多（9000）
    await sort.getByRole('button', { name: '按 Token' }).click();
    await expect(sort.getByRole('button', { name: '按 Token' })).toHaveAttribute('aria-pressed', 'true');
    await expect(firstRowName).toHaveText('cline');

    // 切回按请求数：顺序必须回退（证明排序是受控的，不是一次性副作用）
    await sort.getByRole('button', { name: '按请求数' }).click();
    await expect(firstRowName).toHaveText('cursor');

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('点客户端行跳到请求日志，且筛选被真的写进日志页（不是只带个参数）', async ({ page, pageErrors }) => {
    await gotoDashboard(page);
    const card = panel(page, 'Top 客户端（');
    // 下钻入口：行内「查看」按钮 / 客户端名（`.row-link`）
    const drillBtn = card.locator('tbody tr').first().locator('td').last().getByRole('button', { name: '查看' });
    await expect(drillBtn).toBeVisible();
    await drillBtn.click();

    await expect(page).toHaveURL(/\/logs\?.*client=cursor/);
    // 关键：Logs 页启动时从 localStorage 记忆恢复筛选（不从 URL 读）。
    // 只 navigate 不写记忆的话，页面打开了、筛选项空着 —— 那是个看着像功能、
    // 实际什么都不做的链接。这里断「客户端筛选框里真的是 cursor」。
    await expect(page.locator('input[aria-label="客户端"]')).toHaveValue('cursor');

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });
});

test.describe('报表增强 · 双饼图合并小项阈值', () => {
  test('阈值改变扇区数；两张饼图共用一份设置；图例始终列全量', async ({ page, pageErrors }) => {
    await gotoDashboard(page);
    const modelCard = panel(page, '模型分布（');
    const providerCard = panel(page, '供应商分布（');

    // 桩：12 个模型、token 合计 10000（占比 = token/100）
    // 默认阈值 1%（占比 <1% 并入「其他」）→ 5 大 + 1 个「其他」= 6 个扇区
    const modelSlices = modelCard.locator('svg path');
    await expect(modelSlices).toHaveCount(6);
    // 图例始终列全量（合并只作用于饼图本身）
    await expect(modelCard.locator('.donut-legend-row')).toHaveCount(12);

    // 供应商饼图：10 家（token 合计 10200），默认阈值 1% → 6 大 + 1「其他」= 7 片
    const providerSlices = providerCard.locator('svg path');
    await expect(providerSlices).toHaveCount(7);
    await expect(providerCard.locator('.donut-legend-row')).toHaveCount(10);

    // 阈值抬到 5% → 模型剩 4 大 + 1「其他」= 5 片；供应商剩 3 大 + 1 = 4 片
    // （同一份设置，两张图一起变 —— 这就是「共用」的可验证含义）
    const select = modelCard.locator('select[aria-label="小项合并阈值"]');
    await select.selectOption('5');
    await expect(modelSlices).toHaveCount(5);
    await expect(providerSlices).toHaveCount(4);

    // 关掉开关 → 回到纯 maxSlices 行为。
    // `DonutChart` 的 `maxSlices`（默认 9）是**扇区条数上限**，含「其他」：
    // 装配时 `keep = maxSlices - 1` 先切出 8 片，再把被挤出的尾部并成第 9 个
    // 「其他」。早前这里是 `keep = maxSlices` 再 push「其他」，实际画出 10 片
    // （声明上限 9 却越界 1）；组件已修正为真正的 9，本断言随之改成 9。
    // 图例（12 行）不受 maxSlices 影响，仍列全量。
    await modelCard.locator('[role="switch"][aria-label="合并小项"]').click();
    await expect(modelCard.locator('[role="switch"][aria-label="合并小项"]')).toHaveAttribute('aria-checked', 'false');
    await expect(modelSlices).toHaveCount(9);
    // 阈值下拉在关掉时禁用（此时调阈值没有意义）
    await expect(select).toBeDisabled();

    // 设置落 localStorage 且刷新后仍生效（跨会话记住）
    const stored = await page.evaluate(() => localStorage.getItem('aigx-report-donut-settings'));
    expect(stored, '合并小项设置没落盘').not.toBeNull();
    expect(JSON.parse(stored as string)).toMatchObject({ enabled: false, threshold: 5 });
    await page.reload();
    await expect(panel(page, '模型分布（').locator('svg path')).toHaveCount(9);

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });
});

test.describe('报表增强 · 用量趋势指标 × 形态', () => {
  test('切指标换读数、切形态换图形本体（两者互不干扰）', async ({ page, pageErrors }) => {
    await gotoDashboard(page);
    const card = panel(page, '用量趋势（');
    const metric = card.locator('[role="group"][aria-label="趋势统计指标"]');
    const shape = card.locator('[role="group"][aria-label="图表类型"]');
    const reading = card.locator('.trend-reading strong');

    // 默认「消费」口径：3 天合计 123+45+8 = 176（金额换算随设置变，故只断存在数值）
    await expect(reading).not.toBeEmpty();

    // 「请求数」口径：20+30+10 = 60（整数读数，不受金额换算影响）
    await metric.getByRole('button', { name: '请求数' }).click();
    await expect(reading).toHaveText('60');
    // 「Token」口径：input+output = (1000+500)+(3000+1000)+(1200+300) = 7000
    await metric.getByRole('button', { name: 'Token' }).click();
    await expect(reading).toHaveText('7,000');

    // 形态：柱状 = rect 画柱、无 polyline；折线 = 1 条 polyline、无柱
    await expect(shape.getByRole('button', { name: '柱状' })).toHaveAttribute('aria-pressed', 'true');
    const svg = card.locator('svg').first();
    expect(await svg.locator('polyline').count()).toBe(0);
    expect(await svg.locator('rect').count()).toBeGreaterThanOrEqual(3);

    await shape.getByRole('button', { name: '折线' }).click();
    await expect(svg.locator('polyline')).toHaveCount(1);

    // 切形态不影响指标读数（两套状态互不干扰）
    await expect(reading).toHaveText('7,000');

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });
});
