/**
 * 对话调试器流式状态机（debug-stream-state）—— 逐条复现修掉的三个缺陷。
 *
 * 这三条都是「界面看起来在工作，数据已经错了」的形态，只有把帧序列喂给
 * 纯函数才能稳定复现；测渲染只能看到气泡文字，看不出 isEnd 帧被整帧丢弃、
 * 也看不出 stalled 之后再也回不到生成态。
 */
import { describe, expect, it } from 'vitest';
import type { ChatStreamDelta } from '../../types';
import type { DebugMessage } from './types';
import {
  applyStreamDelta,
  applyStreamError,
  finishStream,
  markInterrupted,
  nextStreamPhaseOnDelta,
  seedStreamPhase,
  STREAM_PLACEHOLDER,
  tickStreamPhase,
} from './debug-stream-state';

function assistant(content: string): DebugMessage {
  return { role: 'assistant', content };
}

function delta(p: Partial<ChatStreamDelta> & { content: string }): ChatStreamDelta {
  return { isEnd: false, ...p };
}

describe('applyStreamDelta — isEnd 帧的正文不能被丢', () => {
  /**
   * 复现的原缺陷：`patch.content = delta.isEnd ? base : base + delta.content`。
   * 非流式降级 / 直连上游时整段正文放在 isEnd 帧里（api/stream.ts 的
   * `onDelta({ content, isEnd: true })`），旧实现整段丢弃 → 界面只剩
   * 「上游未返回内容」。
   */
  it('isEnd 帧带正文时追加到气泡（旧实现丢弃 → 空气泡）', () => {
    const before = [assistant(STREAM_PLACEHOLDER)];
    const after = applyStreamDelta(before, delta({ content: '完整回答', isEnd: true }));
    expect(after[after.length - 1].content).toBe('完整回答');
  });

  it('普通增量逐片追加，占位「…」在首帧被清掉', () => {
    let msgs = [assistant(STREAM_PLACEHOLDER)];
    msgs = applyStreamDelta(msgs, delta({ content: '你', kind: 'content' }));
    msgs = applyStreamDelta(msgs, delta({ content: '好', kind: 'content' }));
    expect(msgs[msgs.length - 1].content).toBe('你好');
  });

  it('isEnd 空帧（[DONE]）不改变正文，也不新增气泡', () => {
    const before = [assistant('已生成')];
    const after = applyStreamDelta(before, delta({ content: '', isEnd: true }));
    expect(after).toHaveLength(1);
    expect(after[0].content).toBe('已生成');
  });

  it('reasoning 帧进 reasoning，正文保持占位', () => {
    const after = applyStreamDelta([assistant(STREAM_PLACEHOLDER)], delta({ content: '想一想', kind: 'reasoning' }));
    expect(after[0].reasoning).toBe('想一想');
    expect(after[0].content).toBe(STREAM_PLACEHOLDER);
  });

  it('tool_call 分片按 index 聚合，走 mergeToolCallDelta 单点（不复制第二套拼接）', () => {
    let msgs = [assistant(STREAM_PLACEHOLDER)];
    msgs = applyStreamDelta(msgs, delta({
      content: '', kind: 'tool_call',
      toolCall: { index: 0, id: 'call_1', name: 'list_users', arguments: '{"li' },
    }));
    msgs = applyStreamDelta(msgs, delta({
      content: '', kind: 'tool_call',
      toolCall: { index: 0, arguments: 'mit":5}' },
    }));
    const calls = msgs[0].toolCalls ?? [];
    expect(calls).toHaveLength(1);
    expect(calls[0].name).toBe('list_users');
    expect(calls[0].arguments).toBe('{"limit":5}');
    expect(msgs[0].toolState).toBe('streaming');
  });

  it('usage 帧覆盖式写入，不产生气泡内容', () => {
    const after = applyStreamDelta([assistant('正文')], delta({
      content: '', kind: 'usage', usage: { prompt_tokens: 3, completion_tokens: 5 },
    }));
    expect(after[0].usage).toEqual({ prompt_tokens: 3, completion_tokens: 5 });
    expect(after[0].content).toBe('正文');
  });

  it('error 帧保留已流出正文，追加「⚠️」尾巴', () => {
    const after = applyStreamDelta([assistant('前半段')], delta({ content: '上游炸了', kind: 'error', isEnd: true }));
    expect(after[0].content).toBe('前半段\n\n⚠️ 上游炸了');
  });

  it('末尾不是 assistant 时原样返回（不替调用方决定 UI 形状）', () => {
    const before: DebugMessage[] = [{ role: 'user', content: '你好' }];
    expect(applyStreamDelta(before, delta({ content: 'x' }))).toBe(before);
  });
});

describe('finishStream', () => {
  it('有 toolCalls 时切 done', () => {
    const msgs: DebugMessage[] = [{
      role: 'assistant', content: '调完了',
      toolCalls: [{ index: 0, arguments: '{}' }], toolState: 'streaming',
    }];
    expect(finishStream(msgs, '上游未返回内容')[0].toolState).toBe('done');
  });

  it('空流（只剩占位）给兜底提示，不留永久空白气泡', () => {
    const after = finishStream([assistant(STREAM_PLACEHOLDER)], '上游未返回内容');
    expect(after[0].content).toBe('⚠️ 上游未返回内容');
  });

  it('有 reasoning 但无正文时不覆盖成警告（思考过就算有输出）', () => {
    const msgs: DebugMessage[] = [{ role: 'assistant', content: STREAM_PLACEHOLDER, reasoning: '想了' }];
    expect(finishStream(msgs, '上游未返回内容')[0].content).toBe(STREAM_PLACEHOLDER);
  });
});

describe('markInterrupted / applyStreamError', () => {
  it('停止时同一只气泡标 interrupted，占位清空', () => {
    const after = markInterrupted([assistant(STREAM_PLACEHOLDER)], '已停止生成');
    expect(after).toHaveLength(1);
    expect(after[0].content).toBe('');
    expect(after[0].toolState).toBe('interrupted');
  });

  it('已流出正文时停止不吞正文', () => {
    const after = markInterrupted([assistant('写到一半')], '已停止生成');
    expect(after[0].content).toBe('写到一半');
    expect(after[0].toolState).toBe('interrupted');
  });

  it('末尾没有 assistant 气泡时新起一条「⏹」', () => {
    const after = markInterrupted([{ role: 'user', content: '问' }], '已停止生成');
    expect(after).toHaveLength(2);
    expect(after[1].content).toBe('⏹ 已停止生成');
  });

  it('失败时替换占位而不是叠一条（一次失败只留一条可见错误）', () => {
    const after = applyStreamError([assistant(STREAM_PLACEHOLDER)], '连接中断');
    expect(after).toHaveLength(1);
    expect(after[0].content).toBe('⚠️ 连接中断');
  });

  it('已收到工具分片时标错误态（保留参数供排查）', () => {
    const msgs: DebugMessage[] = [{
      role: 'assistant', content: '正文',
      toolCalls: [{ index: 0, arguments: '{"a":1}' }], toolState: 'streaming',
    }];
    const after = applyStreamError(msgs, '连接中断');
    expect(after[0].toolState).toBe('error');
    expect(after[0].toolCalls).toHaveLength(1);
  });
});

describe('停滞阶段 — stalled 必须能回到 generating', () => {
  /**
   * 复现的原缺陷：`onDelta` 里 `else if (streamPhase === 'stalled')` 读的是
   * 闭包捕获值（首帧渲染时的 'thinking'），条件恒假，标红后永不恢复。
   */
  it('收到任意增量即回 generating（旧实现读闭包，永不恢复）', () => {
    expect(nextStreamPhaseOnDelta()).toBe('generating');
  });

  it('thinking 阶段不计停滞（首帧本来就慢）', () => {
    expect(tickStreamPhase('thinking', 0, 60_000)).toBe('thinking');
  });

  it('超过阈值无增量 → stalled；阈值内保持 generating', () => {
    expect(tickStreamPhase('generating', 1000, 1000 + 3001)).toBe('stalled');
    expect(tickStreamPhase('generating', 1000, 1000 + 2999)).toBe('generating');
  });

  it('stalled 之后再来增量（时间戳更新）→ 回 generating', () => {
    const now = 10_000;
    expect(tickStreamPhase('stalled', now - 100, now)).toBe('generating');
  });

  it('还没有任何增量（lastDeltaAt=0）时不误报停滞', () => {
    expect(tickStreamPhase('generating', 0, 999_999)).toBe('generating');
  });
});

describe('seedStreamPhase — 新请求不能继承上一轮的时间戳', () => {
  /**
   * 复现的原缺陷（宿主页 `pages/Agent.tsx` + 共享 `components/chat/StreamPhase`）：
   * `lastEventAt` 只在收到 SSE 事件时更新，发送时不重置。上一轮结束 5 分钟后再发，
   * 停滞判定拿旧时间戳算 → 新请求一开始就显示「响应似乎停滞了」。
   */
  it('新请求的阶段是 thinking，时间戳是「现在」', () => {
    const now = 1_700_000_000_000;
    expect(seedStreamPhase(now)).toEqual({ phase: 'thinking', lastEventAt: now });
  });

  it('用种子时间戳算停滞：刚发出不应判停滞（旧时间戳会）', () => {
    const now = 1_700_000_000_000;
    const seed = seedStreamPhase(now);
    // 发出后 1s 无事件：按种子算不 stalled；按「上一轮 5 分钟前」算会 stalled
    expect(tickStreamPhase('generating', seed.lastEventAt, now + 1_000)).toBe('generating');
    expect(tickStreamPhase('generating', now - 300_000, now + 1_000)).toBe('stalled');
  });
});
