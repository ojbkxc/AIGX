/**
 * 图片生成管理面端点（/api/tools/image-generation/*）。
 *
 * 独立 endpoints 文件（不并入 api/index.ts 的聚合对象，避免与并行 wave 的
 * 共享文件互卷）：组件直接 `import { imageToolApi }` 使用。
 *
 * 契约锚点：AIGX `src/api/admin/tool_image_admin.rs`。
 *   GET/PUT/DELETE /api/tools/image-generation/config
 *   GET            /api/tools/image-generation/jobs
 *   DELETE         /api/tools/image-generation/jobs/{id}
 *   GET            /api/tools/image-generation/files/{id}（管理员 Bearer 鉴权）
 *
 * 注意：config / jobs 直接返回裸 JSON（管理视图 / {jobs,storage}），
 * 不套 `{ success, data }` 外壳（与后端 Json(...) 一致）。
 */

import { API_BASE, authHeaders, request } from '../client';
import type {
  ImageConfigView,
  ImageFileMeta,
  ImageGenerationConfig,
  ImageJobsView,
} from '../../types/tools-image';

/** 带管理员 Bearer 拉取二进制图片（<img> 无法自带 Authorization 头）。 */
async function fetchImageBlob(url: string): Promise<Blob> {
  const res = await fetch(url, { headers: authHeaders() });
  if (!res.ok) {
    throw new Error(`图片下载失败（HTTP ${res.status}）`);
  }
  return res.blob();
}

/** 扩展名：mime → 后缀（预览/下载文件名用） */
export function imageExtension(mime: string): string {
  if (mime === 'image/jpeg') return 'jpg';
  if (mime === 'image/webp') return 'webp';
  if (mime === 'image/gif') return 'gif';
  return 'png';
}

/** 触发浏览器下载（已鉴权拿到的 blob）。 */
function saveBlob(blob: Blob, filename: string): void {
  const url = URL.createObjectURL(blob);
  const anchor = document.createElement('a');
  anchor.href = url;
  anchor.download = filename;
  document.body.append(anchor);
  anchor.click();
  anchor.remove();
  setTimeout(() => URL.revokeObjectURL(url), 60_000);
}

export const imageToolApi = {
  /** 读取配置 + 可选模型/账号目录 + 校验错误 */
  getConfig: (): Promise<ImageConfigView> =>
    request<ImageConfigView>('GET', `${API_BASE}/tools/image-generation/config`),

  /** 保存配置（形状 + 目录语义由后端校验，失败回 400 可读文案） */
  saveConfig: (config: ImageGenerationConfig): Promise<ImageConfigView> =>
    request<ImageConfigView>('PUT', `${API_BASE}/tools/image-generation/config`, config),

  /** 清除配置 */
  clearConfig: (): Promise<{ success: boolean }> =>
    request<{ success: boolean }>('DELETE', `${API_BASE}/tools/image-generation/config`),

  /** 最近任务 + 存储用量 */
  listJobs: (): Promise<ImageJobsView> =>
    request<ImageJobsView>('GET', `${API_BASE}/tools/image-generation/jobs`),

  /** 清理一个任务（生成中后端拒绝） */
  deleteJob: (id: string): Promise<{ success: boolean }> =>
    request<{ success: boolean }>(
      'DELETE',
      `${API_BASE}/tools/image-generation/jobs/${encodeURIComponent(id)}`,
    ),

  /** 鉴权拉取图片 blob（缩略图预览用；调用方负责 revokeObjectURL） */
  fetchBlob: (file: ImageFileMeta): Promise<Blob> => fetchImageBlob(file.url),

  /** 下载单张图片到本机 */
  downloadFile: async (file: ImageFileMeta): Promise<void> => {
    const blob = await fetchImageBlob(file.url);
    saveBlob(blob, `image-${file.id.slice(0, 12)}.${imageExtension(file.mime)}`);
  },
};
