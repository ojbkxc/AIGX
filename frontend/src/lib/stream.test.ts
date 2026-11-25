import { describe, expect, it } from 'vitest';
import { SseFramer, isAbortError, readSseBody, splitSseFrames, type SseFrame } from './stream';

const enc = (text: string): Uint8Array => new TextEncoder().encode(text);

/** 用给定的文本块造一个可读流；记录底层 cancel 是否真的被调用。 */
function fakeBody(chunks: string[], hooks: { onCancel?: () => void } = {}): ReadableStream<Uint8Array> {
  let i = 0;
  return new ReadableStream<Uint8Array>({
    pull(controller) {
      if (i < chunks.length) controller.enqueue(enc(chunks[i++]));
      else controller.close();
    },
    cancel() {
      hooks.onCancel?.();
    },
  });
}

/** 断言 promise 以 AbortError 结束（vitest 无 toSatisfy，手写更直白）。 */
async function expectAbort(promise: Promise<unknown>): Promise<void> {
  let error: unknown = null;
  try {
    await promise;
  } catch (err) {
    error = err;
  }
  expect(isAbortError(error)).toBe(true);
}

describe('SseFramer — 分帧', () => {
  it('半帧跨块：`data:` 行被切开也只在补齐后才出帧', () => {
    const framer = new SseFramer();
    expect(framer.feed('data: {"choices":[{"delta":{"con')).toEqual([]);
    expect(framer.feed('tent":"你"}}]}')).toEqual([]);
    const frames = framer.feed('\n\n');
    expect(frames).toEqual([{ event: '', data: '{"choices":[{"delta":{"content":"你"}}]}' }]);
  });

  it('多行 data 按规范用 \\n 拼成一个事件（旧实现逐行解析会丢成坏帧）', () => {
    const frames = splitSseFrames(
      'data: {"choices":[{"delta":{\n'
      + 'data: "content":"拼"\n'
      + 'data: }}]}\n\n',
    );
    expect(frames).toHaveLength(1);
    expect(JSON.parse(frames[0].data)).toEqual({ choices: [{ delta: { content: '拼' } }] });
  });

  it('event / id / retry 与 data 交错各自归位', () => {
    const frames = splitSseFrames('id: 7\nretry: 1500\nevent: error\ndata: {"error":{"message":"boom"}}\n\n');
    expect(frames).toEqual([{
      event: 'error',
      data: '{"error":{"message":"boom"}}',
      id: '7',
      retry: 1500,
    }]);
  });

  it('注释行（心跳 / keepalive）被丢弃，不产生事件', () => {
    expect(splitSseFrames(': ping\n\n')).toEqual([]);
    expect(splitSseFrames(': keepalive\n\n')).toEqual([]);
    // 注释与数据混在同一块：注释丢弃，数据保留
    const frames = splitSseFrames(': keepalive\ndata: [DONE]\n\n');
    expect(frames).toEqual([{ event: '', data: '[DONE]' }]);
  });

  it('CRLF 行尾、且 \\r 恰好落在块边界（旧实现会粘帧）', () => {
    const framer = new SseFramer();
    expect(framer.feed('data: a\r')).toEqual([]);
    expect(framer.feed('\n\r')).toEqual([]);
    expect(framer.feed('\ndata: b\r\n\r\n')).toEqual([
      { event: '', data: 'a' },
      { event: '', data: 'b' },
    ]);
  });

  it('UTF-8 BOM 在首块剥离', () => {
    expect(splitSseFrames('\uFEFFdata: x\n\n')).toEqual([{ event: '', data: 'x' }]);
  });

  it('超长单帧被丢弃并吐一个 error 帧（buffer 不再无限增长）', () => {
    const framer = new SseFramer({ maxFrameChars: 32 });
    const frames = framer.feed(`data: ${'x'.repeat(64)}`);
    expect(frames).toHaveLength(1);
    expect(frames[0].event).toBe('error');
    expect(frames[0].oversize).toBe(true);
    expect(JSON.parse(frames[0].data)).toMatchObject({ error: { type: 'sse_frame_too_large' } });
    // 丢弃之后缓冲是干净的：后续正常帧照常解析
    expect(framer.feed('\n\ndata: ok\n\n')).toEqual([{ event: '', data: 'ok' }]);
  });

  it('flush 吐出缺终结符的尾帧', () => {
    const framer = new SseFramer();
    expect(framer.feed('data: tail')).toEqual([]);
    expect(framer.flush()).toEqual([{ event: '', data: 'tail' }]);
  });
});

describe('readSseBody — 生命周期', () => {
  it('正常结束后释放读锁（旧实现从不 releaseLock，流保持 locked）', async () => {
    const body = fakeBody(['data: a\n\n']);
    const frames: SseFrame[] = [];
    await readSseBody(body, { onFrame: (f) => frames.push(f) });
    expect(frames).toEqual([{ event: '', data: 'a' }]);
    expect(body.locked).toBe(false);
  });

  it('abort 之后不再回调、且真断流（底层 cancel 被调用）', async () => {
    const controller = new AbortController();
    let cancelled = false;
    let step = 0;
    // highWaterMark 0：pull 只在有读请求时触发，保证「abort 落在两读之间」是确定的
    const body = new ReadableStream<Uint8Array>({
      pull(c) {
        if (step === 0) {
          step += 1;
          c.enqueue(enc('data: a\n\n'));
          return;
        }
        if (step === 1) {
          step += 1;
          controller.abort();
          c.enqueue(enc('data: b\n\n'));
          return;
        }
        c.close();
      },
      cancel() { cancelled = true; },
    }, { highWaterMark: 0 });

    const frames: SseFrame[] = [];
    await expectAbort(readSseBody(body, {
      signal: controller.signal,
      onFrame: (f) => frames.push(f),
    }));
    expect(frames).toEqual([{ event: '', data: 'a' }]);
    expect(cancelled).toBe(true);
  });

  it('signal 已 abort 时立刻抛 AbortError，一个帧都不回调', async () => {
    const controller = new AbortController();
    controller.abort();
    const frames: SseFrame[] = [];
    await expectAbort(readSseBody(fakeBody(['data: a\n\n']), {
      signal: controller.signal,
      onFrame: (f) => frames.push(f),
    }));
    expect(frames).toEqual([]);
  });

  it('stopWhen 为真时提前停读并 cancel', async () => {
    let cancelled = false;
    const frames: SseFrame[] = [];
    await readSseBody(fakeBody(['data: [DONE]\n\n', 'data: after\n\n'], {
      onCancel: () => { cancelled = true; },
    }), {
      onFrame: (f) => frames.push(f),
      stopWhen: () => frames.some((f) => f.data === '[DONE]'),
    });
    expect(frames.map((f) => f.data)).toEqual(['[DONE]']);
    expect(cancelled).toBe(true);
  });

  it('EOF 收尾：缺终结符的尾帧照样吐出', async () => {
    const frames: SseFrame[] = [];
    await readSseBody(fakeBody(['data: a\n\ndata: b']), { onFrame: (f) => frames.push(f) });
    expect(frames.map((f) => f.data)).toEqual(['a', 'b']);
  });

  it('跨块切开的 UTF-8 多字节字符不丢', async () => {
    const bytes = enc('data: {"c":"你"}\n\n');
    const cut = bytes.indexOf(0xe4) + 1;
    const body = new ReadableStream<Uint8Array>({
      start(c) {
        c.enqueue(bytes.slice(0, cut));
        c.enqueue(bytes.slice(cut));
        c.close();
      },
    });
    const frames: SseFrame[] = [];
    await readSseBody(body, { onFrame: (f) => frames.push(f) });
    expect(frames).toHaveLength(1);
    expect(JSON.parse(frames[0].data)).toEqual({ c: '你' });
  });
});
