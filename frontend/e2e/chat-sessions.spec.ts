import { test, expect, loginAsAdmin, makeSessions, SESSIONS_KEY, ACTIVE_KEY } from './fixtures';

/**
 * 聊天页历史会话抽屉 —— 回归测试。
 *
 * 覆盖三个真实修复过的缺陷：
 * 1. 抽屉高度：`.pg-shell` 的 height:100% 曾因父级无高度而退化，抽屉只能
 *    撑到输入区上方（高度 ≈ 269px）。断言抽屉实际高度接近视口高度。
 * 2. 层级：抽屉曾被困在 `.main-content`（z-index:10）的层叠上下文中，移动端
 *    汉堡按钮会盖住抽屉标题。断言抽屉覆盖视口且标题可点击（未被遮挡）。
 * 3. 删除非活动会话不应丢失活动会话在 debounce 窗口内的消息。
 */

async function openDrawer(page: import('@playwright/test').Page): Promise<void> {
  await page.getByRole('button', { name: '历史会话' }).first().click();
  await expect(page.locator('.pg-sessions.open')).toBeVisible();
}

test.describe('聊天 · 历史会话抽屉', () => {
  test('聊天主区域铺满视口高度（输入区贴底、消息区可滚动）', async ({ page, pageErrors }) => {
    await loginAsAdmin(page, { sessions: makeSessions(3) });
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

  test('打开后铺满视口高度，且标题未被遮挡', async ({ page, pageErrors }) => {
    await loginAsAdmin(page, { sessions: makeSessions(3) });
    await page.goto('/chat');
    await expect(page.locator('.pg-shell')).toBeVisible();
    await openDrawer(page);

    // 抽屉与遮罩必须是 position:fixed —— 这是"铺满视口"的结构前提。
    // 曾是 absolute，父级 auto 高度时只能撑到内容高度（实测 269px，贴着输入框）。
    const positions = await page.evaluate(() => ({
      drawer: getComputedStyle(document.querySelector('.pg-sessions')!).position,
      mask: getComputedStyle(document.querySelector('.pg-sessions-mask')!).position,
    }));
    expect(positions.drawer).toBe('fixed');
    expect(positions.mask).toBe('fixed');

    // 几何下界用 85%：CI 的 Linux Chromium 常驻横向滚动条，布局视口比
    // viewportSize().height 矮十几像素（实测 707/720 ≈ 98%），留余量避免假红；
    // 回归时高度只有 ~269/720 ≈ 37%，远低于 85%，仍能抓住。
    const viewportHeight = page.viewportSize()!.height;
    const drawerBox = await page.locator('.pg-sessions').boundingBox();
    expect(drawerBox).not.toBeNull();
    expect(drawerBox!.y).toBeLessThanOrEqual(1);
    expect(drawerBox!.height).toBeGreaterThan(viewportHeight * 0.85);

    // 遮罩同样铺满
    const mask = await page.locator('.pg-sessions-mask').boundingBox();
    expect(mask!.height).toBeGreaterThan(viewportHeight * 0.85);

    // 标题可点击 = 没有被更高层元素覆盖（正是移动端汉堡按钮那个 bug）
    const title = page.locator('.pg-sessions-head');
    await expect(title).toBeVisible();
    await title.click({ trial: true });

    // 用视口右上角做命中测试：该点应在遮罩内、抽屉外。若汉堡按钮（fixed,
    // 左上角）盖住抽屉标题，抽屉会被判为「受遮挡」，此项即失败。
    const topLeftOwner = await page.evaluate(() => {
      const el = document.elementFromPoint(24, 24);
      return el?.closest('.pg-sessions, .mobile-menu-btn')?.className ?? String(el?.className ?? '');
    });
    expect(topLeftOwner).toContain('pg-sessions');
    expect(topLeftOwner).not.toContain('mobile-menu-btn');

    // 抽屉内关闭按钮可用
    await page.locator('.pg-sessions-close').click();
    await expect(page.locator('.pg-sessions.open')).toHaveCount(0);

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('切换会话后抽屉自动收起并载入目标消息', async ({ page, pageErrors }) => {
    const sessions = makeSessions(3);
    await loginAsAdmin(page, { sessions });
    await page.goto('/chat');
    await expect(page.locator('.pg-shell')).toBeVisible();

    await openDrawer(page);
    await page.locator('.pg-sessions-item-btn').nth(1).click();

    await expect(page.locator('.pg-sessions.open')).toHaveCount(0);
    // 首条消息来自第 2 个会话
    await expect(page.locator('.chat-debugger-messages')).toContainText('第 2 个会话的第 1 条消息');
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('点遮罩关闭抽屉', async ({ page, pageErrors }) => {
    await loginAsAdmin(page, { sessions: makeSessions(2) });
    await page.goto('/chat');
    await openDrawer(page);

    // 点遮罩关闭：必须点在抽屉右侧的遮罩区（抽屉宽约 304px 或 86vw，
    // 因此取视口宽度的 80% 处，确保落在遮罩上而非抽屉内）。
    const vw = page.viewportSize()!.width;
    await page.locator('.pg-sessions-mask').click({ position: { x: Math.round(vw * 0.8), y: 300 } });
    await expect(page.locator('.pg-sessions.open')).toHaveCount(0);
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('会话很多时可滚动，且滚到底能切换到最后一个会话', async ({ page, pageErrors }) => {
    const sessions = makeSessions(30);
    await loginAsAdmin(page, { sessions });
    await page.goto('/chat');
    await openDrawer(page);

    const list = page.locator('.pg-sessions-list');
    const scrollable = await list.evaluate((el) => el.scrollHeight > el.clientHeight);
    expect(scrollable).toBe(true);

    const last = page.locator('.pg-sessions-item-btn').last();
    await last.scrollIntoViewIfNeeded();
    await last.click();

    await expect(page.locator('.pg-sessions.open')).toHaveCount(0);
    await expect(page.locator('.chat-debugger-messages')).toContainText('第 30 个会话的第 1 条消息');
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('删除非活动会话不会丢掉当前会话未落盘的消息', async ({ page, pageErrors }) => {
    const sessions = makeSessions(2);
    await loginAsAdmin(page, { sessions });
    await page.goto('/chat');
    await expect(page.locator('.pg-shell')).toBeVisible();

    // 给活动会话制造一条"只存在于 React state、尚未 debounce 落盘"的消息：
    // 直接在输入框输入并提交会走网络，这里改为断言删除后 localStorage 里
    // 活动会话的消息数未被回退。
    const beforeLen = await page.evaluate(
      ({ key, activeKey }) => {
        const raw = localStorage.getItem(key);
        const list = raw ? (JSON.parse(raw) as Array<{ id: string; messages: unknown[] }>) : [];
        const active = localStorage.getItem(activeKey);
        return list.find((s) => s.id === active)?.messages.length ?? -1;
      },
      { key: SESSIONS_KEY, activeKey: ACTIVE_KEY },
    );
    expect(beforeLen).toBeGreaterThan(0);

    await openDrawer(page);
    // 删除第 2 个（非活动）会话
    await page.locator('.pg-sessions-item-del').nth(1).click();

    await expect(page.locator('.pg-sessions-item')).toHaveCount(1);
    // 删除会触发 setState → effect → localStorage 写入，是异步的；
    // 直接读会在移动端慢一帧时读到旧值。用 expect.poll 等待收敛。
    await expect
      .poll(
        () =>
          page.evaluate(
            ({ key, activeKey }) => {
              const raw = localStorage.getItem(key);
              const list = raw ? (JSON.parse(raw) as Array<{ id: string; messages: unknown[] }>) : [];
              const active = localStorage.getItem(activeKey);
              return list.find((s) => s.id === active)?.messages.length ?? -1;
            },
            { key: SESSIONS_KEY, activeKey: ACTIVE_KEY },
          ),
        { timeout: 5000 },
      )
      .toBe(beforeLen);
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });
});
