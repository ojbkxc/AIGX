/**
 * 非流式对话响应归一化（debug-chunk）—— 复现两处崩溃路径。
 *
 * 原实现（ChatDebugger.handleSend 非流式分支）：
 *   const res = (await api.testChannelChat(body)) as unknown as ChatChunkResult;
 *   const data = res.data || {};
 *   if (res.stream && res.stream.length) { res.stream.map(...) }
 *
 * 1. `api/client.ts` 的 `request()` 在 **HTTP 204** 时返回 `null` → `res.data`
 *    当场 TypeError，整次发送掉进 catch，用户看到的是内部错误串而不是正常空结果。
 * 2. `res.stream` 不是数组时（上游把 SSE 文本原样塞回/老后端给对象）`.map` 抛
 *    「res.stream.map is not a function」。
 * 3. `data.content` 非字符串时拼进消息，渲染层再炸一次。
 */
import { describe, expect, it } from 'vitest';
import { normalizeChatChunk } from './debug-chunk';

describe('normalizeChatChunk', () => {
  it('null（HTTP 204 时 request() 的返回值）不炸，返回空结果', () => {
    expect(normalizeChatChunk(null)).toEqual({ content: '', error: '', fromStreamArray: false });
  });

  it('undefined / 非对象（字符串裸响应）也不炸', () => {
    expect(normalizeChatChunk(undefined).content).toBe('');
    expect(normalizeChatChunk('raw text').content).toBe('');
  });

  it('stream 数组按顺序拼接', () => {
    const r = normalizeChatChunk({ stream: [{ content: '你' }, { content: '好' }] });
    expect(r.content).toBe('你好');
    expect(r.fromStreamArray).toBe(true);
  });

  it('stream 数组里的空/畸形条目被跳过（不产出 undefined 字样）', () => {
    const r = normalizeChatChunk({ stream: [{ content: 'a' }, {}, null, 'b', { content: 42 }] });
    expect(r.content).toBe('ab');
  });

  it('stream 不是数组时回落 data.content（旧实现在这里 .map 抛错）', () => {
    const r = normalizeChatChunk({ stream: 'data: {...}\n\n', data: { content: '正文' } });
    expect(r.content).toBe('正文');
    expect(r.fromStreamArray).toBe(false);
  });

  it('stream 为空数组时回落 data.content', () => {
    expect(normalizeChatChunk({ stream: [], data: { content: '正文' } }).content).toBe('正文');
  });

  it('data.content 非字符串不采纳（否则渲染层再炸）', () => {
    expect(normalizeChatChunk({ data: { content: { text: 'x' } } }).content).toBe('');
  });

  it('data.error 优先于顶层 error（与原实现分支顺序一致）', () => {
    expect(normalizeChatChunk({ data: { error: '内层' }, error: '外层' }).error).toBe('内层');
  });

  it('只有顶层 error 时也能取出', () => {
    expect(normalizeChatChunk({ error: '上游 500' }).error).toBe('上游 500');
  });

  it('data 为 null 时不炸（res.data || {} 的等价语义）', () => {
    expect(normalizeChatChunk({ data: null }).content).toBe('');
  });

  it('什么都给不出来时返回空结果，由调用方兜底提示', () => {
    expect(normalizeChatChunk({})).toEqual({ content: '', error: '', fromStreamArray: false });
  });
});
