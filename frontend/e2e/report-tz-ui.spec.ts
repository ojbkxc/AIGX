import { test, expect, loginAsAdmin } from './fixtures';
import type { Page, Request } from '@playwright/test';

/**
 * 报表设置 Tab（仪表盘 → 「报表设置」）· **UI 驱动**的时区契约。
 *
 * 为什么单独成文件：`report-tz-settings.spec.ts` 全是 `page.request` 直打桩的
 * API 级断言 —— 它钉的是 HTTP 契约（六键形状 / 热生效 / 幂等 / 顺序坑 400），
 * **一个 UI 控件都没碰**。于是 `ReportSettingsPanel`（`#20` 批3 的前端面，
 * 迁自「系统设置 → 报表」）在 CI 里没有任何落点：
 *
 *   · 输入框初值到底来自 `timezoneOffset` 还是写死的 8；
 *   · 「保存更改」按钮是否真的发出 PUT、body 是不是 `timezone_offset`；
 *   · 保存后刷新页面还能不能读到新值（持久化 vs 只改了本地 state）；
 *   · 「重算历史」的**确认框**（＝迁移向导的预览环节）在取消时是否零写入；
 *   · `needsMigration` 提示条是不是真的由后端标记驱动；
 *   · 「先迁移后改时区」与「先改时区后迁移」的终态是否真的不同（顺序坑）。
 *
 * ── 桩的边界（R3：不许桩自证）─────────────────────────────────
 * 本 spec 断的是**页面对后端返回的处理**与**页面发出的请求体**，不断桩自己
 * 造的数字（`rows_deleted=3` / `backfill.scanned=128` 之类）。桩的形状依据见
 * `static-server.mjs` 顶部注释（settings.rs:626-637 / logs.rs:695-750）。
 *
 * ── 顺序坑的真实契约（读后端实现，不猜）───────────────────────
 * `handle_agg_tz_migrate` 收 `offset_to`（缺省＝当前进程快照）与 `offset_from`
 * （缺省 0），**第一步**就校验 `offset_to == effectiveOffset`，不等则 400 且零写入。
 * 前端面板只发 `offset_from`（`ReportSettingsPanel.runMigrate` →
 * `api.migrateAggTimezone({ offset_from })`），所以：
 *   · 页面本身**无法**触发那条 400（顺序坑由「先保存」这条 UI 纪律承担）；
 *   · 但顺序的**后果**是可观测的：快照=8 时先迁移 ⇒ 标记落 8；此后再保存 5，
 *     存量标记停在 8、`needsMigration` 又变 true（还需要再迁移一次）。
 *     反过来先保存 5 再迁移 ⇒ 标记落 5、`needsMigration` false。
 * 两条路径的终态不同，正是本文件第 5 条用例要钉的东西。
 */

const GET_SETTINGS = '/api/report/settings';
const MIGRATE = '/api/logs/agg/tz-migrate';
const RESET = '/api/__e2e/reset-report-tz';

/** 打开仪表盘并切到「报表设置」Tab（仅管理员可见该 Tab） */
async function openReportSettingsTab(page: Page): Promise<void> {
  await loginAsAdmin(page, { sessions: [] });
  await page.goto('/');
  // 报表面板首屏的锚点：等它出来才说明 admin 视图已就绪、Tab 已渲染
  await expect(page.locator('.main-content')).toContainText('Top 供应商');
  await switchToSettingsTab(page);
}

/**
 * 切到「报表设置」Tab。
 *
 * 视图状态（`view`）是组件 state、**不落 URL**（Dashboard.tsx:69
 * `useState<'report' | 'settings'>('report')`），所以刷新后必然弹回「报表面板」。
 * 任何 `page.reload()` 之后都要重新点一次 —— 这不是测试的绕路，而是页面
 * 现在真实的导航行为；把「刷新后还在设置 Tab」写进断言反而是错的。
 */
async function switchToSettingsTab(page: Page): Promise<void> {
  await page.getByRole('tab', { name: '报表设置' }).click();
  await expect(page.locator('.main-content')).toContainText('报表时区');
}

/** 「报表时区偏移」输入框（按 label 取，不依赖 form-group 的序号） */
function offsetInput(page: Page) {
  return page
    .locator('.form-group')
    .filter({ has: page.locator('label', { hasText: '报表时区偏移' }) })
    .locator('input');
}

/** 「重算旧口径」输入框 */
function offsetFromInput(page: Page) {
  return page
    .locator('.form-group')
    .filter({ has: page.locator('label', { hasText: '重算旧口径' }) })
    .locator('input');
}

/** `needsMigration` 提示条（唯一判据：后端 needsMigration） */
function migrationCanary(page: Page) {
  return page.locator('.badge-warning', { hasText: '存量聚合行口径与当前生效口径不一致' });
}

/** 迁移确认框（＝迁移向导的预览环节；role="dialog" 挂在 .modal-overlay 上） */
function confirmDialog(page: Page) {
  return page.locator('.modal-overlay[role="dialog"]', { hasText: '重算历史聚合行' });
}

/** 收集发往 tz-migrate 的请求体（零写入判据：预览期间必须是空数组） */
function watchMigrate(page: Page): Array<Record<string, unknown>> {
  const bodies: Array<Record<string, unknown>> = [];
  page.on('request', (r: Request) => {
    if (!r.url().includes(MIGRATE)) return;
    try {
      bodies.push((r.postDataJSON() ?? {}) as Record<string, unknown>);
    } catch {
      bodies.push({ __unparsable: true });
    }
  });
  return bodies;
}

/** 收集 PUT /api/report/settings 的请求体 */
function watchSave(page: Page): Array<Record<string, unknown>> {
  const bodies: Array<Record<string, unknown>> = [];
  page.on('request', (r: Request) => {
    if (!r.url().endsWith(GET_SETTINGS) || r.method() !== 'PUT') return;
    try {
      bodies.push((r.postDataJSON() ?? {}) as Record<string, unknown>);
    } catch {
      bodies.push({ __unparsable: true });
    }
  });
  return bodies;
}

/** 直读桩状态（用于断「预览期间后端真的没动」） */
async function readSettings(page: Page): Promise<Record<string, unknown>> {
  const res = await page.request.get(GET_SETTINGS);
  expect(res.status()).toBe(200);
  return (await res.json()).data as Record<string, unknown>;
}

test.describe('报表设置 Tab · 时区 UI', () => {
  // 时区桩是有状态的（PUT 改快照、迁移落标记），desktop / mobile 两个 project
  // 串行共用同一桩进程 ⇒ 每条用例前复位，防跨用例/跨 project 残留。
  test.beforeEach(async ({ page }) => {
    const res = await page.request.post(RESET);
    expect(res.ok(), `复位端点失败: ${res.status()}`).toBeTruthy();
  });

  test('初值来自后端：输入框 / 生效口径 / 存量口径 / 迁移提示条四者对齐 GET', async ({ page, pageErrors }) => {
    await openReportSettingsTab(page);

    // 输入框初值 = timezoneOffset（老库升级后 = 8）。写死 8 的实现也能过这条，
    // 所以下面第 2 条要真的改成 5 再刷新 —— 两条合起来才排得掉「硬编码」。
    await expect(offsetInput(page)).toHaveValue('8');
    await expect(offsetFromInput(page)).toHaveValue('0');

    // 生效口径读的是 effectiveOffset（进程快照），不是 timezoneOffset
    await expect(page.locator('.main-content')).toContainText('UTC+8（进程快照）');
    // 存量口径 storedOffset=null ⇒ 显式「未知」，不是空白也不是 UTC+0
    await expect(page.locator('.main-content')).toContainText('未知（老库从未记录口径）');
    // needsMigration=true（老库）⇒ 提示条在场
    await expect(migrationCanary(page)).toBeVisible();

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('保存即热生效且刷新后仍是新值（PUT body 与页面读数同源）', async ({ page, pageErrors }) => {
    const puts = watchSave(page);
    await openReportSettingsTab(page);

    await offsetInput(page).fill('5');
    await page.getByRole('button', { name: '保存更改' }).click();

    // 请求体形状：snake_case `timezone_offset`（后端 ReportConfig 的 serde 口径）
    await expect.poll(() => puts.length).toBe(1);
    expect(puts[0]).toEqual({ timezone_offset: 5 });

    // 保存响应回灌：生效口径立刻变成 UTC+5（热生效，不重启不重读）
    await expect(page.locator('.main-content')).toContainText('UTC+5（进程快照）');
    await expect(page.locator('.toast', { hasText: '报表设置已保存' })).toBeVisible();

    // 刷新页面：初值必须是**后端**的 5，不是组件默认 8
    await page.reload();
    await switchToSettingsTab(page);
    await expect(offsetInput(page)).toHaveValue('5');
    await expect(page.locator('.main-content')).toContainText('UTC+5（进程快照）');

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('越界输入拦在提交前（不发 PUT），改回合法值就能发', async ({ page, pageErrors }) => {
    const puts = watchSave(page);
    await openReportSettingsTab(page);

    // 15 超出 maxOffset=14。数字输入框的 max 属性在 fill 时**不拦**，拦人的是
    // 提交前的整数+值域校验 —— 这里断的正是那一步（删掉校验就会发出非法 PUT）。
    await offsetInput(page).fill('15');
    await page.getByRole('button', { name: '保存更改' }).click();
    await expect(page.locator('.error-message')).toContainText('报表时区偏移必须是 -12~14 之间的整数');
    expect(puts, '越界值被拦住了却仍然发了 PUT').toEqual([]);

    // 小数同理（真实后端对非整数回 422，前端不该把 422 当正常流程）
    await offsetInput(page).fill('8.5');
    await page.getByRole('button', { name: '保存更改' }).click();
    await expect(page.locator('.error-message')).toContainText('报表时区偏移必须是 -12~14 之间的整数');
    expect(puts).toEqual([]);

    // 正对照：合法值必须发得出去（否则上面两条可能只是「按钮坏了」）
    await offsetInput(page).fill('9');
    await page.getByRole('button', { name: '保存更改' }).click();
    await expect.poll(() => puts.length).toBe(1);
    expect(puts[0]).toEqual({ timezone_offset: 9 });

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('迁移向导：预览阶段零写入；确认后只发一次且只带 offset_from；结果面板渲染四条坑', async ({ page, pageErrors }) => {
    const migrates = watchMigrate(page);
    await openReportSettingsTab(page);

    // 向导前置：先保存目标偏移（顺序坑的正面要求）
    await offsetInput(page).fill('5');
    await page.getByRole('button', { name: '保存更改' }).click();
    await expect(page.locator('.main-content')).toContainText('UTC+5（进程快照）');

    const before = await readSettings(page);

    // ── 预览：点「重算历史」只开确认框，此时**一个写请求都不许发** ──
    await page.getByRole('button', { name: '重算历史' }).click();
    const dialog = confirmDialog(page);
    await expect(dialog).toBeVisible();
    // 向导正文要写明两条边界（切换当日偏低 / 顺序坑），不能只是个「确定吗？」
    await expect(dialog).toContainText('切换当日报表会偏低、次日自愈');
    await expect(dialog).toContainText('请先保存偏移再重算');
    expect(migrates, '预览阶段就发出了迁移请求（＝没预览直接写）').toEqual([]);
    // 后端状态也没动（storedOffset 仍 null、needsMigration 仍 true）
    expect(await readSettings(page)).toEqual(before);

    // 取消：同样零写入，且确认框关掉
    await dialog.getByRole('button', { name: '取消' }).click();
    await expect(dialog).toBeHidden();
    expect(migrates).toEqual([]);
    expect(await readSettings(page)).toEqual(before);
    // 提示条还在（因为真的没迁移）
    await expect(migrationCanary(page)).toBeVisible();

    // ── 确认：只发一次，body 只有 offset_from（offset_to 交给后端缺省＝快照）──
    await page.getByRole('button', { name: '重算历史' }).click();
    await expect(confirmDialog(page)).toBeVisible();
    await confirmDialog(page).getByRole('button', { name: '开始重算' }).click();
    await expect.poll(() => migrates.length).toBe(1);
    expect(migrates[0]).toEqual({ offset_from: 0 });

    // 结果面板：成功摘要 + 后端四条坑原样渲染（含内嵌的 UTC+5 与日期）
    await expect(page.locator('.main-content')).toContainText('重算完成');
    await expect(page.locator('.main-content')).toContainText('删除旧口径聚合行：3 行');
    await expect(page.locator('.main-content')).toContainText('窗口已应用（首次重算）');
    const noticeBox = page.locator('.main-content ul li');
    await expect(noticeBox).toHaveCount(4);
    await expect(noticeBox.nth(0)).toContainText('顺序坑');
    await expect(noticeBox.nth(0)).toContainText('UTC+5');
    await expect(noticeBox.nth(1)).toContainText('分裂窗口');
    await expect(noticeBox.nth(2)).toContainText('切换当日偏低');
    await expect(noticeBox.nth(3)).toContainText('skipped_shrink');

    // 迁移落了标记 ⇒ 页面重读后提示条消失、存量口径变成 UTC+5
    await expect(migrationCanary(page)).toBeHidden();
    await expect(page.locator('.main-content')).toContainText('UTC+5（进程快照）');

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('顺序坑：先迁移后改时区 ⇒ 标记停在旧口径、提示条复活；先改后迁移 ⇒ 干净', async ({ page, pageErrors }) => {
    const migrates = watchMigrate(page);
    await openReportSettingsTab(page);

    // ── 路径 B（错的顺序）：快照=8 时先迁移，再改时区到 5 ──
    await page.getByRole('button', { name: '重算历史' }).click();
    await confirmDialog(page).getByRole('button', { name: '开始重算' }).click();
    await expect.poll(() => migrates.length).toBe(1);
    // 迁移用的是**当前快照 8**（offset_to 缺省），旧口径 0 ⇒ 存量标记落 8
    expect(migrates[0]).toEqual({ offset_from: 0 });
    await expect(migrationCanary(page)).toBeHidden();

    await offsetInput(page).fill('5');
    await page.getByRole('button', { name: '保存更改' }).click();
    await expect(page.locator('.main-content')).toContainText('UTC+5（进程快照）');
    // 关键：改完时区后存量标记（8）与新生效口径（5）又错开 ⇒ 提示条必须复活。
    // 若实现把 needsMigration 当成本地 state 记住「迁移过了」，这里会红。
    await expect(migrationCanary(page)).toBeVisible();
    const afterWrongOrder = await readSettings(page);
    expect(afterWrongOrder).toMatchObject({ storedOffset: 8, effectiveOffset: 5, needsMigration: true });

    // ── 复位后走路径 A（对的顺序）：先改时区到 5，再迁移 ──
    await page.request.post(RESET);
    await page.reload();
    await switchToSettingsTab(page);
    await expect(offsetInput(page)).toHaveValue('8');
    migrates.length = 0;

    await offsetInput(page).fill('5');
    await page.getByRole('button', { name: '保存更改' }).click();
    await expect(page.locator('.main-content')).toContainText('UTC+5（进程快照）');
    // 只保存、没迁移：标记仍是 null ⇒ 提示条还在（这里不能提前消失）
    await expect(migrationCanary(page)).toBeVisible();

    await page.getByRole('button', { name: '重算历史' }).click();
    await confirmDialog(page).getByRole('button', { name: '开始重算' }).click();
    await expect.poll(() => migrates.length).toBe(1);
    await expect(migrationCanary(page)).toBeHidden();
    const afterRightOrder = await readSettings(page);
    expect(afterRightOrder).toMatchObject({ storedOffset: 5, effectiveOffset: 5, needsMigration: false });

    // ── 两条顺序的终态必须不同（这就是「顺序坑」的可验证含义）──
    expect(afterWrongOrder.storedOffset).not.toBe(afterRightOrder.storedOffset);
    expect(afterWrongOrder.needsMigration).toBe(true);
    expect(afterRightOrder.needsMigration).toBe(false);

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });
});
