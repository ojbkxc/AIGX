import { test, expect, loginAsAdmin } from './fixtures';

/**
 * 签到计划编辑器契约字段（#16 前端 + be-fix #16-B）。
 *
 * 逐家时刻覆盖（provider_times/builtin_times）后端已就绪 → 直接渲染；
 * 随机区间（time_end）/补签与重试（retry_time/retry_gap_minutes/retry_limit）
 * 按 feature-detect 门控：后端 schedules 响应含该字段才渲染与提交，
 * 旧后端不误发（发也存不下，编辑回显会丢）。
 * 全部用 page.route 覆盖 GET /api/checkin-center，不动 static-server 桩
 * （桩文件属 fe-lists-fix 域）。
 */
test.describe('签到计划编辑器（契约字段门控）', () => {
  /** 供编辑流程用的完整 center 数据：契约字段齐全 */
  const CENTER_WITH_CONTRACT = {
    settings: { enabled: true, gap_ms: 500, timezone_offset: 8, retry_gap_ms: 600_000, keep_days: 30 },
    supported_providers: ['workbuddy', 'workbuddy-intl'],
    providers: [],
    schedules: [
      {
        id: 'sch-1', label: '每日 09:00', enabled: true, time: '09:00',
        providers: ['workbuddy'],
        provider_times: { workbuddy: '10:05' },
        time_end: '09:30',
        retry_time: '12:00',
        retry_gap_minutes: 15,
        retry_limit: 5,
        last_fired_date: '', last_result: '', last_ok: 0, last_keepalive: 0, last_failed: 0,
      },
    ],
    records_total: 2,
    today_summary: [{ provider: 'workbuddy', done: 1, total: 2 }],
    builtin_times: { workbuddy: '10:01' },
  };

  test('设置卡：时区/重试间隔/台账保留渲染与保存', async ({ page, pageErrors }) => {
    test.skip((page.viewportSize()?.width ?? 0) < 900, '桌面设置卡形态');
    await loginAsAdmin(page, { sessions: [] });
    await page.goto('/checkin');
    await page.locator('.sub-tab', { hasText: '定时签到' }).click();

    // 三行新设置（缺省值：时区 8=UTC+8、重试 10 分钟、保留 0=永久）
    await expect(page.getByLabel('签到时区')).toHaveValue('8');
    await expect(page.getByLabel('失败/窗口重试间隔（分钟）')).toHaveValue('10');
    await expect(page.getByLabel('台账保留天数')).toHaveValue('0');

    // 改时区 → 即时保存，PUT body 带 timezone_offset: 0（跟随系统）
    let tzPayload: Record<string, unknown> | null = null;
    await page.route('**/api/checkin-center/settings', async (route) => {
      if (route.request().method() === 'PUT') {
        tzPayload = route.request().postDataJSON() as Record<string, unknown>;
        const body = tzPayload as Record<string, unknown>;
        return route.fulfill({
          status: 200, contentType: 'application/json',
          body: JSON.stringify({ success: true, data: body }),
        });
      }
      return route.fallback();
    });
    await page.getByLabel('签到时区').selectOption({ label: '跟随系统本地时区' });
    await expect.poll(() => tzPayload).not.toBeNull();
    expect((tzPayload as Record<string, unknown>).timezone_offset).toBe(0);
    // changeTimezone 成功后会 reload（GET 未被拦截 → 桩缺省值 8）：等它落定，
    // 避免下一组 fill 被重渲染的 setState 覆盖
    await expect(page.getByLabel('签到时区')).toHaveValue('8');

    // 重试间隔 5 分钟 + 保留 30 天 → 一次保存，写回毫秒/天
    let advPayload: Record<string, unknown> | null = null;
    await page.unrouteAll({ behavior: 'ignoreErrors' });
    await page.route('**/api/checkin-center/settings', async (route) => {
      advPayload = route.request().postDataJSON() as Record<string, unknown>;
      return route.fulfill({
        status: 200, contentType: 'application/json',
        body: JSON.stringify({ success: true, data: advPayload }),
      });
    });
    await page.getByLabel('失败/窗口重试间隔（分钟）').fill('5');
    await page.getByLabel('台账保留天数').fill('30');
    await page.locator('.ck-setting-row', { hasText: '台账保留天数' }).getByRole('button', { name: '保存' }).click();
    await expect.poll(() => advPayload).not.toBeNull();
    expect((advPayload as Record<string, unknown>).retry_gap_ms).toBe(300_000);
    expect((advPayload as Record<string, unknown>).keep_days).toBe(30);

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('编辑器：逐家覆盖/预览分组/契约字段渲染与提交', async ({ page, pageErrors }) => {
    test.skip((page.viewportSize()?.width ?? 0) < 900, '桌面编辑器形态');
    await page.route('**/api/checkin-center', (r) =>
      r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ success: true, data: CENTER_WITH_CONTRACT }) }));
    await loginAsAdmin(page, { sessions: [] });
    await page.goto('/checkin');
    await page.locator('.sub-tab', { hasText: '定时签到' }).click();
    await page.locator('.ck-sched-row', { hasText: '每日 09:00' }).getByRole('button', { name: '编辑' }).click();

    // 逐家覆盖行：workbuddy 已覆盖 10:05 → 显示恢复默认；平台专属徽章只在无覆盖时出现
    const pickRow = page.locator('.ck-provider-pick-row', { hasText: 'workbuddy' });
    await expect(pickRow.locator('.ck-pick-tag', { hasText: '平台专属' })).toHaveCount(0);
    await expect(pickRow.getByLabel('workbuddy 签到时刻')).toHaveValue('10:05');
    await expect(pickRow.getByRole('button', { name: '恢复默认' })).toBeVisible();

    // 实际签到安排：按生效时刻 10:05 分组
    await expect(page.locator('.ck-preview-group code')).toHaveText('10:05');
    await expect(page.locator('.ck-preview-group')).toContainText('workbuddy');

    // 契约字段门控块可见且带回显：随机区间 09:30 / 补签 12:00 / 间隔 15 / 次数 5
    await expect(page.locator('.ck-editor').getByText('随机签到区间（可选）')).toBeVisible();
    await expect(page.getByLabel('区间结束')).toHaveValue('09:30');
    await expect(page.locator('.ck-editor').getByText('补签与重试')).toBeVisible();
    await expect(page.getByLabel('定时补签时刻（留空 = 关闭）')).toHaveValue('12:00');
    await expect(page.getByLabel('补签/重试间隔（分钟）')).toHaveValue('15');
    await expect(page.getByLabel('每账号每天最多重试（次，0 = 关）')).toHaveValue('5');

    // 保存：POST body 带全部契约字段
    let saved: Record<string, unknown> | null = null;
    await page.route('**/api/checkin-center/schedules', async (route) => {
      saved = route.request().postDataJSON() as Record<string, unknown>;
      return route.fulfill({
        status: 200, contentType: 'application/json',
        body: JSON.stringify({ success: true, data: CENTER_WITH_CONTRACT.schedules }),
      });
    });
    await page.locator('.ck-form-actions').getByRole('button', { name: '保存' }).click();
    await expect.poll(() => saved).not.toBeNull();
    const body = saved as Record<string, unknown>;
    expect(body.provider_times).toEqual({ workbuddy: '10:05' });
    expect(body.time_end).toBe('09:30');
    expect(body.retry_time).toBe('12:00');
    expect(body.retry_gap_minutes).toBe(15);
    expect(body.retry_limit).toBe(5);

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('旧后端形状：门控块不渲染，保存不携带契约字段', async ({ page, pageErrors }) => {
    test.skip((page.viewportSize()?.width ?? 0) < 900, '桌面编辑器形态');
    // 不覆盖 GET center：桩数据无 time_end/retry_time（旧后端形状）
    await loginAsAdmin(page, { sessions: [] });
    await page.goto('/checkin');
    await page.locator('.sub-tab', { hasText: '定时签到' }).click();
    await page.getByRole('button', { name: '添加条目' }).click();

    await expect(page.locator('.ck-editor').getByText('随机签到区间（可选）')).toHaveCount(0);
    await expect(page.locator('.ck-editor').getByText('补签与重试')).toHaveCount(0);

    let saved: Record<string, unknown> | null = null;
    await page.route('**/api/checkin-center/schedules', async (route) => {
      saved = route.request().postDataJSON() as Record<string, unknown>;
      return route.fulfill({
        status: 200, contentType: 'application/json',
        body: JSON.stringify({ success: true, data: [] }),
      });
    });
    await page.locator('.ck-editor').getByLabel('名称').fill('回归用计划');
    await page.locator('.ck-form-actions').getByRole('button', { name: '保存' }).click();
    await expect.poll(() => saved).not.toBeNull();
    const body = saved as Record<string, unknown>;
    expect(body.time_end).toBeUndefined();
    expect(body.retry_time).toBeUndefined();
    expect(body.retry_gap_minutes).toBeUndefined();
    expect(body.retry_limit).toBeUndefined();
    expect(body.provider_times).toEqual({});

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });
});

/**
 * 签到时区边界（#21 补做项，lead 指示：边界值显式构造）。
 *
 * 「下次执行」的日切口径 = settings.timezone_offset（store.rs:132，小时；
 * 0=跟随系统）——展示实现 src/lib/checkinTz.ts（nextRunLabel），与后端
 * scheduler.rs beijing() 同语义。本用例锁 offset=8 档的**日切边界**：
 * 北京 23:30 现刻，23:45 未到 → 今天；09:00 已过 → 明天（跨日）。
 *
 * page.clock 定针（Playwright 1.63）：日期动态取「今天 UTC 15:30Z」——
 * 不冻结到历史日期，避免 fixtures 登录态（24h 有效期）被判过期。
 * offset=0（跟随系统帧）的边界矩阵由 vitest checkinTz.test.ts 显式覆盖
 * （镜像断言，e2e 无法控制运行机器时区，不在浏览器层重复）。
 *
 * mock 形状锚点：center payload 出处 static-server.mjs:236-255（CHECKIN_CENTER）
 * + types/index.ts CheckinCenterData；timezone_offset 语义 = store.rs:132。
 */
test.describe('签到时区边界（page.clock 定针）', () => {
  test('UTC+8：日切前未到 → 今天；已过 → 明天（跨日）', async ({ page, pageErrors }) => {
    test.skip((page.viewportSize()?.width ?? 0) < 900, '桌面设置卡形态');
    // 定针「今天 UTC 15:30Z」= 北京 23:30（UTC+8）
    const now = new Date();
    const pinnedMs = Date.UTC(now.getUTCFullYear(), now.getUTCMonth(), now.getUTCDate(), 15, 30, 0);
    await page.clock.setFixedTime(new Date(pinnedMs));

    // 两条计划：23:45（北京帧未到 → 今天）与 09:00（已过 → 明天跨日）
    await page.route('**/api/checkin-center', (r) =>
      r.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify({
          success: true,
          data: {
            settings: { enabled: true, gap_ms: 500, timezone_offset: 8 },
            supported_providers: ['workbuddy'],
            providers: [],
            schedules: [
              { id: 'sch-1', label: '夜班', enabled: true, time: '23:45', providers: ['workbuddy'], last_fired_date: '', last_result: '', last_ok: 0, last_keepalive: 0, last_failed: 0 },
              { id: 'sch-2', label: '早班', enabled: true, time: '09:00', providers: ['workbuddy'], last_fired_date: '', last_result: '', last_ok: 0, last_keepalive: 0, last_failed: 0 },
            ],
            records_total: 0,
            today_summary: [],
          },
        }),
      }));
    await loginAsAdmin(page);
    await page.goto('/checkin');
    await page.locator('.sub-tab', { hasText: '定时签到' }).click();

    // 期望值：北京帧（UTC+8 数学）今天/明天的 MM-DD，测试内现算
    const pad = (n: number): string => String(n).padStart(2, '0');
    const bj = new Date(pinnedMs + 8 * 3_600_000);
    const bjTomorrow = new Date(pinnedMs + 8 * 3_600_000 + 86_400_000);
    const todayLabel = `${pad(bj.getUTCMonth() + 1)}-${pad(bj.getUTCDate())} 23:45`;
    const tomorrowLabel = `${pad(bjTomorrow.getUTCMonth() + 1)}-${pad(bjTomorrow.getUTCDate())} 09:00`;

    const rowNight = page.locator('.ck-sched-row', { hasText: '夜班' });
    await expect(rowNight.locator('.ck-sched-next')).toContainText(todayLabel);
    const rowMorning = page.locator('.ck-sched-row', { hasText: '早班' });
    await expect(rowMorning.locator('.ck-sched-next')).toContainText(tomorrowLabel);

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });
});
