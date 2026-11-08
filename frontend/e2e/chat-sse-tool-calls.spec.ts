import { test, expect, loginAsAdmin } from './fixtures';
import type { Page } from '@playwright/test';

/**
 * 聊天页 SSE 渐进渲染 + tool_call 卡片 —— #63 回归。
 *
 * 不依赖真实后端，也不依赖静态桩：在页面里替换 `window.fetch`，只拦截
 * `/api/channels/chat_test`，用真正的 ReadableStream 逐帧写出 SSE。
 * （`page.route().fulfill` 只能一次性交出完整 body，无法验证「渐进」。）
 *
 * 覆盖三态：
 *   1. 正常流：增量正文逐 token 上屏 → 工具分片按 index 聚合 → usage 收尾；
 *   2. 中断：点「停止」后保留已收到的部分，卡片标「停止」；
 *   3. 错误：error 帧在已流出正文后追加「⚠️」尾巴收尾，不出现占位空泡叠加。
 */

type StreamScript = {
  frames: string[];
  /** 每帧之间等待 ms（0 = 一次性写完） */
  gapMs?: number;
  /** 不主动 close（中断用例：靠 abort 切断） */
  holdOpen?: boolean;
  /** 首帧前延迟（「思考中」窗口期） */
  delayMs?: number;
};

/** 注入 SSE 桩：window.__e2eSse = <script> 后，chat_test 返回该伪流 */
async function installSseStub(page: Page, script: StreamScript): Promise<void> {
  await page.addInitScript((s: StreamScript) => {
    (window as unknown as { __e2eSse: StreamScript }).__e2eSse = s;
    const origFetch = window.fetch.bind(window);
    window.fetch = async (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
      const url = typeof input === 'string' ? input : input instanceof URL ? input.pathname : input.url;
      if (!url.includes('/api/channels/chat_test')) return origFetch(input, init);
      const cfg = (window as unknown as { __e2eSse: StreamScript }).__e2eSse;
      const enc = new TextEncoder();
      let timer: number | null = null;
      let closed = false;
      const body = new ReadableStream<Uint8Array>({
        start(controller) {
          // 与真实 fetch 一致：AbortSignal 触发时让流以 AbortError 断开，
          // 否则「停止生成」在 stub 下永远等不到 reader.read() 拒绝。
          init?.signal?.addEventListener('abort', () => {
            closed = true;
            if (timer) window.clearTimeout(timer);
            try { controller.error(new DOMException('Aborted', 'AbortError')); } catch { /* 已关闭 */ }
          });
          const pump = async (): Promise<void> => {
            try {
              if (cfg.delayMs) {
                await new Promise((r) => { timer = window.setTimeout(r, cfg.delayMs); });
              }
              for (const frame of cfg.frames) {
                if (closed) return;
                controller.enqueue(enc.encode(frame));
                if (cfg.gapMs) {
                  await new Promise((r) => { timer = window.setTimeout(r, cfg.gapMs); });
                }
              }
              if (!cfg.holdOpen && !closed) { closed = true; controller.close(); }
            } catch {
              // abort 后 enqueue 会抛：吞掉即可（与真实流断开一致）
            }
          };
          void pump();
        },
        cancel() {
          closed = true;
          if (timer) window.clearTimeout(timer);
        },
      });
      return new Response(body, {
        status: 200,
        headers: { 'Content-Type': 'text/event-stream; charset=utf-8' },
      });
    };
  }, script);
}

const contentFrame = (text: string): string =>
  `data: ${JSON.stringify({ choices: [{ delta: { content: text } }] })}\n\n`;

const toolFrame = (tool: Record<string, unknown>): string =>
  `data: ${JSON.stringify({ choices: [{ delta: { tool_calls: [tool] } }] })}\n\n`;

const usageFrame = (prompt: number, completion: number, total: number): string =>
  `data: ${JSON.stringify({ choices: [], usage: { prompt_tokens: prompt, completion_tokens: completion, total_tokens: total } })}\n\n`;

async function gotoChat(page: Page): Promise<void> {
  await loginAsAdmin(page);
  await page.goto('/chat');
  await expect(page.locator('.pg-shell')).toBeVisible();
  await expect(page.locator('.pg-shell textarea').first()).toBeVisible();
}

/** 新建空会话：桩进程的会话状态跨用例/跨 project 共享，前序用例落盘的消息
 *  会在首屏载入回来。所有用例从干净会话开始，断言才对历史不敏感。 */
async function newSession(page: Page): Promise<void> {
  await page.getByRole('button', { name: '历史会话' }).first().click();
  await page.getByRole('button', { name: '新会话' }).first().click();
  await expect(page.locator('.sd-root.open')).toHaveCount(0);
  await expect(page.locator('.pg-empty')).toBeVisible();
}

async function send(page: Page, text: string): Promise<void> {
  const input = page.locator('.pg-shell textarea').first();
  await input.fill(text);
  await input.press('Enter');
}

test.describe('聊天 · SSE 渐进渲染与工具调用卡片', () => {
  test('正常流：正文渐进上屏 + 工具分片聚合 + usage 收尾', async ({ page, pageErrors }) => {
    await installSseStub(page, {
      gapMs: 120,
      frames: [
        contentFrame('正在'),
        contentFrame('检查文件'),
        // 首帧：id/name + 空参数（OpenAI 契约，chat.rs 透传口径）
        toolFrame({ index: 0, id: 'call_abc', type: 'function', function: { name: 'read_file', arguments: '' } }),
        // 后续帧：只有 arguments 分片，必须按 index 拼接
        toolFrame({ index: 0, function: { arguments: '{"path":' } }),
        toolFrame({ index: 0, function: { arguments: '"README.md"}' } }),
        usageFrame(12, 8, 20),
        'data: [DONE]\n\n',
      ],
    });
    await gotoChat(page);
    await newSession(page);
    await send(page, '帮我读一下 README');

    const assistant = page.locator('.chat-debugger-msg-assistant').last();

    // 渐进性：首帧后可见「正在」，而最终内容尚未出现（窗口期内轮询）
    await expect(assistant).toContainText('正在', { timeout: 5000 });
    await expect(assistant.locator('.chat-debugger-msg-content'))
      .not.toContainText('正在检查文件');
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);

    // 最终正文聚合
    await expect(assistant.locator('.chat-debugger-msg-content'))
      .toContainText('正在检查文件', { timeout: 5000 });

    // 工具卡片：名称 + 拼接后的完整参数
    const card = page.locator('[data-testid="tool-call-card"]').last();
    await expect(card).toBeVisible();
    await expect(card).toContainText('read_file');
    await expect(card.locator('[data-testid="tool-call-args"]'))
      .toContainText('{"path":"README.md"}');

    // 正常收尾：卡片完成态 + usage 展示
    await expect(card).toContainText('完成');
    await expect(card.locator('[data-testid="usage-summary"]')).toContainText('20');

    // busy 已复位（发送按钮回到「发送」）
    await expect(page.locator('.chat-debugger-send-fab.stop')).toHaveCount(0);

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('中断：停止后保留已收到的正文与工具分片，卡片标「停止」', async ({ page, pageErrors }) => {
    await installSseStub(page, {
      gapMs: 150,
      holdOpen: true,
      frames: [
        contentFrame('部分回复'),
        toolFrame({ index: 0, id: 'call_x', type: 'function', function: { name: 'search', arguments: '{"q":"aigx' } }),
      ],
    });
    await gotoChat(page);
    await newSession(page);
    await send(page, '搜索一下');

    // 先等到正文与工具卡片出现
    const assistant = page.locator('.chat-debugger-msg-assistant').last();
    await expect(assistant.locator('.chat-debugger-msg-content')).toContainText('部分回复', { timeout: 5000 });
    const card = page.locator('[data-testid="tool-call-card"]').last();
    await expect(card).toBeVisible();
    await expect(card.locator('[data-testid="tool-call-args"]')).toContainText('{"q":"aigx');

    // 点停止：abort 上游请求
    await page.locator('.chat-debugger-send-fab').click();

    // 中断态：已收到的正文原样保留（不被「已停止生成」覆盖）、卡片标「停止」
    await expect(assistant).toContainText('部分回复');
    await expect(card).toContainText('停止');
    // 参数未被中断清空：卡片折叠态点「输入」展开，仍能看到已收到的分片
    await card.getByRole('button', { name: '输入' }).click();
    await expect(card.locator('[data-testid="tool-call-args"]')).toContainText('{"q":"aigx');
    // busy 复位
    await expect(page.locator('.chat-debugger-send-fab.stop')).toHaveCount(0);

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('错误：SSE error 帧收尾为错误气泡，不叠加占位空泡', async ({ page, pageErrors }) => {
    await installSseStub(page, {
      frames: [
        contentFrame('写了一半'),
        `event: error\ndata: ${JSON.stringify({ error: { message: '上游限流', type: 'rate_limit' } })}\n\n`,
        'data: [DONE]\n\n',
      ],
    });
    await gotoChat(page);
    await newSession(page);
    await send(page, '触发错误');

    const bubbles = page.locator('.chat-debugger-msg-assistant');
    await expect(bubbles.last()).toContainText('上游限流', { timeout: 5000 });
    // F4：错误帧是「追加」不是「覆盖」——已流出的正文必须保留在同一气泡里
    await expect(bubbles.last()).toContainText('写了一半');
    // 错误就地收尾：当前错误气泡只有 1 个（不出现「…」+「⚠️」双泡叠加）。
    // 注意桩进程的会话状态跨用例共享，前序用例的消息可能已被持久化回来，
    // 所以只数「带错误文案」的气泡，不数全部助手气泡。
    await expect(page.locator('.chat-debugger-msg-assistant', { hasText: '上游限流' })).toHaveCount(1);
    // 末尾不残留流式占位「…」气泡
    await expect(bubbles.last().locator('.chat-debugger-msg-content')).not.toHaveText('…');
    // busy 复位
    await expect(page.locator('.chat-debugger-send-fab.stop')).toHaveCount(0);

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });
});