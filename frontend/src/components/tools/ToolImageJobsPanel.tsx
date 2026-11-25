// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//
// 图片生成任务列表/详情（工具服务页 image-generation 服务卡内）。
// 对照 A2A tools-image-history.tsx：状态徽标 + 模型/时间 + 存储用量 +
// 图片缩略图（管理员鉴权拉 blob 预览）+ 单张下载 + 任务清理。
// 契约：GET /api/tools/image-generation/jobs、DELETE .../jobs/{id}、
//       GET .../files/{id}（均管理员鉴权，图片需带 Bearer 拉 blob）。

import { useCallback, useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Download, Loader2, RefreshCw, Trash2 } from 'lucide-react';
import { useToast } from '../Toast';
import { Badge, Button, EmptyState, Tooltip } from '../ui';
import ConfirmDialog, { type ConfirmState } from '../ConfirmDialog';
import { imageExtension, imageToolApi } from '../../api/endpoints/tools_image';
import type { ImageJob, ImageJobStatus, ImageJobsView } from '../../types/tools-image';
import { pollImageJob } from '../chat/tool-images';
import './ToolImagePanel.css';

const STATUS_TONE: Record<ImageJobStatus, 'success' | 'warning' | 'danger' | 'neutral' | 'info'> = {
  running: 'info',
  succeeded: 'success',
  failed: 'danger',
  uncertain: 'warning',
  expired: 'neutral',
};

function statusLabel(t: (s: string) => string, status: ImageJobStatus): string {
  switch (status) {
    case 'running': return t('生成中');
    case 'succeeded': return t('已完成');
    case 'failed': return t('失败');
    case 'uncertain': return t('结果不确定');
    case 'expired': return t('已过期或清理');
    default: return status;
  }
}

function mib(bytes: number): string {
  return (bytes / (1024 * 1024)).toFixed(1);
}

/**
 * ToolImageJobsPanel — 最近生图记录。
 *
 * 成功后每 10 秒静默刷新（仅页面可见时）；图片通过鉴权 fetch 取 blob 生成
 * 对象 URL 做缩略图（`<img>` 无法自带 Authorization 头）。卸载时统一 revoke。
 */
export default function ToolImageJobsPanel(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();

  const [data, setData] = useState<ImageJobsView | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  const [busy, setBusy] = useState('');
  const [previews, setPreviews] = useState<Record<string, string>>({});
  const [confirmState, setConfirmState] = useState<ConfirmState | null>(null);

  /** 已发起过预览拉取的 file id（避免轮询重复拉） */
  const requestedRef = useRef<Set<string>>(new Set());
  /** 最新 previews（卸载时 revoke，避免闭包过期） */
  const previewsRef = useRef<Record<string, string>>({});

  const load = useCallback(async (): Promise<void> => {
    try {
      const next = await imageToolApi.listJobs();
      setData(next);
      setError('');
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
    const timer = setInterval(() => {
      if (document.visibilityState === 'visible') void load();
    }, 10_000);
    return () => clearInterval(timer);
  }, [load]);

  /**
   * 进行中的任务逐个轮询（判据在 `chat/tool-images.ts::pollImageJob`）。
   *
   * ── 为什么要逐任务轮询，而不是只靠上面那个 10 秒整表刷新 ──────────
   * 整表刷新是**匀速**的：一次生图通常 10–40 秒，用户看到的就是「生成中」
   * 一格一格跳，最后一次性冒出来。逐任务轮询尊重后端给的 `poll_after_seconds`
   * （上游限速时它会变长，忽略它就是自杀），并且把「已中止 / 已超时 / 已取消」
   * 三种收尾分得清 —— 整表刷新只会让任务一直挂着 `running`。
   *
   * ── 三条不变量 ─────────────────────────────────────────────
   * ①`uncertain` 不是失败：后端明说「结果不确定」（上游可能已扣费但回包丢了），
   *   渲染成失败会诱导用户重试 → 重复扣费，所以它有自己的徽标与提示；
   * ②轮询**永不自动重新提交生成**：重试是用户显式动作；
   * ③超时后任务**仍在后台跑**：`pollImageJob` 返回 `timedOut` 而 phase 停在
   *   running，这里只把控制权交回整表刷新，不谎报失败。
   */
  const polledRef = useRef<Set<string>>(new Set());
  const [pollTick, setPollTick] = useState(0);

  useEffect(() => {
    if (!data) return;
    const running = data.jobs.filter((job) => job.status === 'running');
    for (const job of running) {
      if (polledRef.current.has(job.job_id)) continue;
      polledRef.current.add(job.job_id);
      void pollImageJob({
        jobId: job.job_id,
        list: () => imageToolApi.listJobs(),
        onUpdate: (next) => {
          if (!next) return;
          setData((prev) => {
            if (!prev) return prev;
            const at = prev.jobs.findIndex((item) => item.job_id === next.job_id);
            if (at < 0) return prev;
            const jobs = prev.jobs.slice();
            jobs[at] = next;
            return { ...prev, jobs };
          });
        },
      }).then((result) => {
        if (result.job && (result.phase === 'done' || result.phase === 'failed')) {
          addToast(result.phase === 'done' ? t('图片已生成') : t('生图失败：{{error}}', { error: result.error ?? result.job.error ?? '—' }), result.phase === 'done' ? 'success' : 'error');
        }
        if (result.phase === 'uncertain') {
          // 不报「失败」：结果不确定时要用户自己去核对，重试可能重复扣费
          addToast(t('生图结果不确定，请先确认是否已扣费再决定是否重试'), 'warning');
        }
        if (result.timedOut || result.error) {
          // 交回整表刷新：任务在服务端可能还在跑，等下一轮再判定。
          // 必须先把 job_id 从已轮询集合里摘掉 —— 否则下一次 tick 会被
          // 「已经轮询过」判据直接跳过，重试永远不生效。
          polledRef.current.delete(job.job_id);
          window.setTimeout(() => setPollTick((tick) => tick + 1), 30_000);
        }
        void load();
      });
    }
  }, [addToast, data, load, pollTick, t]);

  // 任务离开「生成中」后允许再次轮询（例如后端把它重置回 running）
  useEffect(() => {
    if (!data) return;
    const alive = new Set(data.jobs.filter((job) => job.status === 'running').map((job) => job.job_id));
    for (const id of [...polledRef.current]) {
      if (!alive.has(id)) polledRef.current.delete(id);
    }
  }, [data]);

  // 任务刷新后为尚未拉取的成功结果补缩略图
  useEffect(() => {
    if (!data) return;
    for (const job of data.jobs) {
      for (const file of job.images ?? []) {
        if (requestedRef.current.has(file.id)) continue;
        requestedRef.current.add(file.id);
        void imageToolApi
          .fetchBlob(file)
          .then((blob) => {
            const url = URL.createObjectURL(blob);
            setPreviews((prev) => ({ ...prev, [file.id]: url }));
          })
          .catch(() => {
            // 预览失败不阻断列表；允许下次刷新重试
            requestedRef.current.delete(file.id);
          });
      }
    }
  }, [data]);

  useEffect(() => {
    previewsRef.current = previews;
  }, [previews]);

  // 卸载时释放全部对象 URL
  useEffect(
    () => () => {
      Object.values(previewsRef.current).forEach((url) => URL.revokeObjectURL(url));
    },
    [],
  );

  const download = async (job: ImageJob, fileId: string): Promise<void> => {
    const file = (job.images ?? []).find((item) => item.id === fileId);
    if (!file) return;
    setBusy(fileId);
    try {
      await imageToolApi.downloadFile(file);
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    } finally {
      setBusy('');
    }
  };

  const remove = (job: ImageJob): void => {
    setConfirmState({
      title: t('清理生图任务'),
      message: t('清理这次生成的临时图片？已保存到本机的文件不会被删除，原提交编号也不会重新生图。'),
      confirmText: t('清理'),
      danger: true,
      onConfirm: async () => {
        setBusy(job.job_id);
        try {
          await imageToolApi.deleteJob(job.job_id);
          addToast(t('已清理生成任务'));
          await load();
        } catch (err) {
          addToast(err instanceof Error ? err.message : String(err), 'error');
        } finally {
          setBusy('');
        }
      },
    });
  };

  return (
    <div className="tip-jobs">
      <div className="tip-jobs-head">
        <div className="tip-jobs-title">
          <h3>{t('最近生成记录')}</h3>
          {data ? (
            <span className="tip-storage">
              {t('图片占用 {{used}} MiB，生成中预留 {{reserved}} MiB。', {
                used: mib(data.storage.used_bytes),
                reserved: mib(data.storage.reserved_bytes),
              })}
            </span>
          ) : null}
        </div>
        <Button variant="outline" size="sm" onClick={() => void load()} disabled={loading}>
          {loading ? <Loader2 size={12} className="tip-spin" /> : <RefreshCw size={12} />}
          {t('刷新')}
        </Button>
      </div>

      {error && <div className="error-message">{error}</div>}

      {loading && !data ? (
        <div className="tip-loading">
          <Loader2 size={14} className="tip-spin" />
          {t('正在读取生成记录…')}
        </div>
      ) : !data || data.jobs.length === 0 ? (
        <EmptyState icon="🖼️" message={t('暂无生图记录，连接 MCP 后即可让 Agent 生成图片。')} />
      ) : (
        <div className="tip-job-list">
          {data.jobs.map((job) => (
            <div key={job.job_id} className="tip-job">
              <div className="tip-job-head">
                <Badge tone={STATUS_TONE[job.status]}>{statusLabel(t, job.status)}</Badge>
                <span className="tip-job-model">{job.model}</span>
                <span className="tip-job-time">{new Date(job.created_at).toLocaleString()}</span>
                {job.status !== 'running' && job.status !== 'expired' ? (
                  <Tooltip label={t('清理任务临时图片（不重新生图）')}>
                    <Button
                      variant="outline"
                      size="sm"
                      className="tip-job-clean"
                      disabled={busy === job.job_id}
                      onClick={() => remove(job)}
                    >
                      <Trash2 size={12} /> {t('清理')}
                    </Button>
                  </Tooltip>
                ) : null}
              </div>

              <div className="tip-job-meta">
                {job.provider} · {job.request_id}
                {job.actual_account_name ? ` · ${job.actual_account_name}` : ''}
              </div>

              {job.status === 'succeeded' && (job.credit != null || job.points_consumed != null) ? (
                <div className="tip-job-meta">
                  {t('积分')}: {job.credit ?? job.points_consumed ?? '—'}
                </div>
              ) : null}

              {job.error ? <div className="tip-job-error">{job.error}</div> : null}

              {job.images && job.images.length > 0 ? (
                <div className="tip-images">
                  {job.images.map((file, index) => (
                    <div key={file.id} className="tip-image">
                      <div className="tip-image-thumb">
                        {previews[file.id] ? (
                          <img src={previews[file.id]} alt={t('生成图片 {{index}}', { index: index + 1 })} />
                        ) : (
                          <div className="tip-image-fallback">
                            <Loader2 size={14} className="tip-spin" />
                          </div>
                        )}
                      </div>
                      <div className="tip-image-foot">
                        <span className="tip-image-dim">
                          {file.width} × {file.height} · {imageExtension(file.mime).toUpperCase()}
                        </span>
                        <Button
                          variant="outline"
                          size="sm"
                          disabled={busy === file.id}
                          onClick={() => void download(job, file.id)}
                        >
                          {busy === file.id ? <Loader2 size={12} className="tip-spin" /> : <Download size={12} />}
                          {t('下载')}
                        </Button>
                      </div>
                    </div>
                  ))}
                </div>
              ) : null}

              {job.status === 'succeeded' && job.expires_at ? (
                <div className="tip-job-meta">
                  {t('保留至 {{time}}', { time: new Date(job.expires_at).toLocaleString() })}
                </div>
              ) : null}
            </div>
          ))}
        </div>
      )}

      <ConfirmDialog state={confirmState} onClose={() => setConfirmState(null)} />
    </div>
  );
}
