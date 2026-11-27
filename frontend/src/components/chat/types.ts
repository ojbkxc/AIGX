import type { ChatStreamUsage, ToolCallState } from '../../types';
import type { AssistantTextFile } from './asst-attach';

export interface DebugMessage {
  role: 'user' | 'assistant';
  content: string;
  /** DeepSeek 式深度思考（SSE 的 reasoning_content，与正文分离） */
  reasoning?: string;
  /** 用户消息可选的多模态附件（URL 或 base64 data URI） */
  attachments?: Array<{ kind: 'image' | 'video' | 'audio'; url: string }>;
  /**
   * 用户消息可选的**文本附件**（JSON / .info，对齐 A2A 的 `UiMessage.files`）。
   *
   * 与 `attachments` 分开两个字段而不是并进一个 `kind: 'file'`：
   * `ChatBubble` 按 `kind` 直接渲染 `<img>/<video>/<audio>`，多一个 `file` 分支
   * 会渲染出一个空 div；而文本附件的展示是「文件名 + 体积」的小块，形态不同。
   *
   * ⚠️ 接线现状：读写链路（`ChatDebugger` 的分拣 / 粘贴 / 渲染、`Chat.tsx` 的
   * `extra.files` 落库）不在本次改动范围，字段先落结构，见交付清单。
   */
  files?: AssistantTextFile[];
  /** 本次回复聚合到的工具调用（#63；流式分片已按 index 拼接） */
  toolCalls?: ToolCallState[];
  /** 本次回复的用量收尾（#63；usage 终帧，旧上游可能缺省） */
  usage?: ChatStreamUsage;
  /** 本次回复的整体状态。有 toolCalls 时驱动卡片着色；无 toolCalls 时
   *  `interrupted` 会让气泡自身渲染「已停止生成」标记（#37）。
   *  标记走状态而非拼进 `content`：正文要保持纯净（回传历史 / 复制 / 朗读）。 */
  toolState?: 'streaming' | 'done' | 'interrupted' | 'error';
}
