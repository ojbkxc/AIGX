// Top 客户端榜（客户端识别维度；对照 Agent2API report client 视角）。
// 行内占比条 + 五指标右对齐（请求/消费/token/成功率/均延迟），标题小字
// 为该维总量。`unknown` 在界面上显示为「未知客户端」。
import { useTranslation } from 'react-i18next';
import { fmtCost, fmtInt, fmtPct, fmtTokens } from '../../lib/format';
import type { TopClient } from '../../hooks/useDashboard';

export default function TopClients({ data, lang }: { data: TopClient[]; lang?: string }): JSX.Element {
  const { t } = useTranslation();
  const total = data.reduce((s, d) => s + (d.count ?? 0), 0);
  const maxCount = Math.max(...data.map((d) => d.count ?? 0), 1);
  return (
    <div>
      <table className="top-rank-table" style={{ width: '100%' }}>
        <thead>
          <tr>
            <th>#</th>
            <th>{t('客户端')}</th>
            <th style={{ width: '22%' }}>{t('请求占比')}</th>
            <th className="num">{t('请求数')}</th>
            <th className="num">{t('消费')}</th>
            <th className="num">{t('Token')}</th>
            <th className="num">{t('成功率')}</th>
            <th className="num">{t('均延迟')}</th>
          </tr>
        </thead>
        <tbody>
          {data.map((c, i) => {
            const pct = total > 0 ? ((c.count ?? 0) / total) * 100 : 0;
            const name = c.name?.trim() ? c.name : t('未知客户端');
            return (
              <tr key={`${i}-${c.name}`}>
                <td style={{ color: 'var(--text-muted)' }}>{i + 1}</td>
                <td style={{ overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', maxWidth: 160 }} title={name}>
                  {name}
                </td>
                <td>
                  {/* 占比条：宽度按 count/maxCount（榜内相对高度），读数用全榜占比 */}
                  <div className="share-bar">
                    <div className="share-bar-fill" style={{ width: `${Math.max(2, ((c.count ?? 0) / maxCount) * 100)}%` }} />
                  </div>
                </td>
                <td className="num">{fmtInt(c.count, lang)}<span className="pct-hint"> {pct.toFixed(1)}%</span></td>
                <td className="num">{fmtCost(c.cost, lang)}</td>
                <td className="num">{c.tokens != null ? fmtTokens(c.tokens, lang) : '—'}</td>
                <td className="num">{c.success_rate != null ? fmtPct(c.success_rate) : '—'}</td>
                <td className="num">{c.avg_latency_ms != null ? `${fmtInt(c.avg_latency_ms, lang)}ms` : '—'}</td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}
