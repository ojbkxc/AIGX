import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { parseSseFrame, readSseResponse, testChannelChatStream } from './index';
import type { ChatStreamDelta } from '../types';

const enc = (text: string): Uint8Array => new TextEncoder().encode(text);

/**
 * 最小 Response 替身：`readSseResponse` 只读 headers / body / text / ok，
 * 不依赖 undici 与 jsdom 对「流式 Response」的实现差异。
 */
function fakeResponse(
  body: ReadableStream<Uint8Array> | string,
  contentType: string,
  hooks: { onCancel?: () => void } = {},
): Response {
  const stream = typeof body === 'string'
    ? new ReadableStream<Uint8Array>({
      start(c) {
        c.enqueue(enc(body));
        c.close();
      },
      cancel() { hooks.onCancel?.(); },
    })
    : body;
  const text = typeof body === 'string' ? body : '';
  return {
    ok: true,
    status: 200,
    headers: new Headers({ 'Content-Type': contentType }),
    body: stream,
    text: async () => text,
  } as unknown as Response;
}

/** 逐块吐出文本的响应体；记录底层 cancel 是否被调用。 */
function chunkedResponse(
  chunks: string[],
  hooks: { onCancel?: () => void } = {},
): ReadableStream<Uint8Array> {
  let i = 0;
  return new ReadableStream<Uint8Array>({
    pull(c) {
      if (i < chunks.length) c.enqueue(enc(chunks[i++]));
      else c.close();
    },
    cancel() { hooks.onCancel?.(); },
  });
}

/** 收集增量到数组 */
function collect(frame: string): { deltas: ChatStreamDelta[]; ended: boolean } {
  const deltas: ChatStreamDelta[] = [];
  const ended = parseSseFrame(frame, (d) => deltas.push(d));
  return { deltas, ended };
}

describe('parseSseFrame — OpenAI SSE', () => {
  it('解析单条 OpenAI delta', () => {
    const frame = 'data: {"choices":[{"delta":{"content":"你好"}}]}\n\n';
    const { deltas, ended } = collect(frame);
    expect(deltas).toEqual([{ content: '你好', isEnd: false, kind: 'content' }]);
    expect(ended).toBe(false);
  });

  it('一条帧内多条 data: 行都解析', () => {
    const frame =
      'data: {"choices":[{"delta":{"content":"你"}}]}\n' +
      'data: {"choices":[{"delta":{"content":"好"}}]}\n\n';
    const { deltas } = collect(frame);
    expect(deltas).toEqual([
      { content: '你', isEnd: false, kind: 'content' },
      { content: '好', isEnd: false, kind: 'content' },
    ]);
  });

  it('[DONE] 标记结束', () => {
    const frame = 'data: [DONE]\n\n';
    const { deltas, ended } = collect(frame);
    expect(deltas).toEqual([{ content: '', isEnd: true }]);
    expect(ended).toBe(true);
  });

  it('忽略 reasoning 以外的空增量（role 帧）', () => {
    const frame = 'data: {"choices":[{"delta":{"role":"assistant"}}]}\n\n';
    const { deltas } = collect(frame);
    expect(deltas).toEqual([]);
  });

  it('解析 reasoning_content 增量', () => {
    const frame = 'data: {"choices":[{"delta":{"reasoning_content":"思考"}}]}\n\n';
    const { deltas } = collect(frame);
    expect(deltas).toEqual([{ content: '思考', isEnd: false, kind: 'reasoning' }]);
  });

  it('同一帧内 reasoning 与 content 分离为两条增量', () => {
    const frame =
      'data: {"choices":[{"delta":{"reasoning_content":"想","content":"答"}}]}\n\n';
    const { deltas } = collect(frame);
    expect(deltas).toEqual([
      { content: '想', isEnd: false, kind: 'reasoning' },
      { content: '答', isEnd: false, kind: 'content' },
    ]);
  });
});

describe('parseSseFrame — Anthropic SSE', () => {
  it('解析 Anthropic delta.text', () => {
    const frame = 'data: {"type":"content_block_delta","delta":{"type":"text_delta","text":"嗨"}}\n\n';
    const { deltas } = collect(frame);
    expect(deltas).toEqual([{ content: '嗨', isEnd: false }]);
  });

  it('message_stop 标记结束', () => {
    const frame = 'data: {"type":"message_stop"}\n\n';
    const { deltas, ended } = collect(frame);
    expect(deltas).toEqual([{ content: '', isEnd: true }]);
    expect(ended).toBe(true);
  });

  it('error 帧输出错误并结束', () => {
    const frame = 'data: {"type":"error","error":{"type":"overloaded_error","message":"过载"}}\n\n';
    const { deltas, ended } = collect(frame);
    // #63：错误帧带 kind='error'，ChatDebugger 据此以「⚠️」收尾而非拼正文
    expect(deltas).toEqual([{ content: '过载', isEnd: true, kind: 'error' }]);
    expect(ended).toBe(true);
  });
});

describe('parseSseFrame — 噪声', () => {
  it('忽略非 JSON 与注释帧', () => {
    const frame = 'data: 这不是JSON\n\n: keep-alive\n\n';
    const { deltas, ended } = collect(frame);
    expect(deltas).toEqual([]);
    expect(ended).toBe(false);
  });
});

describe('testChannelChatStream — 非 2xx 错误体', () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it('{error:{message}} 形态取出文案（旧实现把整段 JSON 当 message 抛给用户）', async () => {
    vi.spyOn(globalThis, 'fetch').mockResolvedValue(
      new Response(JSON.stringify({ error: { message: '上游不可用' } }), {
        status: 500,
        headers: { 'Content-Type': 'application/json' },
      }),
    );
    await expect(
      testChannelChatStream(
        { channel_id: '', protocol: 'openai', model: 'm', message: 'hi', history: [], stream: true },
        () => undefined,
      ),
    ).rejects.toThrow(/^上游不可用$/);
  });

  it('非 JSON 错误体保留原文', async () => {
    vi.spyOn(globalThis, 'fetch').mockResolvedValue(
      new Response('502 Bad Gateway', { status: 502, headers: { 'Content-Type': 'text/plain' } }),
    );
    await expect(
      testChannelChatStream(
        { channel_id: '', protocol: 'openai', model: 'm', message: 'hi', history: [], stream: true },
        () => undefined,
      ),
    ).rejects.toThrow(/502 Bad Gateway/);
  });
});

/* ─── 分帧 / 错误帧 / 终止帧（对齐 A2A v3.5.1 的解析口径）──────────
 * 以下每条都钉住旧实现的**具体丢失**，不是「跑绿即可」的形态断言。 */

describe('parseSseFrame — 多行 data 与 event 名', () => {
  it('多行 data 按规范拼接后解析（旧实现逐行解析 → 坏帧 → 正文丢失）', () => {
    const frame =
      'data: {"choices":[{"delta":{\n'
      + 'data: "content":"拼"\n'
      + 'data: }}]}\n\n';
    const { deltas } = collect(frame);
    expect(deltas).toEqual([{ content: '拼', isEnd: false, kind: 'content' }]);
  });

  it('event: error + JSON data → 错误增量并终止（旧实现只看 data 行，event 名被丢）', () => {
    const frame = 'event: error\ndata: {"error":{"message":"上游 502"}}\n\n';
    const { deltas, ended } = collect(frame);
    expect(deltas).toEqual([{ content: '上游 502', isEnd: true, kind: 'error' }]);
    expect(ended).toBe(true);
  });

  it('event: error + 非 JSON data → 仍按错误上报（旧实现静默忽略）', () => {
    const frame = 'event: error\ndata: 网关连接被重置\n\n';
    const { deltas, ended } = collect(frame);
    expect(deltas).toEqual([{ content: '网关连接被重置', isEnd: true, kind: 'error' }]);
    expect(ended).toBe(true);
  });

  it('终止帧之后的内容不再下发（旧实现继续拼进同一只气泡）', () => {
    const frame =
      'data: {"choices":[{"delta":{"content":"答"}}]}\n\n'
      + 'data: [DONE]\n\n'
      + 'data: {"choices":[{"delta":{"content":"尾巴"}}]}\n\n';
    const { deltas, ended } = collect(frame);
    expect(deltas.map((d) => d.kind)).toEqual(['content', undefined]);
    expect(deltas.map((d) => d.content)).toEqual(['答', '']);
    expect(ended).toBe(true);
  });

  it('非规范上游把三行 data 塞进一个事件块：回退解析也尊重终止帧', () => {
    const frame =
      'data: {"choices":[{"delta":{"content":"答"}}]}\n'
      + 'data: [DONE]\n'
      + 'data: {"choices":[{"delta":{"content":"尾巴"}}]}\n\n';
    const { deltas, ended } = collect(frame);
    expect(deltas.map((d) => d.content)).toEqual(['答', '']);
    expect(ended).toBe(true);
  });

  it('非规范上游（一块内多条独立 data）仍逐条解析 —— 兼容回退', () => {
    const frame =
      'data: {"choices":[{"delta":{"content":"你"}}]}\n'
      + 'data: {"choices":[{"delta":{"content":"好"}}]}\n\n';
    const { deltas } = collect(frame);
    expect(deltas).toEqual([
      { content: '你', isEnd: false, kind: 'content' },
      { content: '好', isEnd: false, kind: 'content' },
    ]);
  });
});

describe('readSseResponse — 非流式降级与生命周期', () => {
  it('非流式 JSON 错误体 → kind=error（旧实现不带 kind，ChatDebugger 直接丢弃该文本）', async () => {
    const deltas: ChatStreamDelta[] = [];
    await readSseResponse(
      fakeResponse(JSON.stringify({ data: { error: '上游鉴权失败' } }), 'application/json'),
      (d) => deltas.push(d),
    );
    expect(deltas).toEqual([{ content: '上游鉴权失败', isEnd: true, kind: 'error' }]);
  });

  it('非流式 JSON 正文 → 先正文增量再空收尾（旧实现把 content 挂在 isEnd 上，被 UI 丢掉）', async () => {
    const deltas: ChatStreamDelta[] = [];
    await readSseResponse(
      fakeResponse(JSON.stringify({ data: { content: '整段回复' } }), 'application/json'),
      (d) => deltas.push(d),
    );
    expect(deltas).toEqual([
      { content: '整段回复', isEnd: false, kind: 'content' },
      { content: '', isEnd: true },
    ]);
  });

  it('缺 [DONE] 的流（Trae）仍恰好收尾一次', async () => {
    const deltas: ChatStreamDelta[] = [];
    await readSseResponse(
      fakeResponse(
        chunkedResponse(['data: {"choices":[{"delta":{"content":"A"}}]}\n\n']),
        'text/event-stream',
      ),
      (d) => deltas.push(d),
    );
    expect(deltas.filter((d) => d.isEnd)).toHaveLength(1);
    expect(deltas.map((d) => d.content)).toEqual(['A', '']);
  });

  it('abort 时真断流：底层 cancel 被调用，且中止后不再回调', async () => {
    const controller = new AbortController();
    let cancelled = false;
    const deltas: ChatStreamDelta[] = [];
    let step = 0;
    const body = new ReadableStream<Uint8Array>({
      pull(c) {
        if (step === 0) {
          step += 1;
          c.enqueue(enc('data: {"choices":[{"delta":{"content":"A"}}]}\n\n'));
          return;
        }
        if (step === 1) {
          step += 1;
          controller.abort();
          c.enqueue(enc('data: {"choices":[{"delta":{"content":"B"}}]}\n\n'));
          return;
        }
        c.close();
      },
      cancel() { cancelled = true; },
    }, { highWaterMark: 0 });

    await expect(
      readSseResponse(fakeResponse(body, 'text/event-stream'), (d) => deltas.push(d), controller.signal),
    ).rejects.toThrow(/abort/i);
    expect(deltas.map((d) => d.content)).toEqual(['A']);
    expect(cancelled).toBe(true);
  });
});

describe('testChannelChatStream — 401 会话清理', () => {
  const API_BASE = '/api';
  const chatTestUrl = `${API_BASE}/channels/chat_test`;

  beforeEach(() => {
    localStorage.setItem('token', 'expired-token');
    localStorage.setItem('email', 'admin@aigx.local');
    localStorage.setItem('username', 'admin');
    localStorage.setItem('role', 'admin');
    localStorage.setItem('expires_at', String(Date.now() + 60000));
  });

  afterEach(() => {
    localStorage.clear();
    vi.restoreAllMocks();
  });

  it('401 时清理本地登录态（与 request() 对齐）', async () => {
    const fetchMock = vi.spyOn(globalThis, 'fetch').mockResolvedValue(
      new Response(JSON.stringify({ error: 'Session revoked' }), {
        status: 401,
        headers: { 'Content-Type': 'application/json' },
      }),
    );
    await expect(
      testChannelChatStream(
        { channel_id: '', protocol: 'openai', model: 'm', message: 'hi', history: [], stream: true },
        () => undefined,
      ),
    ).rejects.toThrow('Unauthorized');
    expect(fetchMock).toHaveBeenCalledWith(
      chatTestUrl,
      expect.objectContaining({ method: 'POST' }),
    );
    expect(localStorage.getItem('token')).toBeNull();
    expect(localStorage.getItem('username')).toBeNull();
  });
});
