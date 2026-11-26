// AI 助手服务端契约（对照 Agent2API `api/assistant.rs` / `assistant_history.rs` /
// `assistant_images.rs`；独立文件，由 api/index.ts barrel 合并进统一 `api` 对象）。
//
// ── 为什么单独成文件 ────────────────────────────────────────
// AIGX 目前的 `/chat` 用的是自家 `/api/chat/*` 会话与 `/api/channels/chat_test`
// 流式通道（已服务端化）。本模块把上游 `assistant*` 的**完整契约**先写清楚，
// 后端由主控/后续批次接线后，页面只换调用、不重写协议。端点未就绪（404）时
// 调用方按「不可用」降级，不吞错到界面。
//
// ── 契约总览 ────────────────────────────────────────────────
//   GET  /api/assistant/config                 → { config: AssistantConfigWire }
//   PUT  /api/assistant/config                 body { config }   → 同上（整块替换）
//   POST /api/assistant/chat                   body { model?, reasoning?, messages, tools? }
//                                              → text/event-stream（OpenAI delta）
//   GET  /api/assistant/sessions               → { sessions: AssistantSessionSummary[] }
//   GET  /api/assistant/sessions/{id}          → { session: AssistantSessionDetail }
//   POST /api/assistant/sessions               body { id, title?, messages, model?, reasoning? }
//                                              → { session: AssistantSessionSummary }
//   DELETE /api/assistant/sessions/{id}        → { removed: true }
//   DELETE /api/assistant/sessions             → { removed: n }
//   POST /api/assistant/images/generations|edits  body { session, request, confirmed? }
//                                              → { data: [{ id, url, mime, ... }] }
//   GET  /api/assistant/images/files/{id}      → 图片字节（image/*）
//
// 助手图片只回附件 id/url，字节不入对话上下文；`id` 是 64 位 hex，随会话删除。
import { API_BASE, authHeaders, request } from '../client';
import { readSseResponse } from '../stream';
import type { ChatStreamCallback } from '@/types';

/* ─── 助手配置（与上游 AssistantConfig 逐字段对齐）─────────── */

export interface AssistantConfigWire {
  model?: string | null;
  apiKeyId?: string | null;
  reasoning?: string | null;
  titleModel?: string | null;
  titlePrompt?: string;
  mode?: 'confirm' | 'full';
  systemPromptEnabled?: boolean;
  systemPrompt?: string;
  tools?: Record<string, boolean>;
}

/** 响应兼容裸 `{config}` 与 `{data:{config}}`。 */
export type AssistantConfigEnvelope = {
  config?: AssistantConfigWire;
  data?: { config?: AssistantConfigWire };
};

/** 从响应里取 config（两种外层形态）。 */
export function extractAssistantConfig(payload: unknown): AssistantConfigWire | null {
  if (!payload || typeof payload !== 'object') return null;
  const obj = payload as Record<string, unknown>;
  if (obj.config && typeof obj.config === 'object') return obj.config as AssistantConfigWire;
  const data = obj.data;
  if (data && typeof data === 'object') {
    const inner = (data as Record<string, unknown>).config;
    if (inner && typeof inner === 'object') return inner as AssistantConfigWire;
  }
  return null;
}

/* ─── 会话 ─────────────────────────────────────────────────── */

/** 会话摘要（左栏用；不含 messages）。 */
export interface AssistantSessionSummary {
  id: string;
  title: string;
  model?: string | null;
  reasoning?: string | null;
  createdAt: number;
  updatedAt: number;
  titleGenerated?: boolean;
}

/** 单条会话（含消息体；消息形状由前端拥有，后端当不透明数组存）。 */
export interface AssistantSessionDetail extends AssistantSessionSummary {
  messages: unknown[];
}

export interface AssistantSessionEnvelope {
  session?: AssistantSessionDetail;
  data?: { session?: AssistantSessionDetail };
}

/** 会话列表响应（兼容裸 `{sessions}` 与 `{data:{sessions}}`）。 */
export interface AssistantSessionsEnvelope {
  sessions?: AssistantSessionSummary[];
  data?: { sessions?: AssistantSessionSummary[] };
}

/* ─── 对话 ─────────────────────────────────────────────────── */

/** 一次助手对话的请求体。model 是**对外模型名**（/v1/models 的 data[].id）。 */
export interface AssistantChatRequest {
  model?: string;
  reasoning?: string;
  messages: unknown[];
  /** OpenAI tools 数组（前端按勾选过滤；后端核对授权后原样送上游）。 */
  tools?: unknown[];
}

/* ─── 图片工具 ─────────────────────────────────────────────── */

export interface AssistantImageItem {
  id: string;
  url: string;
  mime?: string;
  width?: number;
  height?: number;
  revised_prompt?: unknown;
}

export interface AssistantImageRequest {
  /** 当前已保存的会话 id（图片附件按会话归属） */
  session: string;
  /** 透传给上游的 images 请求体（与 OpenAI images 端点同形） */
  request: Record<string, unknown>;
  /** confirm 模式下必须为 true（后端会拒绝未确认的额度消耗） */
  confirmed?: boolean;
}

export const endpoints = {
  /* 配置 */
  getAssistantConfig: (): Promise<AssistantConfigEnvelope> =>
    request<AssistantConfigEnvelope>('GET', `${API_BASE}/assistant/config`),
  saveAssistantConfig: (config: AssistantConfigWire): Promise<AssistantConfigEnvelope> =>
    request<AssistantConfigEnvelope>('PUT', `${API_BASE}/assistant/config`, { config }),

  /* 会话 */
  listAssistantSessions: (): Promise<AssistantSessionsEnvelope> =>
    request<AssistantSessionsEnvelope>('GET', `${API_BASE}/assistant/sessions`),
  getAssistantSession: (id: string): Promise<AssistantSessionEnvelope> =>
    request<AssistantSessionEnvelope>('GET', `${API_BASE}/assistant/sessions/${encodeURIComponent(id)}`),
  saveAssistantSession: (body: {
    id: string;
    title?: string;
    messages: unknown[];
    model?: string;
    reasoning?: string;
  }): Promise<AssistantSessionEnvelope> =>
    request<AssistantSessionEnvelope>('POST', `${API_BASE}/assistant/sessions`, body),
  deleteAssistantSession: (id: string): Promise<{ removed?: boolean }> =>
    request<{ removed?: boolean }>('DELETE', `${API_BASE}/assistant/sessions/${encodeURIComponent(id)}`),
  clearAssistantSessions: (): Promise<{ removed?: number }> =>
    request<{ removed?: number }>('DELETE', `${API_BASE}/assistant/sessions`),

  /* 图片工具（生图 / 编辑） */
  assistantImageGenerations: (body: AssistantImageRequest): Promise<Record<string, unknown>> =>
    request<Record<string, unknown>>('POST', `${API_BASE}/assistant/images/generations`, body),
  assistantImageEdits: (body: AssistantImageRequest): Promise<Record<string, unknown>> =>
    request<Record<string, unknown>>('POST', `${API_BASE}/assistant/images/edits`, body),
};

/** 助手图片附件的下载地址（`<img src>` 直接用；需要 Bearer 时由后端放行同源）。 */
export function assistantImageFileUrl(id: string): string {
  return `${API_BASE}/assistant/images/files/${encodeURIComponent(id)}`;
}

/**
 * POST /api/assistant/chat 的 SSE 流式入口。
 *
 * 与 `testChannelChatStream` 同款分帧/解析（复用 `readSseResponse`），差别只在
 * 路径与请求体：上游助手固定 `stream: true`，返回 OpenAI 格式 delta（含
 * tool_calls），前端自己解析、执行工具后把 `role:"tool"` 结果作为下一轮
 * messages 再发上来。
 *
 * 端点未接线（404）时抛带状态码的错误，调用方据此降级或提示。
 */
export async function assistantChatStream(
  body: AssistantChatRequest,
  onDelta: ChatStreamCallback,
  signal?: AbortSignal,
): Promise<void> {
  const res = await fetch(`${API_BASE}/assistant/chat`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', ...authHeaders() },
    body: JSON.stringify(body),
    signal,
  });
  if (res.status === 401) {
    try {
      localStorage.removeItem('token');
      localStorage.removeItem('email');
      localStorage.removeItem('username');
      localStorage.removeItem('role');
      localStorage.removeItem('expires_at');
    } catch {
      /* ignore */
    }
    throw new Error('Unauthorized');
  }
  if (!res.ok || !res.body) {
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
      /* 非 JSON 错误体直接抛原文 */
    }
    const err = new Error(msg || `Request failed with status ${res.status}`) as Error & { status?: number };
    err.status = res.status;
    throw err;
  }
  await readSseResponse(res, onDelta, signal);
}
