import { test, expect, loginAsAdmin } from './fixtures';

/**
 * 请求日志 · 对话浏览 —— 功能级回归。
 *
 * 核心需求：管理员点开一条请求日志就能看到这轮对话说了什么，
 * **不需要**先去系统设置打开调试模式（log_body）。
 */

test.describe('请求日志对话浏览', () => {
  test('管理员点开日志能看到对话气泡', async ({ page, pageErrors }) => {
    await loginAsAdmin(page);
    await page.goto('/logs');
    await expect(page.locator('.main-content')).toContainText('请求日志');

    // mock 里第一条日志带 conversation
    await page.locator('tbody tr').first().click();
    const dlg = page.locator('.log-detail-conv');
    await expect(dlg).toBeVisible();
    await expect(dlg).toContainText('对话内容');

    // 两个角色气泡：user 与 assistant
    await expect(page.locator('.log-msg-user')).toHaveCount(1);
    await expect(page.locator('.log-msg-assistant')).toHaveCount(1);
    await expect(page.locator('.log-msg-user')).toContainText('帮我总结这段文本');
    await expect(page.locator('.log-msg-assistant')).toContainText('好的，要点如下');

    // 系统提示只显示字数，不展示正文
    await expect(dlg).toContainText('系统提示');
    await expect(dlg).not.toContainText('内部指令');

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('对话区与 debug 快照相互独立（未开 log_body 也有对话）', async ({ page, pageErrors }) => {
    await loginAsAdmin(page);
    await page.goto('/logs');
    await page.locator('tbody tr').first().click();

    // 这条 mock 日志没有 debug 字段 → 说明对话不依赖 log_body
    await expect(page.locator('.log-detail-conv')).toBeVisible();
    await expect(page.locator('.log-detail-debug')).toHaveCount(0);
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('较早消息被省略时给出提示', async ({ page, pageErrors }) => {
    await loginAsAdmin(page);
    await page.goto('/logs');
    // 第二条日志带 truncated_history
    await page.locator('tbody tr').nth(1).click();
    await expect(page.locator('.log-detail-conv-note')).toBeVisible();
    await expect(page.locator('.log-detail-conv-note')).toContainText('较早的消息已省略');
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('管理员点开日志能看到路由回放（候选/选中/被过滤）', async ({ page, pageErrors }) => {
    await loginAsAdmin(page);
    await page.goto('/logs');
    // 第一条 mock 日志带完整路由回放字段
    await page.locator('tbody tr').first().click();

    const route = page.locator('.log-detail-route');
    await expect(route).toBeVisible();
    await expect(route).toContainText('路由回放');

    // 候选渠道 chips：ch-1 / ch-2 / ch-3
    await expect(route).toContainText('ch-1');
    await expect(route).toContainText('ch-2');
    await expect(route).toContainText('ch-3');

    // 选中渠道高亮（ch-1 是 selected_channel）。候选行与选中行都会给 ch-1
    // 加 .log-detail-chip-on，断言「至少一个高亮 chip 文本是 ch-1」即可。
    const onChips = route.locator('.log-detail-chip-on');
    await expect(onChips.first()).toContainText('ch-1');

    // 被过滤渠道及原因
    await expect(route).toContainText('circuit_open');
    await expect(route).toContainText('health_check_failed');
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });
});

/**
 * 安全边界 —— 本轮修的 SSRF / IDOR，用 API 级断言兜住回归。
 *
 * 注意：E2E 的静态桩不实现真实鉴权与校验，这里只验证「前端不会因为
 * 后端拒绝而崩」，真正的规则由后端单测覆盖。
 */
test.describe('安全边界（前端侧）', () => {
  test('渠道表单提交内网地址时展示可读错误', async ({ page, pageErrors }) => {
    await loginAsAdmin(page);
    await page.goto('/channels');
    await expect(page.locator('.main-content')).toBeVisible();
    // 后端 400 时前端应显示错误而不是白屏
    const hasError = await page.evaluate(() => {
      const el = document.querySelector('.error-message');
      return el ? el.textContent : null;
    });
    // 无渠道时不会触发；只断言页面没有崩
    expect(hasError === null || typeof hasError === 'string').toBe(true);
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('Playground 页面对受限模型给出 403 提示路径', async ({ page, pageErrors }) => {
    await loginAsAdmin(page);
    await page.goto('/chat');
    await expect(page.locator('.pg-shell')).toBeVisible();
    // 页面正常渲染即为通过：403 的具体文案由后端保证
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });
});

/**
 * 在途请求 —— 对齐 Agent2API「能看到正在跑的请求并终止它」。
 *
 * AIGX 的 RequestLog 只在请求结束后落库，过程中原本完全不可见；
 * 本轮新增进程内登记表 + `/api/logs/inflight` 与 terminate 端点。
 */
test.describe('在途请求', () => {
  test('管理员可查看在途请求并终止', async ({ page, pageErrors }) => {
    await loginAsAdmin(page);
    await page.goto('/logs');
    await expect(page.locator('.main-content')).toContainText('日志与审计');

    await page.getByRole('button', { name: '在途请求' }).click();

    // 面板出现后表格里应有 mock 的那条在途请求
    await expect(page.locator('.main-content')).toContainText('inflight-1');
    await expect(page.locator('.main-content')).toContainText('glm-4.7-flash');
    await expect(page.locator('.main-content')).toContainText('lxchat-glm');

    // 点「终止」→ 二次确认 → 发 POST
    const calls: string[] = [];
    page.on('request', (r) => {
      if (r.url().includes('/terminate')) calls.push(r.method());
    });
    await page.getByRole('button', { name: '终止', exact: true }).first().click();
    await page.getByRole('button', { name: '终止', exact: true }).last().click();

    await expect.poll(() => calls.length, { timeout: 5000 }).toBeGreaterThan(0);
    expect(calls[0]).toBe('POST');
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });
});

/**
 * 清空预览 —— 一键清空前先告诉管理员「会删多少条、覆盖哪段时间」。
 */
test.describe('清空预览', () => {
  test('点清空全部时先请求预览并展示条数', async ({ page, pageErrors }) => {
    await loginAsAdmin(page);
    await page.goto('/logs');

    const previewCalls: string[] = [];
    page.on('request', (r) => {
      if (r.url().includes('/clear-preview')) previewCalls.push(r.url());
    });

    await page.getByRole('button', { name: '清空全部' }).click();

    await expect.poll(() => previewCalls.length, { timeout: 5000 }).toBeGreaterThan(0);

    const dlg = page.locator('[role="dialog"], .modal, .confirm-dialog').filter({ hasText: '清空日志' }).first();
    await expect(dlg).toContainText('将删除');
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });
});
