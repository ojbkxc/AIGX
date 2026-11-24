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
