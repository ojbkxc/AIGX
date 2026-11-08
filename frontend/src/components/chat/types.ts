import type { ChatStreamUsage, ToolCallState } from '../../types';

export interface DebugMessage {
  role: 'user' | 'assistant';
  content: string;
  /** Workspace_2B8939 式深度思考（SSE 的 reasoning_content，与正文分离） */
  reasoning?: string;
  /** 用户消息可选的多模态附件（URL 或 base64 data URI） */
  attachments?: Array<{ kind: 'image' | 'video' | 'audio'; url: string }>;
  /** 本次回复聚合到的工具调用（#63；流式分片已按 index 拼接） */
  toolCalls?: ToolCallState[];
  /** 本次回复的用量收尾（#63；usage 终帧，旧上游可能缺省） */
  usage?: ChatStreamUsage;
  /** 工具调用卡片整体状态（有 toolCalls 时才有意义） */
  toolState?: 'streaming' | 'done' | 'interrupted' | 'error';
}