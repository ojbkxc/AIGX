import { test, expect, loginAsAdmin } from './fixtures';
import type { Page } from '@playwright/test';

/**
 * 破甲控制台 · 迭代面 e2e（第 19–30 批新增的行为断言）。
 *
 * 与 `purge-console-presets.spec.ts` 的分工：那个文件钉「预设库」的形状契约；
 * 本文件钉**本轮新增的交互语义**——这些点在单测里只能验纯函数，验不了
 * 「点了之后页面真的这么反应」。
 *
 * mock 形状契约（与 src/api/admin/purge.rs 的 payload 逐字段对齐）：
 *   GET  /api/purge              → PurgeData（含 config_warning_items.severity）
 *   GET  /api/purge/defaults     → { settings }
 *   POST /api/purge/preview      → { messages, applied, detail }
 *   POST /api/purge/presets/*    → 本文件不涉及
 */

/** 一套「多个治理项同时生效」的配置：用来验生效项摘要与告警分级。 */
const SETTINGS = {
  enabled: true,
  system_normalize: true,
  dead_zone_rewrite: true,
  synonym_enabled: true,
  reduce_reasoning: true,
  cross_source_fallback: true,
  skill_inject: false,
  structured_prompt: false,
  reasoning_floor: 'medium',
  retry_limit: 8,
  fallback_trigger_attempt: 3,
  fallback_models: ['glm-5'],
  rewrite_rules: [
    { from: '违禁词', to: '敏感话题', enabled: true },
    { from: '删我', to: '', enabled: true },
    { from: '违禁词', to: '重复行', enabled: true },
  ],
  synonym_pairs: [{ from: '老词', to: '新词', enabled: true }],
  normalize_strip_prefixes: [],
  skill_dir: '~/.aigx/skills',
  skill_inject_limit: 2000,
  structured_suffix: '',
  structured_templates: [],
  active_template: '',
  refusal_patterns: [],
  failure_filter_keywords: [],
  failure_empty_keywords: [],
  auto_preset: false,
  auto_preset_id: '',
  upstream_label: 'e2e',
};

const PURGE_DATA = {
  settings: SETTINGS,
  stats: {
    responses: 10,
    retries: 2,
    early_success: 8,
    skill_hits: 0,
    refusals: 1,
    failure_refusal: 1,
    failure_empty: 0,
    failure_filter: 0,
    failure_other: 0,
    synonym_hits: 4,
    auto_preset_hits: 0,
  },
  stats_1h: {
    window_ms: 3_600_000,
    responses: 5,
    retries: 1,
    early_success: 4,
    skill_hits: 0,
    refusals: 0,
    failure_refusal: 0,
    failure_empty: 0,
    failure_filter: 0,
    failure_other: 0,
    synonym_hits: 2,
    auto_preset_hits: 0,
    truncated: false,
  },
  stats_24h: {
    window_ms: 86_400_000,
    responses: 10,
    retries: 2,
    early_success: 8,
    skill_hits: 0,
    refusals: 1,
    failure_refusal: 1,
    failure_empty: 0,
    failure_filter: 0,
    failure_other: 0,
    synonym_hits: 4,
    auto_preset_hits: 0,
    truncated: false,
  },
  stats_last_run: null,
  // 统计基线（清零过就是清零时刻）
  stats_since: 1_780_000_000,
  // 三类严重级各一条：error 必须排在 info 前面
  config_warning_items: [
    { code: 'preset_missing', message: '预设 id 不存在', severity: 'info' },
    { code: 'fallback_empty', message: '异源递推空了', severity: 'error' },
    { code: 'chained_rewrite', message: '链式二次改写', severity: 'warn' },
  ],
  config_warnings: [],
  skills: [],
  skill_dir_resolved: '/home/e2e/.aigx/skills',
  runtime: { pid: 1, uptime_secs: 60, upstream: 'e2e', inflight: 0 },
};

const PREVIEW_RESULT = {
  messages: [{ role: 'user', content: '请帮我处理 敏感话题 相关的问题' }],
  applied: { changed: true, skill_hit: false },
  detail: {
    normalized: 0,
    dead_zone_rules: 1,
    dead_zone_replacements: 1,
    synonym_rules: 0,
    synonym_replacements: 0,
    structured_appended: false,
    skill_names: [],
    skill_truncated: false,
    skill_keywords: [],
    skill_dropped: 0,
    auto_preset_applied: false,
  },
};

async function mockPurge(page: Page): Promise<void> {
  await page.route(/\/api\/purge\/defaults$/, (r) =>
    r.fulfill({ json: { success: true, data: { settings: SETTINGS } } }),
  );
  await page.route(/\/api\/purge\/preview$/, (r) =>
    r.fulfill({ json: { success: true, data: PREVIEW_RESULT } }),
  );
  // 写接口**回显请求体**：开关与保存都是「乐观更新 + 以服务端返回为准」。若 PUT 回一份
  // 固定数据，保存成功后状态会被覆盖回去——看起来就是「点了没反应」，把正确实现
  // 测成缺陷。一处改好，所有用到 mockPurge 的用例都受益。
  await page.route(/\/api\/purge(\?|$)/, async (r) => {
    if (r.request().method() === 'PUT') {
      const body = JSON.parse(r.request().postData() ?? '{}');
      await r.fulfill({
        json: {
          success: true,
          data: { ...PURGE_DATA, settings: { ...PURGE_DATA.settings, ...body } },
        },
      });
      return;
    }
    await r.fulfill({ json: { success: true, data: PURGE_DATA } });
  });
}

async function gotoPurge(page: Page): Promise<void> {
  await loginAsAdmin(page);
  await page.goto('/purge');
  await expect(page.getByRole('heading', { name: '破甲控制台' })).toBeVisible();
}

test('1 · 生效项摘要按数据面语义汇总（词表只算启用行、异源带参数）', async ({ page }) => {
  await mockPurge(page);
  await gotoPurge(page);

  const summary = page.getByTestId('purge-active-summary');
  await expect(summary).toBeVisible();
  // 死区词 3 行里有 1 行 to 为空但仍是「启用行」→ 摘要按启用数计 3
  await expect(summary).toContainText('死区词改写（3 条启用）');
  await expect(summary).toContainText('同义词替换（1 条启用）');
  await expect(summary).toContainText('降思考档位 → medium');
  await expect(summary).toContainText('异源递进（1 个模型，第 3 轮起）');
});

test('2 · 生效项可点并跳到对应配置区', async ({ page }) => {
  await mockPurge(page);
  await gotoPurge(page);

  await page.getByRole('button', { name: /死区词改写/ }).click();
  // 落点闪烁类由 focusFixTarget 添加
  await expect(page.locator('[data-purge-fix="dead-zone-table"].purge-fix-flash')).toHaveCount(1);
});

test('3 · 体检告警按严重级排序：error 在前，info 在后', async ({ page }) => {
  await mockPurge(page);
  await gotoPurge(page);

  const rows = page.locator('.purge-warning-row');
  await expect(rows).toHaveCount(3);
  await expect(rows.nth(0)).toHaveClass(/is-error/);
  await expect(rows.nth(0)).toContainText('异源递推空了');
  await expect(rows.nth(1)).toHaveClass(/is-warn/);
  await expect(rows.nth(2)).toHaveClass(/is-info/);
  await expect(rows.nth(2)).toContainText('预设 id 不存在');
});

test('4 · 词表「整理」删空改写与重复行，并如实回报条数', async ({ page }) => {
  await mockPurge(page);
  await gotoPurge(page);

  // 3 行 → 删 1 行空改写 + 1 行重复 = 剩 1 行
  page.on('dialog', (d) => void d.accept());
  // 两张表各有一个「整理」按钮：只用死区词那张（first）
  await page.getByRole('button', { name: '整理' }).first().click();
  // Toast 的 DOM 是 `.toast`（components/Toast.tsx），不是 role=status
  await expect(page.locator('.toast').first()).toContainText(/已整理：删除 2 行，剩余 1 行/);
});

test('5 · 词表行「试」按钮跑一次预览并渲染 diff', async ({ page }) => {
  await mockPurge(page);
  await gotoPurge(page);

  await page.getByRole('button', { name: '试' }).first().click();
  await expect(page.getByTestId('purge-preview-diff')).toBeVisible();
  await expect(page.getByTestId('purge-diff-changed-only')).toBeVisible();
});

test('6 · 「只看变更行」隐藏上下文并报出隐藏行数', async ({ page }) => {
  await mockPurge(page);
  await gotoPurge(page);

  await page.getByRole('button', { name: '试' }).first().click();
  await page.getByTestId('purge-diff-changed-only').check();
  // 单条消息、单行差异：没有上下文行 → 不该出现隐藏提示
  await expect(page.locator('.pdl-hidden-hint')).toHaveCount(0);
});

test('10 · 应用预设后该条打「最近用过」徽标', async ({ page }) => {
  const preset = {
    id: 'p-recent',
    vendor: 'V',
    model: 'm',
    title: 'Recent preset',
    tags: [],
    bytes: 12,
    sha256: 'cafe'.repeat(16),
    preview: 'body preview',
  };
  await page.route(/\/api\/purge\/presets\/[^/]+\/apply$/, (r) =>
    r.fulfill({ json: { success: true, data: { applied: {}, settings: SETTINGS } } }),
  );
  await page.route(/\/api\/purge\/presets\/[^/]+$/, (r) =>
    r.fulfill({ json: { success: true, data: { ...preset, body: 'preset body' } } }),
  );
  await page.route(/\/api\/purge\/presets(\?|$)/, (r) =>
    r.fulfill({
      json: { success: true, data: { items: [preset], vendors: [{ vendor: 'V', count: 1 }], total: 1 } },
    }),
  );
  await mockPurge(page);
  await gotoPurge(page);

  // 应用前没有徽标
  await expect(page.getByText('最近用过')).toHaveCount(0);

  // 展开 → 设为结构化模板（走 noteRecent）
  await page.getByText('Recent preset').first().click();
  await page.getByTestId('purge-preset-apply-template').click();

  // 应用后该条打标
  await expect(page.getByText('最近用过')).toBeVisible();
});

test('48 · 导入配置前确认：取消则保留未保存编辑', async ({ page }) => {
  // 根因修复前这条跑不通：`fallback_models` 被按「元素必须是对象」校验 → 导出的
  // 配置判 fatal → 在 confirm 之前就 return 了（所以 dialogs=0）。
  page.on('dialog', (d) => void d.dismiss());
  await mockPurge(page);
  await gotoPurge(page);

  await page.getByLabel('重试上限').fill('9');
  await expect(page.locator('.purge-dirty-bar')).toBeVisible();

  const importInput = page.locator('input[type="file"][accept*="json"]').first();
  await importInput.setInputFiles({
    name: 'cfg.json',
    mimeType: 'application/json',
    buffer: Buffer.from(
      JSON.stringify({ version: 1, settings: { ...PURGE_DATA.settings, retry_limit: 11 } }),
    ),
  });
  await page.waitForTimeout(900);

  // 取消 → 草稿保持 9
  await expect(page.getByLabel('重试上限')).toHaveValue('9');
});

test('51 · 规则行的操作按钮带可读名称（几十行同名「删除」无法分辨）', async ({ page }) => {
  const rules = [
    { from: 'alpha', to: '1' },
    { from: 'beta', to: '2' },
    { from: '   ', to: '3' }, // 空原文 → 名称退成行号
    { from: 'delta', to: '4' },
  ];
  await mockPurge(page);
  await page.route(/\/api\/purge(\?|$)/, (r) =>
    r.fulfill({
      json: {
        success: true,
        data: { ...PURGE_DATA, settings: { ...PURGE_DATA.settings, rewrite_rules: rules } },
      },
    }),
  );
  await gotoPurge(page);

  const table = page.locator('[data-purge-fix="dead-zone-table"]');
  // 删除按钮按名字可精确定位（不是「第一个删除按钮」）
  await expect(table.getByRole('button', { name: '删除「alpha」' })).toBeVisible();
  await expect(table.getByRole('button', { name: '删除「delta」' })).toBeVisible();
  // 空原文那行退成行号（第 3 条）
  await expect(table.getByRole('button', { name: '删除第 3 条' })).toBeVisible();
  // 上移/下移同理
  await expect(table.getByRole('button', { name: /^上移.*「beta」$/ })).toBeVisible();
  await expect(table.getByRole('button', { name: /^下移.*「alpha」$/ })).toBeVisible();
});

test('50 · 有未保存修改时导出的是「已保存」配置，并明说', async ({ page }) => {
  await mockPurge(page);
  await gotoPurge(page);

  // 改一处但不保存，然后导出
  await page.getByLabel('重试上限').fill('9');
  await expect(page.locator('.purge-dirty-bar')).toBeVisible();

  const downloadPromise = page.waitForEvent('download');
  await page.getByRole('button', { name: '导出配置' }).click();
  const download = await downloadPromise;
  const stream = await download.createReadStream();
  const chunks: Buffer[] = [];
  for await (const chunk of stream) chunks.push(chunk as Buffer);
  const body = Buffer.concat(chunks).toString('utf8');
  const parsed = JSON.parse(body.replace(/^\uFEFF/, '')) as {
    settings: { retry_limit: number };
  };

  // 导出的是**已保存**值（8），不是草稿（9）
  expect(parsed.settings.retry_limit).toBe(8);
  // 并且**明说**了这件事，不让用户以为导出的就是屏幕上这套
  await expect(page.locator('.toast', { hasText: '未保存修改未包含在内' })).toHaveCount(1);
});

test('49 · 导入配置前确认：确认则用导入值替换', async ({ page }) => {
  page.on('dialog', (d) => void d.accept());
  await mockPurge(page);
  await gotoPurge(page);

  await page.getByLabel('重试上限').fill('9');
  await expect(page.locator('.purge-dirty-bar')).toBeVisible();

  const importInput = page.locator('input[type="file"][accept*="json"]').first();
  await importInput.setInputFiles({
    name: 'cfg.json',
    mimeType: 'application/json',
    buffer: Buffer.from(
      JSON.stringify({ version: 1, settings: { ...PURGE_DATA.settings, retry_limit: 11 } }),
    ),
  });
  await page.waitForTimeout(900);

  await expect(page.getByLabel('重试上限')).toHaveValue('11');
});

test('47 · 写同名 SKILL.md 前先确认（后端是直接覆盖）', async ({ page }) => {
  // 技能目录里已有 `p-skill` 同名技能 → 点「写入技能目录」应先弹确认
  const preset = {
    id: 'p-skill',
    vendor: 'V',
    model: 'm',
    title: 'Skill preset',
    tags: [],
    bytes: 12,
    sha256: 'beef'.repeat(16),
    preview: 'body preview',
  };
  let applyCalls = 0;
  await mockPurge(page);
  await page.route(/\/api\/purge(\?|$)/, (r) =>
    r.fulfill({
      json: {
        success: true,
        data: {
          ...PURGE_DATA,
          // 技能快照里已有同名（name 来自 SKILL.md，缺省退目录名）
          skills: [{ name: 'p-skill', keywords: ['p-skill'], body_chars: 12 }],
        },
      },
    }),
  );
  await page.route(/\/api\/purge\/presets\/[^/]+\/apply$/, (r) => {
    applyCalls += 1;
    return r.fulfill({ json: { success: true, data: { applied: { path: '/x' }, settings: PURGE_DATA.settings } } });
  });
  await page.route(/\/api\/purge\/presets\/[^/]+$/, (r) =>
    r.fulfill({ json: { success: true, data: { ...preset, body: 'preset body' } } }),
  );
  await page.route(/\/api\/purge\/presets(\?|$)/, (r) =>
    r.fulfill({ json: { success: true, data: { items: [preset], vendors: [{ vendor: 'V', count: 1 }], total: 1 } } }),
  );
  await gotoPurge(page);

  await page.getByText('Skill preset').first().click();

  // 取消 → 不发 apply 请求
  page.once('dialog', (d) => void d.dismiss());
  await page.getByRole('button', { name: /写入技能目录/ }).first().click();
  await page.waitForTimeout(800);
  expect(applyCalls).toBe(0);

  // 确认 → 发 apply
  page.once('dialog', (d) => void d.accept());
  await page.getByRole('button', { name: /写入技能目录/ }).first().click();
  await expect.poll(() => applyCalls).toBe(1);
});

test('46 · 加载失败时带出真实原因（不把 500 说成网络异常）', async ({ page }) => {
  await loginAsAdmin(page);
  // 500 + 后端文案：应同时展示状态码与文案
  await page.route(/\/api\/purge(\?|$)/, (r) =>
    r.fulfill({ status: 500, json: { success: false, error: '数据库连接失败' } }),
  );
  await page.goto('/purge');

  const detail = page.getByTestId('purge-load-error-detail');
  await expect(detail).toBeVisible();
  await expect(detail).toContainText('HTTP 500');
  await expect(detail).toContainText('数据库连接失败');
  // 通用文案仍在（不丢掉「可以重试」这层意思）
  await expect(page.getByText(/网络异常或服务不可用/)).toBeVisible();
  await expect(page.getByRole('button', { name: '重试' })).toBeVisible();
});

test('45 · 总开关关着时说明「配了但没生效」，且不禁用其他开关', async ({ page }) => {
  // `mockPurge` 的 PUT 已回显请求体（见其注释），这里不必再覆盖
  await mockPurge(page);
  await gotoPurge(page);

  const notice = page.getByTestId('purge-off-notice');
  await expect(notice).toHaveCount(0); // 默认开着

  // 关掉总开关（用 aria-label 精确定位，不靠「第一个就是」这种顺序假设）
  await page.getByRole('switch', { name: '破甲总开关' }).click();
  await expect(notice).toBeVisible();
  // 两句都要在：一句说请求不改写，一句说下面配了也不生效
  await expect(notice).toContainText(/请求不会被改写/);
  await expect(notice).toContainText(/只有打开总开关后才会真正生效/);

  // 关键：**不禁用**下面的开关（常见流程是先配好再打开总开关）
  const otherSwitch = page.getByRole('switch', { name: 'system 归一化' });
  await expect(otherSwitch).toBeEnabled();
});

test('44 · 「跳到下一个重复」循环定位到重复行', async ({ page }) => {
  // 两组重复：dup 3 次（下标 0/1/4）、rep 2 次（2/3）
  const rules = [
    { from: 'dup', to: 'a' },
    { from: 'dup', to: 'b' },
    { from: 'rep', to: 'c' },
    { from: 'rep', to: 'd' },
    { from: 'dup', to: 'e' },
  ];
  await mockPurge(page);
  await page.route(/\/api\/purge(\?|$)/, (r) =>
    r.fulfill({
      json: {
        success: true,
        data: { ...PURGE_DATA, settings: { ...PURGE_DATA.settings, rewrite_rules: rules } },
      },
    }),
  );
  await gotoPurge(page);

  const jump = page.getByTestId('purge-dead-zone-next-dup');
  await expect(jump).toBeVisible();

  // 第一次 → 第 1 处重复（下标 1，即第 2 条）；再点 → 第 2 处（下标 3）
  await jump.click();
  await expect(page.locator('.toast', { hasText: '第 2 条' })).toHaveCount(1);
  await jump.click();
  await expect(page.locator('.toast', { hasText: '第 4 条' })).toHaveCount(1);

  // 过滤期间不显示该按钮（过滤后没有相邻行可对照）
  await page.getByLabel('搜索死区词表').fill('dup');
  await expect(jump).toHaveCount(0);
});

test('43 · 搜索过滤期间移动按钮禁用（交换的是原表相邻行，会看起来没反应）', async ({ page }) => {
  const rules = [
    { from: 'alpha', to: '1' },
    { from: 'beta', to: '2' },
    { from: 'gamma', to: '3' },
    { from: 'delta', to: '4' },
  ];
  await mockPurge(page);
  await page.route(/\/api\/purge(\?|$)/, (r) =>
    r.fulfill({
      json: {
        success: true,
        data: { ...PURGE_DATA, settings: { ...PURGE_DATA.settings, rewrite_rules: rules } },
      },
    }),
  );
  await gotoPurge(page);

  const table = page.locator('[data-purge-fix="dead-zone-table"]');
  // 未过滤时第 2 行可上下移动
  await expect(table.getByTestId('purge-rule-up-1')).toBeEnabled();

  // 过滤后只剩 1 行（gamma，真实下标 2）→ 两个方向都禁用，并给出原因
  await page.getByLabel('搜索死区词表').fill('gamma');
  await expect(table.locator('.purge-rule-row')).toHaveCount(1);
  await expect(table.getByTestId('purge-rule-up-2')).toBeDisabled();
  await expect(table.getByTestId('purge-rule-down-2')).toBeDisabled();
  await expect(page.getByTitle(/搜索过滤中不能调序/).first()).toBeVisible();

  // 清空搜索 → 恢复可用
  await page.getByLabel('搜索死区词表').fill('');
  await expect(table.getByTestId('purge-rule-up-1')).toBeEnabled();
});

test('42 · 「批量停用异常行」只停用不删除（可回填后再启用）', async ({ page }) => {
  const rules = [
    { from: 'dup', to: '1', enabled: true },
    { from: 'dup', to: '2', enabled: true },
    { from: 'ok', to: '3', enabled: true },
    { from: '   ', to: 'emptyFrom', enabled: true },
  ];
  await mockPurge(page);
  await page.route(/\/api\/purge(\?|$)/, (r) =>
    r.fulfill({
      json: {
        success: true,
        data: { ...PURGE_DATA, settings: { ...PURGE_DATA.settings, rewrite_rules: rules } },
      },
    }),
  );
  await gotoPurge(page);

  const table = page.locator('[data-purge-fix="dead-zone-table"]');
  const rows = table.locator('.purge-rule-row');
  await expect(rows).toHaveCount(4);

  page.once('dialog', (d) => void d.accept());
  await page.getByTestId('purge-dead-zone-disable-issues').click();

  // 行数不变（不删除），但异常行的启用勾选被去掉
  await expect(rows).toHaveCount(4);
  const enabled = await rows
    .locator('input[type="checkbox"]')
    .evaluateAll((els) => els.map((el) => (el as HTMLInputElement).checked));
  // 只停用异常行：第 0 条 dup 是最早出现的（不重复，保持启用），
  // 第 1 条重复、第 3 条空原文被停用
  expect(enabled).toEqual([true, false, true, false]);
  await expect(page.locator('.toast', { hasText: '已停用' })).toHaveCount(1);
});

test('41 · 词表可调序（改写按表序执行，顺序决定谁先命中）', async ({ page }) => {
  const rules = [
    { from: 'first', to: '1' },
    { from: 'second', to: '2' },
    { from: 'third', to: '3' },
    { from: 'fourth', to: '4' },
  ];
  await mockPurge(page);
  await page.route(/\/api\/purge(\?|$)/, (r) =>
    r.fulfill({
      json: {
        success: true,
        data: { ...PURGE_DATA, settings: { ...PURGE_DATA.settings, rewrite_rules: rules } },
      },
    }),
  );
  await gotoPurge(page);

  const table = page.locator('[data-purge-fix="dead-zone-table"]');
  const fromValues = () =>
    table.getByLabel('原文').evaluateAll((els) => els.map((el) => (el as HTMLInputElement).value));

  // 首行上移按钮应被禁用（不越界）
  await expect(table.getByTestId('purge-rule-up-0')).toBeDisabled();
  await expect(table.getByTestId('purge-rule-down-3')).toBeDisabled();

  // 把第 2 条上移 → 顺序变化，且标记为未保存
  await table.getByTestId('purge-rule-up-1').click();
  expect(await fromValues()).toEqual(['second', 'first', 'third', 'fourth']);
  // 断「未保存状态」本身（dirty bar）而不是它内部的明细块：明细还要求
  // savedForDiff 有值，条件更严，与本条要验的「调序生效」无关
  await expect(page.locator('.purge-dirty-bar')).toBeVisible();

  // 再下移回去 → 恢复原序
  await table.getByTestId('purge-rule-down-0').click();
  expect(await fromValues()).toEqual(['first', 'second', 'third', 'fourth']);
});

test('40 · 预览区的「回看当前生效项」能跳回摘要区', async ({ page }) => {
  await mockPurge(page);
  await gotoPurge(page);

  const jump = page.getByTestId('purge-jump-active-summary');
  await expect(jump).toBeVisible();

  // 先滚到底（预览区），确认摘要不在视口内，再点跳转
  await page.evaluate(() => window.scrollTo(0, document.body.scrollHeight));
  const summaryTopBefore = await page
    .locator('[data-purge-fix="active-summary"]')
    .evaluate((el) => el.getBoundingClientRect().top);
  expect(summaryTopBefore).toBeLessThan(0);

  await jump.click();
  await expect
    .poll(async () =>
      page.locator('[data-purge-fix="active-summary"]').evaluate((el) => el.getBoundingClientRect().top),
    )
    .toBeGreaterThan(-50);
  // 复用同一套闪烁提示：落点会被标出来
  await expect(page.locator('[data-purge-fix="active-summary"].purge-fix-flash')).toHaveCount(1);
});

test('39 · 「只看异常」的行会说明异常原因（不是只筛出来不解释）', async ({ page }) => {
  const rules = [
    { from: 'dup', to: '1' },
    { from: 'dup', to: '2' },
    { from: 'ok', to: '3' },
    { from: '   ', to: 'emptyFrom' },
    { from: 'emptyTo', to: '' },
  ];
  await mockPurge(page);
  await page.route(/\/api\/purge(\?|$)/, (r) =>
    r.fulfill({
      json: {
        success: true,
        data: { ...PURGE_DATA, settings: { ...PURGE_DATA.settings, rewrite_rules: rules } },
      },
    }),
  );
  await gotoPurge(page);

  await page.getByTestId('purge-dead-zone-issues-only').check();
  const table = page.locator('[data-purge-fix="dead-zone-table"]');

  // 三种原因各说各的：空原文 / 空改写 / 重复
  await expect(table.getByText(/原文未填/)).toHaveCount(1);
  await expect(table.getByText(/改写为空/)).toHaveCount(1);
  await expect(table.getByText(/原文重复/)).toHaveCount(1);
  // 关键：空行不能被报成「重复」（报错原因错，修法就错）
  const emptyRow = table.locator('.purge-rule-row').filter({ hasText: '原文未填' });
  await expect(emptyRow).not.toContainText(/原文重复/);
});

test('38 · 切到「上次运行」档时说明是哪一次（没有则说清为什么空）', async ({ page }) => {
  await mockPurge(page);
  await gotoPurge(page);

  // mock 的 stats_last_run 为 null → 档位禁用；先把数据换成有快照
  await page.route(/\/api\/purge(\?|$)/, (r) =>
    r.fulfill({
      json: {
        success: true,
        data: {
          ...PURGE_DATA,
          stats_last_run: {
            ...PURGE_DATA.stats,
            saved_at: 1_780_000_000,
            uptime_secs: 3 * 3600 + 25 * 60,
          },
        },
      },
    }),
  );
  await page.reload({ waitUntil: 'domcontentloaded' });
  await page.waitForTimeout(2000);

  await page.getByRole('button', { name: '上次运行' }).click();
  const note = page.getByTestId('purge-last-run-note');
  await expect(note).toBeVisible();
  await expect(note).toContainText(/上次运行快照/);
  await expect(note).toContainText(/3 小时 25 分/);
});

test('37 · 「只看异常」先看清整理会删掉哪些行', async ({ page }) => {
  // 搜索行只在 >3 条时出现，所以要自备 5 条（含 1 条重复 + 1 条空改写）
  const rules = [
    { from: 'dup', to: '1' },
    { from: 'dup', to: '2' },
    { from: 'ok', to: '3' },
    { from: 'emptyTo', to: '' },
    { from: 'okay', to: '4' },
  ];
  await mockPurge(page);
  await page.route(/\/api\/purge(\?|$)/, (r) =>
    r.fulfill({
      json: {
        success: true,
        data: { ...PURGE_DATA, settings: { ...PURGE_DATA.settings, rewrite_rules: rules } },
      },
    }),
  );
  await gotoPurge(page);
  const rows = page.locator('[data-purge-fix="dead-zone-table"] .purge-rule-row');
  const total = await rows.count();
  expect(total).toBe(5);

  const toggle = page.getByTestId('purge-dead-zone-issues-only');
  await expect(toggle).toBeVisible();
  await toggle.check();

  // 异常 = 重复的后一条 + 空改写 = 2 条，与开关上的计数一致
  const label = await page.locator('.purge-issues-only').first().innerText();
  const expected = Number(/(\d+)\s*条/.exec(label)?.[1] ?? '-1');
  expect(expected).toBe(2);
  await expect(rows).toHaveCount(2);
  expect(expected).toBeLessThan(total);

  // 取消勾选 → 全部回来
  await toggle.uncheck();
  await expect(rows).toHaveCount(total);
});

test('36 · 词表搜索过滤不改变编辑目标（下标仍是真实的）', async ({ page }) => {
  // 造 5 条规则（>3 才显示搜索框）。**先 mockPurge 再覆盖**：同一路径的 route
  // 后注册者优先，顺序反了就会被基础 mock 盖掉。
  const rules = [
    { from: 'alpha', to: '1' },
    { from: 'beta', to: '2' },
    { from: 'gamma', to: '3' },
    { from: 'delta', to: '4' },
    { from: 'epsilon', to: '5' },
  ];
  await mockPurge(page);
  await page.route(/\/api\/purge(\?|$)/, (r) =>
    r.fulfill({
      json: {
        success: true,
        data: { ...PURGE_DATA, settings: { ...PURGE_DATA.settings, rewrite_rules: rules } },
      },
    }),
  );
  await gotoPurge(page);

  // 选择器必须限定到死区表：两张词表共用 `.purge-rules` class，
  // 只写 `.purge-rules .purge-rule-row` 会把同义词表的行一并算进来
  const rows = page.locator('[data-purge-fix="dead-zone-table"] .purge-rule-row');
  await expect(rows).toHaveCount(5);

  // 过滤到只剩 1 行，并显示匹配计数
  await page.getByLabel('搜索死区词表').fill('gamma');
  await expect(rows).toHaveCount(1);
  await expect(page.getByText(/匹配 1 \/ 5 条/)).toBeVisible();

  // 改这一行的「改写为」→ 必须写到**原来那一条**（下标 2），不能写到下标 0
  await rows.first().getByLabel('改写为').fill('3-changed');
  await page.getByLabel('搜索死区词表').fill('');
  await expect(rows).toHaveCount(5);
  const toValues = await rows.getByLabel('改写为').evaluateAll((els) =>
    els.map((el) => (el as HTMLInputElement).value),
  );
  expect(toValues[2]).toBe('3-changed');
  expect(toValues[0]).toBe('1');
});

test('35 · 「复制整表」输出 Tab 分隔文本，能粘回批量粘贴', async ({ page }) => {
  await page.addInitScript(() => {
    (window as unknown as { __copied: string[] }).__copied = [];
    Object.defineProperty(navigator, 'clipboard', {
      configurable: true,
      value: {
        writeText: (t: string) => {
          (window as unknown as { __copied: string[] }).__copied.push(t);
          return Promise.resolve();
        },
      },
    });
  });
  await mockPurge(page);
  await gotoPurge(page);

  await page.getByTestId('purge-copy-rules-dead-zone').click();
  await expect
    .poll(() => page.evaluate(() => (window as unknown as { __copied: string[] }).__copied.length))
    .toBe(1);
  const text = await page.evaluate(
    () => (window as unknown as { __copied: string[] }).__copied[0],
  );
  // mock 里的一条规则 → Tab 分隔的一行
  expect(text).toContain('\t');
  expect(text.split('\n').length).toBeGreaterThan(0);
  // 闭环：把它粘回「批量粘贴」应能解析（不出现「格式不对」）
  await page.getByTestId('purge-batch-paste-open').click();
  await page.getByLabel('批量粘贴内容').fill(text);
  await page.getByRole('button', { name: '解析并添加' }).click();
  await expect(page.locator('.toast').first()).not.toContainText(/格式不对/);
});

test('34 · 保存成功后词表撤销入口失效（不能撤销已落库的结果）', async ({ page }) => {
  await page.route(/\/api\/purge(\?|$)/, async (r) => {
    if (r.request().method() === 'PUT') {
      await r.fulfill({ json: { success: true, data: PURGE_DATA } });
      return;
    }
    await r.fulfill({ json: { success: true, data: PURGE_DATA } });
  });
  await gotoPurge(page);

  // 批量粘贴产生可撤销快照
  await page.getByTestId('purge-batch-paste-open').click();
  await page.getByLabel('批量粘贴内容').fill('甲\tA');
  await page.getByRole('button', { name: '解析并添加' }).click();
  await expect(page.getByTestId('purge-table-undo')).toBeVisible();

  // 保存 → 快照过期，入口消失（否则点它会把已落库的结果改回去）
  await page.getByRole('button', { name: '保存' }).click();
  await expect(page.getByTestId('purge-saved-at')).toBeVisible();
  await expect(page.getByTestId('purge-table-undo')).toHaveCount(0);
});

test('33 · 页面明写「配置是全局的」（没有按用户隔离）', async ({ page }) => {
  await mockPurge(page);
  await gotoPurge(page);

  const note = page.getByTestId('purge-scope-note');
  await expect(note).toBeVisible();
  // 三个要点都得说到：全局 / 无按用户隔离 / 保存即影响线上
  await expect(note).toContainText(/全局/);
  await expect(note).toContainText(/没有按用户/);
  await expect(note).toContainText(/立即影响线上/);
  // 总开关关闭时这句仍然在（作用域与开关状态无关）
  await page.getByRole('switch').first().click();
  await expect(page.getByTestId('purge-scope-note')).toBeVisible();
});

test('32 · 空原文行保存前告知会被丢弃', async ({ page }) => {
  // 一行 from 为空：后端 filter_map 会静默剔除，前端要提前说
  const rules = [{ from: 'a', to: 'b' }, { from: '   ', to: 'c' }];
  let puts = 0;
  await page.route(/\/api\/purge(\?|$)/, async (r) => {
    if (r.request().method() === 'PUT') {
      puts += 1;
      await r.fulfill({ json: { success: true, data: PURGE_DATA } });
      return;
    }
    await r.fulfill({
      json: {
        success: true,
        data: { ...PURGE_DATA, settings: { ...PURGE_DATA.settings, rewrite_rules: rules } },
      },
    });
  });
  await gotoPurge(page);

  await page.getByLabel('重试上限').fill('6');

  // 取消 → 不保存
  page.once('dialog', (d) => void d.dismiss());
  await page.getByRole('button', { name: '保存' }).click();
  await page.waitForTimeout(600);
  expect(puts).toBe(0);

  // 确认 → 保存
  page.once('dialog', (d) => void d.accept());
  await page.getByRole('button', { name: '保存' }).click();
  await expect.poll(() => puts).toBe(1);
});

test('31 · 异源模型超上限也走保存前确认（不只词表）', async ({ page }) => {
  // 17 个异源模型（上限 16）：这类字段原本只有输入框下红字，不挡保存
  const models = Array.from({ length: 17 }, (_, i) => `m${i}`);
  let puts = 0;
  await page.route(/\/api\/purge(\?|$)/, async (r) => {
    if (r.request().method() === 'PUT') {
      puts += 1;
      await r.fulfill({ json: { success: true, data: PURGE_DATA } });
      return;
    }
    await r.fulfill({
      json: {
        success: true,
        data: { ...PURGE_DATA, settings: { ...PURGE_DATA.settings, fallback_models: models } },
      },
    });
  });
  await gotoPurge(page);

  await page.getByLabel('重试上限').fill('6');
  page.once('dialog', (d) => void d.dismiss());
  await page.getByRole('button', { name: '保存' }).click();
  await page.waitForTimeout(600);
  expect(puts).toBe(0);

  page.once('dialog', (d) => void d.accept());
  await page.getByRole('button', { name: '保存' }).click();
  await expect.poll(() => puts).toBe(1);
});

test('30 · 词表超上限时保存前确认，取消则不保存', async ({ page }) => {
  // 造 201 条规则（上限 200）：常驻警示之外，保存要走确认
  const many = Array.from({ length: 201 }, (_, i) => ({ from: `k${i}`, to: `v${i}` }));
  let puts = 0;
  await page.route(/\/api\/purge(\?|$)/, async (r) => {
    if (r.request().method() === 'PUT') {
      puts += 1;
      await r.fulfill({ json: { success: true, data: PURGE_DATA } });
      return;
    }
    await r.fulfill({
      json: {
        success: true,
        data: { ...PURGE_DATA, settings: { ...PURGE_DATA.settings, rewrite_rules: many } },
      },
    });
  });
  await gotoPurge(page);

  await expect(page.getByTestId('purge-rules-over-cap')).toBeVisible();
  await page.getByLabel('重试上限').fill('6');

  // ① 取消 → 不发 PUT
  page.once('dialog', (d) => void d.dismiss());
  await page.getByRole('button', { name: '保存' }).click();
  await page.waitForTimeout(600);
  expect(puts).toBe(0);

  // ② 确认 → 发 PUT
  page.once('dialog', (d) => void d.accept());
  await page.getByRole('button', { name: '保存' }).click();
  await expect.poll(() => puts).toBe(1);
});

test('29 · 「放弃修改」先确认，取消则改动保留', async ({ page }) => {
  await mockPurge(page);
  await gotoPurge(page);

  await page.getByLabel('重试上限').fill('9');
  await expect(page.getByTestId('purge-dirty-changes')).toBeVisible();

  // ① 取消 → 草稿不变、仍是未保存状态
  page.once('dialog', (d) => void d.dismiss());
  await page.getByRole('button', { name: '放弃修改' }).click();
  await expect(page.getByLabel('重试上限')).toHaveValue('9');
  await expect(page.locator('.purge-dirty-bar')).toBeVisible();

  // ② 确认 → 回服务器值、未保存提示消失
  page.once('dialog', (d) => void d.accept());
  await page.getByRole('button', { name: '放弃修改' }).click();
  await expect(page.getByLabel('重试上限')).toHaveValue('8');
  await expect(page.locator('.purge-dirty-bar')).toHaveCount(0);
});

test('28 · 保存成功后 header 出现「上次保存」时间戳', async ({ page }) => {
  await page.route(/\/api\/purge(\?|$)/, async (r) => {
    if (r.request().method() === 'PUT') {
      await r.fulfill({ json: { success: true, data: PURGE_DATA } });
      return;
    }
    await r.fulfill({ json: { success: true, data: PURGE_DATA } });
  });
  await gotoPurge(page);

  // 未保存过：没有时间戳（不编造历史）
  await expect(page.getByTestId('purge-saved-at')).toHaveCount(0);

  await page.getByLabel('重试上限').fill('6');
  await page.getByRole('button', { name: '保存' }).click();

  const stamp = page.getByTestId('purge-saved-at');
  await expect(stamp).toBeVisible();
  await expect(stamp).toContainText(/上次保存/);
  // 保存后不再显示「有未保存修改」
  await expect(page.locator('.purge-dirty-bar')).toHaveCount(0);
});

test('27 · 词表批量粘贴后可一键撤销回变更前', async ({ page }) => {
  await mockPurge(page);
  await gotoPurge(page);

  const rows = page.locator('.purge-rules .purge-rule-row');
  const before = await rows.count();
  // 撤销按钮在没变更前不存在
  await expect(page.getByTestId('purge-table-undo')).toHaveCount(0);

  await page.getByTestId('purge-batch-paste-open').click();
  await page.getByLabel('批量粘贴内容').fill('甲\tA\n乙\tB');
  await page.getByRole('button', { name: '解析并添加' }).click();
  await expect(rows).toHaveCount(before + 2);

  // 撤销入口出现（点名是哪一次操作）
  const undo = page.getByTestId('purge-table-undo');
  await expect(undo).toBeVisible();
  await expect(undo).toContainText(/批量粘贴/);

  await undo.click();
  await expect(rows).toHaveCount(before);
  // 撤销后入口消失（只保留一步）
  await expect(page.getByTestId('purge-table-undo')).toHaveCount(0);
});

test('26 · 词表字段给非数组时不崩（进入点兜成空表）', async ({ page }) => {
  await mockPurge(page);
  // rewrite_rules 给字符串：`.map()`/`.length` 都在父层渲染作用域，guard 接不住
  await page.route(/\/api\/purge(\?|$)/, (r) =>
    r.fulfill({
      json: {
        success: true,
        data: { ...PURGE_DATA, settings: { ...PURGE_DATA.settings, rewrite_rules: 'oops' } },
      },
    }),
  );
  await gotoPurge(page);

  await expect(page.getByText('出错了')).toHaveCount(0);
  // 兜成空表 → 空态文案，而不是崩或者渲染出怪东西
  await expect(page.getByText(/暂无改写规则/)).toBeVisible();
  await expect(page.getByRole('button', { name: '保存' })).toBeVisible();
});

test('25 · runtime 缺失时页面不崩（零值兜底，header 显示 PID 0）', async ({ page }) => {
  await mockPurge(page);
  // runtime 给 null：`formatUptime(data.runtime.uptime_secs)` 在渲染函数体里、
  // 边界之外 —— 必须靠派生阶段的零值兜底
  await page.route(/\/api\/purge(\?|$)/, (r) =>
    r.fulfill({ json: { success: true, data: { ...PURGE_DATA, runtime: null } } }),
  );
  await gotoPurge(page);

  await expect(page.getByText('出错了')).toHaveCount(0);
  // header 用零值兜底而不是把 null 漏成 TypeError 页
  await expect(page.locator('.purge-meta')).toContainText('PID 0');
  await expect(page.getByRole('button', { name: '保存' })).toBeVisible();
});

test('24 · 统计字段形状畸形时显示「—」，不漏 undefined/0.00%', async ({ page }) => {
  await mockPurge(page);
  // 字符串形状不会崩（`'oops'.responses` 只是 undefined），但会渲染畸形值
  await page.route(/\/api\/purge(\?|$)/, (r) =>
    r.fulfill({ json: { success: true, data: { ...PURGE_DATA, stats: 'oops' } } }),
  );
  await gotoPurge(page);

  const grid = page.locator('.purge-stats');
  await expect(grid).toBeVisible();
  // 不该把内部值漏给用户
  await expect(grid).not.toContainText('undefined');
  await expect(grid).not.toContainText('NaN');
  // 重试率也不该显示成 0.00%（那会被读成「零重试」，与「没数据」相反）
  await expect(grid).not.toContainText('0.00%');
  await expect(grid).toContainText('—');
});

test('23 · stats 缺失时页面不崩（零值兜底，不是区块边界救场）', async ({ page }) => {
  await mockPurge(page);
  // stats 给 null：派生阶段（retryRate = stats.responses ...）就在渲染函数体里，
  // 那里崩任何 SectionGuard 都接不住 —— 所以这里验的是「零值兜底」而不是降级 UI。
  await page.route(/\/api\/purge(\?|$)/, (r) =>
    r.fulfill({ json: { success: true, data: { ...PURGE_DATA, stats: null } } }),
  );
  await gotoPurge(page);

  // 整页正常渲染，没有全局错误页
  await expect(page.getByText('出错了')).toHaveCount(0);
  await expect(page.getByRole('button', { name: '保存' })).toBeVisible();
  // 统计卡展示零值而不是 undefined
  await expect(page.locator('.purge-stats')).toContainText('0');
  // 仍能改并进入未保存状态
  await page.getByLabel('重试上限').fill('5');
  await expect(page.getByTestId('purge-dirty-changes')).toBeVisible();
});

test('22 · 预设库渲染崩了只降级这一块，其余仍可用', async ({ page }) => {
  await mockPurge(page);
  // 覆盖预设列表：items 给非数组 → 预设库子树渲染必崩
  await page.route(/\/api\/purge\/presets(\?|$)/, (r) =>
    r.fulfill({ json: { success: true, data: { items: { oops: 1 }, vendors: [], total: 0 } } }),
  );
  await gotoPurge(page);

  const fallback = page.getByTestId('purge-section-fallback');
  await expect(fallback).toBeVisible();
  await expect(fallback).toContainText(/破甲预设库/);
  await expect(fallback).toContainText(/其余部分仍可用/);

  // 关键：整页没崩 —— 开关和保存还在，用户能把当前改动落库
  await expect(page.getByRole('button', { name: '保存' })).toBeVisible();
  await expect(page.getByLabel('重试上限')).toBeVisible();
});

test('21 · Ctrl+S 真的有提示、也真的保存', async ({ page }, testInfo) => {
  let puts = 0;
  await page.route(/\/api\/purge(\?|$)/, async (r) => {
    if (r.request().method() === 'PUT') {
      puts += 1;
      await r.fulfill({ json: { success: true, data: PURGE_DATA } });
      return;
    }
    await r.fulfill({ json: { success: true, data: PURGE_DATA } });
  });
  await gotoPurge(page);

  // 提示可见（能力藏着等于没有）。触屏 project 里它被 `hover:none` 媒体查询隐藏
  // ——设备没有键盘，不提示才是对的，所以断言按 project 分支。
  const hint = page.getByTestId('purge-shortcut-hint');
  if (testInfo.project.name === 'mobile') {
    await expect(hint).toBeHidden();
  } else {
    await expect(hint).toBeVisible();
  }

  // 未改动时按 Ctrl+S 不该发 PUT（放行浏览器默认行为）
  await page.keyboard.press('Control+s');
  await page.waitForTimeout(800);
  expect(puts).toBe(0);

  // 有改动时 Ctrl+S 才保存
  await page.getByLabel('重试上限').fill('7');
  await page.keyboard.press('Control+s');
  await expect.poll(() => puts).toBe(1);
  await expect(page.locator('.toast', { hasText: '已保存' })).toHaveCount(1);
});

test('20 · 「对比出厂默认」列出偏离项，一致时说一致', async ({ page }) => {
  await mockPurge(page);
  await gotoPurge(page);

  // ① 先制造一处偏离（重试上限 9），再比对
  await page.getByLabel('重试上限').fill('9');
  await page.getByTestId('purge-compare-defaults').click();
  const panel = page.getByTestId('purge-defaults-diff');
  await expect(panel).toBeVisible();
  await expect(panel).toContainText(/相对出厂默认改了 1 项/);
  await expect(panel).toContainText('重试上限');

  // ② 收起后再点：把重试上限填回 mock 里 defaults 的值（8）→ 完全一致
  await page.getByTestId('purge-defaults-diff-close').click();
  await expect(panel).toHaveCount(0);
  await page.getByLabel('重试上限').fill('8');
  await page.getByTestId('purge-compare-defaults').click();
  await expect(page.getByTestId('purge-defaults-diff')).toContainText(/完全一致/);
});

test('19 · 数字输入失焦就地收敛并说明原因（不是保存时才悄悄改）', async ({ page }) => {
  await mockPurge(page);
  await gotoPurge(page);

  const retry = page.getByLabel('重试上限');
  // 超上限：输入 999 → 失焦后变成上限 32，且弹提示说明「已调整为 32」
  await retry.fill('999');
  await retry.blur();
  await expect(retry).toHaveValue('32');
  await expect(page.locator('.toast', { hasText: '已调整为 32' })).toHaveCount(1);

  // 小数：3.7 → 3，提示换一种说法（取整，而不是范围）。
  // 注意按内容定位而不是 .first()：上一条 toast 还没消失时 .first() 会一直拿到它。
  await retry.fill('3.7');
  await retry.blur();
  await expect(retry).toHaveValue('3');
  await expect(page.locator('.toast', { hasText: '已取整为 3' })).toHaveCount(1);

  // 范围内：原样保留、不弹新提示
  await retry.fill('7');
  await retry.blur();
  await expect(retry).toHaveValue('7');
});

test('18 · 有未保存备份时给恢复条，恢复后变回 dirty', async ({ page }) => {
  // 备份与 mock 的 settings 必须不同，才会弹恢复条
  const backupSettings = { ...PURGE_DATA.settings, retry_limit: 42 };
  await page.addInitScript((payload) => {
    localStorage.setItem('aigx.purge.draftBackup', JSON.stringify(payload));
  }, { settings: backupSettings, savedAt: Date.now() });
  await mockPurge(page);
  await gotoPurge(page);

  const banner = page.getByTestId('purge-draft-backup');
  await expect(banner).toBeVisible();

  // 恢复 → 草稿变成备份值，且标记为未保存
  await page.getByTestId('purge-draft-backup-restore').click();
  await expect(banner).toHaveCount(0);
  await expect(page.getByLabel('重试上限')).toHaveValue('42');
  await expect(page.locator('.purge-dirty-bar')).toBeVisible();
});

test('17 · 批量粘贴：Excel 两列一次加完，坏行如实回报', async ({ page }) => {
  await mockPurge(page);
  await gotoPurge(page);

  const before = await page.locator('.purge-rules .purge-rule-row').count();
  await page.getByTestId('purge-batch-paste-open').click();
  const panel = page.getByTestId('purge-batch-paste-panel');
  await expect(panel).toBeVisible();

  // 三行有效（Tab / 箭头都认）+ 一行格式不对
  await page.getByLabel('批量粘贴内容').fill('甲\tA\n乙 -> B\n丙 → C\n这一行没有分隔符');
  await page.getByRole('button', { name: '解析并添加' }).click();

  await expect(page.locator('.toast').first()).toContainText(/已添加 3 条/);
  await expect(page.locator('.toast').first()).toContainText(/1 行未能解析/);
  await expect(page.locator('.purge-rules .purge-rule-row')).toHaveCount(before + 3);
  // 面板自动收起（加完不该还占着屏幕）
  await expect(panel).toHaveCount(0);
});

test('16 · 三个复制入口真的把内容写进剪贴板', async ({ page }) => {
  // 剪贴板在无头环境不可靠：拦 writeText 记录内容，断言「复制出来的东西」而不只是按钮存在
  await page.addInitScript(() => {
    (window as unknown as { __copied: string[] }).__copied = [];
    Object.defineProperty(navigator, 'clipboard', {
      configurable: true,
      value: {
        writeText: (t: string) => {
          (window as unknown as { __copied: string[] }).__copied.push(t);
          return Promise.resolve();
        },
      },
    });
  });
  await mockPurge(page);
  await gotoPurge(page);

  const read = () => page.evaluate(() => (window as unknown as { __copied: string[] }).__copied);

  // ① 生效项：每行 `- ` 前缀
  await page.getByTestId('purge-active-copy').click();
  await expect.poll(async () => (await read()).length).toBe(1);
  expect((await read())[0]).toMatch(/^- /m);

  // ② 体检结果：mock 里有 3 条告警（info/error/warn）→ 按级分组输出
  await page.getByTestId('purge-warning-copy').click();
  await expect.poll(async () => (await read()).length).toBe(2);
  const report = (await read())[1];
  expect(report).toContain('## 不生效');
  expect(report).toContain('异源递推空了');
  expect(report).toContain('## 需确认');
  expect(report).toContain('## 提示');

  // ③ 现状快照：四段标题齐全
  await page.getByTestId('purge-snapshot-copy').click();
  await expect.poll(async () => (await read()).length).toBe(3);
  const snapshot = (await read())[2];
  for (const heading of ['## 运行', '## 当前生效项', '## 配置体检', '## 统计']) {
    expect(snapshot).toContain(heading);
  }
});

test('15 · 首次未改密的 403 要给改密引导，不是「加载失败」', async ({ page }) => {
  // 只 mock 这一个接口：返回 403 + PASSWORD_CHANGE_REQUIRED（真后端的实际形状）
  await page.route(/\/api\/purge(\?|$)/, (r) =>
    r.fulfill({
      status: 403,
      json: { code: 'PASSWORD_CHANGE_REQUIRED', error: 'Password change required', success: false },
    }),
  );
  await loginAsAdmin(page);
  await page.goto('/purge');

  await expect(page.getByTestId('purge-need-password-change')).toBeVisible();
  await expect(page.getByTestId('purge-need-password-change')).toContainText(/修改密码/);
  // 反向：不该出现通用失败文案
  await expect(page.getByText('网络异常或服务不可用，请稍后重试')).toHaveCount(0);
});

test('14 · 「清零统计」只清计数、不动配置', async ({ page }) => {
  let resetCalls = 0;
  let putCalls = 0;
  await page.route(/\/api\/purge\/stats\/reset$/, (r) => {
    resetCalls += 1;
    return r.fulfill({ json: { success: true, data: { stats: { ...PURGE_DATA.stats } } } });
  });
  await page.route(/\/api\/purge(\?|$)/, async (r) => {
    if (r.request().method() === 'PUT') putCalls += 1;
    await r.fulfill({ json: { success: true, data: PURGE_DATA } });
  });
  await gotoPurge(page);

  page.on('dialog', (d) => void d.accept());
  await page.getByTestId('purge-stats-reset').click();
  await expect(page.locator('.toast').first()).toContainText(/统计已清零/);
  expect(resetCalls).toBe(1);
  // 清零不等于保存配置：不该有任何 PUT
  expect(putCalls).toBe(0);
  // 基线时刻可见（清零后「全程」不再等于「进程启动至今」）
  await expect(page.getByTestId('purge-stats-since')).toBeVisible();
});

test('13 · 整页无横向溢出（窄视口下最容易暴露布局问题）', async ({ page }) => {
  await mockPurge(page);
  await gotoPurge(page);

  // 展开最有内容的几块：词表带「试」按钮、预设库带 chips、统计 8 卡
  const overflow = await page.evaluate(() => {
    const doc = document.documentElement;
    const clipped: string[] = [];
    document.querySelectorAll<HTMLElement>('.purge-console *').forEach((el) => {
      const style = getComputedStyle(el);
      const spills = el.scrollWidth - el.clientWidth > 1 && el.clientWidth > 0;
      if (!spills) return;
      // `overflow-x: visible`：内容溢出但不产生滚动条、也不裁剪（开关的 44×44
      // 透明触摸区就会让父容器的 scrollWidth 比 clientWidth 大几像素）——不是缺陷。
      // `auto/scroll`：本来就是可横向滚动的容器（页内导航、代码块）——设计如此。
      // 真正要拦的是 `hidden`：内容被裁掉，用户看不到也滚不到。
      if (style.overflowX !== 'hidden') return;
      clipped.push(`${el.className || el.tagName}:${el.scrollWidth}>${el.clientWidth}`);
    });
    return {
      page: doc.scrollWidth - doc.clientWidth,
      clipped: clipped.slice(0, 5),
    };
  });

  expect(overflow.page).toBeLessThanOrEqual(1);
  expect(overflow.clipped).toEqual([]);
});

test('12 · 技能卡显示「实际扫描目录」（放错目录时靠它定位）', async ({ page }) => {
  await mockPurge(page);
  await gotoPurge(page);

  const hint = page.getByTestId('purge-skill-dir-resolved');
  await expect(hint).toBeVisible();
  await expect(hint).toContainText('/home/e2e/.aigx/skills');
  // 空目录同时给空态文案（不是整块消失）
  await expect(page.getByText(/扫不到任何 SKILL.md/)).toBeVisible();
});

test('11 · 页内导航条可跳到各区块', async ({ page }) => {
  await mockPurge(page);
  await gotoPurge(page);

  const nav = page.getByTestId('purge-pagenav');
  await expect(nav).toBeVisible();
  await expect(nav.getByRole('button')).toHaveCount(7);

  // 点「改写预览」→ 落点拿到闪烁类（与体检「去修复」同一套机制）
  await nav.getByRole('button', { name: '改写预览' }).click();
  await expect(page.locator('[data-purge-fix="preview"].purge-fix-flash')).toHaveCount(1);
});

test('9 · 搜到正文时展开给出命中片段并高亮', async ({ page }) => {
  const body = '前置上下文'.repeat(12) + 'UniqueNeedleToken' + '后置上下文'.repeat(12);
  await page.route(/\/api\/purge\/presets\/[^/]+$/, (r) =>
    r.fulfill({
      json: {
        success: true,
        data: {
          id: 'p-body',
          vendor: 'V',
          model: 'm',
          title: 'Body hit preset',
          tags: [],
          bytes: body.length,
          sha256: 'deadbeef'.repeat(8),
          body,
        },
      },
    }),
  );
  await page.route(/\/api\/purge\/presets(\?|$)/, (r) =>
    r.fulfill({
      json: {
        success: true,
        data: {
          items: [
            {
              id: 'p-body',
              vendor: 'V',
              model: 'm',
              title: 'Body hit preset',
              tags: [],
              bytes: body.length,
              sha256: 'deadbeef'.repeat(8),
              preview: body.slice(0, 200),
              body_match: true,
            },
          ],
          vendors: [{ vendor: 'V', count: 1 }],
          total: 1,
        },
      },
    }),
  );
  await mockPurge(page);
  await gotoPurge(page);

  await page.getByLabel('搜索预设').fill('uniqueneedletoken');
  await expect(page.getByText('正文命中')).toBeVisible();

  await page.getByText('Body hit preset').first().click();
  const snippet = page.getByTestId('purge-preset-snippet');
  await expect(snippet).toBeVisible();
  // 命中词在 <mark> 里且保留原文大小写
  await expect(snippet.locator('mark')).toHaveText('UniqueNeedleToken');
});

test('8 · 预设库搜索框告知检索范围覆盖正文', async ({ page }) => {
  await mockPurge(page);
  // 预设列表本身由另一个 spec 覆盖；这里只要搜索框的检索范围提示正确
  await page.route(/\/api\/purge\/presets(\?|$)/, (r) =>
    r.fulfill({ json: { success: true, data: { items: [], vendors: [], total: 0 } } }),
  );
  await gotoPurge(page);

  const search = page.getByLabel('搜索预设');
  await expect(search).toBeVisible();
  await expect(search).toHaveAttribute('placeholder', /正文/);
});

test('7 · 「重置为默认」先确认再填草稿（不直接保存）', async ({ page }) => {
  await mockPurge(page);
  await gotoPurge(page);

  let saved = 0;
  await page.route(/\/api\/purge(\?|$)/, async (r) => {
    if (r.request().method() === 'PUT') {
      saved += 1;
      await r.fulfill({ json: { success: true, data: PURGE_DATA } });
      return;
    }
    await r.fulfill({ json: { success: true, data: PURGE_DATA } });
  });

  page.on('dialog', (d) => void d.accept());
  // 未保存警示条里才有这个按钮：先制造一次编辑
  await page.getByLabel('重试上限').fill('9');
  await page.getByRole('button', { name: '重置为默认' }).click();
  await expect(page.locator('.purge-dirty-bar')).toBeVisible();
  expect(saved).toBe(0);
});
