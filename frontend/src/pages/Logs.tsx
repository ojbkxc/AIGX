import { useState, useEffect, useRef, type MouseEvent } from 'react';
import { useTranslation } from 'react-i18next';
import { Trash2, Eraser, EyeOff, Eye } from 'lucide-react';
import { api } from '../api';
import { isAdmin, fmtCompact } from '../lib/utils';
import { getFilter, setFilter } from '../lib/filterMemory';
import { useListKeyboard } from '../hooks/useListKeyboard';
import { useAutoRefresh } from '../hooks/useAutoRefresh';
import { useToast } from '../components/Toast';
import ConfirmDialog, { type ConfirmState } from '../components/ConfirmDialog';
import { RequestLogDetail, AuditLogDetail } from '../components/LogDetailDialogs';
import { Button, Card, Loading, EmptyState, Pagination } from '../components/ui';

export type LogTab = 'requests' | 'audits';
type LogView = 'table' | 'timeline';

export interface RequestLogItem {
  id: string | number;
  created_at?: number;
  user_id?: string;
  /** 后端解析的邮箱展示（user_id → email，解析失败缺省回退 user_id） */
  user_email?: string;
  key_id?: string;
  channel_id?: string;
  channel_name?: string;
  model?: string;
  origin_model?: string;
  input_tokens?: number;
  output_tokens?: number;
  cost?: number;
  channel_cost?: number;
  latency_ms?: number;
  status_code?: number;
  error_msg?: string;
  ip?: string;
  /** 请求/响应快照（log_body 开关开启时后端记录；仅 admin 详情展示） */
  debug?: {
    request_body?: string;
    response_body?: string;
    upstream_url?: string;
  };
  /** 对话快照（**默认记录**，不依赖 log_body）：管理员可在详情里直接看对话 */
  conversation?: {
    messages: Array<{ role: string; text: string; truncated?: boolean }>;
    system_prompt_chars?: number;
    truncated_history?: boolean;
  };
  /** 调度决策回放：候选渠道列表（仅 admin 下发，普通用户已被后端剥离） */
  candidate_channels?: string[];
  /** 被过滤的渠道及原因（仅 admin） */
  filtered_channels?: Array<{ channel_id: string; reason: string }>;
  /** 最终选中渠道（仅 admin） */
  selected_channel?: string;
}

export interface AuditLogItem {
  id: string | number;
  created_at?: number;
  admin_id?: string;
  /** 后端解析的邮箱展示（admin_id → email） */
  admin_email?: string;
  action?: string;
  target?: string;
  before?: string;
  after?: string;
}

/** key_id 特殊来源 → 可读标签（非真实 API Key） */
const SOURCE_LABELS: Record<string, string> = {
  chat_test: '渠道调试',
  agent: 'AI 运维',
  'playground:chat': 'Playground 对话',
  'playground:images': 'Playground 图片',
  'playground:tts': 'Playground TTS',
  'playground:transcriptions': 'Playground 转写',
};
export const sourceLabel = (keyId: string | undefined): string | null =>
  (keyId && SOURCE_LABELS[keyId]) ||
  (keyId?.startsWith('playground:') ? keyId.slice('playground:'.length) : null);

type LogItem = RequestLogItem | AuditLogItem;

interface Filters {
  user: string;
  model: string;
  channel: string;
  /** 状态：'' 全部 / 'ok' / 'error' */
  status: string;
  /** 关键词：模糊匹配模型/渠道/错误/用户 */
  keyword: string;
  start: string;
  end: string;
}

/** 请求日志筛选的落盘键（filterMemory：跨次启动记忆，四页各自一键） */
const LS_LOGS_FILTERS = 'aigx-logs-filters';
/** 筛选默认值（空串 = 全部 / 不限；记忆按此收窄，存坏回落默认） */
const DEFAULT_LOGS_FILTERS: Filters = { user: '', model: '', channel: '', status: '', keyword: '', start: '', end: '' };

export default function Logs(): JSX.Element {
  const admin = isAdmin();
  const { t } = useTranslation();
  const [tab, setTab] = useState<LogTab>('requests');
  const [view, setView] = useState<LogView>('table');
  const [logs, setLogs] = useState<LogItem[]>([]);
  const [total, setTotal] = useState(0);
  const [page, setPage] = useState(1);
  const size = 20;
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  const [exporting, setExporting] = useState<'json' | 'csv' | null>(null);
  const addToast = useToast();
  const [confirmState, setConfirmState] = useState<ConfirmState | null>(null);
  const [selected, setSelected] = useState<Set<string | number>>(new Set());
  const [batchDeleting, setBatchDeleting] = useState(false);
  /** 行点击打开的详情条目（null 关闭） */
  const [detail, setDetail] = useState<LogItem | null>(null);
  /** 在途请求面板（管理员：正在跑、尚未落日志的请求） */
  const [showInflight, setShowInflight] = useState(false);
  const [inflight, setInflight] = useState<import('../types').InflightRequestItem[]>([]);
  const [inflightLoading, setInflightLoading] = useState(false);
  /** 敏感信息脱敏开关（管理员：隐藏用户邮箱/渠道名/IP） */
  const [masked, setMasked] = useState(false);

  // 筛选条件的记忆：启动时从 localStorage 恢复（首查参数随之生效），变更即落盘
  const [filters, setFilters] = useState<Filters>(() => {
    const saved = getFilter(LS_LOGS_FILTERS, DEFAULT_LOGS_FILTERS);
    // user / channel 是管理员专属筛选维度：非管理员界面不渲染这两个输入，
    // 记忆里残留的旧值会让列表被看不见的筛选悄悄过滤（服务端照单全收），恢复时清掉
    return admin ? saved : { ...saved, user: '', channel: '' };
  });
  useEffect(() => {
    setFilter(LS_LOGS_FILTERS, filters);
  }, [filters]);

  // 键盘导航（cc-haha 列表键盘体系）：/ 聚焦用户筛选、↑↓/j k 行高亮、Enter 打开详情
  const { searchRef, rowRefs, activeIndex, setActiveIndex } = useListKeyboard(
    logs.length,
    (i) => { if (logs[i]) setDetail(logs[i]); },
    !loading && !detail && !confirmState,
  );

  // 仅 tab / 分页变化时自动加载；筛选条件由「查询」按钮显式触发，避免每键一请求
  useEffect(() => {
    void load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [tab, page, size]);

  // 切 tab / 翻页时清空选择，避免跨页选中混淆
  useEffect(() => {
    setSelected(new Set());
  }, [tab, page]);

  // 请求序号：快速翻页/切筛选时旧响应晚到不覆盖新数据
  const loadSeq = useRef(0);
  const load = async (silent = false) => {
    const seq = ++loadSeq.current;
    if (!silent) {
      setLoading(true);
      setError('');
    }
    try {
      let resTotal = 0;
      if (tab === 'requests') {
        const params: Record<string, string | number> = { page, size };
        if (filters.user) params.user = filters.user;
        if (filters.model) params.model = filters.model;
        if (filters.channel) params.channel = filters.channel;
        if (filters.status) params.status = filters.status;
        if (filters.keyword.trim()) params.keyword = filters.keyword.trim();
        // 时间范围筛选：datetime-local 值（本地时区）→ unix 秒
        if (filters.start) params.start = Math.floor(new Date(filters.start).getTime() / 1000);
        if (filters.end) params.end = Math.floor(new Date(filters.end).getTime() / 1000);
        const res = await api.listRequestLogs(params);
        if (seq !== loadSeq.current) return; // 已被更新的请求取代
        setLogs(Array.isArray(res?.data) ? (res.data as unknown as LogItem[]) : []);
        resTotal = res?.total || 0;
        setTotal(resTotal);
      } else {
        const res = await api.listAuditLogs({ page, size });
        if (seq !== loadSeq.current) return;
        setLogs(Array.isArray(res?.data) ? (res.data as unknown as LogItem[]) : []);
        resTotal = res?.total || 0;
        setTotal(resTotal);
      }
      // 删空末页唯一条目后钳回有效页（total 变小页码越界显示空页）
      const totalPages = Math.max(1, Math.ceil(resTotal / size));
      if (page > totalPages) {
        setPage(totalPages);
        return; // setPage 会触发 useEffect 重新 load
      }
    } catch (err) {
      if (seq !== loadSeq.current) return;
      if (silent) return; // 轮询失败不覆盖页面错误状态，等下一次或手动刷新
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      if (seq === loadSeq.current) setLoading(false);
    }
  };

  // 前端自动刷新：按「定时任务」页配置的 logsAutoRefresh/requestsAutoRefresh
  // 轮询本页（requests tab → 请求日志刷新，audits tab → 系统事件刷新）。
  // 页面不可见时跳过，配置改动最多滞后一个周期生效。
  const autoRefreshTaskId = tab === 'requests' ? 'requestsAutoRefresh' : 'logsAutoRefresh';
  useAutoRefresh(autoRefreshTaskId, () => {
    void load(true);
  });

  const handleBatchDelete = () => {
    if (selected.size === 0) return;
    const ids = Array.from(selected);
    const targetLabel = tab === 'requests' ? t('请求日志') : t('审计日志');
    setConfirmState({
      title: t('删除日志'),
      message: (
        <>
          {t('确定删除选中的')} <strong>{ids.length}</strong> {t('条')}{targetLabel}？{t('该操作不可撤销。')}
        </>
      ),
      confirmText: t('删除'),
      danger: true,
      onConfirm: async () => {
        setBatchDeleting(true);
        setError('');
        try {
          const res = tab === 'requests'
            ? await api.deleteRequestLogs(ids)
            : await api.deleteAuditLogs(ids);
          addToast(`${t('已删除')} ${res?.data?.removed ?? ids.length} ${t('条')}`);
          setSelected(new Set());
          await load();
        } catch (err) {
          setError(err instanceof Error ? err.message : String(err));
        } finally {
          setBatchDeleting(false);
        }
      },
    });
  };

  const handleClearAll = async () => {
    const targetLabel = tab === 'requests' ? t('请求日志') : t('审计日志');
    // 先取预览：告诉管理员「会删多少条、覆盖哪段时间」，
    // 避免一键清空后才发现删错了区间。预览失败不阻断（退回通用文案）。
    let preview: { would_remove: number; oldest?: number | null; newest?: number | null } | null = null;
    if (tab === 'requests') {
      try {
        const res = await api.clearPreview(0);
        preview = res?.data ?? null;
      } catch {
        preview = null;
      }
    }
    const rangeText = preview?.oldest && preview?.newest
      ? `${fmtTime(preview.oldest)} ~ ${fmtTime(preview.newest)}`
      : null;
    setConfirmState({
      title: t('清空日志'),
      message: (
        <>
          {t('确定清空全部')}{targetLabel}？
          {preview ? (
            <div style={{ marginTop: 8, fontSize: 12.5, color: 'var(--text-muted)' }}>
              {t('将删除 {{n}} 条', { n: preview.would_remove })}
              {rangeText ? `（${rangeText}）` : ''}
            </div>
          ) : null}
          {t('该操作不可撤销。')}
        </>
      ),
      confirmText: t('清空'),
      danger: true,
      onConfirm: async () => {
        setBatchDeleting(true);
        setError('');
        try {
          const res = tab === 'requests'
            ? await api.clearRequestLogs()
            : await api.clearAuditLogs();
          addToast(`${t('已清空')} ${res?.data?.removed ?? 0} ${t('条')}`);
          setSelected(new Set());
          if (page !== 1) {
            setPage(1); // useEffect 会重新 load，避免闭包旧 page 双请求
          } else {
            await load();
          }
        } catch (err) {
          setError(err instanceof Error ? err.message : String(err));
        } finally {
          setBatchDeleting(false);
        }
      },
    });
  };

  const toggleSelect = (id: string | number): void => {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };

  const toggleSelectAll = (): void => {
    setSelected((prev) => {
      if (prev.size === logs.length) return new Set();
      return new Set(logs.map((l) => l.id));
    });
  };

  const handleExport = async (format: 'json' | 'csv') => {
    try {
      setExporting(format);
      // 拼接当前生效的筛选参数，保证导出与页面筛选结果一致
    const qs = new URLSearchParams({ format });
    if (filters.user) qs.set('user', filters.user);
    if (filters.model) qs.set('model', filters.model);
    if (filters.channel) qs.set('channel', filters.channel);
    if (filters.start) qs.set('start', String(Math.floor(new Date(filters.start).getTime() / 1000)));
    if (filters.end) qs.set('end', String(Math.floor(new Date(filters.end).getTime() / 1000)));
      const token = localStorage.getItem('token');
      const res = await fetch(`/api/logs/requests/export?${qs.toString()}`, {
        headers: { 'Authorization': `Bearer ${token}` },
      });
      if (!res.ok) {
        const errData = await res.json().catch(() => null);
        throw new Error(errData?.error || errData?.message || `${t('导出失败')} (${res.status})`);
      }
      const blob = await res.blob();
      const url = URL.createObjectURL(blob);
      const a = document.createElement('a');
      a.href = url;
      a.download = `request_logs.${format}`;
      a.click();
      // Safari/Firefox 在同一帧内 revoke 会导致下载被取消，延迟到下一帧再释放
      setTimeout(() => URL.revokeObjectURL(url), 1000);
      addToast(t('导出成功'));
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setExporting(null);
    }
  };

  // 查询按钮触发筛选：已在第 1 页时直接加载，否则重置页码由 useEffect 自动加载（保证单次请求）
  const handleSearch = (e: MouseEvent<HTMLButtonElement>) => {
    e.preventDefault();
    if (page === 1) {
      void load();
    } else {
      setPage(1);
    }
  };

  const fmtTime = (ts: number | undefined): string => (ts ? new Date(ts * 1000).toLocaleString() : '—');
  const totalPages = Math.ceil(total / size);
  const loadInflight = async (): Promise<void> => {
    setInflightLoading(true);
    try {
      const res = await api.listInflightRequests();
      setInflight((res?.data ?? []) as import('../types').InflightRequestItem[]);
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    } finally {
      setInflightLoading(false);
    }
  };

  const handleTerminateInflight = (id: string): void => {
    setConfirmState({
      title: t('终止在途请求'),
      message: t('将中断这条请求的上游流并记为失败。确定继续？'),
      confirmText: t('终止'),
      danger: true,
      onConfirm: async () => {
        try {
          await api.terminateInflightRequest(id);
          addToast(t('已终止'));
          await loadInflight();
        } catch (err) {
          addToast(err instanceof Error ? err.message : String(err), 'error');
        }
      },
    });
  };

  const isRequest = (_l: LogItem): _l is RequestLogItem => tab === 'requests';

  /** 脱敏显示：邮箱/渠道/IP 等敏感值打码 */
  const mask = (v: string | undefined): string => (v ? '••••' : '—');
  const userDisplay = (email: string | undefined, id: string | undefined): string => {
    if (masked) return mask(email ?? id);
    return email || id || '—';
  };

  // ── 时间线聚合：当前页日志按小时分桶（成功/失败计数 + 总费用） ──
  const timelineBuckets = (() => {
    const map = new Map<number, { ok: number; fail: number; cost: number; models: Set<string> }>();
    for (const l of logs) {
      if (!isRequest(l) || !l.created_at) continue;
      const hour = Math.floor(l.created_at / 3600) * 3600;
      const b = map.get(hour) || { ok: 0, fail: 0, cost: 0, models: new Set<string>() };
      if ((l.status_code ?? 0) < 400) b.ok += 1;
      else b.fail += 1;
      b.cost += l.cost ?? 0;
      if (l.model) b.models.add(l.model);
      map.set(hour, b);
    }
    return Array.from(map.entries()).sort((a, b) => a[0] - b[0]);
  })();

  const renderTimeline = (): JSX.Element => {
    const buckets = timelineBuckets;
    if (buckets.length === 0) return <EmptyState message={t('暂无时间线数据')} />;
    const W = 720;
    const H = 160;
    const PAD_L = 46;
    const PAD_B = 26;
    const PAD_T = 12;
    const PAD_R = 12;
    const plotW = W - PAD_L - PAD_R;
    const plotH = H - PAD_T - PAD_B;
    const maxCount = Math.max(1, ...buckets.map(([, b]) => b.ok + b.fail));
    const barGap = 3;
    const barW = Math.max(2, Math.floor((plotW - barGap * (buckets.length - 1)) / buckets.length));
    const yFor = (n: number): number => PAD_T + plotH - (n / maxCount) * plotH;
    const ticks = [0, Math.round(maxCount / 2), maxCount];
    return (
      <div className="log-timeline">
        <svg viewBox={`0 0 ${W} ${H}`} preserveAspectRatio="none" style={{ width: '100%', height: 'auto' }} role="img" aria-label={t('请求时间线')}>
          {ticks.map((tk) => (
            <g key={tk}>
              <line x1={PAD_L} y1={yFor(tk)} x2={W - PAD_R} y2={yFor(tk)} stroke="var(--border-color)" strokeWidth={1} />
              <text x={PAD_L - 6} y={yFor(tk) + 3} fontSize={9} fill="var(--text-muted)" textAnchor="end">{tk}</text>
            </g>
          ))}
          {buckets.map(([hour, b], i) => {
            const x = PAD_L + i * (barW + barGap);
            const okH = (b.ok / maxCount) * plotH;
            const failH = (b.fail / maxCount) * plotH;
            const totalH = okH + failH;
            const label = new Date(hour * 1000).toLocaleString(undefined, { month: '2-digit', day: '2-digit', hour: '2-digit' });
            return (
              <g key={hour} data-hour={hour}>
                <title>{`${label} — ${t('成功')} ${b.ok} / ${t('失败')} ${b.fail}`}</title>
                <rect x={x} y={PAD_T + plotH - totalH} width={barW} height={totalH} rx={2} fill="rgba(47,111,237,0.25)" />
                <rect x={x} y={PAD_T + plotH - okH} width={barW} height={okH} rx={2} fill="#34d399" />
                {b.fail > 0 && <rect x={x} y={PAD_T + plotH - totalH} width={barW} height={failH} rx={2} fill="rgba(239,68,68,0.8)" />}
                {buckets.length <= 14 && (
                  <text x={x + barW / 2} y={H - PAD_B + 12} fontSize={8} fill="var(--text-muted)" textAnchor="middle">{label}</text>
                )}
              </g>
            );
          })}
        </svg>
        <div className="log-timeline-legend">
          <span><i style={{ background: '#34d399' }} />{t('成功')}</span>
          <span><i style={{ background: 'rgba(239,68,68,0.8)' }} />{t('失败')}</span>
          <span className="log-timeline-note">{t('当前页日志按小时聚合')}</span>
        </div>
      </div>
    );
  };

  return (
    <div>
      <div className="page-header">
        <h1>{t('日志与审计')}</h1>
        <p>{t('请求日志检索与管理员操作审计')}</p>
      </div>

      {error && <div className="error-message">{error}</div>}

      <Card bodyClassName="">
        <div style={{ display: 'flex', gap: 12, flexWrap: 'wrap', alignItems: 'center' }}>
          <Button variant={tab === 'requests' ? 'primary' : 'outline'} onClick={() => { setTab('requests'); setPage(1); setView('table'); }}>
            {t('请求日志')}
          </Button>
          {admin && (
            <Button variant={tab === 'audits' ? 'primary' : 'outline'} onClick={() => { setTab('audits'); setPage(1); setView('table'); }}>
              {t('审计日志')}
            </Button>
          )}
          {admin && (
            <Button
              variant={showInflight ? 'primary' : 'outline'}
              onClick={() => {
                const next = !showInflight;
                setShowInflight(next);
                if (next) void loadInflight();
              }}
            >
              {t('在途请求')}
            </Button>
          )}
          {tab === 'requests' && (
            <>
              <Button variant="outline" size="sm" onClick={() => void handleExport('json')} disabled={exporting !== null}>
                {exporting === 'json' ? t('导出中…') : t('导出 JSON')}
              </Button>
              <Button variant="outline" size="sm" onClick={() => void handleExport('csv')} disabled={exporting !== null}>
                {exporting === 'csv' ? t('导出中…') : t('导出 CSV')}
              </Button>
            </>
          )}
          {tab === 'requests' && (
            <div className="ui-tabs" role="tablist" aria-label={t('视图切换')}>
              <button type="button" className={`ui-tab ${view === 'table' ? 'active' : ''}`} onClick={() => setView('table')}>
                {t('表格')}
              </button>
              <button type="button" className={`ui-tab ${view === 'timeline' ? 'active' : ''}`} onClick={() => setView('timeline')}>
                {t('时间线')}
              </button>
            </div>
          )}
          {admin && (
            <button
              type="button"
              className="btn btn-outline btn-sm"
              onClick={() => setMasked((v) => !v)}
              title={masked ? t('显示敏感信息') : t('隐藏敏感信息（用户邮箱/渠道名等打码）')}
              style={{ display: 'inline-flex', alignItems: 'center', gap: 4 }}
            >
              {masked ? <EyeOff size={14} /> : <Eye size={14} />}
              {masked ? t('已脱敏') : t('脱敏开关')}
            </button>
          )}
        </div>
      </Card>

      {/* 在途请求面板：正在跑、尚未落库的请求，可逐条终止 */}
      {admin && showInflight && (
        <Card
          title={t('在途请求（{{n}}）', { n: inflight.length })}
          actions={
            <Button variant="outline" size="sm" onClick={() => void loadInflight()} disabled={inflightLoading}>
              {inflightLoading ? t('刷新中…') : t('刷新')}
            </Button>
          }
        >
          {inflight.length === 0 ? (
            <EmptyState icon="🕊️" message={t('当前没有在途请求')} />
          ) : (
            <div className="table-wrapper">
              <table>
                <thead>
                  <tr>
                    <th>{t('请求 ID')}</th>
                    <th>{t('模型')}</th>
                    <th>{t('渠道')}</th>
                    <th>{t('已运行')}</th>
                    <th>{t('类型')}</th>
                    <th>{t('操作')}</th>
                  </tr>
                </thead>
                <tbody>
                  {inflight.map((r) => (
                    <tr key={r.id}>
                      <td className="log-detail-mono">{r.id}</td>
                      <td>{r.model}</td>
                      <td>{r.channel_name || r.channel_id || '—'}</td>
                      <td>{(r.elapsed_ms / 1000).toFixed(1)}s</td>
                      <td>{r.stream ? t('流式') : t('非流式')}</td>
                      <td>
                        <Button variant="outline" size="sm" onClick={() => handleTerminateInflight(r.id)}>
                          {t('终止')}
                        </Button>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </Card>
      )}

      {tab === 'requests' && (
        <Card title={t('筛选')} className="" bodyClassName="">
          <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fill, minmax(200px, 1fr))', gap: 12 }}>
            {admin && (
              <div className="form-group">
                <label>{t('用户')}</label>
                <input ref={searchRef} className="form-input" value={filters.user} onChange={(e) => setFilters({ ...filters, user: e.target.value })} placeholder={t('按用户 ID 或邮箱过滤')} />
              </div>
            )}
            {!admin && (
              <div className="form-group">
                <label>{t('模型')}</label>
                <input ref={searchRef} className="form-input" value={filters.model} onChange={(e) => setFilters({ ...filters, model: e.target.value })} placeholder={t('按模型过滤')} />
              </div>
            )}
            {admin && (
              <div className="form-group">
                <label>{t('模型')}</label>
                <input className="form-input" value={filters.model} onChange={(e) => setFilters({ ...filters, model: e.target.value })} placeholder={t('按模型过滤')} />
              </div>
            )}
            {admin && (
              <div className="form-group">
                <label>{t('渠道 ID')}</label>
                <input className="form-input" value={filters.channel} onChange={(e) => setFilters({ ...filters, channel: e.target.value })} placeholder={t('按渠道过滤')} />
              </div>
            )}
            <div className="form-group">
              <label>{t('关键词')}</label>
              <input
                className="form-input"
                value={filters.keyword}
                onChange={(e) => setFilters({ ...filters, keyword: e.target.value })}
                onKeyDown={(e) => {
                  if (e.key === 'Enter') {
                    if (page === 1) void load();
                    else setPage(1);
                  }
                }}
                placeholder={t('模型 / 渠道 / 错误信息')}
              />
            </div>
            <div className="form-group">
              <label>{t('状态')}</label>
              <select
                className="form-input"
                value={filters.status}
                onChange={(e) => setFilters({ ...filters, status: e.target.value })}
              >
                <option value="">{t('全部')}</option>
                <option value="ok">{t('成功')}</option>
                <option value="error">{t('失败')}</option>
              </select>
            </div>
            <div className="form-group">
              <label>{t('开始时间')}</label>
              <input
                type="datetime-local"
                className="form-input"
                value={filters.start}
                onChange={(e) => setFilters({ ...filters, start: e.target.value })}
              />
            </div>
            <div className="form-group">
              <label>{t('结束时间')}</label>
              <input
                type="datetime-local"
                className="form-input"
                value={filters.end}
                onChange={(e) => setFilters({ ...filters, end: e.target.value })}
              />
            </div>
          </div>
          {!admin && (
            <div className="form-hint" style={{ marginTop: 4 }}>
              {t('仅显示你自己的请求记录')}
            </div>
          )}
          <div style={{ marginTop: 12, display: 'flex', gap: 8, alignItems: 'center', flexWrap: 'wrap' }}>
            <Button size="sm" onClick={handleSearch}>{t('查询')}</Button>
            {admin && (
              <>
                <Button
                  variant="danger"
                  size="sm"
                  onClick={handleBatchDelete}
                  disabled={selected.size === 0 || batchDeleting}
                >
                  <Trash2 size={14} style={{ marginRight: 4 }} />
                  {t('删除选中')} {selected.size > 0 ? `(${selected.size})` : ''}
                </Button>
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => void handleClearAll()}
                  disabled={batchDeleting || total === 0}
                >
                  <Eraser size={14} style={{ marginRight: 4 }} />
                  {t('清空全部')}
                </Button>
              </>
            )}
          </div>
        </Card>
      )}

      <Card title={`${tab === 'requests' ? t('请求日志') : t('审计日志')} (${total})`}>
        {loading ? (
          <Loading text={t('加载中')} />
        ) : view === 'timeline' && tab === 'requests' ? (
          renderTimeline()
        ) : logs.length === 0 ? (
          <EmptyState message={t('暂无日志')} />
        ) : (
          <div className="table-wrapper">
            <table>
              <thead>
                {tab === 'requests' ? (
                  <tr>
                    {admin && (
                      <th style={{ width: 36 }}>
                        <input
                          type="checkbox"
                          checked={logs.length > 0 && selected.size === logs.length}
                          onChange={toggleSelectAll}
                          aria-label={t('全选')}
                        />
                      </th>
                    )}
                    {admin && <th style={{ width: 190 }}>{t('用户')}</th>}
                    <th>{t('时间')}</th>
                    <th>{t('模型')}</th>
                    {admin && <th>{t('渠道')}</th>}
                    <th>{t('Tokens')}</th>
                    <th>{t('费用')}</th>
                    <th>{t('延迟')}</th>
                    <th>{t('状态')}</th>
                  </tr>
                ) : (
                  <tr>
                    {admin && (
                      <th style={{ width: 36 }}>
                        <input
                          type="checkbox"
                          checked={logs.length > 0 && selected.size === logs.length}
                          onChange={toggleSelectAll}
                          aria-label={t('全选')}
                        />
                      </th>
                    )}
                    <th>{t('管理员')}</th>
                    <th>{t('时间')}</th>
                    <th>{t('操作')}</th>
                    <th>{t('目标')}</th>
                    <th>{t('变更')}</th>
                  </tr>
                )}
              </thead>
              <tbody>
                {logs.map((l, i) =>
                  isRequest(l) ? (
                    <tr
                      key={l.id}
                      ref={(el) => { rowRefs.current[i] = el; }}
                      className={`log-row-clickable ${activeIndex === i ? 'kbd-active' : ''}`}
                      onClick={() => setDetail(l)}
                      onMouseEnter={() => setActiveIndex(i)}
                      title={t('点击查看详情')}
                    >
                      {admin && (
                        <td onClick={(e) => e.stopPropagation()}>
                          <input
                            type="checkbox"
                            checked={selected.has(l.id)}
                            onChange={() => toggleSelect(l.id)}
                            aria-label={t('选择该行')}
                          />
                        </td>
                      )}
                      {admin && (
                        <td style={{ maxWidth: 190 }}>
                          <span className="log-user-cell">
                            <span className="log-user-avatar">{(userDisplay(l.user_email, l.user_id) || '?').charAt(0).toUpperCase()}</span>
                            <span style={{ overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{userDisplay(l.user_email, l.user_id)}</span>
                          </span>
                        </td>
                      )}
                      <td style={{ whiteSpace: 'nowrap' }}>
                        {fmtTime(l.created_at)}
                        <div>
                          <span className={(l.status_code ?? 0) < 400 ? 'badge badge-success' : 'badge badge-danger'} style={{ fontSize: 10 }}>
                            {(l.status_code ?? 0) < 400 ? t('成功') : t('失败')}
                          </span>
                          {sourceLabel(l.key_id) && (
                            <span className="badge badge-info" style={{ fontSize: 10, marginLeft: 4 }} title={l.key_id}>
                              {sourceLabel(l.key_id)}
                            </span>
                          )}
                        </div>
                      </td>
                      <td style={{ maxWidth: 200, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }} title={l.model || ''}>
                        {l.model || '—'}
                        {l.origin_model && l.origin_model !== l.model && (
                          <div style={{ fontSize: 10, color: 'var(--text-muted)' }}>← {l.origin_model}</div>
                        )}
                      </td>
                      {admin && (
                        <td style={{ maxWidth: 140, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }} title={l.channel_name || ''}>
                          {masked ? mask(l.channel_name) : (l.channel_name || '—')}
                        </td>
                      )}
                      <td style={{ whiteSpace: 'nowrap', fontSize: 12 }}>
                        <span>{fmtCompact(l.input_tokens ?? 0)}</span>
                        <span style={{ color: 'var(--text-muted)' }}> / </span>
                        <span>{fmtCompact(l.output_tokens ?? 0)}</span>
                      </td>
                      <td style={{ whiteSpace: 'nowrap' }}>¥{l.cost}</td>
                      <td style={{ whiteSpace: 'nowrap' }}>{l.latency_ms}ms</td>
                      <td>
                        <span className={(l.status_code ?? 0) < 400 ? 'badge badge-success' : 'badge badge-danger'}>{l.status_code}</span>
                      </td>
                    </tr>
                  ) : (
                    <tr
                      key={l.id}
                      ref={(el) => { rowRefs.current[i] = el; }}
                      className={`log-row-clickable ${activeIndex === i ? 'kbd-active' : ''}`}
                      onClick={() => setDetail(l)}
                      onMouseEnter={() => setActiveIndex(i)}
                      title={t('点击查看详情')}
                    >
                      {admin && (
                        <td onClick={(e) => e.stopPropagation()}>
                          <input
                            type="checkbox"
                            checked={selected.has(l.id)}
                            onChange={() => toggleSelect(l.id)}
                            aria-label={t('选择该行')}
                          />
                        </td>
                      )}
                      <td style={{ maxWidth: 170 }}>
                        <span className="log-user-cell">
                          <span className="log-user-avatar">{(userDisplay(l.admin_email, l.admin_id) || '?').charAt(0).toUpperCase()}</span>
                          <span style={{ overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{userDisplay(l.admin_email, l.admin_id)}</span>
                        </span>
                      </td>
                      <td style={{ whiteSpace: 'nowrap' }}>{fmtTime(l.created_at)}</td>
                      <td><code style={{ background: 'var(--card-bg)', padding: '2px 6px', borderRadius: 4 }}>{l.action}</code></td>
                      <td style={{ maxWidth: 200, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }} title={l.target || ''}>{l.target}</td>
                      <td style={{ maxWidth: 300, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', fontSize: 11, color: 'var(--text-muted)' }} title={l.after || '—'}>
                        {l.after || '—'}
                      </td>
                    </tr>
                  )
                )}
              </tbody>
            </table>
          </div>
        )}

        {totalPages > 1 && (
          <Pagination page={page} totalPages={totalPages} onChange={setPage} />
        )}
      </Card>

      {/* 行点击详情弹窗 */}
      {detail && isRequest(detail) && (
        <RequestLogDetail log={detail} admin={admin} onClose={() => setDetail(null)} />
      )}
      {detail && !isRequest(detail) && (
        <AuditLogDetail log={detail} onClose={() => setDetail(null)} />
      )}

      <ConfirmDialog state={confirmState} onClose={() => setConfirmState(null)} />
    </div>
  );
}
