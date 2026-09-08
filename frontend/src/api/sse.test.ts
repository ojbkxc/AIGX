import { describe, expect, it } from 'vitest';
import { parseSseFrame } from './index';
import type { ChatStreamDelta } from '../types';

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
    expect(deltas).toEqual([{ content: '过载', isEnd: true }]);
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
