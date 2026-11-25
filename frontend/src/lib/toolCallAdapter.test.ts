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

  // #24：#21-B 合成尾帧的口径标记挂在帧顶层（与 usage 同级），必须透传出来供 UI 画角标
  it('estimated 透传：顶层键 true → 结果带 estimated:true', () => {
    expect(extractUsage({
      choices: [],
      estimated: true,
      usage: { prompt_tokens: 4, completion_tokens: 3, total_tokens: 7 },
    })).toEqual({ prompt_tokens: 4, completion_tokens: 3, total_tokens: 7, estimated: true });
  });

  it('estimated 区分口径：上游真值帧不写该键（不是 false）', () => {
    const truth = extractUsage({ choices: [], usage: { total_tokens: 9 } });
    expect(truth).toEqual({ total_tokens: 9 });
    expect('estimated' in (truth ?? {})).toBe(false);
    // 后端只在估算场景写 true，误写 false / 字符串同样不落
    expect(extractUsage({ choices: [], estimated: false, usage: { total_tokens: 9 } }))
      .toEqual({ total_tokens: 9 });
    expect(extractUsage({ choices: [], estimated: 'true', usage: { total_tokens: 9 } }))
      .toEqual({ total_tokens: 9 });
  });

  it('estimated 嵌在 usage 内（形状分歧）不落——只认顶层键', () => {
    expect(extractUsage({ choices: [], usage: { total_tokens: 9, estimated: true } }))
      .toEqual({ total_tokens: 9 });
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

  /* ── 边界：同一 id 多次 delta / 上游重写 index ───────────────────
   * 两条都钉住旧实现的「把两次调用拼成一张卡」：参数被粘成
   * `{"x":1}{"y":2}` 这种不可解析的串，用户看到一张参数坏掉的卡片。 */

  it('同 index 但 id 不同 = 另一次调用，不能拼进同一张卡', () => {
    let list = mergeToolCallDelta([], { index: 0, id: 'c1', name: 'read_file', arguments: '{"a":1}' });
    list = mergeToolCallDelta(list, { index: 0, id: 'c2', name: 'list_dir', arguments: '{"b":2}' });
    expect(list).toHaveLength(2);
    expect(list.map((t) => t.id)).toEqual(['c1', 'c2']);
    expect(list.map((t) => t.arguments)).toEqual(['{"a":1}', '{"b":2}']);
    // 新调用拿到未被占用的 index，且不覆盖原有那张卡
    expect(list.map((t) => t.index)).toEqual([0, 1]);
  });

  it('同一 id 的后到帧 index 更小：按 id 归并，不新起一张卡', () => {
    let list = mergeToolCallDelta([], { index: 2, id: 'c9', name: 'write', arguments: '{"a' });
    list = mergeToolCallDelta(list, { index: 0, id: 'c9', arguments: '":1}' });
    expect(list).toHaveLength(1);
    expect(list[0].index).toBe(2);
    expect(list[0].arguments).toBe('{"a":1}');
  });

  it('省略 index 的多次调用（上游每帧一条、无 index）：按 id 分开', () => {
    // normalizeToolCall 对缺失 index 用数组位置回退 → 每帧都是 0，旧实现会把
    // 两个不同 id 的调用合成一张卡。
    let list = mergeToolCallDelta([], { index: 0, id: 'c1', name: 'a', arguments: '{}' });
    list = mergeToolCallDelta(list, { index: 0, id: 'c2', name: 'b', arguments: '{}' });
    expect(list.map((t) => t.name)).toEqual(['a', 'b']);
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