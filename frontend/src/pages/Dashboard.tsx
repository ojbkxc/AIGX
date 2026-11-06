// 仪表盘（阶段 4 重构）：每面板独立 TanStack Query 取数 + PanelWrap 外壳，
// 单面板失败只坏该面板（旧版 allSettled+pick(i) 位置化会让残缺数字无差别
// 铺满全页）。权限矩阵：管理员 = A2A 全量报表 ∪ AIGX 渠道/租户维（一张不落）；
// 普通用户 = 个人账单口径（report_scope 收敛），admin 专属端点 enabled=false
// 根本不发请求。时间档六档含自定义区间（start/end 参数，热力图固定 365 天）。
import { useEffect, useMemo, useRef, useState } from 'react';
import { useAutoRefresh } from '../hooks/useAutoRefresh';
import { useTranslation } from 'react-i18next';
import { Link } from 'react-router-dom';
import { useIsFetching, useQueryClient } from '@tanstack/react-query';
import { isAdmin } from '../lib/utils';
import { SkeletonCards } from '../components/ui';
import { fmtInt, fmtTokens, nameColorMap, toCsv, downloadText } from '../lib/format';
import PanelWrap from '../components/dashboard/PanelWrap';
import RangeSwitcher, { initialRange, rangeLabel as rangeLabelOf, windowOf, readCustomRange, type RangeKey } from '../components/dashboard/RangeSwitcher';
import StatCards from '../components/dashboard/StatCards';
import QuotaCards from '../components/dashboard/QuotaCards';
import TopSources from '../components/dashboard/TopSources';
import UserRanking from '../components/dashboard/UserRanking';
import ChannelHealth from '../components/dashboard/ChannelHealth';
import LiveCards from '../components/dashboard/LiveCards';
import {
  CachePanels, ConsumptionPanel, HeatmapPanel, ModelPanels, TokenTrendPanel,
} from '../components/dashboard/TrendPanels';
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
  const scope = admin ? 'all' : 'personal';

  // 角色变更清缓存：admin↔user 切换时清除仪表盘缓存，避免数据串号。
  // scope 已在 queryKey 中隔离（'all' vs 'personal'），此处清理旧 scope 残留。
  const prevAdmin = useRef(admin);
  useEffect(() => {
    if (prevAdmin.current !== admin) {
      queryClient.removeQueries({ queryKey: ['dashboard'] });
      prevAdmin.current = admin;
    }
  }, [admin, queryClient]);

  const [range, setRange] = useState<RangeKey>(() => initialRange());
  const [custom, setCustom] = useState(() => readCustomRange());
  const win = useMemo(() => windowOf(range, custom), [range, custom]);
  const rLabel = rangeLabelOf(range, custom);

  // 全部 hooks 无条件调用（enabled 控制是否发请求，避免 admin 端点在个人视图 403）。
  // scope 维度随 admin 角色走：admin='all' / user='personal'，queryKey 自动隔离。
  const overviewQ = useOverview(win, { scope });
  const savingsQ = useCacheSavings(win, { scope });
  const dailyQ = useDailyTrend(win, { scope });
  const tokenTrendQ = useTokenTrend(win, { scope });
  const modelsQ = useReportModels(win, { scope });
  const ratesQ = useCacheRates(win, { scope });
  const trend24Q = useCacheTrend24h({ scope });
  const heatmapQ = useHeatmap({ days: 365 }, { scope });
  const sourcesQ = useTopSources(win, { enabled: admin, scope });
  const userRankQ = useUserRanking(win, { enabled: admin, scope });
  const healthQ = useChannelHealthQ(win, { enabled: admin, scope });
  const realtimeQ = useRealtime({ enabled: admin, scope });
  const todayQ = useTodayTokens({ enabled: admin, scope });
  const usageQ = useUsageSummary({ enabled: admin, scope });
  const limitsQ = useLimits({ enabled: admin, scope });
  const meQ = usePersonalMe({ enabled: !admin, scope });

  // 报表自动刷新：按「定时任务」页配置的 reportAutoRefresh 周期重拉仪表盘。
  // 页面不可见时跳过，配置改动最多滞后一个周期生效。
  useAutoRefresh('reportAutoRefresh', () => {
    queryClient.invalidateQueries({ queryKey: ['dashboard'] });
  });

  const ov = overviewQ.data;
  const savings = savingsQ.data;
  const daily = useMemo(() => dailyQ.data ?? [], [dailyQ.data]);
  const models = useMemo(() => modelsQ.data ?? [], [modelsQ.data]);
  const health = healthQ.data ?? [];
  const rt = realtimeQ.data;
  const ts = todayQ.data;
  const us = usageQ.data;
  const lm = limitsQ.data;
  const me = meQ.data;

  // 启用渠道数按 status==='enabled' 过滤（后端 channel_health 返回 status 字段，
  // 取值 enabled/disabled，见 legacy.rs handle_channel_health；ChannelHealthRow
  // 类型未声明该字段，用断言消费，避免为单个字段改 useDashboard.ts）。
  const channels = admin && health.length > 0
    ? {
        total: health.length,
        circuitOpen: health.filter((c) => String(c.circuit_breaker || 'closed').toLowerCase() === 'open').length,
        enabled: health.filter((c) => (c as { status?: string }).status === 'enabled').length,
      }
    : null;

  const trendRows = useMemo(() => daily.map((d) => ({
    date: d.date ?? '',
    value: d.cost ?? 0,
    sub: t('{{n}} 次请求', { n: fmtInt(d.count, lang) }),
    models: (d.models ?? []).map((m) => ({ model: m.model ?? '?', tokens: m.tokens ?? 0 })),
  })), [daily, t, lang]);

  // 环形图按 token 分片（A2A donutView 口径）：扇区角表达相对占比、中心给
  // 共同分母（总 token）、图例每行是绝对量 + 占比 —— 只看角度读不出量级，
  // 而这正是运维要问的「这段是 1.2 亿还是 1200 万」。
  const tokenOf = (m: { input_tokens?: number; output_tokens?: number }): number =>
    (m.input_tokens ?? 0) + (m.output_tokens ?? 0);
  const donutData = models.map((m) => ({
    name: m.model ?? '—',
    value: tokenOf(m),
    labelValue: fmtTokens(tokenOf(m), lang),
  }));
  const donutColors = useMemo(() => nameColorMap(models, (m) => m.model ?? '', tokenOf), [models]);
  const modelTotalTokens = models.reduce((s, m) => s + tokenOf(m), 0);

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

        <ConsumptionPanel query={dailyQ} rows={trendRows} lang={lang} rLabel={rLabel} />
        <TokenTrendPanel query={tokenTrendQ} lang={lang} rLabel={rLabel} />
        <ModelPanels query={modelsQ} donutData={donutData} colors={donutColors}
          totalTokens={modelTotalTokens} lang={lang} rLabel={rLabel} personal />
        <CachePanels ratesQuery={ratesQ} trendQuery={trend24Q} savings={savings} lang={lang} />
        <HeatmapPanel query={heatmapQ} lang={lang} />

        {/* 快捷操作（个人工具箱） */}
        <div className="card" style={{ marginTop: 16 }}>
          <div className="card-header">
            <h2>{t('快捷操作')}</h2>
          </div>
          <div className="card-body quick-actions">
            <Link to="/chat" className="btn btn-primary">{t('打开聊天')}</Link>
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

      <LiveCards
        realtime={rt} realtimePending={realtimeQ.isPending}
        realtimeError={realtimeQ.isError} realtimeRefetch={realtimeQ.refetch}
        activeUsersFallback={userRankQ.data?.length}
        activeChannelsFallback={health.length}
        today={ts} todayError={todayQ.isError} todayRefetch={todayQ.refetch}
        usage={us} usageError={usageQ.isError} usageRefetch={usageQ.refetch}
        monthlyUsed={monthlyUsed} monthlyLimit={monthlyLimit}
        limitsError={limitsQ.isError} limitsRefetch={limitsQ.refetch}
        lang={lang}
      />

      {statCardsBlock}

      <ConsumptionPanel query={dailyQ} rows={trendRows} lang={lang} rLabel={rLabel} />
      <TokenTrendPanel query={tokenTrendQ} lang={lang} rLabel={rLabel} />
      {/* 缓存效率：四窗口卡 + 近 24h 双轴折线 */}
      <CachePanels ratesQuery={ratesQ} trendQuery={trend24Q} savings={savings} lang={lang} />
      {/* 模型分布环形图 + Top 模型榜 */}
      <ModelPanels query={modelsQ} donutData={donutData} colors={donutColors}
        totalTokens={modelTotalTokens} lang={lang} rLabel={rLabel} personal={false} />

      {/* Top 渠道榜 + 用户消费榜 */}
      <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fit, minmax(min(400px, 100%), 1fr))', gap: 16, marginTop: 16 }}>
        <PanelWrap title={t('Top 渠道（{{range}}）', { range: t(rLabel) })} query={sourcesQ} emptyText={t('暂无渠道数据')}>
          {(data) => <TopSources data={data} lang={lang} />}
        </PanelWrap>
        <PanelWrap title={t('用户消费排行（Top 20）')} query={userRankQ} emptyText={t('暂无用户排行数据')}>
          {(data) => <UserRanking data={data} lang={lang} />}
        </PanelWrap>
      </div>

      <HeatmapPanel query={heatmapQ} lang={lang} />

      <PanelWrap title={t('渠道健康状态')} query={healthQ} style={{ marginTop: 16 }}
        emptyText={t('暂无渠道健康数据')}>
        {(data) => <ChannelHealth data={data} lang={lang} updatedAt={healthQ.dataUpdatedAt} />}
      </PanelWrap>

      {/* 快捷操作（管理员工具箱） */}
      <div className="card" style={{ marginTop: 16 }}>
        <div className="card-header">
          <h2>{t('快捷操作')}</h2>
        </div>
        <div className="card-body quick-actions">
          <Link to="/channels" className="btn btn-primary">{t('管理渠道')}</Link>
          <Link to="/model-manage" className="btn btn-outline">{t('模型管理')}</Link>
          <Link to="/keys" className="btn btn-outline">{t('管理 API 密钥')}</Link>
          <Link to="/users" className="btn btn-outline">{t('用户管理')}</Link>
          <Link to="/logs" className="btn btn-outline">{t('查看日志')}</Link>
          <Link to="/products?tab=redemptions" className="btn btn-outline">{t('兑换码管理')}</Link>
          <Link to="/settings" className="btn btn-outline">{t('调整限额')}</Link>
        </div>
      </div>
    </div>
  );
}
