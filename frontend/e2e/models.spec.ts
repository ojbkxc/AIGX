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

  test('左栏分三段：供应商 / 账号池提供商 / 自定义提供商', async ({ page, pageErrors }) => {
    const rail = page.locator('.mm-rail');
    await expect(rail).toBeVisible();

    // 三个分段标题都在
    await expect(rail).toContainText('供应商');
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

  test('无「启用」列；「映射」列首颗是默认绑定开关，切换打到 /api/models/state', async ({ page, pageErrors }) => {
    const stateCalls: string[] = [];
    page.on('request', (r) => {
      if (r.url().includes('/api/models/state')) stateCalls.push(r.method());
    });

    // SPEC-1.0.46 §4.1 / §6.1：启用列已删（表头与单元格都不该有）
    await expect(page.locator('.mm-h-enable')).toHaveCount(0);
    await expect(page.locator('.mm-c-enable')).toHaveCount(0);
    await expect(page.locator('.mm-table thead')).not.toContainText('启用');

    const row = dataRows(page).filter({ hasText: 'glm-4.7-flash' });
    const cell = row.locator('.mm-alias-cell');
    // 映射列首颗 = 默认绑定 chip（id→id 自映射），带独立开关
    const binding = cell.locator('.mm-alias').first();
    await expect(binding).toHaveClass(/mm-binding-default/);
    await expect(binding).toContainText(/glm-4\.7-flash\s*→\s*glm-4\.7-flash/);
    await expect(binding.locator('.mm-alias-switch')).toBeVisible();

    await binding.locator('.mm-alias-switch').click();

    await expect.poll(() => stateCalls.length, { timeout: 5000 }).toBeGreaterThan(0);
    expect(stateCalls[0]).toBe('POST');
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('映射列：默认绑定 chip 常驻 + 真别名 chips（开关可切、关闭态划线、行内 + 映射入口）', async ({ page, pageErrors }) => {
    const aliasCalls: string[] = [];
    page.on('request', (r) => {
      if (r.url().includes('/api/models/alias')) aliasCalls.push(r.url());
    });

    const row = dataRows(page).filter({ hasText: 'unpriced-model' });
    const cell = row.locator('.mm-alias-cell');
    await expect(cell).toBeVisible();

    // 映射列 = 首颗默认绑定 chip（mm-binding-default，常驻）+ 两条真别名。
    // identity 提示 chip 已随默认绑定上线移除（SPEC §4.2），不再单独断言。
    const chips = cell.locator('.mm-alias');
    await expect(chips).toHaveCount(3);
    const binding = chips.first();
    await expect(binding).toHaveClass(/mm-binding-default/);
    await expect(binding).toContainText(/unpriced-model\s*→\s*unpriced-model/);
    await expect(binding.locator('.mm-binding-default-tag')).toContainText('默认');

    // 关闭态划线只命中 alias-off（默认绑定开着，不算 is-off）
    await expect(cell.locator('.mm-alias.is-off')).toHaveCount(1);
    await expect(cell.locator('.mm-alias.is-off')).toContainText('alias-off');

    // 真别名 chip 都有开关，且目标模型名可见（不再只在 tooltip 里）
    const freeChip = cell.locator('.mm-alias', { hasText: 'alias-free' });
    await expect(freeChip).toContainText(/alias-free\s*→\s*unpriced-model/);
    await expect(cell.locator('.mm-alias.is-off', { hasText: 'alias-off' })).toContainText(
      /alias-off\s*→\s*unpriced-model/,
    );

    // 行内「+ 映射」入口存在
    await expect(cell.locator('.mm-alias-add')).toBeVisible();

    // 切真别名开关（第二颗 = alias-free）→ 打到 /api/models/alias/state
    await chips.nth(1).getByRole('switch').click();
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

  test('默认绑定关掉 → 行压淡；行还有别名开着时**不**压淡（A2A 口径）', async ({ page, pageErrors }) => {
    // 自建视图：solo-model 无别名（关默认绑定即整行失活）；
    // mapped-model 默认绑定 + keep-alias 开着（关默认绑定后行仍生效）。
    const view = {
      providers: [{ id: 'openai_compatible', label: 'OpenAI 兼容', total: 2, enabled: 2, account: false }],
      models: [
        {
          provider: 'openai_compatible', provider_label: 'OpenAI 兼容', id: 'solo-model',
          enabled: true, default_enabled: true,
          sources: [{ kind: 'channel', channel_id: 'ch-1', name: 'lxchat-glm' }],
          owned_by: 'aigx', context_length: null, capabilities: [], price: null, model_ratio: 1,
          aliases: [], alias_entries: [],
        },
        {
          provider: 'openai_compatible', provider_label: 'OpenAI 兼容', id: 'mapped-model',
          enabled: true, default_enabled: true,
          sources: [{ kind: 'channel', channel_id: 'ch-1', name: 'lxchat-glm' }],
          owned_by: 'aigx', context_length: null, capabilities: [], price: null, model_ratio: 1,
          aliases: ['keep-alias'],
          alias_entries: [{ alias: 'keep-alias', target: 'mapped-model', enabled: true }],
        },
      ],
      mappings: { 'keep-alias': 'mapped-model' },
      unpriced: 2,
    };
    const recompute = () => {
      for (const p of view.providers) {
        p.enabled = view.models.filter((m) => m.provider === p.id && m.enabled).length;
      }
    };
    await page.route('**/api/models/manage', (r) =>
      r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ success: true, data: view }) }),
    );
    // 回写：POST /api/models/state 改 default_enabled，并按 §2.1 重算派生 enabled
    await page.route('**/api/models/state', (r) => {
      const b = r.request().postDataJSON() as { provider?: string; id?: string; enabled?: boolean };
      const m = view.models.find((x) => x.provider === b.provider && x.id === b.id);
      if (m) {
        m.default_enabled = !!b.enabled;
        m.enabled = m.default_enabled || (m.alias_entries ?? []).some((e) => e.enabled);
        recompute();
      }
      return r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ success: true, data: view }) });
    });

    await page.goto('/model-manage');
    await expect(page.locator('h1')).toContainText('模型管理');

    const solo = dataRows(page).filter({ hasText: 'solo-model' });
    const mapped = dataRows(page).filter({ hasText: 'mapped-model' });
    await expect(solo).toHaveCount(1);
    await expect(mapped).toHaveCount(1);
    // 初始都生效（默认绑定开着）
    await expect(solo).not.toHaveClass(/mm-off/);
    await expect(mapped).not.toHaveClass(/mm-off/);

    // 关掉 solo-model 的默认绑定 → 无别名兜底 → 整行压淡
    await solo.locator('.mm-binding-default .mm-alias-switch').click();
    await expect(solo).toHaveClass(/mm-off/);

    // 关掉 mapped-model 的默认绑定 → keep-alias 仍开着 → **不**压淡
    await mapped.locator('.mm-binding-default .mm-alias-switch').click();
    await expect(mapped).not.toHaveClass(/mm-off/);
    // 默认绑定 chip 自身进入 is-off 态
    await expect(mapped.locator('.mm-binding-default')).toHaveClass(/is-off/);

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('「获取模型」：请求体只带 channel_id（§2.3 裁决），成功渲染发现列表 / 失败显示失败态', async ({
    page,
    pageErrors,
  }) => {
    // SPEC-1.0.46 §2.3（lead 裁决）：弹窗里全是**已保存**渠道，前端只发 channel_id；
    // channel_type/base_url 的兜底在后端做（body 参数语义是「未保存草稿」，前端快照
    // 可能是过期的，发过去会静默打错上游）。故本用例正向锁住「只带 channel_id」。
    const bodies: Array<Record<string, unknown>> = [];
    await page.route('**/api/channels/fetch_models', (r) => {
      bodies.push(r.request().postDataJSON() as Record<string, unknown>);
      return r.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify({ success: true, data: { models: ['glm-4.7-flash', 'glm-4-plus'], removed_mappings: 0 } }),
      });
    });

    await page.getByRole('button', { name: '获取模型' }).click();
    const modal = page.locator('.modal-overlay .modal').first();
    await expect(modal).toBeVisible();
    await expect(modal.locator('h3')).toContainText('获取模型');

    // ch-4 legacy-zero-interval（启用 + 唯一名）→ 点它的「拉取」
    const fetchRow = modal.locator('.mm-fetch-row', { hasText: 'legacy-zero-interval' });
    await expect(fetchRow).toBeVisible();
    await fetchRow.getByRole('button', { name: '拉取' }).click();

    await expect.poll(() => bodies.length, { timeout: 5000 }).toBeGreaterThan(0);
    const body = bodies[bodies.length - 1];
    // 正向：只发 channel_id（不带 channel_type/base_url —— 回归护栏，防回退到错误设计）
    expect(body.channel_id).toBe('ch-4');
    expect(body).not.toHaveProperty('channel_type');
    expect(body).not.toHaveProperty('base_url');
    // 成功路径：出现「已拉取」徽标
    await expect(fetchRow.locator('.mm-badge-ok')).toContainText('已拉取');

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('「获取模型」失败路径：4xx → 弹窗显示失败态（不静默）', async ({ page, pageErrors }) => {
    await page.route('**/api/channels/fetch_models', (r) =>
      r.fulfill({
        status: 400,
        contentType: 'application/json',
        // 形状照本次事故：后端把上游 403 包成 400 JSON
        body: JSON.stringify({ success: false, error: 'Auth failed: HTTP 403 (invalid api key?)' }),
      }),
    );

    await page.getByRole('button', { name: '获取模型' }).click();
    const modal = page.locator('.modal-overlay .modal').first();
    await expect(modal).toBeVisible();

    const fetchRow = modal.locator('.mm-fetch-row', { hasText: 'legacy-zero-interval' });
    await fetchRow.getByRole('button', { name: '拉取' }).click();

    // 失败不静默：行内渲染失败文案 + 按钮回到可重试的「重试」
    await expect(fetchRow.locator('.mm-fetch-err')).toBeVisible();
    await expect(fetchRow.locator('.mm-fetch-err')).toContainText('403');
    await expect(fetchRow.getByRole('button', { name: '重试' })).toBeVisible();

    // 本次是**故意**打一个 400：浏览器必然在 console 记一条 resource-load 错误，
    // 属预期噪声；过滤掉它再断言「没有其它未捕获异常」。
    const unexpected = pageErrors.filter((e) => !/status of 400/.test(e));
    expect(unexpected, unexpected.join('\n')).toEqual([]);
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
    await page.locator('.mm-colset > button').click();
    const panel = page.locator('.tbl-colset');
    await expect(panel).toBeVisible();
    // 公共面板：原生 checkbox 开关（对齐 TableColumnSettings），按行内列名定位
    await panel.locator('.tbl-colset-row', { hasText: '定价' }).locator('input[type="checkbox"]').click();
    await expect(page.locator('.mm-h-price')).toHaveCount(0);
    // 恢复默认，避免 localStorage 影响后续用例
    await panel.locator('.tbl-colset-reset').click();
    await expect(page.locator('.mm-h-price')).toHaveCount(1);

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('旧 localStorage 带已删「启用」列残留 → 不崩、无幽灵列（新键 + 旧键两条路径）', async ({ page, pageErrors }) => {
    // 两条路径都要覆盖（fe-table §9 双保险）：
    //   A) 新键 aigx-tbl-cols.models 里混入 {key:'enable'} → 公共层 sanitizeItems 丢弃未声明键；
    //   B) 只有旧键 aigx-mm-colcfg（含 enable）→ 一次性迁移过滤器按 MM_COL_DEFS 挡掉。
    // 用 addInitScript 在页面脚本之前写入，才能被列状态初始化读到。
    await page.addInitScript(() => {
      localStorage.setItem(
        'aigx-tbl-cols.models',
        JSON.stringify({
          v: 2,
          defaults: [],
          items: [
            { key: 'enable', visible: true, align: 'left' },
            { key: 'model', visible: true, align: 'left' },
            { key: 'alias', visible: true, align: 'left' },
          ],
        }),
      );
    });

    await page.goto('/model-manage');
    await expect(page.locator('h1')).toContainText('模型管理');
    await expect(page.locator('.mm-table')).toBeVisible();

    // 无幽灵列：表头/单元格都不出现 enable
    await expect(page.locator('.mm-h-enable')).toHaveCount(0);
    await expect(page.locator('.mm-c-enable')).toHaveCount(0);

    // 列设置面板不崩：7 列（选择/模型/定价/来源/能力/映射/操作），无「启用」
    await page.locator('.mm-colset > button').click();
    const panel = page.locator('.tbl-colset');
    await expect(panel).toBeVisible();
    await expect(panel.locator('.tbl-colset-row', { hasText: '启用' })).toHaveCount(0);
    await expect(panel.locator('.tbl-colset-row')).toHaveCount(7);
    // 面板仍可用：改一次对齐 → 触发回写；清洗后的存储不再残留 enable
    await panel.locator('.tbl-colset-row', { hasText: '定价' })
      .locator('.tbl-colset-align button', { hasText: '中' }).click();
    await expect(page.locator('.mm-h-price')).toHaveClass(/ta-center/);
    const persisted = await page.evaluate(() => localStorage.getItem('aigx-tbl-cols.models'));
    expect(persisted ?? '').not.toContain('"enable"');

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('仅旧键 aigx-mm-colcfg（含 enable）→ 一次性迁移过滤，不产生幽灵列', async ({ page, pageErrors }) => {
    await page.addInitScript(() => {
      localStorage.removeItem('aigx-tbl-cols.models');
      localStorage.setItem(
        'aigx-mm-colcfg',
        JSON.stringify([
          { key: 'enable', visible: true, align: 'left' },
          { key: 'model', visible: true, align: 'left' },
          { key: 'alias', visible: true, align: 'left' },
        ]),
      );
    });

    await page.goto('/model-manage');
    await expect(page.locator('h1')).toContainText('模型管理');
    await expect(page.locator('.mm-table')).toBeVisible();

    await expect(page.locator('.mm-h-enable')).toHaveCount(0);
    await expect(page.locator('.mm-c-enable')).toHaveCount(0);
    await page.locator('.mm-colset > button').click();
    const panel = page.locator('.tbl-colset');
    await expect(panel).toBeVisible();
    await expect(panel.locator('.tbl-colset-row', { hasText: '启用' })).toHaveCount(0);
    // 旧键里的 model / alias 顺序偏好仍被迁移保留（列仍按声明集合齐全）
    await expect(panel.locator('.tbl-colset-row')).toHaveCount(7);

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('列设置：对齐三档作用于表头与单元格，锁定列不可关', async ({ page, pageErrors }) => {
    await page.locator('.mm-colset > button').click();
    const panel = page.locator('.tbl-colset');
    await expect(panel).toBeVisible();

    // 定价列 → 居中（默认左；公共面板三档文案 左/中/右）
    const priceRow = panel.locator('.tbl-colset-row', { hasText: '定价' });
    await priceRow.locator('.tbl-colset-align button', { hasText: '中' }).click();
    await expect(page.locator('.mm-h-price')).toHaveClass(/ta-center/);

    // 勾列（锁定）→ 改左对齐；默认是居中
    const checkRow = panel.locator('.tbl-colset-row', { hasText: '选择' });
    await checkRow.locator('.tbl-colset-align button', { hasText: '左' }).click();
    await expect(page.locator('.mm-h-check')).toHaveClass(/ta-left/);

    // 锁定列（选择/模型/操作）开关禁用，点不动
    await expect(checkRow.locator('input[type="checkbox"]')).toBeDisabled();
    await expect(panel.locator('.tbl-colset-row', { hasText: '模型' }).locator('input[type="checkbox"]')).toBeDisabled();

    // 恢复默认：对齐回到各列声明值
    await panel.locator('.tbl-colset-reset').click();
    await expect(page.locator('.mm-h-price')).toHaveClass(/ta-left/);
    await expect(page.locator('.mm-h-check')).toHaveClass(/ta-center/);

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('覆盖管理弹窗列出覆盖项，删除入口可达', async ({ page, pageErrors }) => {
    // 原「元信息覆盖」Tab 已并入模型目录：入口在目录工具栏
    await expect(page.getByRole('tab', { name: '元信息覆盖' })).toHaveCount(0);

    await page.getByRole('button', { name: '覆盖管理' }).click();
    const modal = page.locator('.modal-overlay .modal').first();
    await expect(modal).toBeVisible();
    await expect(modal).toContainText('元信息覆盖');

    // 覆盖项表格：mock 里 glm-4.7-flash 有覆盖（归属方/上下文/能力）
    const table = modal.locator('.mm-overrides-table');
    await expect(table).toBeVisible();
    await expect(table).toContainText('glm-4.7-flash');
    await expect(table).toContainText('zhipu');
    await expect(table.locator('tbody tr')).toHaveCount(1);
    // 删除按钮挂在操作列（危险色），可点到确认弹窗
    await expect(table.locator('.mm-act-btn.danger')).toHaveCount(1);

    // 新增入口也在弹窗里
    await expect(modal.getByRole('button', { name: '新增覆盖' })).toBeVisible();

    // 关闭后回到目录，Tab 仍只有两个（目录 / 定价倍率）
    await modal.getByRole('button', { name: '关闭' }).click();
    await expect(modal).toHaveCount(0);
    await expect(page.getByRole('tab')).toHaveCount(2);
    await expect(page.locator('.mm-layout')).toBeVisible();
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
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
