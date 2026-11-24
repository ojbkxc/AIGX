/**
 * RoutingTrace —— 请求跟踪：列表 + 详情。
 *
 * 详情区照 A2A `RoutingExecTimeline` 的分段与顺序落地（这是本面板与上游 1:1 对齐的部分）：
 *   ① 头（模型 / 状态 / 客户端 / 时间 / HTTP）→
 *   ② 模型改写（特殊请求路由改写，排在承载之前 —— 请求真实经过的顺序就是「先改写模型、再选账号」）→
 *   ③ 承载（谁最终接了这次请求、为什么是它）→
 *   ④ 执行耗时（逐次尝试的分段条）→
 *   ⑤ 选路情况（候选清单里每个账号这一次的角色）→
 *   ⑥ 当时配置（选路方式 / 会话粘滞；AIGX 未记录配置范围与成员快照，照实显示「未记录」）。
 *
 * 数据源（be-routing）：
 *   GET /api/routing/trace?offset=&limit=&search=  → { entries[], matched, available }
 *   GET /api/routing/trace/request?id=&ts=         → 单条详情
 * 三态降级：`available === false` / 404 未就绪 / 请求失败。
 * 列表手动刷新与翻页；运行中的行不轮询（AIGX 管理面刷新节奏比桌面端保守）。
 */
import { useCallback, useEffect, useMemo, useState, type ReactNode } from 'react';
import { useTranslation } from 'react-i18next';
import { ChevronLeft, ChevronRight, Loader2, RefreshCw } from 'lucide-react';
import { api } from '../../api';
import { Badge, Button, Card, EmptyState } from '../ui';
import {
  type RoutingTraceList,
  type RoutingTraceRow,
} from '../../types/routing-workspace';
import {
  errorText,
  formatDuration,
  formatTraceTokens,
  isTraceCancelled,
  isTraceRunning,
  isTraceSuccess,
  requestModel,
  toMillis,
  traceAttempts,
  traceCandidates,
  traceProvider,
} from './trace-model';
import {
  execAccounts,
  execAttemptName,
  execCarrier,
  execScopeLabel,
  execStickyLabel,
  execStrategyLabel,
  execTiming,
} from './trace-exec-model';
import { requestRewrite, clientText } from './trace-rewrite';
import './trace-exec.css';

const PAGE = 30;

function statusOf(err: unknown): number | undefined {
  return (err as { status?: number } | null)?.status;
}

function rowState(row: RoutingTraceRow): 'running' | 'success' | 'failed' | 'unknown' {
  if (row.status == null) return 'unknown';
  if (row.status === 0) return 'running';
  if (row.status >= 200 && row.status < 300) return 'success';
  return 'failed';
}

const fmtDuration = formatDuration;

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
                    <b>{requestModel(row) || t('模型未记录')}</b>
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
          <>
            {detailError ? (
              <div className="rt-notice rt-notice-warn">
                {t('详情加载失败：{{msg}}。以下为列表摘要。', { msg: detailError })}
              </div>
            ) : null}
            <TraceDetail row={detailAttrs} />
          </>
        ) : null}
      </Card>
    </div>
  );
}

/* ═══════════════ 详情：与 A2A RoutingExecTimeline 同序 ═══════════════ */

/** 详情分段容器（对齐上游 `.routing-exec-section`：上边框 + 段头）。 */
function TraceSection({
  title,
  note,
  children,
}: {
  title: string;
  note?: ReactNode;
  children: ReactNode;
}): JSX.Element {
  return (
    <section className="rt-exec-section">
      <div className="rt-exec-section-head">
        <h3>{title}</h3>
        {note ? <span className="rt-exec-section-note">{note}</span> : null}
      </div>
      {children}
    </section>
  );
}

/** ② 模型改写：特殊请求路由改写过这次请求时才有内容；普通请求整块不渲染。 */
function TraceRewriteBlock({ row }: { row: RoutingTraceRow }): JSX.Element | null {
  const { t } = useTranslation();
  const rewrite = requestRewrite(row, t);
  if (!rewrite) return null;
  return (
    <section className="rt-exec-rewrite" data-kind={rewrite.kind}>
      <header className="rt-exec-rewrite-head">
        <b>{t('模型改写')}</b>
        <Badge tone={rewrite.kind === 'vision' ? 'warning' : rewrite.kind === 'title' ? 'info' : 'neutral'}>
          {rewrite.label}
        </Badge>
        <span>{rewrite.detail}</span>
      </header>
      <div className="rt-exec-rewrite-flow">
        <div className="rt-exec-rewrite-node">
          <span>{t('客户端模型')}</span>
          <b>{rewrite.from || '—'}</b>
        </div>
        <span className="rt-exec-rewrite-arrow" aria-hidden="true">→</span>
        <div className="rt-exec-rewrite-node is-accent">
          <span>{t('实际使用')}</span>
          <b>{rewrite.to || '—'}</b>
        </div>
      </div>
    </section>
  );
}

/** ④ 执行耗时：逐次尝试的分段条（等待响应 → 响应到达后）。 */
function TraceDuration({ row }: { row: RoutingTraceRow }): JSX.Element {
  const { t } = useTranslation();
  const now = Date.now();
  const { total, firstFrame, response, scale, timings } = execTiming(row, now, t);
  const attempts = row.attempts ?? timings.length;
  const recorded = timings.filter((timing) => !timing.attempt.inferred).length;
  const multi = Math.max(attempts, timings.length) > 1;
  const metrics = [
    `${isTraceRunning(row) ? t('已用时') : t('总耗时')} ${formatDuration(total)}`,
    `${t('首帧')} ${firstFrame == null ? '—' : formatDuration(firstFrame)}`,
    `${t('输出')} ${formatTraceTokens(null)} tokens`,
  ];
  return (
    <TraceSection title={t('执行耗时')} note={metrics.join(' · ')}>
      <div className="rt-exec-bars">
        {timings.map((timing) => {
          const { attempt, segments } = timing;
          const info = [
            multi ? `${t('本次用时')} ${formatDuration(timing.duration)}` : '',
            `${t('响应到达')} ${timing.response == null ? '—' : `+${formatDuration(timing.response)}`}`,
          ].filter(Boolean).join(' · ');
          return (
            <div className="rt-exec-bar-row" data-multi={multi ? '' : undefined} key={attempt.index}>
              {multi ? (
                <span
                  className="rt-exec-bar-owner"
                  title={[traceProvider(attempt.provider, null, (k) => t(k)), execAttemptName(attempt, t), attempt.model].filter(Boolean).join(' · ')}
                >
                  {execAttemptName(attempt, t)}
                </span>
              ) : null}
              <div className="rt-exec-bar-cell">
                {segments.length ? (
                  <div className="rt-exec-bar-track" role="img" aria-label={segments.map((segment) => segment.label).join(' · ')}>
                    {segments.map((segment, index) => (
                      <span
                        key={index}
                        data-tone={segment.tone}
                        title={segment.label}
                        style={{
                          left: `${(segment.start / scale) * 100}%`,
                          width: `${((segment.end - segment.start) / scale) * 100}%`,
                        }}
                      />
                    ))}
                  </div>
                ) : null}
                <span className="rt-exec-bar-note">{info}</span>
                {!multi && attempt.notice ? (
                  <span className="rt-exec-bar-note">{errorText(attempt.notice)}</span>
                ) : null}
              </div>
              {multi ? <span className="rt-exec-bar-result" data-tone={timing.tone}>{timing.label}</span> : null}
            </div>
          );
        })}
        {!timings.length ? (
          <p className="rt-exec-note">
            {t('响应到达')} {formatDuration(response)} · {t('没有保存调用明细')}
          </p>
        ) : null}
        {attempts > recorded ? (
          <p className="rt-exec-note">
            {t('共尝试 {{total}} 次，保存了 {{saved}} 条明细', { total: attempts, saved: recorded })}
          </p>
        ) : null}
      </div>
    </TraceSection>
  );
}

/** ⑤ 选路情况：候选清单里每个账号这一次的角色 + 原因。 */
function TraceCandidates({ row }: { row: RoutingTraceRow }): JSX.Element {
  const { t } = useTranslation();
  const entries = execAccounts(row, t);
  const route = row.route;
  const saved = (route?.ordered?.length || 0) + (route?.blocked?.length || 0);
  const recordedTotal = (route?.ordered?.length || 0) + (route?.blocked?.length || 0);
  const partial = saved < recordedTotal;
  const empty = !entries.length
    ? t('没有保存其他候选信息')
    : entries.some((item) => item.current)
      ? t('没有其他候选账号')
      : t('没有保存调用明细');
  return (
    <TraceSection title={t('选路情况')} note={t('已保存 {{n}} 个候选', { n: traceCandidates(row).length })}>
      {entries.length ? (
        <div className="rt-exec-candidates" role="table" aria-label={t('选路情况')}>
          <div className="rt-exec-candidate is-heading" role="row">
            <span role="columnheader">{t('账号')}</span>
            <span role="columnheader">{t('本次状态')}</span>
            <span role="columnheader">{t('原因')}</span>
          </div>
          <div role="rowgroup">
            {entries.map((item) => (
              <div className="rt-exec-candidate" role="row" key={item.key}>
                <div className="rt-exec-candidate-account" role="cell">
                  <div>
                    <b>{item.name}</b>
                    <span>{item.provider}</span>
                  </div>
                  {item.model && item.model !== requestModel(row) ? <code>{item.model}</code> : null}
                </div>
                <span className="rt-exec-candidate-state" role="cell" data-tone={item.tone}>{item.label}</span>
                <span className="rt-exec-candidate-reason" role="cell">{item.reason}</span>
              </div>
            ))}
          </div>
        </div>
      ) : (
        <p className="rt-exec-note">{empty}</p>
      )}
      {partial ? <p className="rt-exec-note">{t('候选快照仅保存部分记录，未保存的账号无法回放')}</p> : null}
    </TraceSection>
  );
}

/** ⑥ 当时配置：AIGX 的 trace 行只记 strategy / sticky，其余照实显示「未记录」。 */
function TraceSnapshot({ row }: { row: RoutingTraceRow }): JSX.Element {
  const { t } = useTranslation();
  const strategy = row.route?.strategy;
  const sticky = row.route?.sticky;
  return (
    <TraceSection title={t('当时配置')}>
      <dl className="rt-exec-definitions">
        <div>
          <dt>{t('配置范围')}</dt>
          <dd>{execScopeLabel(row, t)}</dd>
        </div>
        <div>
          <dt>{t('选路方式')}</dt>
          <dd>{execStrategyLabel(strategy, t)}</dd>
        </div>
        <div>
          <dt>{t('会话粘滞')}</dt>
          <dd>{execStickyLabel(sticky, t)}</dd>
        </div>
        {row.upstreamModel ? (
          <div className="is-wide">
            <dt>{t('上游模型')}</dt>
            <dd className="rt-exec-mono">{row.upstreamModel}</dd>
          </div>
        ) : null}
      </dl>
    </TraceSection>
  );
}

/** 详情主体：① 头 → ② 改写 → ③ 承载 → ④ 耗时 → ⑤ 选路 → ⑥ 当时配置。 */
function TraceDetail({ row }: { row: RoutingTraceRow }): JSX.Element {
  const { t } = useTranslation();
  const carrier = execCarrier(row, t);
  const failed = !isTraceRunning(row) && !isTraceSuccess(row) && !isTraceCancelled(row);
  const ts = toMillis(row.ts);
  const state = rowState(row);
  const attempts = traceAttempts(row);
  return (
    <div className="rt-exec">
      <header className="rt-exec-head">
        <div className="rt-exec-heading">
          <h2>{requestModel(row) || t('模型未记录')}</h2>
          <div className="rt-exec-meta">
            <span className={`rt-trace-state is-${state}`}>
              {state === 'running'
                ? t('进行中')
                : state === 'success'
                  ? t('成功')
                  : state === 'failed'
                    ? t('失败')
                    : t('未记录')}
            </span>
            <span>{clientText(row.client, t)}</span>
            <time title={ts != null ? new Date(ts).toLocaleString() : undefined}>
              {ts != null ? new Date(ts).toLocaleTimeString() : '—'}
            </time>
            {row.status ? <span>{`HTTP ${row.status}`}</span> : null}
            {row.attempts ? <span>{t('尝试 {{n}} 次', { n: row.attempts })}</span> : null}
            {attempts.length && !row.attempts ? <span>{t('尝试 {{n}} 次', { n: attempts.length })}</span> : null}
          </div>
        </div>
      </header>

      {/* 模型改写排在承载之前：请求真实经过的顺序就是「先改写模型、再选账号」 */}
      <TraceRewriteBlock row={row} />

      <section className="rt-exec-carrier" data-failed={failed ? '' : undefined}>
        <p className="rt-exec-label">{carrier.label}</p>
        <p className="rt-exec-carrier-name">{carrier.name}</p>
        <p className="rt-exec-carrier-reason">{carrier.reason}</p>
      </section>

      <TraceDuration row={row} />
      <TraceCandidates row={row} />
      <TraceSnapshot row={row} />
    </div>
  );
}