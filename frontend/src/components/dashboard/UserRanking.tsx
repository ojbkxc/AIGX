// 用户消费 Top 榜（AIGX 多租户独有维度）：行内消费占比条 + 请求/Token/消费。
import { useTranslation } from 'react-i18next';
import { fmtCost, fmtInt, fmtTokens } from '../../lib/format';
import type { UserRankRow } from '../../hooks/useDashboard';

export default function UserRanking({ data, lang }: { data: UserRankRow[]; lang?: string }): JSX.Element {
  const { t } = useTranslation();
  const rows = data.map((r) => ({
    label: r.email || r.username || `User#${r.user_id ?? '?'}`,
    count: r.request_count ?? r.requests ?? r.count ?? 0,
    tokens: r.total_tokens ?? r.tokens ?? 0,
    cost: r.total_cost ?? r.cost ?? 0,
  }));
  const maxCost = Math.max(...rows.map((r) => r.cost), 1);
  const totalCost = rows.reduce((s, r) => s + r.cost, 0);
  return (
    <table className="top-rank-table" style={{ width: '100%' }}>
      <thead>
        <tr>
          <th>#</th>
          <th>{t('用户')}</th>
          <th style={{ width: '26%' }}>{t('消费占比')}</th>
          <th className="num">{t('请求数')}</th>
          <th className="num">{t('Token')}</th>
          <th className="num">{t('消费')}</th>
        </tr>
      </thead>
      <tbody>
        {rows.map((r, i) => (
          <tr key={`${i}-${r.label}`}>
            <td style={{ color: 'var(--text-muted)' }}>{i + 1}</td>
            <td style={{ overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', maxWidth: 160 }} title={r.label}>{r.label}</td>
            <td>
              <div className="share-bar">
                <div className="share-bar-fill" style={{ width: `${Math.max(2, (r.cost / maxCost) * 100)}%` }} />
              </div>
            </td>
            <td className="num">{fmtInt(r.count, lang)}</td>
            <td className="num">{fmtTokens(r.tokens, lang)}</td>
            <td className="num">
              {fmtCost(r.cost, lang)}
              <span className="pct-hint"> {totalCost > 0 ? ((r.cost / totalCost) * 100).toFixed(1) : '0.0'}%</span>
            </td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}
