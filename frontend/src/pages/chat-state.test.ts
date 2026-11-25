import { afterEach, describe, expect, it, vi } from 'vitest';
import type { AssistantConfig } from './chat-state';
import {
  DEFAULT_SYSTEM_PROMPT,
  buildChatBody,
  isChatStreamInFlight,
  streamChat,
  toWireMessages,
  type ChatMessage,
} from './chat-state';

const baseConfig: AssistantConfig = {
  model: 'gpt-4o-mini',
  reasoning: null,
  titleModel: '',
  titlePrompt: '',
  mode: 'confirm',
  systemPromptEnabled: false,
  systemPrompt: DEFAULT_SYSTEM_PROMPT,
  tools: {},
  apiKeyId: null,
};

afterEach(() => {
  localStorage.clear();
  vi.restoreAllMocks();
});

describe('toWireMessages — 工具回灌', () => {
  it('role=tool 带出 tool_call_id（旧实现写死空串，上游无法配对）', () => {
    const wire = toWireMessages([
      { role: 'tool', content: '结果', toolCallId: 'call_1' },
    ]);
    expect(wire).toEqual([{ role: 'tool', tool_call_id: 'call_1', content: '结果' }]);
  });

  it('assistant 带 toolCalls 时转成 OpenAI 的 tool_calls 形状', () => {
    const wire = toWireMessages([
      { role: 'assistant', content: '', toolCalls: [{ id: 'c1', name: 'read', arguments: '{}' }] },
    ]);
    expect(wire[0]).toEqual({
      role: 'assistant',
      content: '',
      tool_calls: [{ id: 'c1', type: 'function', function: { name: 'read', arguments: '{}' } }],
    });
  });
});

describe('buildChatBody — 本轮消息与历史不重叠', () => {
  const messages: ChatMessage[] = [
    { role: 'user', content: '第一问' },
    { role: 'assistant', content: '第一答' },
    { role: 'user', content: '第二问' },
  ];

  it('message 取最后一条用户消息，history 只含它之前（旧实现 message 恒为空串）', () => {
    const body = buildChatBody(messages, baseConfig);
    expect(body.message).toBe('第二问');
    expect(body.history).toEqual([
      { role: 'user', content: '第一问' },
      { role: 'assistant', content: '第一答' },
    ]);
  });

  it('空文本直接拒绝，不发注定 4001 的请求', () => {
    expect(() => buildChatBody([{ role: 'user', content: '   ' }], baseConfig))
      .toThrow(/当前消息为空/);
  });

  it('带图消息走多模态数组（不是被空串顶掉）', () => {
    const body = buildChatBody([
      { role: 'user', content: '看图', images: ['data:image/png;base64,AAA'] },
    ], baseConfig);
    expect(Array.isArray(body.message)).toBe(true);
    expect((body.message as Array<Record<string, unknown>>).map((p) => p.type)).toEqual(['text', 'image_url']);
  });

  it('systemPromptEnabled 时带 system_prompt，关闭时不带', () => {
    expect(buildChatBody(messages, baseConfig).system_prompt).toBeUndefined();
    const withPrompt = buildChatBody(messages, { ...baseConfig, systemPromptEnabled: true, systemPrompt: ' 你是助手 ' });
    expect(withPrompt.system_prompt).toBe('你是助手');
  });
});

describe('streamChat — 单飞在途闸', () => {
  const body = { channel_id: '', protocol: 'openai', model: 'm', message: 'hi', history: [], stream: true };

  it('第一条在途时第二条直接拒绝，且不再发第二个请求', async () => {
    let release: (() => void) | null = null;
    const gate = new Promise<void>((resolve) => { release = resolve; });
    const fetchMock = vi.spyOn(globalThis, 'fetch').mockImplementation(async () => {
      await gate;
      return new Response('data: [DONE]\n\n', { headers: { 'Content-Type': 'text/event-stream' } });
    });

    const first = streamChat(body, () => undefined);
    expect(isChatStreamInFlight()).toBe(true);
    await expect(streamChat(body, () => undefined)).rejects.toThrow(/仍在生成/);
    expect(fetchMock).toHaveBeenCalledTimes(1);

    release!();
    await first;
    expect(isChatStreamInFlight()).toBe(false);

    // 收尾之后可以正常再发
    await streamChat(body, () => undefined);
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });

  it('失败也要释放闸（否则一次网络错误会把对话页锁死）', async () => {
    vi.spyOn(globalThis, 'fetch').mockRejectedValue(new TypeError('Failed to fetch'));
    await expect(streamChat(body, () => undefined)).rejects.toThrow(/Failed to fetch/);
    expect(isChatStreamInFlight()).toBe(false);
  });
});

