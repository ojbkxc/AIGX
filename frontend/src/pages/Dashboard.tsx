// 仪表盘（阶段 4 重构）：每面板独立 TanStack Query 取数 + PanelWrap 外壳，
// 单面板失败只坏该面板（旧版 allSettled+pick(i) 位置化会让残缺数字无差别
// 铺满全页）。权限矩阵：管理员 = A2A 全量报表 ∪ AIGX 渠道/租户维（一张不落）；
// 普通用户 = 个人账单口径（report_scope 收敛），admin 专属端点 enabled=false
// 根本不发请求。时间档六档含自定义区间（start/end 参数，热力图固定 365 天）。
import { useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Link } from 'react-router-dom';
import { useIsFetching, useQueryClient } from '@tanstack/react-query';
import { isAdmin } from '../lib/utils';
import { SkeletonCards } from '../components/ui';
import { fmtCost, fmtInt, fmtPct, fmtTokens, nameColorMap, toCsv, downloadText } from '../lib/format';
import PanelWrap from '../components/dashboard/PanelWrap';
import RangeSwitcher, { initialRange, rangeLabel as rangeLabelOf, windowOf, readCustomRange, type RangeKey } from '../components/dashboard/RangeSwitcher';
import StatCards from '../components/dashboard/StatCards';
import QuotaCards from '../components/dashboard/QuotaCards';
import TrendBars from '../components/dashboard/TrendBars';
import TokenTrendChart from '../components/dashboard/TokenTrendChart';
import DonutChart from '../components/dashboard/DonutChart';
import Heatmap from '../components/dashboard/Heatmap';
import TopSources from '../components/dashboard/TopSources';
import UserRanking from '../components/dashboard/UserRanking';
import ChannelHealth from '../components/dashboard/ChannelHealth';
import CacheTrend24h from '../components/dashboard/CacheTrend24h';
import {
  useCacheRates, useCacheSavings, useCacheTrend24h, useChannelHealth as useChannelHealthQ,
  useDailyTrend, useHeatmap, useLimits, useOverview, usePersonalMe, useRealtime,
  useReportModels, useTodayTokens, useTokenTrend, useTopSources, useUsageSummary, useUserRanking,
} from '../hooks/useDashboard';
import './Dashboard.css';

function RefreshIcon({ spinning }: { spinning: boolean }): JSX.Element {
  return (
    <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"
      style={spinning ? { animation: 'spin 0.9s linear infinite' } : undefined}>
      <polyline points="23 4 23 10 17 10" />
      <polyline points="1 20 1 14 7 14" />
      <path d="M3.51 9a9 9 0 0 1 14.85-3.36L23 10M1 14l4.64 4.36A9 9 0 0 0 20.49 15" />
    </svg>
  );
}

export default function Dashboard(): JSX.Element {
  const { t, i18n } = useTranslation();
  const lang = i18n.language;
  const admin = isAdmin();
  const queryClient = useQueryClient();
  const fetching = useIsFetching({ queryKey: ['dashboard'] });

  const [range, setRange] = useState<RangeKey>(() => initialRange());
  const [custom, setCustom] = useState(() => readCustomRange());
  const win = useMemo(() => windowOf(range, custom), [range, custom]);
  const rLabel = rangeLabelOf(range, custom);

  // 全部 hooks 无条件调用（enabled 控制是否发请求，避免 admin 端点在个人视图 403）。
  const overviewQ = useOverview(win);
  const savingsQ = useCacheSavings(win);
  const dailyQ = useDailyTrend(win);
  const tokenTrendQ = useTokenTrend(win);
  const modelsQ = useReportModels(win);
  const ratesQ = useCacheRates(win);
  const trend24Q = useCacheTrend24h();
  const heatmapQ = useHeatmap({ days: 365 });
  const sourcesQ = useTopSources(win, { enabled: admin });
  const userRankQ = useUserRanking(win, { enabled: admin });
  const healthQ = useChannelHealthQ(win, { enabled: admin });
  const realtimeQ = useRealtime({ enabled: admin });
  const todayQ = useTodayTokens({ enabled: admin });
  const usageQ = useUsageSummary({ enabled: admin });
  const limitsQ = useLimits({ enabled: admin });
  const meQ = usePersonalMe({ enabled: !admin });

  const ov = overviewQ.data;
  const savings = savingsQ.data;
  const daily = dailyQ.data ?? [];
  const models = modelsQ.data ?? [];
  const health = healthQ.data ?? [];
  const rt = realtimeQ.data;
  const ts = todayQ.data;
  const us = usageQ.data;
  const lm = limitsQ.data;
  const me = meQ.data;

  const channels = admin && health.length > 0
    ? {
        total: health.length,
        circuitOpen: health.filter((c) => String(c.circuit_breaker || 'closed').toLowerCase() === 'open').length,
        enabled: health.length,
      }
    : null;

  const trendRows = useMemo(() => daily.map((d) => ({
    date: d.date ?? '',
    value: d.cost ?? 0,
    sub: t('{{n}} 次请求', { n: fmtInt(d.count, lang) }),
    models: (d.models ?? []).map((m) => ({ model: m.model ?? '?', tokens: m.tokens ?? 0 })),
  })), [daily, t, lang]);

  const donutData = models.map((m) => ({
    name: m.model ?? '—',
    value: m.count ?? 0,
    labelValue: fmtTokens((m.input_tokens ?? 0) + (m.output_tokens ?? 0), lang),
  }));
  const donutColors = useMemo(() => nameColorMap(models, (m) => m.model ?? '', (m) => m.count ?? 0), [models]);
  const modelTotalCount = models.reduce((s, m) => s + (m.count ?? 0), 0);

  const monthlyUsed = me?.used_quota ?? lm?.monthly_used ?? 0;
  const monthlyLimit = me && !admin ? me.quota ?? null : lm?.monthly_limit ?? null;

  const refresh = (): void => {
    void queryClient.invalidateQueries({ queryKey: ['dashboard'] });
  };
  const exportCsv = (): void => {
    downloadText(
      `aigx-report-${range === 'custom' ? `${custom.start}_${custom.end}` : `${range}d`}.csv`,
      toCsv(daily.map((d) => ({
        date: d.date ?? '',
        requests: d.count ?? 0,
        input_tokens: d.input_tokens ?? 0,
        output_tokens: d.output_tokens ?? 0,
        cost: d.cost ?? 0,
      }))),
    );
  };

  const header = (
    <div className="page-header">
      <div>
        <h1>{t('仪表盘')}</h1>
        <p>{admin ? t('AI 网关使用概览') : t('我的用量概览')}</p>
      </div>
      <div style={{ display: 'flex', alignItems: 'center', gap: 12, flexWrap: 'wrap' }}>
        <RangeSwitcher range={range} custom={custom} onChange={setRange} onCustomChange={setCustom} />
        <button className="btn btn-outline btn-sm" onClick={exportCsv} disabled={daily.length === 0} style={{ gap: 6 }}>
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
            <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" />
            <polyline points="7 10 12 15 17 10" />
            <line x1="12" y1="15" x2="12" y2="3" />
          </svg>
          {t('导出 CSV')}
        </button>
        <button className="btn btn-outline btn-sm" onClick={refresh} style={{ gap: 6 }}>
          <RefreshIcon spinning={fetching > 0} />
          {t('刷新')}
        </button>
      </div>
    </div>
  );

  // 首屏（无任何缓存数据时）整体骨架；有数据后切档/刷新由面板各自 placeholder 承接。
  const firstLoad = admin
    ? (overviewQ.isPending && realtimeQ.isPending && todayQ.isPending)
    : (overviewQ.isPending && meQ.isPending);
  if (firstLoad) {
    return (
      <div>
        {header}
        <SkeletonCards count={6} />
      </div>
    );
  }

  const statCardsBlock = (
    <div style={{ marginBottom: 16 }}>
      {overviewQ.isError ? (
        <div className="error-message">
          {t('概览加载失败')}
          <button className="btn btn-outline btn-sm" style={{ marginLeft: 8 }} onClick={() => void overviewQ.refetch()}>{t('重试')}</button>
        </div>
      ) : overviewQ.isPending ? (
        <SkeletonCards count={6} />
      ) : (
        <StatCards
          overview={ov ?? null}
          savedEstimate={savings?.saved_estimate}
          savedApprox={savings?.approx}
          channels={channels}
          lang={lang}
          rangeLabel={rLabel}
        />
      )}
    </div>
  );

  // ── 普通用户个人视图 ──
  if (!admin) {
    return (
      <div>
        {header}
        <QuotaCards monthlyUsed={monthlyUsed} monthlyLimit={monthlyLimit} lang={lang} />
        <div style={{ marginTop: 16 }} />
        {statCardsBlock}

        <PanelWrap title={t('消费趋势（{{range}}）', { range: t(rLabel) })} query={dailyQ}
          emptyText={t('所选范围内还没有消费记录')}>
          {() => (
            <TrendBars rows={trendRows} formatValue={(v) => fmtCost(v, lang)} formatAxis={(v) => fmtCost(v, lang)} lang={lang} />
          )}
        </PanelWrap>

        <PanelWrap title={t('Token 趋势（输入 / 输出，{{range}}）', { range: t(rLabel) })} query={tokenTrendQ} style={{ marginTop: 16 }}
          emptyText={t('暂无趋势数据')}>
          {(data) => <TokenTrendChart data={data} lang={lang} />}
        </PanelWrap>

        <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fit, minmax(min(400px, 100%), 1fr))', gap: 16, marginTop: 16 }}>
          <PanelWrap title={t('模型分布（{{range}}）', { range: t(rLabel) })} query={modelsQ} emptyText={t('暂无模型分布数据')}>
            {() => (
              <DonutChart data={donutData} colors={donutColors}
                totalText={fmtInt(modelTotalCount, lang)} totalSub={t('次调用')} otherName={t('其他')} />
            )}
          </PanelWrap>
          <PanelWrap title={t('我的 Top 模型（{{range}}）', { range: t(rLabel) })} query={modelsQ} emptyText={t('暂无模型数据')}>
            {(data: typeof models) => <TopModelsTable rows={data} lang={lang} />}
          </PanelWrap>
        </div>

        <PanelWrap title={t('缓存命中率（请求数口径）')} query={ratesQ} style={{ marginTop: 16 }}
          emptyText={t('所选范围内没有缓存请求')}>
          {(data) => (
            <div className="stats-grid" style={{ gridTemplateColumns: 'repeat(auto-fill, minmax(160px, 1fr))' }}>
              {data.map((w: { window?: string; rate?: number; hit_requests?: number; requests?: number }) => (
                <div key={w.window} className="stat-card">
                  <div className="stat-title">{w.window}</div>
                  <div className="stat-value" style={{ fontSize: '22px' }}>{fmtPct(w.rate)}</div>
                  <div className="stat-desc" style={{ fontSize: '11px' }}>
                    {t('命中')} {fmtInt(w.hit_requests, lang)} / {fmtInt(w.requests, lang)} {t('次请求')}
                  </div>
                </div>
              ))}
            </div>
          )}
        </PanelWrap>

        <PanelWrap title={t('近 24 小时缓存命中')} query={trend24Q} style={{ marginTop: 16 }}
          emptyText={t('近 24 小时没有请求')}>
          {(data) => <CacheTrend24h data={data} lang={lang} />}
        </PanelWrap>

        <PanelWrap title={t('消费热力图（近 365 天）')} query={heatmapQ} style={{ marginTop: 16 }}
          emptyText={t('近一年还没有使用记录')}>
          {(data) => <Heatmap data={data} lang={lang} />}
        </PanelWrap>

        {/* 快捷操作（个人工具箱） */}
        <div className="card" style={{ marginTop: 16 }}>
          <div className="card-header">
            <h2>{t('快捷操作')}</h2>
          </div>
          <div className="card-body quick-actions">
            <Link to="/playground" className="btn btn-primary">{t('去 Playground 调试')}</Link>
            <Link to="/keys" className="btn btn-outline">{t('管理 API 密钥')}</Link>
            <Link to="/logs" className="btn btn-outline">{t('用量日志')}</Link>
            <Link to="/wallet" className="btn btn-outline">{t('钱包充值')}</Link>
            <Link to="/profile" className="btn btn-outline">{t('个人中心')}</Link>
          </div>
        </div>
      </div>
    );
  }

  // ── 管理员全局视图 ──
  return (
    <div>
      {header}

      {/* 实时 4 卡（30s 轮询，页面隐藏自动暂停） */}
      {!realtimeQ.isPending && (
        <div className="stats-grid" style={{ gridTemplateColumns: 'repeat(auto-fill, minmax(180px, 1fr))', marginBottom: 16 }}>
          <div className="stat-card">
            <div className="stat-card-top">
              <div className="stat-title">{t('实时 RPS')}</div>
              <div className="stat-icon-badge" style={{ background: 'rgba(34, 197, 94, 0.15)', color: '#34d399' }}>⚡</div>
            </div>
            <div className="stat-value" style={{ fontSize: '24px' }}>{fmtInt(rt?.qps ?? rt?.rps ?? 0, lang)}</div>
            <div className="stat-desc" style={{ fontSize: '11px' }}>{t('每秒请求数')}</div>
          </div>
          <div className="stat-card">
            <div className="stat-card-top">
              <div className="stat-title">{t('活跃用户')}</div>
              <div className="stat-icon-badge" style={{ background: 'rgba(47, 111, 237, 0.15)', color: '#7ca4f5' }}>👥</div>
            </div>
            <div className="stat-value" style={{ fontSize: '24px' }}>{fmtInt(rt?.active_users ?? userRankQ.data?.length, lang)}</div>
            <div className="stat-desc" style={{ fontSize: '11px' }}>{t('近 5 分钟')}</div>
          </div>
          <div className="stat-card">
            <div className="stat-card-top">
              <div className="stat-title">{t('活跃渠道')}</div>
              <div className="stat-icon-badge" style={{ background: 'rgba(47, 111, 237, 0.15)', color: '#7ca4f5' }}>🔗</div>
            </div>
            <div className="stat-value" style={{ fontSize: '24px' }}>{fmtInt(rt?.active_channels ?? health.length, lang)}</div>
            <div className="stat-desc" style={{ fontSize: '11px' }}>{t('近 5 分钟')}</div>
          </div>
          <div className="stat-card">
            <div className="stat-card-top">
              <div className="stat-title">{t('平均延迟')}</div>
              <div className="stat-icon-badge" style={{ background: 'rgba(251, 146, 60, 0.15)', color: '#fb923c' }}>⏱️</div>
            </div>
            <div className="stat-value" style={{ fontSize: '24px' }}>{fmtInt(rt?.avg_latency_ms ?? 0, lang)}ms</div>
            <div className="stat-desc" style={{ fontSize: '11px' }}>{t('近 5 分钟平均')}</div>
          </div>
        </div>
      )}

      {/* 全站累计三卡（今日 / 总量 / 限额） */}
      <div className="stats-grid" style={{ marginBottom: 16 }}>
        <div className="stat-card">
          <div className="stat-card-top">
            <div className="stat-title">{t('今日用量')}</div>
            <div className="stat-icon-badge" style={{ background: 'rgba(47, 111, 237, 0.15)', color: '#7ca4f5' }}>📊</div>
          </div>
          <div className="stat-value" style={{ fontSize: '26px' }}>{fmtTokens(ts?.total_tokens, lang)}</div>
          <div className="stat-desc" style={{ display: 'flex', gap: '16px', fontSize: '11px' }}>
            <span>↑ {t('输入')} {fmtTokens(ts?.input_tokens, lang)}</span>
            <span>↓ {t('输出')} {fmtTokens(ts?.output_tokens, lang)}</span>
          </div>
          <div className="stat-sub">
            <span>{fmtInt(ts?.request_count, lang)} {t('次请求')}</span>
          </div>
        </div>
        <div className="stat-card">
          <div className="stat-card-top">
            <div className="stat-title">{t('Token 统计')}</div>
            <div className="stat-icon-badge" style={{ background: 'rgba(47, 111, 237, 0.15)', color: '#7ca4f5' }}>🔤</div>
          </div>
          <div className="stat-value" style={{ fontSize: '26px' }}>{fmtTokens(us?.total_tokens, lang)}</div>
          <div className="stat-desc" style={{ fontSize: '11px' }}>
            <div>{t('输入')}: {fmtTokens(us?.total_input_tokens ?? us?.total_tokens, lang)}</div>
            <div>{t('输出')}: {fmtTokens(us?.total_output_tokens, lang)}</div>
          </div>
        </div>
        <div className="stat-card">
          <div className="stat-card-top">
            <div className="stat-title">{t('本月用量限额')}</div>
            <div className="stat-icon-badge" style={{ background: 'rgba(16, 185, 129, 0.15)', color: '#34d399' }}>📅</div>
          </div>
          <div className="stat-value" style={{ fontSize: '26px' }}>{monthlyLimit != null ? fmtInt(monthlyLimit, lang) : '∞'}</div>
          <div className="stat-desc" style={{ fontSize: '11px' }} title={t('统计口径：token（本月累计），与「配额/余额」的货币口径不同')}>
            <span>{t('已用')} {fmtInt(monthlyUsed, lang)} tokens</span>
            {monthlyLimit != null && monthlyLimit > 0 && (
              <span style={{ marginLeft: 12 }}>{((monthlyUsed / monthlyLimit) * 100).toFixed(1)}%</span>
            )}
          </div>
        </div>
      </div>

      {statCardsBlock}

      <PanelWrap title={t('消费趋势（{{range}}）', { range: t(rLabel) })} query={dailyQ}
        emptyText={t('所选范围内还没有消费记录')}>
        {() => (
          <TrendBars rows={trendRows} formatValue={(v) => fmtCost(v, lang)} formatAxis={(v) => fmtCost(v, lang)} lang={lang} />
        )}
      </PanelWrap>

      <PanelWrap title={t('Token 趋势（输入 / 输出，{{range}}）', { range: t(rLabel) })} query={tokenTrendQ} style={{ marginTop: 16 }}
        emptyText={t('暂无趋势数据')}>
        {(data) => <TokenTrendChart data={data} lang={lang} />}
      </PanelWrap>

      {/* 缓存效率：四窗口卡 + 近 24h 双轴折线 */}
      <PanelWrap title={t('缓存命中率（请求数口径）')} query={ratesQ} style={{ marginTop: 16 }}
        extra={savings?.saved_estimate != null ? (
          <span style={{ fontSize: 12, color: 'var(--text-muted)' }}>
            {t('估算节省')} {fmtCost(savings.saved_estimate, lang)}{savings.approx ? ` (${t('近似')})` : ''}
          </span>
        ) : null}
        emptyText={t('所选范围内没有缓存请求')}>
        {(data) => (
          <div className="stats-grid" style={{ gridTemplateColumns: 'repeat(auto-fill, minmax(160px, 1fr))' }}>
            {data.map((w: { window?: string; rate?: number; hit_requests?: number; requests?: number }) => (
              <div key={w.window} className="stat-card">
                <div className="stat-title">{w.window}</div>
                <div className="stat-value" style={{ fontSize: '22px' }}>{fmtPct(w.rate)}</div>
                <div className="stat-desc" style={{ fontSize: '11px' }}>
                  {t('命中')} {fmtInt(w.hit_requests, lang)} / {fmtInt(w.requests, lang)} {t('次请求')}
                </div>
              </div>
            ))}
          </div>
        )}
      </PanelWrap>

      <PanelWrap title={t('近 24 小时缓存命中')} query={trend24Q} style={{ marginTop: 16 }}
        emptyText={t('近 24 小时没有请求')}>
        {(data) => <CacheTrend24h data={data} lang={lang} />}
      </PanelWrap>

      {/* 模型分布环形图 + Top 模型榜 */}
      <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fit, minmax(min(400px, 100%), 1fr))', gap: 16, marginTop: 16 }}>
        <PanelWrap title={t('模型分布（{{range}}）', { range: t(rLabel) })} query={modelsQ} emptyText={t('暂无模型分布数据')}>
          {() => (
            <DonutChart data={donutData} colors={donutColors}
              totalText={fmtInt(modelTotalCount, lang)} totalSub={t('次调用')} otherName={t('其他')} />
          )}
        </PanelWrap>
        <PanelWrap title={t('Top 模型（{{range}}）', { range: t(rLabel) })} query={modelsQ} emptyText={t('暂无模型数据')}>
          {(data: typeof models) => <TopModelsTable rows={data} lang={lang} />}
        </PanelWrap>
      </div>

      {/* Top 渠道榜 + 用户消费榜 */}
      <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fit, minmax(min(400px, 100%), 1fr))', gap: 16, marginTop: 16 }}>
        <PanelWrap title={t('Top 渠道（{{range}}）', { range: t(rLabel) })} query={sourcesQ} emptyText={t('暂无渠道数据')}>
          {(data) => <TopSources data={data} lang={lang} />}
        </PanelWrap>
        <PanelWrap title={t('用户消费排行（Top 20）')} query={userRankQ} emptyText={t('暂无用户排行数据')}>
          {(data) => <UserRanking data={data} lang={lang} />}
        </PanelWrap>
      </div>

      <PanelWrap title={t('消费热力图（近 365 天）')} query={heatmapQ} style={{ marginTop: 16 }}
        emptyText={t('近一年还没有使用记录')}>
        {(data) => <Heatmap data={data} lang={lang} />}
      </PanelWrap>

      <PanelWrap title={t('渠道健康状态')} query={healthQ} style={{ marginTop: 16 }}
        emptyText={t('暂无渠道健康数据')}>
        {(data) => <ChannelHealth data={data} lang={lang} />}
      </PanelWrap>

      {/* 快捷操作（管理员工具箱） */}
      <div className="card" style={{ marginTop: 16 }}>
        <div className="card-header">
          <h2>{t('快捷操作')}</h2>
        </div>
        <div className="card-body quick-actions">
          <Link to="/channels" className="btn btn-primary">{t('管理渠道')}</Link>
          <Link to="/keys" className="btn btn-outline">{t('管理 API 密钥')}</Link>
          <Link to="/mappings" className="btn btn-outline">{t('配置模型映射')}</Link>
          <Link to="/logs" className="btn btn-outline">{t('查看日志')}</Link>
          <Link to="/redemptions" className="btn btn-outline">{t('兑换码管理')}</Link>
          <Link to="/settings" className="btn btn-outline">{t('调整限额')}</Link>
        </div>
      </div>
    </div>
  );
}

// Top 模型表：请求数 + 占比条 + 消费/Token（右对齐 num 列，与 Top 渠道榜同法）。
function TopModelsTable({ rows, lang }: { rows: Array<{ model?: string; count?: number; input_tokens?: number; output_tokens?: number; cost?: number }>; lang?: string }): JSX.Element {
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
