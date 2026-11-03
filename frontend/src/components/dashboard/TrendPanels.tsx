// 报表主体面板集：管理员与普通用户共用同一套渲染，只有标题与数据源不同
// （管理员读全站主档，用户读自己的用户日行；差异在 hooks 层，不在视图层）。
// 旧版两份近乎逐字相同的 JSX 各写一遍，改一处漏一处的风险很高。
import { useTranslation } from 'react-i18next';
import type { UseQueryResult } from '@tanstack/react-query';
import { fmtCost, fmtInt, fmtPct, fmtTokens } from '../../lib/format';
import PanelWrap from './PanelWrap';
import TrendBars, { type BarRow } from './TrendBars';
import TokenTrendChart from './TokenTrendChart';
import DonutChart, { type DonutDatum } from './DonutChart';
import CacheTrend24h from './CacheTrend24h';
import Heatmap from './Heatmap';
import type {
  CacheRateWindowData, CacheSavingsData, CacheTrendHour, DailyTrendRow,
  HeatmapCell, ReportModelRow, TokenTrendPoint,
} from '../../hooks/useDashboard';

// PanelWrap 只关心这四个字段，视图层因此可以接受 UseQueryResult 或它的子集
type PanelQuery<T> = Pick<UseQueryResult<T | null, Error>, 'isPending' | 'isError' | 'refetch' | 'data'>;

export function ConsumptionPanel({ query, rows, lang, rLabel }: {
  query: PanelQuery<DailyTrendRow[]>;
  rows: BarRow[];
  lang?: string;
  rLabel: string;
}): JSX.Element {
  const { t } = useTranslation();
  return (
    <PanelWrap title={t('消费趋势（{{range}}）', { range: t(rLabel) })} query={query}
      emptyText={t('所选范围内还没有消费记录')}>
      {() => (
        <TrendBars rows={rows} formatValue={(v) => fmtCost(v, lang)} formatAxis={(v) => fmtCost(v, lang)} lang={lang} />
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

/** 模型分布环形图 + Top 模型榜（左右并排，窄屏自动堆叠） */
export function ModelPanels({ query, donutData, colors, totalTokens, lang, rLabel, personal }: {
  query: PanelQuery<ReportModelRow[]>;
  donutData: DonutDatum[];
  colors: Map<string, string>;
  /** 环形图中心读数（总 token）：图例占比的共同分母 */
  totalTokens: number;
  lang?: string;
  rLabel: string;
  /** 普通用户视图：所有模型都是自己的，「我的 Top 模型」 */
  personal: boolean;
}): JSX.Element {
  const { t } = useTranslation();
  return (
    <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fit, minmax(min(400px, 100%), 1fr))', gap: 16, marginTop: 16 }}>
      <PanelWrap title={t('模型分布（{{range}}）', { range: t(rLabel) })} query={query} emptyText={t('暂无模型分布数据')}>
        {() => (
          <DonutChart data={donutData} colors={colors}
            totalText={fmtTokens(totalTokens, lang)} totalSub={t('总 Token')} otherName={t('其他')} />
        )}
      </PanelWrap>
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

/** Top 模型表：请求数 + 占比条 + 消费/Token（右对齐 num 列，与 Top 渠道榜同法） */
export function TopModelsTable({ rows, lang }: {
  rows: Array<{ model?: string; count?: number; input_tokens?: number; output_tokens?: number; cost?: number }>;
  lang?: string;
}): JSX.Element {
  const { t } = useTranslation();
  const total = rows.reduce((s, r) => s + (r.count ?? 0), 0);
  const maxCount = Math.max(...rows.map((r) => r.count ?? 0), 1);
  return (
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
        {rows.slice(0, 10).map((r, i) => {
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
  );
}
