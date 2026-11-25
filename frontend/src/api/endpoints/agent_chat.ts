/**
 * 域端点（design 1.4：从 `api/index.ts` 的 `api` 对象按域搬出，纯搬移）。
 *
 * 方法原样保留，由 `index.ts` 展开合并进同一个 `api` 对象 —— 调用方仍写
 * `api.<method>()`，导入路径不变。
 */

import type {
  ChatSessionSummary,
  ApiResponse,
} from '@/types';
import { API_BASE, request } from '../client';

/**
 * `/api/agent/config` 的响应形状 —— 与 `/api/agent/settings` **同一份**
 * （后端 `admin/settings.rs::agent_settings_json`，**camelCase**）。
 *
 * 前端只需要「模型 / 渠道 / 思考强度 / 工具规格」几项；`tools[]` 是工具卡与
 * 审批卡的**唯一**中文名与风险来源（`agent/tools.rs` 的 `RiskLevel::as_str()`），
 * 不要在页面里再写一张表。
 *
 * ⚠️ 写侧不在这里：`PUT /api/agent/settings` 已由 `endpoints/tickets.ts` 的
 * `saveAgentSettings` 承担（`AgentSettingsItem`）。两个文件各写一份同名方法会
 * 在 `api/index.ts` 的展开合并里**后者覆盖前者**，类型与实现分叉 —— 所以本文件
 * 只加只读视图，不重复声明写接口。
 */
export interface AgentToolSpecView {
  name: string;
  /** 后端 registry 的中文名（`agent/tools.rs`），工具卡与审批卡共用 */
  label: string;
  description?: string;
  /** `readonly` / `low` / `high`（`RiskLevel::as_str()`） */
  risk: string;
}

export interface AgentConfigView {
  enabled?: boolean;
  model?: string;
  channel?: string;
  /** 空 `model` 时后端自动发现的实际生效模型 */
  effectiveModel?: string | null;
  /** 思考强度（`off` / `low` / `medium` / `high`） */
  reasoningEffort?: string | null;
  systemPrompt?: string;
  maxTurns?: number;
  approvalTimeoutSecs?: number;
  maxHistory?: number;
  disabledTools?: string[];
  tools?: AgentToolSpecView[];
}

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
  /**
   * 运行期配置只读视图（与 `/api/agent/settings` 同源同形状，额外附
   * `effectiveModel`）。`tools[]` 是工具卡/审批卡的中文名与风险等级的**唯一**
   * 来源（A2A 的 `toolById` + `TOOL_META` 在 AIGX 由这个端点承担）。
   */
  getAgentConfig: (): Promise<ApiResponse<AgentConfigView>> =>
    request<ApiResponse<AgentConfigView>>('GET', `${API_BASE}/agent/config`),
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
};
