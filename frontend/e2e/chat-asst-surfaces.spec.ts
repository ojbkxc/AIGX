import { test, expect, loginAsAdmin, newChatSession } from './fixtures';
import type { Page, Request } from '@playwright/test';

/**
 * `/chat` 的四层判据（`17573e87` 把 `asst-attach.ts` / `asst-model.ts` /
 * `asst-message-ops.ts` / `tool-calls.ts` 接上了界面）—— **入口真的在页面上** 的
 * 回归。`chat-sessions.spec.ts` 管会话管理、`chat-sse-tool-calls.spec.ts` 管 SSE
 * 正文与工具分片，两条都不碰下面这些判据层：
 *
 *   1. 附件：粘贴图片 / 拖入文本文件 → 附件行出现 → 上限（图 6 / 文本 3）→ 移除；
 *   2. 思考等级胶囊：候选**来自后端声明**（`/api/models/meta` 的 reasoning_levels），
 *      不是硬编码四档；选中后请求体带对；
 *   3. 消息操作：忙碌时编辑/删除/分叉/重生成整颗不渲染；空消息不给复制；
 *   4. 工具卡：一成一败不同色；非法 JSON 有标签；凭证被遮蔽。
 *
 * ── 为什么每块都要「反向对照」──────────────────────────────────
 * 只断「按钮在 / 标签在」的用例，在把被测逻辑**改坏**之后依然可能是绿的
 * （按钮本来就在页面上）。所以每块都配了同选择器的**正/反向成对**断言：
 *   · 附件：超限时物体**没进**列表（而不是只断 toast）；移除后再断数量下降；
 *   · 等级：候选里**没有 `off`**（兜底四档有 `off`，声明档没有」——这条才是
 *     「来自后端声明」的可验证含义），并用请求体反证选择真的进了 wire；
 *   · 消息操作：忙时 0 个、忙完 > 0（同一条选择器），否则「0 个」可能只是选择器写错；
 *   · 工具卡：`.tc-phase-done` 与 `.tc-phase-failed` 的**计算色不同**（只断类名
 *     会漏掉「两态同色」这个真实回归）。
 */

/* ── 1×1 PNG（合法图片：prepareImage 要过 Image 解码 + canvas 降采样）── */
const PNG_1PX =
  'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==';

/** 打开 /chat（等布局与输入区就绪） */
async function gotoChat(page: Page): Promise<void> {
  await page.goto('/chat');
  await expect(page.locator('.asst-layout')).toBeVisible();
  await expect(page.locator('.asst-composer textarea').first()).toBeVisible();
}

/** 左侧会话列表里点「新对话」并等空态（桩会话跨用例共享，必须从干净会话开始） */
async function freshSession(page: Page): Promise<void> {
  await newChatSession(page);
}

/**
 * 往输入框派发一次真实的 `paste` 事件（带 `clipboardData`）。
 *
 * 为什么不用 `setInputFiles`：粘贴是 `AsstComposer.onPaste` → `routeFiles` 的
 * 分拣路径（图片走 canvas、文本走 FileReader），`setInputFiles` 走的是
 * `<input type=file>` 的 onChange，是**另一个入口**（两条入口分开实现，只测一条
 * 会让另一条的分拣判据（`planAttachmentAdd`）完全不被覆盖）。
 */
async function pasteFiles(
  page: Page,
  files: Array<{ name: string; type: string; text?: string; base64?: string }>,
): Promise<void> {
  await page.locator('.asst-composer textarea').first().evaluate((el, payload) => {
    const dt = new DataTransfer();
    for (const f of payload) {
      let bytes: Uint8Array;
      if (f.base64) {
        const bin = atob(f.base64);
        bytes = new Uint8Array(bin.length);
        for (let i = 0; i < bin.length; i += 1) bytes[i] = bin.charCodeAt(i);
      } else {
        bytes = new TextEncoder().encode(f.text ?? '');
      }
      dt.items.add(new File([bytes], f.name, { type: f.type }));
    }
    el.dispatchEvent(new ClipboardEvent('paste', { clipboardData: dt, bubbles: true, cancelable: true }));
  }, files);
}

/** 往 `.asst-composer` 派发一次真实的 `drop`（拖入路径，与粘贴同一分拣函数） */
async function dropFiles(
  page: Page,
  files: Array<{ name: string; type: string; text: string }>,
): Promise<void> {
  await page.locator('.asst-composer').first().evaluate((el, payload) => {
    const dt = new DataTransfer();
    for (const f of payload) {
      dt.items.add(new File([new TextEncoder().encode(f.text)], f.name, { type: f.type }));
    }
    el.dispatchEvent(new DragEvent('drop', { dataTransfer: dt, bubbles: true, cancelable: true }));
  }, files);
}

/** 图片缩略图数 / 文本附件条数 */
function imageChips(page: Page) {
  return page.locator('.asst-composer .asst-atts .asst-att');
}
function fileChips(page: Page) {
  return page.locator('.asst-composer .asst-msg-files .asst-msg-file');
}

/** 一次性 SSE 桩（与 chat-sse-tool-calls.spec 同一手法：ReadableStream 逐帧写） */
async function installSse(page: Page, script: { frames: string[]; gapMs?: number; holdOpen?: boolean }): Promise<void> {
  await page.addInitScript((s: { frames: string[]; gapMs?: number; holdOpen?: boolean }) => {
    const origFetch = window.fetch.bind(window);
    window.fetch = async (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
      const url = typeof input === 'string' ? input : input instanceof URL ? input.pathname : input.url;
      if (!url.includes('/api/channels/chat_test')) return origFetch(input, init);
      const enc = new TextEncoder();
      let timer: number | null = null;
      let closed = false;
      const body = new ReadableStream<Uint8Array>({
        start(controller) {
          init?.signal?.addEventListener('abort', () => {
            closed = true;
            if (timer) window.clearTimeout(timer);
            try { controller.error(new DOMException('Aborted', 'AbortError')); } catch { /* 已关 */ }
          });
          const pump = async (): Promise<void> => {
            try {
              for (const frame of s.frames) {
                if (closed) return;
                controller.enqueue(enc.encode(frame));
                if (s.gapMs) await new Promise((r) => { timer = window.setTimeout(r, s.gapMs); });
              }
              if (!s.holdOpen && !closed) { closed = true; controller.close(); }
            } catch { /* abort 后 enqueue 抛错：与真实流断开一致 */ }
          };
          void pump();
        },
        cancel() { closed = true; if (timer) window.clearTimeout(timer); },
      });
      return new Response(body, { status: 200, headers: { 'Content-Type': 'text/event-stream; charset=utf-8' } });
    };
  }, script);
}

const contentFrame = (text: string): string =>
  `data: ${JSON.stringify({ choices: [{ delta: { content: text } }] })}\n\n`;

/** 一帧里塞三条调用：完成 / 失败+非法 JSON / 带凭据（见各用例的注释） */
function toolFrame(calls: unknown[]): string {
  return `data: ${JSON.stringify({ tool_calls: calls })}\n\n`;
}

test.describe('聊天 · 附件（粘贴 / 拖入 / 上限 / 移除）', () => {
  test('粘贴图片与拖入文本文件都进附件行；移除后数量下降', async ({ page, pageErrors }) => {
    await loginAsAdmin(page);
    await gotoChat(page);
    await freshSession(page);

    await pasteFiles(page, [{ name: 'shot.png', type: 'image/png', base64: PNG_1PX }]);
    await expect(imageChips(page)).toHaveCount(1);
    // 缩略图真的解码成功（data URL 形态，不是 blob：会话要落库）
    await expect(imageChips(page).first().locator('img')).toHaveAttribute('src', /^data:image\/png;base64,/);

    await dropFiles(page, [
      { name: 'cred.json', type: 'application/json', text: '{"token":"x","region":"cn"}' },
      { name: 'keys.info', type: 'text/plain', text: 'user=admin' },
    ]);
    await expect(fileChips(page)).toHaveCount(2);
    await expect(fileChips(page).first()).toContainText('cred.json');
    // 文件大小做了人性化（12×2 字节级 → B/KB），不是空白
    await expect(fileChips(page).first().locator('.asst-msg-file-size')).not.toBeEmpty();
    // 文本附件在场时提示「凭证也会存进会话」（安全口径必须在加完就能看到）
    await expect(page.locator('.asst-composer-warn', { hasText: '文本附件会以文本发送' })).toBeVisible();

    // 移除：图片一个、文本一个，各自消失且另一个不受影响
    await imageChips(page).first().locator('button.asst-att-x').click();
    await expect(imageChips(page)).toHaveCount(0);
    await expect(fileChips(page)).toHaveCount(2);

    await fileChips(page).first().locator('button.asst-msg-file-x').click();
    await expect(fileChips(page)).toHaveCount(1);
    await expect(fileChips(page).first()).toContainText('keys.info');

    /**
     * `.txt` / `.md` 必须与 `<input accept>` 一致地被收下。
     *
     * 这条曾经钉住的是**旧的真实行为**（accept 写着 `.txt,.md`，但
     * `isTextCredentialFile` 只认 `.json`/`.info` → 用户按界面提示选了 `.txt`
     * 却被静默判「不支持」）。两处已经对齐（判据补上 `.txt`/`.md`/`text/plain`），
     * 所以这里改成钉住**正确行为**：收下、且不弹「不支持」的 toast。
     * 谁把判据改回去，这条会红。
     */
    await dropFiles(page, [{ name: 'notes.txt', type: 'text/plain', text: 'hello' }]);
    await expect(fileChips(page)).toHaveCount(2);
    await expect(fileChips(page).last()).toContainText('notes.txt');
    await expect(
      page.locator('.toast', { hasText: '只支持图片和 JSON / .info 文本文件' }),
    ).toHaveCount(0);

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('上限：图片第 7 张进不来（6 张）、文本第 4 个进不来（3 个），且给出原因', async ({ page, pageErrors }) => {
    await loginAsAdmin(page);
    await gotoChat(page);
    await freshSession(page);

    // 一次粘 7 张：只该收 6 张，第 7 张按 too-many-images 跳过
    await pasteFiles(
      page,
      Array.from({ length: 7 }, (_, i) => ({ name: `p${i + 1}.png`, type: 'image/png', base64: PNG_1PX })),
    );
    await expect(imageChips(page)).toHaveCount(6);
    await expect(page.locator('.toast', { hasText: '最多 6 张图片' })).toBeVisible();

    // 已有 6 张时再粘一张：仍然 6 张（上限按「已有 + 读取中 + 本次」三段算）
    await pasteFiles(page, [{ name: 'p8.png', type: 'image/png', base64: PNG_1PX }]);
    await expect(imageChips(page)).toHaveCount(6);

    // 文本：4 个只收 3 个
    await dropFiles(
      page,
      Array.from({ length: 4 }, (_, i) => ({
        name: `cfg${i + 1}.json`, type: 'application/json', text: `{"i":${i + 1}}`,
      })),
    );
    await expect(fileChips(page)).toHaveCount(3);
    await expect(page.locator('.toast', { hasText: '一次最多 3 个文本附件' })).toBeVisible();

    // 反向对照：非 JSON/txt 的文件才该以「不支持」被跳过（上限不是唯一判据）
    await dropFiles(page, [{ name: 'archive.zip', type: 'application/zip', text: 'PK' }]);
    await expect(fileChips(page)).toHaveCount(3);
    await expect(page.locator('.toast', { hasText: '只支持图片和 JSON / .info 文本文件' })).toBeVisible();

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });
});

test.describe('聊天 · 思考等级胶囊', () => {
  /**
   * 把当前模型设成 `/api/models/meta` 里**声明过** reasoning_levels 的那个
   * （glm-4.7-flash: low/medium/high）。候选若来自兜底四档，会出现 `off`；
   * 来自模型声明则不会 —— 这就是「候选来自后端」的可验证含义。
   */
  async function seedDeclaredModel(page: Page): Promise<void> {
    await page.addInitScript(() => {
      localStorage.setItem('aigx_chat_assistant_cfg', JSON.stringify({
        model: 'glm-4.7-flash', reasoning: null,
      }));
    });
  }

  test('候选来自模型声明（无 off），选中后请求体带 reasoning_effort', async ({ page, pageErrors }) => {
    await loginAsAdmin(page);
    await seedDeclaredModel(page);
    await gotoChat(page);
    await freshSession(page);

    // 胶囊显示当前模型 + 「跟随模型」
    const pill = page.locator('[data-asst-trigger="model"]');
    await expect(pill).toContainText('glm-4.7-flash');
    await expect(pill).toContainText('跟随模型');

    await pill.click();
    const pop = page.locator('.asst-pop-lg');
    await expect(pop).toBeVisible();
    await pop.getByRole('button', { name: '推理等级' }).click();

    // 声明档：low / medium / high（+ 「跟随模型」占位），**没有 off**
    const levels = pop.locator('.asst-sub-list .asst-sub-item:not(:first-child) .mono');
    await expect(levels).toHaveText(['low', 'medium', 'high']);
    await expect(pop.locator('.asst-sub-item', { hasText: /^off$/ })).toHaveCount(0);

    await pop.locator('.asst-sub-item', { hasText: /^high$/ }).click();
    await expect(pop).toBeHidden();
    await expect(pill).toContainText('High');

    // 选择真的进了 wire：POST body 带 reasoning_effort=high
    const ta = page.locator('.asst-composer textarea').first();
    await ta.fill('等级已选，发一条');
    const [req] = await Promise.all([
      page.waitForRequest((r: Request) => r.url().includes('/api/channels/chat_test') && r.method() === 'POST'),
      ta.press('Enter'),
    ]);
    const body = req.postDataJSON() as Record<string, unknown>;
    expect(body.reasoning_effort, '选了等级却没进请求体').toBe('high');
    expect(body.model).toBe('glm-4.7-flash');

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('未选等级时不带 reasoning_effort（该字段由胶囊驱动，不是恒发）', async ({ page, pageErrors }) => {
    await loginAsAdmin(page);
    await seedDeclaredModel(page);
    await gotoChat(page);
    await freshSession(page);

    const ta = page.locator('.asst-composer textarea').first();
    await ta.fill('不选等级');
    const [req] = await Promise.all([
      page.waitForRequest((r: Request) => r.url().includes('/api/channels/chat_test') && r.method() === 'POST'),
      ta.press('Enter'),
    ]);
    const body = req.postDataJSON() as Record<string, unknown>;
    expect(body).not.toHaveProperty('reasoning_effort');

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });
});

test.describe('聊天 · 消息操作可用性', () => {
  test('忙碌时编辑/删除/分叉/重生成整颗不渲染；停止后恢复', async ({ page, pageErrors }) => {
    // 首帧后一直 hold：busy 保持在 true，才有可断言的窗口
    await installSse(page, { frames: [contentFrame('正在处理'), contentFrame('……')], gapMs: 400, holdOpen: true });
    await loginAsAdmin(page);
    await gotoChat(page);
    await freshSession(page);

    const ta = page.locator('.asst-composer textarea').first();
    await ta.fill('你好');
    await ta.press('Enter');
    // 生成中：发送键变停止
    await expect(page.locator('.asst-send.stop')).toBeVisible();

    // 四个判据：整颗不渲染（不是 disabled，是不存在）
    for (const label of ['编辑并重发', '删除这条', '重新生成', '从此条分叉']) {
      await expect(
        page.locator(`.asst-msg-acts button[aria-label="${label}"]`),
        `忙碌时「${label}」仍然渲染出来了`,
      ).toHaveCount(0);
    }
    // 复制仍可用（判据只拦写操作；已收到的正文要能复制）——证明上面「四个都是 0」
    // 不是选择器写错：同一条 `.asst-msg-acts` 容器里的按钮还是渲染得出来的
    await expect(page.locator('.asst-msg-acts button[aria-label="复制"]').first()).toBeVisible();

    // 停止 → 恢复：同样的选择器立刻能查到，反向断言才有意义
    await page.locator('.asst-send.stop').click();
    await expect(page.locator('.asst-send.stop')).toHaveCount(0);
    await expect(page.locator('.asst-msg-acts button[aria-label="编辑并重发"]').first()).toBeVisible();
    await expect(page.locator('.asst-msg-acts button[aria-label="删除这条"]').first()).toBeVisible();
    await expect(page.locator('.asst-msg-acts button[aria-label="重新生成"]').first()).toBeVisible();
    await expect(page.locator('.asst-msg-acts button[aria-label="从此条分叉"]').first()).toBeVisible();

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('空消息不给复制：只有附件没有正文的用户消息没有复制键，但有编辑/删除', async ({ page, pageErrors }) => {
    await loginAsAdmin(page);
    await gotoChat(page);
    await freshSession(page);

    // 只贴一张图，不写正文 → 用户消息 content === ''
    await pasteFiles(page, [{ name: 'only.png', type: 'image/png', base64: PNG_1PX }]);
    await expect(imageChips(page)).toHaveCount(1);
    await page.locator('.asst-send').click();

    const userMsg = page.locator('.asst-msg.user').last();
    await expect(userMsg.locator('.asst-msg-shot')).toBeVisible();
    // 空正文：复制键**不存在**（复制出来会是空串）
    await expect(userMsg.locator('.asst-msg-acts button[aria-label="复制"]')).toHaveCount(0);
    // 同一条消息的编辑/删除仍在（证明上面的 0 是因为判据，不是因为整条没渲染操作条）
    await expect(userMsg.locator('.asst-msg-acts button[aria-label="编辑并重发"]')).toBeVisible();
    await expect(userMsg.locator('.asst-msg-acts button[aria-label="删除这条"]')).toBeVisible();

    // 助手回复非空 ⇒ 复制/重生成/分叉都在（成对：空才没有复制）
    const bot = page.locator('.asst-msg.bot').last();
    await expect(bot.locator('.asst-msg-acts button[aria-label="复制"]')).toBeVisible();
    await expect(bot.locator('.asst-msg-acts button[aria-label="重新生成"]')).toBeVisible();
    await expect(bot.locator('.asst-msg-acts button[aria-label="从此条分叉"]')).toBeVisible();

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });
});

test.describe('聊天 · 工具轨迹卡片', () => {
  test('一成一败不同色；非法 JSON 有标签；凭证被遮蔽但 key_id 保留', async ({ page, pageErrors }) => {
    await installSse(page, {
      frames: [
        contentFrame('跑三个工具'),
        // 一帧三条调用：
        //  0 完成（结果 ok）
        //  1 失败 + 参数是半截 JSON（流式常态）
        //  2 完成但参数里带字面凭证（api_key），key_id 是标识符不该被遮
        toolFrame([
          { index: 0, id: 'c1', name: 'read_file', arguments: '{"path":"a.txt"}', result: 'ok: 12 lines' },
          { index: 1, id: 'c2', name: 'write_file', arguments: '{"path"', result: '⚠️ permission denied' },
          {
            index: 2, id: 'c3', name: 'create_key',
            arguments: '{"api_key":"sk-secret-literal","key_id":"k-9"}',
            result: '{"ok":true}',
          },
        ]),
        'data: [DONE]\n\n',
      ],
    });
    await loginAsAdmin(page);
    await gotoChat(page);
    await freshSession(page);

    const ta = page.locator('.asst-composer textarea').first();
    await ta.fill('跑工具');
    await ta.press('Enter');

    const card = page.locator('[data-testid="tool-call-card"]').last();
    await expect(card).toBeVisible();
    const items = card.locator('[data-testid="tool-call-item"]');
    await expect(items).toHaveCount(3);

    // 一成一败：单条调用的阶段徽标各自成档（不是全按消息级状态着色）
    await expect(card.locator('.tc-phase-done')).toHaveCount(2);
    await expect(card.locator('.tc-phase-failed')).toHaveCount(1);
    // 不同色：计算样式必须真的不同（只断类名会漏掉「两态同色」这个回归）
    const colorOf = async (sel: string): Promise<string> =>
      card.locator(sel).first().evaluate((el) => getComputedStyle(el).color);
    const doneColor = await colorOf('.tc-phase-done');
    const failColor = await colorOf('.tc-phase-failed');
    expect(failColor, `失败态与完成态同色：${failColor}`).not.toBe(doneColor);

    // 完成态收起参数（done 默认折叠）——展开失败那条与凭证那条才能看标签
    await items.nth(1).locator('.tc-fold').click();
    await expect(items.nth(1).locator('[data-testid="tool-call-invalid"]')).toBeVisible();
    // 非法 JSON 的参数**保留原文**（不是被当成空对象藏起来）
    await expect(items.nth(1).locator('[data-testid="tool-call-args"]')).toContainText('{"path"');

    await items.nth(2).locator('.tc-fold').click();
    await expect(items.nth(2).locator('[data-testid="tool-call-redacted"]')).toBeVisible();
    const argText = await items.nth(2).locator('[data-testid="tool-call-args"]').innerText();
    expect(argText, '凭证原文漏到 DOM 上了').not.toContain('sk-secret-literal');
    expect(argText).toContain('***');
    // 标识符不遮：遮了就看不出改的是哪把钥匙
    expect(argText).toContain('k-9');

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });
});
