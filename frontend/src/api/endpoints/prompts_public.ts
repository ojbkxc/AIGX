/**
 * 域端点（design 1.4：从 `api/index.ts` 的 `api` 对象按域搬出，纯搬移）。
 *
 * 方法原样保留，由 `index.ts` 展开合并进同一个 `api` 对象 —— 调用方仍写
 * `api.<method>()`，导入路径不变。
 */

import type { ApiResponse, PromptSource, PromptSourceItem } from '@/types';
import { API_BASE, authHeaders, request } from '../client';

export const endpoints = {
    // ==================== 公开提示词源（Prompts 页「拉取公开源」） ====================
  listPromptSources: (): Promise<ApiResponse<PromptSource[]>> =>
    request<ApiResponse<PromptSource[]>>('GET', `${API_BASE}/prompts/sources`),
  /** 抓取公开源：成功时 data 为条目数组；truncated 表示因体积闸丢弃过尾部条目 */
  fetchPromptSource: (source: string): Promise<ApiResponse<PromptSourceItem[]>> =>
    request<ApiResponse<PromptSourceItem[]>>('POST', `${API_BASE}/prompts/fetch`, { source }),
  /** Agent 对话：SSE 流式，逐事件回调（AgentEvent） */
  agentChatStream: async (
    id: string,
    message: string,
    onEvent: (ev: { type: string; content?: string; name?: string; ok?: boolean; message?: string }) => void,
    signal?: AbortSignal,
    model?: string,
    reasoningEffort?: string,
    role?: 'observer' | 'operator',
  ): Promise<void> => {
    const body: { message: string; model?: string; reasoning_effort?: string; role?: string } = { message };
    if (model) body.model = model;
    if (reasoningEffort) body.reasoning_effort = reasoningEffort;
    if (role) body.role = role;
    const res = await fetch(`${API_BASE}/agent/sessions/${encodeURIComponent(id)}/chat`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', ...authHeaders() },
      body: JSON.stringify(body),
      signal,
    });
    if (!res.ok || !res.body) {
      const text = await res.text().catch(() => '');
      throw new Error(text || `Agent chat failed (HTTP ${res.status})`);
    }
    const reader = res.body.getReader();
    const decoder = new TextDecoder('utf-8');
    let buffer = '';
    for (;;) {
      if (signal?.aborted) throw new DOMException('Aborted', 'AbortError');
      const { done, value } = await reader.read();
      if (done) break;
      buffer += decoder.decode(value, { stream: true }).replace(/\r/g, '');
      let idx = buffer.indexOf('\n\n');
      while (idx !== -1) {
        const frame = buffer.slice(0, idx);
        buffer = buffer.slice(idx + 2);
        for (const line of frame.split('\n')) {
          const l = line.trim();
          if (!l.startsWith('data:')) continue;
          const data = l.slice(5).trim();
          if (!data || data === '[DONE]') continue;
          try {
            onEvent(JSON.parse(data) as { type: string; content?: string; name?: string; ok?: boolean; message?: string });
          } catch {
            // 忽略非 JSON 帧
          }
        }
        idx = buffer.indexOf('\n\n');
      }
    }
  },
};
