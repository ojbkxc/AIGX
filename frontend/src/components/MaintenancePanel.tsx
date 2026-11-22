// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//
// 维护任务明细面板（定时任务页「上次明细 / 重试」入口）。
// 对照 A2A ui-shared maintenance-*.tsx：
//   两类维护任务概览条 + 执行记录表 + 单次明细弹窗（逐项结果 + 单条快照）+ 重试。
//
// 与上游对齐的交互：
//   · 明细弹窗：状态筛选（全部/失败/成功/跳过，条数随提供商联动）+ 提供商筛选 +
//     每页 10 条分页 + 点行打开单条快照 + 失败项「去账号页处理」。
//   · 明细按 id 缓存最近 20 条，in-flight 去重；running/interrupted 记录跑完自动重取。
//   · 执行/重试后自动打开新记录明细，并按上游策略重置筛选。
//   · 记录表标出 retryOf「失败重试」、页脚给出保留期说明；翻页越界自动回退。
//
// 契约：GET  /api/maintenance?taskId=&source=&status=&keyword=&offset=&limit=
//       GET  /api/maintenance/records/{id}
//       POST /api/maintenance/run/{taskId}
//       POST /api/maintenance/records/{id}/retry  {itemId?}

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { useNavigate } from 'react-router-dom';
import { Loader2, RefreshCw, RotateCcw, X } from 'lucide-react';
import { api } from '../api';
import { useToast } from './Toast';
import { Badge, Button, EmptyState, Input, Pagination, Select } from './ui';
import { MAINTENANCE_TASKS } from '../types';
import type {
  MaintenanceDetailView,
  MaintenanceItemView,
  MaintenanceOverviewView,
  MaintenanceRunView,
  MaintenanceTaskId,
} from '../types';
import './MaintenancePanel.css';

/** beta.7 起维护任务含软件版本检查与社区插件检查（不含桌面专属 Magpie）。 */
const TASKS: readonly MaintenanceTaskId[] = MAINTENANCE_TASKS;
/** 记录列表每页条数 */
const LIMIT = 10;
/** 明细弹窗每页条数（上游 PAGE_SIZE） */
const DETAIL_PAGE_SIZE = 10;
/** 明细缓存保留条数（上游「最近 20 条」） */
const DETAIL_CACHE_SIZE = 20;

type Tone = 'success' | 'warning' | 'danger' | 'info' | 'neutral';
type ItemFilter = 'all' | 'failed' | 'refreshed' | 'skipped';

interface Query {
  taskId: string;
  source: string;
  status: string;
  keyword: string;
  offset: number;
}

const INITIAL_QUERY: Query = { taskId: '', source: '', status: '', keyword: '', offset: 0 };

/** 任务名（字面量 t()，i18n 扫描器才登记得到） */
function taskLabel(t: (s: string) => string, id: string): string {
  if (id === 'credentialMaintenance') return t('凭证维护');
  if (id === 'modelRefresh') return t('模型刷新');
  if (id === 'updateCheck') return t('软件检查更新');
  if (id === 'pluginCatalog') return t('社区插件检查');
  return id;
}

/** 明细表「对象」列标题：不同任务的检查对象不同。 */
function targetColumnTitle(t: (s: string) => string, id: string | undefined): string {
  if (id === 'updateCheck') return t('检查对象');
  if (id === 'pluginCatalog') return t('插件');
  return t('提供商');
}

/** 明细表「范围」列标题：账号 / 更新通道 / 当前版本。 */
function scopeColumnTitle(t: (s: string) => string, id: string | undefined): string {
  if (id === 'updateCheck') return t('更新通道');
  if (id === 'pluginCatalog') return t('当前版本');
  return t('账号');
}

/** 明细表「变化」列标题：凭证变化 / 模型目录 / 检查结果。 */
function changeColumnTitle(t: (s: string) => string, id: string | undefined): string {
  if (id === 'credentialMaintenance') return t('凭证变化');
  if (id === 'modelRefresh') return t('模型目录');
  return t('检查结果');
}

/** 思考/版本检查的结论短标签 → 本地化文案（识别失败时回退原文）。 */
function changeLabelText(t: (s: string, o?: Record<string, string | number>) => string, label: string): string {
  switch (label) {
    case '已是最新': return t('已是最新');
    case '检查失败': return t('检查失败');
    case '拉取失败': return t('拉取失败');
    case '自动更新失败': return t('自动更新失败');
    case '已停止支持': return t('已停止支持');
    case '信息异常': return t('信息异常');
    case '未在目录中': return t('未在目录中');
    case '已切换通道': return t('已切换通道');
    case '已关闭': return t('已关闭');
    case '目录已刷新': return t('目录已刷新');
    case '目录刷新失败': return t('目录刷新失败');
  }
  const found = /^发现 (.+)$/.exec(label);
  if (found) return t('发现 {{version}}', { version: found[1] });
  const upgraded = /^已升至 (.+)$/.exec(label);
  if (upgraded) return t('已升至 {{version}}', { version: upgraded[1] });
  const updatable = /^可更新 (.+)$/.exec(label);
  if (updatable) return t('可更新 {{version}}', { version: updatable[1] });
  const total = /^共 (\d+) 个$/.exec(label);
  if (total) return t('共 {{n}} 个', { n: total[1] });
  return t(label);
}

/** 「变化」列取值：模型目录条目数 / 凭证变化 / 版本检查结论。 */
function itemChange(
  t: (s: string, o?: Record<string, string | number>) => string,
  item: MaintenanceItemView,
  taskId: string | undefined,
): string {
  if (taskId === 'modelRefresh') {
    if (item.status !== 'refreshed') return t('未变更');
    return item.count == null ? '—' : t('{{n}} 个模型', { n: item.count });
  }
  if (taskId === 'credentialMaintenance') {
    if (item.status !== 'refreshed') return t('未变更');
    return item.expiresBefore && item.expiresAfter && item.expiresAfter > item.expiresBefore
      ? t('有效期已延长')
      : t('凭证已更新');
  }
  const label = item.changeLabel;
  if (!label) return item.status === 'refreshed' ? t('已完成') : t('未变更');
  if (label === '发现新版本') {
    return item.latestVersion
      ? t('发现新版本 {{version}}', { version: item.latestVersion })
      : t('发现新版本');
  }
  return changeLabelText(t, label);
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

/** 单条结果状态 → 徽章色调 + 文案（与明细筛选口径一致） */
function itemStatus(t: (s: string) => string, status: string): { tone: Tone; label: string } {
  switch (status) {
    case 'refreshed': return { tone: 'success', label: t('成功') };
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

/** 单条结果说明：优先用后端文案，缺失时按状态给出兜底（都是 t() 字面量） */
function describeItem(t: (s: string) => string, item: MaintenanceItemView): string {
  if (item.message) return item.message;
  if (item.fixed) return t('固定模型清单');
  if (item.status === 'refreshed') return t('已刷新');
  if (item.status === 'skipped') return t('本次无需更新');
  return t('未返回失败原因');
}

/** 一次执行是否含失败（含 models.dev 元信息失败） */
function hasFailures(run: MaintenanceRunView): boolean {
  return run.failed > 0 || run.metadata?.status === 'failed';
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

/** 保留最近 DETAIL_CACHE_SIZE 条记录的 id → value 映射（对象插入序即缓存顺序） */
function remember<T>(map: Record<string, T>, id: string, value: T): Record<string, T> {
  const next = Object.fromEntries(
    Object.entries(map).slice(-(DETAIL_CACHE_SIZE - 1)),
  ) as Record<string, T>;
  next[id] = value;
  return next;
}

interface MaintenancePanelProps {
  /** 作为定时任务页内嵌（隐藏自身标题） */
  embedded?: boolean;
}

/**
 * MaintenancePanel — 维护任务执行记录。
 *
 * 一屏三段：两类任务概览（是否正常、下次何时跑）→ 筛选条 → 执行记录表。
 * 记录点「明细」开弹窗看逐账号结果并可重试失败项；点「上次明细」看最近一轮。
 */
export default function MaintenancePanel({ embedded = false }: MaintenancePanelProps): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();
  const navigate = useNavigate();

  const [overview, setOverview] = useState<MaintenanceOverviewView | null>(null);
  const [query, setQuery] = useState<Query>(INITIAL_QUERY);
  const [search, setSearch] = useState('');
  const [loading, setLoading] = useState(true);
  const [notReady, setNotReady] = useState(false);
  const [error, setError] = useState('');
  const [expanded, setExpanded] = useState('');
  const [details, setDetails] = useState<Record<string, MaintenanceDetailView>>({});
  const [detailLoading, setDetailLoading] = useState('');
  const [detailError, setDetailError] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState<string | null>(null);

  const seq = useRef(0);
  const mounted = useRef(true);
  const operation = useRef(false);
  const queryRef = useRef<Query>(query);
  queryRef.current = query;
  const detailInFlight = useRef(new Set<string>());

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      seq.current += 1;
    };
  }, []);

  const load = useCallback(async (silent = false): Promise<void> => {
    const ticket = ++seq.current;
    const current = queryRef.current;
    if (!silent) setLoading(true);
    try {
      const res = await api.getMaintenanceOverview({
        taskId: current.taskId || undefined,
        source: current.source || undefined,
        status: current.status || undefined,
        keyword: current.keyword || undefined,
        offset: current.offset,
        limit: LIMIT,
      });
      if (!mounted.current || ticket !== seq.current) return;
      setOverview(res?.data ?? null);
      setNotReady(false);
      setError('');
    } catch (err) {
      if (!mounted.current || ticket !== seq.current) return;
      const msg = err instanceof Error ? err.message : String(err);
      const is404 = /404|not found/i.test(msg);
      setNotReady(is404);
      setError(is404 ? '' : msg);
      setOverview(null);
    } finally {
      if (mounted.current && ticket === seq.current) setLoading(false);
    }
  }, []);

  // 首次加载 + 筛选变化重取；20s 静默轮询，页面不可见时不打
  useEffect(() => {
    void load();
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
  }, [query, load]);

  // 关键词 300ms 防抖
  useEffect(() => {
    const timer = window.setTimeout(() => {
      setQuery((prev) => (prev.keyword === search.trim() ? prev : { ...prev, keyword: search.trim(), offset: 0 }));
    }, 300);
    return () => window.clearTimeout(timer);
  }, [search]);

  // 翻页越界回退：总数变小时回到最后一个合法页（对齐上游）
  useEffect(() => {
    if (!overview || loading) return;
    const total = overview.records?.total ?? 0;
    const lastOffset = Math.max(0, (Math.ceil(total / LIMIT) - 1) * LIMIT);
    if (query.offset > lastOffset) {
      setQuery((prev) => ({ ...prev, offset: lastOffset }));
    }
  }, [overview, loading, query.offset]);

  const fetchDetail = useCallback(async (id: string): Promise<void> => {
    if (!id || detailInFlight.current.has(id)) return;
    detailInFlight.current.add(id);
    setDetailLoading(id);
    setDetailError((prev) => remember(prev, id, ''));
    try {
      const res = await api.getMaintenanceRecord(id);
      if (!mounted.current) return;
      const data = res?.data ?? null;
      if (!data) throw new Error(t('维护记录不存在或已清理'));
      setDetails((prev) => remember(prev, id, data));
    } catch (err) {
      if (!mounted.current) return;
      const msg = err instanceof Error ? err.message : String(err);
      setDetailError((prev) => remember(prev, id, msg));
    } finally {
      detailInFlight.current.delete(id);
      if (mounted.current) setDetailLoading((prev) => (prev === id ? '' : prev));
    }
  }, [t]);

  // 打开某条明细：已缓存且不是执行中的直接复用，否则拉取
  const toggleDetail = (id: string): void => {
    if (id === expanded) {
      setExpanded('');
      return;
    }
    setExpanded(id);
    const cached = details[id];
    if (!cached || cached.status === 'running' || cached.status === 'interrupted') {
      void fetchDetail(id);
    }
  };

  // running/interrupted 记录跑完后自动重取明细
  useEffect(() => {
    if (!expanded) return;
    const cached = details[expanded];
    if (!cached || (cached.status !== 'running' && cached.status !== 'interrupted')) return;
    const run = overview?.records?.items?.find((item) => item.id === expanded);
    if (run && run.status !== cached.status) void fetchDetail(expanded);
  }, [overview, expanded, details, fetchDetail]);

  // 执行 / 重试：成功后自动打开新记录明细，并按上游重置全部筛选
  const execute = async (opts: { taskId?: MaintenanceTaskId; retryId?: string; itemId?: string }): Promise<void> => {
    const { taskId, retryId, itemId } = opts;
    if (operation.current || (!retryId && !taskId)) return;
    operation.current = true;
    const busyKey = retryId ? `retry:${retryId}${itemId ? `:${itemId}` : ''}` : `run:${taskId}`;
    setBusy(busyKey);
    try {
      const res = retryId
        ? await api.retryMaintenance(retryId, itemId)
        : await api.runMaintenance(taskId as MaintenanceTaskId);
      if (!mounted.current) return;
      const d = res?.data;
      if (d) {
        setDetails((prev) => remember(prev, d.id, d));
        setExpanded(d.id);
        setQuery(INITIAL_QUERY);
        queryRef.current = INITIAL_QUERY;
        setSearch('');
        const failed = hasFailures(d);
        addToast(
          d.status === 'running'
            ? t('维护已开始，结果会自动更新，可离开此页')
            : failed
              ? t('维护已结束，有失败项，请查看明细')
              : t('维护已完成，结果已保存'),
          failed ? 'error' : 'success',
        );
      }
      await load(true);
    } catch (err) {
      if (!mounted.current) return;
      addToast(err instanceof Error ? err.message : String(err), 'error');
      await load(true);
    } finally {
      operation.current = false;
      if (mounted.current) setBusy(null);
    }
  };

  // 概览「上次明细」：按上游把列表收窄到该类任务，再打开最近一轮
  const openLatest = (taskId: MaintenanceTaskId): void => {
    const latest = overview?.latest?.[taskId];
    if (!latest) return;
    setQuery({ ...INITIAL_QUERY, taskId });
    setSearch('');
    setExpanded(latest.id);
    void fetchDetail(latest.id);
  };

  const changeQuery = (patch: Partial<Query>): void => {
    setQuery((prev) => ({ ...prev, ...patch, offset: 0 }));
    setExpanded('');
  };

  // AIGX 没有「账号详情」独立路由：账号池在 /channels?tab=accounts（provider 参数
  // 只接受 channel_id，不能安全地传提供商 key）。退化为跳到账号池并复制账号 id。
  const goToAccount = (accountId: string): void => {
    navigate('/channels?tab=accounts');
    const copied = navigator.clipboard?.writeText(accountId);
    if (copied) {
      void copied
        .then(() => addToast(t('已复制账号 ID，请在账号池中查找')))
        .catch(() => addToast(t('请在账号池中按账号 ID 查找：{{id}}', { id: accountId }), 'warning'));
    }
  };

  const records = overview?.records?.items ?? [];
  const total = overview?.records?.total ?? 0;
  const totalPages = Math.max(1, Math.ceil(total / LIMIT));
  const page = Math.floor(query.offset / LIMIT) + 1;

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

      {/* 维护任务概览条（凭证维护 / 模型刷新 / 软件检查 / 社区插件检查） */}
      <div className="mtp-tasks">
        {TASKS.map((taskId) => {
          const state = overview?.tasks?.find((item) => item?.id === taskId) ?? null;
          const latest = latestByTask[taskId];
          const followsSoftware = taskId === 'pluginCatalog' && state?.checkMode === 'follow';
          return (
            <div key={taskId} className="mtp-task">
              <div className="mtp-task-main">
                <div className="mtp-task-name">
                  {taskLabel(t, taskId)}
                  {state?.running ? <Badge tone="info">{t('执行中')}</Badge> : null}
                  {followsSoftware ? <Badge tone="neutral">{t('跟随软件')}</Badge> : null}
                  {taskId === 'pluginCatalog' && state?.autoUpdate ? (
                    <Badge tone="success">{t('自动更新')}</Badge>
                  ) : null}
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
                  onClick={() => void execute({ taskId })}
                >
                  {busy === `run:${taskId}` ? <Loader2 size={12} className="mtp-spin" /> : <RotateCcw size={12} />}
                  {t('立即执行')}
                </Button>
                {latest ? (
                  <Button variant="outline" size="sm" disabled={detailLoading === latest.id} onClick={() => openLatest(taskId)}>
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
        <Select value={query.taskId} onChange={(e) => changeQuery({ taskId: e.target.value })} aria-label={t('任务')}>
          <option value="">{t('全部任务')}</option>
          {TASKS.map((taskId) => (
            <option key={taskId} value={taskId}>
              {taskLabel(t, taskId)}
            </option>
          ))}
        </Select>
        <Select value={query.source} onChange={(e) => changeQuery({ source: e.target.value })} aria-label={t('来源')}>
          <option value="">{t('全部来源')}</option>
          <option value="automatic">{t('自动')}</option>
          <option value="manual">{t('手动')}</option>
          <option value="passive">{t('被动')}</option>
        </Select>
        <Select value={query.status} onChange={(e) => changeQuery({ status: e.target.value })} aria-label={t('状态')}>
          <option value="">{t('全部状态')}</option>
          <option value="success">{t('成功')}</option>
          <option value="failed">{t('失败')}</option>
        </Select>
        <Input
          placeholder={t('搜索账号 / 提供商 / 错误')}
          value={search}
          onChange={(e) => setSearch(e.target.value)}
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
                <td>
                  {taskLabel(t, record.taskId)}
                  {record.retryOf ? (
                    <span className="mtp-retry-label" title={t('失败重试')}>
                      {t('失败重试')}
                    </span>
                  ) : null}
                </td>
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
                    <Button variant="outline" size="sm" disabled={detailLoading === record.id} onClick={() => toggleDetail(record.id)}>
                      {t('明细')}
                    </Button>
                    <Button
                      variant="outline"
                      size="sm"
                      disabled={busy !== null || record.status === 'running'}
                      onClick={() => void execute({ retryId: record.id })}
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

      {overview ? (
        <div className="mtp-retention">
          {t('维护记录保留 {{days}} 天，最多 {{count}} 次；仅保存结果，不保存密钥或令牌。', {
            days: overview.retentionDays,
            count: overview.maxRecords,
          })}
        </div>
      ) : null}

      {total > LIMIT && (
        <div className="mtp-pager">
          <span>{t('共 {{n}} 条 · 第 {{p}}/{{pages}} 页', { n: total, p: page, pages: totalPages })}</span>
          <div className="mtp-pager-ops">
            <Button
              variant="outline"
              size="sm"
              disabled={page <= 1}
              onClick={() => setQuery((prev) => ({ ...prev, offset: Math.max(0, prev.offset - LIMIT) }))}
            >
              {t('上一页')}
            </Button>
            <Button
              variant="outline"
              size="sm"
              disabled={page >= totalPages}
              onClick={() => setQuery((prev) => ({ ...prev, offset: prev.offset + LIMIT }))}
            >
              {t('下一页')}
            </Button>
          </div>
        </div>
      )}

      {expanded ? (
        <MaintenanceDetailDialog
          key={expanded}
          detail={details[expanded]}
          loading={detailLoading === expanded}
          error={detailError[expanded] ?? ''}
          busyKey={busy}
          onReload={() => void fetchDetail(expanded)}
          onClose={() => setExpanded('')}
          onRetryWhole={() => void execute({ retryId: expanded })}
          onRetryItem={(itemId) => void execute({ retryId: expanded, itemId })}
          onAccount={goToAccount}
        />
      ) : null}
    </section>
  );
}

interface DetailDialogProps {
  detail?: MaintenanceDetailView;
  loading: boolean;
  error: string;
  busyKey: string | null;
  onReload(): void;
  onClose(): void;
  onRetryWhole(): void;
  onRetryItem(itemId: string): void;
  onAccount(accountId: string): void;
}

/**
 * 单次执行的逐项明细弹窗。
 *
 * 高度固定、表体自己滚动；顶部两个筛选器（状态 / 提供商），状态上的条数随提供商
 * 联动；每页 10 条；点行打开单条快照。
 */
function MaintenanceDetailDialog({
  detail,
  loading,
  error,
  busyKey,
  onReload,
  onClose,
  onRetryWhole,
  onRetryItem,
  onAccount,
}: DetailDialogProps): JSX.Element {
  const { t } = useTranslation();
  const [filter, setFilter] = useState<ItemFilter>('all');
  const [provider, setProvider] = useState('');
  const [page, setPage] = useState(1);
  const [selected, setSelected] = useState<MaintenanceItemView | null>(null);
  // 明细一到手先替用户切到「失败」（仅一次，之后不再抢筛选）
  const appliedDefault = useRef(false);

  useEffect(() => {
    if (!detail || appliedDefault.current) return;
    appliedDefault.current = true;
    if (detail.failed > 0) setFilter('failed');
  }, [detail]);

  // 提供商选项来自本次结果本身（去重后按展示名排序）
  const providers = useMemo(() => {
    const map = new Map<string, string>();
    for (const item of detail?.results ?? []) map.set(item.provider, item.providerLabel || item.provider);
    return [...map.entries()].sort((left, right) => left[1].localeCompare(right[1]));
  }, [detail]);

  const results = detail?.results ?? [];
  const byProvider = results.filter((item) => !provider || item.provider === provider);
  const rows = byProvider.filter((item) => filter === 'all' || item.status === filter);
  const statusCount = (value: ItemFilter): number =>
    value === 'all' ? byProvider.length : byProvider.filter((item) => item.status === value).length;
  const pageCount = Math.max(1, Math.ceil(rows.length / DETAIL_PAGE_SIZE));
  const current = Math.min(page, pageCount);
  const shown = rows.slice((current - 1) * DETAIL_PAGE_SIZE, current * DETAIL_PAGE_SIZE);
  const stalled = !detail || detail.status === 'running' || detail.status === 'interrupted';
  const busy = busyKey !== null;
  const emptyText = !detail
    ? t('正在读取维护记录…')
    : detail.status === 'running'
      ? t('维护正在执行，完成后会显示逐项结果')
      : detail.status === 'interrupted'
        ? t('该次执行未完成，没有完整明细')
        : t('本轮没有符合条件的项目');

  return (
    <div className="mtp-overlay" role="dialog" aria-modal="true" aria-label={t('维护执行明细')}>
      <div className="mtp-dialog">
        <div className="mtp-dialog-head">
          <div>
            <h3>
              {t('维护执行明细')}
              {detail ? ` · ${taskLabel(t, detail.taskId)}` : ''}
            </h3>
            {detail ? (
              <p>
                {fmtTime(detail.startedAt)} · {runStatus(t, detail.status).label} · {fmtDuration(detail.durationMs)} ·{' '}
                {t('刷新 {{r}} · 跳过 {{s}} · 失败 {{f}}', {
                  r: detail.refreshed,
                  s: detail.skipped,
                  f: detail.failed,
                })}
                {!stalled ? ` · ${t('本轮共 {{n}} 项', { n: detail.total })}` : ''}
              </p>
            ) : (
              <p>{t('正在读取维护记录…')}</p>
            )}
            {detail?.error ? <p className="mtp-dialog-err">{detail.error}</p> : null}
          </div>
          <div className="mtp-dialog-actions">
            {detail && !stalled && hasFailures(detail) ? (
              <Button variant="outline" size="sm" disabled={busy} onClick={onRetryWhole}>
                {busyKey === `retry:${detail.id}` ? <Loader2 size={12} className="mtp-spin" /> : <RotateCcw size={12} />}
                {detail.taskId === 'modelRefresh' ? t('重试本轮') : t('仅重试失败项')}
              </Button>
            ) : null}
            <Button variant="outline" size="sm" onClick={onClose}>
              <X size={13} /> {t('关闭')}
            </Button>
          </div>
        </div>
        <div className="mtp-dialog-body">
          {error ? (
            <div className="mtp-detail-error">
              <span>{error}</span>
              <Button variant="outline" size="sm" onClick={onReload}>
                {t('重新加载')}
              </Button>
            </div>
          ) : null}
          {loading && !detail ? (
            <div className="mtp-loading">
              <Loader2 size={14} className="mtp-spin" />
              {t('正在读取维护记录…')}
            </div>
          ) : null}
          {detail ? (
            detail.results.length === 0 ? (
              <EmptyState icon="🗂️" message={emptyText} />
            ) : (
              <>
                <div className="mtp-detail-filters">
                  <div className="mtp-seg" role="group" aria-label={t('明细状态筛选')}>
                    {(
                      [
                        ['all', t('全部')],
                        ['failed', t('失败')],
                        ['refreshed', t('成功')],
                        ['skipped', t('跳过')],
                      ] as Array<[ItemFilter, string]>
                    ).map(([value, label]) => (
                      <button
                        key={value}
                        type="button"
                        className="mtp-seg-btn"
                        aria-pressed={filter === value}
                        onClick={() => {
                          setFilter(value);
                          setPage(1);
                        }}
                      >
                        {label}
                        <span className="mtp-seg-count">{statusCount(value)}</span>
                      </button>
                    ))}
                  </div>
                  <Select
                    value={provider}
                    onChange={(e) => {
                      setProvider(e.target.value);
                      setPage(1);
                    }}
                    aria-label={t('提供商')}
                  >
                    <option value="">{t('全部提供商')}</option>
                    {providers.map(([value, label]) => (
                      <option key={value} value={value}>
                        {label}
                      </option>
                    ))}
                  </Select>
                </div>
                <table className="mtp-table mtp-detail-table">
                  <thead>
                    <tr>
                      <th>{targetColumnTitle(t, detail.taskId)}</th>
                      <th>{scopeColumnTitle(t, detail.taskId)}</th>
                      <th>{t('结果')}</th>
                      <th>{changeColumnTitle(t, detail.taskId)}</th>
                      <th>{t('说明')}</th>
                      <th>{t('耗时')}</th>
                      <th style={{ width: 92 }}>{t('操作')}</th>
                    </tr>
                  </thead>
                  <tbody>
                    {shown.map((item) => {
                      const s = itemStatus(t, item.status);
                      const scope = item.currentVersion && detail.taskId === 'pluginCatalog'
                        ? item.currentVersion
                        : item.accountName || item.accountId || (detail.taskId === 'updateCheck' || detail.taskId === 'pluginCatalog' ? '—' : t('未使用账号'));
                      const change = itemChange(t, item, detail.taskId);
                      return (
                        <tr key={item.id} className="mtp-clickable-row" onClick={() => setSelected(item)}>
                          <td>{item.providerLabel || item.provider || '—'}</td>
                          <td title={item.accountId ?? undefined}>{scope}</td>
                          <td>
                            <Badge tone={s.tone}>{s.label}</Badge>
                          </td>
                          <td className="mtp-message">{change}</td>
                          <td className="mtp-message">{describeItem(t, item)}</td>
                          <td className="mtp-time">{fmtDuration(item.durationMs)}</td>
                          <td>
                            <Button
                              variant="outline"
                              size="sm"
                              onClick={(e) => {
                                e.stopPropagation();
                                setSelected(item);
                              }}
                            >
                              {t('查看详情')}
                            </Button>
                          </td>
                        </tr>
                      );
                    })}
                    {shown.length === 0 ? (
                      <tr>
                        <td colSpan={7}>
                          <EmptyState icon="🗂️" message={emptyText} />
                        </td>
                      </tr>
                    ) : null}
                  </tbody>
                </table>
                {detail.metadata ? (
                  <div className="mtp-task-meta" style={{ marginTop: 8, display: 'flex', alignItems: 'center', gap: 8, flexWrap: 'wrap' }}>
                    <span>{detail.metadata.providerLabel || taskLabel(t, detail.taskId)}</span>
                    <Badge tone={itemStatus(t, detail.metadata.status).tone}>
                      {itemStatus(t, detail.metadata.status).label}
                    </Badge>
                    <span>{describeItem(t, detail.metadata)}</span>
                  </div>
                ) : null}
                <div className="mtp-detail-foot">
                  <span className="mtp-muted">{t('重试生成新记录，不覆盖本次结果')}</span>
                  {rows.length > DETAIL_PAGE_SIZE ? (
                    <Pagination page={current} totalPages={pageCount} onChange={setPage} />
                  ) : null}
                </div>
              </>
            )
          ) : null}
        </div>
      </div>
      {selected && detail ? (
        <ItemSnapshotDialog
          item={selected}
          detail={detail}
          busy={busy}
          busyKey={busyKey}
          onClose={() => setSelected(null)}
          onRetryItem={onRetryItem}
          onAccount={onAccount}
        />
      ) : null}
    </div>
  );
}

interface ItemSnapshotDialogProps {
  item: MaintenanceItemView;
  detail: MaintenanceDetailView;
  busy: boolean;
  busyKey: string | null;
  onClose(): void;
  onRetryItem(itemId: string): void;
  onAccount(accountId: string): void;
}

/** 单条明细的结果快照：执行时的字段，不随账号后续变化而改变 */
function ItemSnapshotDialog({
  item,
  detail,
  busy,
  busyKey,
  onClose,
  onRetryItem,
  onAccount,
}: ItemSnapshotDialogProps): JSX.Element {
  const { t } = useTranslation();
  const credential = detail.taskId === 'credentialMaintenance';
  const model = detail.taskId === 'modelRefresh';
  const versioned = detail.taskId === 'updateCheck' || detail.taskId === 'pluginCatalog';
  const status = itemStatus(t, item.status);

  const scopeLabel = scopeColumnTitle(t, detail.taskId);
  const scopeValue = detail.taskId === 'pluginCatalog'
    ? item.currentVersion || item.accountName || item.accountId || '—'
    : item.accountName || item.accountId || (versioned ? '—' : t('未使用账号'));

  const facts: Array<[string, string]> = [
    [t('执行时间'), fmtTime(detail.startedAt)],
    [t('触发方式'), sourceLabel(t, detail.source)],
    [scopeLabel, scopeValue],
    [t('耗时'), fmtDuration(item.durationMs)],
  ];
  if (credential) {
    facts.push([t('刷新前有效期'), fmtTime(item.expiresBefore)]);
    facts.push([t('刷新后有效期'), fmtTime(item.expiresAfter)]);
  } else if (model) {
    facts.push([
      t('模型目录'),
      item.status !== 'refreshed'
        ? t('未变更')
        : item.count == null
          ? '—'
          : t('{{n}} 个模型', { n: item.count }),
    ]);
    facts.push([t('目录最近更新时间'), fmtTime(item.refreshedAt)]);
  } else if (versioned) {
    facts.push([t('检查结果'), itemChange(t, item, detail.taskId)]);
    if (item.currentVersion) facts.push([t('当前版本'), item.currentVersion]);
    if (item.latestVersion) facts.push([t('最新版本'), item.latestVersion]);
  }
  if (item.retryAt) facts.push([t('建议重试时间'), fmtTime(item.retryAt)]);

  // 单条重试只对凭证维护有意义：模型刷新按整轮重跑（后端没有独立明细项）。
  const canRetryItem = credential && item.status === 'failed' && !item.invalid;

  return (
    <div className="mtp-overlay mtp-overlay--nested" role="dialog" aria-modal="true" aria-label={t('单条维护明细')}>
      <div className="mtp-dialog mtp-dialog--snapshot">
        <div className="mtp-dialog-head">
          <div>
            <h3>
              {item.providerLabel || item.provider} · {taskLabel(t, detail.taskId)}
            </h3>
            <p>{t('执行时的结果快照，不随账号后续变化而改变')}</p>
          </div>
          <div className="mtp-dialog-actions">
            <Button variant="outline" size="sm" onClick={onClose}>
              <X size={13} /> {t('关闭')}
            </Button>
          </div>
        </div>
        <div className="mtp-dialog-body">
          <div className="mtp-snapshot-status">
            <Badge tone={status.tone}>{status.label}</Badge>
            {item.fixed ? <span className="mtp-muted">· {t('固定模型清单')}</span> : null}
          </div>
          <p className="mtp-snapshot-msg">{describeItem(t, item)}</p>
          <dl className="mtp-facts">
            {facts.map(([label, value]) => (
              <div key={label} className="mtp-fact">
                <dt>{label}</dt>
                <dd>{value}</dd>
              </div>
            ))}
          </dl>
          <p className="mtp-muted mtp-snapshot-note">{t('仅记录状态与结果，不展示密钥或令牌。')}</p>
          <div className="mtp-snapshot-actions">
            {item.accountId ? (
              <Button
                variant="outline"
                size="sm"
                className="mtp-snapshot-account"
                onClick={() => onAccount(item.accountId as string)}
              >
                {t('去账号页处理')}
              </Button>
            ) : null}
            {canRetryItem ? (
              <Button size="sm" disabled={busy} onClick={() => onRetryItem(item.id)}>
                {busyKey === `retry:${detail.id}:${item.id}` ? (
                  <Loader2 size={12} className="mtp-spin" />
                ) : (
                  <RotateCcw size={12} />
                )}
                {t('仅重试这一项')}
              </Button>
            ) : null}
          </div>
        </div>
      </div>
    </div>
  );
}
