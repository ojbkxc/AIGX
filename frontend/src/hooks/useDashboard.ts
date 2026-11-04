// 仪表盘数据层（阶段 4）：TanStack Query 按面板独立取数。
//
// 替代旧 Dashboard.tsx 的「一次 allSettled 拉 15 个接口 + 位置化 pick(i)」：
// - 每面板一个 useQuery：单面板失败只有该面板显示重试，不再整页数字残缺；
// - placeholderData=keepPreviousData：切时间档时旧数据留在屏上不闪骨架；
// - 轮询策略显式下沉到本层（全局默认 refetchInterval=false，见 main.tsx）：
//   报表面板 60s、realtime 面板 30s，且 refetchIntervalInBackground: false
//   （页面隐藏自动暂停；在途请求跳过下一拍；数据没变不触发重渲染）。
import { keepPreviousData, useQuery, type UseQueryResult } from '@tanstack/react-query';
import { api, type ReportWindow } from '../api';

/** 后端统一 {success,data} 外壳；unwrap 拿 data（缺失回退整包，兼容个别端点裸返回） */
function unwrap<T>(res: unknown): T | null {
  const v = (res as { data?: unknown })?.data ?? res;
  if (v === undefined) return null;
  return v as T;
}

/** 窗口 → queryKey 片段（days 档与自定义区间分开缓存） */
export function windowKey(win: ReportWindow): string {
  return win.start && win.end ? `${win.start}~${win.end}` : `d${win.days ?? 30}`;
}

type PanelOpts = { staleSec?: number; pollMs?: number; enabled?: boolean };

/** 单面板查询：key=['dashboard', name, window]，统一 unwrap 与缓存策略 */
export function usePanel<T>(
  name: string,
  win: ReportWindow,
  fetcher: () => Promise<unknown>,
  opts: PanelOpts = {},
): UseQueryResult<T | null, Error> {
  return useQuery<T | null>({
    queryKey: ['dashboard', name, windowKey(win)],
    queryFn: async () => unwrap<T>(await fetcher()),
    placeholderData: keepPreviousData,
    staleTime: (opts.staleSec ?? 60) * 1000,
    // 显式轮询：默认报表面板 60s；realtime 面板传 pollMs: 30000 覆盖。
    // 全局默认已关轮询，这里的值就是唯一事实来源。
    refetchInterval: opts.pollMs ?? 60000,
    refetchIntervalInBackground: false,
    // 管理员专属端点在个人视图必须禁用，否则逐面板 403。
    enabled: opts.enabled ?? true,
  });
}

// ── 面板数据形状（与后端 handler 响应逐字段对齐） ──────────────

export interface OverviewData {
  total_requests?: number;
  success_requests?: number;
  success_rate?: number;
  input_tokens?: number;
  output_tokens?: number;
  total_cost?: number;
  active_days?: number;
  streak?: number;
  top_models?: Array<{ model?: string; count?: number }>;
}

export interface HeatmapCell {
  date?: string;
  cost?: number;
  count?: number;
  tokens?: number;
  models?: Array<{ model?: string; tokens?: number }>;
}

export interface TopSource {
  name?: string;
  count?: number;
  cost?: number;
  tokens?: number | null;
  success_rate?: number | null;
  avg_latency_ms?: number | null;
}

export interface TokenTrendPoint {
  date?: string;
  input_tokens?: number;
  output_tokens?: number;
}

export interface DailyTrendRow {
  date?: string;
  count?: number;
  input_tokens?: number;
  output_tokens?: number;
  cost?: number;
  models?: Array<{ model?: string; count?: number; tokens?: number }>;
}

export interface CacheRateWindowData {
  window?: string;
  requests?: number;
  hit_requests?: number;
  rate?: number;
  input_tokens?: number;
  hit_input_tokens?: number;
  sampled?: boolean;
}

export interface CacheTrendHour {
  hour?: string;
  requests?: number;
  hit_requests?: number;
  rate?: number;
  input_tokens?: number;
  hit_input_tokens?: number;
  total_tokens?: number;
  models?: Array<{ model?: string; tokens?: number }>;
  sampled?: boolean;
}

export interface CacheSavingsData {
  hit_count?: number;
  cache_tokens?: number;
  billed_cost?: number;
  saved_estimate?: number;
  approx?: boolean;
}

export interface ModelDistItem {
  model?: string;
  count?: number;
  tokens?: number;
}

export interface ReportModelRow {
  model?: string;
  count?: number;
  input_tokens?: number;
  output_tokens?: number;
  cost?: number;
}

export interface UserRankRow {
  user_id?: string | number;
  email?: string;
  username?: string;
  request_count?: number;
  requests?: number;
  count?: number;
  total_tokens?: number;
  tokens?: number;
  total_cost?: number;
  cost?: number;
}

export interface ChannelHealthRow {
  id?: string | number;
  name?: string;
  success_rate?: number | null;
  success_count?: number;
  total_requests?: number;
  requests?: number;
  error_rate?: number | null;
  circuit_breaker?: string;
  avg_latency_ms?: number;
  consecutive_empty?: number | null;
  health?: { error_rate?: number | null; last_error?: string } | null;
}

export interface RealtimeData {
  qps?: number;
  rps?: number;
  avg_latency_ms?: number;
  active_users?: number;
  active_channels?: number;
}

export interface UsageSummary {
  total_tokens?: number;
  total_input_tokens?: number;
  total_output_tokens?: number;
}

export interface TodayTokens {
  total_tokens?: number;
  input_tokens?: number;
  output_tokens?: number;
  request_count?: number;
}

// ── 面板 hooks ─────────────────────────────────────────────────
// 各 hook 都接受透传 opts（enabled=false 用于个人视图禁用 admin 专属端点）。

export const useOverview = (win: ReportWindow, opts: PanelOpts = {}) =>
  usePanel<OverviewData>('overview', win, () => api.getReportOverview(win), { staleSec: 100, ...opts });

export const useHeatmap = (win: ReportWindow, opts: PanelOpts = {}) =>
  usePanel<HeatmapCell[]>('heatmap', win, () => api.getReportHeatmap(win), { staleSec: 100, ...opts });

export const useTopSources = (win: ReportWindow, opts: PanelOpts = {}) =>
  usePanel<TopSource[]>('top_sources', win, () => api.getReportTopSources(win), { staleSec: 100, ...opts });

export const useTokenTrend = (win: ReportWindow, opts: PanelOpts = {}) =>
  usePanel<TokenTrendPoint[]>('token_trend', win, () => api.getReportTokenTrend(win), { staleSec: 100, ...opts });

export const useDailyTrend = (win: ReportWindow, opts: PanelOpts = {}) =>
  usePanel<DailyTrendRow[]>('daily_trend', win, () => api.getReportDailyTrend(win), { staleSec: 100, ...opts });

export const useCacheRates = (win: ReportWindow, opts: PanelOpts = {}) =>
  usePanel<CacheRateWindowData[]>('cache_rates', win, () => api.getReportCacheRates(win), { staleSec: 45, ...opts });

export const useCacheTrend24h = (opts: PanelOpts = {}) =>
  usePanel<CacheTrendHour[]>('cache_trend_24h', {}, () => api.getReportCacheTrend24h(), { staleSec: 45, ...opts });

export const useCacheSavings = (win: ReportWindow, opts: PanelOpts = {}) =>
  usePanel<CacheSavingsData>('cache_savings', win, () => api.getCacheSavings(win), { staleSec: 45, ...opts });

export const useConsumptionTrend = (win: ReportWindow, opts: PanelOpts = {}) =>
  usePanel<Array<{ date?: string; cost?: number; count?: number }>>('consumption', win, () => api.getConsumptionTrend(win), { staleSec: 100, ...opts });

export const useModelDist = (win: ReportWindow, opts: PanelOpts = {}) =>
  usePanel<ModelDistItem[]>('model_dist', win, () => api.getModelDistribution(win), { staleSec: 100, ...opts });

export const useReportModels = (win: ReportWindow, opts: PanelOpts = {}) =>
  usePanel<ReportModelRow[]>('report_models', win, () => api.getReportModels(win), { staleSec: 100, ...opts });

export const useUserRanking = (win: ReportWindow, opts: PanelOpts = {}) =>
  usePanel<UserRankRow[]>('user_ranking', win, () => api.getUserRanking(win), { staleSec: 100, ...opts });

export const useChannelHealth = (win: ReportWindow, opts: PanelOpts = {}) =>
  usePanel<ChannelHealthRow[]>('channel_health', win, () => api.getChannelHealth(win), { staleSec: 45, ...opts });

/** 实时流：唯一轮询面板（30s，页面隐藏自动暂停；数据没变不重渲染） */
export const useRealtime = (opts: PanelOpts = {}) =>
  usePanel<RealtimeData>('realtime', {}, () => api.getRealtime(), { staleSec: 25, pollMs: 30000, ...opts });

/** 今日 / 总量 / 配额（非窗口驱动，进页刷新即可） */
export const useTodayTokens = (opts: PanelOpts = {}) =>
  usePanel<TodayTokens>('today_tokens', {}, () => api.getTodayTokens(), { staleSec: 45, ...opts });

export const useUsageSummary = (opts: PanelOpts = {}) =>
  usePanel<UsageSummary>('usage_summary', {}, () => api.getUsageSummary(), { staleSec: 60, ...opts });

export const useLimits = (opts: PanelOpts = {}) =>
  usePanel<{ monthly_used?: number; monthly_limit?: number | null }>('limits', {}, () => api.getLimits(), { staleSec: 60, ...opts });

/** 个人配额（/users/me 全员可用）：used_quota 已用 / quota 总额（null=不限） */
export const usePersonalMe = (opts: PanelOpts = {}) =>
  usePanel<{ used_quota?: number; quota?: number | null }>('me', {}, () => api.getMe(), { staleSec: 60, ...opts });
