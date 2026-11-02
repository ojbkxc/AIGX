// Top 渠道榜（A2A Top providers/accounts 的 AIGX 合并对应物）：行内占比条
// （CSS 宽度百分比 = count 占该榜总量）+ 五指标右对齐（请求/消费/token/
// 成功率/均延迟），标题小字 = 该维总量（各行相加与标题可对账）。
import { useTranslation } from 'react-i18next';
import { fmtCost, fmtInt, fmtPct, fmtTokens } from '../../lib/format';
import type { TopSource } from '../../hooks/useDashboard';

export default function TopSources({ data, lang }: { data: TopSource[]; lang?: string }): JSX.Element {
  const { t } = useTranslation();
  const total = data.reduce((s, d) => s + (d.count ?? 0), 0);
  const maxCount = Math.max(...data.map((d) => d.count ?? 0), 1);
  return (
    <div>
      <table className="top-rank-table" style={{ width: '100%' }}>
        <thead>
          <tr>
            <th>#</th>
            <th>{t('渠道')}</th>
            <th style={{ width: '22%' }}>{t('请求占比')}</th>
            <th className="num">{t('请求数')}</th>
            <th className="num">{t('消费')}</th>
            <th className="num">{t('Token')}</th>
            <th className="num">{t('成功率')}</th>
            <th className="num">{t('均延迟')}</th>
          </tr>
        </thead>
        <tbody>
          {data.map((s, i) => {
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
    </div>
  );
}
