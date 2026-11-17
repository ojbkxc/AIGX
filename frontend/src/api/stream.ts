/**
 * SSE / 流式对话解析层（design 1.4：`api/index.ts` 按域拆分的第 2 块）。
 *
 * 纯解析函数（可单测）+ 两条流式入口：经后端中转 / 浏览器直连上游。
 */

import { extractToolCalls, extractUsage, type ToolCallWireMode } from '@/lib/toolCallAdapter';
import type { ChatStreamCallback, PlaygroundChatRequest } from '@/types';

import { API_BASE, authHeaders } from './client';

/**
 * 解析单个 SSE 帧（OpenAI / Anthropic 协议）为增量回调序列。
 * 纯函数，便于单元测试。返回是否有终止帧（[DONE] / message_stop / error）。
 */
export function parseSseFrame(
  frame: string,
  onDelta: ChatStreamCallback,
  opts: { toolCallMode?: ToolCallWireMode } = {},
): boolean {
  const toolCallMode = opts.toolCallMode ?? 'auto';
  let ended = false;
  const lines = frame
    .split('\n')
    .map((l) => l.trim())
    .filter((l) => l.startsWith('data:') && l.length > 5)
    .map((l) => l.slice(5).trim())
    .filter((l) => l.length > 0);
  for (const data of lines) {
    if (data === '[DONE]') {
      if (!ended) onDelta({ content: '', isEnd: true });
      ended = true;
      continue;
    }
    try {
      const parsed = JSON.parse(data) as Record<string, unknown>;
      // Anthropic message_stop
      if (parsed.type === 'message_stop') {
        if (!ended) onDelta({ content: '', isEnd: true });
        ended = true;
        continue;
      }
      if (parsed.type === 'error' || parsed.error) {
        const raw = parsed.error as Record<string, unknown> | string | undefined;
        const msg = typeof raw === 'string' ? raw : (raw?.message as string | undefined) ?? '上游流式错误';
        // 标记 kind=error：ChatDebugger 据此以「⚠️」收尾而不是当正文拼接
        onDelta({ content: msg, isEnd: true, kind: 'error' });
        ended = true;
        continue;
      }
      // 工具调用分片 / 用量收尾（#63）：形状全部走适配层归一化，
      // 回灌层落地后只改 toolCallAdapter，不动解析与 UI。
      for (const tc of extractToolCalls(parsed, toolCallMode)) {
        onDelta({ content: '', isEnd: false, kind: 'tool_call', toolCall: tc });
      }
      const usage = extractUsage(parsed);
      if (usage) onDelta({ content: '', isEnd: false, kind: 'usage', usage });
      // Anthropic delta
      const anthropicDelta = parsed.delta as { text?: string } | undefined;
      if (anthropicDelta && typeof anthropicDelta.text === 'string' && anthropicDelta.text) {
        onDelta({ content: anthropicDelta.text, isEnd: false });
        continue;
      }
      // OpenAI delta（content / reasoning_content / text）
      const choices = parsed.choices as Array<{ delta?: Record<string, unknown> }> | undefined;
      const firstDelta = choices?.[0]?.delta;
      if (firstDelta) {
        const reasoning = [firstDelta.reasoning_content, firstDelta.reasoning]
          .find((v) => typeof v === 'string') as string | undefined;
        if (reasoning) onDelta({ content: reasoning, isEnd: false, kind: 'reasoning' });
        const content = ([firstDelta.content, firstDelta.text]
          .find((v) => typeof v === 'string') ?? '') as string;
        if (content) onDelta({ content, isEnd: false, kind: 'content' });
      }
    } catch {
      // 非 JSON 帧（心跳/注释）忽略
    }
  }
  return ended;
}

/**
 * 真·流式对话调试：逐块读取 text/event-stream 并增量解析。
 *
 * 与 testChannelChat 的区别：不等整个响应结束，每解析到一个增量
 * （OpenAI choices[0].delta 或 Anthropic delta.text）立即回调，
 * ChatDebugger 用它在界面上逐 token 渲染。
 */
export async function testChannelChatStream(
  data: PlaygroundChatRequest,
  onDelta: ChatStreamCallback,
  signal?: AbortSignal,
  opts: { toolCallMode?: ToolCallWireMode } = {},
): Promise<void> {
  const res = await fetch(`${API_BASE}/channels/chat_test`, {
    method: 'POST',
    headers: {
      'Content-Type': 'application/json',
      ...authHeaders(),
    },
    body: JSON.stringify(data),
    signal,
  });

  if (res.status === 401) {
    // 与 request() 对齐：会话失效时清理本地登录态，避免 SSE 通道被 401 后
    // 页面停留在「已登录但令牌已死」的状态。
    try {
      localStorage.removeItem('token');
      localStorage.removeItem('email');
      localStorage.removeItem('username');
      localStorage.removeItem('role');
      localStorage.removeItem('expires_at');
    } catch {
      // ignore
    }
    throw new Error('Unauthorized');
  }

  if (!res.ok || !res.body) {
    const text = await res.text().catch(() => '');
    let msg = text;
    if (text) {
      try {
        const parsed = JSON.parse(text) as Record<string, unknown>;
        const raw = parsed.error ?? parsed.message;
        if (typeof raw === 'string') msg = raw;
      } catch {
        // 非 JSON 错误体，直接抛原文
      }
    }
    throw new Error(msg || `Request failed with status ${res.status}`);
  }

  await readSseResponse(res, onDelta, signal, opts);
}

/**
 * 读取 SSE 响应并逐帧解析为增量回调。
 *
 * 由两条路径共用：testChannelChatStream（经后端中转）与 ChatDebugger
 * 直连模式（无密钥渠道由浏览器直接请求上游）。非流式 JSON（上游降级）
 * 时解析后一次性回调。
 */
export async function readSseResponse(
  res: Response,
  onDelta: ChatStreamCallback,
  signal?: AbortSignal,
  opts: { toolCallMode?: ToolCallWireMode } = {},
): Promise<void> {
  const contentType = res.headers.get('Content-Type') || '';
  if (!contentType.includes('text/event-stream')) {
    // 非流式 JSON（例如上游降级或后端报错），解析后一次性回调
    const text = await res.text();
    let parsed: unknown = null;
    if (text) {
      try {
        parsed = JSON.parse(text);
      } catch {
        // ignore
      }
    }
    const p = (parsed ?? {}) as Record<string, unknown>;
    const data = (p.data ?? {}) as Record<string, unknown>;
    // 工具调用 / 用量：非流式整帧也要走同一适配层（上游降级时常见）
    for (const tc of extractToolCalls(p, opts.toolCallMode ?? 'auto')) {
      onDelta({ content: '', isEnd: false, kind: 'tool_call', toolCall: tc });
    }
    const usage = extractUsage(p);
    if (usage) onDelta({ content: '', isEnd: false, kind: 'usage', usage });
    // 中转路径包一层 data.content；直连上游是 OpenAI 原生 choices 形状
    let content = typeof data.content === 'string' ? data.content : '';
    if (!content) {
      const choices = p.choices as Array<{ message?: { content?: string } }> | undefined;
      content = choices?.[0]?.message?.content ?? '';
    }
    const error = typeof data.error === 'string' ? data.error : typeof p.error === 'string' ? p.error : '';
    if (error) {
      onDelta({ content: error, isEnd: true });
    } else if (content) {
      onDelta({ content, isEnd: true });
    }
    return;
  }

  const reader = res.body?.getReader();
  if (!reader) return;
  const decoder = new TextDecoder('utf-8');
  let buffer = '';
  let ended = false;

  const handleFrame = (frame: string): void => {
    ended = parseSseFrame(frame, onDelta, opts) || ended;
  };

  for (;;) {
    if (signal?.aborted) throw new DOMException('Aborted', 'AbortError');
    const { done, value } = await reader.read();
    if (done) break;
    buffer += decoder.decode(value, { stream: true });
    // 统一行尾：剥离所有 \r（上游透传可能用 CRLF）。
    // SSE 数据行内不应有孤立 \r，strip 是安全的。
    buffer = buffer.replace(/\r/g, '');
    // 空行 = SSE 帧边界，只处理完整帧，避免半个 JSON 被解析失败
    let idx = buffer.indexOf('\n\n');
    while (idx !== -1) {
      const frame = buffer.slice(0, idx);
      buffer = buffer.slice(idx + 2);
      handleFrame(frame);
      idx = buffer.indexOf('\n\n');
    }
  }
  // 尾部残留帧
  if (buffer.trim()) handleFrame(buffer);
  if (!ended) onDelta({ content: '', isEnd: true });
}

/**
 * 无密钥渠道浏览器直连上游：不经 AIGX 后端中转，请求从用户本机直接发往
 * 上游 base_url。仅用于渠道「对话调试」弹窗中 api_key 为空的渠道。
 *
 * 协议复刻后端 handle_channel_chat_test 的 URL 构造：
 * - openai：normalize_base_url（无路径时补 /v1）+ /chat/completions
 * - anthropic：base 原样 + /v1/messages（x-api-key 空则省略）
 *
 * 注意：浏览器直连受上游 CORS 策略约束，跨域被拒时抛出带提示的错误。
 */
export async function directChannelChatStream(
  channel: { base_url?: string; channel_type?: string },
  data: PlaygroundChatRequest,
  onDelta: ChatStreamCallback,
  signal?: AbortSignal,
  opts: { toolCallMode?: ToolCallWireMode } = {},
): Promise<void> {
  if (!channel.base_url || !channel.base_url.trim()) {
    throw new Error('供应商未配置 Base URL，无法直连');
  }
  const isAnthropic = (channel.channel_type || '').toLowerCase() === 'anthropic';
  const rawBase = channel.base_url.trim().replace(/\/+$/, '');
  const hasPath = /^https?:\/\/[^/]+\/.+/.test(rawBase);
  const base = isAnthropic || hasPath ? rawBase : `${rawBase}/v1`;

  const messages: unknown[] = Array.isArray(data.history) ? data.history.slice() : [];
  messages.push({ role: 'user', content: data.message ?? '' });

  let url: string;
  let payload: Record<string, unknown>;
  if (isAnthropic) {
    url = `${base}/v1/messages`;
    payload = {
      model: data.model,
      max_tokens: data.max_tokens ?? 1024,
      messages,
    };
    if (data.stream) payload.stream = true;
    if (data.system_prompt) payload.system = data.system_prompt;
  } else {
    url = `${base}/chat/completions`;
    payload = {
      model: data.model,
      messages,
      stream: data.stream ?? false,
    };
    if (data.temperature !== undefined) payload.temperature = data.temperature;
    if (data.max_tokens !== undefined) payload.max_tokens = data.max_tokens;
    if (data.top_p !== undefined) payload.top_p = data.top_p;
    if (data.frequency_penalty !== undefined) payload.frequency_penalty = data.frequency_penalty;
    if (data.presence_penalty !== undefined) payload.presence_penalty = data.presence_penalty;
    if (data.system_prompt) {
      payload.messages = [{ role: 'system', content: data.system_prompt }, ...messages];
    }
  }

  let res: Response;
  try {
    res = await fetch(url, {
      method: 'POST',
      headers: isAnthropic
        ? { 'Content-Type': 'application/json', 'anthropic-version': '2023-06-01' }
        : { 'Content-Type': 'application/json' },
      body: JSON.stringify(payload),
      signal,
    });
  } catch (err) {
    // fetch 网络层失败（上游不可达或 CORS 拦截）——TypeError 不带原始原因
    if (signal?.aborted) throw err;
    throw new Error(
      `直连失败（请求未发或被 CORS 拦截）：上游 ${base} 不允许浏览器跨域访问。` +
      '可改用经服务器中转的普通供应商，或在上游网关配置 CORS 允许本站来源。',
    );
  }

  if (!res.ok) {
    const text = await res.text().catch(() => '');
    let msg = text;
    try {
      const parsed = JSON.parse(text) as Record<string, unknown>;
      const raw = parsed.error ?? parsed.message;
      if (typeof raw === 'string') msg = raw;
      else if (raw && typeof raw === 'object') {
        const m = (raw as Record<string, unknown>).message;
        if (typeof m === 'string') msg = m;
      }
    } catch {
      // 非 JSON 错误体，直接抛原文
    }
    throw new Error(msg || `Upstream HTTP ${res.status}`);
  }

  await readSseResponse(res, onDelta, signal, opts);
}
