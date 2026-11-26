import { test, expect, loginAsAdmin } from './fixtures';
import type { Page, Request } from '@playwright/test';

/**
 * API 密钥页 · 轮换 / 重置已用 / 批量启停 / 批量删除 / 每页条数。
 *
 * 为什么单独成文件：`keys-providers.spec.ts` 覆盖的是**提供商白名单**那条线，
 * `smoke.spec.ts` 只证明页面能渲染。于是这五件事在 CI 里一条断言都没有 ——
 * 而它们全是「看起来成功、实际没发生」的高发区：
 *
 *   · 轮换：新明文只该出现**一次**。旧实现若把新 key 顺手写回列表单元格，
 *     关掉弹窗后仍能从页面上抄到明文；
 *   · 重置已用：`reset_used` 失败被吞时界面照样提示「已重置」，用量纹丝不动；
 *   · 批量启停/删除：逐条发请求，**每条都要真的打出去**（只发第一条 = 部分成功
 *     却报「已处理 N 个」）；
 *   · 每页条数：默认档 50 是 `usePageSize` 的 fallback，改过之后必须落
 *     localStorage（`aigx-tbl-pagesize.keys`）并跨刷新保持。
 *
 * ── 桩的两个前提（缺一个这些断言就恒真/恒假）─────────────────
 * 1. 令牌种子共 **60 条**（3 条业务种子 + 57 条 filler）：只有超过 50 条，
 *    「默认 50 / 改 20 / 全部」三档的行数才互不相同，断言才有区分力；
 * 2. 明文只走单条端点：列表响应经 `maskTokenRow` 剥掉 `_plain`，明文由
 *    `GET /api/tokens/:id/key` 与 `POST .../rotate` 一次性给出 —— 与真实
 *    `handle_get_token_key`（tokens.rs:279）/ `handle_rotate_token`
 *    （legacy.rs:2870）一致。桩若把明文塞进列表，本文件第 2 条会变成桩自证。
 */

const RESET = '/api/__e2e/reset-tokens';

/** 打开 /keys 并等令牌表出现 */
async function gotoKeys(page: Page): Promise<void> {
  await loginAsAdmin(page, { sessions: [] });
  await page.goto('/keys');
  await expect(page.locator('.keys-table tbody tr').first()).toBeVisible();
}

/**
 * 按名称取行（**精确**匹配名称单元格）。
 *
 * 不能用 `{ hasText: '填充令牌 1' }`：`hasText` 是子串匹配，「填充令牌 1」会同时
 * 命中 1 / 10~19 共 11 行 —— 用例会变成「批量删了一个不特定的行」。
 */
function row(page: Page, name: string) {
  const exact = new RegExp(`^${name.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}$`);
  return page
    .locator('.keys-table tbody tr')
    .filter({ has: page.locator('.col-name .tk-name', { hasText: exact }) })
    .first();
}

/**
 * 行内「更多操作」菜单（fixed 挂在 body 层，逃出表格 overflow 裁剪）。
 *
 * ⚠️ 这里必须先 `scrollIntoViewIfNeeded` 并**等滚动停下**再点 —— 不是测试的
 * 保守写法，而是当前页面的真实缺陷绕行（已登记在交付说明里）：
 * 目标行的按钮在表格可视区之外时，点击会让 `.table-wrapper` 在同一次交互里
 * 自动滚动；`Keys.tsx:254-265` 给 `window` 挂了 capture 的 `scroll` 监听，
 * 它把 `rowMenuId` 立刻清空 ⇒ 菜单**打开同一 tick 就被关掉**。实测时序
 * （MutationObserver + performance.now）：PANEL=1@1017 → scroll@1019 → PANEL=0@1036。
 * 预先滚到可见后单击则稳定打开（两次实测 n=1），所以这里只做「先滚再点」，
 * 不去掩盖「点了却看不见菜单」这个产品问题本身。
 */
async function openRowMenu(page: Page, name: string): Promise<void> {
  const btn = row(page, name).locator('button[title="更多操作"]');
  await btn.scrollIntoViewIfNeeded();
  // 等自动滚动（及其 scroll 事件）彻底停下，否则菜单会在打开的同一 tick 被清掉
  await page.waitForTimeout(250);
  await btn.click();
  await expect(page.locator('.tk-row-menu-panel')).toBeVisible();
}

/** 确认弹窗（role="dialog" 挂在 .modal-overlay 上） */
function dialog(page: Page, title: string) {
  return page.locator('.modal-overlay[role="dialog"]', { hasText: title });
}

/**
 * 轮换后的「一次性明文」弹窗。
 *
 * 它不是 ConfirmDialog（那个才有 `role="dialog"`）：`Keys.tsx:1241-1266` 自己画的
 * `.modal-overlay`，**没有 role / aria-modal**。所以这里按文案取 —— 顺带把
 * 「这个弹窗缺 dialog 语义」这件事暴露在测试里（无障碍回归应另做，不在本轮面）。
 */
function rotatedDialog(page: Page) {
  return page.locator('.modal-overlay', { hasText: '令牌已轮换' });
}

/** 勾选若干行（返回勾选数量，便于断言「真的勾上了」） */
async function pickRows(page: Page, names: string[]): Promise<void> {
  for (const name of names) {
    await row(page, name).locator('td.col-select input[type="checkbox"]').check();
  }
}

/** 收集发往某端点的请求（批量操作必须逐条发出，不能只发第一条） */
function watchRequests(page: Page, pattern: RegExp): Request[] {
  const hits: Request[] = [];
  page.on('request', (r: Request) => {
    if (pattern.test(`${r.method()} ${r.url()}`)) hits.push(r);
  });
  return hits;
}

test.describe('API 密钥 · 每页条数', () => {
  test.beforeEach(async ({ page }) => {
    const res = await page.request.post(RESET);
    expect(res.ok(), `复位端点失败: ${res.status()}`).toBeTruthy();
  });

  test('默认 50 条/页（不是 20），改档后落 localStorage 且刷新保持', async ({ page, pageErrors }) => {
    await gotoKeys(page);

    const sizeSelect = page.locator('.tbl-pagesize');
    const info = page.locator('.tbl-foot-info');
    const rows = page.locator('.keys-table tbody tr');

    // 默认档：60 条数据 ⇒ 50 条一页，第 2 页有 10 条。
    // 「默认 20」的实现会在这里给出 20 行 + 「1–20」，两条断言同时红。
    await expect(sizeSelect).toHaveValue('50');
    await expect(rows).toHaveCount(50);
    await expect(info).toContainText('共 60 条');
    await expect(info).toContainText('当前第 1–50 条');

    // 翻到第 2 页：只剩 10 行（不是 0，也不是重复的 50 行）。
    //
    // 窄屏（mobile project，390px）下中间页码是**故意隐藏**的
    // （App.css:2253 `.pagination-num:not(.is-active)` → display:none，GitHub/Stripe
    // 同款做法：只留首页/上一页/当前页/下一页/末页），此时精确跳页走页脚的
    // 「跳至 N 页」输入框。两条路径都要断 —— 只测宽屏会把窄屏的分页彻底放开不管。
    const page2 = page.locator('.tbl-foot nav.pagination-bar')
      .getByRole('button', { name: '第 2 页', exact: true });
    if ((page.viewportSize()?.width ?? 1440) < 900) {
      await expect(page2, '窄屏不该再渲染中间页码').toHaveCount(0);
      const jump = page.locator('.tbl-jump-input');
      await jump.fill('2');
      await jump.press('Enter');
    } else {
      await page2.click();
    }
    await expect(info).toContainText('当前第 51–60 条');
    await expect(rows).toHaveCount(10);

    // 改档：20/页 ⇒ 60 条分 3 页，回第 1 页
    await sizeSelect.selectOption('20');
    await expect(rows).toHaveCount(20);
    await expect(info).toContainText('当前第 1–20 条');

    // 落盘（跨刷新记住）
    const stored = await page.evaluate(() => localStorage.getItem('aigx-tbl-pagesize.keys'));
    expect(stored, '每页条数没落 localStorage（刷新就回默认）').toBe('20');

    await page.reload();
    await expect(page.locator('.keys-table tbody tr').first()).toBeVisible();
    await expect(page.locator('.tbl-pagesize')).toHaveValue('20');
    await expect(page.locator('.keys-table tbody tr')).toHaveCount(20);

    // 「全部」档：一次画 60 行（客户端分页的第三种取值）
    await page.locator('.tbl-pagesize').selectOption('all');
    await expect(page.locator('.keys-table tbody tr')).toHaveCount(60);
    await expect(page.locator('.tbl-foot-info')).toContainText('当前第 1–60 条');

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });
});

test.describe('API 密钥 · 轮换与重置已用', () => {
  test.beforeEach(async ({ page }) => {
    await page.request.post(RESET);
  });

  test('轮换：一次性明文只在弹窗里出现一次，关掉后页面上再也拿不到', async ({ page, pageErrors }) => {
    const rotates = watchRequests(page, /POST .*\/api\/tokens\/[^/]+\/rotate$/);
    await gotoKeys(page);

    const target = row(page, 'CrewAI 生产');
    const beforeMask = await target.locator('.tk-key-code').innerText();
    expect(beforeMask).toContain('sk-e2e-****-aaaa');

    await openRowMenu(page, 'CrewAI 生产');
    await page.locator('.tk-row-menu-panel').getByRole('button', { name: '轮换' }).click();

    const confirm = dialog(page, '轮换令牌密钥');
    await expect(confirm).toBeVisible();
    await expect(confirm).toContainText('轮换将生成新密钥，旧密钥立即失效');
    await confirm.getByRole('button', { name: '轮换', exact: true }).click();

    // 一次性明文弹窗：新密钥形态是后端 `sk-<uuid>` 那一族（桩用 sk-e2e-rot- 前缀）
    const rotated = rotatedDialog(page);
    await expect(rotated).toBeVisible();
    await expect(rotated).toContainText('此为最后一次显示');
    const newKey = (await rotated.locator('code.generated-key').innerText()).trim();
    expect(newKey, `新密钥形态不对：${newKey}`).toMatch(/^sk-e2e-rot-[0-9a-z]+-[0-9a-z]+$/);
    expect(newKey).not.toBe('sk-e2e-plain-aaaa-0001');
    expect(rotates, '轮换请求没发出去').toHaveLength(1);

    // 关闭后：弹窗与明文都必须消失（页面别处也不许再出现这串）
    await rotated.getByRole('button', { name: '我已保存' }).click();
    await expect(rotated).toBeHidden();
    await expect(page.locator('code.generated-key')).toHaveCount(0);
    await expect(page.getByText(newKey, { exact: true })).toHaveCount(0);

    // 列表已重载：掩码换成了新 key 的尾四位（证明后端状态真的变了，
    // 而不是只在弹窗里画了一串随机字符）
    const afterMask = await row(page, 'CrewAI 生产').locator('.tk-key-code').innerText();
    expect(afterMask).not.toBe(beforeMask);
    expect(afterMask).toContain(`sk-e2e-****-${newKey.slice(-4)}`);

    // 再开一次菜单：先关掉（点同一入口是 toggle），确认菜单确实能反复开关；
    // 且新明文不会因为「再开一次菜单」而重新出现在任何地方
    await openRowMenu(page, 'CrewAI 生产');
    await page.locator('.tk-row-menu-panel').getByRole('button', { name: '轮换' }).click();
    await expect(dialog(page, '轮换令牌密钥')).toBeVisible();
    // 取消这次轮换：该用例只验证「弹窗正文是一次性载体」，不重复写
    await dialog(page, '轮换令牌密钥').getByRole('button', { name: '取消' }).click();
    await expect(dialog(page, '轮换令牌密钥')).toBeHidden();
    await expect(page.getByText(newKey, { exact: true })).toHaveCount(0);

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('重置已用：确认后用量归零且列表重载；无配额上限的行不出现该菜单项', async ({ page, pageErrors }) => {
    const resets = watchRequests(page, /POST .*\/api\/tokens\/[^/]+\/reset_used$/);
    await gotoKeys(page);

    const target = row(page, 'CrewAI 生产');
    // 12000 额度 ÷ 200 = 60.00（fmtQuota 的「额度→元」口径）
    await expect(target.locator('.col-quota')).toContainText('60.00');

    await openRowMenu(page, 'CrewAI 生产');
    await page.locator('.tk-row-menu-panel').getByRole('button', { name: '重置已用' }).click();
    const confirm = dialog(page, '重置已用');
    await expect(confirm).toBeVisible();
    await confirm.getByRole('button', { name: '重置已用', exact: true }).click();

    await expect(page.locator('.toast', { hasText: '已重置已用额度' })).toBeVisible();
    await expect(resets, '重置请求没发出去').toHaveLength(1);
    expect(resets[0].url()).toContain('/api/tokens/tk-seed-1/reset_used');
    // 列表已重载：用量归零
    await expect(row(page, 'CrewAI 生产').locator('.col-quota')).toContainText('0.00');
    await expect(row(page, 'CrewAI 生产').locator('.col-quota')).not.toContainText('60.00');

    // 反例（判据的另一半）：没有 quota_limit 的行**不该**有「重置已用」——
    // 没有上限就没有「已用」，给它这个按钮只是让用户误点
    await openRowMenu(page, 'LiteLLM 网关');
    await expect(page.locator('.tk-row-menu-panel')).toContainText('复制密钥');
    await expect(page.locator('.tk-row-menu-panel').getByRole('button', { name: '重置已用' })).toHaveCount(0);

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });
});

test.describe('API 密钥 · 批量操作', () => {
  test.beforeEach(async ({ page }) => {
    await page.request.post(RESET);
  });

  test('批量启用 / 禁用：逐条发 PUT，界面状态与每条请求都对得上', async ({ page, pageErrors }) => {
    const puts = watchRequests(page, /PUT .*\/api\/tokens\//);
    await gotoKeys(page);

    // tk-seed-3 初始 disabled、tk-seed-2 active：一启一停，两方向都被驱动
    await expect(row(page, '三方混跑').locator('.tk-status-badge')).toHaveText('禁用');
    await pickRows(page, ['LiteLLM 网关', '三方混跑']);
    const bulk = page.locator('.keys-bulk');
    await expect(bulk).toContainText('已选 2 / 60');

    // 启用方向：不弹确认（启用不会打断线上调用），直接每组一条 PUT
    await bulk.getByRole('button', { name: '启用' }).click();
    await expect(page.locator('.toast', { hasText: '已启用 2 个令牌' })).toBeVisible();
    await expect.poll(() => puts.length).toBe(2);
    expect(puts.map((r) => r.postDataJSON() as Record<string, unknown>))
      .toEqual([{ status: 'active' }, { status: 'active' }]);
    // 批量操作后选择被清空、列表重载：批量栏收起、两行都是「启用」
    await expect(page.locator('.keys-bulk')).toHaveCount(0);
    await expect(row(page, '三方混跑').locator('.tk-status-badge')).toHaveText('启用');
    await expect(row(page, 'LiteLLM 网关').locator('.tk-status-badge')).toHaveText('启用');

    // 禁用方向：必须二次确认（会立刻打断生产调用），文案带条数
    puts.length = 0;
    await pickRows(page, ['LiteLLM 网关', '三方混跑']);
    await page.locator('.keys-bulk').getByRole('button', { name: '禁用' }).click();
    const confirm = dialog(page, '批量禁用令牌');
    await expect(confirm).toBeVisible();
    await expect(confirm).toContainText('确定禁用选中的 2 个令牌');
    await confirm.getByRole('button', { name: '禁用', exact: true }).click();

    await expect(page.locator('.toast', { hasText: '已禁用 2 个令牌' })).toBeVisible();
    await expect.poll(() => puts.length).toBe(2);
    expect(puts.map((r) => r.postDataJSON() as Record<string, unknown>))
      .toEqual([{ status: 'disabled' }, { status: 'disabled' }]);
    await expect(row(page, 'LiteLLM 网关').locator('.tk-status-badge')).toHaveText('禁用');
    await expect(row(page, '三方混跑').locator('.tk-status-badge')).toHaveText('禁用');

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('批量删除：确认框写清条数，逐条 DELETE，删完总数与行数都对', async ({ page, pageErrors }) => {
    const deletes = watchRequests(page, /DELETE .*\/api\/tokens\/[^/]+$/);
    await gotoKeys(page);
    await expect(page.locator('.tbl-foot-info')).toContainText('共 60 条');

    await pickRows(page, ['三方混跑', '填充令牌 1']);
    await expect(page.locator('.keys-bulk')).toContainText('已选 2 / 60');
    await page.locator('.keys-bulk').getByRole('button', { name: '删除', exact: true }).click();

    const confirm = dialog(page, '批量删除令牌');
    await expect(confirm).toBeVisible();
    // 文案必须写明条数与后果（「撤回不了」这件事得在按下去之前看到）
    await expect(confirm).toContainText('确定删除选中的 2 个令牌');
    await expect(confirm).toContainText('使用这些令牌的调用将立即失败');
    await confirm.getByRole('button', { name: '删除', exact: true }).click();

    await expect(page.locator('.toast', { hasText: '已删除 2 个令牌' })).toBeVisible();
    await expect.poll(() => deletes.length).toBe(2);
    // 逐条删除的是**选中的那两条**（不是「选了第一条就删第一条」）
    const urls = deletes.map((r) => r.url()).sort();
    expect(urls.some((u) => u.endsWith('/tk-seed-3'))).toBe(true);
    expect(urls.some((u) => u.endsWith('/tk-fill-1'))).toBe(true);

    // 列表重载：总数与行数同步下降，被删的行消失
    await expect(page.locator('.tbl-foot-info')).toContainText('共 58 条');
    await expect(row(page, '三方混跑')).toHaveCount(0);
    await expect(row(page, '填充令牌 1')).toHaveCount(0);
    // 其余行不受影响（批量 ≠ 全删）
    await expect(row(page, 'CrewAI 生产')).toBeVisible();

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });
});
