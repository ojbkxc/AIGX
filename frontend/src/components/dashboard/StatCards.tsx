// 概览统计卡（区间概览六格：总请求/成功请求/总 Token/总成本/活跃天数/连续天数）。
// 三张重复卡已删：Top 模型（ModelPanels 已有）、缓存节省估算（CachePanels 已有）、
// 渠道（供应商健康面板已有）——见规格 §12.2。
// 成功率 0 请求时给「—」而不是 0%（A2A 空数据口径）；连续天数含今天往前数。
import { useTranslation } from 'react-i18next';
import { StatCard } from '../ui';
import { fmtCost, fmtInt, fmtTokens } from '../../lib/format';
import type { OverviewData } from '../../hooks/useDashboard';

interface Props {
  overview: OverviewData | null;
  lang?: string;
  rangeLabel?: string;
}

export default function StatCards({ overview, lang, rangeLabel }: Props): JSX.Element {
  const { t } = useTranslation();
  const o = overview ?? {};
  const total = o.total_requests ?? 0;
  const success = o.success_requests ?? 0;
  const rate = total > 0 ? ((success / total) * 100).toFixed(1) + '%' : '—';
  const cards: JSX.Element[] = [
    <StatCard
      key="requests"
      title={t('总请求数')}
      value={fmtInt(total, lang)}
      desc={rangeLabel ? t('范围：{{r}}', { r: rangeLabel }) : t('所选时间范围')}
    />,
    <StatCard
      key="success"
      title={t('成功请求数')}
      value={fmtInt(success, lang)}
      desc={`${t('成功率')} ${rate}`}
    />,
    <StatCard
      key="tokens"
      title={t('总 Token')}
      value={fmtTokens((o.input_tokens ?? 0) + (o.output_tokens ?? 0), lang)}
      desc={`${t('输入')} ${fmtTokens(o.input_tokens, lang)} / ${t('输出')} ${fmtTokens(o.output_tokens, lang)}`}
    />,
    <StatCard
      key="cost"
      title={t('总成本')}
      value={fmtCost(o.total_cost, lang)}
      desc={t('配额口径，与日志页可对账')}
    />,
    <StatCard
      key="activeDays"
      title={t('活跃天数')}
      value={fmtInt(o.active_days, lang)}
      desc={t('区间内有请求的天数')}
    />,
    <StatCard
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
