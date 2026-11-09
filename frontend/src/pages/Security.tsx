import { useState, useEffect, useRef, useCallback, useMemo, type MouseEvent } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api';
import SystemMonitorPanel from '../components/SystemMonitorPanel';
import { Button, Card, EmptyState, Select, SkeletonTable } from '../components/ui';
import {
  applyTableColumns,
  readTableColumns,
  resetTableColumns,
  writeTableColumns,
  type TableColumnDef,
  type TableColumnItem,
  type TableColumnState,
} from '../lib/tableColumns';
import TableColumnSettings from '../components/ui/TableColumnSettings';
import { useColumnResize } from '../components/ui/useColumnResize';
import { TablePaginationFooter, useServerPaging, SERVER_PAGE_SIZES } from '../components/ui/TableShell';
import { Tooltip } from '../components/ui';
import { Columns3 } from 'lucide-react';

const LS_PSZ = 'aigx-tbl-pagesize.security';
const LS_TCOLS = 'aigx-tbl-cols.security';
/** 安全事件列声明（时间/详情锁定） */
const SEC_COL_DEFS: TableColumnDef[] = [
  { key: 'time', label: '时间', width: 170, locked: true },
  { key: 'type', label: '类型', width: 140 },
  { key: 'ip', label: 'IP', width: 130 },
  { key: 'user', label: '用户', width: 180 },
  { key: 'detail', label: '详情', width: 320 },
  { key: 'severity', label: '严重程度', align: 'center', width: 100 },
];
import './Security.css';

interface SecurityOverview {
  total_events?: number;
  critical_events?: number;
  recent_24h?: number;
}

interface SecurityEvent {
  id?: string | number;
  created_at?: number;
  time?: number;
  timestamp?: number;
  type?: string;
  event_type?: string;
  ip?: string;
  client_ip?: string;
  user?: string;
  user_id?: string;
  username?: string;
  detail?: string;
  details?: string;
  message?: string;
  severity?: string;
  level?: string;
}

interface TimeRangeOption {
  value: string;
  labelKey: string;
}

// 安全事件类型选项（用于筛选下拉）
const EVENT_TYPES: TimeRangeOption[] = [
  { value: '', labelKey: '全部' },
  { value: 'auth_failure', labelKey: '认证失败' },
  { value: 'rate_limit', labelKey: '限流触发' },
  { value: 'ip_blocked', labelKey: 'IP 拦截' },
  { value: 'abuse', labelKey: '滥用检测' },
  { value: 'intrusion', labelKey: '入侵尝试' },
];

// 时间范围选项
const TIME_RANGES: TimeRangeOption[] = [
  { value: '1h', labelKey: '近 1 小时' },
  { value: '24h', labelKey: '近 24 小时' },
  { value: '7d', labelKey: '近 7 天' },
  { value: '30d', labelKey: '近 30 天' },
  { value: '', labelKey: '全部' },
];

// Security 页面：安全监控，显示安全概览与事件列表。
// 参照 Logs.tsx 的表格样式与筛选交互模式。
export default function Security(): JSX.Element {
  const { t } = useTranslation();

  const [overview, setOverview] = useState<SecurityOverview | null>(null);
  const [events, setEvents] = useState<SecurityEvent[]>([]);
  const [colState, setColState] = useState<TableColumnState>(() => readTableColumns(LS_TCOLS, SEC_COL_DEFS));
  const [colPanelOpen, setColPanelOpen] = useState(false);
  const colPanelRef = useRef<HTMLDivElement | null>(null);
  const commitCols = useCallback((next: TableColumnItem[], persist: boolean) => {
    setColState((prev) => {
      const nextState: TableColumnState = { ...prev, items: next };
      if (persist) writeTableColumns(LS_TCOLS, nextState);
      return nextState;
    });
  }, []);
  const resetCols = useCallback(() => setColState(resetTableColumns(LS_TCOLS, SEC_COL_DEFS)), []);
  const appliedCols = useMemo(() => applyTableColumns(colState, SEC_COL_DEFS), [colState]);
  const visibleKeySet = useMemo(() => new Set(appliedCols.map((c) => c.key)), [appliedCols]);
  const colResize = useColumnResize({
    widths: Object.fromEntries(colState.items.filter((i) => i.width != null).map((i) => [i.key, i.width as number])),
    setWidths: (next) => {
      commitCols(
        colState.items.map((i) => {
          const w = next[i.key];
          const defW = SEC_COL_DEFS.find((d) => d.key === i.key)?.width;
          if (w == null || (defW != null && w === defW)) {
            const { width: _drop, ...rest } = i;
            return rest;
          }
          return { ...i, width: w };
        }),
        true,
      );
    },
    defaultWidths: Object.fromEntries(SEC_COL_DEFS.map((d) => [d.key, d.width])),
    lockedKeys: new Set(SEC_COL_DEFS.filter((d) => d.locked).map((d) => d.key)),
    onCommit: () => { /* 落盘已在 setWidths 内做 */ },
  });
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');

  // 筛选条件
  const [timeRange, setTimeRange] = useState('24h');
  const [eventType, setEventType] = useState('');

  // 分页（服务端真分页）：公共 hook 统一承担越界钳位 / size 变更回第 1 页 /
  // 筛选变化回第 1 页（timeRange / eventType 变化即回第 1 页，不再依赖「查询」按钮）。
  const { page, setPage, pageSize, size, setSize, total, setTotal, totalPages } = useServerPaging({
    key: LS_PSZ,
    sizes: SERVER_PAGE_SIZES,
    fallback: 20,
    resetDeps: [timeRange, eventType],
  });

  // 概览加载（独立于事件列表，避免分页触发概览重复请求）
  useEffect(() => {
    void loadOverview();
  }, []);

  // 事件列表：分页/每页条数/筛选条件变化自动加载
  useEffect(() => {
    void loadEvents();
    // loadEvents 随分页/筛选参数变化，禁用 exhaustive-deps 以保留显式触发语义
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [page, size, timeRange, eventType]);

  const loadOverview = async () => {
    try {
      const res = await api.getSecurityOverview();
      setOverview((res?.data ?? null) as unknown as SecurityOverview | null);
    } catch (err) {
      // 概览加载失败不阻塞事件列表
      setError(err instanceof Error ? err.message : String(err));
    }
  };

  const loadEvents = async () => {
    setLoading(true);
    setError('');
    try {
      const params: Record<string, string | number> = { page, size };
      if (timeRange) params.range = timeRange;
      if (eventType) params.type = eventType;
      const res = await api.getSecurityEvents(params);
      // 后端返回 { data: { events, total, page, page_size } }（对象嵌套），
      // 不是数组。原来按数组读会导致列表恒空、分页恒 0。
      const payload = res?.data as unknown as
        | { events?: SecurityEvent[]; total?: number }
        | undefined;
      setEvents(Array.isArray(payload?.events) ? payload.events : []);
      setTotal(payload?.total ?? 0);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  };

  // 查询按钮：筛选条件变化时显式触发
  const handleSearch = (e: MouseEvent<HTMLButtonElement>) => {
    e.preventDefault();
    if (page === 1) {
      void loadEvents();
    } else {
      setPage(1);
    }
  };

  const fmtTime = (ts: number | undefined): string => {
    if (!ts) return '—';
    // 兼容秒级与毫秒级时间戳
    const ms = ts > 1e12 ? ts : ts * 1000;
    return new Date(ms).toLocaleString();
  };

  // 严重程度 → 中文标签 + 语义色徽章（critical/high 危险、medium 警告、low/warning/info 提示、其余中性原样）
  const severityLabel = (sv: string): string => {
    if (sv === 'critical') return t('严重');
    if (sv === 'high') return t('高危');
    if (sv === 'medium') return t('中危');
    if (sv === 'low' || sv === 'warning') return t('低危');
    if (sv === 'info') return t('信息');
    return sv;
  };

  const severityBadgeClass = (sv: string): string => {
    if (sv === 'critical' || sv === 'high') return 'badge badge-danger';
    if (sv === 'medium') return 'badge badge-warning';
    if (sv === 'low') return 'badge badge-info';
    if (sv === 'warning') return 'badge badge-warning';
    if (sv === 'info') return 'badge badge-info';
    return 'badge badge-neutral';
  };

  // 概览卡片数据（容错：后端字段缺失时回退 0）
  const totalEvents = overview?.total_events ?? 0;
  const criticalEvents = overview?.critical_events ?? 0;
  const recent24h = overview?.recent_24h ?? 0;

  return (
    <div className="security-shell">
      {error && <div className="error-message">{error}</div>}

      {/* 系统资源监控（CPU/内存/负载，自动轮询） */}
      <Card title={t('系统资源')} bodyClassName="">

        actions={
          <div className="as-colset" ref={colPanelRef} style={{ position: 'relative', display: 'inline-block' }}>
            <Tooltip label={t('列设置（显隐 / 顺序 / 对齐 / 列宽拖动）')}>
              <button
                type="button"
                className={`btn btn-outline btn-sm${colPanelOpen ? ' as-colset-open' : ''}`}
                onClick={() => setColPanelOpen((v) => !v)}
                aria-label={t('列设置')}
              >
                <Columns3 size={14} />
              </button>
            </Tooltip>
            {colPanelOpen && (
              <TableColumnSettings
                open
                onClose={() => setColPanelOpen(false)}
                defs={SEC_COL_DEFS}
                items={colState.items}
                onChange={commitCols}
                onReset={resetCols}
                anchorRef={colPanelRef}
              />
            )}
          </div>
        }        <SystemMonitorPanel />
      </Card>

      {/* 概览卡片 */}
      <div className="security-overview">
        <div className="card security-stat-card">
          <div className="card-body">
            <div className="security-stat-label">{t('总事件数')}</div>
            <div className="security-stat-value">{totalEvents}</div>
          </div>
        </div>
        <div className="card security-stat-card security-stat-critical">
          <div className="card-body">
            <div className="security-stat-label">{t('严重事件')}</div>
            <div className="security-stat-value">{criticalEvents}</div>
          </div>
        </div>
        <div className="card security-stat-card security-stat-recent">
          <div className="card-body">
            <div className="security-stat-label">{t('近 24 小时')}</div>
            <div className="security-stat-value">{recent24h}</div>
          </div>
        </div>
      </div>

      {/* 筛选区 */}
      <Card title={t('筛选')} bodyClassName="">
        <div className="security-filter-row">
          <Select
            label={t('时间范围')}
            value={timeRange}
            onChange={(e) => setTimeRange(e.target.value)}
          >
            {TIME_RANGES.map((r) => (
              <option key={r.value} value={r.value}>{t(r.labelKey)}</option>
            ))}
          </Select>
          <Select
            label={t('事件类型')}
            value={eventType}
            onChange={(e) => setEventType(e.target.value)}
          >
            {EVENT_TYPES.map((tp) => (
              <option key={tp.value} value={tp.value}>{t(tp.labelKey)}</option>
            ))}
          </Select>
          <div className="security-filter-action">
            <Button size="sm" onClick={handleSearch} disabled={loading}>{loading ? t('查询中...') : t('查询')}</Button>
          </div>
        </div>
      </Card>

      {/* 事件列表 */}
      <Card title={`${t('安全事件')} (${total})`}>
        {loading ? (
          <SkeletonTable columns={6} rows={8} />
        ) : events.length === 0 ? (
          <EmptyState message={t('暂无安全事件')} icon="🛡️" />
        ) : (
          <div className="table-wrapper">
            <table>
              <thead>
                <colgroup>
                  {appliedCols.map((c) => (
                    <col key={c.key} data-col-key={c.key} style={c.width != null ? { width: `${c.width}px` } : undefined} />
                  ))}
                </colgroup>
                <tr>
                  {appliedCols.map((c, idx) => {
                    const grip = colResize.gripProps(c.key, {
                      isLast: idx === appliedCols.length - 1,
                      locked: c.locked,
                      defaultWidth: c.width,
                    });
                    return (
                      <th
                        key={c.key}
                        style={c.width != null ? { width: `${c.width}px` } : undefined}
                      >
                        {c.key === 'ip' ? 'IP' : t(SEC_COL_DEFS.find((d) => d.key === c.key)?.label ?? c.key)}
                        {c.key !== 'time' && c.key !== 'detail' && (
                          <span className="as-col-grip" title={t('拖动调整列宽 · 双击还原')} {...grip} />
                        )}
                      </th>
                    );
                  })}
                </tr>
              </thead>
              <tbody>
                {events.map((ev) => {
                  const severity = ev.severity || ev.level || 'info';
                  return (
                    <tr key={ev.id ?? `${ev.created_at}-${ev.ip}`}>
{visibleKeySet.has('time') && (
                      <td style={{ fontSize: 12, color: 'var(--text-muted)' }}>
                        {fmtTime(ev.created_at || ev.time || ev.timestamp)}
                      </td>
)}
{visibleKeySet.has('type') && (
                      <td>
                        <code style={{ background: 'var(--card-bg)', padding: '2px 6px', borderRadius: 4, fontSize: 12 }}>
                          {ev.type || ev.event_type || '—'}
                        </code>
                      </td>
)}
{visibleKeySet.has('ip') && (
                      <td style={{ fontSize: 12 }}>{ev.ip || ev.client_ip || '—'}</td>
)}
{visibleKeySet.has('user') && (
                      <td style={{ fontSize: 12 }} title={ev.user || ev.user_id || ev.username || ''}>{ev.user || ev.user_id || ev.username || '—'}</td>
)}
{visibleKeySet.has('detail') && (
                      <td style={{ maxWidth: 320, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', fontSize: 12, color: 'var(--text-muted)' }} title={ev.detail || ev.details || ev.message || ''}>
                        {ev.detail || ev.details || ev.message || '—'}
                      </td>
)}
{visibleKeySet.has('severity') && (
                      <td>
                        <span className={severityBadgeClass(severity)}>
                          {severityLabel(severity)}
                        </span>
                      </td>
)}
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
        )}

        <TablePaginationFooter
          total={total}
          page={page}
          totalPages={totalPages}
          size={pageSize}
          sizes={SERVER_PAGE_SIZES}
          onSizeChange={setSize}
          onPageChange={setPage}
        />
      </Card>
    </div>
  );
}
