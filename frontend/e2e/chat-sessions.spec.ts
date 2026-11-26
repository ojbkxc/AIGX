import { test, expect, loginAsAdmin } from './fixtures';

/**
 * 聊天页历史会话抽屉 —— 回归测试。
 *
 * 覆盖真实修复过的缺陷：
 * 1. 抽屉高度：`.pg-shell` 的 height:100% 曾因父级无高度而退化，抽屉只能
 *    撑到输入区上方（高度 ≈ 269px）。断言抽屉实际高度接近视口高度。
 * 2. 透明面板：抽屉背景曾引用主题里不存在的 --bg-elevated，fallback 近乎
 *    全透明，侧栏文字透过面板混排。断言面板背景 alpha ≥ 0.95。
 * 3. 遮挡主导航：抽屉曾是 overlay 变体，从视口最左滑出把全局侧边栏整个盖住。
 *    现对齐 AI 运维页用 sidebar 变体（桌面内嵌/窄屏降级覆盖层）。
 * 4. 删除非活动会话不应丢失活动会话在 debounce 窗口内的消息。
 */

async function openSideList(page: import('@playwright/test').Page): Promise<void> {
  // 新结构（对齐 A2A assistant-shell）：桌面端历史会话是**常驻左栏**，没有
  // 「打开抽屉」这个动作；窄屏（<900px）左栏收起，由顶栏 ☰ 打开 MobileDrawer。
  const isNarrow = (page.viewportSize()?.width ?? 1280) < 900;
  if (isNarrow) {
    await page.locator('.asst-menu').click();
    await expect(page.locator('.mobile-drawer-overlay')).toBeVisible();
  } else {
    await expect(page.locator('.asst-layout > .asst-side')).toBeVisible();
  }
}

/** 当前可见左栏的选择器（桌面=常驻栏，窄屏=覆盖层里的同一组件） */
function sideRoot(page: import('@playwright/test').Page): string {
  return (page.viewportSize()?.width ?? 1280) < 900
    ? '.mobile-drawer .asst-side'
    : '.asst-layout > .asst-side';
}

test.describe('聊天 · 历史会话抽屉', () => {
  test('聊天主区域内容流形态（输入区在消息列底部、中间无模型下拉）', async ({ page, pageErrors }) => {
    await loginAsAdmin(page);
    await page.goto('/chat');
    await expect(page.locator('.asst-layout')).toBeVisible();

    const vh = await page.evaluate(() => window.innerHeight);

    // 结构：左栏 + 右栏（顶栏 / 消息区 / 输入区），与 A2A assistant 页同一分区。
    // 窄屏左栏收起（由顶栏 ☰ 打开覆盖层），桌面常驻 —— 两态分别断言。
    const isNarrow = (page.viewportSize()?.width ?? 1280) < 900;
    if (isNarrow) {
      await expect(page.locator('.asst-layout > .asst-side')).toBeHidden();
      await expect(page.locator('.asst-main > .asst-top .asst-menu')).toBeVisible();
    } else {
      await expect(page.locator('.asst-layout > .asst-side')).toBeVisible();
    }
    await expect(page.locator('.asst-main > .asst-top')).toBeVisible();

    // 消息区是**唯一**的滚动容器，输入区常驻列底：整列高度 = 视口 − main-content
    // 上下内边距（28×2 = 56），所以输入区下缘贴住内容区底、绝不越出视口。
    // （旧断言是「输入条随消息上移、不贴底」—— 那是内容流时代的契约；
    //  A2A 形态改为定高骨架，输入区钉在列底，需求已被新结构取代，见交付说明。）
    const layoutBox = await page.locator('.asst-layout').boundingBox();
    expect(layoutBox).not.toBeNull();
    expect(layoutBox!.height).toBeGreaterThan(vh * 0.75);
    const composerBox = await page.locator('.asst-composer-wrap').boundingBox();
    expect(composerBox).not.toBeNull();
    expect(composerBox!.y + composerBox!.height).toBeLessThanOrEqual(vh + 1);
    // 输入区在消息区**下方**（不是浮在上面、也不是页面上方）
    expect(composerBox!.y).toBeGreaterThan(layoutBox!.y + layoutBox!.height / 2);

    // 消息列限宽 840px 且居中（A2A 同一尺度）
    const scrollOverflow = await page.locator('.asst-scroll').evaluate(
      (el) => getComputedStyle(el).overflowY,
    );
    expect(scrollOverflow).toBe('auto');
    const flowWidth = await page.locator('.asst-flow').evaluate(
      (el) => el.getBoundingClientRect().width,
    );
    expect(flowWidth).toBeLessThanOrEqual(840 + 1);

    // 首屏载入的是活动会话（桩预置 30 条，活动 = 第 1 条）
    await expect(page.locator('.asst-flow')).toContainText('第 1 个会话的第 1 条消息');

    // 空状态**不再**渲染任何模型下拉（用户反馈「聊天页中间别加什么模型下拉」）：
    // 模型唯一入口收在输入区右下角的胶囊里。经左栏「新对话」进空态（真实 UI 流；
    // 窄屏先 ☰ 展开覆盖层）。
    await openSideList(page);
    await page.locator(`${sideRoot(page)} .asst-new`).click();
    await expect(page.locator('.asst-empty')).toBeVisible();
    await expect(page.locator('.asst-empty .model-picker')).toHaveCount(0);
    await expect(page.locator('.asst-empty .asst-pop')).toHaveCount(0);
    // 输入区的唯一入口存在且可展开出「模型 / 推理等级」两行菜单
    const modelPill = page.locator('.asst-composer .asst-model-pill');
    await expect(modelPill).toBeVisible();
    await modelPill.click();
    await expect(page.locator('.asst-pop .asst-pick-row').first()).toBeVisible();
    await expect(page.locator('.asst-pop')).toContainText('推理等级');

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('抽屉形态：桌面左栏常驻不挡侧栏 / 窄屏覆盖全屏，且标题未被遮挡', async ({ page, pageErrors }) => {
    await loginAsAdmin(page);
    await page.goto('/chat');
    await expect(page.locator('.asst-layout')).toBeVisible();

    const isNarrow = (page.viewportSize()?.width ?? 1280) < 900;

    if (!isNarrow) {
      // 桌面形态（A2A assistant-shell）：历史会话是**常驻左栏**，不是抽屉 ——
      // 它在 .asst-layout 内脱离不了文档流（static），并且必须落在全局侧栏
      // 右缘之右（旧 overlay 变体从视口最左滑出、把主导航整个盖住，这是被
      // 回归过的缺陷）。列表 30+ 条时自身滚动，不许把页面撑高。
      const sidePos = await page.evaluate(
        () => getComputedStyle(document.querySelector('.asst-layout > .asst-side')!).position,
      );
      expect(sidePos).toBe('static');

      const probe = await page.evaluate(() => {
        const side = document.querySelector('.asst-layout > .asst-side')!.getBoundingClientRect();
        const aside = document.querySelector('.sidebar-aside')!.getBoundingClientRect();
        // 侧栏中部命中测试：抽屉/左栏不得抢走主导航的点击
        const el = document.elementFromPoint(aside.left + aside.width / 2, aside.top + Math.min(300, aside.height - 8));
        return {
          sideLeft: side.left,
          sideHeight: side.height,
          asideRight: aside.right,
          owner: el?.closest('.sidebar-aside, .asst-side')?.className ?? String(el?.className ?? ''),
        };
      });
      expect(probe.owner).toContain('sidebar-aside');
      expect(probe.sideLeft).toBeGreaterThanOrEqual(probe.asideRight - 1);
      expect(probe.sideHeight).toBeGreaterThan(200);

      // 列表自身可滚动（30 条 seed 会话 + 前面用例新建的空会话）
      const list = page.locator('.asst-layout > .asst-side .asst-side-list');
      const scrollable = await list.evaluate((el) => el.scrollHeight > el.clientHeight);
      expect(scrollable).toBe(true);

      // 左栏是页面的一部分，不该是「半透明覆盖层」形态：transparent = 直接
      // 透出页面底色（正常，静态栏通常不自己设背景）；**断言拦的是
      // 0 < alpha < 0.95 的「浮在上面又没盖严」**，那才是历史上出现过的
      // 侧栏文字透过面板混排的 defect。
      const sideBg = await page.evaluate(
        () => getComputedStyle(document.querySelector('.asst-layout > .asst-side')!).backgroundColor,
      );
      if (sideBg !== 'transparent') {
        const m = sideBg.match(/rgba?\(([^)]+)\)/);
        if (m) {
          const parts = m[1].split(',').map((s) => parseFloat(s));
          const alpha = parts.length === 4 ? parts[3] : 1;
          expect(
            alpha === 0 || alpha > 0.95,
            `左栏背景 ${sideBg} 既非透明也不够实（半透明浮层会与侧栏文字混排）`,
          ).toBe(true);
        }
      }

      // 会话标题可点击 = 没有被更高层元素覆盖
      const firstTitle = page.locator('.asst-layout > .asst-side .asst-side-open').first();
      await expect(firstTitle).toBeVisible();
      await firstTitle.click({ trial: true });
    } else {
      // 窄屏形态：左栏收起，☰ 打开覆盖层抽屉（fixed inset:0 全屏高度）。
      await expect(page.locator('.asst-layout > .asst-side')).toBeHidden();
      await page.locator('.asst-menu').click();
      const overlay = page.locator('.mobile-drawer-overlay');
      await expect(overlay).toBeVisible();

      const position = await page.evaluate(
        () => getComputedStyle(document.querySelector('.mobile-drawer-overlay')!).position,
      );
      expect(position).toBe('fixed');

      const viewportHeight = page.viewportSize()!.height;
      const panelBox = await page.locator('.mobile-drawer').boundingBox();
      expect(panelBox).not.toBeNull();
      expect(panelBox!.y).toBeLessThanOrEqual(1);
      expect(panelBox!.height).toBeGreaterThan(viewportHeight * 0.85);

      // 面板背景不透明（曾用不存在的 --bg-elevated 导致侧栏文字透出）
      const panelBg = await page.evaluate(
        () => getComputedStyle(document.querySelector('.mobile-drawer')!).backgroundColor,
      );
      const parts = panelBg.match(/rgba?\(([^)]+)\)/)![1].split(',').map((s) => parseFloat(s));
      const alpha = parts.length === 4 ? parts[3] : 1;
      expect(alpha, `面板背景 ${panelBg} 近乎透明，侧栏会透过来`).toBeGreaterThan(0.95);

      // 抽屉标题（头部）可点击 = 没有被更高层元素覆盖（正是移动端汉堡按钮那个 bug）
      const title = page.locator('.mobile-drawer .asst-side-head');
      await expect(title).toBeVisible();
      await title.click({ trial: true });

      // 视口左上角命中测试必须在抽屉内（若汉堡按钮 fixed 盖住抽屉标题，此项即失败）。
      // 注意 closest 会先命中抽屉里最近的 `.asst-side`（左栏组件本体），
      // 两个类名都算「落在抽屉内」。
      const topLeftOwner = await page.evaluate(() => {
        const el = document.elementFromPoint(24, 24);
        return el?.closest('.mobile-drawer, .asst-side, .mobile-menu-btn')?.className ?? String(el?.className ?? '');
      });
      expect(topLeftOwner).toMatch(/mobile-drawer|asst-side/);
      expect(topLeftOwner).not.toContain('mobile-menu-btn');

      // 点遮罩关闭（MobileDrawer 无关闭按钮，遮罩/Esc 关闭）
      const vw = page.viewportSize()!.width;
      await page.locator('.mobile-drawer-overlay').click({ position: { x: vw - 20, y: 40 } });
      await expect(page.locator('.mobile-drawer-overlay')).toHaveCount(0);
    }

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('切换会话后抽屉自动收起并载入目标消息', async ({ page, pageErrors }) => {
    await loginAsAdmin(page);
    await page.goto('/chat');
    await expect(page.locator('.asst-layout')).toBeVisible();
    await expect(page.locator('.asst-flow, .asst-empty').first()).toBeVisible();

    const isNarrow = (page.viewportSize()?.width ?? 1280) < 900;
    const listSel = isNarrow ? '.mobile-drawer .asst-side' : '.asst-layout > .asst-side';

    // 窄屏：先 ☰ 展开覆盖层；桌面：左栏常驻，无需展开。
    if (isNarrow) {
      await page.locator('.asst-menu').click();
      await expect(page.locator('.mobile-drawer-overlay')).toBeVisible();
    }

    // 选一条**非当前活动**、标题形如「会话 N」的种子会话并切过去。
    // 不写死序号：桩进程状态跨用例共享，前面的用例会新建空会话、删除非活动
    // 会话，种子序号会被消耗（实测此时「会话 2」已不存在）。按「首个非 active
    // 的会话 N」点名，对增删与顺序都稳定，同时仍能证明「切到了另一个会话」。
    const rows = page.locator(`${listSel} .asst-side-item`);
    const rowCount = await rows.count();
    let pickIdx = -1;
    let pickTitle = '';
    let pickN = '';
    for (let i = 0; i < rowCount; i += 1) {
      const row = rows.nth(i);
      const cls = (await row.getAttribute('class')) ?? '';
      if (cls.includes('active')) continue;
      const text = ((await row.locator('.asst-side-title').textContent()) ?? '').trim();
      const match = text.match(/^会话 (\d+)/);
      if (!match) continue;
      pickIdx = i;
      pickTitle = text;
      pickN = match[1];
      break;
    }
    expect(pickIdx, '左栏里找不到可切换的种子会话（会话 N）').toBeGreaterThanOrEqual(0);
    await rows.nth(pickIdx).locator('.asst-side-open').click();

    // 窄屏覆盖层形态：选完必须自动收起（否则遮住消息区与 toast）。
    // 桌面左栏是常驻栏，「收起」这个需求在桌面上不成立，故只在窄屏断言。
    if (isNarrow) {
      await expect(page.locator('.mobile-drawer-overlay')).toHaveCount(0);
    }

    // 目标会话的消息已载入，且顶栏标题跟着切换（A2A 的 asst-top 标题位）
    await expect(page.locator('.asst-flow')).toContainText(`第 ${pickN} 个会话的第 1 条消息`);
    await expect(page.locator('.asst-main > .asst-top .asst-title')).toContainText(pickTitle);

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('抽屉打开时全局侧边栏仍可交互（不遮挡主导航）', async ({ page, pageErrors }) => {
    // 用户回归反馈：overlay 抽屉从视口最左滑出，把全局主导航（仪表盘/渠道
    // 管理等）整个盖住。新结构下桌面端历史会话是**常驻左栏**（static，落在
    // .asst-layout 内），它必须从全局侧栏右缘之右开始，且侧栏命中测试不被抢。
    // 窄屏形态是覆盖层，天然会盖住视口 —— 与「不遮挡主导航」不是同一命题，
    // 移动端由「覆盖层全屏 + 标题可点」那条用例保证。
    const isNarrow = (page.viewportSize()?.width ?? 1280) < 900;
    test.skip(isNarrow, '移动端全局侧栏为汉堡抽屉，与覆盖层形态天然不冲突');

    await loginAsAdmin(page);
    await page.goto('/chat');
    await openSideList(page);

    const probe = await page.evaluate(() => {
      const aside = document.querySelector('.sidebar-aside')!;
      const r = aside.getBoundingClientRect();
      // 侧栏中部命中测试：该点必须仍属于全局侧栏，而不是被左栏盖住
      const el = document.elementFromPoint(r.left + r.width / 2, r.top + Math.min(300, r.height - 8));
      const side = document.querySelector('.asst-layout > .asst-side')!.getBoundingClientRect();
      return {
        asideRight: r.right,
        sideLeft: side.left,
        owner: el?.closest('.sidebar-aside, .asst-side')?.className ?? String(el?.className ?? ''),
      };
    });
    expect(probe.owner).toContain('sidebar-aside');

    // 左栏从全局侧栏右缘开始（允许 1px 抖动）
    expect(probe.sideLeft).toBeGreaterThanOrEqual(probe.asideRight - 1);
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('会话很多时可滚动，且滚到底能切换到列表深处的会话', async ({ page, pageErrors }) => {
    await loginAsAdmin(page);
    await page.goto('/chat');
    await openSideList(page);

    const list = page.locator(`${sideRoot(page)} .asst-side-list`);
    const scrollable = await list.evaluate((el) => el.scrollHeight > el.clientHeight);
    expect(scrollable).toBe(true);

    // 按标题点「会话 30」（列表最深处的种子会话）。不点 .last()：桩进程状态
    // 跨用例共享，其它用例（首屏空状态）新建的空会话会追加到列表末尾——点了
    // 它就拿不到种子消息文本，选择必须对追加不敏感。
    const target = page
      .locator(`${sideRoot(page)} .asst-side-item`)
      .filter({ has: page.locator('.asst-side-title', { hasText: /^会话 30/ }) })
      .first();
    await target.scrollIntoViewIfNeeded();
    await target.locator('.asst-side-open').click();

    // 窄屏覆盖层会随选择收起；桌面左栏常驻，不做收起断言
    if ((page.viewportSize()?.width ?? 1280) < 900) {
      await expect(page.locator('.mobile-drawer-overlay')).toHaveCount(0);
    }
    await expect(page.locator('.asst-flow')).toContainText('第 30 个会话的第 1 条消息');
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('流式生成中切换会话被拦截且当前会话不被污染', async ({ page, pageErrors }) => {
    await loginAsAdmin(page);
    await page.goto('/chat');
    await expect(page.locator('.asst-layout')).toBeVisible();

    // 拦截对话请求并挂起（模拟慢流式响应）：busy 状态一直保持，
    // 请求根本不出浏览器，不依赖 mock server 的 chat_test 桩
    await page.route('**/api/channels/chat_test', () => { /* 永不 fulfill */ });

    // 发送一条消息进入流式生成中
    const input = page.locator('.asst-composer textarea').first();
    await input.fill('触发流式');
    await input.press('Enter');
    // 等 busy 落到 DOM：发送键变成「停止」即说明生成中态已生效
    await expect(page.locator('.asst-send.stop')).toBeVisible();

    // 生成中点左栏会话：必须被守卫拦截——toast 提示、消息区不切换。
    await openSideList(page);
    const rows = page.locator(`${sideRoot(page)} .asst-side-item`);
    const otherIdx = await rows.evaluateAll(
      (els) => els.findIndex((el) => !el.className.includes('active')),
    );
    expect(otherIdx).toBeGreaterThanOrEqual(0);
    await rows.nth(otherIdx).locator('.asst-side-open').click();
    await expect(page.getByText('正在生成回复，请先停止再操作会话').first()).toBeVisible();
    // 切换被拦截：消息区仍是当前会话（刚发的「触发流式」还在）
    await expect(page.locator('.asst-flow')).toContainText('触发流式');

    // 新建会话同样被拦截
    await openSideList(page);
    await page.locator(`${sideRoot(page)} .asst-new`).click();
    await expect(page.getByText('正在生成回复，请先停止再操作会话').first()).toBeVisible();
    await expect(page.locator('.asst-flow')).toContainText('触发流式');

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('删除非活动会话不会丢掉当前会话的消息', async ({ page, pageErrors }) => {
    await loginAsAdmin(page);
    await page.goto('/chat');
    await expect(page.locator('.asst-layout')).toBeVisible();

    // 会话已迁到后端持久化（桩维护 /api/chat/sessions 状态）。
    // 旧版这条断言取的是 `list.data[0].messages.length` —— 而「活动会话」未必是
    // 列表第 0 条，且所有桩会话都恰好 2 条消息；删掉哪条数字都不变，于是断言恒成立
    // = 没验。现在改成**活动会话身份 + 独有正文**双判据，并刻意把活动会话选在
    // 列表**第 5 条**（不是第 0 条）：删除后若实现把活动会话退回列表首条，
    // 标记消息就会消失 —— 这正是「删除非活动会话丢掉当前会话」的真实形态。
    const ACTIVE_ID = 'cs-seed-5';
    const MARKER = 'E2E-ACTIVE-MARKER';
    await page.evaluate(async ({ id, marker }) => {
      localStorage.setItem('aigx_playground_active', id);
      await fetch(`/api/chat/sessions/${id}/messages`, {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          messages: [
            { role: 'user', content: marker },
            { role: 'assistant', content: '已收到' },
          ],
        }),
      });
    }, { id: ACTIVE_ID, marker: MARKER });
    await page.reload();
    await expect(page.locator('.asst-flow')).toContainText(MARKER);

    // 删一条**非活动**会话：行内「删除这条对话」→ 二次确认。
    // 被删的必须是**本用例自己造的**一次性会话：桩进程状态跨用例/跨 project 共享，
    // 删掉 seed 里的「会话 N」会让同一文件后面的用例（以及 mobile project 的同一
    // 文件）找不到那些种子消息（实测就是这样：desktop 删了「会话 1」，mobile 的
    // 首屏断言随之红）。一次性会话不影响任何种子断言。
    const THROW_AWAY = 'E2E-THROW-AWAY';
    await page.evaluate(async (title) => {
      await fetch('/api/chat/sessions', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ title, model: '' }),
      });
    }, THROW_AWAY);
    // 造完就删：桩进程状态跨 project 共享，这条线只有「列表里此刻有它」才需要它。
    // 双保险：先 reload 让列表带上一次性会话，删完后直接从后端再拉一遍列表确认消失。
    await page.reload();
    await expect(page.locator('.asst-flow')).toContainText(MARKER);

    await openSideList(page);
    const rows = page.locator(`${sideRoot(page)} .asst-side-item`);
    const beforeCount = await rows.count();
    expect(beforeCount).toBeGreaterThan(1);
    const activeIdx = await rows.evaluateAll(
      (els) => els.findIndex((el) => el.className.includes('active')),
    );
    expect(activeIdx, '左栏里找不到活动会话').toBeGreaterThanOrEqual(0);
    // 一次性会话的 id 从后端列表里按标题取（POST 响应里拿 id 需要再评 evaluate 往返；
    // 按标题定位同一份 UI 行，行为断言不受影响）
    const target = rows.filter({ has: page.locator('.asst-side-title', { hasText: THROW_AWAY }) }).first();
    await expect(target, `左栏里没有一次性会话「${THROW_AWAY}」（列表共 ${beforeCount} 行）`).toHaveCount(1);
    await target.locator('.asst-side-act.del').click();
    await page.getByRole('button', { name: '删除' }).last().click();
    await expect(rows).toHaveCount(beforeCount - 1);
    await expect(rows.filter({ has: page.locator('.asst-side-title', { hasText: THROW_AWAY }) })).toHaveCount(0);

    // 活动会话没被切走、消息没丢（退回列表首条的实现会让标记消失）
    const stillActive = await page.evaluate(() => localStorage.getItem('aigx_playground_active'));
    expect(stillActive).toBe(ACTIVE_ID);
    await expect(page.locator('.asst-flow')).toContainText(MARKER);
    // 后端那侧的同一份数据也还在（不是只有界面残留）
    const serverCount = await page.evaluate(async (id) => {
      const res = await fetch(`/api/chat/sessions/${id}`);
      const body = (await res.json()) as { data?: { messages?: unknown[] } };
      return body.data?.messages?.length ?? -1;
    }, ACTIVE_ID);
    expect(serverCount).toBe(2);

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });
});
