/**
 * RoutingTrace —— 请求跟踪：列表 + 详情（对照 A2A `routing-request-list` 的
 * 列表 / 分页 + `routing-exec-timeline` 的详情骨架，但数据源是 AIGX 的
 * trace 端点，形状对齐回放快照的扁平字段）。
 *
 * 数据源（be-routing）：
 *   GET /api/routing/trace?offset=&limit=&search=  → { entries[], matched, available }
 *   GET /api/routing/trace/request?id=&ts=                 → 单条详情
 * 三态降级：`available === false` / 404 未就绪 / 请求失败。
 * 列表手动刷新与翻页；运行中的行不轮询（AIGX 管理面刷新节奏比桌面端保守）。
 */
import { useCallback, useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { ChevronLeft, ChevronRight, Loader2, RefreshCw } from 'lucide-react';
import { api } from '../../api';
import { Button, Card, EmptyState } from '../ui';
import {
  type RoutingTraceList,
  type RoutingTraceRow,
} from '../../types/routing-workspace';

const PAGE = 30;

function statusOf(err: unknown): number | undefined {
  return (err as { status?: number } | null)?.status;
}

/** 秒/毫秒自适应（<1e11 视为秒，对齐账号页口径） */
function toMillis(v: number | undefined): number | null {
  if (v == null || !Number.isFinite(v)) return null;
  return v < 1e11 ? v * 1000 : v;
}

function rowState(row: RoutingTraceRow): 'running' | 'success' | 'failed' | 'unknown' {
  if (row.status == null) return 'unknown';
  if (row.status === 0) return 'running';
  if (row.status >= 200 && row.status < 300) return 'success';
  return 'failed';
}

function rowModel(row: RoutingTraceRow): string {
  return row.clientModel || row.model || row.upstreamModel || '';
}

function fmtDuration(ms: number | undefined): string {
  if (ms == null || !Number.isFinite(ms) || ms < 0) return '—';
  if (ms < 1000) return `${Math.round(ms)}ms`;
  if (ms < 60_000) return `${(ms / 1000).toFixed(1)}s`;
  const m = Math.floor(ms / 60_000);
  return `${m}m${String(Math.round((ms % 60_000) / 1000)).padStart(2, '0')}s`;
}

export default function RoutingTrace({ active }: { active: boolean }): JSX.Element {
  const { t } = useTranslation();
  const [searchInput, setSearchInput] = useState('');
  const [search, setSearch] = useState('');
  const [offset, setOffset] = useState(0);
  const [list, setList] = useState<RoutingTraceList | null>(null);
  const [selected, setSelected] = useState<RoutingTraceRow | null>(null);
  const [detail, setDetail] = useState<RoutingTraceRow | null>(null);
  const [detailLoading, setDetailLoading] = useState(false);
  const [detailError, setDetailError] = useState('');
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState('');
  const [notReady, setNotReady] = useState(false);

  const load = useCallback(
    async (search: string, offset: number): Promise<void> => {
      setLoading(true);
      setError('');
      try {
        const res = await api.getRoutingTrace({
          offset,
          limit: PAGE,
          ...(search.trim() ? { search: search.trim() } : {}),
        });
        setList(res?.data ?? null);
        setNotReady(false);
      } catch (err) {
        const st = statusOf(err);
        if (st === 404 || st === 405) {
          setNotReady(true);
          setList(null);
        } else {
          setError(err instanceof Error ? err.message : String(err));
        }
      } finally {
        setLoading(false);
      }
    },
    [],
  );

  useEffect(() => {
    if (!active) return;
    void load(search, offset);
  }, [active, search, offset, load]);

  // 详情：列表选中行 → 按 id(+ts) 拉取；详情拉取失败保留列表摘要数据
  const openRow = useCallback(async (row: RoutingTraceRow): Promise<void> => {
    setSelected(row);
    setDetail(null);
    setDetailError('');
    setDetailLoading(true);
    try {
      const res = await api.getRoutingTraceRequest(row.id, row.ts);
      setDetail(res?.data ?? row);
    } catch (err) {
      // 列表已有摘要，不把详情失败升级成整页错误；失败文案就地显示
      setDetailError(err instanceof Error ? err.message : String(err));
      setDetail(row);
    } finally {
      setDetailLoading(false);
    }
  }, []);

  const entries = Array.isArray(list?.entries) ? list?.entries ?? [] : [];
  const matched = typeof list?.matched === 'number' ? list.matched : entries.length;
  const pageCount = Math.max(1, Math.ceil(matched / PAGE));
  const page = Math.floor(offset / PAGE) + 1;

  const detailAttrs = useMemo(() => detail ?? selected, [detail, selected]);

  if (notReady) {
    return (
      <Card title={t('请求跟踪')}>
        <div className="rt-notice rt-notice-warn" role="status">
          {t('请求跟踪端点尚未就绪（后端建设中）。就绪后这里列出最近请求，点开可查看选路过程与尝试明细。')}
        </div>
      </Card>
    );
  }

  return (
    <div className="rt-trace-layout">
      <Card
        title={t('请求跟踪')}
        className="rt-trace-listcard"
        actions={
          <Button variant="outline" onClick={() => void load(search, offset)} disabled={loading}>
            {loading ? <Loader2 size={13} className="spin" /> : <RefreshCw size={13} />}
            {t('刷新')}
          </Button>
        }
      >
        {/* 参数对齐 Agent2API：搜索 + 分页（无时间窗参数；需要筛模型时用搜索） */}
        <div className="rt-flow-toolbar" role="group" aria-label={t('跟踪筛选')}>
          <input
            className="form-input"
            style={{ maxWidth: 280 }}
            placeholder={t('搜索请求、模型或账号')}
            aria-label={t('搜索请求、模型或账号')}
            value={searchInput}
            onChange={(e) => setSearchInput(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter') {
                setOffset(0);
                setSearch(searchInput);
              }
            }}
          />
          <Button
            variant="outline"
            onClick={() => {
              setOffset(0);
              setSearch(searchInput);
            }}
            disabled={loading}
          >
            {t('查询')}
          </Button>
          <span className="rt-count" style={{ marginLeft: 'auto' }}>
            {t('{{n}} 条', { n: matched })}
          </span>
        </div>

        {error ? <div className="error-message">{error}</div> : null}
        {list?.available === false ? (
          <EmptyState message={t('请求记录数据库不可用，无法列出跟踪记录。')} />
        ) : entries.length ? (
          <div className="rt-trace-list" role="list">
            {entries.map((row, i) => {
              const state = rowState(row);
              const key = `${row.id}-${row.ts ?? i}`;
              return (
                <button
                  key={key}
                  type="button"
                  role="listitem"
                  aria-pressed={selected != null && `${selected.id}-${selected.ts}` === key}
                  className={`rt-trace-row${selected != null && selected.id === row.id ? ' is-on' : ''}`}
                  onClick={() => void openRow(row)}
                >
                  <span className="rt-trace-row-line">
                    <b>{rowModel(row) || t('模型未记录')}</b>
                    <span className={`rt-trace-state is-${state}`}>
                      {state === 'running'
                        ? t('进行中')
                        : state === 'success'
                          ? t('成功')
                          : state === 'failed'
                            ? t('失败')
                            : t('未记录')}
                    </span>
                  </span>
                  <span className="rt-trace-row-sub">
                    <time>{toMillis(row.ts) != null ? new Date(toMillis(row.ts) as number).toLocaleTimeString() : '—'}</time>
                    <span>{` · ${row.providerLabel || row.provider || '—'}`}</span>
                    {row.accountName || row.accountId ? (
                      <span>{` · ${row.accountName || row.accountId}`}</span>
                    ) : null}
                    <span>{` · ${fmtDuration(row.durationMs)}`}</span>
                  </span>
                </button>
              );
            })}
          </div>
        ) : loading ? (
          <div className="rt-special-loading">
            <Loader2 size={14} className="spin" />
            {t('读取中…')}
          </div>
        ) : (
          <EmptyState message={search ? t('没有匹配的请求') : t('当前没有可跟踪的请求')} />
        )}

        <div className="rt-trace-foot">
          <Button
            variant="outline"
            size="sm"
            aria-label={t('上一页')}
            disabled={offset === 0 || loading}
            onClick={() => setOffset(Math.max(0, offset - PAGE))}
          >
            <ChevronLeft size={14} />
          </Button>
          <span>{`${page} / ${pageCount}`}</span>
          <Button
            variant="outline"
            size="sm"
            aria-label={t('下一页')}
            disabled={loading || offset + PAGE >= matched}
            onClick={() => setOffset(offset + PAGE)}
          >
            <ChevronRight size={14} />
          </Button>
        </div>
      </Card>

      <Card title={t('跟踪详情')} className="rt-trace-detailcard">
        {!selected ? (
          <EmptyState message={t('选择左侧请求查看选路过程')} />
        ) : detailLoading ? (
          <div className="rt-special-loading">
            <Loader2 size={14} className="spin" />
            {t('读取详情…')}
          </div>
        ) : detailAttrs ? (
          <div>
            {detailError ? (
              <div className="rt-notice rt-notice-warn">
                {t('详情加载失败：{{msg}}。以下为列表摘要。', { msg: detailError })}
              </div>
            ) : null}

            <div className="rt-trace-detail-head">
              <h3>{rowModel(detailAttrs) || t('模型未记录')}</h3>
              <span className={`rt-trace-state is-${rowState(detailAttrs)}`}>
                {detailAttrs.status != null ? `HTTP ${detailAttrs.status}` : t('未记录')}
              </span>
            </div>

            <div className="rt-trace-metrics">
              <div>
                <b>{detailAttrs.attempts ?? '—'}</b>
                <span>{t('尝试次数')}</span>
              </div>
              <div>
                <b>{fmtDuration(detailAttrs.durationMs)}</b>
                <span>{t('总耗时')}</span>
              </div>
              <div>
                <b>{detailAttrs.route?.ordered?.length ?? '—'}</b>
                <span>{t('候选')}</span>
              </div>
              <div>
                <b>{detailAttrs.route?.blocked?.length ?? '—'}</b>
                <span>{t('被跳过')}</span>
              </div>
            </div>

            <div className="rt-trace-kv">
              <span>{t('提供商：{{v}}', { v: detailAttrs.providerLabel || detailAttrs.provider || '—' })}</span>
              <span>{t('账号：{{v}}', { v: detailAttrs.accountName || detailAttrs.accountId || '—' })}</span>
              {detailAttrs.upstreamModel ? (
                <span>{t('上游模型：{{v}}', { v: detailAttrs.upstreamModel })}</span>
              ) : null}
              {toMillis(detailAttrs.ts) != null ? (
                <span>
                  {t('请求时间 {{time}}', {
                    time: new Date(toMillis(detailAttrs.ts) as number).toLocaleString(),
                  })}
                </span>
              ) : null}
            </div>

            {detailAttrs.error ? (
              <div className="rt-notice rt-notice-warn" style={{ marginTop: 8 }}>
                {detailAttrs.error}
              </div>
            ) : null}

            {Array.isArray(detailAttrs.attemptDetails) && detailAttrs.attemptDetails.length ? (
              <div style={{ marginTop: 12 }}>
                <div className="rt-rail-title">{t('尝试明细')}</div>
                <ol className="rt-trace-attempts">
                  {detailAttrs.attemptDetails.map((a, i) => (
                    <li key={`${a.index ?? i}-${a.channel_id ?? ''}`}>
                      <b>{a.channel_name || a.channel_id || t('未知供应商')}</b>
                      {a.status != null ? (
                        <span className={`rt-trace-state ${a.status >= 200 && a.status < 300 ? 'is-success' : 'is-failed'}`}>
                          {a.status}
                        </span>
                      ) : null}
                      {a.stickyHit ? (
                        <span className="rt-chip rt-chip-model rt-chip-sm">{t('粘滞命中')}</span>
                      ) : null}
                      <span className="rt-muted">
                        {a.finishedAt != null && a.startedAt != null
                          ? ` · ${fmtDuration((a.finishedAt - a.startedAt) * 1000)}`
                          : ''}
                      </span>
                      {a.error ? <p className="rt-trace-err">{a.error}</p> : null}
                    </li>
                  ))}
                </ol>
              </div>
            ) : (
              <div className="form-hint" style={{ marginTop: 10 }}>
                {t('没有逐次尝试明细（该记录未保存或快照未启用）。')}
              </div>
            )}

            {detailAttrs.route?.ordered?.length ? (
              <div style={{ marginTop: 12 }}>
                <div className="rt-rail-title">{t('候选顺序')}</div>
                <ol className="rt-trace-attempts">
                  {detailAttrs.route.ordered.map((c, i) => (
                    <li key={c.id ?? i}>
                      <b>{`${i + 1}. ${c.name ?? c.id ?? ''}`}</b>
                      <span className="rt-muted">{` · ${c.provider ?? ''}${c.state ? ` · ${c.state}` : ''}`}</span>
                    </li>
                  ))}
                </ol>
              </div>
            ) : null}
          </div>
        ) : null}
      </Card>
    </div>
  );
}