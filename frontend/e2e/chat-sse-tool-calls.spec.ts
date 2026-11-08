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
 *
 * 前三个用例与末尾「形状用例」是页内桩——帧序列由测试作者手写，只能证明
 * **前端会解析这个形状**，不能证明**线上会有这个帧**。#17/F1 的病根正在这里：
 * 原 `usageFrame()` 无条件吐 usage 终帧，于是「界面出现 usage 数字」只是桩自证
 * （#31 同型假绿）。中间四个用例不打页内桩，请求真打到 static-server 的
 * **两跳链**（后端双按 #21-A 注入 `stream_options.include_usage` → 上游双只在
 * 收到该字段时才产 usage 终帧；账号池档则由后端双按 #21-B 合成
 * `estimated:true` 尾帧），数字出现与否、角标出现与否都由那条因果链决定。
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

/**
 * 页内桩的 usage 终帧——只用于「前端能否解析该形状」的形状用例（正常流）。
 * ⚠️ 它**不能**作为「线上有 usage」的证据：真实链路的 usage 终帧由上游按
 * `stream_options.include_usage` 条件产出（后端 #21-A 负责注入），页内手写
 * 帧无条件吐 = #17/F1 假绿本体。因果链验证在文件末尾的两个两跳用例里。
 */
const usageFrame = (prompt: number, completion: number, total: number, estimated = false): string =>
  `data: ${JSON.stringify({
    choices: [],
    usage: { prompt_tokens: prompt, completion_tokens: completion, total_tokens: total },
    // #24：口径标记挂在帧顶层（与 usage 同级），照 #21-B 的合成形状写
    ...(estimated ? { estimated: true } : {}),
  })}\n\n`;

/**
 * 两跳链路的固定数字（与 static-server.mjs 的 E2E_UPSTREAM_USAGE 一致）：
 * 断言用常量而非常量再抄一遍帧内容——帧由桩的**条件分支**产出，不是测试手写。
 */
const TWO_HOP_USAGE = { prompt: 11, completion: 7, total: 18 };

/** 触发「老 vLLM」档：上游收到 stream_options 直接 400，后端双去字段重试 */
const LEGACY_MARK = '[e2e-upstream:legacy]';

/** 触发「账号池归一化」档（#21-B 镜像）：上游不回报 usage，后端双合成 estimated 尾帧 */
const ACCOUNT_MARK = '[e2e-upstream:account]';

/**
 * 触发「慢速上游」档（#25）：帧间隔 60ms → 500ms。
 * 用途——「中途点停止」用例必须让 abort **落在 usage 尾帧之前**：默认 60ms 档
 * 下整条流约 300ms 就跑完（尾帧 ~250ms 到），点停止永远抢在后面，反向断言
 * 「不得出现数字」会在帧未落地时立刻成立 = 假绿（同 #22 的顺序纪律）。
 */
const SLOW_MARK = '[e2e-upstream:slow]';

/** 后端双合成的估算数字（与 static-server.mjs 的 E2E_GATEWAY_USAGE 一致）；
 *  刻意与上游真值 TWO_HOP_USAGE 不同——断言到 26/9/35 才证明走的是合成支路 */
const GATEWAY_USAGE = { prompt: 26, completion: 9, total: 35 };

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
    // #24：真值帧不带 `estimated` ⇒ 卡片内不得出现「估算」角标。
    // 与末尾「形状用例：估算帧」成对——只测有角标会退化成「角标恒开」的假绿。
    await expect(card.locator('[data-testid="usage-estimated"]')).toHaveCount(0);

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
    // #25（①）：中断收尾**不得凭空出现 usage 数字**。页内桩这条流压根没有 usage 帧，
    // 所以这条钉的是「前端不会自己造一个 usage 出来」；真链路上的同一不变量在
    // 下面「两跳中途停止」用例里验（那边帧确实存在，只是 abort 落在到达之前）。
    await expect(assistant.locator('[data-testid="usage-summary"]')).toHaveCount(0);

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

  // ── #22：usage 去桩自证（不打页内桩，请求真打到两跳链）──────────────────
  //
  // 链路（static-server.mjs）：前端 ─▶ `/api/channels/chat_test` 后端双（按
  // #21-A：stream=true 时往出站 payload 注入 `stream_options.include_usage`，
  // 上游 400 则去字段单次重试，SSE 原样透传）─▶ `/mock-upstream/chat/completions`
  // 上游双（真 OpenAI 行为：**只有**收到该字段才在 [DONE] 前吐 usage 终帧）。
  // 于是「界面有 usage 数字」不再是测试作者手写帧的结果，而是这条因果链的结果。

  test('两跳链路：后端双注入 stream_options → 上游才吐 usage → 界面显数字（#22）', async ({ page, pageErrors }) => {
    await gotoChat(page);
    await newSession(page);
    await send(page, '走两跳链路');

    // 上游双的正文分片必须经透传到达（顺带证明两跳真的打通）
    const assistant = page.locator('.chat-debugger-msg-assistant').last();
    await expect(assistant.locator('.chat-debugger-msg-content')).toContainText('两跳链路验证', { timeout: 10000 });

    // 关键断言：usage 三个数字只能来自「上游按条件产出的终帧」——
    // 该帧的存在 ⇔ 后端双确实注入了 stream_options.include_usage。
    const usage = assistant.locator('[data-testid="usage-summary"]');
    await expect(usage).toBeVisible();
    await expect(usage).toContainText(`${TWO_HOP_USAGE.prompt}`);
    await expect(usage).toContainText(`${TWO_HOP_USAGE.completion}`);
    await expect(usage).toContainText(`${TWO_HOP_USAGE.total}`);

    // busy 复位
    await expect(page.locator('.chat-debugger-send-fab.stop')).toHaveCount(0);
    // #24 口径区分（下半边）：该帧由**上游回报**，不带 estimated ⇒ 必须没有角标。
    // 与末尾账号池用例（有角标 + 另一组数字）成对，才叫「两种口径可区分」。
    await expect(assistant.locator('[data-testid="usage-estimated"]')).toHaveCount(0);
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('两跳反例：老上游 400 → 去字段重试 → 正文正常但界面不得凭空显 usage（#22）', async ({ page, pageErrors }) => {
    await gotoChat(page);
    await newSession(page);
    // message 带触发标记：上游双对 stream_options 直接 400（模拟老 vLLM/自建网关）
    await send(page, `走不识别的老上游 ${LEGACY_MARK}`);

    const assistant = page.locator('.chat-debugger-msg-assistant').last();
    // 重试链路完好：正文照常上屏（400 兜底不能把回复弄丢）
    await expect(assistant.locator('.chat-debugger-msg-content')).toContainText('两跳链路验证', { timeout: 10000 });

    // 先确认整条流**已经结束**（busy 复位发生在 [DONE] 处理之后），再断言「没有数字」。
    // 顺序不能颠倒：`toHaveCount(0)` 是反向断言，帧还没解析到就会立刻成立 —— 早于
    // usage 终帧落地则永远「通过」，等于没测（本地变异验证过：把上游改成无条件
    // 吐 usage 时，若先断言缺失，用例仍假绿）。
    await expect(page.locator('.chat-debugger-send-fab.stop')).toHaveCount(0);

    // 反向断言（去桩自证的下半边）：重试后出站请求已不含 stream_options，
    // 上游不发 usage 终帧 ⇒ 界面**绝不能**出现 usage 数字。此前页内桩无条件
    // 吐帧，这条回归根本测不出来——数字看起来「前端自造」也能过。
    await expect(assistant.locator('[data-testid="usage-summary"]')).toHaveCount(0);
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  // ── #25：两跳链路「中途停止」——abort 抢在 usage 尾帧之前 ⇒ 界面不得有数字 ──
  //
  // #22 的反例钉的是「上游不吐帧」，本例钉的是另一条：**帧存在但没到达**。
  // 慢速档（SLOW_MARK，500ms/帧）下 usage 尾帧约在 2.5s 才到、`[DONE]` 在 3s，
  // 而停止点在 0.5s 之后——于是「不出现数字」由 abort 截断这条因果决定，
  // 不是「桩压根没这帧」。纪律同 #22：反向断言必须**先跨过尾帧本应到达的
  // 时间窗**再断言缺失，否则帧未落地即成立 = 假绿。

  test('两跳中途停止：abort 落在 usage 尾帧之前 → 保留部分正文但不得出现数字（#25）', async ({ page, pageErrors }) => {
    await gotoChat(page);
    await newSession(page);
    await send(page, `走两跳链路但慢一点 ${SLOW_MARK}`);

    const body = page.locator('.chat-debugger-msg-assistant').last().locator('.chat-debugger-msg-content');
    // 第 1 个正文分片（500ms 档下 t≈0.5s）上屏即停手
    await expect(body).toContainText('两跳', { timeout: 10000 });
    // 此刻仍在流中：完整正文未出现 ⇒ 证明停止点确实落在尾帧之前（不是自然收尾）
    await expect(body).not.toContainText('两跳链路验证');

    await page.locator('.chat-debugger-send-fab').click();

    // 跨过「尾帧本应到达」的窗口（500ms 档：尾帧 2.5s、`[DONE]` 3s，停止在 ~0.6s）。
    // 纪律同 #22：反向断言若早于帧落地就会立刻成立＝假绿，必须先等过那个时间窗。
    await page.waitForTimeout(2600);

    // 停止已生效：⏹ 气泡出现（无工具分片时「已停止生成」按既定设计单独成泡，
    // 见 ChatDebugger.tsx abort 支路 6086f4a2/#63），busy 复位
    await expect(page.locator('.chat-debugger-msg-assistant', { hasText: '已停止生成' })).toHaveCount(1);
    await expect(page.locator('.chat-debugger-send-fab.stop')).toHaveCount(0);
    // 已收到的部分正文保留在同一只气泡里，未被回滚/覆盖
    const partial = page.locator('.chat-debugger-msg-assistant', { hasText: '两跳' });
    await expect(partial).toHaveCount(1);
    await expect(partial.locator('.chat-debugger-msg-content')).toContainText('两跳');
    // 关键反向断言：尾帧从未到达 ⇒ 界面不得出现 usage 数字。
    // 不只查那只气泡——整屏一起查，免得「数字被画到别处」漏网。
    await expect(partial.locator('[data-testid="usage-summary"]')).toHaveCount(0);
    await expect(page.locator('[data-testid="usage-summary"]')).toHaveCount(0);
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  // ── #24：估算口径角标（消费 #21-B 的 `estimated` 尾帧）────────────────────
  //
  // 账号池归一化链路里 usage 不是上游回报的，而是网关**估**出来的，口径必须
  // 能在界面上和真值区分开。e2e 侧把这条链镜像进 static-server：上游双对该档
  // 压根不回报 usage，后端双按 #21-B 在 `[DONE]` **前**合成
  // `{choices:[], usage:{…}, estimated:true}`（标记在帧顶层，不是 usage 内字段）。
  // ⇒ 「角标出现」依赖合成链 + 顶层键透传，不依赖测试作者手写帧。

  test('两跳·账号池档：后端双合成 estimated 尾帧 → 数字带「估算」角标（#24）', async ({ page, pageErrors }) => {
    await gotoChat(page);
    await newSession(page);
    await send(page, `走账号池归一化链路 ${ACCOUNT_MARK}`);

    const assistant = page.locator('.chat-debugger-msg-assistant').last();
    // 该档正文与真值档不同（「账号池归一」），档位选错不会撞绿
    await expect(assistant.locator('.chat-debugger-msg-content'))
      .toContainText('账号池归一', { timeout: 10000 });

    // 合成尾帧的数字（26/9/35）只能来自后端双——上游该档压根不回报 usage
    const usage = assistant.locator('[data-testid="usage-summary"]');
    await expect(usage).toContainText(`${GATEWAY_USAGE.prompt}`);
    await expect(usage).toContainText(`${GATEWAY_USAGE.completion}`);
    await expect(usage).toContainText(`${GATEWAY_USAGE.total}`);

    // 关键断言：这组数字是网关估算的 ⇒ 角标必须出现（且文案与真值档不同源）
    const badge = usage.locator('[data-testid="usage-estimated"]');
    await expect(badge).toHaveCount(1);
    await expect(badge).toContainText(/估算|Estimated/);

    await expect(page.locator('.chat-debugger-send-fab.stop')).toHaveCount(0);
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('形状用例：工具卡片内的 usage 带 estimated → 角标进卡片、meta 不重复渲染（#24）', async ({ page, pageErrors }) => {
    // ⚠️ 页内桩只证明「前端会渲染该形状」；线上会有该帧由上面的账号池两跳用例负责。
    // 本例补的是**第二个渲染位**（有工具卡片时 usage 在卡片内，而非气泡 meta）。
    await installSseStub(page, {
      frames: [
        toolFrame({ index: 0, id: 'call_e', type: 'function', function: { name: 'list_files', arguments: '{}' } }),
        contentFrame('已列出'),
        usageFrame(4, 3, 7, true),
        'data: [DONE]\n\n',
      ],
    });
    await gotoChat(page);
    await newSession(page);
    await send(page, '列一下文件');

    const assistant = page.locator('.chat-debugger-msg-assistant').last();
    const card = page.locator('[data-testid="tool-call-card"]').last();
    await expect(card.locator('[data-testid="usage-summary"]')).toContainText('7');
    await expect(card.locator('[data-testid="usage-estimated"]')).toHaveCount(1);
    // 二选一渲染：有卡片时气泡 meta 不再重复出一份 usage（角标也不会出现两处）
    await expect(assistant.locator('.chat-debugger-msg-usage')).toHaveCount(0);
    await expect(assistant.locator('[data-testid="usage-estimated"]')).toHaveCount(1);

    await expect(page.locator('.chat-debugger-send-fab.stop')).toHaveCount(0);
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });
});