import { describe, expect, it } from 'vitest';
import type { ImageJob, ImageJobsView } from '../../types/tools-image';
import {
  findImageJob,
  isDeduplicated,
  isTerminalPhase,
  jobImages,
  jobPhase,
  nextPollDelayMs,
  pollImageJob,
  retryablePhase,
} from './tool-images';

const job = (patch: Partial<ImageJob> = {}): ImageJob => ({
  job_id: 'j1',
  request_id: 'r1',
  status: 'running',
  operation: 'generation',
  model: 'm',
  provider: 'p',
  created_at: 0,
  expires_at: 999,
  ...patch,
});

const view = (jobs: ImageJob[]): ImageJobsView => ({
  jobs,
  storage: { used_bytes: 0, reserved_bytes: 0 },
});

describe('tool-images — 状态映射', () => {
  it('uncertain 单列，不并入失败（否则会诱导重复扣费）', () => {
    expect(jobPhase('running')).toBe('running');
    expect(jobPhase('succeeded')).toBe('done');
    expect(jobPhase('failed')).toBe('failed');
    expect(jobPhase('uncertain')).toBe('uncertain');
    expect(jobPhase('expired')).toBe('expired');
    expect(jobPhase(undefined)).toBe('missing');
  });

  it('只有 running / missing 不是终态', () => {
    expect(isTerminalPhase('running')).toBe(false);
    expect(isTerminalPhase('missing')).toBe(false);
    for (const phase of ['done', 'failed', 'uncertain', 'expired'] as const) {
      expect(isTerminalPhase(phase)).toBe(true);
    }
  });

  it('重试判据：死了且不是成功（uncertain 可重试但必须人来点）', () => {
    expect(retryablePhase('failed')).toBe(true);
    expect(retryablePhase('expired')).toBe(true);
    expect(retryablePhase('uncertain')).toBe(true);
    expect(retryablePhase('done')).toBe(false);
    expect(retryablePhase('running')).toBe(false);
  });
});

describe('tool-images — 取数与退避', () => {
  it('列表里按 job_id 过滤；空列表不抛', () => {
    expect(findImageJob(view([job()]), 'j1')?.request_id).toBe('r1');
    expect(findImageJob(view([job()]), 'nope')).toBeUndefined();
    expect(findImageJob(undefined, 'j1')).toBeUndefined();
  });

  it('后端 poll_after_seconds 优先，缺省指数退避且封顶 10s', () => {
    expect(nextPollDelayMs(job({ poll_after_seconds: 3 }), 0)).toBe(3000);
    expect(nextPollDelayMs(job({ poll_after_seconds: 999 }), 0)).toBe(60_000);
    expect(nextPollDelayMs(job({ poll_after_seconds: 0 }), 0)).toBe(1000);
    expect(nextPollDelayMs(undefined, 0)).toBe(1000);
    expect(nextPollDelayMs(undefined, 1)).toBe(2000);
    expect(nextPollDelayMs(undefined, 9)).toBe(10_000);
  });

  it('图片与去重标记都是只读派生', () => {
    expect(jobImages(job())).toEqual([]);
    expect(jobImages(job({ images: [{ id: 'i', mime: 'image/png', width: 1, height: 1, bytes: 1, url: 'u', ref: 'r' }] }))).toHaveLength(1);
    expect(isDeduplicated(job({ deduplicated_until: 2000 }), 1_000_000)).toBe(true);
    expect(isDeduplicated(job({ deduplicated_until: 500 }), 1_000_000)).toBe(false);
    expect(isDeduplicated(job())).toBe(false);
  });
});

describe('tool-images — 轮询状态机', () => {
  it('跑到终态即停，并回调每次快照', async () => {
    const snapshots: string[] = [];
    const sequence = [view([job()]), view([job()]), view([job({ status: 'succeeded' })])];
    let cursor = 0;
    const result = await pollImageJob({
      jobId: 'j1',
      list: () => Promise.resolve(sequence[Math.min(cursor++, sequence.length - 1)]),
      onUpdate: (_job, phase) => snapshots.push(phase),
      sleep: () => Promise.resolve(),
      now: () => 0,
    });
    expect(result.phase).toBe('done');
    expect(result.attempts).toBe(3);
    expect(snapshots).toEqual(['running', 'running', 'done']);
  });

  it('uncertain 视为终态停止轮询（不自动重试）', async () => {
    let calls = 0;
    const result = await pollImageJob({
      jobId: 'j1',
      list: () => {
        calls += 1;
        return Promise.resolve(view([job({ status: 'uncertain' })]));
      },
      sleep: () => Promise.resolve(),
      now: () => 0,
    });
    expect(result.phase).toBe('uncertain');
    expect(calls).toBe(1);
  });

  it('任务迟迟不出现：连续 miss 到上限后报错，phase 仍是 missing', async () => {
    const result = await pollImageJob({
      jobId: 'j1',
      list: () => Promise.resolve(view([])),
      sleep: () => Promise.resolve(),
      now: () => 0,
      maxMisses: 2,
    });
    expect(result.phase).toBe('missing');
    expect(result.error).toContain('任务未出现');
    expect(result.attempts).toBe(2);
  });

  it('取数失败可容忍一次，但连续失败会终止并带上原因', async () => {
    let calls = 0;
    const result = await pollImageJob({
      jobId: 'j1',
      list: () => {
        calls += 1;
        if (calls === 1) return Promise.reject(new Error('network down'));
        return Promise.resolve(view([job({ status: 'succeeded' })]));
      },
      sleep: () => Promise.resolve(),
      now: () => 0,
    });
    expect(result.phase).toBe('done');

    const failing = await pollImageJob({
      jobId: 'j1',
      list: () => Promise.reject(new Error('network down')),
      sleep: () => Promise.resolve(),
      now: () => 0,
      maxMisses: 2,
    });
    expect(failing.error).toContain('network down');
  });

  it('超时不伪装成失败：phase 停在 running，并标 timedOut', async () => {
    let clock = 0;
    const result = await pollImageJob({
      jobId: 'j1',
      list: () => Promise.resolve(view([job()])),
      sleep: (ms) => {
        clock += ms;
        return Promise.resolve();
      },
      now: () => clock,
      timeoutMs: 2_500,
    });
    expect(result.timedOut).toBe(true);
    expect(result.phase).toBe('running');
  });

  it('中止是用户意图：aborted 而不是 error', async () => {
    const controller = new AbortController();
    const result = await pollImageJob({
      jobId: 'j1',
      list: () => {
        controller.abort();
        return Promise.resolve(view([job()]));
      },
      sleep: () => Promise.resolve(),
      now: () => 0,
      signal: controller.signal,
    });
    expect(result.aborted).toBe(true);
    expect(result.error).toBeUndefined();
  });
});
