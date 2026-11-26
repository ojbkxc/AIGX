// Top 渠道榜（A2A Top providers/accounts 的 AIGX 合并对应物）：行内占比条
// （CSS 宽度百分比 = count 占该榜总量）+ 五指标右对齐（请求/消费/token/
// 成功率/均延迟），标题小字 = 该维总量（各行相加与标题可对账）。
//
// 「前 N 名 / 全部」选择器 + 榜尾占比合计：对齐 A2A report/rank-panel.tsx
// 的 COUNTS 选择器与「前 {n} 名占全部用量 {pct}」合计行，口径差异见
// components/report/rank-controls.tsx 头部注释（后端 truncate(10)、分母取概览总量）。
import { useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { fmtCost, fmtInt, fmtPct, fmtTokens } from '../../lib/format';
import type { TopSource } from '../../hooks/useDashboard';
import { RankCountControl, RankShareFooter, rankShare, rankSlice, type RankCount } from '../report/rank-controls';

export default function TopSources({ data, lang, totalRequests }: {
  data: TopSource[];
  lang?: string;
  /** 窗口内全站总请求（概览 total_requests）：榜尾占比的分母；缺省退回榜内合计 */
  totalRequests?: number | null;
}): JSX.Element {
  const { t } = useTranslation();
  // 默认 5：与 A2A 的 useState('5') 一致（榜单一眼可读完，需要更多可切「全部」）
  const [count, setCount] = useState<RankCount>(5);
  const rows = useMemo(() => rankSlice(data, count), [data, count]);
  const rowsTotal = useMemo(() => data.reduce((s, d) => s + (d.count ?? 0), 0), [data]);
  const shownSum = useMemo(() => rows.reduce((s, d) => s + (d.count ?? 0), 0), [rows]);
  // 行内占比的分母用全量行（切 5 名后各行占比不跳变，只是少列几行）
  const total = rowsTotal;
  const maxCount = Math.max(...rows.map((d) => d.count ?? 0), 1);
  const share = rankShare(shownSum, totalRequests ?? 0, rowsTotal);

  return (
    <div>
      <div className="rank-toolbar">
        <RankCountControl count={count} onChange={setCount} label={t('供应商显示数量')} />
      </div>
      <table className="top-rank-table" style={{ width: '100%' }}>
        <thead>
          <tr>
            <th>#</th>
            <th>{t('供应商')}</th>
            <th style={{ width: '22%' }}>{t('请求占比')}</th>
            <th className="num">{t('请求数')}</th>
            <th className="num">{t('消费')}</th>
            <th className="num">{t('Token')}</th>
            <th className="num">{t('成功率')}</th>
            <th className="num">{t('均延迟')}</th>
          </tr>
        </thead>
        <tbody>
          {rows.map((s, i) => {
            const pct = total > 0 ? ((s.count ?? 0) / total) * 100 : 0;
            return (
              <tr key={`${i}-${s.name}`}>
                <td style={{ color: 'var(--text-muted)' }}>{i + 1}</td>
                <td style={{ overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', maxWidth: 160 }} title={s.name}>
                  {s.name || '—'}
                </td>
                <td>
                  {/* 占比条：宽度按 count/maxCount（榜内相对高度），读数用全榜占比 */}
                  <div className="share-bar">
                    <div className="share-bar-fill" style={{ width: `${Math.max(2, ((s.count ?? 0) / maxCount) * 100)}%` }} />
                  </div>
                </td>
                <td className="num">{fmtInt(s.count, lang)}<span className="pct-hint"> {pct.toFixed(1)}%</span></td>
                <td className="num">{fmtCost(s.cost, lang)}</td>
                <td className="num">{s.tokens != null ? fmtTokens(s.tokens, lang) : '—'}</td>
                <td className="num">{s.success_rate != null ? fmtPct(s.success_rate) : '—'}</td>
                <td className="num">{s.avg_latency_ms != null ? `${fmtInt(s.avg_latency_ms, lang)}ms` : '—'}</td>
              </tr>
            );
          })}
        </tbody>
      </table>
      <RankShareFooter
        count={rows.length}
        share={share}
        metric={t('请求')}
        totalText={`${t('合计')} ${fmtInt(rowsTotal, lang)}`}
      />
    </div>
  );
}
