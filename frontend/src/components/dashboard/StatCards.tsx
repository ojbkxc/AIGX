// 概览统计卡（区间概览六格：总请求/成功请求/总 Token/总成本/活跃天数/连续天数）。
// 三张重复卡已删：Top 模型（ModelPanels 已有）、缓存节省估算（CachePanels 已有）、
// 渠道（供应商健康面板已有）——见规格 §12.2。
// 成功率 0 请求时给「—」而不是 0%（A2A 空数据口径）；连续天数含今天往前数。
// 趋势（#11 锦上添花）：可选 prev（上期概览）给出环比 delta；series（如每日
// 请求数）给小 sparkline。两者缺省时卡片与旧版逐字一致（向后兼容，接线由
// 调用方决定——Dashboard 页未在本轮改动范围内，故暂不传）。
import { useTranslation } from 'react-i18next';
import { fmtCost, fmtInt, fmtTokens, round1 } from '../../lib/format';
import type { OverviewData } from '../../hooks/useDashboard';

interface Props {
  overview: OverviewData | null;
  lang?: string;
  rangeLabel?: string;
  /** 上期概览（环比 delta 用；缺省不显示 delta） */
  prev?: OverviewData | null;
  /** 迷你趋势序列（如每日请求数；缺省不显示 sparkline） */
  series?: number[];
}

/** 环比：返回「▲/▼ x.x%」；基期为 0/缺省或差值 <0.05% 时不显示 */
function deltaPct(cur: number, prevVal: number | undefined | null): string | null {
  const base = Number(prevVal);
  if (!(base > 0)) return null;
  const pct = ((cur - base) / base) * 100;
  if (Math.abs(pct) < 0.05) return null;
  return `${pct > 0 ? '▲' : '▼'} ${Math.abs(pct).toFixed(1)}%`;
}

/** 百分比点差（成功率这类百分比指标用 pp） */
function deltaPP(cur: number | undefined, prevVal: number | undefined): string | null {
  if (cur == null || prevVal == null) return null;
  const diff = cur - prevVal;
  if (Math.abs(diff) < 0.05) return null;
  return `${diff > 0 ? '▲' : '▼'} ${Math.abs(diff).toFixed(1)}pp`;
}

/** 小 sparkline：面积+折线，走势向上用 mint、向下用 coral（数据缺省不渲染） */
function Sparkline({ values, up }: { values: number[]; up: boolean }): JSX.Element | null {
  if (values.length < 2) return null;
  const w = 88;
  const h = 24;
  const pad = 2;
  const max = Math.max(...values, 0);
  const min = Math.min(...values, 0);
  const span = max - min || 1;
  const x = (i: number): number => pad + (i * (w - 2 * pad)) / (values.length - 1);
  const y = (v: number): number => h - pad - ((v - min) / span) * (h - 2 * pad);
  const line = values.map((v, i) => `${i === 0 ? 'M' : 'L'} ${round1(x(i))} ${round1(y(v))}`).join(' ');
  const color = up ? 'var(--mint)' : 'var(--coral)';
  return (
    <svg width={w} height={h} viewBox={`0 0 ${w} ${h}`} style={{ display: 'block', marginTop: 6 }}>
      <path d={`${line} L ${round1(x(values.length - 1))} ${h - pad} L ${round1(x(0))} ${h - pad} Z`} fill={color} opacity="0.15" />
      <path d={line} fill="none" stroke={color} strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round" />
    </svg>
  );
}

/** 带趋势的统计卡：复用 App.css 的 stat-card 体系（与 LiveCards 同款布局） */
function TrendCard({
  title, value, desc, delta, series,
}: {
  title: string;
  value: string;
  desc: string;
  delta?: string | null;
  series?: number[];
}): JSX.Element {
  const last = series && series.length ? series[series.length - 1] : 0;
  const prev = series && series.length > 1 ? series[series.length - 2] : 0;
  return (
    <div className="stat-card">
      <div className="stat-title">{title}</div>
      <div className="stat-value">{value}</div>
      {delta != null && (
        <div className="stat-desc" style={{ fontSize: 11 }}>
          <span style={{ color: delta.startsWith('▲') ? 'var(--success-color)' : 'var(--danger-color)' }}>{delta}</span>
        </div>
      )}
      {series != null && series.length >= 2 && <Sparkline values={series} up={last >= prev} />}
      {desc && <div className="stat-desc">{desc}</div>}
    </div>
  );
}

export default function StatCards({ overview, lang, rangeLabel, prev, series }: Props): JSX.Element {
  const { t } = useTranslation();
  const o = overview ?? {};
  const total = o.total_requests ?? 0;
  const success = o.success_requests ?? 0;
  const rate = total > 0 ? ((success / total) * 100).toFixed(1) + '%' : '—';
  const p = prev ?? {};
  const cards: JSX.Element[] = [
    <TrendCard
      key="requests"
      title={t('总请求数')}
      value={fmtInt(total, lang)}
      delta={deltaPct(total, p.total_requests)}
      series={series}
      desc={rangeLabel ? t('范围：{{r}}', { r: rangeLabel }) : t('所选时间范围')}
    />,
    <TrendCard
      key="success"
      title={t('成功请求数')}
      value={fmtInt(success, lang)}
      delta={total > 0 && p.total_requests != null && p.total_requests > 0 && p.success_requests != null ? deltaPP(success / total * 100, p.success_requests / p.total_requests * 100) : null}
      desc={`${t('成功率')} ${rate}`}
    />,
    <TrendCard
      key="tokens"
      title={t('总 Token')}
      value={fmtTokens((o.input_tokens ?? 0) + (o.output_tokens ?? 0), lang)}
      delta={deltaPct((o.input_tokens ?? 0) + (o.output_tokens ?? 0), (p.input_tokens ?? 0) + (p.output_tokens ?? 0))}
      desc={`${t('输入')} ${fmtTokens(o.input_tokens, lang)} / ${t('输出')} ${fmtTokens(o.output_tokens, lang)}`}
    />,
    <TrendCard
      key="cost"
      title={t('总成本')}
      value={fmtCost(o.total_cost, lang)}
      delta={deltaPct(o.total_cost ?? 0, p.total_cost)}
      desc={t('配额口径，与日志页可对账')}
    />,
    <TrendCard
      key="activeDays"
      title={t('活跃天数')}
      value={fmtInt(o.active_days, lang)}
      delta={deltaPct(o.active_days ?? 0, p.active_days)}
      desc={t('区间内有请求的天数')}
    />,
    <TrendCard
      key="streak"
      title={t('当前连续天数')}
      value={fmtInt(o.streak, lang)}
      desc={t('含今天在内往前数')}
    />,
  ];
  return (
    <div className="stats-grid" style={{ gridTemplateColumns: 'repeat(auto-fill, minmax(180px, 1fr))' }}>
      {cards}
    </div>
  );
}
