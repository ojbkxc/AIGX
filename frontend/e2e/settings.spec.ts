import { test, expect, loginAsAdmin } from './fixtures';

/**
 * 系统设置 —— 分类导航回归（v1.0.93 设置菜单迁移后终态）。
 *
 * 迁移口径（docs/ROUTING-MENU-PARITY.md §9，对齐 Agent2API settings-page）：
 * - 保留分类：通用 / 显示 / 反馈与需求 / 网关 / 安全 / 数据 / 版本信息 / 更新
 *   （账户视落地情况另定；提供商不建——供应商已是顶级页 /channels，记分叉）
 * - 已迁出：AI 助手→/agent 抽屉、聊天→/chat、报表→仪表盘 Tab、限流与缓存→/limit、
 *   计费→/products 计费设置 Tab、通知→顶级 /notify、重试+超时→路由页「错误处理」rail 面板
 * - 已删除：流兼容 / 自动续写（A2A 无对应）
 */

/** 设置左栏分类导航 */
const nav = (page: import('@playwright/test').Page) => page.locator('.settings-nav');

test.describe('系统设置分类（v1.0.93 迁移后）', () => {
  test.beforeEach(async ({ page }) => {
    await loginAsAdmin(page, { sessions: [] });
    await page.goto('/settings');
    await expect(page.locator('.settings-layout')).toBeVisible();
  });

  test('左栏渲染终态分类，默认落在通用', async ({ page, pageErrors }) => {
    await expect(nav(page)).toBeVisible();
    for (const label of ['通用', '显示', '反馈与需求', '网关', '安全', '数据', '版本信息', '更新']) {
      await expect(nav(page).locator('.settings-nav-item', { hasText: label })).toHaveCount(1);
    }
    // 迁走的分类不再出现在设置左栏
    for (const gone of ['AI 助手', '聊天', '报表', '重试', '超时', '限流与缓存', '计费', '通知', '系统信息']) {
      await expect(nav(page).locator('.settings-nav-item', { hasText: gone })).toHaveCount(0);
    }
    // 默认分类高亮，右栏是通用（使用限额）
    await expect(page.locator('.settings-nav-item.active')).toContainText('通用');
    await expect(page.locator('.settings-pane')).toContainText('使用限额');
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('重试与超时已迁路由页「错误处理」：设置不再有，路由 rail 一级承载', async ({ page, pageErrors }) => {
    // 设置右栏（通用态）不含任何超时/重试字段
    await expect(page.locator('.settings-pane')).not.toContainText('请求总超时');

    // 路由页 rail 点「错误处理」→ RoutingErrorsPanel 渲染重试 4 字段 + 超时 5 字段
    await page.goto('/routing');
    await page.locator('.rt-shell-rail .rt-nav-item', { hasText: '错误处理' }).click();
    const pane = page.locator('.rt-shell-main');
    await expect(pane).toContainText('最大重试次数（供应商切换）');
    await expect(pane).toContainText('同账号重试次数');
    await expect(pane).toContainText('重试间隔');
    await expect(pane).toContainText('直接换号/换供应商状态码');
    await expect(pane).toContainText('请求总超时 (秒)');
    await expect(pane).toContainText('连接超时 (秒)');
    await expect(pane).toContainText('流空闲超时 (秒)');
    await expect(pane).toContainText('非流式响应体超时 (秒)');
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('错误处理面板保存打到 /api/limits', async ({ page, pageErrors }) => {
    const calls: string[] = [];
    page.on('request', (r) => {
      if (r.url().includes('/api/limits')) calls.push(r.method());
    });

    await page.goto('/routing');
    await page.locator('.rt-shell-rail .rt-nav-item', { hasText: '错误处理' }).click();
    // 路由错误处理面板内「重试/超时」卡的可见数字输入与保存钮（避开隐藏的粘滞档输入）
    await page.locator('.rt-shell-main input[type=number]:visible').first().fill('3');
    await page.locator('.rt-shell-main').getByRole('button', { name: '保存更改' }).first().click();

    await expect.poll(() => calls.filter((m) => m === 'PUT' || m === 'POST').length, { timeout: 5000 }).toBeGreaterThan(0);
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('网关 / 数据 / 安全分类各自渲染内容', async ({ page, pageErrors }) => {
    await nav(page).locator('.settings-nav-item', { hasText: '网关' }).click();
    await expect(page.locator('.settings-pane')).toContainText('网络层');

    // 「数据」在导航里唯一，用 exact 精确匹配导航项自身的文本（避免命中分组名）
    await nav(page).getByRole('button', { name: '数据', exact: true }).click();
    await expect(page.locator('.settings-pane')).toContainText('数据管理');
    const text = await page.locator('.settings-pane').innerText();
    expect(text.length).toBeGreaterThan(10);

    await nav(page).locator('.settings-nav-item', { hasText: '安全' }).click();
    await expect(page.locator('.settings-pane')).toContainText('安全策略');
    await expect(page.locator('.settings-pane')).toContainText('IP 过滤');
    // 安全面板首屏会拉系统指标，e2e 桩未提供该数据时的 console 报错与本迁移无关，过滤后断言
    const relevant = pageErrors.filter((e) => !e.includes('Failed to fetch metrics'));
    expect(relevant, relevant.join('\n')).toEqual([]);
  });

  test('显示分类：主题 / 缩放 / 语言（真落地，非空壳）', async ({ page, pageErrors }) => {
    await nav(page).locator('.settings-nav-item', { hasText: '显示' }).click();
    const pane = page.locator('.settings-pane');
    await expect(pane).toContainText('主题');
    await expect(pane).toContainText('界面缩放');
    await expect(pane).toContainText('语言');
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('反馈与需求分类：issue 入口 + 站内工单跳转（真落地，非空壳）', async ({ page, pageErrors }) => {
    await nav(page).locator('.settings-nav-item', { hasText: '反馈与需求' }).click();
    const pane = page.locator('.settings-pane');
    await expect(pane).toContainText('问题反馈');
    await expect(pane).toContainText('功能建议');
    await expect(pane).toContainText('提交工单（站内通道）');
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });
});
