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
  // 三类严重级各一条：error 必须排在 info 前面
  config_warning_items: [
    { code: 'preset_missing', message: '预设 id 不存在', severity: 'info' },
    { code: 'fallback_empty', message: '异源递推空了', severity: 'error' },
    { code: 'chained_rewrite', message: '链式二次改写', severity: 'warn' },
  ],
  config_warnings: [],
  skills: [],
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
  await page.route(/\/api\/purge(\?|$)/, (r) =>
    r.fulfill({ json: { success: true, data: PURGE_DATA } }),
  );
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
