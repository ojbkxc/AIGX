// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//
// 维护任务明细面板（定时任务页「上次明细 / 重试」入口）。
// 对照 A2A ui-shared maintenance-*.tsx：
//   四类维护任务概览条 + 执行记录表 + 单次明细弹窗（逐项结果 + 单条快照）+ 重试。
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
import { Badge, Button, EmptyState, Input, Pagination, Select, Switch } from './ui';
import { MAINTENANCE_TASKS } from '../types';
import type {
  MaintenanceDetailView,
  MaintenanceItemView,
  MaintenanceOverviewView,
  MaintenanceRunView,
  MaintenanceTaskId,
  MaintenanceTaskView,
  ScheduledTaskPatch,
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
  if (id === 'modelRefresh') return t('模型目录更新');
  if (id === 'updateCheck') return t('软件检查更新');
  if (id === 'pluginCatalog') return t('社区插件检查');
  return id;
}

/**
 * 任务说明（A2A `maintenance-format.ts::maintenanceDescription` 逐条对齐）。
 *
 * 左栏/概览条把它挂在任务名的 title 上 —— 名字只占一行，说明不占版面，
 * 但「这项到底维护什么」随时可查（A2A 同一取舍）。
 */
function taskDescription(t: (s: string) => string, id: string): string {
  if (id === 'credentialMaintenance') return t('仅维护过期或即将过期的凭证');
  if (id === 'modelRefresh') return t('逐个提供商更新模型目录，模型基础信息单独列出结果');
  if (id === 'updateCheck') return t('定期向官网检查软件新版本；与「更新设置」同步');
  if (id === 'pluginCatalog') return t('刷新社区插件目录并检查更新，支持自动更新已签名的插件');
  return '';
}

/** 概览条主按钮文案（A2A `maintenanceActionLabel`：只有模型目录叫「立即更新」）。 */
function actionLabel(t: (s: string) => string, id: string): string {
  return id === 'modelRefresh' ? t('立即更新') : t('立即检查');
}

/** 明细弹窗标题后缀（A2A `maintenance-details.tsx`：`任务名 · 开始时间`）。 */
function maintenanceHeading(t: (s: string) => string, id: string, startedAt: number): string {
  return `${taskLabel(t, id)} · ${fmtTime(startedAt)}`;
}

/** 触发方式全称（A2A `sourceLabel`：passive 叫「客户端触发」）。 */
function sourceFull(t: (s: string) => string, source: string): string {
  if (source === 'manual') return t('手动触发');
  if (source === 'passive') return t('客户端触发');
  return t('自动触发');
}

/** 执行状态文案（A2A `statusLabel`：与记录表徽章同一套词）。 */
function runStatusText(t: (s: string) => string, status: string): string {
  switch (status) {
    case 'running': return t('执行中…');
    case 'completed': return t('已完成');
    case 'partial': return t('部分失败');
    case 'failed': return t('失败');
    case 'skipped': return t('全部跳过');
    case 'interrupted': return t('执行中断');
    default: return status;
  }
}

/**
 * 一轮执行的「成功 / 跳过 / 失败」计数（A2A `maintenance-shared.tsx::RunCounts`）。
 *
 * 执行中与执行中断没有逐项结果 —— 给破折号并说明原因，不显示 0/0/0 让人
 * 误以为「跑了但全没成」。
 */
function RunCounts({ run, t }: { run: MaintenanceRunView; t: (s: string) => string }): JSX.Element {
  if (run.status === 'running' || run.status === 'interrupted') {
    return (
      <span
        className="mtp-muted"
        title={run.status === 'running' ? t('维护正在执行，完成后会显示逐项结果') : t('该次执行未完成，没有完整明细')}
      >
        —
      </span>
    );
  }
  return (
    <span className="mtp-counts">
      <span>
        {t('成功')} <b>{run.refreshed}</b>
      </span>
      <span className="mtp-counts-sep">·</span>
      <span>
        {t('跳过')} <b>{run.skipped}</b>
      </span>
      <span className="mtp-counts-sep">·</span>
      <span className={run.failed ? 'mtp-fail' : undefined}>
        {t('失败')} <b>{run.failed}</b>
      </span>
    </span>
  );
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

/**
 * 执行状态 → 徽章色调 + 文案。
 *
 * 文案与 A2A `statusLabel` 同一套词（已完成 / 部分失败 / 全部跳过 / 执行中断）：
 * 同一件事在概览条、记录表、明细弹窗三处叫法必须一致，否则「概览说部分失败、
 * 表格写部分成功」这类不一致会让人以为看的是两条记录。
 */
function runStatus(t: (s: string) => string, status: string): { tone: Tone; label: string } {
  switch (status) {
    case 'running': return { tone: 'info', label: runStatusText(t, status) };
    case 'completed': return { tone: 'success', label: runStatusText(t, status) };
    case 'partial': return { tone: 'warning', label: runStatusText(t, status) };
    case 'failed': return { tone: 'danger', label: runStatusText(t, status) };
    case 'skipped': return { tone: 'neutral', label: runStatusText(t, status) };
    case 'interrupted': return { tone: 'warning', label: runStatusText(t, status) };
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

/**
 * 执行来源 → 文案。
 *
 * A2A `sourceLabel` 是同一份：`manual` = 手动触发、`passive` = 客户端触发、
 * 其余 = 自动触发。**`passive` 是 AIGX 独有的一档**（`/v1/models` 列举触发的
 * 被动刷新），A2A 也有这个取值但过滤下拉只给自动/手动 —— AIGX 保留它并在
 * 下拉里单列，否则这类记录只能落进「自动」里被含糊掉。
 */
function sourceLabel(t: (s: string) => string, source: string): string {
  return sourceFull(t, source);
}

/**
 * 单条结果说明：优先用后端文案，缺失时按状态给出兜底（都是 t() 字面量）。
 *
 * 对齐 A2A `itemMessage`：同一句 message 在不同任务里含义不同（凭证「已续期」、
 * 目录「已获取最新清单」、插件「已完成检查」），所以兜底按 taskId 分派而不是
 * 一律写「已刷新」—— 后者会让明细表看起来像只服务凭证维护。
 */
function describeItem(t: (s: string) => string, item: MaintenanceItemView, taskId?: string): string {
  if (item.fixed) return t('该提供商使用固定模型清单');
  if (item.message) return item.message;
  if (item.status === 'refreshed') {
    if (taskId === 'credentialMaintenance') return t('已续期并确认凭证更新');
    if (taskId === 'modelRefresh') return t('已获取最新模型目录');
    if (taskId === 'updateCheck') return t('已完成版本检查');
    if (taskId === 'pluginCatalog') return t('已完成社区插件检查与更新');
    return t('已刷新');
  }
  if (item.status === 'skipped') return t('本次无需更新，沿用现有状态');
  return t('未返回失败原因');
}

/** 「最近一次执行」那一行（A2A `maintenance-summary.tsx::lastRun`）。 */
function lastRunText(
  t: (s: string, o?: Record<string, string | number>) => string,
  run: MaintenanceRunView | undefined,
  fallback: string | null | undefined,
): string {
  if (run) {
    return t('最近一次 · {{t}} · {{s}} · {{d}}', {
      t: fmtTime(run.startedAt),
      s: sourceFull(t, run.source),
      d: fmtDuration(run.durationMs),
    });
  }
  return fallback ? t('此前仅有汇总，尚无逐项记录') : t('尚未执行维护');
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
 * 一屏三段：四类任务概览（是否正常、下次何时跑）→ 筛选条 → 执行记录表。
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
  const [settingsOpen, setSettingsOpen] = useState(false);

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
          message={t('无法读取维护记录（接口返回 404）')}
          action={<p className="mtp-hint">{t('请确认当前后端版本已包含 /api/maintenance；刚升级后重启服务即可恢复。')}</p>}
        />
      </section>
    );
  }

  return (
    <section className="mtp-wrap">
      <div className="mtp-head">
        {!embedded && <h2 className="mtp-heading">{t('维护任务明细')}</h2>}
        <div style={{ marginLeft: 'auto', display: 'flex', gap: 8 }}>
          <Button
            variant="outline"
            size="sm"
            onClick={() => setSettingsOpen(true)}
            disabled={loading || !overview?.tasks?.length}
          >
            {t('维护设置')}
          </Button>
          <Button variant="outline" size="sm" onClick={() => void load()} disabled={loading}>
            {loading ? <Loader2 size={13} className="mtp-spin" /> : <RefreshCw size={13} />}
            {t('刷新')}
          </Button>
        </div>
      </div>

      {error && <div className="error-message">{error}</div>}

      {/* 维护任务概览条（凭证维护 / 模型目录更新 / 软件检查更新 / 社区插件检查）
          对齐 A2A maintenance-summary.tsx：每项一行 = 名称(title=说明) · 状态 ·
          最近一次 · 计数 · 计划(含冷却) · [查看结果 / 立即检查]。 */}
      <div className="mtp-tasks">
        {TASKS.map((taskId) => {
          const state = overview?.tasks?.find((item) => item?.id === taskId) ?? null;
          const latest = latestByTask[taskId];
          const followsSoftware = taskId === 'pluginCatalog' && state?.checkMode === 'follow';
          const running = Boolean(state?.running) || busy === `run:${taskId}`;
          const cooldownUntil = state?.enabled && state?.retryAt && state.retryAt > Date.now() ? state.retryAt : null;
          return (
            <div key={taskId} className="mtp-task">
              <div className="mtp-task-main">
                <div className="mtp-task-name" title={taskDescription(t, taskId)}>
                  {taskLabel(t, taskId)}
                  {running ? <Badge tone="info">{t('执行中')}</Badge> : null}
                  {followsSoftware && state?.enabled !== false ? <Badge tone="neutral">{t('跟随软件')}</Badge> : null}
                  {taskId === 'pluginCatalog' && state?.autoUpdate ? (
                    <Badge tone="success">{t('自动更新')}</Badge>
                  ) : null}
                  {latest || running ? (
                    <Badge tone={runStatus(t, running ? 'running' : latest!.status).tone}>
                      {runStatus(t, running ? 'running' : latest!.status).label}
                    </Badge>
                  ) : (
                    <Badge tone="neutral">{t('暂无记录')}</Badge>
                  )}
                </div>
                <div className="mtp-task-meta">{lastRunText(t, latest, state?.lastResult)}</div>
                {latest ? (
                  <div className="mtp-task-meta">
                    <RunCounts run={latest} t={t} />
                  </div>
                ) : null}
                <div className="mtp-task-meta">
                  {state ? (
                    state.enabled ? (
                      <>
                        {followsSoftware ? `${t('跟随软件')} · ` : ''}
                        {t('每 {{n}} 分钟', { n: state.interval })}
                        {' · '}
                        {t('下次执行 {{time}}', { time: fmtTime(state.nextRunAt) })}
                        {cooldownUntil ? (
                          <span className="mtp-cooldown">{` · ${t('冷却至 {{time}}', { time: fmtTime(cooldownUntil) })}`}</span>
                        ) : null}
                      </>
                    ) : followsSoftware ? (
                      t('跟随软件（已关闭）')
                    ) : (
                      t('自动维护已关闭')
                    )
                  ) : (
                    t('没有该任务的状态')
                  )}
                </div>
                {state?.lastError ? <div className="mtp-task-err">{state.lastError}</div> : null}
              </div>
              <div className="mtp-task-ops">
                <Button
                  variant="outline"
                  size="sm"
                  disabled={!latest || detailLoading === latest?.id}
                  onClick={() => openLatest(taskId)}
                >
                  {t('查看结果')}
                </Button>
                <Button
                  variant="outline"
                  size="sm"
                  disabled={busy !== null || Boolean(state?.running)}
                  onClick={() => void execute({ taskId })}
                >
                  {busy === `run:${taskId}` ? <Loader2 size={12} className="mtp-spin" /> : <RotateCcw size={12} />}
                  {running ? t('执行中…') : actionLabel(t, taskId)}
                </Button>
              </div>
            </div>
          );
        })}
      </div>

      {/* 筛选条（对齐 A2A maintenance-records.tsx 的四件套）：维护类型 / 执行结果 /
          触发方式 / 关键词。A2A 的「自动」档把 passive 一并纳入（两者都是非人工），
          AIGX 另给「被动」单列一档（后端 run_matches 支持，否则记录只能被含糊掉）。 */}
      <div className="mtp-filters">
        <Select value={query.taskId} onChange={(e) => changeQuery({ taskId: e.target.value })} aria-label={t('维护类型')}>
          <option value="">{t('全部类型')}</option>
          {TASKS.map((taskId) => (
            <option key={taskId} value={taskId}>
              {taskLabel(t, taskId)}
            </option>
          ))}
        </Select>
        <Select value={query.status} onChange={(e) => changeQuery({ status: e.target.value })} aria-label={t('执行结果')}>
          <option value="">{t('全部结果')}</option>
          <option value="failed">{t('有失败项')}</option>
          <option value="success">{t('无失败项')}</option>
        </Select>
        <Select value={query.source} onChange={(e) => changeQuery({ source: e.target.value })} aria-label={t('触发方式')}>
          <option value="">{t('全部触发方式')}</option>
          <option value="automatic">{t('自动触发')}</option>
          <option value="manual">{t('手动触发')}</option>
          <option value="passive">{t('客户端触发')}</option>
        </Select>
        <Input
          placeholder={t('搜索账号、提供商或插件')}
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          aria-label={t('搜索账号、提供商或插件')}
        />
      </div>

      {/* 执行记录表（对齐 A2A maintenance-records.tsx 的七列 + 底栏）：
          执行时间 / 维护类型(带「失败重试」标) / 触发方式 / 结果 / 执行汇总 / 耗时 / 操作。
          汇总列用同一枚 RunCounts 零件（执行中与中断给破折号，不显示 0/0/0）。 */}
      {loading && records.length === 0 ? (
        <div className="mtp-loading">
          <Loader2 size={14} className="mtp-spin" />
          {t('正在读取维护记录…')}
        </div>
      ) : records.length === 0 ? (
        <EmptyState
          icon="🗂️"
          message={
            query.taskId || query.source || query.status || query.keyword
              ? t('没有符合筛选条件的维护记录')
              : t('暂无维护记录，首次执行后会自动保存')
          }
        />
      ) : (
        <table className="mtp-table">
          <thead>
            <tr>
              <th>{t('执行时间')}</th>
              <th>{t('维护类型')}</th>
              <th>{t('触发方式')}</th>
              <th>{t('结果')}</th>
              <th>{t('执行汇总')}</th>
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
                <td>
                  <RunCounts run={record} t={t} />
                </td>
                <td className="mtp-time">{fmtDuration(record.durationMs)}</td>
                <td>
                  <div className="mtp-row-ops">
                    <Button variant="outline" size="sm" disabled={detailLoading === record.id} onClick={() => toggleDetail(record.id)}>
                      {t('查看明细')}
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
          <span>{t('共 {{n}} 次执行', { n: total })}</span>
          <span className="mtp-muted">{t('第 {{p}}/{{pages}} 页', { p: page, pages: totalPages })}</span>
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

      {settingsOpen && overview ? (
        <MaintenanceSettingsDialog
          tasks={(overview.tasks ?? []).filter((task): task is MaintenanceTaskView => task !== null)}
          onClose={() => setSettingsOpen(false)}
          onSaved={() => void load(true)}
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
              {detail ? ` ${maintenanceHeading(t, detail.taskId, detail.startedAt)}` : ''}
            </h3>
            {detail ? (
              <p>
                {!stalled ? `${t('本轮共 {{n}} 项', { n: detail.total })} · ` : ''}
                {sourceFull(t, detail.source)}
                {detail.durationMs != null ? ` · ${fmtDuration(detail.durationMs)}` : ''}
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
                      <th>{t('原因 / 说明')}</th>
                      {/* AIGX 扩展列：单条耗时（A2A 明细表没有这一列，但这里每条
                          都有 durationMs，删掉等于丢掉已有信息） */}
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
                          <td className="mtp-message">{describeItem(t, item, detail.taskId)}</td>
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
                              {item.status === 'failed' ? t('查看原因') : t('查看详情')}
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
                    <span>
                      {detail.taskId === 'pluginCatalog'
                        ? t('官网社区插件目录')
                        : `${t('模型基础信息')} · models.dev`}
                    </span>
                    <Badge tone={itemStatus(t, detail.metadata.status).tone}>
                      {itemStatus(t, detail.metadata.status).label}
                    </Badge>
                    <span>
                      {detail.taskId === 'modelRefresh' && detail.metadata.status === 'refreshed'
                        ? t('已更新 {{n}} 条', { n: detail.metadata.count ?? 0 })
                        : describeItem(t, detail.metadata, detail.taskId)}
                    </span>
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
    [t('触发方式'), sourceFull(t, detail.source)],
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

  /**
   * 失败项的原始原因（A2A `maintenanceRawDetail`）：后端 `message` 可能带完整
   * 上游报文，折叠在上方那句人话之下单独一块等宽展示 —— 排查要原文，读界面
   * 要人话，两者都留。
   */
  const rawDetail = item.status === 'failed' ? (item.message ?? '') : '';

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
          <p className="mtp-snapshot-msg">{describeItem(t, item, detail.taskId)}</p>
          {rawDetail ? <pre className="mtp-snapshot-raw">{rawDetail}</pre> : null}
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

interface MaintenanceSettingsProps {
  tasks: MaintenanceTaskView[];
  onClose(): void;
  onSaved(): void;
}

/**
 * 维护设置弹窗（对齐上游 `maintenance-settings.tsx`）。
 *
 * 4 个 AIGX 维护任务的开关 + 间隔，保存走 `PATCH /api/scheduled-tasks/{id}`
 * 的 `{enabled?, interval?}`。`pluginCatalog` 另有 `checkMode`（`follow` / `custom`
 * 两档，见 `ScheduledSettings.plugin_catalog_follow`）与 `autoUpdate`。
 * （跟随软件检查计划）；`autoUpdate` 落 `update.auto_apply`（`PATCH` 已支持）。
 *
 * 与 A2A `maintenance-settings.tsx` 的细节对齐：
 *   · 每项下面一行「这项定期做什么」（`settingSummary`），开关旁边不看说明也能懂；
 *   · `pluginCatalog` 多一个「跟随『软件检查更新』计划」开关：**跟随时隐藏自身
 *     开关与间隔**（间隔由软件计划决定，露出一个改不动的输入框只会误导）；
 *   · 跟随态下保存跳过间隔范围校验（间隔不落库，不校验）；
 *   · 有失败项时提示「部分设置可能已保存，请核对后重试」并刷新，**不关窗**
 *     —— 让用户看得见哪些没落地。
 */
function MaintenanceSettingsDialog({ tasks, onClose, onSaved }: MaintenanceSettingsProps): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();
  const [drafts, setDrafts] = useState<Record<string, { enabled: boolean; interval: string }>>(() =>
    Object.fromEntries(tasks.map((task) => [task.id, { enabled: task.enabled, interval: String(task.interval) }])),
  );
  const pluginTask = tasks.find((task) => task.id === 'pluginCatalog') ?? null;
  const [autoUpdateDraft, setAutoUpdateDraft] = useState<boolean>(pluginTask?.autoUpdate === true);
  const [followDraft, setFollowDraft] = useState<boolean>(pluginTask?.checkMode === 'follow');
  const softwareTask = tasks.find((task) => task.id === 'updateCheck') ?? null;
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const saving = useRef(false);

  const patchDraft = (id: string, next: Partial<{ enabled: boolean; interval: string }>): void =>
    setDrafts((prev) => ({ ...prev, [id]: { ...prev[id], ...next } }));

  const save = async (): Promise<void> => {
    if (saving.current) return;
    for (const task of tasks) {
      // 跟随态不校验间隔：该任务不落 interval（由软件计划决定），校验它等于
      // 拦一个不会生效的值。
      if (task.id === 'pluginCatalog' && followDraft) continue;
      const draft = drafts[task.id];
      const value = Number.parseInt(draft.interval, 10);
      if (!draft.interval || Number.isNaN(value) || value < task.min || value > task.max) {
        setError(
          t('{{name}}的间隔需在 {{min}}–{{max}} 分钟', {
            name: taskLabel(t, task.id),
            min: task.min,
            max: task.max,
          }),
        );
        return;
      }
    }
    saving.current = true;
    setBusy(true);
    setError('');
    let failed = 0;
    for (const task of tasks) {
      const draft = drafts[task.id];
      const interval = Number.parseInt(draft.interval, 10);
      const body: ScheduledTaskPatch = {};
      if (task.id === 'pluginCatalog') {
        if (followDraft !== (task.checkMode === 'follow')) {
          body.checkMode = followDraft ? 'follow' : 'custom';
        }
        if (autoUpdateDraft !== (task.autoUpdate === true)) body.autoUpdate = autoUpdateDraft;
        // 跟随态只发「跟随」意图，不发 enabled/interval（后端在 follow 下也会
        // 忽略 interval，这里少发一次比多存一个死值干净）。
        if (!followDraft) {
          if (draft.enabled !== task.enabled) body.enabled = draft.enabled;
          if (interval !== task.interval) body.interval = interval;
        }
      } else {
        if (draft.enabled !== task.enabled) body.enabled = draft.enabled;
        if (interval !== task.interval) body.interval = interval;
      }
      if (
        body.enabled === undefined
        && body.interval === undefined
        && body.autoUpdate === undefined
        && body.checkMode === undefined
      ) {
        continue;
      }
      try {
        const res = await api.patchScheduledTask(task.id, body);
        if (!res?.success) failed += 1;
      } catch {
        failed += 1;
      }
    }
    saving.current = false;
    setBusy(false);
    if (failed > 0) {
      addToast(t('部分设置可能已保存，请核对后重试'), 'error');
      onSaved();
      return;
    }
    addToast(t('维护设置已保存'));
    onSaved();
    onClose();
  };

  return (
    <div
      className="mtp-overlay"
      role="dialog"
      aria-modal="true"
      aria-label={t('维护设置')}
      onClick={busy ? undefined : onClose}
    >
      <div
        className="mtp-dialog"
        style={{ width: 'min(560px, 100%)' }}
        onClick={(event) => event.stopPropagation()}
      >
        <div className="mtp-dialog-head">
          <div>
            <h3>{t('维护设置')}</h3>
            <p>{t('自动维护与手动执行共用同一份结果记录')}</p>
          </div>
          <div className="mtp-dialog-actions">
            <Button variant="outline" size="sm" onClick={onClose} disabled={busy}>
              <X size={13} /> {t('关闭')}
            </Button>
          </div>
        </div>
        <div className="mtp-dialog-body">
          {error ? <div className="error-message">{error}</div> : null}
          {tasks.map((task) => {
            const draft = drafts[task.id];
            const isPlugin = task.id === 'pluginCatalog';
            const following = isPlugin && followDraft;
            return (
              <div key={task.id} style={{ padding: '10px 0', borderBottom: '1px solid var(--border-color)' }}>
                <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
                  <strong>{taskLabel(t, task.id)}</strong>
                  {following ? <Badge tone="neutral">{t('跟随软件')}</Badge> : null}
                  {following ? null : (
                    <span style={{ marginLeft: 'auto' }}>
                      <Switch
                        checked={draft.enabled}
                        onChange={(next) => patchDraft(task.id, { enabled: next })}
                        disabled={busy}
                        ariaLabel={taskLabel(t, task.id)}
                      />
                    </span>
                  )}
                </div>
                {/* 与 A2A `settingSummary` 同一行说明：这一项定期做什么 */}
                <p className="mtp-muted" style={{ margin: '4px 0 0' }}>
                  {taskDescription(t, task.id)}
                </p>
                {isPlugin ? (
                  <div style={{ display: 'flex', flexDirection: 'column', gap: 8, marginTop: 8 }}>
                    <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
                      <span style={{ color: 'var(--text-muted)' }}>{t('跟随「软件检查更新」计划')}</span>
                      <Switch
                        checked={followDraft}
                        onChange={(next) => {
                          setFollowDraft(next);
                          // 从跟随切回自定义：把间隔与开关起点取软件计划现值，
                          // 与 A2A 同一取舍（切过去不会突然变成 0/禁用）。
                          if (!next && softwareTask) {
                            patchDraft(task.id, {
                              enabled: softwareTask.enabled,
                              interval: draft.interval || String(softwareTask.interval),
                            });
                          }
                        }}
                        disabled={busy}
                        ariaLabel={t('跟随「软件检查更新」计划')}
                      />
                    </div>
                    {following ? (
                      <p className="mtp-muted" style={{ margin: 0 }}>
                        {softwareTask?.enabled
                          ? t('当前跟随软件计划：已开启，每 {{n}} 分钟', { n: softwareTask.interval })
                          : t('当前跟随软件计划：已关闭')}
                      </p>
                    ) : null}
                    <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
                      <span style={{ color: 'var(--text-muted)' }}>{t('自动更新已启用且已签名的插件')}</span>
                      <Switch
                        checked={autoUpdateDraft}
                        onChange={(next) => setAutoUpdateDraft(next)}
                        disabled={busy}
                        ariaLabel={t('自动更新已启用且已签名的插件')}
                      />
                    </div>
                  </div>
                ) : null}
                {following ? null : (
                  <div style={{ display: 'flex', alignItems: 'center', gap: 8, marginTop: 8 }}>
                    <span style={{ color: 'var(--text-muted)' }}>{t('执行间隔（分钟）')}</span>
                    <input
                      type="number"
                      className="form-input"
                      style={{ width: 96 }}
                      min={task.min}
                      max={task.max}
                      value={draft.interval}
                      disabled={busy || !draft.enabled}
                      onChange={(e) => patchDraft(task.id, { interval: e.target.value })}
                      aria-label={t('执行间隔（分钟）')}
                    />
                    <span style={{ color: 'var(--text-muted)' }}>
                      {t('可填 {{min}}–{{max}}', { min: task.min, max: task.max })}
                    </span>
                  </div>
                )}
              </div>
            );
          })}
          <p className="mtp-muted" style={{ marginTop: 10 }}>
            {t('关闭自动维护不影响手动执行，也不影响对话请求所需的按需续期。')}
          </p>
        </div>
        <div
          style={{
            display: 'flex',
            justifyContent: 'flex-end',
            gap: 8,
            padding: '12px 16px',
            borderTop: '1px solid var(--border-color)',
          }}
        >
          <Button variant="outline" size="sm" onClick={onClose} disabled={busy}>
            {t('取消')}
          </Button>
          <Button size="sm" onClick={() => void save()} disabled={busy}>
            {busy ? t('保存中…') : t('保存设置')}
          </Button>
        </div>
      </div>
    </div>
  );
}
