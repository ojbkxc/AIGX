import { test, expect, loginAsAdmin } from './fixtures';

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

  test('打开后铺满视口高度，且标题未被遮挡', async ({ page, pageErrors }) => {
    await loginAsAdmin(page);
    await page.goto('/chat');
    await expect(page.locator('.pg-shell')).toBeVisible();
    await openDrawer(page);

    // 抽屉与遮罩必须是 position:fixed —— 这是"铺满视口"的结构前提。
    // 曾是 absolute，父级 auto 高度时只能撑到内容高度（实测 269px，贴着输入框）。
    // 覆盖层定位在 `.sd-root.overlay` 上（fixed），panel / mask 相对它 absolute
    // 铺满 —— fixed 的是 root，所以拿 root 判「是否脱离文档流铺满视口」。
    const positions = await page.evaluate(() => ({
      root: getComputedStyle(document.querySelector('.sd-root.overlay')!).position,
      mask: getComputedStyle(document.querySelector('.sd-mask')!).position,
    }));
    expect(positions.root).toBe('fixed');
    expect(positions.mask).toBe('absolute');

    // 几何下界用 85%：CI 的 Linux Chromium 常驻横向滚动条，布局视口比
    // viewportSize().height 矮十几像素（实测 707/720 ≈ 98%），留余量避免假红；
    // 回归时高度只有 ~269/720 ≈ 37%，远低于 85%，仍能抓住。
    const viewportHeight = page.viewportSize()!.height;
    const drawerBox = await page.locator('.sd-panel').boundingBox();
    expect(drawerBox).not.toBeNull();
    expect(drawerBox!.y).toBeLessThanOrEqual(1);
    expect(drawerBox!.height).toBeGreaterThan(viewportHeight * 0.85);

    // 遮罩同样铺满
    const mask = await page.locator('.sd-mask').boundingBox();
    expect(mask!.height).toBeGreaterThan(viewportHeight * 0.85);

    // 标题可点击 = 没有被更高层元素覆盖（正是移动端汉堡按钮那个 bug）
    const title = page.locator('.sd-head');
    await expect(title).toBeVisible();
    await title.click({ trial: true });

    // 用视口右上角做命中测试：该点应在遮罩内、抽屉外。若汉堡按钮（fixed,
    // 左上角）盖住抽屉标题，抽屉会被判为「受遮挡」，此项即失败。
    const topLeftOwner = await page.evaluate(() => {
      const el = document.elementFromPoint(24, 24);
      return el?.closest('.sd-root, .mobile-menu-btn')?.className ?? String(el?.className ?? '');
    });
    expect(topLeftOwner).toContain('sd-root');
    expect(topLeftOwner).not.toContain('mobile-menu-btn');

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

  test('点遮罩关闭抽屉', async ({ page, pageErrors }) => {
    await loginAsAdmin(page);
    await page.goto('/chat');
    await openDrawer(page);

    // 点遮罩关闭：必须点在抽屉**右侧**的遮罩区。抽屉宽 min(320px, 86vw)，
    // 窄屏时 86vw 占比很大，用固定百分比（80%）会落进抽屉内。这里按实际
    // 抽屉宽度 + 一点余量取点，并钳制在视口内。
    const panel = await page.locator('.sd-panel').boundingBox();
    const vw = page.viewportSize()!.width;
    const x = Math.min(Math.round((panel?.width ?? 0) + 24), vw - 12);
    await page.locator('.sd-mask').click({ position: { x, y: 300 } });
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
    // （抽屉在 overlay 变体下点项即收拢，与切换是否成功无关，不以抽屉状态断言）
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
    // 删除第 2 个（非活动）会话（点击后弹确认框）
    await page.locator('.sd-item-del').nth(1).click();
    await page.getByRole('button', { name: '删除' }).last().click();

    // 只少一条（seed 有 30 条，不是删到只剩 1 条）
    await expect(page.locator('.sd-item')).toHaveCount(beforeCount - 1);
    // 删除是异步的（API 往返 + 状态收敛），poll 等待活动会话消息数收敛回原值
    await expect.poll(readActiveCount, { timeout: 5000 }).toBe(beforeLen);
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });
});
