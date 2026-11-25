import { afterEach, describe, expect, it, vi } from 'vitest';
import { api } from './index';

/**
 * AI 运维（/agent）SSE 读取层的缺陷回归。
 *
 * `agentChatStream` 是「AI 运维」页唯一的事件通道：后端按 `data: {AgentEvent}\n\n`
 * 逐事件下发，空闲期注入 `: keepalive` 注释行，且**不发 `[DONE]`**。
 * 旧实现自己写了一遍 `reader.read()` + `indexOf('\n\n')` + 逐行 `data:` 解析，
 * 与对话调试那条链各自丢数据（见下面每条用例的名字）。
 */

const enc = (text: string): Uint8Array => new TextEncoder().encode(text);

function streamOf(chunks: string[], hooks: { onCancel?: () => void } = {}): ReadableStream<Uint8Array> {
  let i = 0;
  return new ReadableStream<Uint8Array>({
    pull(c) {
      if (i < chunks.length) c.enqueue(enc(chunks[i++]));
      else c.close();
    },
    cancel() { hooks.onCancel?.(); },
  });
}

function responseOf(body: ReadableStream<Uint8Array> | string, init: { ok?: boolean; status?: number } = {}): Response {
  const ok = init.ok ?? true;
  const stream = typeof body === 'string'
    ? new ReadableStream<Uint8Array>({
      start(c) {
        c.enqueue(enc(body));
        c.close();
      },
    })
    : body;
  return {
    ok,
    status: init.status ?? 200,
    headers: new Headers({ 'Content-Type': 'text/event-stream; charset=utf-8' }),
    body: ok ? stream : null,
    text: async () => (typeof body === 'string' ? body : ''),
  } as unknown as Response;
}

type Ev = { type: string; content?: string; message?: string; request_id?: string; approved?: boolean };

afterEach(() => {
  vi.restoreAllMocks();
});

describe('agentChatStream — 分帧与生命周期', () => {
  it('正常事件逐条回调', async () => {
    vi.spyOn(globalThis, 'fetch').mockResolvedValue(responseOf(streamOf([
      'data: {"type":"final","content":"你好"}\n\n',
      'data: {"type":"tool_call","name":"list_models"}\n\n',
    ])));
    const events: Ev[] = [];
    await api.agentChatStream('s1', 'hi', (ev) => events.push(ev as Ev));
    expect(events.map((e) => e.type)).toEqual(['final', 'tool_call']);
  });

  it('尾帧缺空行（连接被中途关闭）也要回调，不能整条丢', async () => {
    vi.spyOn(globalThis, 'fetch').mockResolvedValue(responseOf(streamOf([
      'data: {"type":"final","content":"A"}\n\n',
      'data: {"type":"final","content":"B"}',
    ])));
    const events: Ev[] = [];
    await api.agentChatStream('s1', 'hi', (ev) => events.push(ev as Ev));
    expect(events.map((e) => e.content)).toEqual(['A', 'B']);
  });

  it('event: error 帧按错误事件上报（旧实现只认 data 行，event 名被丢）', async () => {
    vi.spyOn(globalThis, 'fetch').mockResolvedValue(responseOf(streamOf([
      'event: error\ndata: 上游连接被重置\n\n',
    ])));
    const events: Ev[] = [];
    await api.agentChatStream('s1', 'hi', (ev) => events.push(ev as Ev));
    expect(events).toEqual([{ type: 'error', message: '上游连接被重置' }]);
  });

  it('注释行 keepalive 不产生事件', async () => {
    vi.spyOn(globalThis, 'fetch').mockResolvedValue(responseOf(streamOf([
      ': keepalive\n\n',
      'data: {"type":"final","content":"X"}\n\n',
    ])));
    const events: Ev[] = [];
    await api.agentChatStream('s1', 'hi', (ev) => events.push(ev as Ev));
    expect(events).toHaveLength(1);
  });

  it('abort 后不再回调，且真断流（底层 cancel 被调用）', async () => {
    const controller = new AbortController();
    let cancelled = false;
    let step = 0;
    const body = new ReadableStream<Uint8Array>({
      pull(c) {
        if (step === 0) {
          step += 1;
          c.enqueue(enc('data: {"type":"final","content":"A"}\n\n'));
          return;
        }
        if (step === 1) {
          step += 1;
          controller.abort();
          c.enqueue(enc('data: {"type":"final","content":"B"}\n\n'));
          return;
        }
        c.close();
      },
      cancel() { cancelled = true; },
    }, { highWaterMark: 0 });
    vi.spyOn(globalThis, 'fetch').mockResolvedValue(responseOf(body));

    const events: Ev[] = [];
    await expect(
      api.agentChatStream('s1', 'hi', (ev) => events.push(ev as Ev), controller.signal),
    ).rejects.toThrow(/abort/i);
    expect(events.map((e) => e.content)).toEqual(['A']);
    expect(cancelled).toBe(true);
  });

  it('非 2xx 的 JSON 错误体：抛出可读文案而不是原始 JSON（旧实现把整个 body 当 message）', async () => {
    vi.spyOn(globalThis, 'fetch').mockResolvedValue(
      responseOf(JSON.stringify({ error: '上游鉴权失败' }), { ok: false, status: 401 }),
    );
    await expect(api.agentChatStream('s1', 'hi', () => undefined))
      .rejects.toThrow(/^上游鉴权失败$/);
  });

  it('非 2xx 且错误体是嵌套 error.message 时也取到文案', async () => {
    vi.spyOn(globalThis, 'fetch').mockResolvedValue(
      responseOf(JSON.stringify({ error: { message: '会话不存在' } }), { ok: false, status: 404 }),
    );
    await expect(api.agentChatStream('s1', 'hi', () => undefined))
      .rejects.toThrow(/^会话不存在$/);
  });

  it('非 2xx 且无错误体：退回状态码文案', async () => {
    vi.spyOn(globalThis, 'fetch').mockResolvedValue(responseOf('', { ok: false, status: 502 }));
    await expect(api.agentChatStream('s1', 'hi', () => undefined))
      .rejects.toThrow('502');
  });
});
