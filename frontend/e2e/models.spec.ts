import { test, expect, loginAsAdmin } from './fixtures';

/**
 * 模型管理页 —— 功能级回归。
 *
 * 覆盖「渠道 + 账号池模型合流」这个核心改造（Agent2API 同款信息架构）：
 * - 左栏按 provider 分组与计数（monogram 图标）
 * - 聚合视图分组带 + 组内折叠
 * - 来源徽标区分渠道 / 账号
 * - 启停开关可点且请求打到 /api/models/state
 * - 未定价模型高亮 + 头部统计
 * - 搜索与状态筛选真的过滤行
 * - 操作列「测试」打开模型测试弹窗，结果按来源分块渲染
 */

/** 数据行（排除分组带 / 展开折叠行） */
const dataRows = (page: import('@playwright/test').Page) =>
  page.locator('.mm-table tbody tr:not(.mm-tr-group):not(.mm-tr-more)');

test.describe('模型管理', () => {
  test.beforeEach(async ({ page }) => {
    await loginAsAdmin(page, { sessions: [] });
    await page.goto('/model-manage');
    await expect(page.locator('h1')).toContainText('模型管理');
  });

  test('左栏分三段：渠道供应商 / 账号池提供商 / 自定义提供商', async ({ page, pageErrors }) => {
    const rail = page.locator('.mm-rail');
    await expect(rail).toBeVisible();

    // 三个分段标题都在
    await expect(rail).toContainText('渠道供应商');
    await expect(rail).toContainText('账号池提供商');
    await expect(rail).toContainText('自定义提供商');

    // 「全部」+ 两个渠道供应商（mock 里 ch-1 lxchat-glm / ch-2 backup-channel）
    // + 账号池提供商 1 个（workbuddy，account 来源）
    await expect(rail).toContainText('lxchat-glm');
    await expect(rail).toContainText('backup-channel');
    await expect(rail).toContainText('WorkBuddy 国内版');

    // 「全部」+ 3 家各一枚 monogram 图标
    await expect(rail.locator('.mm-mono')).toHaveCount(4);
    // 自定义提供商暂为空 → 有提示文案
    await expect(rail.locator('.mm-rail-empty')).toHaveCount(1);

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('点渠道供应商只显示该渠道的模型', async ({ page, pageErrors }) => {
    // lxchat-glm 只承载 glm-4.7-flash
    await page.locator('.mm-rail-item', { hasText: 'lxchat-glm' }).click();
    await expect(dataRows(page)).toHaveCount(1);
    await expect(page.locator('.mm-table')).toContainText('glm-4.7-flash');
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('聚合视图按提供商分组，数据行与来源徽标正确', async ({ page, pageErrors }) => {
    // 「全部」视图 = 分组模式：2 个分组带（openai_compatible / workbuddy）
    await expect(page.locator('.mm-tr-group')).toHaveCount(2);
    await expect(dataRows(page)).toHaveCount(3);

    // 渠道来源与账号来源的徽标
    await expect(page.locator('.mm-src-tag.provider', { hasText: 'lxchat-glm' })).toBeVisible();
    await expect(page.locator('.mm-src-tag:not(.provider)', { hasText: 'workbuddy-1' })).toBeVisible();

    // 未定价模型显示占位
    await expect(page.locator('.mm-price-empty')).toHaveCount(1);
    await expect(dataRows(page).filter({ hasText: 'unpriced-model' })).toContainText('未定价');

    // 已定价模型显示输入/输出价
    await expect(dataRows(page).filter({ hasText: 'glm-4.7-flash' })).toContainText('0.001');

    // 头部统计行（对齐 Agent2API mm-meta：总数 / 已启用 / 有映射 / 未定价）
    await expect(page.locator('.mm-titlebar .mm-meta')).toContainText('3');
    await expect(page.locator('.mm-titlebar .mm-meta')).toContainText('已启用');
    await expect(page.locator('.mm-titlebar .mm-meta')).toContainText('有映射');

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('点击左栏分组只显示该组模型', async ({ page, pageErrors }) => {
    await page.locator('.mm-rail-item', { hasText: 'WorkBuddy 国内版' }).click();
    await expect(dataRows(page)).toHaveCount(1);
    await expect(page.locator('.mm-table')).toContainText('wb-coder');

    await page.locator('.mm-rail-item', { hasText: '全部' }).click();
    await expect(dataRows(page)).toHaveCount(3);

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('搜索过滤模型行', async ({ page, pageErrors }) => {
    const search = page.locator('.mm-search input.form-input').first();
    await search.fill('glm');
    await expect(dataRows(page)).toHaveCount(1);
    await expect(page.locator('.mm-table')).toContainText('glm-4.7-flash');

    await search.fill('不存在的模型');
    // 无匹配：显示空态，无数据行
    await expect(dataRows(page)).toHaveCount(0);

    await search.fill('');
    await expect(dataRows(page)).toHaveCount(3);
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('状态筛选「有映射」只显示带别名的行', async ({ page, pageErrors }) => {
    await page.locator('.mm-seg-btn', { hasText: '有映射' }).click();
    await expect(dataRows(page)).toHaveCount(1);
    await expect(page.locator('.mm-table')).toContainText('unpriced-model');
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('启停开关可用，请求打到 /api/models/state', async ({ page, pageErrors }) => {
    const stateCalls: string[] = [];
    page.on('request', (r) => {
      if (r.url().includes('/api/models/state')) stateCalls.push(r.method());
    });

    const row = dataRows(page).filter({ hasText: 'unpriced-model' });
    // 行内第一个复选框是「批量选择」，启停是 role=switch 的独立控件。
    // 映射列里也有 role=switch（映射开关），所以按列定位——第 2 个 td 是启停列。
    const toggle = row.locator('td').nth(1).getByRole('switch');
    await expect(toggle).toBeVisible();
    await toggle.click();

    await expect.poll(() => stateCalls.length, { timeout: 5000 }).toBeGreaterThan(0);
    expect(stateCalls[0]).toBe('POST');
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('映射列：映射 chips —— 开关可切、关闭态划线、行内 + 映射入口', async ({ page, pageErrors }) => {
    const aliasCalls: string[] = [];
    page.on('request', (r) => {
      if (r.url().includes('/api/models/alias')) aliasCalls.push(r.url());
    });

    const row = dataRows(page).filter({ hasText: 'unpriced-model' });
    const cell = row.locator('.mm-alias-cell');
    await expect(cell).toBeVisible();

    // 两条映射：一条启用（alias-free）、一条关闭（alias-off）
    const chips = cell.locator('.mm-alias');
    await expect(chips).toHaveCount(2);
    await expect(cell.locator('.mm-alias.is-off')).toHaveCount(1);
    await expect(cell.locator('.mm-alias.is-off')).toContainText('alias-off');

    // 每条 chip 都有开关
    await expect(cell.locator('.mm-alias').first().getByRole('switch')).toBeVisible();

    // 行内「+ 映射」入口存在
    await expect(cell.locator('.mm-alias-add')).toBeVisible();

    // 切开关：请求打到 /api/models/alias/state
    await cell.locator('.mm-alias').first().getByRole('switch').click();
    await expect
      .poll(() => aliasCalls.filter((u) => u.includes('/alias/state')).length, { timeout: 5000 })
      .toBeGreaterThan(0);

    // 点「+ 映射」打开弹窗，且带实时预览区
    await cell.locator('.mm-alias-add').click();
    const modal = page.locator('.modal-overlay .modal').first();
    await expect(modal).toBeVisible();
    const preview = modal.locator('.mm-alias-preview');
    await expect(preview).toBeVisible();
    // 弹窗默认把本行模型填进「转发到上游模型」（Combobox 的输入框）
    await expect(modal.locator('.cb-input').first()).toHaveValue('unpriced-model');
    // 填上对外名后，预览立刻显示完整链路
    await modal.locator('input.form-input').first().fill('alias-new');
    await expect(preview).toContainText('alias-new');
    await expect(preview).toContainText('unpriced-model');

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('操作列「测试」打开模型测试弹窗，按来源并发并渲染结论', async ({ page, pageErrors }) => {
    const testCalls: string[] = [];
    page.on('request', (r) => {
      if (r.url().includes('/api/models/test')) testCalls.push(r.method());
    });

    // wb-coder 行有一个账号来源（workbuddy-1）
    const row = dataRows(page).filter({ hasText: 'wb-coder' });
    await row.locator('.mm-act-btn', { hasText: '测试' }).click();

    const modal = page.locator('.mt-modal');
    await expect(modal).toBeVisible();
    // 目标行预览 + 默认勾选了唯一来源
    await expect(modal.locator('.mt-target')).toContainText('wb-coder');
    await expect(modal.locator('.mt-source.on')).toHaveCount(1);

    await modal.locator('.btn-primary', { hasText: '开始测试' }).click();

    // 结果层：成功徽标（用时/首字）+ 回复正文 + tokens 行
    const results = modal.locator('.mt-results');
    await expect(results).toBeVisible();
    await expect(results.locator('.mm-badge-ok')).toContainText('200');
    await expect(results.locator('.mt-reply')).toContainText('你好！我是测试回复。');
    await expect(results.locator('.mt-slot-meta')).toContainText('tokens: 2 + 9 = 11');

    expect(testCalls.length).toBeGreaterThan(0);
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('勾选后出现批量条，批量操作弹窗列出动作', async ({ page, pageErrors }) => {
    await dataRows(page).first().locator('td').nth(0).locator('input[type=checkbox]').click();
    await expect(page.locator('.mm-batch-chip')).toContainText('已选');

    await page.locator('.mm-batch-chip .btn', { hasText: '批量操作' }).click();
    const modal = page.locator('.modal-overlay .modal').last();
    await expect(modal).toBeVisible();
    await expect(modal).toContainText('批量操作');
    await expect(modal.locator('.mm-batch-models code')).toHaveCount(1);
    await expect(modal.locator('.mm-batch-action')).toHaveCount(4);

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('列设置可以关掉「定价」列', async ({ page, pageErrors }) => {
    await expect(page.locator('.mm-h-price')).toHaveCount(1);
    await page.locator('.mm-colset .btn').click();
    const panel = page.locator('.mm-colset-panel');
    await expect(panel).toBeVisible();
    await panel.locator('.mm-colset-row', { hasText: '定价' }).getByRole('switch').click();
    await expect(page.locator('.mm-h-price')).toHaveCount(0);
    // 恢复默认，避免 localStorage 影响后续用例
    await panel.locator('.mm-colset-reset').click();
    await expect(page.locator('.mm-h-price')).toHaveCount(1);

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('列设置：对齐三档作用于表头与单元格，锁定列不可关', async ({ page, pageErrors }) => {
    await page.locator('.mm-colset .btn').click();
    const panel = page.locator('.mm-colset-panel');
    await expect(panel).toBeVisible();

    // 定价列 → 居中（默认左）
    const priceRow = panel.locator('.mm-colset-row', { hasText: '定价' });
    await priceRow.getByRole('button', { name: '居中' }).click();
    await expect(page.locator('.mm-h-price')).toHaveClass(/ta-center/);

    // 勾列（锁定）→ 改左对齐；默认是居中
    const checkRow = panel.locator('.mm-colset-row', { hasText: '选择' });
    await checkRow.getByRole('button', { name: '左', exact: true }).click();
    await expect(page.locator('.mm-h-check')).toHaveClass(/ta-left/);

    // 锁定列（选择/模型/操作）开关禁用，点不动
    await expect(checkRow.getByRole('switch')).toBeDisabled();
    await expect(panel.locator('.mm-colset-row', { hasText: '模型' }).getByRole('switch')).toBeDisabled();

    // 恢复默认：对齐回到各列声明值
    await panel.locator('.mm-colset-reset').click();
    await expect(page.locator('.mm-h-price')).toHaveClass(/ta-left/);
    await expect(page.locator('.mm-h-check')).toHaveClass(/ta-center/);

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('元信息覆盖 Tab 可切换并渲染', async ({ page, pageErrors }) => {
    await page.getByRole('tab', { name: '元信息覆盖' }).click();
    const text = await page.locator('.main-content').innerText();
    expect(text.length).toBeGreaterThan(10);
    // 目录 Tab 的布局不应再显示
    await expect(page.locator('.mm-layout')).toHaveCount(0);
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);

    await page.getByRole('tab', { name: '模型目录' }).click();
    await expect(page.locator('.mm-layout')).toBeVisible();
  });

  test('定价倍率 Tab 渲染倍率表格（旧 /pricing 已并入此处）', async ({ page, pageErrors }) => {
    await page.getByRole('tab', { name: '定价倍率' }).click();
    // 面板骨架：标题栏 + 两张倍率表（模型倍率 / 分组倍率）
    await expect(page.locator('.ratio-panel')).toBeVisible();
    await expect(page.locator('.ratio-panel')).toContainText('模型倍率');
    await expect(page.locator('.ratio-panel')).toContainText('分组倍率');
    // 目录 Tab 的左栏布局不应再显示
    await expect(page.locator('.mm-layout')).toHaveCount(0);

    await page.getByRole('tab', { name: '模型目录' }).click();
    await expect(page.locator('.mm-layout')).toBeVisible();
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('旧 /pricing 书签重定向到模型管理倍率 Tab', async ({ page, pageErrors }) => {
    await page.goto('/pricing');
    await expect(page).toHaveURL(/\/model-manage\?tab=ratio$/);
    await expect(page.locator('.ratio-panel')).toBeVisible();
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });
});

test.describe('模型管理 · 思考等级下拉与悬空映射', () => {
  test.beforeEach(async ({ page }) => {
    await loginAsAdmin(page, { sessions: [] });
    await page.goto('/model-manage');
    await expect(page.locator('h1')).toContainText('模型管理');
  });

  test('映射弹窗思考等级下拉：候选来自模型声明档位 + 自定义入口', async ({ page, pageErrors }) => {
    // unpriced-model 行的「+ 映射」入口打开弹窗（无声明 → 兜底表候选）
    const row = dataRows(page).filter({ hasText: 'unpriced-model' });
    await row.locator('.mm-alias-add').click();
    const modal = page.locator('.modal-overlay .modal').first();
    await expect(modal).toBeVisible();

    // 等级下拉存在且带「不覆盖」空档（弹窗内唯一原生 select）
    const levelSelect = modal.locator('select').first();
    await expect(levelSelect).toBeVisible();
    await expect(levelSelect.locator('option', { hasText: '不覆盖' })).toHaveCount(1);
    // 无声明的模型 → 兜底候选表里有 high（按 value 匹配，防 xhigh 子串误中）
    await expect(levelSelect.locator('option[value="high"]')).toHaveCount(1);
    // 自定义等级哨兵档存在
    await expect(levelSelect.locator('option', { hasText: '自定义等级' })).toHaveCount(1);

    // 选自定义 → 输入框露出
    await levelSelect.selectOption('__custom__');
    await expect(modal.locator('input[placeholder*="自定义等级"]')).toBeVisible();
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('映射弹窗思考等级：目标模型有声明的候选档位', async ({ page, pageErrors }) => {
    // 双击 glm-4.7-flash 的映射名打开弹窗（无声明的模型走「+ 映射」；
    // 声明档位的模型用别名弹窗的目标框切换验证）
    const row = dataRows(page).filter({ hasText: 'unpriced-model' });
    await row.locator('.mm-alias-add').click();
    const modal = page.locator('.modal-overlay .modal').first();
    await expect(modal).toBeVisible();

    // 把目标模型切成有声明的 glm-4.7-flash：候选应换成声明的 low/medium/high
    // （Combobox 输入即过滤，Enter 选定过滤后的第一项；idx 初始即 0）
    const combo = modal.locator('.cb-input').first();
    await combo.fill('glm-4.7-flash');
    await page.keyboard.press('Enter');

    const levelSelect = modal.locator('select').first();
    await expect(levelSelect.locator('option[value="low"]')).toHaveCount(1);
    await expect(levelSelect.locator('option[value="medium"]')).toHaveCount(1);
    // 声明表里没有的档位不该出现（兜底表的 minimal/off 不在）
    await expect(levelSelect.locator('option[value="minimal"]')).toHaveCount(0);
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('悬空映射提醒区：列出无宿主行的映射，删除入口可达', async ({ page, pageErrors }) => {
    // 桩 mappings 里 alias-dangling 指向 gone-model（不在 models 清单）
    const section = page.locator('.mm-dangling');
    await expect(section).toBeVisible();
    await expect(section).toContainText('未挂载的模型映射');
    await expect(section.locator('.mm-dg-item')).toHaveCount(1);
    await expect(section.locator('.mm-dg-item')).toContainText('alias-dangling');
    await expect(section.locator('.mm-dg-item')).toContainText('gone-model');
    await expect(section.locator('.mm-dg-item')).toContainText('目标已下线');

    // 正常映射（alias-free → unpriced-model 有宿主行）不进悬空区
    await expect(section.locator('.mm-dg-item', { hasText: 'alias-free' })).toHaveCount(0);
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });
});
