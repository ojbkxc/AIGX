/**
 * 「签到记录」视图（对齐 Agent2API `checkin-records-tab.tsx`）。
 *
 * ── 列集与顺序照 A2A ────────────────────────────────────────
 * 时间 / 账号 / 提供商 / 来源 / 结果 / 说明。AIGX 额外有「类型」与「失败原因」
 * 两列（契约里 `kind` 与 `source` 同值双写、`error` 是失败原文），默认**收起**，
 * 需要时在列设置里打开 —— 默认视图与 A2A 一致。
 *
 * ── 筛选档位照 A2A ─────────────────────────────────────────
 * 时间五档（全部 / 今天 / 7 天 / 30 天 / 本月）· 提供商 · 签到类型 · 签到结果 ·
 * 关键词。AIGX 另带「起止日期」（A2A 无此控件）与按当前筛选删除，作为独占项保留。
 *
 * ── 服务端分页 ────────────────────────────────────────────
 * `offset / limit` 走服务端真分页（筛选变化回第 1 页、越界钳位、页大小记忆）。
 */

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Columns3, X } from 'lucide-react';
import { api } from '../api';
import Badge from '../components/ui/Badge';
import Button from '../components/ui/Button';
import Card from '../components/ui/Card';
import EmptyState from '../components/ui/EmptyState';
import Loading from '../components/ui/Loading';
import Tooltip from '../components/ui/Tooltip';
import TableColumnSettings from '../components/ui/TableColumnSettings';
import ConfirmDialog, { type ConfirmState } from '../components/ConfirmDialog';
import { useServerPaging, SERVER_PAGE_SIZES } from '../components/ui/TableShell';
import {
  applyTableColumns,
  readTableColumns,
  resetTableColumns,
  writeTableColumns,
  type TableColumnDef,
  type TableColumnItem,
  type TableColumnState,
} from '../lib/tableColumns';
import type { CheckinCenterData, CheckinRecord } from '../types';
import {
  RECORD_KIND_LABEL,
  RECORD_OUTCOME_LABEL,
  RECORD_OUTCOME_TONE,
  RECORD_SOURCE_LABEL,
} from './checkin-shared';

const LS_TCOLS = 'aigx-tbl-cols.checkin-records';

/** 列声明：前六列与 A2A 逐列一致，最后两列是 AIGX 契约扩展（默认收起） */
const CK_REC_COL_DEFS: TableColumnDef[] = [
  { key: 'time', label: '时间', width: 150, locked: true },
  { key: 'account', label: '账号', width: 180 },
  { key: 'provider', label: '提供商', width: 110 },
  { key: 'source', label: '来源', width: 120 },
  { key: 'result', label: '结果', width: 110 },
  { key: 'note', label: '说明', width: 280 },
  { key: 'kind', label: '类型', width: 80 },
  { key: 'error', label: '失败原因', width: 260 },
];

/** 默认收起的扩展列（AIGX 契约有、A2A 视图没有） */
const EXTRA_COLUMNS = new Set(['kind', 'error']);

/** 时间档（A2A `RANGES`：全部 / 今天 / 7 天 / 30 天 / 本月） */
const RANGES = [
  { value: 'all', label: '全部' },
  { value: 'today', label: '今天' },
  { value: '7', label: '7 天' },
  { value: '30', label: '30 天' },
  { value: 'month', label: '本月' },
] as const;

type RangeValue = (typeof RANGES)[number]['value'];

/** 时间档 → 起始毫秒（`all` = 不限下界；`month` = 本月 1 日 00:00） */
function rangeStart(value: RangeValue): number | undefined {
  const now = new Date();
  const midnight = (date: Date): number =>
    new Date(date.getFullYear(), date.getMonth(), date.getDate()).getTime();
  switch (value) {
    case 'today':
      return midnight(now);
    case '7':
      return midnight(new Date(now.getFullYear(), now.getMonth(), now.getDate() - 6));
    case '30':
      return midnight(new Date(now.getFullYear(), now.getMonth(), now.getDate() - 29));
    case 'month':
      return midnight(new Date(now.getFullYear(), now.getMonth(), 1));
    default:
      return undefined;
  }
}

/** `<input type="date">` 的 `YYYY-MM-DD` → 当日 00:00 / 23:59:59.999 毫秒 */
function dateStart(value: string): number | undefined {
  if (!value) return undefined;
  const ms = new Date(`${value}T00:00:00`).getTime();
  return Number.isFinite(ms) ? ms : undefined;
}
function dateEnd(value: string): number | undefined {
  if (!value) return undefined;
  const ms = new Date(`${value}T23:59:59`).getTime();
  return Number.isFinite(ms) ? ms + 999 : undefined;
}

/** 记录类型三档（A2A `KIND_OPTIONS`；AIGX 另带「重试」） */
const KIND_OPTIONS = [
  { value: '', label: '全部类型' },
  { value: 'manual', label: '手动签到' },
  { value: 'schedule', label: '定时签到' },
  { value: 'makeup', label: '定时补签' },
  { value: 'retry', label: '重试' },
] as const;

/** 结果档（A2A `OUTCOME_OPTIONS` 的取值与措辞） */
const OUTCOME_OPTIONS = [
  { value: '', label: '全部结果' },
  { value: 'success', label: RECORD_OUTCOME_LABEL.success },
  { value: 'already', label: RECORD_OUTCOME_LABEL.already },
  { value: 'pending', label: RECORD_OUTCOME_LABEL.pending },
  { value: 'captcha', label: RECORD_OUTCOME_LABEL.captcha },
  { value: 'failed', label: RECORD_OUTCOME_LABEL.failed },
] as const;

/** 记录时间：`MM-DD HH:MM`（A2A `formatShort`）+ 后台账日期 */
function timeText(record: CheckinRecord): string {
  const date = new Date(record.ts * 1000);
  if (Number.isNaN(date.getTime())) return '—';
  const pad = (n: number): string => String(n).padStart(2, '0');
  return `${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(date.getHours())}:${pad(date.getMinutes())}`;
}

/** 说明列：消息 > 失败原文 > 保活成功 > `—`（A2A `detailText` 同口径） */
function detailText(record: CheckinRecord): string {
  if (record.message) return record.message;
  if (record.error) return record.error;
  if (record.keptActive) return '保活成功';
  return '—';
}

export default function CheckinRecordsView({
  center,
  onOpenBenefits,
}: {
  center: CheckinCenterData | null;
  /** 记录页不渲染福利，占位给「当天没有记录」时空态的引导（保持与调用方一致） */
  onOpenBenefits?: () => void;
}): JSX.Element {
  const { t } = useTranslation();
  const [records, setRecords] = useState<CheckinRecord[]>([]);
  const [loading, setLoading] = useState(false);
  const [range, setRange] = useState<RangeValue>('all');
  const [provider, setProvider] = useState('');
  const [kind, setKind] = useState('');
  const [outcome, setOutcome] = useState('');
  const [start, setStart] = useState('');
  const [end, setEnd] = useState('');
  const [keyword, setKeyword] = useState('');
  const [keywordApplied, setKeywordApplied] = useState('');
  const [confirmState, setConfirmState] = useState<ConfirmState | null>(null);
  const [panelOpen, setPanelOpen] = useState(false);
  const panelRef = useRef<HTMLDivElement | null>(null);

  const [cols, setCols] = useState<TableColumnState>(() => {
    const saved = readTableColumns(LS_TCOLS, CK_REC_COL_DEFS);
    // 未存过任何列设置 = 首次打开：按 A2A 的六列默认视图，扩展列先收起
    let pristine = true;
    try {
      pristine = localStorage.getItem(LS_TCOLS) === null;
    } catch {
      pristine = true;
    }
    if (!pristine) return saved;
    return {
      ...saved,
      items: saved.items.map((item) =>
        EXTRA_COLUMNS.has(item.key) ? { ...item, visible: false } : item,
      ),
    };
  });
  const commitCols = useCallback((next: TableColumnItem[], persist: boolean) => {
    setCols((prev) => {
      const nextState: TableColumnState = { ...prev, items: next };
      if (persist) writeTableColumns(LS_TCOLS, nextState);
      return nextState;
    });
  }, []);
  const resetCols = useCallback(() => setCols(resetTableColumns(LS_TCOLS, CK_REC_COL_DEFS)), []);
  const appliedCols = useMemo(() => applyTableColumns(cols, CK_REC_COL_DEFS), [cols]);
  const colKeys = useMemo(() => new Set(appliedCols.map((column) => column.key)), [appliedCols]);

  const { page, setPage, pageSize, size, setSize, total, setTotal, totalPages } = useServerPaging({
    key: 'aigx-tbl-pagesize.checkin',
    sizes: SERVER_PAGE_SIZES,
    fallback: 50,
    resetDeps: [range, provider, kind, outcome, start, end, keywordApplied],
  });

  const explicitStart = dateStart(start);
  const explicitEnd = dateEnd(end);
  const queryStart = explicitStart ?? rangeStart(range);

  /**
   * 请求序号：快速翻页 / 改筛选时旧响应晚到不得覆盖新数据。
   *
   * `load` 由 `page/size/筛选` 重新创建（useCallback 依赖），每个请求拿自己的
   * 序号；响应回来只认「仍是最后一个」的那次，否则丢弃。旧实现三条 `setState`
   * 无条件执行 —— 先发的慢请求会把后发的快请求结果覆盖掉，页面停在上一页的数据
   * 却显示新页码（翻页越快的网络越容易看到）。
   */
  const loadSeq = useRef(0);
  const load = useCallback(async (): Promise<void> => {
    const seq = ++loadSeq.current;
    setLoading(true);
    try {
      const res = await api.getCheckinRecords({
        provider: provider || undefined,
        outcome: outcome || undefined,
        kind: kind || undefined,
        start: queryStart,
        end: explicitEnd,
        keyword: keywordApplied || undefined,
        offset: (page - 1) * size,
        limit: size,
      });
      if (seq !== loadSeq.current) return;
      setRecords(res?.records ?? []);
      setTotal(res?.total ?? 0);
    } catch {
      if (seq !== loadSeq.current) return;
      // 读取失败由空态承担（错误文案不覆盖整张表）
      setRecords([]);
      setTotal(0);
    } finally {
      if (seq === loadSeq.current) setLoading(false);
    }
  }, [provider, outcome, kind, queryStart, explicitEnd, keywordApplied, page, size, setTotal]);

  useEffect(() => {
    void load();
  }, [load]);

  /** 提供商选项：快照的可签清单 + 记录里出现过的家（历史账号 / 已移除插件） */
  const providerOptions = useMemo(() => {
    const options = new Map<string, string>();
    for (const option of center?.auto?.providerOptions ?? []) options.set(option.id, option.label);
    for (const row of records) {
      if (row.providerId && !options.has(row.providerId)) {
        options.set(row.providerId, row.provider || row.providerId);
      }
    }
    return [...options.entries()];
  }, [center?.auto?.providerOptions, records]);

  const hasFilter = Boolean(
    keywordApplied || outcome || provider || kind || start || end || range !== 'all',
  );

  const clearRecords = (): void => {
    setConfirmState({
      title: t('清空签到记录'),
      message: hasFilter
        ? t('将删除当前筛选命中的全部签到记录（与「筛出什么就删什么」同一过滤链），不可恢复。')
        : t('未设置筛选：将清空全部签到记录，不可恢复。'),
      confirmText: t('清空'),
      danger: true,
      onConfirm: async () => {
        await api.clearCheckinRecords(
          hasFilter
            ? {
                outcome: outcome || undefined,
                kind: kind || undefined,
                provider: provider || undefined,
                keyword: keywordApplied || undefined,
                start: queryStart,
                end: explicitEnd,
              }
            : { all: true },
        );
        // 删除后：回第 1 页并重新拉取（总数以服务端为准，不在本地做减法）。
        // 只有已经停在第 1 页时才需要显式 `load()`；否则 `setPage(1)` 会让
        // useEffect 重新创建 load 并拉一次 —— 两者同时做等于旧页码一次 + 新页码
        // 一次共两次请求，且先发那次的 `offset` 用的是闭包里的旧 page。
        if (page !== 1) {
          setPage(1);
        } else {
          await load();
        }
      },
    });
  };

  return (
    <Card
      title={t('签到记录')}
      actions={
        <div className="ck-records-tools">
          <span className="ck-time-subtle">{t('共 {{n}} 条', { n: total })}</span>
          <div className="as-colset" ref={panelRef} style={{ position: 'relative', display: 'inline-block' }}>
            <Tooltip label={t('列设置（显隐 / 顺序 / 对齐 / 列宽拖动）')}>
              <button
                type="button"
                className={`btn btn-outline btn-sm${panelOpen ? ' as-colset-open' : ''}`}
                onClick={() => setPanelOpen((open) => !open)}
                aria-label={t('列设置')}
              >
                <Columns3 size={14} />
              </button>
            </Tooltip>
            {panelOpen && (
              <TableColumnSettings
                open
                onClose={() => setPanelOpen(false)}
                defs={CK_REC_COL_DEFS}
                items={cols.items}
                onChange={commitCols}
                onReset={resetCols}
                anchorRef={panelRef}
              />
            )}
          </div>
          <div className="ck-seg" role="group" aria-label={t('时间筛选')}>
            {RANGES.map((item) => (
              <button
                key={item.value}
                type="button"
                className={`ck-seg-item${range === item.value ? ' is-active' : ''}`}
                onClick={() => setRange(item.value)}
                title={explicitStart || explicitEnd ? t('起止日期生效时，此快选不参与过滤') : undefined}
              >
                {t(item.label)}
              </button>
            ))}
          </div>
          <select
            className="form-input"
            style={{ maxWidth: 160 }}
            value={provider}
            onChange={(e) => setProvider(e.target.value)}
            aria-label={t('提供商')}
          >
            <option value="">{t('全部提供商')}</option>
            {providerOptions.map(([id, label]) => (
              <option key={id} value={id}>
                {label}
              </option>
            ))}
          </select>
          <select
            className="form-input"
            style={{ maxWidth: 120 }}
            value={kind}
            onChange={(e) => setKind(e.target.value)}
            aria-label={t('签到类型')}
          >
            {KIND_OPTIONS.map((item) => (
              <option key={item.value} value={item.value}>
                {t(item.label)}
              </option>
            ))}
          </select>
          <select
            className="form-input"
            style={{ maxWidth: 130 }}
            value={outcome}
            onChange={(e) => setOutcome(e.target.value)}
            aria-label={t('签到结果')}
          >
            {OUTCOME_OPTIONS.map((item) => (
              <option key={item.value} value={item.value}>
                {item.label ? t(item.label) : t('全部结果')}
              </option>
            ))}
          </select>
          {/* AIGX 独占：起止日期（本地时区走 00:00–23:59:59，后端闭区间） */}
          <input
            type="date"
            className="form-input ck-date-input"
            style={{ maxWidth: 140 }}
            value={start}
            onChange={(e) => setStart(e.target.value)}
            aria-label={t('起始日期')}
            title={t('起始日期')}
          />
          <span className="ck-date-sep">–</span>
          <input
            type="date"
            className="form-input ck-date-input"
            style={{ maxWidth: 140 }}
            value={end}
            onChange={(e) => setEnd(e.target.value)}
            aria-label={t('结束日期')}
            title={t('结束日期')}
          />
          <input
            className="form-input"
            style={{ maxWidth: 160 }}
            placeholder={t('搜索账号或说明')}
            value={keyword}
            onChange={(e) => {
              setKeyword(e.target.value);
              if (e.target.value === '') setKeywordApplied('');
            }}
            onBlur={() => setKeywordApplied(keyword.trim())}
            onKeyDown={(e) => {
              if (e.key === 'Enter') setKeywordApplied(keyword.trim());
              if (e.key === 'Escape') {
                setKeyword('');
                setKeywordApplied('');
              }
            }}
          />
          <button type="button" className="btn btn-outline btn-sm" onClick={() => void load()}>
            {t('刷新')}
          </button>
          <button type="button" className="btn btn-danger btn-sm" onClick={clearRecords}>
            <X size={12} /> {t('清空全部')}
          </button>
        </div>
      }
    >
      <p className="ck-time-subtle">
        {t('每执行一次签到 · 每个账号一条明细 · 结果与收益以后端落账为准')}
      </p>
      {loading ? (
        <Loading text={t('加载记录')} />
      ) : records.length === 0 ? (
        <EmptyState
          icon="📝"
          message={hasFilter ? t('没有匹配的签到记录') : t('这段时间里还没有签到记录。')}
          action={
            onOpenBenefits && !hasFilter ? (
              <Button variant="outline" size="sm" onClick={onOpenBenefits}>
                {t('去福利通道看看')}
              </Button>
            ) : undefined
          }
        />
      ) : (
        <>
          <table className="ck-table">
            <colgroup>
              {appliedCols.map((column) => (
                <col
                  key={column.key}
                  data-col-key={column.key}
                  style={column.width != null ? { width: `${column.width}px` } : undefined}
                />
              ))}
            </colgroup>
            <thead>
              <tr>
                {appliedCols.map((column) => (
                  <th
                    key={column.key}
                    style={column.width != null ? { width: `${column.width}px` } : undefined}
                  >
                    {t(CK_REC_COL_DEFS.find((def) => def.key === column.key)?.label ?? column.key)}
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {records.map((record, index) => (
                <tr key={record.rowId ?? `${record.ts}-${record.accountId}-${index}`}>
                  {colKeys.has('time') && (
                    <td>
                      {timeText(record)}
                      {record.date && <div className="ck-time-subtle">{record.date}</div>}
                    </td>
                  )}
                  {colKeys.has('account') && <td>{record.accountName || record.accountId}</td>}
                  {colKeys.has('provider') && <td>{record.provider}</td>}
                  {colKeys.has('source') && (
                    <td className="ck-record-source">
                      {t(RECORD_SOURCE_LABEL[record.source] ?? record.source)}
                      {record.reason ? <div className="ck-time-subtle">{record.reason}</div> : null}
                    </td>
                  )}
                  {colKeys.has('result') && (
                    <td>
                      <Badge tone={RECORD_OUTCOME_TONE[record.outcome] ?? 'neutral'}>
                        {t(RECORD_OUTCOME_LABEL[record.outcome] ?? record.outcome)}
                      </Badge>
                      {record.keptActive ? <Badge tone="info">{t('保活')}</Badge> : null}
                    </td>
                  )}
                  {colKeys.has('note') && (
                    <td
                      className={
                        record.outcome === 'failed' || record.outcome === 'captcha'
                          ? 'ck-message ck-record-detail-fail'
                          : 'ck-message'
                      }
                      title={detailText(record)}
                    >
                      {detailText(record)}
                    </td>
                  )}
                  {colKeys.has('kind') && (
                    <td className="ck-record-kind">
                      {(() => {
                        const key = record.kind || record.source || '';
                        return key ? t(RECORD_KIND_LABEL[key] ?? key) : '—';
                      })()}
                    </td>
                  )}
                  {colKeys.has('error') && (
                    <td className={record.error ? 'ck-message ck-record-detail-fail' : 'ck-message'}>
                      {record.error || '—'}
                    </td>
                  )}
                </tr>
              ))}
            </tbody>
          </table>
          <div className="ck-pager">
            <span className="ck-pager-info">
              {t('共 {{n}} 条 · 第 {{page}}/{{pages}} 页', { n: total, page, pages: totalPages })}
            </span>
            <div className="ck-pager-tools">
              <select
                className="form-input"
                style={{ maxWidth: 90 }}
                value={pageSize}
                onChange={(e) => setSize(Number(e.target.value))}
                aria-label={t('每页条数')}
              >
                {SERVER_PAGE_SIZES.map((option) => (
                  <option key={option} value={option}>
                    {option} / 页
                  </option>
                ))}
              </select>
              <button
                type="button"
                className="btn btn-outline btn-sm"
                disabled={page <= 1}
                onClick={() => setPage(Math.max(1, page - 1))}
              >
                {t('上一页')}
              </button>
              <button
                type="button"
                className="btn btn-outline btn-sm"
                disabled={page >= totalPages}
                onClick={() => setPage(page + 1)}
              >
                {t('下一页')}
              </button>
            </div>
          </div>
        </>
      )}
      <ConfirmDialog state={confirmState} onClose={() => setConfirmState(null)} />
    </Card>
  );
}
