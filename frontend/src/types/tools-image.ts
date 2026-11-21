/**
 * 图片生成服务前端类型（独立文件：不碰并行编辑中的 types/index.ts）。
 *
 * 契约锚点：AIGX `src/api/admin/tool_image_admin.rs` +
 * `src/tool_services/image_config.rs` / `image_runtime.rs` /
 * `image_jobs.rs`（字段名与后端 JSON 逐字段一致，均为 snake_case）。
 *
 * 端点：
 *   GET/PUT/DELETE /api/tools/image-generation/config  → ImageConfigView / ImageConfig
 *   GET            /api/tools/image-generation/jobs    → { jobs, storage }
 *   DELETE         /api/tools/image-generation/jobs/{id}
 *   GET            /api/tools/image-generation/files/{id}（管理员鉴权，二进制）
 */

export interface ImageTarget {
  provider: string;
  model: string;
  account_id: string;
}

export interface ImageGenerationConfig {
  generation: ImageTarget;
  editing: ImageTarget | null;
  max_images: number;
  max_concurrency: number;
  retention_hours: number;
  storage_mb: number;
}

/** 目录「可选模型」一行（后端 ImageModelRow） */
export interface ImageModelOption {
  id: string;
  provider: string;
  provider_label: string;
  enabled: boolean;
  generation: boolean;
  editing: boolean;
}

/** 目录「可选账号」一行（后端 ImageAccountRow） */
export interface ImageAccountOption {
  id: string;
  name: string;
  provider: string;
  enabled: boolean;
}

/** GET/PUT config 的管理视图（后端 management_view） */
export interface ImageConfigView {
  config: ImageGenerationConfig | null;
  defaults: ImageGenerationConfig;
  models: ImageModelOption[];
  accounts: ImageAccountOption[];
  generation_error: string | null;
  editing_error: string | null;
  timeout_seconds: number;
  runtime_available?: boolean;
  runtime_note?: string;
}

/** 生成结果的单张图片元信息（后缀由后端 job_result 注入 url/ref/requires_auth） */
export interface ImageFileMeta {
  id: string;
  mime: string;
  width: number;
  height: number;
  bytes: number;
  url: string;
  ref: string;
  requires_auth?: boolean;
}

export type ImageJobStatus = 'running' | 'succeeded' | 'failed' | 'uncertain' | 'expired';

/** 单个生图任务（后端 ImageJob + job_result 注入字段） */
export interface ImageJob {
  job_id: string;
  request_id: string;
  status: ImageJobStatus;
  operation: string;
  model: string;
  provider: string;
  created_at: number;
  updated_at?: number;
  expires_at: number;
  deduplicated_until?: number;
  error?: string | null;
  images?: ImageFileMeta[];
  account_id?: string;
  actual_account_name?: string;
  actual_model?: string;
  credit?: number | null;
  points_consumed?: number;
  usage?: unknown;
  poll_after_seconds?: number | null;
  guidance?: string;
}

export interface ImageJobsStorage {
  used_bytes: number;
  reserved_bytes: number;
}

export interface ImageJobsView {
  jobs: ImageJob[];
  storage: ImageJobsStorage;
}

/** 空目标（开启参考图编辑时的初始值） */
export const emptyImageTarget = (): ImageTarget => ({ provider: '', model: '', account_id: '' });
