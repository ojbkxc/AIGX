/**
 * 特殊请求路由「最近命中」右栏（对照上游 ui-islands/routing-special-hits.tsx）。
 *
 * ── 数据从哪来 ────────────────────────────────────────────────
 * 不新增接口：回放列表接口（GET /api/routing/replays，route_replay 有捕获的终态日志）
 * 返回的每一行都带 route 快照，里面的 `compactRoute` / `titleRoute` /
 * `visionCompatibility` 就是「这次请求被谁改写过」——与请求回放页读的是同一份数据。
 * 所以这里取一页最近请求、按改写类型筛出这条路由的即可。
 *
 * ── 为什么不点进详情 ──────────────────────────────────────────
 * 浏览记录只留「请求回放」一个入口（右上角「查看全部」切到回放 rail）。配置页里
 * 再做一套点开详情的交互，两处迟早会走偏，而配置页的主角应该是规则。
 */
import type { TFunction } from 'i18next';
import { useCallback, useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Loader2 } from 'lucide-react';
import { api } from '../../../api';
import { Button } from '../../ui';
import type { RoutingTraceRow } from '../../../types/routing-workspace';
import './SpecialHits.css';

export type SpecialHitsKind = 'compact' | 'title' | 'vision';

/** 取一页最近请求：命中可能稀疏（一条路由的命中混在全部请求里），取宽一点。 */
const WINDOW = 50;
/** 右栏最多列几条：再多就该去请求回放页看了。 */
const SHOWN = 6;
const REFRESH_MS = 15_000;

type Rewrite = { kind: SpecialHitsKind; from: string; to: string };

/** 秒/毫秒自适应（<1e11 视为秒，与账号页/回放页同口径） */
function toMillis(v: number | undefined): number | null {
  if (v == null || !Number.isFinite(v)) return null;
  return v < 1e11 ? v * 1000 : v;
}

function requestModel(row: RoutingTraceRow): string {
  return row.clientModel || row.model || row.upstreamModel || '';
}

/** 从 route 快照里读出这次请求发生过的模型改写（与上游 requestRewrite 同口径）。 */
function requestRewrite(row: RoutingTraceRow, t: TFunction): Rewrite | null {
  const route = row.route;
  if (!route) return null;

  const compact = route.compactRoute;
  if (compact?.source && compact.target) {
    return { kind: 'compact', from: compact.source, to: compact.target };
  }
  const title = route.titleRoute;
  if (title?.source && title.target) {
    return { kind: 'title', from: title.source, to: title.target };
  }
  const vision = route.visionCompatibility;
  if (vision?.mode) {
    // 图片兼容不改「请求的模型」：辅助识图是另起一次读图调用，整体接管才是换模型。
    const calls = Array.isArray(vision.calls) ? vision.calls.length : 0;
    const assist = vision.mode === 'assist_call' || vision.mode === 'assist';
    return {
      kind: 'vision',
      from: requestModel(row),
      to: assist
        ? t('辅助识图 · {{n}} 次读图', { n: calls })
        : (vision.target?.model || t('视觉模型接管')),
    };
  }
  return null;
}

export default function SpecialHits({
  kind,
  active,
  onOpenReplay,
}: {
  kind: SpecialHitsKind;
  active: boolean;
  onOpenReplay(): void;
}): JSX.Element {
  const { t } = useTranslation();
  const [rows, setRows] = useState<RoutingTraceRow[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState('');
  const [tick, setTick] = useState(0);

  // 15s 静默重取（仅当本栏可见；页面隐藏时不打）
  useEffect(() => {
    if (!active) return undefined;
    const timer = window.setInterval(() => {
      if (document.hidden) return;
      setTick((v) => v + 1);
    }, REFRESH_MS);
    return () => window.clearInterval(timer);
  }, [active]);

  const load = useCallback(
    async (aliveRef: { alive: boolean }): Promise<void> => {
      setLoading(true);
      try {
        const res = await api.getRoutingReplays({ offset: 0, limit: WINDOW });
        if (!aliveRef.alive) return;
        const data = res?.data;
        if (data?.available === false) throw new Error(t('请求记录数据库不可用'));
        setRows(Array.isArray(data?.entries) ? data.entries ?? [] : []);
        setError('');
      } catch (err) {
        if (aliveRef.alive) setError(err instanceof Error ? err.message : String(err));
      } finally {
        if (aliveRef.alive) setLoading(false);
      }
    },
    [t],
  );

  useEffect(() => {
    if (!active) return undefined;
    const aliveRef = { alive: true };
    void load(aliveRef);
    return () => {
      aliveRef.alive = false;
    };
  }, [active, tick, load]);

  const hits = useMemo(
    () =>
      rows
        .map((row) => ({ row, rewrite: requestRewrite(row, t) }))
        .filter(
          (item): item is { row: RoutingTraceRow; rewrite: Rewrite } => item.rewrite?.kind === kind,
        )
        .slice(0, SHOWN),
    [rows, kind, t],
  );

  return (
    <aside className="rt-hits" aria-label={t('最近命中')}>
      <header className="rt-hits-head">
        <h3>{t('最近命中')}</h3>
        <span className="rt-hits-count">{t('最近 {{n}} 条', { n: hits.length })}</span>
        <Button variant="outline" size="sm" className="rt-hits-more" onClick={onOpenReplay}>
          {t('查看全部')}
        </Button>
      </header>
      <div className="rt-hits-list">
        {hits.map(({ row, rewrite }, i) => {
          const when = toMillis(row.ts);
          return (
            <div className="rt-hits-row" key={`${row.id}-${row.ts ?? i}`}>
              <span className="rt-hits-flow">
                <span className="rt-hits-from" title={rewrite.from}>{rewrite.from || '—'}</span>
                <span className="rt-hits-arrow" aria-hidden="true">→</span>
                <span className="rt-hits-to" title={rewrite.to}>{rewrite.to}</span>
              </span>
              <span className="rt-hits-meta">
                <span>{when != null ? new Date(when).toLocaleTimeString() : '—'}</span>
                <span className="rt-hits-account" title={row.accountName || row.providerLabel || ''}>
                  {row.accountName || row.providerLabel || row.provider || '—'}
                </span>
                <span className="rt-hits-status">{row.status != null ? row.status : '—'}</span>
              </span>
            </div>
          );
        })}
        {!hits.length ? (
          <p className="rt-hits-empty">
            {loading ? (
              <>
                <Loader2 size={12} className="spin" />
                {t('读取中…')}
              </>
            ) : (
              t('最近没有命中记录')
            )}
          </p>
        ) : null}
      </div>
      {error ? <p className="rt-hits-error" role="alert">{error}</p> : null}
    </aside>
  );
}
