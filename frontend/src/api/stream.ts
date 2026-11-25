/**
 * SSE / 流式对话解析层（design 1.4：`api/index.ts` 按域拆分的第 2 块）。
 *
 * 纯解析函数（可单测）+ 两条流式入口：经后端中转 / 浏览器直连上游。
 *
 * ── 分帧已下沉到 `lib/stream` ────────────────────────────────
 * 切帧、多行 `data:` 拼接、`event:` 名、注释行、CRLF、超长帧、abort 真断流
 * 都在 `lib/stream.ts`（唯一实现，纯逻辑可离线驱动）。本文件只做**协议语义**：
 * 把一帧 OpenAI / Anthropic 增量翻译成 `ChatStreamDelta`，以及两条请求入口。
 */

import { extractToolCalls, extractUsage, type ToolCallWireMode } from '@/lib/toolCallAdapter';
import { readSseBody, splitSseFrames, type SseFrame } from '@/lib/stream';
import type { ChatStreamCallback, PlaygroundChatRequest } from '@/types';

import { API_BASE, authHeaders } from './client';

/** 从错误对象/字符串里取出可读文案（三种形态都捋一遍，取不到给 null） */
function errorMessage(raw: unknown): string | null {
  if (typeof raw === 'string') return raw.trim() || null;
  if (raw && typeof raw === 'object') {
    const obj = raw as Record<string, unknown>;
    const message = obj.message;
    if (typeof message === 'string' && message.trim()) return message.trim();
    const nested = obj.error;
    if (typeof nested === 'string' && nested.trim()) return nested.trim();
    if (nested && typeof nested === 'object') {
      const inner = (nested as Record<string, unknown>).message;
      if (typeof inner === 'string' && inner.trim()) return inner.trim();
    }
    try {
      return JSON.stringify(raw);
    } catch {
      return null;
    }
  }
  return null;
}

/** 统一错误帧出口：`kind='error'` + `isEnd` 是 ChatDebugger 收尾「⚠️」的判据 */
function emitError(onDelta: ChatStreamCallback, message: string): boolean {
  onDelta({ content: message, isEnd: true, kind: 'error' });
  return true;
}

/**
 * 解析一帧已切好的 SSE 事件（`lib/stream` 的产物），返回是否终止。
 *
 * 终止判据（与 A2A 同口径）：`[DONE]` / Anthropic `message_stop` / 错误帧。
 * **缺 `[DONE]` 不算错误** —— Trae 等上游本来就不发（调用方在流结束时补收尾）。
 */
function handleSseFrame(
  frame: SseFrame,
  onDelta: ChatStreamCallback,
  toolCallMode: ToolCallWireMode,
): boolean {
  const eventName = frame.event.trim().toLowerCase();
  const data = frame.data.trim();
  if (!data) {
    // 空 data 帧：只有显式 event 名时才可能有语义（如 `event: error` 无正文）
    if (eventName === 'error') return emitError(onDelta, '上游流式错误');
    return false;
  }
  if (data === '[DONE]') {
    onDelta({ content: '', isEnd: true });
    return true;
  }
  let parsed: Record<string, unknown> | null = null;
  try {
    parsed = JSON.parse(data) as Record<string, unknown>;
  } catch {
    parsed = null;
  }
  if (!parsed) {
    // 兼容非规范上游：一个事件块里塞了多条独立的 `data:` JSON（规范要求用 \n
    // 拼成一条负载，拼接后自然不是 JSON）——逐条回退解析，别把整块丢掉。
    if (data.includes('\n')) {
      let ended = false;
      for (const line of data.split('\n')) {
        if (ended) break;
        const text = line.trim();
        if (!text) continue;
        ended = handleSseFrame({ event: frame.event, data: text }, onDelta, toolCallMode) || ended;
      }
      return ended;
    }
    // 非 JSON 负载：`event: error` 当错误文案上报；其余（心跳/日志）忽略
    return eventName === 'error' ? emitError(onDelta, data) : false;
  }
  if (eventName === 'error') {
    return emitError(onDelta, errorMessage(parsed.error ?? parsed.message ?? parsed) ?? '上游流式错误');
  }
  // Anthropic message_stop
  if (parsed.type === 'message_stop') {
    onDelta({ content: '', isEnd: true });
    return true;
  }
  if (parsed.type === 'error' || (parsed.error !== undefined && parsed.error !== null)) {
    const msg = errorMessage(parsed.error ?? parsed.message) ?? '上游流式错误';
    return emitError(onDelta, msg);
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
    return false;
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
  return false;
}

/**
 * 解析一段 SSE 文本（一个或多个完整帧）为增量回调序列。
 * 纯函数，便于单元测试。返回是否有终止帧（[DONE] / message_stop / error）。
 *
 * 终止帧之后的内容不再下发：旧实现会继续把它们拼进同一只气泡，
 * 用户看到「回答后面挂了一段尾巴」，而那尾巴其实是后端收尾噪声。
 */
export function parseSseFrame(
  frame: string,
  onDelta: ChatStreamCallback,
  opts: { toolCallMode?: ToolCallWireMode } = {},
): boolean {
  const toolCallMode = opts.toolCallMode ?? 'auto';
  let ended = false;
  for (const parsed of splitSseFrames(frame)) {
    if (ended) break;
    ended = handleSseFrame(parsed, onDelta, toolCallMode);
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
        // 错误体三种形态：`{error:"…"}` / `{message:"…"}` / `{error:{message}}`。
        // 旧实现只认前两种的字符串分支，对象形态会把整段 JSON 当文案怼给用户。
        const extracted = errorMessage(parsed.error ?? parsed.message);
        if (extracted) msg = extracted;
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
    const error = errorMessage(data.error) ?? errorMessage(p.error) ?? '';
    if (error) {
      // kind='error' 是「错误气泡」的唯一判据：旧实现漏了它，ChatDebugger
      // 走正文分支且 isEnd=true ⇒ 错误文案被整段丢弃，界面只剩「上游未返回内容」。
      onDelta({ content: error, isEnd: true, kind: 'error' });
    } else if (content) {
      // 正文必须走**非终止**增量：终止增量在 UI 侧只用来收尾、不拼正文
      // （旧实现把 content 挂在 isEnd 上，正文被丢掉，见上面的同一处判据）。
      onDelta({ content, isEnd: false, kind: 'content' });
      onDelta({ content: '', isEnd: true });
    } else {
      onDelta({ content: '', isEnd: true });
    }
    return;
  }

  const body = res.body;
  if (!body) return;
  const toolCallMode = opts.toolCallMode ?? 'auto';
  let ended = false;
  await readSseBody(body, {
    signal,
    stopWhen: () => ended,
    onFrame: (frame) => {
      if (ended) return;
      ended = handleSseFrame(frame, onDelta, toolCallMode);
    },
  });
  // 缺 [DONE]（Trae 等上游明确不发）也要恰好收尾一次
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
