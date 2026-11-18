/**
 * 域端点（design 1.4：从 `api/index.ts` 的 `api` 对象按域搬出，纯搬移）。
 *
 * 方法原样保留，由 `index.ts` 展开合并进同一个 `api` 对象 —— 调用方仍写
 * `api.<method>()`，导入路径不变。
 */

import type {
  ChatSessionSummary,
  UserToolCatalogItem,
  UserToolPolicy,
  ApiResponse,
} from '@/types';
import { API_BASE, request } from '../client';

export const endpoints = {
  // ==================== AI 运维 Agent（管理员专属 /admin/agent） ====================
  listAgentSessions: (): Promise<ApiResponse<unknown[]>> =>
    request<ApiResponse<unknown[]>>('GET', `${API_BASE}/agent/sessions`),
  createAgentSession: (title: string, role?: 'observer' | 'operator'): Promise<ApiResponse<{ id: string; title: string }>> =>
    request<ApiResponse<{ id: string; title: string }>>('POST', `${API_BASE}/agent/sessions`, { title, role }),
  getAgentSession: (id: string): Promise<ApiResponse<unknown>> =>
    request<ApiResponse<unknown>>('GET', `${API_BASE}/agent/sessions/${encodeURIComponent(id)}`),
  deleteAgentSession: (id: string): Promise<ApiResponse<null>> =>
    request<ApiResponse<null>>('DELETE', `${API_BASE}/agent/sessions/${encodeURIComponent(id)}`),
  agentApprove: (requestId: string, action: 'allow' | 'deny' | 'remember'): Promise<ApiResponse<null>> =>
    request<ApiResponse<null>>('POST', `${API_BASE}/agent/approvals/${encodeURIComponent(requestId)}`, { action }),
  getAgentConfig: (): Promise<ApiResponse<{
    model?: string;
    channel?: string;
    /** 空 model 时后端自动发现的实际生效模型 */
    effectiveModel?: string | null;
    /** 工具清单（name/label/risk），审批卡中文名与设置面板共用 */
    tools?: Array<{ name: string; label: string; risk: string }>;
  }>> =>
    request<ApiResponse<{ model?: string; channel?: string; effectiveModel?: string | null; tools?: Array<{ name: string; label: string; risk: string }> }>>('GET', `${API_BASE}/agent/config`),
  // ==================== 聊天会话（按用户隔离的对话记录） ====================
  listChatSessions: (): Promise<ApiResponse<Array<ChatSessionSummary>>> =>
    request<ApiResponse<Array<ChatSessionSummary>>>('GET', `${API_BASE}/chat/sessions`),
  createChatSession: (data: { title?: string; model?: string; updated_at?: number } = {}): Promise<ApiResponse<{ id: string; title: string; model: string }>> =>
    request<ApiResponse<{ id: string; title: string; model: string }>>('POST', `${API_BASE}/chat/sessions`, data),
  getChatSession: (id: string): Promise<ApiResponse<{ session: { id: string; title: string; model: string }; messages: Array<{ role: string; content: string; extra?: unknown }> }>> =>
    request<ApiResponse<{ session: { id: string; title: string; model: string }; messages: Array<{ role: string; content: string; extra?: unknown }> }>>('GET', `${API_BASE}/chat/sessions/${encodeURIComponent(id)}`),
  deleteChatSession: (id: string): Promise<ApiResponse<null>> =>
    request<ApiResponse<null>>('DELETE', `${API_BASE}/chat/sessions/${encodeURIComponent(id)}`),
  renameChatSession: (id: string, title: string): Promise<ApiResponse<null>> =>
    request<ApiResponse<null>>('POST', `${API_BASE}/chat/sessions/${encodeURIComponent(id)}/rename`, { title }),
  /**
   * 会话 fork（#37 后端端点；#58 前端接线）。
   * 从 `message_seq`（0-based，**含该条**，与 GET 详情的消息下标同序）分叉；
   * 后端有界复制（200 条 / 128KiB，超限保尾部），`dropped` 如实返回不静默。
   * 响应 `session` 带 parent_session_id / title_source（父子追溯）。
   */
  forkChatSession: (id: string, messageSeq: number): Promise<ApiResponse<{ session: ChatSessionSummary; copied: number; dropped: number }>> =>
    request<ApiResponse<{ session: ChatSessionSummary; copied: number; dropped: number }>>('POST', `${API_BASE}/chat/sessions/${encodeURIComponent(id)}/fork`, { message_seq: messageSeq }),
  /** 整表替换消息（编辑/重发后 debounce 保存） */
  replaceChatMessages: (id: string, messages: Array<{ role: string; content: string; extra?: unknown }>, touch = true): Promise<ApiResponse<{ count: number }>> =>
    request<ApiResponse<{ count: number }>>('PUT', `${API_BASE}/chat/sessions/${encodeURIComponent(id)}/messages`, { messages, touch }),
  /** 追加一条消息（导入旧 localStorage 会话用） */
  appendChatMessage: (id: string, message: { role: string; content: string; extra?: unknown }): Promise<ApiResponse<{ seq: number }>> =>
    request<ApiResponse<{ seq: number }>>('POST', `${API_BASE}/chat/sessions/${encodeURIComponent(id)}/messages`, message),
  /**
   * 终端用户工具策略（#65；设计稿 §5.1 三端点，后端未落地前 404 → 页面降级）。
   * PUT 超限（组数/工具数 caps）返回 409 + 原因文案。
   */
  getUserToolPolicy: (): Promise<ApiResponse<UserToolPolicy>> =>
    request<ApiResponse<UserToolPolicy>>('GET', `${API_BASE}/user-tools/policy`),
  saveUserToolPolicy: (policy: UserToolPolicy): Promise<ApiResponse<UserToolPolicy>> =>
    request<ApiResponse<UserToolPolicy>>('PUT', `${API_BASE}/user-tools/policy`, policy),
  getUserToolCatalog: (): Promise<ApiResponse<{ tools: UserToolCatalogItem[] }>> =>
    request<ApiResponse<{ tools: UserToolCatalogItem[] }>>('GET', `${API_BASE}/user-tools/catalog`),
};
