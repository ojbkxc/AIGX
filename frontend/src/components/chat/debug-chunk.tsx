/**
 * 非流式对话结果归一化（`api.testChannelChat` 的返回形状）。
 *
 * 抽出来的原因：`ChatDebugger.handleSend` 的非流式分支直接写
 *
 *     const res = (await api.testChannelChat(body)) as unknown as ChatChunkResult;
 *     const data = res.data || {};
 *     if (res.stream && res.stream.length) { res.stream.map(...) }
 *
 * 三处都没有形状判据：
 * 1. `request()` 在 **HTTP 204** 时返回 `null`（见 `api/client.ts`），`res.data`
 *    当场 TypeError——整个发送流程掉进 catch，用户只看到「Cannot read properties
 *    of null」这种内部错误串。
 * 2. `res.stream` 只要不是数组（上游降级把 SSE 文本原样塞回来、或老后端给对象），
 *    `.map` 就抛「res.stream.map is not a function」。
 * 3. `data.content` 若是非字符串（数字/对象），拼进消息后渲染层再炸一次。
 *
 * 这里统一成「要么给一段正文，要么给一条错误串」，调用方不再碰原始形状。
 */
export interface NormalizedChatChunk {
  /** 要追加到 assistant 气泡的正文（空串 = 没有正文） */
  content: string;
  /** 要展示的错误文案（空串 = 没有错误） */
  error: string;
  /** 是否走了流式数组形态（供调试面判断要不要标记「非流式」） */
  fromStreamArray: boolean;
}

/**
 * 归一化 `testChannelChat` 的响应。
 *
 * 优先级与原实现一致：stream 数组拼接 > data.content > data.error > res.error，
 * 只是每一级都先判类型。
 */
export function normalizeChatChunk(res: unknown): NormalizedChatChunk {
  const empty: NormalizedChatChunk = { content: '', error: '', fromStreamArray: false };
  // 204 / 空响应体：request() 返回 null
  if (res === null || res === undefined || typeof res !== 'object') return empty;
  const obj = res as Record<string, unknown>;

  // 1. stream 数组（老后端把增量收齐后一次性返回）
  const stream = obj.stream;
  if (Array.isArray(stream)) {
    const parts: string[] = [];
    for (const chunk of stream) {
      if (chunk && typeof chunk === 'object') {
        const c = (chunk as Record<string, unknown>).content;
        if (typeof c === 'string' && c) parts.push(c);
      } else if (typeof chunk === 'string' && chunk) {
        parts.push(chunk);
      }
    }
    const joined = parts.join('');
    if (joined) return { content: joined, error: '', fromStreamArray: true };
  }

  const data = obj.data;
  const dataObj = data && typeof data === 'object' ? (data as Record<string, unknown>) : {};

  // 2. data.content（中转路径的标准形状）
  if (typeof dataObj.content === 'string' && dataObj.content) {
    return { content: dataObj.content, error: '', fromStreamArray: false };
  }

  // 3. data.error → res.error
  if (typeof dataObj.error === 'string' && dataObj.error) {
    return { content: '', error: dataObj.error, fromStreamArray: false };
  }
  if (typeof obj.error === 'string' && obj.error) {
    return { content: '', error: obj.error, fromStreamArray: false };
  }

  return empty;
}
