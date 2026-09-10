export interface DebugMessage {
  role: 'user' | 'assistant';
  content: string;
  /** Workspace_2B8939 式深度思考（SSE 的 reasoning_content，与正文分离） */
  reasoning?: string;
  /** 用户消息可选的多模态附件（URL 或 base64 data URI） */
  attachments?: Array<{ kind: 'image' | 'video' | 'audio'; url: string }>;
}
