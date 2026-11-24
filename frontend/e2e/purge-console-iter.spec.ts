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
