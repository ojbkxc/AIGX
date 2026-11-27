/**
 * AIGX API客户端
 * 统一的API调用层，为前端应用提供类型安全的数据访问接口
 */

import type { ApiResponse, PromptTranslateResult, SchedulerStatusItem } from '@/types';
// 路由回放/粘滞会话类型独立成文件（types/index.ts 正被 #13 并行编辑，避免共享大文件互卷在制品）
// 路由多规则体系（#40）类型独立成文件（同上：不碰 types/index.ts）
// 桌面导入来源类型（#22；独立文件理由同上）
// 报表时区设置类型（#20；独立文件理由同上：types/index.ts 正被 #13 并行编辑）
// 会话图片压缩与视觉兼容类型（#7；独立文件，理由同上：types/index.ts 并行编辑）
// 路由工作台 / 特殊路由（压缩 / 标题 / 视觉兼容）类型（本波新增；独立文件，不改 types/index.ts）
// 工具调用 / usage 帧适配层（#63；回灌层未定，形状变更只改适配层）

import { API_BASE, request } from './client';

// design 1.4：传输层与 SSE 解析层已拆到 ./client / ./stream；此处 re-export
// 保持既有 `import { api, parseSseFrame } from '../api'` 路径不变。
export type { ReportWindow } from './client';
export type { RegisterOptions } from './endpoints/storage_reports';
export {
  directChannelChatStream,
  parseSseFrame,
  readSseResponse,
  testChannelChatStream,
} from './stream';

import { endpoints as storage_reportsEndpoints } from './endpoints/storage_reports';
import { endpoints as subscriptionsEndpoints } from './endpoints/subscriptions';
import { endpoints as users_modelsEndpoints } from './endpoints/users_models';
import { endpoints as ticketsEndpoints } from './endpoints/tickets';
import { endpoints as routingEndpoints } from './endpoints/routing';
import { endpoints as pluginsEndpoints } from './endpoints/plugins';
import { endpoints as agent_chatEndpoints } from './endpoints/agent_chat';
import { endpoints as prompts_publicEndpoints } from './endpoints/prompts_public';
import { endpoints as miscEndpoints } from './endpoints/misc';
import { endpoints as notifyBarkEndpoints } from './endpoints/notify';
import { endpoints as sidebarLayoutEndpoints } from './endpoints/sidebarLayout';
import { endpoints as imageCompressEndpoints } from './endpoints/imageCompress';
import { endpoints as assistantEndpoints } from './endpoints/assistant';
import { endpoints as oauthEndpoints } from './endpoints/oauth';

export const api = {
  ...storage_reportsEndpoints,
  ...subscriptionsEndpoints,
  ...users_modelsEndpoints,
  ...ticketsEndpoints,
  ...routingEndpoints,
  ...pluginsEndpoints,
  ...agent_chatEndpoints,
  ...prompts_publicEndpoints,
  ...miscEndpoints,
  ...notifyBarkEndpoints,
  ...sidebarLayoutEndpoints,
  ...imageCompressEndpoints,
  ...assistantEndpoints,
  ...oauthEndpoints,
};

// 助手流式通道与图片地址不是 `api` 对象方法（一个要逐帧回调、一个直接做 src），单独 re-export。
export { assistantChatStream, assistantImageFileUrl } from './endpoints/assistant';
export type {
  AssistantChatRequest,
  AssistantConfigWire,
  AssistantImageRequest,
  AssistantSessionDetail,
  AssistantSessionSummary,
} from './endpoints/assistant';

export default api;

/** 调度实时状态（/api/dashboard/scheduler_status，NetworkLayer 调度地图用） */
export async function getSchedulerStatus(): Promise<ApiResponse<SchedulerStatusItem>> {
  return request<ApiResponse<SchedulerStatusItem>>('GET', `${API_BASE}/dashboard/scheduler_status`);
}

/** 自环翻译提示词（/api/prompts/translate，走 AIGX 自己的渠道） */
export async function translatePrompts(
  items: Array<{ content: string }>,
  target = '简体中文',
): Promise<ApiResponse<PromptTranslateResult>> {
  return request<ApiResponse<PromptTranslateResult>>('POST', `${API_BASE}/prompts/translate`, {
    target,
    items,
  });
}
