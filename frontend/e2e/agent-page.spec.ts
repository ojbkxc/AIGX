import { test, expect, loginAsAdmin } from './fixtures';
import type { Page } from '@playwright/test';

/**
 * AI 运维页（`/agent`）· 结构与一轮对话的交付面。
 *
 * 为什么这条 spec 是缺口而不是锦上添花：`/agent` 此前**不在任何 spec 的 goto
 * 列表里**（`grep goto(` 全仓一条都没有），而这一轮它整页重写过（客户端配置面
 * `12f0a5b1` + 失败原因交付 `f2656e30`/`b5c9fe94`）。`/chat` 有 chat-*.spec，
 * 于是「AI 运维改坏了」这件事在 E2E 里没有任何一条会红。
 *
 * 覆盖面刻意分两层：
 *   · 结构层：A2A 客户端配置分区 + 对话工作台四件套（会话抽屉 / 消息流 / 输入区 /
 *     设置抽屉）都在，且关键控件可交互；
 *   · 行为层：一轮对话的**交付面**——正文上屏，且「跨模型切换时前面那些模型为什么
 *     失败」必须逐条出现在同一条助手消息里（这是 `b5c9fe94` 之前被压成一句话的
 *     那个缺陷）。
 *
 * 不发真请求：`/api/agent/sessions/:id/chat` 是 SSE，用 page.route 打一帧
 * `final` + 一帧 `notice`（协议见 `api/endpoints/prompts_public.ts` 与
 * `src/agent/api.rs`：每帧 `data: {AgentEvent}`，**不发 [DONE]**）。
 */

const SSE_FRAMES = [
  // 正常正文
  `data: ${JSON.stringify({ type: 'final', content: '正在检查供应商健康状态。' })}`,
  // 跨模型切换成功的 notice：后端把「前面那些模型为何失败」一并送来，
  // 前端必须**逐条**展示（压成一句 = 用户不知道为什么换了模型）
  `data: ${JSON.stringify({
    type: 'notice',
    message: 'glm-4.7-flash: 上游 429 rate limited；claude-sonnet-4: 渠道已禁用',
  })}`,
];

async function stubAgentChat(page: Page): Promise<void> {
  await page.route(/\/api\/agent\/sessions\/[^/]+\/chat$/, async (route) => {
    await route.fulfill({
      status: 200,
      headers: { 'Content-Type': 'text/event-stream; charset=utf-8', 'Cache-Control': 'no-store' },
      body: `${SSE_FRAMES.join('\n\n')}\n\n`,
    });
  });
}

test.describe('AI 运维页 · 结构', () => {
  test('A2A 客户端配置面 + 对话工作台四件套都在，控件可交互', async ({ page, pageErrors }) => {
    await loginAsAdmin(page, { sessions: [] });
    await page.goto('/agent');
    await expect(page.locator('.agent-page')).toBeVisible();

    // A2A v3.5.1 客户端配置分区（`12f0a5b1` 新增；此前页面只有对话工作台）
    // A2A 客户端检测面板已下掉（166d46c2）：AI 运维是「用 AI 运维网关」，
    // 不是「给外部客户端配网关」。腾出来的右栏改成运维概览。
    // 窄屏（≤1100px）下右栏降级为覆盖层抽屉：先点「运行信息」按钮打开，再断言。
    // 断言后必须关闭抽屉——抽屉遮罩层会拦截输入区/角色胶囊的后续点击。
    if ((page.viewportSize()?.width ?? 1440) <= 1100) {
      await page.getByRole('button', { name: '运行信息' }).click();
      await expect(page.locator('.mobile-drawer .agent-ops')).toBeVisible();
      // 关闭抽屉：按 Escape（遮罩层点击在窄屏下坐标可能落在 aside 上，不可靠）
      await page.locator('.mobile-drawer').press('Escape');
      await expect(page.locator('.mobile-drawer')).toHaveCount(0);
    } else {
      await expect(page.locator('.agent-ops')).toBeVisible();
    }

    // 对话工作台骨架：会话抽屉（sidebar 变体）+ 消息流 + 输入区
    await expect(page.locator('.agent-shell')).toBeVisible();
    await expect(page.locator('.agent-main .agent-messages')).toBeVisible();
    // 空态：标题 + 建议提示词（可点即发）
    await expect(page.locator('.agent-empty-title')).toHaveText('AI 运维工作台');
    await expect(page.locator('.agent-empty-prompt').first()).toBeVisible();
    await expect(page.locator('.agent-input-row')).toBeVisible();

    // 输入区：文本域 + 发送键（空输入时禁用）+ 角色胶囊 + 三个图标按钮
    const textarea = page.locator('textarea.agent-input');
    await expect(textarea).toHaveAttribute('placeholder', /输入运维指令/);
    const sendBtn = page.locator('.agent-send-fab');
    await expect(sendBtn).toBeDisabled();
    await textarea.fill('看一下渠道健康');
    await expect(sendBtn).toBeEnabled();
    // 角色胶囊：默认运维员（可写工具，高危仍审批）；点一下切到观察员（只读）
    const rolePill = page.locator('.agent-role-pill').first();
    await expect(rolePill).toHaveText(/运维员/);
    await expect(rolePill).toHaveClass(/operator/);
    await rolePill.click();
    await expect(rolePill).toHaveText(/观察员/);
    await expect(rolePill).not.toHaveClass(/operator/);
    // 图标按钮：会话列表 / 助手设置 / 清空对话
    await expect(page.locator('.agent-icon-btn[title="会话列表"]')).toBeVisible();
    await expect(page.locator('.agent-icon-btn[title="助手设置"]')).toBeVisible();
    await expect(page.locator('.agent-icon-btn[title="清空对话"]')).toBeVisible();

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('设置抽屉：点「助手设置」打开面板（不是空壳抽屉）', async ({ page, pageErrors }) => {
    await loginAsAdmin(page, { sessions: [] });
    await page.goto('/agent');
    await page.locator('.agent-icon-btn[title="助手设置"]').click();

    const drawer = page.locator('.mobile-drawer');
    await expect(drawer).toBeVisible();
    // 抽屉里是 AgentSettingsPanel：至少要有一个可保存的控件与真实字段
    await expect(drawer.locator('button, input, textarea, select').first()).toBeVisible();
    const controlCount = await drawer.locator('button, input, textarea, select').count();
    expect(controlCount, '助手设置抽屉里没有任何可交互控件（空壳）').toBeGreaterThan(3);

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });
});

test.describe('AI 运维页 · 一轮对话的交付面', () => {
  test('正文上屏；跨模型切换的逐条失败原因必须一起送达', async ({ page, pageErrors }) => {
    await stubAgentChat(page);
    await loginAsAdmin(page, { sessions: [] });
    await page.goto('/agent');
    await expect(page.locator('.agent-page')).toBeVisible();

    await page.locator('textarea.agent-input').fill('看一下渠道健康');
    await page.locator('.agent-send-fab').click();

    // 助手气泡：正文来自 final 帧
    const assistant = page.locator('.agent-msg-assistant').last();
    await expect(assistant).toContainText('正在检查供应商健康状态。', { timeout: 10_000 });
    // 逐条原因：两个模型的失败原因都要在**同一条**消息里（压成一句时这条会红）
    await expect(assistant).toContainText('glm-4.7-flash: 上游 429 rate limited');
    await expect(assistant).toContainText('claude-sonnet-4: 渠道已禁用');
    // 用户那条消息也在（发送成功的一半）
    await expect(page.locator('.agent-msg-user').last()).toContainText('看一下渠道健康');
    // 收尾：发送键回到可发送形态（busy 复位），页面没有落到错误态
    await expect(page.locator('.agent-send-fab.stop')).toHaveCount(0);
    await expect(page.locator('.agent-error')).toHaveCount(0);

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });
});
