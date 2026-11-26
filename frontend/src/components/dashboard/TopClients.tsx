// Top 客户端榜（客户端识别维度；对照 Agent2API report client 视角）。
// 行内占比条 + 五指标右对齐（请求/消费/token/成功率/均延迟），标题小字
// 为该维总量。**没读到客户端 = 空名**：界面只给占位符 `—`，不显示任何
// `unknown` 之类的假客户端名（后端也不会把空值合并成一个 unknown 分组）。
//
// 本文件承担两处 A2A 对齐：
// 1) 「前 N 名 / 全部」+ 榜尾占比合计（A2A rank-panel.tsx，见 report/rank-controls.tsx）；
// 2) 客户端统计表：按 Token / 按请求数 切换排序 + 点行跳请求日志
//    （A2A source-stats.tsx:43-77 的 SegmentedControl 与 :98-123 的行点击下钻）。
//    跳转走 AIGX 的 router（`?` 参数）+ Logs 既有筛选记忆，见 report/logs-drilldown.ts。
import { useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { useNavigate } from 'react-router-dom';
import { fmtCost, fmtInt, fmtPct, fmtTokens } from '../../lib/format';
import type { TopClient } from '../../hooks/useDashboard';
import { RankCountControl, RankShareFooter, rankShare, rankSlice, type RankCount } from '../report/rank-controls';
import { logsDrilldownPath, primeLogsFilters } from '../report/logs-drilldown';
import '../report/report-panels.css';

/** 排序口径：A2A source-stats 的 `sort` 只有 tokens / requests 两档 */
type ClientSort = 'requests' | 'tokens';

export default function TopClients({ data, lang, totalRequests, drill }: {
  data: TopClient[];
  lang?: string;
  /** 窗口内总请求（概览 total_requests）：榜尾占比分母；缺省退回榜内合计 */
  totalRequests?: number | null;
  /** 点行下钻的目标窗口（概览同窗口的起止自然日）；不给则不渲染跳转 */
  drill?: { start?: string; end?: string } | null;
}): JSX.Element {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const [count, setCount] = useState<RankCount>(5);
  const [sort, setSort] = useState<ClientSort>('requests');

  // 个人视图 tokens 恒为 null（后端不给个人口径的 token），此时「按 Token」排不出
  // 任何顺序（全是 null）—— 直接禁掉，而不是给一个点了没反应的按钮。
  const hasTokens = useMemo(() => data.some((c) => c.tokens != null), [data]);

  const sorted = useMemo(() => {
    if (sort === 'tokens' && hasTokens) {
      return [...data].sort((a, b) => (b.tokens ?? 0) - (a.tokens ?? 0) || (b.count ?? 0) - (a.count ?? 0));
    }
    return [...data].sort((a, b) => (b.count ?? 0) - (a.count ?? 0) || (b.tokens ?? 0) - (a.tokens ?? 0));
  }, [data, sort, hasTokens]);

  const rows = useMemo(() => rankSlice(sorted, count), [sorted, count]);
  const rowsTotal = useMemo(() => data.reduce((s, d) => s + (d.count ?? 0), 0), [data]);
  const shownSum = useMemo(() => rows.reduce((s, d) => s + (d.count ?? 0), 0), [rows]);
  const total = rowsTotal;
  const maxCount = Math.max(...rows.map((d) => d.count ?? 0), 1);
  const share = rankShare(shownSum, totalRequests ?? 0, rowsTotal);

  // 下钻：先写 Logs 的筛选记忆（否则页面打开但筛选不生效），再带 `?` 参数导航
  const openLogs = (client: string): void => {
    if (!client || client === '—') return;
    const q = { client, start: drill?.start, end: drill?.end };
    primeLogsFilters(q);
    navigate(logsDrilldownPath(q));
  };

  return (
    <div>
      <div className="rank-toolbar">
        <div className="seg" role="group" aria-label={t('客户端排序')}>
          <button
            type="button"
            className={`seg-btn${sort === 'requests' ? ' is-active' : ''}`}
            aria-pressed={sort === 'requests'}
            onClick={() => setSort('requests')}
          >
            {t('按请求数')}
          </button>
          <button
            type="button"
            className={`seg-btn${sort === 'tokens' ? ' is-active' : ''}`}
            aria-pressed={sort === 'tokens'}
            disabled={!hasTokens}
            title={hasTokens ? undefined : t('个人视图没有 Token 读数，只能按请求数排序')}
            onClick={() => setSort('tokens')}
          >
            {t('按 Token')}
          </button>
        </div>
        <RankCountControl count={count} onChange={setCount} label={t('客户端显示数量')} />
      </div>
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
            {drill ? <th className="num" style={{ width: 72 }}>{t('请求日志')}</th> : null}
          </tr>
        </thead>
        <tbody>
          {rows.map((c, i) => {
            const pct = total > 0 ? ((c.count ?? 0) / total) * 100 : 0;
            const name = c.name?.trim() || '—';
            const canDrill = Boolean(drill) && name !== '—';
            return (
              <tr
                key={`${i}-${c.name}`}
                className={canDrill ? 'clickable-row' : undefined}
                onClick={canDrill ? () => openLogs(name) : undefined}
              >
                <td style={{ color: 'var(--text-muted)' }}>{i + 1}</td>
                <td style={{ overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', maxWidth: 160 }} title={name}>
                  {canDrill ? (
                    <button type="button" className="row-link" title={name} onClick={(e) => { e.stopPropagation(); openLogs(name); }}>
                      {name}
                    </button>
                  ) : name}
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
                {drill ? (
                  <td className="num">
                    {canDrill ? (
                      <button type="button" className="row-link" onClick={(e) => { e.stopPropagation(); openLogs(name); }}>
                        {t('查看')}
                      </button>
                    ) : '—'}
                  </td>
                ) : null}
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
