import { test, expect, loginAsAdmin } from './fixtures';

/**
 * 渠道列设置面板 E2E。
 *
 * 面板结构对齐模型管理页：四列网格（手柄 / 列名 / 显示 / 对齐）+ 拖拽排序。
 * 覆盖：面板骨架、对齐三档落盘、显隐开关、恢复默认、拖拽排序落盘。
 *
 * 数据桩：static-server.mjs 的 `/api/channels` 返回三条样本，使表格渲染，
 * 否则 `channels.length === 0` 时整张表不渲染，列设置面板无法在真实表头上验证。
 */

const COLCFG_KEY = 'aigx-ch-colcfg';
const PANEL_TOGGLE = '.ch-colset > button';

/** 打开列设置面板，等表格数据渲染出来（面板在工具栏，表头异步加载） */
async function openPanel(page: import('@playwright/test').Page): Promise<void> {
  // 等表格表头出现，确认渠道数据已加载
  await expect(page.locator('th.col-name')).toBeVisible();
  await page.locator(PANEL_TOGGLE).click();
  await expect(page.locator('.ch-colset-panel')).toBeVisible();
}

/** 读面板内行的 DOM 顺序 */
function domOrder(page: import('@playwright/test').Page): Promise<string[]> {
  return page.$$eval('.ch-colset-rows [data-col-key]', (els) =>
    els.map((e) => e.getAttribute('data-col-key') as string),
  );
}

/** 读落盘的列配置数组 */
function readColcfg(page: import('@playwright/test').Page): Promise<Array<{ key: string; visible: boolean; align: string }>> {
  return page.evaluate((k) => JSON.parse(localStorage.getItem(k) || 'null'), COLCFG_KEY);
}

test.describe('渠道列设置面板', () => {
  test('面板骨架：四列表头 + 十行，select/actions 不进面板', async ({ page, pageErrors }) => {
    await loginAsAdmin(page);
    await page.goto('/channels');
    await openPanel(page);

    // 表头四格：手柄(空) / 列 / 显示 / 对齐
    const headCells = await page.$$eval('.ch-colset-head > span', (els) => els.map((e) => e.textContent));
    expect(headCells).toHaveLength(4);
    expect(headCells[1]).toBe('列');
    expect(headCells[2]).toBe('显示');
    expect(headCells[3]).toBe('对齐');

    // 十行业务列，select / actions 不参与
    const order = await domOrder(page);
    expect(order).toHaveLength(10);
    expect(order).not.toContain('select');
    expect(order).not.toContain('actions');
    expect(order).toEqual([
      'id', 'name', 'type', 'status', 'models',
      'priority', 'weight', 'balance', 'response', 'testtime',
    ]);

    // 每行四格：手柄 / 名称 / 开关 / 对齐三档
    const firstRow = page.locator('[data-col-key="id"]');
    await expect(firstRow.locator('.ch-colset-grip')).toBeVisible();
    await expect(firstRow.locator('.ch-colset-name')).toBeVisible();
    await expect(firstRow.locator('[role="switch"]')).toBeVisible();
    await expect(firstRow.locator('.ch-seg-mini .ch-seg-btn')).toHaveCount(3);

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('对齐三档：点居中 → 表头与单元格挂 ta-center 且落盘', async ({ page, pageErrors }) => {
    await loginAsAdmin(page);
    await page.goto('/channels');
    await openPanel(page);

    // 响应列点「居中」
    const respAlign = page.locator('[data-col-key="response"] .ch-seg-mini .ch-seg-btn').filter({ hasText: '居中' });
    await respAlign.click();

    // 表头 th 与数据 td 都挂 ta-center
    const thCls = await page.locator('th.col-response').evaluate((el) => el.className);
    expect(thCls).toContain('ta-center');
    const tdCls = await page.locator('td.col-response').first().evaluate((el) => el.className);
    expect(tdCls).toContain('ta-center');

    // 落盘：响应列 align=center
    const colcfg = await readColcfg(page);
    const resp = colcfg.find((c) => c.key === 'response');
    expect(resp).toBeDefined();
    expect(resp!.align).toBe('center');

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('显隐开关：关权重 → 表头消失、单元格 ch-hidden 且落盘', async ({ page, pageErrors }) => {
    await loginAsAdmin(page);
    await page.goto('/channels');
    await openPanel(page);

    // 权重列开关点一下（关）
    const weightSwitch = page.locator('[data-col-key="weight"] [role="switch"]');
    await expect(weightSwitch).toHaveAttribute('aria-checked', 'true');
    await weightSwitch.click();

    // 表头被过滤掉（隐藏列不渲染 th）
    await expect(page.locator('th.col-weight')).toHaveCount(0);
    // 单元格仍渲染但带 ch-hidden（display:none，不计入布局）
    const tdCls = await page.locator('td.col-weight').first().evaluate((el) => el.className);
    expect(tdCls).toContain('ch-hidden');

    // 落盘：权重 visible=false
    const colcfg = await readColcfg(page);
    const weight = colcfg.find((c) => c.key === 'weight');
    expect(weight).toBeDefined();
    expect(weight!.visible).toBe(false);

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('恢复默认：顺序/显隐/对齐一起还原', async ({ page, pageErrors }) => {
    await loginAsAdmin(page);
    await page.goto('/channels');
    await openPanel(page);

    // 先改两处：响应列居中、权重列隐藏
    await page.locator('[data-col-key="response"] .ch-seg-mini .ch-seg-btn').filter({ hasText: '居中' }).click();
    await page.locator('[data-col-key="weight"] [role="switch"]').click();

    // 点恢复默认
    await page.locator('.ch-colset-reset').click();

    // 权重表头回来
    await expect(page.locator('th.col-weight')).toBeVisible();
    // 响应列回到默认左对齐
    const thCls = await page.locator('th.col-response').evaluate((el) => el.className);
    expect(thCls).toContain('ta-left');
    expect(thCls).not.toContain('ta-center');

    // 落盘：顺序回到默认、全可见、全左对齐
    const colcfg = await readColcfg(page);
    expect(colcfg.map((c) => c.key)).toEqual([
      'id', 'name', 'type', 'status', 'models',
      'priority', 'weight', 'balance', 'response', 'testtime',
    ]);
    expect(colcfg.every((c) => c.visible)).toBe(true);
    expect(colcfg.every((c) => c.align === 'left')).toBe(true);

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('拖拽排序：weight 拖到 name 上方 → 顺序变化且与落盘一致', async ({ page, pageErrors }) => {
    await loginAsAdmin(page);
    await page.goto('/channels');
    await openPanel(page);

    const src = page.locator('[data-col-key="weight"] .ch-colset-grip');
    const dst = page.locator('[data-col-key="name"] .ch-colset-grip');
    await src.scrollIntoViewIfNeeded();
    await dst.scrollIntoViewIfNeeded();

    const sb = await src.boundingBox();
    const db = await dst.boundingBox();
    expect(sb).not.toBeNull();
    expect(db).not.toBeNull();

    const sx = sb!.x + sb!.width / 2;
    const sy = sb!.y + sb!.height / 2;
    const dx = db!.x + db!.width / 2;
    const dy = db!.y + db!.height / 2;

    await page.mouse.move(sx, sy);
    await page.mouse.down();
    // 单步直达目标：避免中途跨行反复重排，落点确定
    await page.mouse.move(dx, dy, { steps: 1 });
    await page.mouse.up();

    // 顺序变了：weight 提到 name 之前
    const order = await domOrder(page);
    expect(order.indexOf('weight')).toBeLessThan(order.indexOf('name'));
    // 且不再是默认顺序
    expect(order).not.toEqual([
      'id', 'name', 'type', 'status', 'models',
      'priority', 'weight', 'balance', 'response', 'testtime',
    ]);

    // 落盘顺序与 DOM 一致
    const colcfg = await readColcfg(page);
    expect(colcfg.map((c) => c.key)).toEqual(order);

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });
});
