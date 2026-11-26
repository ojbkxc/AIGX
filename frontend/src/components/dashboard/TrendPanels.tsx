// 报表主体面板集：管理员与普通用户共用同一套渲染，只有标题与数据源不同
// （管理员读全站主档，用户读自己的用户日行；差异在 hooks 层，不在视图层）。
// 旧版两份近乎逐字相同的 JSX 各写一遍，改一处漏一处的风险很高。
//
// 与 A2A 的对齐点（都落在本文件）：
// - 用量趋势的「指标 × 形态」切换（A2A report/daily-trend-panel.tsx:15-26 的
//   METRICS / TYPES SegmentedControl）；
// - 模型饼图 + 供应商饼图共用一套「合并小项」设置（A2A report/usage-donuts.tsx）；
// - Top 模型榜的「前 N 名 / 全部」+ 榜尾占比（A2A report/rank-panel.tsx）。
// 没做的是「维度（模型 / 客户端 / API Key）× 单值过滤」：/report/daily_trend
// 每日只带 top6 模型、没有按天的客户端/Key 拆分，前端筛会静默少算（见交付说明）。
import { useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import type { UseQueryResult } from '@tanstack/react-query';
import { fmtCost, fmtInt, fmtPct, fmtTokens } from '../../lib/format';
import PanelWrap from './PanelWrap';
import TrendBars, { type BarRow, type TrendShape } from './TrendBars';
import TokenTrendChart from './TokenTrendChart';
import DonutChart, { type DonutDatum } from './DonutChart';
import CacheTrend24h from './CacheTrend24h';
import Heatmap from './Heatmap';
import {
  DonutSettingsControl, donutMinPercent, readDonutSettings, saveDonutSettings,
  type DonutSettings,
} from '../report/donut-settings';
import { RankCountControl, RankShareFooter, rankShare, rankSlice, type RankCount } from '../report/rank-controls';
import type {
  CacheRateWindowData, CacheSavingsData, CacheTrendHour, DailyTrendRow,
  HeatmapCell, ReportModelRow, TokenTrendPoint, TopSource,
} from '../../hooks/useDashboard';

// PanelWrap 只关心这四个字段，视图层因此可以接受 UseQueryResult 或它的子集
type PanelQuery<T> = Pick<UseQueryResult<T | null, Error>, 'isPending' | 'isError' | 'refetch' | 'data'>;

/** 趋势指标（A2A TrendMetric 的 AIGX 三档：Token / 请求数，外加本仓已有的消费额） */
export type TrendMetric = 'cost' | 'tokens' | 'requests';

export function ConsumptionPanel({ query, daily, lang, rLabel }: {
  query: PanelQuery<DailyTrendRow[]>;
  daily: DailyTrendRow[];
  lang?: string;
  rLabel: string;
}): JSX.Element {
  const { t } = useTranslation();
  const [metric, setMetric] = useState<TrendMetric>('cost');
  const [shape, setShape] = useState<TrendShape>('bar');

  const metrics = useMemo(() => ([
    { key: 'cost' as const, label: t('消费') },
    { key: 'tokens' as const, label: t('Token') },
    { key: 'requests' as const, label: t('请求数') },
  ]), [t]);
  const shapes = useMemo(() => ([
    { key: 'bar' as const, label: t('柱状') },
    { key: 'line' as const, label: t('折线') },
  ]), [t]);

  const tokensOf = (d: DailyTrendRow): number => (d.input_tokens ?? 0) + (d.output_tokens ?? 0);
  const rows: BarRow[] = useMemo(() => daily.map((d) => {
    const value = metric === 'cost' ? (d.cost ?? 0) : metric === 'tokens' ? tokensOf(d) : (d.count ?? 0);
    return {
      date: d.date ?? '',
      value,
      // 气泡第二行读数跟着指标换：看请求数时补 Token，其余补请求数
      sub: metric === 'requests'
        ? t('{{n}} Token', { n: fmtTokens(tokensOf(d), lang) })
        : t('{{n}} 次请求', { n: fmtInt(d.count, lang) }),
      // 按模型分解只在 Token / 消费口径下有意义（请求数口径会读成「这个模型花了多少钱」）
      models: metric === 'requests'
        ? undefined
        : (d.models ?? []).map((m) => ({ model: m.model ?? '?', tokens: m.tokens ?? 0 })),
    };
  }), [daily, metric, t, lang]);

  const total = rows.reduce((s, r) => s + r.value, 0);
  const average = rows.length ? total / rows.length : 0;
  const fmt = (v: number): string =>
    metric === 'cost' ? fmtCost(v, lang) : metric === 'tokens' ? fmtTokens(v, lang) : fmtInt(v, lang);

  return (
    <PanelWrap
      title={t('用量趋势（{{range}}）', { range: t(rLabel) })}
      query={query}
      extra={(
        <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap', marginTop: 8 }}>
          <div className="seg" role="group" aria-label={t('趋势统计指标')}>
            {metrics.map((m) => (
              <button
                key={m.key}
                type="button"
                className={`seg-btn${metric === m.key ? ' is-active' : ''}`}
                aria-pressed={metric === m.key}
                onClick={() => setMetric(m.key)}
              >
                {m.label}
              </button>
            ))}
          </div>
          <div className="seg" role="group" aria-label={t('图表类型')}>
            {shapes.map((s) => (
              <button
                key={s.key}
                type="button"
                className={`seg-btn${shape === s.key ? ' is-active' : ''}`}
                aria-pressed={shape === s.key}
                onClick={() => setShape(s.key)}
              >
                {s.label}
              </button>
            ))}
          </div>
        </div>
      )}
      emptyText={t('所选范围内还没有消费记录')}
    >
      {() => (
        <>
          <div className="trend-reading">
            <span><strong>{fmt(total)}</strong> {t('合计')}</span>
            <span>{t('日均 {{value}}', { value: fmt(average) })}</span>
          </div>
          <TrendBars rows={rows} shape={shape} formatValue={fmt} formatAxis={(v) => fmt(v)} lang={lang} />
        </>
      )}
    </PanelWrap>
  );
}

export function TokenTrendPanel({ query, lang, rLabel }: {
  query: PanelQuery<TokenTrendPoint[]>;
  lang?: string;
  rLabel: string;
}): JSX.Element {
  const { t } = useTranslation();
  return (
    <PanelWrap title={t('Token 趋势（输入 / 输出，{{range}}）', { range: t(rLabel) })} query={query}
      style={{ marginTop: 16 }} emptyText={t('暂无趋势数据')}>
      {(data) => <TokenTrendChart data={data} lang={lang} />}
    </PanelWrap>
  );
}

/**
 * 模型分布 + 供应商分布双环形图 + Top 模型榜（窄屏自动堆叠）。
 *
 * 双饼图对齐 A2A report/usage-donuts.tsx：模型 / 提供商各一张，共用同一套
 * 「合并小项」设置（开关 + 阈值，localStorage 记住）。供应商那张只在管理员视图
 * 出现 —— 个人口径的 top_sources 行里 tokens 恒为 null（后端不给个人 token），
 * 画出来会是一圈空环。
 */
export function ModelPanels({
  query, donutData, colors, totalTokens, lang, rLabel, personal, providers, providersTotal, providersColors, providersQuery,
}: {
  query: PanelQuery<ReportModelRow[]>;
  donutData: DonutDatum[];
  colors: Map<string, string>;
  /** 环形图中心读数（总 token）：图例占比的共同分母 */
  totalTokens: number;
  lang?: string;
  rLabel: string;
  /** 普通用户视图：所有模型都是自己的，「我的 Top 模型」 */
  personal: boolean;
  /** 供应商饼图数据（管理员视图才给；不给则不渲染第二张饼图） */
  providers?: DonutDatum[];
  providersTotal?: number;
  providersColors?: Map<string, string>;
  providersQuery?: PanelQuery<TopSource[]>;
}): JSX.Element {
  const { t } = useTranslation();
  const [settings, setSettings] = useState<DonutSettings>(readDonutSettings);
  const changeSettings = (next: DonutSettings): void => {
    setSettings(next);
    saveDonutSettings(next);
  };
  const minPercent = donutMinPercent(settings);
  const showProviders = Boolean(providers && providersQuery);
  return (
    <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fit, minmax(min(400px, 100%), 1fr))', gap: 16, marginTop: 16 }}>
      <PanelWrap
        title={t('模型分布（{{range}}）', { range: t(rLabel) })}
        query={query}
        extra={<div style={{ marginTop: 8 }}><DonutSettingsControl settings={settings} onChange={changeSettings} /></div>}
        emptyText={t('暂无模型分布数据')}
      >
        {() => (
          <DonutChart data={donutData} colors={colors} minPercent={minPercent}
            totalText={fmtTokens(totalTokens, lang)} totalSub={t('总 Token')} otherName={t('其他')} />
        )}
      </PanelWrap>
      {showProviders ? (
        <PanelWrap title={t('供应商分布（{{range}}）', { range: t(rLabel) })} query={providersQuery!}
          emptyText={t('暂无供应商分布数据')}>
          {() => (
            <DonutChart data={providers!} colors={providersColors ?? new Map()} minPercent={minPercent}
              totalText={fmtTokens(providersTotal ?? 0, lang)} totalSub={t('总 Token')} otherName={t('其他')} />
          )}
        </PanelWrap>
      ) : null}
      <PanelWrap title={t(personal ? '我的 Top 模型（{{range}}）' : 'Top 模型（{{range}}）', { range: t(rLabel) })}
        query={query} emptyText={t('暂无模型数据')}>
        {(data: ReportModelRow[]) => <TopModelsTable rows={data} lang={lang} />}
      </PanelWrap>
    </div>
  );
}

/** 缓存四窗口 + 近 24h 双轴折线（节省估算挂在小标题右侧） */
export function CachePanels({ ratesQuery, trendQuery, savings, lang }: {
  ratesQuery: PanelQuery<CacheRateWindowData[]>;
  trendQuery: PanelQuery<CacheTrendHour[]>;
  savings?: CacheSavingsData | null;
  lang?: string;
}): JSX.Element {
  const { t } = useTranslation();
  return (
    <>
      <PanelWrap title={t('缓存命中率（请求数口径）')} query={ratesQuery} style={{ marginTop: 16 }}
        extra={savings?.saved_estimate != null ? (
          <span style={{ fontSize: 12, color: 'var(--text-muted)' }}>
            {t('估算节省')} {fmtCost(savings.saved_estimate, lang)}{savings.approx ? ` (${t('近似')})` : ''}
          </span>
        ) : null}
        emptyText={t('所选范围内没有缓存请求')}>
        {(data: CacheRateWindowData[]) => (
          <div className="stats-grid" style={{ gridTemplateColumns: 'repeat(auto-fill, minmax(160px, 1fr))' }}>
            {data.map((w) => (
              <div key={w.window} className="stat-card">
                <div className="stat-title">{w.window}</div>
                <div className="stat-value" style={{ fontSize: '22px' }}>{fmtPct(w.rate)}</div>
                <div className="stat-desc" style={{ fontSize: '11px' }}>
                  {t('命中')} {fmtInt(w.hit_requests, lang)} / {fmtInt(w.requests, lang)} {t('次请求')}
                  {w.sampled ? ` · ${t('抽样')}` : ''}
                </div>
              </div>
            ))}
          </div>
        )}
      </PanelWrap>

      <PanelWrap title={t('近 24 小时缓存命中')} query={trendQuery} style={{ marginTop: 16 }}
        emptyText={t('近 24 小时没有请求')}>
        {(data: CacheTrendHour[]) => <CacheTrend24h data={data} lang={lang} />}
      </PanelWrap>
    </>
  );
}

export function HeatmapPanel({ query, lang }: { query: PanelQuery<HeatmapCell[]>; lang?: string }): JSX.Element {
  const { t } = useTranslation();
  return (
    <PanelWrap title={t('消费热力图（近 365 天）')} query={query} style={{ marginTop: 16 }}
      emptyText={t('近一年还没有使用记录')}>
      {(data: HeatmapCell[]) => <Heatmap data={data} lang={lang} />}
    </PanelWrap>
  );
}

/**
 * Top 模型表：请求数 + 占比条 + 消费/Token（右对齐 num 列，与 Top 渠道榜同法）。
 * 「前 N 名 / 全部」+ 榜尾占比对齐 A2A rank-panel：A2A 的榜按 Token 排，这里按
 * 后端给的请求数序（`/report/models` 不截断，行数是完整模型数，所以「全部」是真全部）。
 */
function TopModelsTable({ rows, lang }: {
  rows: Array<{ model?: string; count?: number; input_tokens?: number; output_tokens?: number; cost?: number }>;
  lang?: string;
}): JSX.Element {
  const { t } = useTranslation();
  const [count, setCount] = useState<RankCount>(10);
  const shown = useMemo(() => rankSlice(rows, count), [rows, count]);
  const rowsTotal = rows.reduce((s, r) => s + (r.count ?? 0), 0);
  const shownSum = shown.reduce((s, r) => s + (r.count ?? 0), 0);
  const total = rowsTotal;
  const maxCount = Math.max(...shown.map((r) => r.count ?? 0), 1);
  const share = rankShare(shownSum, 0, rowsTotal);
  return (
    <>
      <div className="rank-toolbar">
        <RankCountControl count={count} onChange={setCount} label={t('模型显示数量')} />
      </div>
      <table className="top-rank-table" style={{ width: '100%' }}>
        <thead>
          <tr>
            <th>#</th>
            <th>{t('模型')}</th>
            <th style={{ width: '22%' }}>{t('请求占比')}</th>
            <th className="num">{t('请求数')}</th>
            <th className="num">{t('消费')}</th>
            <th className="num">{t('Token')}</th>
          </tr>
        </thead>
        <tbody>
          {shown.map((r, i) => {
            const pct = total > 0 ? ((r.count ?? 0) / total) * 100 : 0;
            return (
              <tr key={`${i}-${r.model}`}>
                <td style={{ color: 'var(--text-muted)' }}>{i + 1}</td>
                <td style={{ overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', maxWidth: 160 }} title={r.model}>{r.model || '—'}</td>
                <td>
                  <div className="share-bar">
                    <div className="share-bar-fill" style={{ width: `${Math.max(2, ((r.count ?? 0) / maxCount) * 100)}%` }} />
                  </div>
                </td>
                <td className="num">{fmtInt(r.count, lang)}<span className="pct-hint"> {pct.toFixed(1)}%</span></td>
                <td className="num">{fmtCost(r.cost, lang)}</td>
                <td className="num">{fmtTokens((r.input_tokens ?? 0) + (r.output_tokens ?? 0), lang)}</td>
              </tr>
            );
          })}
        </tbody>
      </table>
      <RankShareFooter
        count={shown.length}
        share={share}
        metric={t('请求')}
        totalText={`${t('合计')} ${fmtInt(rowsTotal, lang)}`}
      />
    </>
  );
}
