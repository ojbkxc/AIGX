/**
 * 生图任务轮询状态机（对齐 A2A 的 `job_id` 轮询口径）。
 *
 * ── AIGX 的形态与 A2A 不同 ──────────────────────────────────
 * A2A 有 `assistant-image:ID` 与「生成 → 轮询 → 展示」专用链路；AIGX 的生图是
 * 管理面能力（`/api/tools/image-generation/jobs`，见 `api/endpoints/tools_image.ts`），
 * **没有单任务 GET**，只能拉最近任务列表再按 `job_id` 过滤。所以这里把
 * 「取数」做成注入的 `list()`：轮询逻辑（终态判定 / 退避 / 总超时 / 中止）
 * 与传输解耦，能用假时钟单测，也不必在组件里手写 `setInterval`。
 *
 * ── 状态机的两条硬规矩 ───────────────────────────────────────
 * ①`uncertain` **不是失败**：后端明确表示「结果不确定」（上游可能已扣费但
 *   回包丢了）。把它渲染成失败会诱导用户重试 → 重复消耗额度。它单独一档。
 * ②轮询**永不自动重新提交生成**：重试是用户的显式动作（A2A 的
 *   `jobNeedsResubmit` 同理 —— 失败重试要人点，不自动烧额度）。
 */
import type { ImageJob, ImageJobsView, ImageJobStatus } from '../../types/tools-image';

/** 任务在界面上的四档（`uncertain` / `expired` 单列，不并入失败） */
export type ImageJobPhase = 'running' | 'done' | 'failed' | 'uncertain' | 'expired' | 'missing';

/** 后端状态 → 界面档 */
export function jobPhase(status: ImageJobStatus | string | undefined): ImageJobPhase {
  switch (status) {
    case 'running':
      return 'running';
    case 'succeeded':
      return 'done';
    case 'failed':
      return 'failed';
    case 'uncertain':
      return 'uncertain';
    case 'expired':
      return 'expired';
    default:
      return 'missing';
  }
}

/** 终态：轮询到此为止 */
export function isTerminalPhase(phase: ImageJobPhase): boolean {
  return phase !== 'running' && phase !== 'missing';
}

/** 「可以重试」= 任务已经死了且不是成功（`uncertain` 也可重试，但必须人工确认） */
export function retryablePhase(phase: ImageJobPhase): boolean {
  return phase === 'failed' || phase === 'expired' || phase === 'uncertain';
}

/** 从任务列表里取一条（后端只给列表，前端按 id 过滤） */
export function findImageJob(view: ImageJobsView | undefined | null, jobId: string): ImageJob | undefined {
  if (!view || !Array.isArray(view.jobs) || !jobId) return undefined;
  return view.jobs.find((job) => job.job_id === jobId);
}

/**
 * 下一次轮询等多久。
 *
 * 优先用后端给的 `poll_after_seconds`（上游限速时它会变长，忽略它就是自杀），
 * 缺省按尝试次数指数退避，上限 10 秒 —— 生图是分钟级任务，2 秒一次足够，
 * 再密只是浪费请求。
 */
export function nextPollDelayMs(job: ImageJob | undefined, attempt: number): number {
  const advised = typeof job?.poll_after_seconds === 'number' ? job.poll_after_seconds : undefined;
  if (advised && Number.isFinite(advised) && advised > 0) {
    return Math.min(60_000, Math.max(500, Math.round(advised * 1000)));
  }
  const safeAttempt = Math.max(0, Math.floor(attempt));
  return Math.min(10_000, 1_000 * 2 ** Math.min(4, safeAttempt));
}

const sleepDefault = (ms: number): Promise<void> =>
  new Promise((resolve) => {
    window.setTimeout(resolve, ms);
  });

export interface PollImageJobOptions {
  /** 取任务列表（生产传 `imageToolApi.listJobs`） */
  list: () => Promise<ImageJobsView>;
  jobId: string;
  /** 中止信号（组件卸载 / 用户点停止） */
  signal?: AbortSignal;
  /** 每次拿到新快照回调（含首次） */
  onUpdate?: (job: ImageJob | undefined, phase: ImageJobPhase) => void;
  /** 总时长上限（缺省 10 分钟：生图任务是分钟级，再长基本是上游卡死） */
  timeoutMs?: number;
  /** 连续多少次「列表拉取失败 / 任务还不在列表里」后放弃（缺省 5） */
  maxMisses?: number;
  /** 时钟注入（单测用） */
  now?: () => number;
  sleep?: (ms: number) => Promise<void>;
}

export interface PollImageJobResult {
  job?: ImageJob;
  phase: ImageJobPhase;
  attempts: number;
  /** 中止 / 超时 / 连续失败的原因（有值时 phase 仍可能是 running：见下） */
  error?: string;
  /** 用户中止（不是错误，界面按「已停止」处理） */
  aborted?: boolean;
  /** 超时（phase 保持 running：任务在服务端可能还在跑，界面提示「仍在后台」） */
  timedOut?: boolean;
}

/**
 * 轮询到终态。
 *
 * 三种结束方式都如实返回，不把「中止/超时」伪装成成功：
 *   · 终态 → `phase` 是 done/failed/uncertain/expired；
 *   · 中止 → `aborted: true`（调用方别报错，那是用户意图）；
 *   · 超时 / 连续 miss → `timedOut` / `error`，`phase` 停留在 `running`。
 *     任务在服务端**可能还在跑**，把它标成失败会让人以为白花了钱。
 */
export async function pollImageJob(options: PollImageJobOptions): Promise<PollImageJobResult> {
  const {
    list,
    jobId,
    signal,
    onUpdate,
    timeoutMs = 10 * 60_000,
    maxMisses = 5,
    now = () => Date.now(),
    sleep = sleepDefault,
  } = options;
  const startedAt = now();
  let attempts = 0;
  let misses = 0;
  let lastError = '';
  let job: ImageJob | undefined;
  let phase: ImageJobPhase = 'missing';
  for (;;) {
    if (signal?.aborted) return { job, phase, attempts, aborted: true };
    if (now() - startedAt > timeoutMs) {
      return {
        job,
        phase: phase === 'missing' ? 'missing' : 'running',
        attempts,
        timedOut: true,
        ...(lastError ? { error: lastError } : {}),
      };
    }
    attempts += 1;
    try {
      const view = await list();
      job = findImageJob(view, jobId);
      phase = jobPhase(job?.status);
      if (!job) {
        // 刚提交时后端可能还没落表：算 miss 不算失败
        misses += 1;
      } else {
        misses = 0;
        lastError = '';
      }
      onUpdate?.(job, phase);
      if (job && isTerminalPhase(phase)) return { job, phase, attempts };
      if (misses >= maxMisses) {
        return {
          job,
          phase,
          attempts,
          error: lastError || '任务未出现在任务列表里（可能已被清理）',
        };
      }
    } catch (failure) {
      misses += 1;
      lastError = failure instanceof Error ? failure.message : String(failure);
      if (misses >= maxMisses) return { job, phase, attempts, error: lastError };
      onUpdate?.(job, phase);
    }
    if (signal?.aborted) return { job, phase, attempts, aborted: true };
    await sleep(nextPollDelayMs(job, attempts));
  }
}

/** 任务里的图片（界面据此渲染，不给 base64、不编造路径） */
export function jobImages(job: ImageJob | undefined): NonNullable<ImageJob['images']> {
  return job && Array.isArray(job.images) ? job.images : [];
}

/** 任务是否是「同请求去重」命中的（后端 `deduplicated_until`），界面别当新任务 */
export function isDeduplicated(job: ImageJob | undefined, now = Date.now()): boolean {
  if (!job || typeof job.deduplicated_until !== 'number') return false;
  // 后端下发的是秒级时间戳（与 expires_at 同口径）
  return job.deduplicated_until * 1000 > now;
}
