import { test, expect, loginAsAdmin } from './fixtures';
import type { Page } from '@playwright/test';

/**
 * 渠道「周期余额巡检」读写面 E2E（#32 PR3，前端侧）。
 *
 * 后端契约（对账基线，逐条来自真实产出方，不是桩自己定的。
 * 对账时间 2026-11-09，PR2 已随 `1a51d6f0` 落在 main；下面每个函数名后是 main 上的行号）：
 *   · `mask_channel`（src/api/admin/channels.rs:287-289）恒**显式**序列化三键，不带
 *     `skip_serializing_if`（全文件 0 命中）⇒ `None` 与「这条数据比字段更早」在线上是
 *     同一条 JSON，前端只能画成同一个「未启用」，不许出现「未配置」这种第二种状态；
 *     `balance_last_error` 在**出口**再过一道 `mask_error_text`；
 *   · 三键的定义处是 `crate::channel::Channel`（src/channel/mod.rs:323/326/329，
 *     `#[serde(default)]`、容器级**没有** rename_all ⇒ 线上名 = 字段名逐字）。
 *     ⚠️ 不是 `src/db/entity/channel.rs`——那是 SeaORM 的 `channels` 表模型，压根没这三列；
 *   · PATCH 三态（`apply_interval_patch`，src/api/admin/legacy.rs:1296）：键不存在 = 保持，
 *     显式 `null` 与 `0` 都归一成关闭，非负整数封顶 30 天，其余形状（字符串/负数/小数/
 *     对象）后端**忽略不改动**；
 *   · `balance_last_query_at` / `balance_last_error` 是巡检写回的**运行态**，
 *     不接受 PATCH 写入（写了等于造假，legacy.rs:1408 注释点名）。手动「点击更新余额」走
 *     `handle_get_channel_balance`（legacy.rs:1456）→ `save_balance_with_unit`
 *     （channel/mod.rs:981）：只写 balance/credit/unit，**不碰** `balance_last_query_at`；
 *     写时间戳的只有巡检侧 `save_balance_refresh`（channel/mod.rs:1037）。所以口径上
 *     「上次查询」只报周期任务，不报手动点击 —— 由下面第一条用例的 tooltip 钉住；
 *   · `is_due`（src/channel/balance_refresh.rs:34）用 `.filter(|v| *v > 0)`，同文件
 *     L170 有「显式 0 = 关闭」的单测 ⇒ `Some(0)` 是**真能到前端**的形状（手写
 *     config.toml / 旧版持久化数据），前端必须跟巡检侧同口径收成「未启用」。
 *
 * 关于「PUT 保留三键」那条断言能抓什么、抓不到什么，说清楚别夸大：
 * 桩是 mock，抓不到后端自己把 `preserve_non_form_fields` 改回去（那由该函数的
 * Rust 单测钉）。它能抓的是前端这两件事——
 *   ① 用「每次保存都无条件补一刀 PATCH」去掩盖后端缺陷（lead 2026-11-08 明令禁止的形状）；
 *   ② 把间隔塞进 PUT body 假装写进去了（`ChannelRequest` 压根没这字段，serde 静默丢）。
 * 前提是按 #31 的纪律把桩与真实产出方保持同步；桩漂了这条就跟着漂，注释里注明了
 * 形状来源就是为了让下一次对账能发现漂移。
 *
 * ⚠️ 写路径用例（2–5）跑在 `useChannelStore` 造的**每用例私有副本**上，不改
 * static-server 的共享 `CHANNELS`：那个进程被全部 spec 共用（desktop/mobile 两轮串跑，
 * 本地还 `reuseExistingServer`），写坏一条会把后续用例的显示态带偏
 * （proxies.spec.ts 踩过同一个坑，它的解法是复位接口）。副本里 PATCH 真的落值，
 * 所以「保存后重新 GET 的列表里间隔变了」这条仍然测得到，且不留残留。
 */

type ChannelRow = Record<string, unknown> & { id: string };

/**
 * 列表行按「序号列」定位，不按名字：桩里 ch-1 与 ch-9 **同名** `lxchat-glm`
 * （ch-9 是 account 型池），按名字 + `.first()` 等于赌 DOM 顺序。
 * 序号列渲染的是 `ch.seq`（见 Channels.tsx 的 col-id 单元格）。
 */
const rowOf = (page: Page, seq: number) =>
  page
    .locator('tbody tr')
    .filter({ has: page.locator('td.col-id', { hasText: new RegExp(`^${seq}$`) }) });

/** 桩里四条样本的序号（见 static-server.mjs 的 CHANNELS） */
const SEQ = {
  /** lxchat-glm：定频 21600（每 6 小时），有上次读数、无失败 */
  on: 1,
  /** cf-free-gateway：三键显式 null（关闭） */
  off: 2,
  /** anthropic-relay：定频 86400 + 脱敏后的巡检失败文案 */
  error: 3,
  /** legacy-zero-interval：显式 0，但 balance_last_query_at 非空 */
  zero: 4,
} as const;

/** 余额格右侧的巡检徽标（独立于「点击更新余额」那个 button，见 Channels.tsx） */
const badgeOf = (page: Page, seq: number) =>
  rowOf(page, seq).locator('[data-testid="balance-refresh"]');

/** 徽标 tooltip（口径全在 title 上，徽标本体只有图标） */
const titleOf = async (page: Page, seq: number): Promise<string> =>
  (await badgeOf(page, seq).getAttribute('title')) ?? '';

/**
 * 打开编辑弹窗：行菜单「更多操作」→「编辑供应商」。
 * 菜单面板是挂在 body 层级的 `.ch-row-menu-panel`（逃 table 的 overflow 裁剪），
 * 项就是面板里的裸 button，没有专用 class。
 *
 * 为什么用 `toPass` 收敛而不是「点一次就假定面板在」：`Channels.tsx` 的行菜单
 * 注册了 `window.addEventListener('scroll', close, true)`（fixed 坐标不随滚动联动，
 * 一滚就关），capture=true ⇒ **任何**元素的滚动都算，包括 Playwright 为了定位
 * 单元格自己滚的那一下。实测：只点一次触发键时，ch-2 这条 30s 等不到面板
 * （面板确实不在 DOM 里，不是不可见），而 ch-1 三条用例同写法能过 —— 差别就在
 * 谁先被滚动。这里不赌那一下：面板不在就再点一次，直到出现。
 * （没继续往下钉「具体是哪一次滚动」：能确定的是 app 的关闭条件 + 面板缺席，
 *  再往下要读 Playwright 的 actionability 内部时序，收益不匹配。）
 */
async function openEditModal(page: Page, seq: number): Promise<void> {
  const trig = rowOf(page, seq).locator('.ch-row-menu button');
  const editItem = page.locator('.ch-row-menu-panel button', { hasText: '编辑供应商' });
  await expect(async () => {
    if ((await editItem.count()) === 0) await trig.click();
    // 断言放在回调里：toPass 靠「回调抛错」才重试，只点不判等于只跑一轮。
    // 内层超时压到 800ms：默认 5s 会让「第一轮没面板」白等 5s（实测整条用例 8s），
    // 面板要么这轮就出现、要么下一轮重点一次，不需要在这里等满
    await expect(editItem).toHaveCount(1, { timeout: 800 });
  }).toPass({ timeout: 10_000 });
  await editItem.click();
  // 巡检下拉在「高级配置」details 里，默认收起：不收就没有可见的 select。
  // 必须用**直接子代** `> summary`：高级配置里还套了一层「上游特判」details，
  // `details.ch-advanced` + `.locator('summary')` 是后代匹配，会同时命中嵌套那条
  // （实测 strict mode violation：2 elements）。
  const advanced = page.locator('details.ch-advanced');
  const advancedSummary = page.locator('details.ch-advanced > summary');
  await expect(advancedSummary).toBeVisible();
  if (!(await advanced.evaluate((el: HTMLElement) => (el as HTMLDetailsElement).open))) {
    await advancedSummary.click();
  }
  await expect(page.getByLabel('余额自动查询', { exact: true })).toBeVisible();
}

/**
 * 每用例私有的渠道 store：`page.route` 拦在浏览器侧，语义照后端。
 * 返回 PUT / PATCH 的 body 收集数组（route 处理函数跑在 Node 侧，测试直接读）。
 */
async function useChannelStore(
  page: Page,
  overrides: Record<string, Partial<ChannelRow>> = {},
): Promise<{
  puts: Array<Record<string, unknown>>;
  patches: Array<Record<string, unknown>>;
}> {
  const puts: Array<Record<string, unknown>> = [];
  const patches: Array<Record<string, unknown>> = [];
  let rows: ChannelRow[] | null = null;

  await page.route('**/api/channels**', async (route) => {
    const req = route.request();
    const path = new URL(req.url()).pathname;
    const method = req.method();

    // 列表：首枪从真桩取形状（这样桩里漂了的字段会一起漂进本用例，暴露而不是掩盖），
    // 之后所有 GET 都读副本 —— 保存后的回读测的就是「写进去了没有」
    if (path === '/api/channels' && method === 'GET') {
      if (!rows) {
        const res = await route.fetch();
        const json = await res.json();
        rows = (json.data as ChannelRow[]).map((c) => ({ ...c, ...(overrides[c.id] ?? {}) }));
      }
      return route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify({
          success: true,
          data: rows,
          total: rows.length,
          page: 1,
          size: rows.length,
        }),
      });
    }

    const hit = /^\/api\/channels\/([^/]+)$/.exec(path);
    if (hit && (method === 'PUT' || method === 'PATCH')) {
      const row = rows?.find((c) => c.id === hit[1]);
      if (!row) {
        return route.fulfill({
          status: 404,
          contentType: 'application/json',
          body: JSON.stringify({ success: false, error: { message: 'Channel not found' } }),
        });
      }
      const body = (req.postDataJSON() ?? {}) as Record<string, unknown>;
      if (method === 'PUT') {
        puts.push(body);
        // `preserve_non_form_fields` 口径：三键从已存行搬回来，body 带不带都不生效。
        // 只回显、不把 body 落进副本：本用例只关心巡检间隔的落地态，而表单形状的
        // quirks/balance_fields 灌回列表渲染会把行画坏（那是另一种假红）。
        return route.fulfill({
          status: 200,
          contentType: 'application/json',
          body: JSON.stringify({
            success: true,
            data: {
              ...row,
              ...body,
              balance_query_interval_secs: row.balance_query_interval_secs ?? null,
              balance_last_query_at: row.balance_last_query_at ?? null,
              balance_last_error: row.balance_last_error ?? null,
            },
          }),
        });
      }
      // PATCH：`apply_interval_patch` 三态（src/api/admin/legacy.rs:1296，`1a51d6f0` 已在 main）
      // —— 键不存在 = 保持；显式 null 或 0 = 关闭；非负整数封顶 30 天；
      // 其余形状（字符串/负数/小数/对象）后端**忽略不改动**，不能收成关闭。
      // 这一条**会**落到副本上（写路径要能被回读测出来）
      patches.push(body);
      if (Object.prototype.hasOwnProperty.call(body, 'balance_query_interval_secs')) {
        const raw = body.balance_query_interval_secs;
        if (raw === null) {
          row.balance_query_interval_secs = null;
        } else if (Number.isInteger(raw) && raw >= 0) {
          row.balance_query_interval_secs = raw === 0 ? null : Math.min(raw, 30 * 86400);
        }
      }
      return route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify({ success: true, data: row }),
      });
    }

    // /channels/:id/test、/:id/balance、/:id/reset-circuit、/fetch_models、/chat_test…
    return route.continue();
  });

  // beforeEach 的 goto 已经发过列表请求：重取一次才让上面的拦截生效
  await page.reload();
  // reload 只等到 load 事件，表格是之后才渲染的。先等首行出现再交还给用例：
  // 在「刚渲染 / 还在渲染」的窗口里点行菜单，正好踩上面那条滚动关菜单的坑
  await expect(rowOf(page, SEQ.on)).toBeVisible();
  return { puts, patches };
}

/**
 * 点提交，并等「PUT 响应 + 保存后那一发列表 GET」都落地。
 *
 * 为什么非要等这发 GET：`handleSave` 是 PUT → `await` PATCH → closeModal →
 * loadChannels 串起来的，GET 出现即代表「该发的 PATCH 要么发完了、要么决定不发」。
 * 不等就断言 `patches` 为空是抢跑式假绿（PATCH 在路上，测不到）。
 */
async function saveModal(page: Page) {
  const putResp = page.waitForResponse((r) => r.request().method() === 'PUT');
  const refetch = page.waitForResponse(
    (r) => r.request().method() === 'GET' && new URL(r.url()).pathname === '/api/channels',
  );
  await page.locator('.modal-footer button[type="submit"]').click();
  const res = await putResp;
  await refetch;
  return res;
}

test.describe('渠道周期余额巡检（#32 PR3）', () => {
  test.beforeEach(async ({ page }) => {
    await loginAsAdmin(page);
    await page.goto('/channels');
    await expect(rowOf(page, SEQ.on)).toBeVisible();
  });

  test('三态显示：null / 显式 0 / 定频 各画一种，关闭不叫「未配置」也不画 1970', async ({ page, pageErrors }) => {
    // ── 桩自身先对账（#31 的 mock 契约纪律）：三键必须**显式在场**，关是 null 不是省键。
    // 省键的话，前端「把 undefined 与 null 分两种画法」的偏差永远测不出来。
    const list = await (await page.request.get('/api/channels')).json();
    const stub = (list.data as ChannelRow[]).filter((c) => ['ch-1', 'ch-2', 'ch-3', 'ch-4'].includes(c.id));
    expect(stub, '桩里四条样本都在').toHaveLength(4);
    for (const c of stub) {
      expect(c, `${c.id} 缺 balance_query_interval_secs 键`).toHaveProperty('balance_query_interval_secs');
      expect(c, `${c.id} 缺 balance_last_query_at 键`).toHaveProperty('balance_last_query_at');
      expect(c, `${c.id} 缺 balance_last_error 键`).toHaveProperty('balance_last_error');
    }
    expect(stub.find((c) => c.id === 'ch-2')!.balance_query_interval_secs).toBeNull();

    // ── 定频档：ch-1 = 21600 秒 ──
    const onBadge = badgeOf(page, SEQ.on);
    await expect(onBadge).toHaveAttribute('data-state', 'ok');
    await expect(onBadge).toHaveAttribute('data-interval', '21600');
    const onTitle = await titleOf(page, SEQ.on);
    expect(onTitle).toContain('每 6 小时');
    // 「上次查询」这条只报自动巡检，并明示它由周期任务写（手动刷新不记这笔）
    expect(onTitle).toContain('上次查询');
    expect(onTitle).toContain('由周期任务更新');

    // ── 显式 null：ch-2 = 关闭 ──
    const offBadge = badgeOf(page, SEQ.off);
    await expect(offBadge).toHaveAttribute('data-state', 'off');
    const offTitle = await titleOf(page, SEQ.off);
    expect(offTitle).toContain('未启用');
    // lead 裁定：三键显式 null 与旧数据同态 ⇒ 界面上不许有第二种说法
    expect(offTitle).not.toContain('未配置');

    // ── 显式 0：ch-4 = Some(0)，巡检侧 is_due 已按关闭处理，界面必须同口径 ──
    const zeroBadge = badgeOf(page, SEQ.zero);
    await expect(zeroBadge).toHaveAttribute('data-state', 'off');
    const zeroTitle = await titleOf(page, SEQ.zero);
    expect(zeroTitle).toContain('未启用');
    // 「每 0 秒」是把 0 当定频读出来的假信息；
    // ch-4 故意留了非空 balance_last_query_at：关闭态还念旧读数就是自相矛盾，
    // 所以这条时间戳在关闭态的 tooltip 里一个字都不该出现（也就画不出 1970）
    expect(zeroTitle).not.toMatch(/每 0|0 秒/);
    expect(zeroTitle).not.toContain('上次查询');

    // ── 失败态：ch-3 = 定频 + balance_last_error ──
    await expect(badgeOf(page, SEQ.error)).toHaveAttribute('data-state', 'error');
    const errTitle = await titleOf(page, SEQ.error);
    expect(errTitle).toContain('每天');
    // 失败文案是后端 mask_error_text 处理后的形状，前端原样透出（不二次截断、不猜意思）
    expect(errTitle).toContain('上游返回 HTTP 401');
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('未改间隔保存：PUT 不带该键、也不补 PATCH（禁止前端掩盖后端缺陷）', async ({ page }) => {
    const { puts, patches } = await useChannelStore(page);
    await openEditModal(page, SEQ.on);

    // 回填正确性先钉住：库里 21600，下拉必须选到「每 6 小时」，
    // 不能显示成关闭/空值——那样任何一次保存都会看起来像「没动过但被改了」
    await expect(page.getByLabel('余额自动查询', { exact: true })).toHaveValue('21600');

    // 只改名称，间隔一个字没碰
    await page.getByLabel('名称').fill('lxchat-glm-renamed');
    const putRes = await saveModal(page);

    expect(puts, JSON.stringify(puts)).toHaveLength(1);
    // ① PUT body 不含间隔：`ChannelRequest` 没这字段，带上只是被 serde 静默丢弃，
    //    写在 payload 里是给下一个人埋「PUT 能改它」的坑
    expect(puts[0]).not.toHaveProperty('balance_query_interval_secs');
    // ② 值没变 ⇒ 一条 PATCH 都不发。改成「每次保存都补一刀 PATCH」会掩盖
    //    PUT 丢字段的缺陷（lead 2026-11-08 明令），这条就是它的哨兵
    expect(patches, JSON.stringify(patches)).toHaveLength(0);

    // ③ PUT 响应仍是原间隔（`preserve_non_form_fields` 的口径：一次普通保存
    //    不该把巡检关掉）。前端读的是保存后重新 GET 的列表，所以这里同时钉
    //    「保存后徽标还在、还是 21600」——行仍按序号认，桩没把改名落地
    const body = await putRes.json();
    expect(body.data.balance_query_interval_secs).toBe(21600);
    await expect(badgeOf(page, SEQ.on)).toHaveAttribute('data-interval', '21600');
    await expect(badgeOf(page, SEQ.on)).toHaveAttribute('data-state', 'ok');
  });

  test('改间隔保存：PATCH 唯一写路径，body 只有那一键；下拉选到「每小时」就写 3600', async ({ page }) => {
    const { puts, patches } = await useChannelStore(page);
    // ch-2 是显式 null（关闭）：从关闭改到定频
    await openEditModal(page, SEQ.off);
    await expect(page.getByLabel('余额自动查询', { exact: true })).toHaveValue('0');

    await page.getByLabel('余额自动查询', { exact: true }).selectOption('3600');
    await saveModal(page);

    // await 链已走完（PATCH 先于保存后那发 GET），所以这里直接判条数，不用 poll
    expect(patches, JSON.stringify(patches)).toHaveLength(1);
    // body 精确一条键：不许顺手把别的字段也 PATCH 出去（那会撞运行态两键的
    // 「写了等于造假」红线），也不许把间隔做成 {interval: '3600'} 这种字符串形状
    expect(patches[0]).toEqual({ balance_query_interval_secs: 3600 });
    expect(puts, JSON.stringify(puts)).toHaveLength(1);
    expect(puts[0]).not.toHaveProperty('balance_query_interval_secs');

    // 保存后列表回读：徽标从关闭档切到定频档（写路径真的通了，不只是发了请求）
    await expect(badgeOf(page, SEQ.off)).toHaveAttribute('data-interval', '3600');
    await expect(badgeOf(page, SEQ.off)).toHaveAttribute('data-state', 'ok');
  });

  test('关掉巡检：下拉选回「未启用」发数字 0，后端归一成 None 与显式 null 同态', async ({ page }) => {
    const { patches } = await useChannelStore(page);
    await openEditModal(page, SEQ.on);
    await page.getByLabel('余额自动查询', { exact: true }).selectOption('0');
    await saveModal(page);

    expect(patches, JSON.stringify(patches)).toHaveLength(1);
    // 发 0 而不是 null：后端 `apply_interval_patch` 里 0 与显式 null 都归一成 None，
    // 而账号池页同名配置也是发数字 0（两处形状一致，避免同一语义两种写法）
    expect(patches[0]).toEqual({ balance_query_interval_secs: 0 });
    // 回读：store 按后端语义把 0 归一成 null ⇒ 界面仍是「未启用」，不是「每 0 秒」
    await expect(badgeOf(page, SEQ.on)).toHaveAttribute('data-state', 'off');
    await expect(badgeOf(page, SEQ.on)).toHaveAttribute('data-interval', '0');
    expect(await titleOf(page, SEQ.on)).not.toMatch(/每 0|0 秒/);
  });

  test('档位外的存量值（7200）原样回填并显示秒数，不就近取整成档位', async ({ page }) => {
    // 手写 config.toml / 旧版持久化数据能给到 Some(7200)：既不在档位里、又是真实在跑的间隔。
    // 就近取整到「每 6 小时」会把「后端实际按 7200 秒查」显示成假信息；
    // 显示成空白则会让人以为没配，随手一次保存就把它改写掉。
    const { puts, patches } = await useChannelStore(page, {
      'ch-1': { balance_query_interval_secs: 7200 },
    });

    await expect(badgeOf(page, SEQ.on)).toHaveAttribute('data-interval', '7200');
    const title = await titleOf(page, SEQ.on);
    expect(title).toContain('7200');
    expect(title).not.toContain('每 6 小时');

    await openEditModal(page, SEQ.on);
    // 回填成档位外的那条自定义 option，而不是回落到 '0'
    await expect(page.getByLabel('余额自动查询', { exact: true })).toHaveValue('7200');

    // 原样保存：值没变 ⇒ 不该有任何 PATCH（同第一条用例的口径），PUT body 也不带该键
    await saveModal(page);
    expect(puts, JSON.stringify(puts)).toHaveLength(1);
    expect(puts[0]).not.toHaveProperty('balance_query_interval_secs');
    expect(patches, JSON.stringify(patches)).toHaveLength(0);
  });

  test('桩自身的写分支：可达 + 三态与后端逐臂对齐 + 只回显不改写共享样本', async ({ page }) => {
    // 这条**钉的是桩不是产品**：用例 2–5 的写路径都跑在 useChannelStore 的浏览器侧副本上，
    // 一条请求都不进 static-server，于是桩里那两个写端点天然是「没有用例经过」的分支——
    // 第一轮就踩到了：id 取成 `p.split('/')[2]`（那是 `channels` 段而不是渠道 id），
    // 两条分支恒 404，而全套 e2e 依旧全绿。桩漂了 ⇒ 2–5 的期望就跟着漂，
    // 所以这里用 `page.request`（不经 page.route 拦截，直打桩）把臂逐个钉住。
    const patchInterval = async (value: unknown): Promise<unknown> => {
      const res = await page.request.patch('/api/channels/ch-1', {
        data: { balance_query_interval_secs: value },
      });
      expect(res.ok(), `PATCH ${JSON.stringify(value)} → ${res.status()}`).toBeTruthy();
      return (await res.json()).data.balance_query_interval_secs;
    };

    // 桩是**无状态**的：每次 PATCH 都从共享样本 ch-1 的原值 21600 起算，上一发的结果
    // 不会留给下一发（这正是"只回显不改写"的另一面）。所以下面每一发的期望值都是
    // 相对 21600 算的，不是相对上一发——真后端的 PATCH 会落库，那是 useChannelStore
    // 的浏览器侧副本负责模拟的形状（用例 2–5 钉的就是那条回读）。
    // 正数 = 定频
    expect(await patchInterval(3600)).toBe(3600);
    // 0 与显式 null 都归一成关闭
    expect(await patchInterval(0)).toBeNull();
    expect(await patchInterval(null)).toBeNull();
    // 封顶 30 天
    expect(await patchInterval(30 * 86_400 + 999)).toBe(30 * 86_400);
    // 其余形状：后端 `as_u64` 取不到值就**不改** ⇒ 保持桩里的 21600。桩若把它收成关闭，
    // 等于把「一次没生效的 PATCH」教成「关掉了」——下一个人照着写就错（字符串/负数/小数 三臂）
    expect(await patchInterval('3600')).toBe(21600);
    expect(await patchInterval(-5)).toBe(21600);
    expect(await patchInterval(3600.5)).toBe(21600);
    // 键不存在 = 保持
    const noKey = await page.request.patch('/api/channels/ch-1', { data: { status: 'enabled' } });
    expect(
      (await noKey.json()).data.balance_query_interval_secs,
      'PATCH 不带该键却改掉了间隔',
    ).toBe(21600);
    expect((await noKey.json()).data.status).toBe('enabled');

    // PUT 分支：body 想把三键抹掉也抹不动（`preserve_non_form_fields` 口径）
    const put = await page.request.put('/api/channels/ch-3', {
      data: { name: 'renamed-by-stub-test', balance_query_interval_secs: null, balance_last_error: '***' },
    });
    expect(put.ok()).toBeTruthy();
    const putData = (await put.json()).data;
    expect(putData.balance_query_interval_secs).toBe(86400);
    expect(putData.balance_last_error).toContain('HTTP 401');

    // 最后一条是**污染哨兵**：上面这些写全都只回显，共享样本必须一个字节没变
    const list = await (await page.request.get('/api/channels')).json();
    const stored = (list.data as ChannelRow[]).find((c) => c.id === 'ch-1');
    expect(stored!.balance_query_interval_secs, '桩把共享 CHANNELS 写坏了').toBe(21600);
    expect((list.data as ChannelRow[]).find((c) => c.id === 'ch-3')!.name).not.toBe('renamed-by-stub-test');
  });
});
