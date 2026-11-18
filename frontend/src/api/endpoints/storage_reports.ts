/**
 * 域端点（design 1.4：从 `api/index.ts` 的 `api` 对象按域搬出，纯搬移）。
 *
 * 方法原样保留，由 `index.ts` 展开合并进同一个 `api` 对象 —— 调用方仍写
 * `api.<method>()`，导入路径不变。
 */

import type {
  ApiResponse,
  ApiList,
  LoginResult,
  LoginCodeResult,
  RegisterResult,
  MessageResult,
  TokenKeyResult,
  PriceEntry,
  GroupItem,
  OrderItem,
  PlanItem,
  RedemptionItem,
  IpFilterEntry,
  SettingsItem,
  UsageSummaryItem,
  TrendItem,
  DashboardItem,
} from '@/types';
import { API_BASE, buildQuery, reportWindowQs, request, ReportWindow } from '../client';

/** 注册选项（GET /api/auth/register/options 公共端点返回） */
export interface RegisterOptions {
  /** 注册开关：false 时前端显示「注册已关闭」并禁用提交 */
  register_enabled: boolean;
  /** 邮箱白名单开关：true 时邮箱拆为「本地部分 + 域名下拉」 */
  email_whitelist_enable: boolean;
  /** 允许的邮箱域名后缀（不含 @，已 trim/去重/排序） */
  email_whitelist_suffix: string[];
  /** 机器人校验（ALTCHA）开关 */
  captcha_required: boolean;
}

export const endpoints = {
  // ==================== 顶层便捷方法（页面直调，沿用后端路由契约） ====================
  login: (email: string, password: string, captcha?: string): Promise<ApiResponse<LoginResult>> =>
    request<ApiResponse<LoginResult>>('POST', `${API_BASE}/auth/login`, { email, password, captcha }, { skipAuthRedirect: true }),
  loginSendCode: (email: string): Promise<ApiResponse<LoginCodeResult>> =>
    request<ApiResponse<LoginCodeResult>>('POST', `${API_BASE}/auth/login/send-code`, { email }, { skipAuthRedirect: true }),
  loginWithCode: (email: string, code: string, captcha?: string): Promise<ApiResponse<LoginResult>> =>
    request<ApiResponse<LoginResult>>('POST', `${API_BASE}/auth/login/code`, { email, code, captcha }, { skipAuthRedirect: true }),
  loginTotp: (tmp_token: string, code: string): Promise<ApiResponse<LoginResult>> =>
    request<ApiResponse<LoginResult>>('POST', `${API_BASE}/auth/login/totp`, { tmp_token, code }, { skipAuthRedirect: true }),
  // 兼容历史调用顺序 (email, password, username?)：后端仅使用 email/password/username/aff_code 字段
  register: (emailOrUsername: string, password: string, username?: string, affCode?: string, captcha?: string): Promise<ApiResponse<RegisterResult>> =>
    request<ApiResponse<RegisterResult>>('POST', `${API_BASE}/auth/register`, {
      email: emailOrUsername,
      password,
      username: username ?? undefined,
      aff_code: affCode ?? undefined,
      captcha,
    }),
  // 注册页公共选项（GET /api/auth/register/options，无需鉴权）：注册开关 / 邮箱白名单后缀 / 机器人校验开关
  getRegisterOptions: (): Promise<ApiResponse<RegisterOptions>> =>
    request<ApiResponse<RegisterOptions>>('GET', `${API_BASE}/auth/register/options`, null, { skipAuthRedirect: true }),
  forgotPassword: (email: string): Promise<ApiResponse<MessageResult>> =>
    request<ApiResponse<MessageResult>>('POST', `${API_BASE}/auth/forgot-password`, { email }, { skipAuthRedirect: true }),
  changePassword: (old_password: string, new_password: string): Promise<ApiResponse<MessageResult>> =>
    request<ApiResponse<MessageResult>>('POST', `${API_BASE}/auth/change-password`, { old_password, new_password }, { skipAuthRedirect: true }),
  // OAuth 登录回跳：callback 只把一次性 ticket 放进 URL fragment，这里换回会话数据。
  // skipAuthRedirect：ticket 无效返回 401 属业务错误（登录页自己提示），不参与登出逻辑。
  oauthConsume: (ticket: string): Promise<ApiResponse<LoginResult>> =>
    request<ApiResponse<LoginResult>>('POST', `${API_BASE}/auth/oauth/consume`, { ticket }, { skipAuthRedirect: true }),
  // 个人中心发起第三方账号绑定（登录态）：返回上游授权 URL，前端整页跳转过去。
  oauthBindStart: (provider: string): Promise<ApiResponse<{ url: string }>> =>
    request<ApiResponse<{ url: string }>>('POST', `${API_BASE}/auth/${provider}/bind/start`, {}),
  oauthUnbind: (provider: string): Promise<ApiResponse<{ provider: string }>> =>
    request<ApiResponse<{ provider: string }>>('POST', `${API_BASE}/auth/oauth/unbind`, { provider }),
  getUsageSummary: (lightweight = false): Promise<ApiResponse<UsageSummaryItem>> =>
    request<ApiResponse<UsageSummaryItem>>('GET', `${API_BASE}/usage/summary${lightweight ? '?lightweight=true' : ''}`),
  getTodayTokens: (): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('GET', `${API_BASE}/tokens/today`),
  getLimits: (): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('GET', `${API_BASE}/limits`),
  getTrend: (): Promise<ApiResponse<TrendItem[]>> =>
    request<ApiResponse<TrendItem[]>>('GET', `${API_BASE}/usage/trend`),
  getConsumptionTrend: (q: ReportWindow = {}): Promise<ApiResponse<TrendItem[]>> =>
    request<ApiResponse<TrendItem[]>>('GET', `${API_BASE}/dashboard/consumption_trend?${reportWindowQs(q)}`),
  getModelDistribution: (q: ReportWindow = {}): Promise<ApiResponse<DashboardItem[]>> =>
    request<ApiResponse<DashboardItem[]>>('GET', `${API_BASE}/dashboard/model_distribution?${reportWindowQs(q)}`),
  getUserRanking: (q: ReportWindow = {}): Promise<ApiResponse<DashboardItem[]>> =>
    request<ApiResponse<DashboardItem[]>>('GET', `${API_BASE}/dashboard/user_ranking?${reportWindowQs(q)}`),
  getChannelHealth: (q: ReportWindow = {}): Promise<ApiResponse<DashboardItem[]>> =>
    request<ApiResponse<DashboardItem[]>>('GET', `${API_BASE}/dashboard/channel_health?${reportWindowQs(q)}`),
  getRealtime: (): Promise<ApiResponse<DashboardItem>> =>
    request<ApiResponse<DashboardItem>>('GET', `${API_BASE}/dashboard/realtime`),
  getCacheSavings: (q: ReportWindow = {}): Promise<ApiResponse<DashboardItem>> =>
    request<ApiResponse<DashboardItem>>('GET', `${API_BASE}/dashboard/cache_savings?${reportWindowQs(q)}`),
  // 报表扩展（Agent2API report 视角；普通用户后端按 user_id 自动收敛数据集）。
  // 窗口参数：days = 含今天的 N 个自然日；start/end（YYYY-MM-DD，含端点）
  // 合法时优先生效——与后端 resolve_window 同一契约（阶段 2 铺路）。
  getReportHeatmap: (q: { days?: number; start?: string; end?: string } = {}): Promise<ApiResponse<DashboardItem[]>> =>
    request<ApiResponse<DashboardItem[]>>('GET', `${API_BASE}/dashboard/report/heatmap?${reportWindowQs({ days: 365, ...q })}`),
  getReportTopSources: (q: ReportWindow = {}): Promise<ApiResponse<DashboardItem[]>> =>
    request<ApiResponse<DashboardItem[]>>('GET', `${API_BASE}/dashboard/report/top_sources?${reportWindowQs(q)}`),
  // 客户端维度 Top 榜（识别模块输出；普通用户后端按 user_id 自动收敛）
  getReportTopClients: (q: ReportWindow = {}): Promise<ApiResponse<DashboardItem[]>> =>
    request<ApiResponse<DashboardItem[]>>('GET', `${API_BASE}/dashboard/report/top_clients?${reportWindowQs(q)}`),
  getReportTokenTrend: (q: ReportWindow = {}): Promise<ApiResponse<DashboardItem[]>> =>
    request<ApiResponse<DashboardItem[]>>('GET', `${API_BASE}/dashboard/report/token_trend?${reportWindowQs(q)}`),
  getReportOverview: (q: ReportWindow = {}): Promise<ApiResponse<DashboardItem>> =>
    request<ApiResponse<DashboardItem>>('GET', `${API_BASE}/dashboard/report/overview?${reportWindowQs(q)}`),
  getReportModels: (q: ReportWindow = {}): Promise<ApiResponse<DashboardItem[]>> =>
    request<ApiResponse<DashboardItem[]>>('GET', `${API_BASE}/dashboard/report/models?${reportWindowQs(q)}`),
  getReportCacheRates: (q: ReportWindow = {}): Promise<ApiResponse<DashboardItem[]>> =>
    request<ApiResponse<DashboardItem[]>>('GET', `${API_BASE}/dashboard/report/cache_rates?${reportWindowQs(q)}`),
  getReportDailyTrend: (q: ReportWindow = {}): Promise<ApiResponse<DashboardItem[]>> =>
    request<ApiResponse<DashboardItem[]>>('GET', `${API_BASE}/dashboard/report/daily_trend?${reportWindowQs(q)}`),
  // 近 24 个本地整点缓存命中双轴序列（A2A cacheTrend24h；窗口固定不受 days 影响）
  getReportCacheTrend24h: (): Promise<ApiResponse<DashboardItem[]>> =>
    request<ApiResponse<DashboardItem[]>>('GET', `${API_BASE}/dashboard/report/cache_trend_24h`),
  // 日聚合手动重建（逃生门：聚合数字可疑时管理员一键重算历史）
  rebuildDailyAgg: (): Promise<ApiResponse<{ scanned: number; days_written: number; user_rows_written: number; skipped_shrink: number }>> =>
    request<ApiResponse<{ scanned: number; days_written: number; user_rows_written: number; skipped_shrink: number }>>('POST', `${API_BASE}/logs/agg/rebuild`, {}),
  saveEpayConfig: (config: Record<string, unknown>): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('PUT', `${API_BASE}/epay/config`, config),
  listGroups: (): Promise<ApiList<GroupItem>> =>
    request<ApiList<GroupItem>>('GET', `${API_BASE}/groups`),
  getIpLists: (): Promise<ApiResponse<IpFilterEntry[]>> =>
    request<ApiResponse<IpFilterEntry[]>>('GET', `${API_BASE}/ip/filter`),
  addIpWhitelist: (ip: string): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('POST', `${API_BASE}/ip/whitelist`, { pattern: ip }),
  addIpBlacklist: (ip: string): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('POST', `${API_BASE}/ip/blacklist`, { pattern: ip }),
  removeIpWhitelist: (pattern: string): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('DELETE', `${API_BASE}/ip/whitelist/${encodeURIComponent(pattern)}`),
  removeIpBlacklist: (pattern: string): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('DELETE', `${API_BASE}/ip/blacklist/${encodeURIComponent(pattern)}`),
  getRequestLogs: (): Promise<ApiResponse<DashboardItem[]>> =>
    request<ApiResponse<DashboardItem[]>>('GET', `${API_BASE}/logs/requests`),
  saveModelMapping: (mapping: unknown): Promise<ApiResponse<SettingsItem>> =>
    request<ApiResponse<SettingsItem>>('PUT', `${API_BASE}/settings`, { mappings: mapping, replace_all: false }),
  deleteModelMapping: (_id: string): Promise<ApiResponse<SettingsItem>> =>
    request<ApiResponse<SettingsItem>>('PUT', `${API_BASE}/settings`, { mappings: {}, replace_all: false }),
  listOrders: (params: Record<string, string | number> = {}): Promise<ApiList<OrderItem>> =>
    request<ApiList<OrderItem>>('GET', `${API_BASE}/orders${Object.keys(params).length ? `?${buildQuery(params)}` : ''}`),
  chatCompletions: (payload: Record<string, unknown>): Promise<ApiResponse<DashboardItem>> =>
    request<ApiResponse<DashboardItem>>('POST', '/v1/chat/completions', payload),
  listPrices: (): Promise<ApiList<PriceEntry>> =>
    request<ApiList<PriceEntry>>('GET', `${API_BASE}/prices`),
  listRedemptions: (params: Record<string, string | number> = {}): Promise<ApiList<RedemptionItem>> =>
    request<ApiList<RedemptionItem>>('GET', `${API_BASE}/redemptions?${buildQuery(params)}`),
  // 套餐管理（按量套餐：模板 CRUD + 按套餐发放 API Key）
  listPlans: (): Promise<ApiList<PlanItem>> =>
    request<ApiList<PlanItem>>('GET', `${API_BASE}/plans`),
  upsertPlan: (data: Record<string, unknown>): Promise<ApiResponse<PlanItem>> =>
    request<ApiResponse<PlanItem>>('POST', `${API_BASE}/plans`, data),
  deletePlan: (id: string | number): Promise<ApiResponse<MessageResult>> =>
    request<ApiResponse<MessageResult>>('DELETE', `${API_BASE}/plans/${id}`),
  issuePlanKey: (id: string | number, data: Record<string, unknown>): Promise<ApiResponse<TokenKeyResult>> =>
    request<ApiResponse<TokenKeyResult>>('POST', `${API_BASE}/plans/${id}/issue`, data),
};
