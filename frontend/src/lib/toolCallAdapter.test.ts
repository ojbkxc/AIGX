import { describe, expect, it } from 'vitest';
import {
  extractToolCalls,
  extractUsage,
  mergeToolCallDelta,
  normalizeToolCall,
  normalizeToolCallList,
} from './toolCallAdapter';
import { parseSseFrame } from '../api';
import type { ChatStreamDelta } from '../types';

const collect = (frame: string): { deltas: ChatStreamDelta[]; ended: boolean } => {
  const deltas: ChatStreamDelta[] = [];
  const ended = parseSseFrame(frame, (d) => deltas.push(d));
  return { deltas, ended };
};

describe('toolCallAdapter — 归一化', () => {
  it('OpenAI 首帧：id/name/空 arguments 保留', () => {
    const tc = normalizeToolCall(
      { index: 0, id: 'call_1', type: 'function', function: { name: 'read_file', arguments: '' } },
      3,
    );
    expect(tc).toEqual({ index: 0, id: 'call_1', name: 'read_file', arguments: '' });
  });

  it('后续分片帧只有 arguments（name/id 缺省不产生空字段）', () => {
    const tc = normalizeToolCall({ index: 1, function: { arguments: '{"a"' } }, 0);
    expect(tc).toEqual({ index: 1, arguments: '{"a"' });
  });

  it('回灌层扁平形状 + result/output/content 三种结果字段', () => {
    expect(normalizeToolCall({ tool_call_id: 't1', name: 'x', result: 'ok' }, 0))
      .toEqual({ index: 0, id: 't1', name: 'x', result: 'ok' });
    expect(normalizeToolCall({ id: 't2', name: 'y', output: 'done' }, 0)?.result).toBe('done');
    expect(normalizeToolCall({ role: 'tool', content: 'res' }, 0))
      .toEqual({ index: 0, result: 'res' });
  });

  it('无任何字段的对象不当作工具帧（心跳等噪声）', () => {
    expect(normalizeToolCall({}, 0)).toBeNull();
    expect(normalizeToolCall(null, 0)).toBeNull();
    expect(normalizeToolCallList({ not: 'array' })).toEqual([]);
  });
});

describe('toolCallAdapter — 帧提取', () => {
  it('流式 delta.tool_calls 与 message.tool_calls 都识别', () => {
    expect(extractToolCalls({
      choices: [{ delta: { tool_calls: [{ index: 0, id: 'c1', function: { name: 'f', arguments: '{}' } }] } }],
    })).toEqual([{ index: 0, id: 'c1', name: 'f', arguments: '{}' }]);
    expect(extractToolCalls({
      choices: [{ message: { tool_calls: [{ id: 'c2', function: { name: 'g', arguments: '{"x":1}' } }] } }],
    })).toEqual([{ index: 0, id: 'c2', name: 'g', arguments: '{"x":1}' }]);
  });

  it('role_tool 模式识别顶层 tool_calls / tool_call / role:tool 帧', () => {
    expect(extractToolCalls(
      { tool_calls: [{ id: 'a', function: { name: 'n', arguments: '{}' } }] },
      'role_tool',
    )).toEqual([{ index: 0, id: 'a', name: 'n', arguments: '{}' }]);
    expect(extractToolCalls({ tool_call: { id: 'b', name: 'm' } }, 'role_tool'))
      .toEqual([{ index: 0, id: 'b', name: 'm' }]);
    expect(extractToolCalls({ role: 'tool', tool_call_id: 'c', content: 'r' }, 'role_tool'))
      .toEqual([{ index: 0, id: 'c', result: 'r' }]);
  });

  it('模式互斥：openai_delta 模式忽略回灌层形状', () => {
    expect(extractToolCalls({ tool_calls: [{ id: 'a', function: { name: 'n' } }] }, 'openai_delta'))
      .toEqual([]);
  });
});

describe('toolCallAdapter — usage 提取', () => {
  it('OpenAI include_usage 终帧（choices 空数组）', () => {
    expect(extractUsage({ choices: [], usage: { prompt_tokens: 10, completion_tokens: 5, total_tokens: 15 } }))
      .toEqual({ prompt_tokens: 10, completion_tokens: 5, total_tokens: 15 });
  });

  it('中转包一层 data.usage 也识别', () => {
    expect(extractUsage({ data: { usage: { total_tokens: 7 } } })).toEqual({ total_tokens: 7 });
  });

  it('无 usage / 非数值字段返回 null（不产生假 0）', () => {
    expect(extractUsage({ choices: [{ delta: { content: 'x' } }] })).toBeNull();
    expect(extractUsage({ usage: { prompt_tokens: 'ten' } })).toBeNull();
    expect(extractUsage({ usage: {} })).toBeNull();
  });
});

describe('mergeToolCallDelta — 分片聚合', () => {
  it('同 index 拼 arguments，id/name 只取首帧，结果覆盖式更新', () => {
    let list = mergeToolCallDelta([], { index: 0, id: 'c1', name: 'read', arguments: '' });
    list = mergeToolCallDelta(list, { index: 0, arguments: '{"path":' });
    list = mergeToolCallDelta(list, { index: 0, arguments: '"a.txt"}' });
    expect(list).toEqual([{ index: 0, id: 'c1', name: 'read', arguments: '{"path":"a.txt"}' }]);
    list = mergeToolCallDelta(list, { index: 0, result: 'ok' });
    list = mergeToolCallDelta(list, { index: 0, result: 'final' });
    expect(list[0].result).toBe('final');
    expect(list[0].arguments).toBe('{"path":"a.txt"}');
  });

  it('多个 index 按出现顺序追加，互不干扰', () => {
    let list = mergeToolCallDelta([], { index: 1, name: 'b', arguments: '{"b":1}' });
    list = mergeToolCallDelta(list, { index: 0, name: 'a', arguments: '{"a":1}' });
    expect(list.map((t) => t.index)).toEqual([1, 0]);
    expect(list.map((t) => t.name)).toEqual(['b', 'a']);
  });

  it('纯函数：不修改入参', () => {
    const prev = [{ index: 0, arguments: 'x' }];
    const next = mergeToolCallDelta(prev, { index: 0, arguments: 'y' });
    expect(prev[0].arguments).toBe('x');
    expect(next[0].arguments).toBe('xy');
  });
});

describe('parseSseFrame — 工具调用 / 用量帧（#63 契约）', () => {
  it('OpenAI tool_calls 分片输出 kind=tool_call 增量', () => {
    const frame =
      'data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"f","arguments":"{\\"q\\":"}}]}}]}\n\n';
    const { deltas } = collect(frame);
    expect(deltas).toHaveLength(1);
    expect(deltas[0]).toMatchObject({
      isEnd: false,
      kind: 'tool_call',
      toolCall: { index: 0, id: 'c1', name: 'f', arguments: '{"q":' },
    });
  });

  it('[DONE] 前的 usage 帧单独输出，不与 content 混排', () => {
    const frame =
      'data: {"choices":[{"delta":{"content":"答"}}]}\n' +
      'data: {"choices":[],"usage":{"prompt_tokens":3,"completion_tokens":2,"total_tokens":5}}\n\n';
    const { deltas } = collect(frame);
    expect(deltas.map((d) => d.kind)).toEqual(['content', 'usage']);
    expect(deltas[1].usage).toEqual({ prompt_tokens: 3, completion_tokens: 2, total_tokens: 5 });
  });

  it('[DONE] 仍正常收尾', () => {
    const { deltas, ended } = collect('data: [DONE]\n\n');
    expect(ended).toBe(true);
    expect(deltas).toEqual([{ content: '', isEnd: true }]);
  });
});