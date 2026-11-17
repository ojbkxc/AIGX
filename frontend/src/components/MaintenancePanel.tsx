// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//
// 维护任务明细面板（定时任务页「上次明细 / 重试」入口）。
// 对照 A2A ui-shared maintenance-panel.tsx / maintenance-types.ts：
//   两类维护任务概览条 + 执行记录表 + 单次明细弹窗（逐账号结果）+ 重试。
// 契约：GET  /api/maintenance?taskId=&source=&status=&keyword=&offset=&limit=
//       GET  /api/maintenance/records/{id}
//       POST /api/maintenance/run/{taskId}
//       POST /api/maintenance/records/{id}/retry  {itemId?}

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Loader2, RefreshCw, RotateCcw, X } from 'lucide-react';
import { api } from '../api';
import { useToast } from './Toast';
import { Badge, Button, EmptyState, Input, Select } from './ui';
import type {
  MaintenanceDetailView,
  MaintenanceOverviewView,
  MaintenanceRunView,
  MaintenanceTaskId,
} from '../types';
import './MaintenancePanel.css';

const TASKS: MaintenanceTaskId[] = ['credentialMaintenance', 'modelRefresh'];

type Tone = 'success' | 'warning' | 'danger' | 'info' | 'neutral';

/** 任务名（字面量 t()，i18n 扫描器才登记得到） */
function taskLabel(t: (s: string) => string, id: string): string {
  if (id === 'credentialMaintenance') return t('凭证维护');
  if (id === 'modelRefresh') return t('模型刷新');
  return id;
}

/** 执行状态 → 徽章色调 + 文案 */
function runStatus(t: (s: string) => string, status: string): { tone: Tone; label: string } {
  switch (status) {
    case 'running': return { tone: 'info', label: t('执行中') };
    case 'completed': return { tone: 'success', label: t('完成') };
    case 'partial': return { tone: 'warning', label: t('部分成功') };
    case 'failed': return { tone: 'danger', label: t('失败') };
    case 'skipped': return { tone: 'neutral', label: t('跳过') };
    case 'interrupted': return { tone: 'warning', label: t('中断') };
    default: return { tone: 'neutral', label: status };
  }
}

/** 单条结果状态 → 徽章色调 + 文案 */
function itemStatus(t: (s: string) => string, status: string): { tone: Tone; label: string } {
  switch (status) {
    case 'refreshed': return { tone: 'success', label: t('已刷新') };
    case 'skipped': return { tone: 'neutral', label: t('跳过') };
    default: return { tone: 'danger', label: t('失败') };
  }
}

/** 执行来源 → 文案 */
function sourceLabel(t: (s: string) => string, source: string): string {
  switch (source) {
    case 'automatic': return t('自动');
    case 'manual': return t('手动');
    case 'passive': return t('被动');
    default: return source;
  }
}

/** 毫秒时间戳 → 本地时间 */
function fmtTime(value: number | null | undefined): string {
  if (!value || !Number.isFinite(value)) return '—';
  try {
    return new Date(value).toLocaleString();
  } catch {
    return '—';
  }
}

/** 耗时 → 人话 */
function fmtDuration(ms: number | null | undefined): string {
  if (!ms || !Number.isFinite(ms)) return '—';
  if (ms >= 60_000) return `${(ms / 60_000).toFixed(1)} 分钟`;
  if (ms >= 1_000) return `${(ms / 1_000).toFixed(1)} 秒`;
  return `${Math.round(ms)} ms`;
}

interface MaintenancePanelProps {
  /** 作为定时任务页内嵌（隐藏自身标题） */
  embedded?: boolean;
}

/**
 * MaintenancePanel — 维护任务执行记录。
 *
 * 一屏三段：两类任务概览（是否正常、下次何时跑）→ 筛选条 → 执行记录表。
 * 记录点「明细」开弹窗看逐账号结果并可重试失败项；点「重试」整次重跑。
 */
export default function MaintenancePanel({ embedded = false }: MaintenancePanelProps): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();

  const [overview, setOverview] = useState<MaintenanceOverviewView | null>(null);
  const [loading, setLoading] = useState(true);
  const [notReady, setNotReady] = useState(false);
  const [error, setError] = useState('');
  const [taskFilter, setTaskFilter] = useState('');
  const [sourceFilter, setSourceFilter] = useState('');
  const [statusFilter, setStatusFilter] = useState('');
  const [keyword, setKeyword] = useState('');
  const [keywordApplied, setKeywordApplied] = useState('');
  const [offset, setOffset] = useState(0);
  const [detail, setDetail] = useState<MaintenanceDetailView | null>(null);
  const [detailLoading, setDetailLoading] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);
  const limit = 10;

  const seq = useRef(0);

  const load = useCallback(
    async (silent = false): Promise<void> => {
      const ticket = ++seq.current;
      if (!silent) setLoading(true);
      try {
        const res = await api.getMaintenanceOverview({
          taskId: taskFilter || undefined,
          source: sourceFilter || undefined,
          status: statusFilter || undefined,
          keyword: keywordApplied || undefined,
          offset,
          limit,
        });
        if (ticket !== seq.current) return;
        setOverview(res?.data ?? null);
        setNotReady(false);
        setError('');
      } catch (err) {
        if (ticket !== seq.current) return;
        const msg = err instanceof Error ? err.message : String(err);
        const is404 = /404|not found/i.test(msg);
        setNotReady(is404);
        setError(is404 ? '' : msg);
        setOverview(null);
      } finally {
        if (ticket === seq.current) setLoading(false);
      }
    },
    [taskFilter, sourceFilter, statusFilter, keywordApplied, offset],
  );

  useEffect(() => {
    void load();
  }, [load]);

  // 执行中的记录跑完后自动重取（20s 静默轮询；页面不可见时不打）
  useEffect(() => {
    const timer = window.setInterval(() => {
      if (document.hidden) return;
      void load(true);
    }, 20_000);
    const onVisible = (): void => {
      if (!document.hidden) void load(true);
    };
    document.addEventListener('visibilitychange', onVisible);
    return () => {
      window.clearInterval(timer);
      document.removeEventListener('visibilitychange', onVisible);
    };
  }, [load]);

  // 关键词 300ms 防抖
  useEffect(() => {
    const timer = window.setTimeout(() => {
      setKeywordApplied((prev) => (prev === keyword.trim() ? prev : keyword.trim()));
      setOffset(0);
    }, 300);
    return () => window.clearTimeout(timer);
  }, [keyword]);

  const openDetail = async (id: string): Promise<void> => {
    setDetailLoading(true);
    try {
      const res = await api.getMaintenanceRecord(id);
      setDetail(res?.data ?? null);
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    } finally {
      setDetailLoading(false);
    }
  };

  const runTask = async (taskId: MaintenanceTaskId): Promise<void> => {
    setBusy(`run:${taskId}`);
    try {
      const res = await api.runMaintenance(taskId);
      const d = res?.data;
      if (d) {
        addToast(
          t('执行完成：{{status}} · 刷新 {{r}} · 跳过 {{s}} · 失败 {{f}}', {
            status: d.status,
            r: d.refreshed,
            s: d.skipped,
            f: d.failed,
          }),
        );
      }
      await load(true);
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    } finally {
      setBusy(null);
    }
  };

  const retryRun = async (record: MaintenanceRunView, itemId?: string): Promise<void> => {
    setBusy(itemId ? `retry:${record.id}:${itemId}` : `retry:${record.id}`);
    try {
      const res = await api.retryMaintenance(record.id, itemId);
      const d = res?.data;
      if (d) {
        setDetail(d);
        addToast(
          t('重试完成：{{status}} · 刷新 {{r}} · 失败 {{f}}', {
            status: d.status,
            r: d.refreshed,
            f: d.failed,
          }),
        );
      }
      await load(true);
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    } finally {
      setBusy(null);
    }
  };

  const records = overview?.records?.items ?? [];
  const total = overview?.records?.total ?? 0;
  const totalPages = Math.max(1, Math.ceil(total / limit));
  const page = Math.floor(offset / limit) + 1;

  const latestByTask = useMemo(() => overview?.latest ?? {}, [overview]);

  if (notReady) {
    return (
      <section className="mtp-wrap">
        {!embedded && <h2 className="mtp-heading">{t('维护任务明细')}</h2>}
        <EmptyState
          icon="🛠️"
          message={t('维护任务接口尚未就绪（/api/maintenance）')}
          action={<p className="mtp-hint">{t('后端 maintenance 模块上线后，本段自动显示两类维护任务的执行记录、明细与重试。')}</p>}
        />
      </section>
    );
  }

  return (
    <section className="mtp-wrap">
      <div className="mtp-head">
        {!embedded && <h2 className="mtp-heading">{t('维护任务明细')}</h2>}
        <Button variant="outline" size="sm" onClick={() => void load()} disabled={loading}>
          {loading ? <Loader2 size={13} className="mtp-spin" /> : <RefreshCw size={13} />}
          {t('刷新')}
        </Button>
      </div>

      {error && <div className="error-message">{error}</div>}

      {/* 两类任务概览条 */}
      <div className="mtp-tasks">
        {TASKS.map((taskId) => {
          const state = overview?.tasks?.find((item) => item?.id === taskId) ?? null;
          const latest = latestByTask[taskId];
          return (
            <div key={taskId} className="mtp-task">
              <div className="mtp-task-main">
                <div className="mtp-task-name">
                  {taskLabel(t, taskId)}
                  {state?.running ? <Badge tone="info">{t('执行中')}</Badge> : null}
                  {latest ? (
                    <Badge tone={runStatus(t, latest.status).tone}>{runStatus(t, latest.status).label}</Badge>
                  ) : null}
                </div>
                <div className="mtp-task-meta">
                  {state
                    ? t('间隔 {{n}} 分钟 · 下次 {{t}}', {
                        n: state.interval,
                        t: fmtTime(state.nextRunAt),
                      })
                    : t('没有该任务的状态')}
                </div>
                {latest ? (
                  <div className="mtp-task-meta">
                    {t('上次执行 {{t}} · 刷新 {{r}} · 跳过 {{s}} · 失败 {{f}} · 耗时 {{d}}', {
                      t: fmtTime(latest.startedAt),
                      r: latest.refreshed,
                      s: latest.skipped,
                      f: latest.failed,
                      d: fmtDuration(latest.durationMs),
                    })}
                  </div>
                ) : null}
                {state?.lastError ? <div className="mtp-task-err">{state.lastError}</div> : null}
              </div>
              <div className="mtp-task-ops">
                <Button
                  variant="outline"
                  size="sm"
                  disabled={busy !== null || state?.running}
                  onClick={() => void runTask(taskId)}
                >
                  {busy === `run:${taskId}` ? <Loader2 size={12} className="mtp-spin" /> : <RotateCcw size={12} />}
                  {t('立即执行')}
                </Button>
                {latest ? (
                  <Button variant="outline" size="sm" disabled={detailLoading} onClick={() => void openDetail(latest.id)}>
                    {t('上次明细')}
                  </Button>
                ) : null}
              </div>
            </div>
          );
        })}
      </div>

      {/* 筛选条 */}
      <div className="mtp-filters">
        <Select value={taskFilter} onChange={(e) => { setTaskFilter(e.target.value); setOffset(0); }} aria-label={t('任务')}>
          <option value="">{t('全部任务')}</option>
          {TASKS.map((taskId) => (
            <option key={taskId} value={taskId}>
              {taskLabel(t, taskId)}
            </option>
          ))}
        </Select>
        <Select value={sourceFilter} onChange={(e) => { setSourceFilter(e.target.value); setOffset(0); }} aria-label={t('来源')}>
          <option value="">{t('全部来源')}</option>
          <option value="automatic">{t('自动')}</option>
          <option value="manual">{t('手动')}</option>
          <option value="passive">{t('被动')}</option>
        </Select>
        <Select value={statusFilter} onChange={(e) => { setStatusFilter(e.target.value); setOffset(0); }} aria-label={t('状态')}>
          <option value="">{t('全部状态')}</option>
          <option value="success">{t('成功')}</option>
          <option value="failed">{t('失败')}</option>
        </Select>
        <Input
          placeholder={t('搜索账号 / 提供商 / 错误')}
          value={keyword}
          onChange={(e) => setKeyword(e.target.value)}
          aria-label={t('搜索')}
        />
      </div>

      {/* 执行记录表 */}
      {loading && records.length === 0 ? (
        <div className="mtp-loading">
          <Loader2 size={14} className="mtp-spin" />
          {t('正在读取维护记录…')}
        </div>
      ) : records.length === 0 ? (
        <EmptyState icon="🗂️" message={t('暂无维护执行记录')} />
      ) : (
        <table className="mtp-table">
          <thead>
            <tr>
              <th>{t('开始时间')}</th>
              <th>{t('任务')}</th>
              <th>{t('来源')}</th>
              <th>{t('状态')}</th>
              <th>{t('刷新')}</th>
              <th>{t('跳过')}</th>
              <th>{t('失败')}</th>
              <th>{t('耗时')}</th>
              <th style={{ width: 140 }}>{t('操作')}</th>
            </tr>
          </thead>
          <tbody>
            {records.map((record) => (
              <tr key={record.id}>
                <td className="mtp-time">{fmtTime(record.startedAt)}</td>
                <td>{taskLabel(t, record.taskId)}</td>
                <td>{sourceLabel(t, record.source)}</td>
                <td>
                  <Badge tone={runStatus(t, record.status).tone}>{runStatus(t, record.status).label}</Badge>
                </td>
                <td className="mtp-num">{record.refreshed}</td>
                <td className="mtp-num">{record.skipped}</td>
                <td className={record.failed > 0 ? 'mtp-num mtp-fail' : 'mtp-num'}>{record.failed}</td>
                <td className="mtp-time">{fmtDuration(record.durationMs)}</td>
                <td>
                  <div className="mtp-row-ops">
                    <Button variant="outline" size="sm" disabled={detailLoading} onClick={() => void openDetail(record.id)}>
                      {t('明细')}
                    </Button>
                    <Button
                      variant="outline"
                      size="sm"
                      disabled={busy !== null || record.status === 'running'}
                      onClick={() => void retryRun(record)}
                    >
                      {busy === `retry:${record.id}` ? <Loader2 size={12} className="mtp-spin" /> : null}
                      {t('重试')}
                    </Button>
                  </div>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}

      {total > limit && (
        <div className="mtp-pager">
          <span>{t('共 {{n}} 条 · 第 {{p}}/{{pages}} 页', { n: total, p: page, pages: totalPages })}</span>
          <div className="mtp-pager-ops">
            <Button variant="outline" size="sm" disabled={page <= 1} onClick={() => setOffset(Math.max(0, offset - limit))}>
              {t('上一页')}
            </Button>
            <Button
              variant="outline"
              size="sm"
              disabled={page >= totalPages}
              onClick={() => setOffset(offset + limit)}
            >
              {t('下一页')}
            </Button>
          </div>
        </div>
      )}

      {/* 明细弹窗 */}
      {detail && (
        <div className="mtp-overlay" role="dialog" aria-modal="true" aria-label={t('维护执行明细')}>
          <div className="mtp-dialog">
            <div className="mtp-dialog-head">
              <div>
                <h3>
                  {t('维护执行明细')} · {taskLabel(t, detail.taskId)}
                </h3>
                <p>
                  {fmtTime(detail.startedAt)} · {runStatus(t, detail.status).label} · {fmtDuration(detail.durationMs)} ·{' '}
                  {t('刷新 {{r}} · 跳过 {{s}} · 失败 {{f}}', {
                    r: detail.refreshed,
                    s: detail.skipped,
                    f: detail.failed,
                  })}
                </p>
                {detail.error ? <p className="mtp-dialog-err">{detail.error}</p> : null}
              </div>
              <div className="mtp-dialog-actions">
                <Button
                  variant="outline"
                  size="sm"
                  disabled={busy !== null}
                  onClick={() => void retryRun(detail)}
                >
                  {busy === `retry:${detail.id}` ? <Loader2 size={12} className="mtp-spin" /> : <RotateCcw size={12} />}
                  {t('整次重试')}
                </Button>
                <Button variant="outline" size="sm" onClick={() => setDetail(null)}>
                  <X size={13} /> {t('关闭')}
                </Button>
              </div>
            </div>
            <div className="mtp-dialog-body">
              {detail.results.length === 0 ? (
                <EmptyState icon="🗂️" message={t('这次执行没有逐项结果')} />
              ) : (
                <table className="mtp-table">
                  <thead>
                    <tr>
                      <th>{t('账号')}</th>
                      <th>{t('提供商')}</th>
                      <th>{t('状态')}</th>
                      <th>{t('说明')}</th>
                      <th>{t('耗时')}</th>
                      <th style={{ width: 90 }}>{t('操作')}</th>
                    </tr>
                  </thead>
                  <tbody>
                    {detail.results.map((item) => (
                      <tr key={item.id}>
                        <td>{item.accountName || item.accountId || '—'}</td>
                        <td>{item.providerLabel || item.provider}</td>
                        <td>
                          <Badge tone={itemStatus(t, item.status).tone}>{itemStatus(t, item.status).label}</Badge>
                        </td>
                        <td className="mtp-message">{item.message || '—'}</td>
                        <td className="mtp-time">{fmtDuration(item.durationMs)}</td>
                        <td>
                          <Button
                            variant="outline"
                            size="sm"
                            disabled={busy !== null || item.status !== 'failed'}
                            onClick={() => void retryRun(detail, item.id)}
                          >
                            {busy === `retry:${detail.id}:${item.id}` ? (
                              <Loader2 size={12} className="mtp-spin" />
                            ) : null}
                            {t('重试')}
                          </Button>
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              )}
            </div>
          </div>
        </div>
      )}
    </section>
  );
}