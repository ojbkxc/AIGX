import { test, expect, loginAsAdmin } from './fixtures';
import type { Page } from '@playwright/test';

/**
 * 破甲预设库 1.0.72 验收面（docs/SPEC-1.0.72-PURGE-PRESETS.md）。
 *
 * ┌─────────────────────────────────────────────────────────────────────┐
 * │ mock 形状契约锚点（逐字段出处 = src/api/admin/purge.rs +            │
 * │ src/purge/presets.rs，防「前端假定形状」假绿）：                     │
 * │  GET  /api/purge                    → PurgeData                     │
 * │  GET  /api/purge/presets?q&vendor&limit                              │
 * │      → { items: PresetBrief[], vendors: {vendor,count}[], total }    │
 * │      PresetBrief = { id,vendor,model,title,tags,bytes,sha256,preview}│
 * │  GET  /api/purge/presets/{id}       → PresetDetail（同上 + body）    │
 * │  POST /api/purge/presets/{id}/apply → { applied, settings }          │
 * │      body = { target: 'structured_template'|'skill_file', activate } │
 * │  mock 分流纪律：列表用 `/\/api\/purge\/presets(\?|$)/` 精确 regex，  │
 * │  详情/apply 用更长的路径单独注册（Playwright 后注册优先，且长路径    │
 * │  不会命中列表 regex——`presets?` 与 `presets/` 前缀不同）。           │
 * └─────────────────────────────────────────────────────────────────────┘
 */

const SETTINGS = {
  enabled: false,
  system_normalize: false,
  dead_zone_rewrite: false,
  synonym_enabled: false,
  reduce_reasoning: false,
  cross_source_fallback: false,
  skill_inject: false,
  structured_prompt: false,
  reasoning_floor: 'low',
  retry_limit: 3,
  fallback_trigger_attempt: 2,
  fallback_models: [],
  rewrite_rules: [],
  synonym_pairs: [],
  normalize_strip_prefixes: [],
  skill_dir: '~/.aigx/skills',
  skill_inject_limit: 2000,
  structured_suffix: '',
  structured_templates: [],
  active_template: '',
  refusal_patterns: [],
  failure_filter_keywords: [],
  failure_empty_keywords: [],
  upstream_label: 'e2e',
};

const PURGE_DATA = {
  settings: SETTINGS,
  stats: {
    responses: 0,
    retries: 0,
    early_success: 0,
    skill_hits: 0,
    refusals: 0,
    failure_refusal: 0,
    failure_empty: 0,
    failure_filter: 0,
    failure_other: 0,
    synonym_hits: 0,
  },
  runtime: { pid: 1, uptime_secs: 10, upstream: 'e2e', inflight: 0 },
  config_warnings: [],
  skills: [],
};

/** 两条预设：一条 Anthropic、一条 xAI，用来验证 chip 过滤 */
const PRESET_A = {
  id: 'anthropic-opus-v2',
  vendor: 'Anthropic',
  model: 'opus v2',
  title: 'opus v2',
  tags: ['jailbreak', 'persona'],
  bytes: 26620,
  sha256: 'a1b2c3d4e5f60718',
  preview: 'Anthropic 预览正文',
};
const PRESET_B = {
  id: 'xai-grok-build',
  vendor: 'xAI',
  model: 'Grok build',
  title: 'Grok build',
  tags: ['policy'],
  bytes: 4096,
  sha256: '0011223344556677',
  preview: 'xAI 预览正文',
};

const BODY_A = 'Anthropic 完整正文\n第二行';

function listResponse(items: typeof PRESET_A[]) {
  const counts = new Map<string, number>();
  for (const item of items) counts.set(item.vendor, (counts.get(item.vendor) ?? 0) + 1);
  return {
    success: true,
    data: {
      items,
      vendors: [...counts.entries()].map(([vendor, count]) => ({ vendor, count })),
      total: items.length,
    },
  };
}

/** 安装默认桩；返回 apply 请求体收集数组，供断言落点参数 */
async function mockPresets(page: Page): Promise<Array<Record<string, unknown>>> {
  const applyBodies: Array<Record<string, unknown>> = [];

  await page.route(/\/api\/purge\/presets\/[^/]+\/apply$/, async (r) => {
    applyBodies.push(r.request().postDataJSON() as Record<string, unknown>);
    await r.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({
        success: true,
        data: {
          applied: { target: 'skill_file', name: 'opus v2', path: '~/.aigx/skills/anthropic-opus-v2/SKILL.md' },
          settings: SETTINGS,
        },
      }),
    });
  });

  await page.route(/\/api\/purge\/presets\/[^/]+$/, async (r) => {
    const id = r.request().url().split('/presets/')[1];
    const base = id === PRESET_A.id ? PRESET_A : PRESET_B;
    await r.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({ success: true, data: { ...base, body: id === PRESET_A.id ? BODY_A : 'xAI 完整正文' } }),
    });
  });

  await page.route(/\/api\/purge\/presets(\?|$)/, async (r) => {
    const url = new URL(r.request().url());
    const q = (url.searchParams.get('q') ?? '').toLowerCase();
    const vendor = url.searchParams.get('vendor') ?? '';
    let items = [PRESET_A, PRESET_B];
    if (vendor) items = items.filter((i) => i.vendor === vendor);
    if (q) items = items.filter((i) => `${i.title} ${i.vendor} ${i.model}`.toLowerCase().includes(q));
    await r.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify(listResponse(items)),
    });
  });

  return applyBodies;
}

async function gotoPurge(page: Page): Promise<void> {
  await page.goto('/purge');
  await expect(page.getByText('破甲预设库')).toBeVisible({ timeout: 10000 });
}

test.describe('破甲预设库 1.0.72', () => {
  test.beforeEach(async ({ page }) => {
    await page.route(/\/api\/purge(\?|$)/, (r) =>
      r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ success: true, data: PURGE_DATA }) }));
    await loginAsAdmin(page);
  });

  test('1 · 列表渲染 title / 厂商·模型 / KB / sha256 前 8 位与总数', async ({ page }) => {
    await mockPresets(page);
    await gotoPurge(page);

    const items = page.getByTestId('purge-preset-item');
    await expect(items).toHaveCount(2);
    await expect(page.getByText('共 2 条')).toBeVisible();

    const first = items.first();
    await expect(first).toContainText('opus v2');
    await expect(first).toContainText('Anthropic · opus v2');
    await expect(first).toContainText('26.0 KB');
    await expect(first).toContainText('a1b2c3d4');
  });

  test('2 · 厂商 chip 过滤只留该厂商，且 chip 计数跟随', async ({ page }) => {
    await mockPresets(page);
    await gotoPurge(page);

    await page.getByRole('button', { name: /^xAI/ }).click();
    await expect(page.getByTestId('purge-preset-item')).toHaveCount(1);
    await expect(page.getByTestId('purge-preset-item').first()).toContainText('Grok build');
    await expect(page.getByText('共 1 条')).toBeVisible();

    await page.getByRole('button', { name: '全部' }).click();
    await expect(page.getByTestId('purge-preset-item')).toHaveCount(2);
  });

  test('3 · 搜索防抖后按标题过滤，无命中显示空态', async ({ page }) => {
    await mockPresets(page);
    await gotoPurge(page);

    await page.getByLabel('搜索预设').fill('grok');
    // 防抖 250ms：等后端桩返回后的结果
    await expect(page.getByTestId('purge-preset-item')).toHaveCount(1, { timeout: 5000 });

    await page.getByLabel('搜索预设').fill('不存在的关键词');
    await expect(page.getByText('没有匹配的预设')).toBeVisible({ timeout: 5000 });
  });

  test('4 · 展开后预览面板显示完整正文（非 200 字截断）', async ({ page }) => {
    await mockPresets(page);
    await gotoPurge(page);

    await page.getByTestId('purge-preset-item').first().click();
    const preview = page.getByTestId('purge-preset-preview');
    await expect(preview).toBeVisible();
    await expect(preview).toContainText('Anthropic 完整正文');
    await expect(preview).toContainText('第二行');
  });

  test('5 · 「设为结构化模板」写入草稿并置脏（不自动 PUT）', async ({ page }) => {
    let putCount = 0;
    await page.route(/\/api\/purge(\?|$)/, (r) => {
      if (r.request().method() === 'PUT') {
        putCount += 1;
        return r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ success: true, data: PURGE_DATA }) });
      }
      return r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ success: true, data: PURGE_DATA }) });
    });
    await mockPresets(page);
    await gotoPurge(page);

    await page.getByTestId('purge-preset-item').first().click();
    await page.getByTestId('purge-preset-apply-template').click();

    // 模板名出现在下方「结构化模板」区的启用下拉里
    await expect(page.getByText('已写入模板草稿，点底部「保存」后生效')).toBeVisible();
    await expect(page.locator('select option', { hasText: 'opus v2' })).toHaveCount(1);
    // 未保存：保存按钮从禁用变可用，且没有任何 PUT 发生
    await expect(page.getByRole('button', { name: '保存', exact: true })).toBeEnabled();
    expect(putCount).toBe(0);
  });

  test('6 · 「写入技能目录」带 target=skill_file 且刷新技能快照', async ({ page }) => {
    const bodies = await mockPresets(page);
    await gotoPurge(page);

    await page.getByTestId('purge-preset-item').first().click();
    await page.getByTestId('purge-preset-apply-skill').click();

    await expect(page.getByText(/已写入技能目录：/)).toBeVisible();
    expect(bodies).toHaveLength(1);
    expect(bodies[0]).toEqual({ target: 'skill_file', activate: false });
  });

  test('7 · 授权提示常驻可见（不是可关闭的 toast）', async ({ page }) => {
    await mockPresets(page);
    await gotoPurge(page);
    await expect(page.locator('.purge-preset-notice')).toContainText('仅供授权的防御性安全评估使用');
  });
});
