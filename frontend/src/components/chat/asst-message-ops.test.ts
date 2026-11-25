import { describe, expect, it } from 'vitest';
import { deleteMentionsTools, editAnchor, messageActions, regenerateAnchor } from './asst-message-ops';

describe('asst-message-ops — 用户消息', () => {
  it('可编辑可删；不给重生成与分叉（分叉只对助手消息）', () => {
    const actions = messageActions({ role: 'user', content: 'hi', isLast: false });
    expect(actions.edit).toBe(true);
    expect(actions.remove).toBe(true);
    expect(actions.copy).toBe(true);
    expect(actions.regenerate).toBe(false);
    expect(actions.fork).toBe(false);
    expect(actions.blocked.regenerate).toBe('role');
    expect(actions.blocked.fork).toBe('role');
  });
});

describe('asst-message-ops — 助手消息', () => {
  it('最后一条且有正文才能重生成', () => {
    expect(messageActions({ role: 'assistant', content: 'ok', isLast: true }).regenerate).toBe(true);
    expect(messageActions({ role: 'assistant', content: 'ok', isLast: false }).regenerate).toBe(false);
    expect(messageActions({ role: 'assistant', content: 'ok', isLast: false }).blocked.regenerate).toBe('not-last');
  });

  it('空回复不给复制/重生成/分叉（占位气泡不该能点出一次新请求）', () => {
    const actions = messageActions({ role: 'assistant', content: '   ', isLast: true });
    expect(actions.copy).toBe(false);
    expect(actions.regenerate).toBe(false);
    expect(actions.fork).toBe(false);
    expect(actions.blocked.copy).toBe('empty');
    expect(actions.blocked.regenerate).toBe('empty');
  });

  it('流式中一律不可操作（避免编辑把流式回灌写回错位置）', () => {
    const actions = messageActions({ role: 'assistant', content: '正在生成', isLast: true, streaming: true });
    expect(actions).toMatchObject({
      copy: false, edit: false, regenerate: false, remove: false, fork: false, speak: false, source: false,
    });
    expect(actions.blocked.remove).toBe('streaming');
  });

  it('会话忙碌时编辑/删除/重生成/分叉都不可点', () => {
    const actions = messageActions({ role: 'assistant', content: 'ok', isLast: true, busy: true });
    expect(actions.regenerate).toBe(false);
    expect(actions.remove).toBe(false);
    expect(actions.fork).toBe(false);
    expect(actions.copy).toBe(true); // 复制只依赖内容
    expect(actions.blocked.regenerate).toBe('busy');
  });

  it('朗读：正在播时不可点（同一时刻只播一条）', () => {
    expect(messageActions({ role: 'assistant', content: 'ok', isLast: true, speaking: true }).speak).toBe(false);
    expect(messageActions({ role: 'assistant', content: 'ok', isLast: true }).speak).toBe(true);
  });
});

describe('asst-message-ops — 边界', () => {
  it('删除提示只在带工具调用时才提「工具结果一并删除」', () => {
    expect(deleteMentionsTools({ hasToolCalls: true })).toBe(true);
    expect(deleteMentionsTools({ hasToolCalls: false })).toBe(false);
    expect(deleteMentionsTools({})).toBe(false);
  });

  it('重生成锚点：最后一条必须是带正文的助手消息', () => {
    const messages = [
      { role: 'user', content: 'q' },
      { role: 'assistant', content: 'a' },
    ];
    expect(regenerateAnchor(messages)).toBe(1);
    expect(regenerateAnchor([{ role: 'assistant', content: 'a' }, { role: 'user', content: 'q' }])).toBe(-1);
    expect(regenerateAnchor([{ role: 'user', content: 'q' }])).toBe(-1);
    expect(regenerateAnchor([{ role: 'assistant', content: '' }])).toBe(-1);
    expect(regenerateAnchor(messages, { busy: true })).toBe(-1);
    expect(regenerateAnchor([])).toBe(-1);
  });

  it('编辑重发：只认用户消息，并给出要丢弃的尾部条数', () => {
    const messages = [
      { role: 'user' },
      { role: 'assistant' },
      { role: 'user' },
      { role: 'assistant' },
    ];
    expect(editAnchor(messages, 2)).toEqual({ ok: true, keep: 2, drop: 2 });
    expect(editAnchor(messages, 1)).toEqual({ ok: false, keep: 0, drop: 0 });
    expect(editAnchor(messages, 9)).toEqual({ ok: false, keep: 0, drop: 0 });
  });
});
