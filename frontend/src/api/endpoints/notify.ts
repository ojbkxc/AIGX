// Bark 通知渠道端点（独立文件，由 api/index.ts barrel 合并进统一 `api` 对象）。
//
// 仅「测试 Bark」需要独立端点；读取/保存通知配置复用 routing.ts 中既有的
// getNotifyConfig / updateNotifyConfig。
import { API_BASE, request } from '../client';
import type { ApiResponse } from '@/types';

export const endpoints = {
  /** 用已保存的 Bark 配置发送一条测试推送 */
  testBark: (): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('POST', `${API_BASE}/notify/test-bark`),
};
