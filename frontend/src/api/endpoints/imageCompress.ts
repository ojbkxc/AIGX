// 会话图片压缩设置端点（独立文件，由 api/index.ts barrel 合并进统一 `api` 对象）。
//
// 契约（后端由主控接线）：GET/PUT `/api/image-compress`，body 与响应都是
// `{ imageCompressEnabled, imageCompressMaxEdge, imageCompressQuality, imageCompressThresholdKb }`。
// PUT 允许部分字段（缺省项不动），全部校验通过才落盘；保存后对下一个请求立即生效。
//   · enabled 默认 true（开）；
//   · maxEdge 默认 2048，合法 256~8192；
//   · quality 默认 80，合法 40~95；
//   · thresholdKb 默认 512，合法 0~10240（0 = 所有内嵌图片都过一遍）。
// 只作用于对话链路：生图 / 图片编辑在转发层另有分支，不经过压缩。
import { API_BASE, request } from '../client';

export interface ImageCompressSettings {
  imageCompressEnabled: boolean;
  imageCompressMaxEdge: number;
  imageCompressQuality: number;
  imageCompressThresholdKb: number;
}

/** 响应形态：兼容裸对象与 `{data}` 信封。 */
export type ImageCompressPayload = Partial<ImageCompressSettings> & {
  data?: Partial<ImageCompressSettings>;
};

export const endpoints = {
  /** 读当前开关与三项参数 */
  getImageCompress: (): Promise<ImageCompressPayload> =>
    request<ImageCompressPayload>('GET', `${API_BASE}/image-compress`),
  /** 部分更新（只传改动的字段）；响应为新状态 */
  saveImageCompress: (patch: Partial<ImageCompressSettings>): Promise<ImageCompressPayload> =>
    request<ImageCompressPayload>('PUT', `${API_BASE}/image-compress`, patch),
};
