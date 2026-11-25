/**
 * 域端点（design 1.4：从 `api/index.ts` 的 `api` 对象按域搬出，纯搬移）。
 *
 * 方法原样保留，由 `index.ts` 展开合并进同一个 `api` 对象 —— 调用方仍写
 * `api.<method>()`，导入路径不变。
 */

import type { ApiResponse, PromptSource, PromptSourceItem } from '@/types';
import { readSseBody } from '../../lib/stream';
import { API_BASE, authHeaders, request } from '../client';

/**
 * AI 运维 Agent 的 SSE 事件（后端 `AgentEvent` 的序列化形状）。
 *
 * 字段刻意写全：`request_id` / `approved` / `arguments` 是审批卡与工具轨迹
 * 依赖的键，此前类型里没有，页面侧只能靠自己的 `AgentEvent` 声明「猜」。
 */
export interface AgentChatEvent {
  type: string;
  turn?: number;
  name?: string;
  arguments?: string;
  ok?: boolean;
  text?: string;
  content?: string;
  message?: string;
  request_id?: string;
  approved?: boolean;
}

/** 非 2xx 的响应体 → 可读文案（JSON 信封三种形态都捋一遍，取不到返回空串） */
function agentErrorText(raw: string): string {
  const text = raw.trim();
  if (!text) return '';
  try {
    const parsed = JSON.parse(text) as unknown;
    if (typeof parsed === 'string') return parsed.trim();
    if (parsed && typeof parsed === 'object') {
      const obj = parsed as Record<string, unknown>;
      for (const candidate of [obj.error, obj.message]) {
        if (typeof candidate === 'string' && candidate.trim()) return candidate.trim();
        if (candidate && typeof candidate === 'object') {
          const inner = (candidate as Record<string, unknown>).message;
          if (typeof inner === 'string' && inner.trim()) return inner.trim();
        }
      }
    }
  } catch {
    // 非 JSON 错误体：原文即文案
  }
  return text;
}

export const endpoints = {
    // ==================== 公开提示词源（Prompts 页「拉取公开源」） ====================
  listPromptSources: (): Promise<ApiResponse<PromptSource[]>> =>
    request<ApiResponse<PromptSource[]>>('GET', `${API_BASE}/prompts/sources`),
  /** 抓取公开源：成功时 data 为条目数组；truncated 表示因体积闸丢弃过尾部条目 */
  fetchPromptSource: (source: string): Promise<ApiResponse<PromptSourceItem[]>> =>
    request<ApiResponse<PromptSourceItem[]>>('POST', `${API_BASE}/prompts/fetch`, { source }),
  /**
   * Agent 对话：SSE 流式，逐事件回调（AgentEvent）。
   *
   * 后端协议（`src/agent/api.rs`）：每个事件一帧 `data: {AgentEvent}\n\n`，
   * 空闲期注入 `: keepalive` 注释行，**不发 `[DONE]`** —— 因此这里不能把
   * 「没有终止帧」当错误（Trae 一类上游同样不发）。
   *
   * 分帧与生命周期统一走 `lib/stream`（与对话调试那条链同一实现）：
   * abort / 异常 / 正常结束都会 `cancel()` 底层流，abort 之后不再回调，
   * 缺终结符的尾帧在 EOF 处 flush 出来。
   */
  agentChatStream: async (
    id: string,
    message: string,
    onEvent: (ev: AgentChatEvent) => void,
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
      // JSON 信封要取出文案：直接把 body 塞进 Error 会让页面显示 `{"error":"…"}`，
      // 而 describeAgentError 按英文征状匹配，匹配不上就原样怼给用户。
      throw new Error(agentErrorText(text) || `Agent chat failed (HTTP ${res.status})`);
    }
    await readSseBody(res.body, {
      signal,
      onFrame: (frame) => {
        const data = frame.data.trim();
        const eventName = frame.event.trim().toLowerCase();
        if (eventName === 'error') {
          onEvent({ type: 'error', message: data || '上游流式错误' });
          return;
        }
        if (!data || data === '[DONE]') return;
        try {
          onEvent(JSON.parse(data) as AgentChatEvent);
        } catch {
          // 非 JSON 帧（心跳 / 后端日志行）忽略
        }
      },
    });
  },
};
