import { test, expect } from './fixtures';

/**
 * 报表时区设置端点 E2E（#20 批3，前端侧）。
 *
 * 后端契约（对账基线，逐条来自真实产出方，不是桩自己定的；对账时间 2026-11-09，
 * 全部在 origin/main tip `ee4ca78f` 上实读）：
 *   · `report_settings_json`（src/api/admin/settings.rs:626-637）吐六个键：
 *     `timezoneOffset`（配置文件值）/ `effectiveOffset`（进程快照，读路径实际用的那个）/
 *     `storedOffset`（存量聚合行口径标记，null = 老库从没记过）/ `needsMigration`
 *     （= `stored_offset != Some(effective)`，null 保守为 true）/ `minOffset` / `maxOffset`；
 *   · PUT（settings.rs:661-696）：缺省字段保持原值（`Option<i32>`）；越界 400
 *     （error_response 形状 `{success:false, error}`，文案含值域与「整数小时 UTC 偏移」）；
 *     保存成功后 `:684` 写快照 ⇒ **热生效**，且 storedOffset 不动（存量重算不在本端点职责内）；
 *   · POST tz-migrate（logs.rs:695-750 + log/mod.rs:1870-1900）：双 alias 请求体、
 *     缺省 offset_to=当前快照 / offset_from=0；**幂等短路**（标记已等于目标偏移 ⇒
 *     不删不回填）；顺序坑（offset_to ≠ 快照）报错**零写入**；
 *   · 四条坑提示（logs.rs:665-685）逐字回给调用方，文案内嵌实际日期与偏移；
 *   · 非整数 body ⇒ axum `JsonDataError` ⇒ **422 + text/plain**（axum-0.7.9 的
 *     rejection.rs:11-19），不是 400 JSON——前端数字输入框的产出若带小数，真实后端
 *     就是这个反应。
 *
 * ⚠️ 顺序坑状态码（#10 裁定）：端点返 **400**（client 前置违例）——与同类 range 检查 400、
 *    坑 1 notice 文案「否则本端点直接 400」、50-report §3.1/§6.3 三处一致；原 handler 把
 *    `tz_migrate` 的 anyhow 错误**一律**映射成 500 是实现漏映射（be-agent-d 单独一笔修，
 *    `tz_migrate` 内部 `ensure!` 保留作纵深防御、存储故障路径仍 500）。
 *
 * 「不许桩自证」的分工：本 spec 断言的是**契约形状与状态机语义**（热生效、幂等短路、
 * needsMigration 判据、四条坑在场），桩的每一分支都按 handler 现码逐字段投影；handler 自身
 * 的逻辑正确性由 be-obs 的 14 条 Rust 单测钉（含 `tz_migrate_is_idempotent_on_second_call`、
 * `report_settings_json_shape_and_migration_flag` 等），前端 e2e 补的是 HTTP 层契约
 * （fe-fix 的 Settings 报表区块将来消费的就是这份形状）。桩里 backfill 的具体计数
 * （scanned/days_written/user_rows_written）是桩数据，只断形状与 `skipped_shrink===0`
 * 这个「窗口先删干净」的不变式，不断桩自己造的数字。
 *
 * 全部用例走 `page.request` 直打桩（前端尚未接线报表设置区块，无 UI 可驱动）。
 * 桩是有状态的（PUT 改快照、迁移落标记）⇒ 每用例 beforeEach 经复位端点回预置态，
 * 防跨用例 / 跨 project（desktop/mobile 串跑）/ 本地 reuseExistingServer 残留。
 */

const GET_SETTINGS = '/api/report/settings';
const MIGRATE = '/api/logs/agg/tz-migrate';
const RESET = '/api/__e2e/reset-report-tz';

/** 日键断言：YYYY-MM-DD */
const DATE_RE = /^\d{4}-\d{2}-\d{2}$/;

test.describe('报表时区设置端点（#20 批3）', () => {
  test.beforeEach(async ({ page }) => {
    const res = await page.request.post(RESET);
    expect(res.ok(), `复位端点失败: ${res.status()}`).toBeTruthy();
  });

  test('GET：六字段全给且不多不少，初始态镜像升级后老库（needsMigration 保守为 true）', async ({ page }) => {
    const res = await page.request.get(GET_SETTINGS);
    expect(res.status()).toBe(200);
    const body = await res.json();
    // 顶层也全等：顺带钉「无多余顶层键」（字段平价的另一半）
    expect(body).toEqual({
      success: true,
      data: {
        timezoneOffset: 8,
        effectiveOffset: 8,
        storedOffset: null,
        needsMigration: true,
        minOffset: -12,
        maxOffset: 14,
      },
    });
  });

  test('PUT 保存即热生效：响应里 effectiveOffset 已变，且保存后立刻 GET 到的是新值', async ({ page }) => {
    // 保存前：快照=缺省 8
    const before = await page.request.get(GET_SETTINGS);
    expect((await before.json()).data.effectiveOffset).toBe(8);

    const put = await page.request.put(GET_SETTINGS, { data: { timezone_offset: 5 } });
    expect(put.status()).toBe(200);
    // 保存成功的响应里 effectiveOffset 就已经是新值（handler 先写快照再组响应，settings.rs:684-688）
    expect(await put.json()).toEqual({
      success: true,
      data: {
        timezoneOffset: 5,
        effectiveOffset: 5,
        storedOffset: null,
        needsMigration: true,
        minOffset: -12,
        maxOffset: 14,
      },
    });

    // 保存后立刻 GET：effectiveOffset 必须已经是新值（热生效＝不重启、不重读配置）
    const after = await page.request.get(GET_SETTINGS);
    expect((await after.json()).data).toMatchObject({
      timezoneOffset: 5,
      effectiveOffset: 5,
      storedOffset: null,
      needsMigration: true,
    });
  });

  test('PUT 收 snake/camel 双拼法；缺省与显式 null 都保持原值', async ({ page }) => {
    // camelCase 别名（ReportSettingsRequest 的 serde alias）
    const alias = await page.request.put(GET_SETTINGS, { data: { timezoneOffset: -4 } });
    expect(alias.status()).toBe(200);
    expect((await alias.json()).data).toMatchObject({ timezoneOffset: -4, effectiveOffset: -4 });

    // 缺省字段 = 保持原值（Option 口径：{} 与 {"timezone_offset": null} 同态）
    const empty = await page.request.put(GET_SETTINGS, { data: {} });
    expect(empty.status()).toBe(200);
    expect((await empty.json()).data).toMatchObject({ timezoneOffset: -4, effectiveOffset: -4 });

    const nullBody = await page.request.put(GET_SETTINGS, { data: { timezone_offset: null } });
    expect(nullBody.status()).toBe(200);
    expect((await nullBody.json()).data).toMatchObject({ timezoneOffset: -4, effectiveOffset: -4 });
  });

  test('PUT 值域：-12/0/14 合法（0=跟随系统本地），15/-13 返 400 且不改状态', async ({ page }) => {
    for (const v of [-12, 0, 14]) {
      const res = await page.request.put(GET_SETTINGS, { data: { timezone_offset: v } });
      expect(res.status(), `PUT ${v} 应 200`).toBe(200);
      expect((await res.json()).data.effectiveOffset).toBe(v);
    }
    // 越界：400 + error_response 形状，文案含值域与「整数小时 UTC 偏移」字样（settings.rs:669-676）
    for (const v of [15, -13]) {
      const bad = await page.request.put(GET_SETTINGS, { data: { timezone_offset: v } });
      expect(bad.status(), `PUT ${v} 应 400`).toBe(400);
      expect(await bad.json()).toEqual({
        success: false,
        error: '报表时区偏移必须在 -12~14 之间（整数小时，UTC 偏移）',
      });
    }
    // 400 不改状态：最后一次合法写入是 14
    const get = await page.request.get(GET_SETTINGS);
    expect((await get.json()).data).toMatchObject({ timezoneOffset: 14, effectiveOffset: 14 });
  });

  test('PUT 非整数 body：axum JsonDataError ⇒ 422 + text/plain（不是 400 JSON）', async ({ page }) => {
    const res = await page.request.put(GET_SETTINGS, { data: { timezone_offset: 8.5 } });
    expect(res.status()).toBe(422);
    const text = await res.text();
    expect(text).toContain('Failed to deserialize the JSON body into the target type');
    // 422 不改状态
    const get = await page.request.get(GET_SETTINGS);
    expect((await get.json()).data.effectiveOffset).toBe(8);
  });

  test('tz-migrate 幂等：连调两次，第二次不重做、固定部分与首次一致、终态与单次调用相同', async ({ page }) => {
    // 正序操作：迁移前必须先保存目标偏移（顺序坑的反面）
    const put = await page.request.put(GET_SETTINGS, { data: { timezone_offset: 5 } });
    expect(put.status()).toBe(200);

    const call = () => page.request.post(MIGRATE, { data: { offset_from: 0 } });

    // ── 第一次调用：真实执行迁移（删窗口 + 回填 + 落标记）──
    const first = await call();
    expect(first.status()).toBe(200);
    const firstBody = await first.json();
    expect(firstBody.success).toBe(true);
    // tzEffectiveFrom 与 data.effective_from 同值（logs.rs:743 的注释口径）
    expect(firstBody.tzEffectiveFrom).toBe(firstBody.data.effective_from);
    expect(firstBody.tzEffectiveFrom).toMatch(DATE_RE);
    expect(firstBody.today).toMatch(DATE_RE);
    // window_to 恒 = 昨天 ⇒ 与 today 不同日
    expect(firstBody.data.window_to).toMatch(DATE_RE);
    expect(firstBody.data.window_to).not.toBe(firstBody.today);
    // 内核报告五键（TzMigrateReport 的 serde 字段名，无 rename_all）
    expect(Object.keys(firstBody.data).sort()).toEqual([
      'backfill',
      'effective_from',
      'rows_deleted',
      'window_applied',
      'window_to',
    ]);
    expect(Object.keys(firstBody.data.backfill).sort()).toEqual([
      'days_written',
      'scanned',
      'skipped_shrink',
      'user_rows_written',
    ]);
    // 首次迁移真的动了窗口：window_applied + 删行数 > 0
    expect(firstBody.data.window_applied).toBe(true);
    expect(firstBody.data.rows_deleted).toBeGreaterThan(0);
    // shrink 守卫签名：窗口先删干净 ⇒ skipped_shrink 恒 0（50-doc §3.1 坑 4）
    expect(firstBody.data.backfill.skipped_shrink).toBe(0);
    for (const k of ['scanned', 'days_written', 'user_rows_written']) {
      expect(typeof firstBody.data.backfill[k], `backfill.${k} 应为数字`).toBe('number');
    }

    // ── 四条坑提示逐条在场，文案内嵌实际日期与偏移（不许前端自己拼日期）──
    expect(firstBody.notices).toHaveLength(4);
    expect(firstBody.notices[0]).toContain('顺序坑');
    expect(firstBody.notices[0]).toContain('UTC+5');
    expect(firstBody.notices[1]).toContain('分裂窗口');
    expect(firstBody.notices[1]).toContain(firstBody.data.effective_from);
    expect(firstBody.notices[1]).toContain(firstBody.data.window_to);
    expect(firstBody.notices[2]).toContain('切换当日偏低');
    expect(firstBody.notices[2]).toContain('次日自愈');
    expect(firstBody.notices[2]).toContain(firstBody.today);
    expect(firstBody.notices[3]).toContain('先删后算');
    expect(firstBody.notices[3]).toContain('skipped_shrink');

    // ── 第二次调用：幂等短路（标记已等于目标偏移）⇒ 不删不回填 ──
    const second = await call();
    expect(second.status()).toBe(200);
    const secondBody = await second.json();
    expect(secondBody.success).toBe(true);
    // 不重做的签名：window_applied=false、rows_deleted=0、backfill 归零
    expect(secondBody.data.window_applied).toBe(false);
    expect(secondBody.data.rows_deleted).toBe(0);
    expect(secondBody.data.backfill).toEqual({
      scanned: 0,
      days_written: 0,
      user_rows_written: 0,
      skipped_shrink: 0,
    });
    // 响应的固定部分与首次完全一致：notices / tzEffectiveFrom / today / 窗口日期
    expect(secondBody.notices).toEqual(firstBody.notices);
    expect(secondBody.tzEffectiveFrom).toBe(firstBody.tzEffectiveFrom);
    expect(secondBody.today).toBe(firstBody.today);
    expect(secondBody.data.effective_from).toBe(firstBody.data.effective_from);
    expect(secondBody.data.window_to).toBe(firstBody.data.window_to);

    // ── 终态：连调两次 == 单次调用（标记落了、needsMigration 翻 false）──
    const get = await page.request.get(GET_SETTINGS);
    expect((await get.json()).data).toEqual({
      timezoneOffset: 5,
      effectiveOffset: 5,
      storedOffset: 5,
      needsMigration: false,
      minOffset: -12,
      maxOffset: 14,
    });
  });

  test('顺序坑：offset_to ≠ 当前生效口径 ⇒ 报错且零写入（标记不动、快照不动）', async ({ page }) => {
    await page.request.put(GET_SETTINGS, { data: { timezone_offset: 5 } });
    const res = await page.request.post(MIGRATE, { data: { offset_to: 8 } });
    // #10 裁定：client 前置违例返 400（与同类 range 检查 400 / notice 文案一致）。
    // 钉住状态码 = 防「静默回退 500」；error body 形状与文案逐字钉住。
    expect(res.status()).toBe(400);
    expect(await res.json()).toEqual({
      success: false,
      error: 'Agg tz migrate failed: 目标偏移 8 与当前生效口径 5 不一致：请先在报表设置里保存偏移，再调迁移端点',
    });
    // 零写入：标记没落、快照没动
    const get = await page.request.get(GET_SETTINGS);
    expect((await get.json()).data).toMatchObject({
      storedOffset: null,
      effectiveOffset: 5,
      needsMigration: true,
    });
  });

  test('tz-migrate 请求体：空体/别名合法，越界 400，非整数 422', async ({ page }) => {
    // 空体：offset_to 缺省=当前快照(8)、offset_from 缺省=0 ⇒ 合法执行迁移
    const empty = await page.request.post(MIGRATE, { data: {} });
    expect(empty.status()).toBe(200);
    expect((await empty.json()).data.window_applied).toBe(true);

    // camelCase 别名：此时标记已是 8 ⇒ 幂等短路
    const alias = await page.request.post(MIGRATE, { data: { offsetFrom: 0, offsetTo: 8 } });
    expect(alias.status()).toBe(200);
    expect((await alias.json()).data.window_applied).toBe(false);

    // 越界：两条标签逐字钉住（旧口径/新口径，logs.rs:705-718）
    const fromBad = await page.request.post(MIGRATE, { data: { offset_from: 99 } });
    expect(fromBad.status()).toBe(400);
    const fromBody = await fromBad.json();
    expect(fromBody.error).toContain('旧口径 offset_from=99 越界');
    expect(fromBody.error).toContain('必须在 -12~14 之间');
    const toBad = await page.request.post(MIGRATE, { data: { offset_to: -99 } });
    expect(toBad.status()).toBe(400);
    expect((await toBad.json()).error).toContain('新口径 offset_to=-99 越界');

    // 非整数：axum JsonDataError ⇒ 422 text/plain
    const nan = await page.request.post(MIGRATE, { data: { offset_from: 'abc' } });
    expect(nan.status()).toBe(422);
    expect(await nan.text()).toContain('Failed to deserialize the JSON body into the target type');
  });
});
