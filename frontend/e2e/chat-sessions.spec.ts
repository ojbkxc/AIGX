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

async function openDrawer(page: import('@playwright/test').Page): Promise<void> {
  await page.getByRole('button', { name: '历史会话' }).first().click();
  await expect(page.locator('.sd-root.open')).toBeVisible();
}

test.describe('聊天 · 历史会话抽屉', () => {
  test('聊天主区域铺满视口高度（输入区贴底、消息区可滚动）', async ({ page, pageErrors }) => {
    await loginAsAdmin(page);
    await page.goto('/chat');
    await expect(page.locator('.pg-shell')).toBeVisible();

    const vh = await page.evaluate(() => window.innerHeight);

    // 修复前 .pg-shell 的 height:100% 在 auto 高度父级里退化为内容高度
    // （实测约 269px），消息区被压扁、输入框悬在页面中部。
    const shell = await page.locator('.pg-shell').boundingBox();
    expect(shell).not.toBeNull();
    expect(shell!.height).toBeGreaterThan(vh * 0.9);

    // 输入区应贴近视口底部（允许状态行占用的空间）
    const inputRow = await page.locator('.chat-debugger-input-row').boundingBox();
    expect(inputRow).not.toBeNull();
    expect(inputRow!.y + inputRow!.height).toBeGreaterThan(vh * 0.85);

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('抽屉形态：桌面内嵌不挡侧栏 / 移动覆盖全屏，且标题未被遮挡', async ({ page, pageErrors }) => {
    await loginAsAdmin(page);
    await page.goto('/chat');
    await expect(page.locator('.pg-shell')).toBeVisible();
    await openDrawer(page);

    // sidebar 变体（对齐 AI 运维页）：桌面端内嵌在 .pg-shell 左端，把消息区
    // 往右推，不脱离文档流（static）；窄屏（<900px）由组件 CSS 降级为
    // fixed 覆盖层。曾为 overlay 变体：从视口最左滑出把全局侧栏整个盖住。
    const isNarrow = (page.viewportSize()?.width ?? 1280) < 900;
    const rootPos = await page.evaluate(
      () => getComputedStyle(document.querySelector('.sd-root.sidebar')!).position,
    );
    expect(rootPos).toBe(isNarrow ? 'fixed' : 'static');

    // 几何下界用 85%：CI 的 Linux Chromium 常驻横向滚动条，布局视口比
    // viewportSize().height 矮十几像素（实测 707/720 ≈ 98%），留余量避免假红；
    // 回归时高度只有 ~269/720 ≈ 37%，远低于 85%，仍能抓住。
    const viewportHeight = page.viewportSize()!.height;
    const drawerBox = await page.locator('.sd-panel').boundingBox();
    expect(drawerBox).not.toBeNull();
    expect(drawerBox!.y).toBeLessThanOrEqual(1);
    expect(drawerBox!.height).toBeGreaterThan(viewportHeight * 0.85);

    // 面板背景必须不透明：曾用 var(--bg-elevated, rgba(255,255,255,.04))，
    // 该变量在主题体系中不存在，fallback 近乎全透明，亮色主题下侧栏文字
    // 透过面板混排（标题与品牌名叠印）。断言 alpha ≥ 0.95 防回归。
    const panelBg = await page.evaluate(
      () => getComputedStyle(document.querySelector('.sd-panel')!).backgroundColor,
    );
    const parts = panelBg.match(/rgba?\(([^)]+)\)/)![1].split(',').map((s) => parseFloat(s));
    const alpha = parts.length === 4 ? parts[3] : 1;
    expect(alpha, `面板背景 ${panelBg} 近乎透明，侧栏会透过来`).toBeGreaterThan(0.95);

    // 标题可点击 = 没有被更高层元素覆盖（正是移动端汉堡按钮那个 bug）
    const title = page.locator('.sd-head');
    await expect(title).toBeVisible();
    await title.click({ trial: true });

    // 窄屏覆盖层形态：视口左上角命中测试必须在抽屉内（若汉堡按钮 fixed
    // 盖住抽屉标题，此项即失败）。桌面内嵌形态该点属于全局侧栏，在
    // 「不遮挡主导航」用例里单独断言。
    if (isNarrow) {
      const topLeftOwner = await page.evaluate(() => {
        const el = document.elementFromPoint(24, 24);
        return el?.closest('.sd-root, .mobile-menu-btn')?.className ?? String(el?.className ?? '');
      });
      expect(topLeftOwner).toContain('sd-root');
      expect(topLeftOwner).not.toContain('mobile-menu-btn');
    }

    // 抽屉内关闭按钮可用
    await page.locator('.sd-close').click();
    await expect(page.locator('.sd-root.open')).toHaveCount(0);

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('切换会话后抽屉自动收起并载入目标消息', async ({ page, pageErrors }) => {
    await loginAsAdmin(page);
    await page.goto('/chat');
    await expect(page.locator('.pg-shell')).toBeVisible();

    await openDrawer(page);
    // 从列表里读出第 2 项的标题，再用它反推该会话的消息文本
    // （seed 的标题是「会话 N」，消息是「第 N 个会话的第 1/2 条消息」）
    const secondTitle = (await page.locator('.sd-item-title').nth(1).innerText()).trim();
    const n = secondTitle.replace(/\D+/g, '');
    await page.locator('.sd-item-btn').nth(1).click();

    await expect(page.locator('.sd-root.open')).toHaveCount(0);
    await expect(page.locator('.chat-debugger-messages')).toContainText(`第 ${n} 个会话的第 1 条消息`);
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('抽屉打开时全局侧边栏仍可交互（不遮挡主导航）', async ({ page, pageErrors }) => {
    // 用户回归反馈：overlay 抽屉从视口最左滑出，把全局主导航（仪表盘/渠道
    // 管理等）整个盖住。sidebar 变体后抽屉起点必须在全局侧栏右缘之右，
    // 且侧栏命中测试不被抽屉抢走。
    const isNarrow = (page.viewportSize()?.width ?? 1280) < 900;
    test.skip(isNarrow, '移动端全局侧栏为汉堡抽屉，与覆盖层抽屉天然不冲突');

    await loginAsAdmin(page);
    await page.goto('/chat');
    await openDrawer(page);

    const probe = await page.evaluate(() => {
      const aside = document.querySelector('.sidebar-aside')!;
      const r = aside.getBoundingClientRect();
      // 侧栏中部命中测试：该点必须仍属于全局侧栏，而不是被抽屉盖住
      const el = document.elementFromPoint(r.left + r.width / 2, r.top + Math.min(300, r.height - 8));
      return {
        asideRight: r.right,
        owner: el?.closest('.sidebar-aside, .sd-root')?.className ?? String(el?.className ?? ''),
      };
    });
    expect(probe.owner).toContain('sidebar-aside');

    // 抽屉面板从全局侧栏右缘开始（允许 1px 抖动）
    const panel = await page.locator('.sd-panel').boundingBox();
    expect(panel).not.toBeNull();
    expect(panel!.x).toBeGreaterThanOrEqual(probe.asideRight - 1);

    await page.locator('.sd-close').click();
    await expect(page.locator('.sd-root.open')).toHaveCount(0);
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('会话很多时可滚动，且滚到底能切换到最后一个会话', async ({ page, pageErrors }) => {
    await loginAsAdmin(page);
    await page.goto('/chat');
    await openDrawer(page);

    const list = page.locator('.sd-list');
    const scrollable = await list.evaluate((el) => el.scrollHeight > el.clientHeight);
    expect(scrollable).toBe(true);

    const last = page.locator('.sd-item-btn').last();
    await last.scrollIntoViewIfNeeded();
    await last.click();

    await expect(page.locator('.sd-root.open')).toHaveCount(0);
    await expect(page.locator('.chat-debugger-messages')).toContainText('第 30 个会话的第 1 条消息');
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('流式生成中切换会话被拦截且当前会话不被污染', async ({ page, pageErrors }) => {
    await loginAsAdmin(page);
    await page.goto('/chat');
    await expect(page.locator('.pg-shell')).toBeVisible();

    // 拦截对话请求并挂起（模拟慢流式响应）：busy 状态一直保持，
    // 请求根本不出浏览器，不依赖 mock server 的 chat_test 桩
    await page.route('**/api/channels/chat_test', () => { /* 永不 fulfill */ });

    // 发送一条消息进入流式生成中
    const input = page.locator('.pg-shell textarea').first();
    await input.fill('触发流式');
    await input.press('Enter');
    // 等 onBusyChange(true) 上报（发送即 setBusy，无需等网络）
    await page.waitForTimeout(400);

    // 生成中点历史会话：必须被守卫拦截——toast 提示、消息区不切换。
    // （sidebar 变体点项不自动收拢，守卫拦截时会主动收起抽屉——否则窄屏
    // 覆盖层形态下抽屉盖着 toast 与「历史会话」按钮；不以抽屉状态断言切换成败）
    await openDrawer(page);
    await page.locator('.sd-item-btn').nth(1).click();
    await expect(page.getByText('正在生成回复，请先停止再操作会话').first()).toBeVisible();
    // 切换被拦截：消息区仍是当前会话（刚发的「触发流式」还在，
    // 且没有载入目标会话的 seed 消息）
    await expect(page.locator('.chat-debugger-messages')).toContainText('触发流式');
    await expect(page.locator('.chat-debugger-messages')).not.toContainText('第 2 个会话');

    // 新建会话同样被拦截
    await openDrawer(page);
    await page.getByRole('button', { name: '新会话' }).first().click();
    await expect(page.getByText('正在生成回复，请先停止再操作会话').first()).toBeVisible();
    await expect(page.locator('.chat-debugger-messages')).toContainText('触发流式');

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('删除非活动会话不会丢掉当前会话的消息', async ({ page, pageErrors }) => {
    await loginAsAdmin(page);
    await page.goto('/chat');
    await expect(page.locator('.pg-shell')).toBeVisible();

    // 会话已迁到后端持久化（mock 维护 /api/chat/sessions 状态）：
    // 删除**非活动**会话时，活动会话的消息（含 debounce 窗口内未 PUT 的）
    // 必须完好。这里从活动会话视角取删除前后的消息条数对比。
    const readActiveCount = async (): Promise<number> =>
      page.evaluate(async () => {
        const list = (await (await fetch('/api/chat/sessions')).json()) as {
          data?: Array<{ id: string }>;
        };
        const first = list.data?.[0]?.id;
        if (!first) return -1;
        const detail = (await (await fetch(`/api/chat/sessions/${first}`)).json()) as {
          data?: { messages?: unknown[] };
        };
        return detail.data?.messages?.length ?? -1;
      });
    const beforeLen = await readActiveCount();
    expect(beforeLen).toBeGreaterThan(0);

    await openDrawer(page);
    const beforeCount = await page.locator('.sd-item').count();
    expect(beforeCount).toBeGreaterThan(1);
    // 删除第 2 个（非活动）会话（点击后弹确认框）。
    // 确认按钮必须点名 dialog 内的：sidebar 变体下 ConfirmDialog 内联渲染在
    // .sd-root 里，裸 .last() 会命中消息区更靠后的「删除消息」按钮。
    await page.locator('.sd-item-del').nth(1).click();
    await page.getByRole('dialog').getByRole('button', { name: '删除' }).click();

    // 只少一条（seed 有 30 条，不是删到只剩 1 条）
    await expect(page.locator('.sd-item')).toHaveCount(beforeCount - 1);
    // 删除是异步的（API 往返 + 状态收敛），poll 等待活动会话消息数收敛回原值
    await expect.poll(readActiveCount, { timeout: 5000 }).toBe(beforeLen);
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });
});
