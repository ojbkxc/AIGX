/**
 * AIGX API客户端
 * 统一的API调用层，为前端应用提供类型安全的数据访问接口
 */

import type {
  AccountBalance,
  AccountBatchReceipt,
  ChatSessionSummary,
  UserToolCatalogItem,
  UserToolPolicy,
  AccountBatchRequest,
  AccountExportFile,
  AccountProviderItem,
  AccountSessionItem,
  ProxyPoolItem,
  ProxyPoolResult,
  ProxyTestResult,
  AccountSessionStatsItem,
  ApiResponse,
  User,
  ApiList,
  LoginResult,
  LoginCodeResult,
  RegisterResult,
  MessageResult,
  ChannelItem,
  TokenItem,
  TokenKeyResult,
  PriceEntry,
  GroupItem,
  OrderItem,
  PlanItem,
  RedemptionItem,
  TicketItem,
  UserSubscriptionItem,
  AlertRule,
  AlertEvent,
  NotifyConfigItem,
  IpFilterEntry,
  CacheStatsItem,
  SystemMonitorItem,
  DiagnosticsSummary,
  DiagnosticsChannelProbes,
  DiagnosticsBreakers,
  PluginsPayload,
  PricingSyncConfigItem,
  ExchangeRatesItem,
  IpFilterConfigItem,
  RateLimitConfigItem,
  SettingsItem,
  ModelInfo,
  InflightRequestItem,
  ModelMetaOverride,
  ManageView,
  ModelTestPayload,
  ModelTestResult,
  CatalogSyncResult,
  ImportEntry,
  ImportSources,
  ImportRunResult,
  CheckinCenterData,
  CheckinRecord,
  CheckinSchedule,
  CheckinSettings,
  CheckinTravelState,
  CheckinRunResult,
  TotpSetupResult,
  TotpEnableResult,
  TotpDisableResult,
  PlaygroundChatRequest,
  PlaygroundChatResult,
  PlaygroundChatData,
  PlaygroundImagesRequest,
  PlaygroundRawResult,
  ChatStreamCallback,
  UsageSummaryItem,
  TrendItem,
  DashboardItem,
  SchedulerStatusItem,
  PromptTranslateResult,
  PromptSource,
  PromptSourceItem,
  UpdateStatusItem,
  UpdateSettingsItem,
  AccountRotationPolicyItem,
  AgentSettingsItem,
  ChatSettingsItem,
  GatewaySettingsItem,
  SecurityPolicyItem,
  DebugTrafficView,
  ModelMetadataStatus,
  ModelMetadataRefreshResult,
  RoutingPolicyItem,
  RoutingQueueItem,
  ClineHeadersItem,
  GatewayPromptItem,
  SanitizeHeadersItem,
  ZcodeCaptchaConfig,
  ZcodeCaptchaPoolStats,
  ZcodeCaptchaPushResult,
  ZcodeCaptchaPoolSettings,
  CheckinBenefitsData,
  BenefitResult,
  KeepaliveConfig,
  StorageOverviewItem,
  BackupImportResult,
  ScheduledTaskList,
  ScheduledTaskItem,
  ScheduledTaskPatch,
  ScheduledTaskRunResult,
} from '@/types';
// 路由回放/粘滞会话类型独立成文件（types/index.ts 正被 #13 并行编辑，避免共享大文件互卷在制品）
import type { RouteReplaySnapshot, RouteStickyList } from '@/types/routing-replay';
// 路由多规则体系（#40）类型独立成文件（同上：不碰 types/index.ts）
import type {
  RoutingPreviewItem,
  RoutingPreviewRequest,
  RoutingProfiles,
  RoutingPolicyWithProfiles,
  RoutingQueueWithLayers,
} from '@/types/routing-profile';
// 桌面导入来源类型（#22；独立文件理由同上）
import type { DesktopSourceItem } from '@/types/desktop';
// 报表时区设置类型（#20；独立文件理由同上：types/index.ts 正被 #13 并行编辑）
import type { ReportSettingsItem, TzMigrateResult } from '@/types/report-tz';
// 工具调用 / usage 帧适配层（#63；回灌层未定，形状变更只改适配层）
import { extractToolCalls, extractUsage, type ToolCallWireMode } from '@/lib/toolCallAdapter';

const API_BASE = '/api';

function getToken(): string | null {
  return localStorage.getItem('token');
}

function authHeaders(): Record<string, string> {
  const token = getToken();
  if (!token) return {};
  return { Authorization: `Bearer ${token}` };
}

/** 把分页/筛选参数安全转成查询串（数字自动字符串化） */
function buildQuery(params: Record<string, string | number>): string {
  const entries = Object.entries(params)
    .filter(([, v]) => v !== undefined && v !== null && v !== '')
    .map(([k, v]) => [k, String(v)] as [string, string]);
  return new URLSearchParams(entries).toString();
}

/** 报表窗口参数（与后端 resolve_window 契约一致）：days 缺省 30；
 *  start/end 合法时后端优先生效（自定义区间档传这两个，不传 days） */
export type ReportWindow = { days?: number; start?: string; end?: string };

function reportWindowQs(q: ReportWindow): string {
  if (q.start && q.end) {
    return new URLSearchParams({ start: q.start, end: q.end }).toString();
  }
  return `days=${q.days ?? 30}`;
}

async function request<T = unknown>(
  method: string,
  path: string,
  body: unknown = null,
  opts: { skipAuthRedirect?: boolean; signal?: AbortSignal } = {},
): Promise<T> {
  const headers: Record<string, string> = {
    'Content-Type': 'application/json',
    ...authHeaders(),
  };
  const options: RequestInit = { method, headers, signal: opts.signal };
  if (body !== null) {
    options.body = JSON.stringify(body);
  }
  let res: Response;
  try {
    res = await fetch(path, options);
  } catch (networkErr) {
    // 请求层 L1：网络中断/DNS 失败 → 统一可读错误（不上抛 TypeError 到页面组件）
    if (networkErr instanceof DOMException && networkErr.name === 'AbortError') {
      throw networkErr;
    }
    throw new Error('网络连接失败，请检查网络后重试');
  }
  // 仅真实登录态失效才清除会话并跳转登录页；避免「已登录但访问
  // 无权限页面」时被误踢（后端对越权访问返回 403，而非 401）。
  //
  // `skipAuthRedirect`：登录 / 二次验证 / 改密这类**以 401 表达业务错误**
  // 的接口（如 TOTP 密码错误也返回 401）不参与登出逻辑 —— 否则用户在
  // 安全设置里输错一次密码就会被强制登出。这类调用方自己处理错误文案。
  if (res.status === 401 && !opts.skipAuthRedirect) {
    const token = getToken();
    if (token && !window.location.pathname.startsWith('/login')) {
      try {
        localStorage.removeItem('token');
        localStorage.removeItem('email');
        localStorage.removeItem('username');
        localStorage.removeItem('role');
        localStorage.removeItem('expires_at');
      } catch {
        // ignore
      }
      if (window.location.pathname !== '/login') {
        window.location.href = '/login';
      }
    }
    throw new Error('Unauthorized');
  }
  if (res.status === 204) {
    return null as T;
  }
  const text = await res.text();
  let data: unknown = null;
  if (text) {
    const contentType = res.headers.get('Content-Type') || '';
    if (contentType.includes('application/json')) {
      try {
        data = JSON.parse(text) as unknown;
      } catch {
        if (!res.ok) throw new Error(text || `Request failed with status ${res.status}`);
        data = text;
      }
    } else if (res.ok) {
      data = text;
    }
  }
  if (!res.ok) {
    const msg: unknown =
      (data && typeof data === 'object' && (((data as Record<string, unknown>).error as string) || ((data as Record<string, unknown>).message as string))) ||
      (typeof text === 'string' && text) ||
      `Request failed with status ${res.status}`;
    // 保留 HTTP 状态码：调用方（如翻译的 429 预算限流）可据此区分
    // 「可重试/限流」与「参数错/服务错」等不同失败语义。
    const err = new Error(typeof msg === 'string' ? msg : String(msg)) as Error & { status?: number };
    err.status = res.status;
    throw err;
  }
  return data as T;
}

export const api = {
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
  listKeys: (): Promise<ApiList<TokenItem>> =>
    request<ApiList<TokenItem>>('GET', `${API_BASE}/keys`),
  getRequestLogs: (): Promise<ApiResponse<DashboardItem[]>> =>
    request<ApiResponse<DashboardItem[]>>('GET', `${API_BASE}/logs/requests`),
  getModelMappings: (): Promise<ApiResponse<SettingsItem>> =>
    request<ApiResponse<SettingsItem>>('GET', `${API_BASE}/settings`),
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
  // ==================== 订阅（#85 套餐订阅化，对齐 new-api subscription） ====================
  /** 上架订阅套餐列表（用户侧购买页） */
  subscriptionPlans: (): Promise<ApiResponse<PlanItem[]>> =>
    request<ApiResponse<PlanItem[]>>('GET', `${API_BASE}/subscription/plans`),
  /** 当前用户全部订阅（含过期/取消） */
  subscriptionSelf: (): Promise<ApiResponse<UserSubscriptionItem[]>> =>
    request<ApiResponse<UserSubscriptionItem[]>>('GET', `${API_BASE}/subscription/self`),
  /** 余额购买订阅 */
  subscriptionBalancePay: (plan_id: string): Promise<ApiResponse<UserSubscriptionItem>> =>
    request<ApiResponse<UserSubscriptionItem>>('POST', `${API_BASE}/subscription/balance/pay`, { plan_id }),
  /** 管理员给用户绑定订阅 */
  adminBindSubscription: (user_id: string, plan_id: string): Promise<ApiResponse<UserSubscriptionItem>> =>
    request<ApiResponse<UserSubscriptionItem>>('POST', `${API_BASE}/subscription/admin/bind`, { user_id, plan_id }),
  /** 管理员查看指定用户订阅 */
  adminListUserSubscriptions: (user_id: string): Promise<ApiResponse<UserSubscriptionItem[]>> =>
    request<ApiResponse<UserSubscriptionItem[]>>('GET', `${API_BASE}/subscription/admin/users/${encodeURIComponent(user_id)}/subscriptions`),
  /** 管理员取消订阅并回退分组 */
  adminCancelSubscription: (id: string): Promise<ApiResponse<UserSubscriptionItem>> =>
    request<ApiResponse<UserSubscriptionItem>>('POST', `${API_BASE}/subscription/admin/subscriptions/${encodeURIComponent(id)}/cancel`),
  getSecurityIncidents: (): Promise<ApiList<AlertEvent>> =>
    request<ApiList<AlertEvent>>('GET', `${API_BASE}/monitor/security/events`),
  getSecurityAlerts: (): Promise<ApiList<AlertEvent>> =>
    request<ApiList<AlertEvent>>('GET', `${API_BASE}/alerts/active`),
  getBalance: (): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('GET', `${API_BASE}/users/me`),
  getTransactions: (): Promise<ApiList<OrderItem>> =>
    request<ApiList<OrderItem>>('GET', `${API_BASE}/orders/me`),

  // ==================== 迁移自 api.js 的顶层方法（页面直调契约） ====================

  // Auth
  logout: (): Promise<ApiResponse<MessageResult>> =>
    request<ApiResponse<MessageResult>>('POST', `${API_BASE}/auth/logout`),

  // Models（网关可用模型列表，用于对话调试模型下拉）
  // 通用网关语义：模型来自渠道声明的 models 聚合。登录用户均可访问
  // /api/models/available（对齐 new-api /api/user/models）；不用 /v1/models
  //（数据面需 sk-xxx，会 401 误踢登录），也不用 mappings keys。
  // P1：后端已附带元信息（owned_by/context_length/capabilities），兼容旧
  // 字符串数组与新对象数组两种形状。
  // 模型元信息覆盖（P1 收尾：owned_by/上下文长度/能力管理员覆盖）
  listModelMeta: (): Promise<ApiResponse<Record<string, ModelMetaOverride>>> =>
    request<ApiResponse<Record<string, ModelMetaOverride>>>('GET', `${API_BASE}/models/meta`),
  setModelMeta: (model: string, meta: ModelMetaOverride): Promise<ApiResponse<ModelMetaOverride>> =>
    request<ApiResponse<ModelMetaOverride>>('PUT', `${API_BASE}/models/meta/${encodeURIComponent(model)}`, meta),
  deleteModelMeta: (model: string): Promise<ApiResponse<null>> =>
    request<ApiResponse<null>>('DELETE', `${API_BASE}/models/meta/${encodeURIComponent(model)}`),

  // ── 统一模型管理（渠道模型 + 账号池模型合流，参照 Agent2API 模型管理） ──
  // 写接口统一回读最新完整视图，前端就地重绘、不必再拉一次。
  listManageModels: (): Promise<ApiResponse<ManageView>> =>
    request<ApiResponse<ManageView>>('GET', `${API_BASE}/models/manage`),
  setModelEnabled: (provider: string, id: string, enabled: boolean): Promise<ApiResponse<ManageView>> =>
    request<ApiResponse<ManageView>>('POST', `${API_BASE}/models/state`, { provider, id, enabled }),
  upsertModelPrice: (data: {
    model: string;
    input_price: number;
    output_price: number;
    cache_price?: number | null;
    price_type?: string;
    model_ratio?: number;
  }): Promise<ApiResponse<ManageView>> =>
    request<ApiResponse<ManageView>>('POST', `${API_BASE}/models/price`, data),
  removeModelPrice: (model: string): Promise<ApiResponse<ManageView>> =>
    request<ApiResponse<ManageView>>('POST', `${API_BASE}/models/price/remove`, { model }),
  setModelAlias: (
    alias: string,
    target: string,
    enabled?: boolean,
    reasoning_effort?: string
  ): Promise<ApiResponse<ManageView>> =>
    request<ApiResponse<ManageView>>('POST', `${API_BASE}/models/alias`, {
      alias,
      target,
      ...(enabled === undefined ? {} : { enabled }),
      ...(reasoning_effort === undefined ? {} : { reasoning_effort })
    }),
  setModelAliasState: (alias: string, enabled: boolean): Promise<ApiResponse<ManageView>> =>
    request<ApiResponse<ManageView>>('POST', `${API_BASE}/models/alias/state`, {
      alias,
      enabled
    }),
  removeModelAlias: (alias: string): Promise<ApiResponse<ManageView>> =>
    request<ApiResponse<ManageView>>('POST', `${API_BASE}/models/alias/remove`, { alias }),
  // 批量映射前缀（对齐 Agent2API 批量加映射：前缀 + 同名合并到多家；后端端点待实现，调用失败时页面报错兜底）
  batchAddAliasPrefix: (
    prefix: string,
    opts: { sep?: string; merge?: boolean; targets?: string[] }
  ): Promise<ManageView | null | undefined> =>
    request<{ data?: ManageView }>('POST', `${API_BASE}/models/alias/prefix`, {
      prefix,
      sep: opts.sep ?? '-',
      merge: opts.merge ?? true,
      targets: opts.targets ?? []
    }).then((res) => res?.data),
  // 批量设置映射级思考等级
  setAliasReasoning: (
    aliases: string[],
    effort: string
  ): Promise<ApiResponse<{ written: number; view: ManageView }>> =>
    request<ApiResponse<{ written: number; view: ManageView }>>('POST', `${API_BASE}/models/alias/reasoning`, {
      aliases,
      effort
    }),
  // 添加自定义模型登记
  addCustomModel: (
    provider: string,
    id: string
  ): Promise<ApiResponse<ManageView>> =>
    request<ApiResponse<ManageView>>('POST', `${API_BASE}/models/custom`, { provider, id }),
  // 删除自定义模型登记
  removeCustomModel: (
    provider: string,
    id: string
  ): Promise<ApiResponse<ManageView>> =>
    request<ApiResponse<ManageView>>('POST', `${API_BASE}/models/custom/remove`, { provider, id }),
  // 模型测试（管理员）：钉 provider+model 走真实链路，恒 200，结论在 data.ok
  testModel: (data: ModelTestPayload, signal?: AbortSignal): Promise<ApiResponse<ModelTestResult>> =>
    request<ApiResponse<ModelTestResult>>('POST', `${API_BASE}/models/test`, data, { signal }),

  // ── 签到中心（管理员替上游账号签到，区别于用户侧 /api/checkin） ──
  getCheckinCenter: (): Promise<ApiResponse<CheckinCenterData>> =>
    request<ApiResponse<CheckinCenterData>>('GET', `${API_BASE}/checkin-center`),
  runCheckin: (data: {
    account_ids?: string[];
    providers?: string[];
  }): Promise<ApiResponse<{ results: CheckinRunResult[]; done: number; total: number }>> =>
    request('POST', `${API_BASE}/checkin-center/run`, data),
  getCheckinRecords: (params: {
    days?: number;
    provider?: string;
    account_id?: string;
    outcome?: string;
    keyword?: string;
    offset?: number;
    limit?: number;
  }): Promise<ApiResponse<{ records: CheckinRecord[]; total: number }>> => {
    const query = new URLSearchParams();
    if (params.days != null) query.set('days', String(params.days));
    if (params.provider) query.set('provider', params.provider);
    if (params.account_id) query.set('account_id', params.account_id);
    if (params.outcome) query.set('outcome', params.outcome);
    if (params.keyword) query.set('keyword', params.keyword);
    if (params.offset != null) query.set('offset', String(params.offset));
    if (params.limit != null) query.set('limit', String(params.limit));
    const suffix = query.toString();
    return request('GET', `${API_BASE}/checkin-center/records${suffix ? `?${suffix}` : ''}`);
  },
  markKeptActive: (account_id: string, provider: string): Promise<ApiResponse<{ kept_active: boolean }>> =>
    request('POST', `${API_BASE}/checkin-center/records/mark-kept-active`, {
      account_id,
      provider,
    }),
  clearCheckinRecords: (days: number): Promise<ApiResponse<{ removed: number }>> =>
    request('DELETE', `${API_BASE}/checkin-center/records?days=${days}`),
  updateCheckinSettings: (settings: CheckinSettings): Promise<ApiResponse<CheckinSettings>> =>
    request('PUT', `${API_BASE}/checkin-center/settings`, settings),
  upsertCheckinSchedule: (schedule: {
    id: string;
    label: string;
    enabled: boolean;
    time: string;
    providers: string[];
    /** 逐家时刻覆盖（非空项才提交；空 = 回落内置/条目时刻） */
    provider_times?: Record<string, string>;
    /* ── be-fix #16-B 契约字段：后端未支持时不要发送（前端 feature-detect 门控）── */
    time_end?: string;
    retry_time?: string;
    retry_gap_minutes?: number;
    retry_limit?: number;
  }): Promise<ApiResponse<CheckinSchedule[]>> =>
    request('POST', `${API_BASE}/checkin-center/schedules`, schedule),
  removeCheckinSchedule: (id: string): Promise<ApiResponse<CheckinSchedule[]>> =>
    request('POST', `${API_BASE}/checkin-center/schedules/remove`, { id }),
  runCheckinSchedule: (id: string): Promise<ApiResponse<{ summary: string; ok: number; keepalive: number; failed: number }>> =>
    request('POST', `${API_BASE}/checkin-center/schedules/run`, { id }),
  // 旅行/成长巡检（对照上游 growth_scheduler；be-fix #16-B 契约，未就绪 404 → 降级）
  updateCheckinTravel: (body: { enabled?: boolean; times?: string[] }): Promise<ApiResponse<CheckinTravelState>> =>
    request('PATCH', `${API_BASE}/checkin-center/travel`, body),
  runCheckinTravelNow: (): Promise<ApiResponse<{ summary?: string; ok?: number; failed?: number }>> =>
    request('POST', `${API_BASE}/checkin-center/travel/run`, {}),

  // 一次性 / 限时福利通道（新手礼 / 保活 / 成长任务 / 限时套餐 / UMID）
  getCheckinBenefits: (): Promise<ApiResponse<CheckinBenefitsData>> =>
    request('GET', `${API_BASE}/checkin-center/benefits`),
  runCheckinBenefit: (
    account_id: string,
    action: string,
    params?: Record<string, unknown>,
  ): Promise<ApiResponse<BenefitResult>> =>
    request('POST', `${API_BASE}/checkin-center/benefits/run`, {
      account_id,
      action,
      params: params ?? {},
    }),
  getKeepaliveConfig: (): Promise<ApiResponse<KeepaliveConfig>> =>
    request('GET', `${API_BASE}/checkin-center/benefits/keepalive`),
  saveKeepaliveConfig: (models: string[]): Promise<ApiResponse<KeepaliveConfig>> =>
    request('PUT', `${API_BASE}/checkin-center/benefits/keepalive`, { models }),

  // 间隔型定时任务（对齐 Agent2API「定时任务」页）
  getScheduledTasks: (): Promise<ApiResponse<ScheduledTaskList>> =>
    request('GET', `${API_BASE}/scheduled-tasks`),
  patchScheduledTask: (
    id: string,
    patch: ScheduledTaskPatch,
  ): Promise<ApiResponse<ScheduledTaskItem>> =>
    request('PATCH', `${API_BASE}/scheduled-tasks/${id}`, patch),
  runScheduledTask: (id: string): Promise<ApiResponse<ScheduledTaskRunResult>> =>
    request('POST', `${API_BASE}/scheduled-tasks/${id}/run`, {}),

  listModels: async (): Promise<ApiResponse<ModelInfo[]>> => {
    const res = await request<ApiResponse<unknown[]>>('GET', `${API_BASE}/models/available`);
    const list: unknown[] = Array.isArray(res?.data) ? res.data : [];
    return {
      success: true,
      data: list.map((item) =>
        typeof item === 'string'
          ? { id: item, object: 'model', owned_by: 'aigx' }
          : item as ModelInfo,
      ),
    };
  },

  // Settings / Model Mappings
  getSettings: (): Promise<ApiResponse<SettingsItem>> =>
    request<ApiResponse<SettingsItem>>('GET', `${API_BASE}/settings`),
  updateSettings: (mappings: unknown, replace_all = false): Promise<ApiResponse<SettingsItem>> =>
    request<ApiResponse<SettingsItem>>('PUT', `${API_BASE}/settings`, { mappings, replace_all }),

  // Limits
  updateLimits: (data: Record<string, unknown>): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('PUT', `${API_BASE}/limits`, data),

  // 服务器自动更新（检查 / 应用 / 重启 / 回滚 / 设置）
  getUpdateStatus: (): Promise<ApiResponse<UpdateStatusItem>> =>
    request<ApiResponse<UpdateStatusItem>>('GET', `${API_BASE}/update/status`),
  checkUpdate: (): Promise<ApiResponse<UpdateStatusItem>> =>
    request<ApiResponse<UpdateStatusItem>>('POST', `${API_BASE}/update/check`),
  applyUpdate: (force = false): Promise<ApiResponse<UpdateStatusItem>> =>
    request<ApiResponse<UpdateStatusItem>>('POST', `${API_BASE}/update/apply`, { force }),
  restartService: (): Promise<ApiResponse<{ restarting: boolean; delaySecs: number; restartEpoch: number }>> =>
    request<ApiResponse<{ restarting: boolean; delaySecs: number; restartEpoch: number }>>('POST', `${API_BASE}/update/restart`),
  rollbackUpdate: (): Promise<ApiResponse<{ restored: boolean; needRestart: boolean }>> =>
    request<ApiResponse<{ restored: boolean; needRestart: boolean }>>('POST', `${API_BASE}/update/rollback`),
  getUpdateSettings: (): Promise<ApiResponse<UpdateSettingsItem>> =>
    request<ApiResponse<UpdateSettingsItem>>('GET', `${API_BASE}/update/settings`),
  saveUpdateSettings: (data: Record<string, unknown>): Promise<ApiResponse<UpdateSettingsItem>> =>
    request<ApiResponse<UpdateSettingsItem>>('PUT', `${API_BASE}/update/settings`, data),

  // Users
  listUsers: (params: Record<string, string | number> = {}): Promise<ApiList<User>> =>
    request<ApiList<User>>('GET', `${API_BASE}/users?${buildQuery(params)}`),
  createUser: (data: Record<string, unknown>): Promise<ApiResponse<User>> =>
    request<ApiResponse<User>>('POST', `${API_BASE}/users`, data),
  updateUser: (id: string | number, data: Record<string, unknown>): Promise<ApiResponse<User>> =>
    request<ApiResponse<User>>('PUT', `${API_BASE}/users/${id}`, data),
  deleteUser: (id: string | number): Promise<ApiResponse<MessageResult>> =>
    request<ApiResponse<MessageResult>>('DELETE', `${API_BASE}/users/${id}`),
  getMe: (): Promise<ApiResponse<User>> =>
    request<ApiResponse<User>>('GET', `${API_BASE}/users/me`),
  /** 用户自助更新资料（对齐 new-api PUT /api/user/self）：username/改密（需原密码） */
  updateSelf: (data: { username?: string; original_password?: string; password?: string }): Promise<ApiResponse<User>> =>
    request<ApiResponse<User>>('PUT', `${API_BASE}/users/self`, data),
  /** 管理端用户操作（对齐 new-api POST /api/user/manage）：enable/disable */
  manageUser: (id: string, action: 'enable' | 'disable'): Promise<ApiResponse<User>> =>
    request<ApiResponse<User>>('POST', `${API_BASE}/users/manage`, { id, action }),
  /** 管理员强制禁用用户 2FA（对齐 new-api DELETE /api/user/:id/2fa） */
  adminDisable2FA: (id: string): Promise<ApiResponse<null>> =>
    request<ApiResponse<null>>('DELETE', `${API_BASE}/users/${encodeURIComponent(id)}/2fa`),
  /** 获取当前用户邀请码（对齐 new-api GET /api/user/aff，data 为码字符串） */
  getAffCode: (): Promise<ApiResponse<string>> =>
    request<ApiResponse<string>>('GET', `${API_BASE}/aff`),
  /** 邀请奖励划转到可用配额（对齐 new-api POST /api/user/aff_transfer） */
  affTransfer: (): Promise<ApiResponse<{ transferred: number }>> =>
    request<ApiResponse<{ transferred: number }>>('POST', `${API_BASE}/aff_transfer`, {}),
  // ==================== 工单（对齐 v2board Ticket） ====================
  listTickets: (id = ''): Promise<ApiResponse<TicketItem[] | TicketItem>> =>
    request<ApiResponse<TicketItem[] | TicketItem>>('GET', `${API_BASE}/tickets${id ? `?id=${encodeURIComponent(id)}` : ''}`),
  createTicket: (data: { subject: string; level: number; message: string }): Promise<ApiResponse<TicketItem>> =>
    request<ApiResponse<TicketItem>>('POST', `${API_BASE}/tickets`, data),
  replyTicket: (id: string, message: string): Promise<ApiResponse<TicketItem>> =>
    request<ApiResponse<TicketItem>>('POST', `${API_BASE}/tickets/reply`, { id, message }),
  closeTicket: (id: string): Promise<ApiResponse<TicketItem>> =>
    request<ApiResponse<TicketItem>>('POST', `${API_BASE}/tickets/close`, { id }),
  /** 管理员工单列表/详情 */
  adminListTickets: (params: Record<string, string | number> = {}): Promise<ApiResponse<TicketItem[] | TicketItem>> =>
    request<ApiResponse<TicketItem[] | TicketItem>>('GET', `${API_BASE}/admin/tickets${Object.keys(params).length ? `?${buildQuery(params)}` : ''}`),
  adminReplyTicket: (id: string, message: string): Promise<ApiResponse<TicketItem>> =>
    request<ApiResponse<TicketItem>>('POST', `${API_BASE}/admin/tickets/reply`, { id, message }),
  adminCloseTicket: (id: string): Promise<ApiResponse<TicketItem>> =>
    request<ApiResponse<TicketItem>>('POST', `${API_BASE}/admin/tickets/close`, { id }),
  /** 签到状态与当月记录（对齐 new-api GET /api/user/checkin） */
  checkinStatus: (): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('GET', `${API_BASE}/checkin`),
  /** 执行签到（对齐 new-api POST /api/user/checkin） */
  doCheckin: (): Promise<ApiResponse<{ quota_awarded: number; checkin_date: string }>> =>
    request<ApiResponse<{ quota_awarded: number; checkin_date: string }>>('POST', `${API_BASE}/checkin`),
  /** 用户侧积分流水（签到获得 + API 消耗合并时间线） */
  userRecords: (params: {
    days?: number;
    offset?: number;
    limit?: number;
    type?: 'all' | 'earned' | 'consumed';
  }): Promise<ApiResponse<Record<string, unknown>>> => {
    const query = new URLSearchParams();
    if (params.days != null) query.set('days', String(params.days));
    if (params.offset != null) query.set('offset', String(params.offset));
    if (params.limit != null) query.set('limit', String(params.limit));
    if (params.type) query.set('type', params.type);
    const suffix = query.toString();
    return request<ApiResponse<Record<string, unknown>>>(
      'GET',
      `${API_BASE}/user/records${suffix ? `?${suffix}` : ''}`,
    );
  },
  checkUsername: (username: string): Promise<ApiResponse<{ available?: boolean }>> =>
    request<ApiResponse<{ available?: boolean }>>('GET', `${API_BASE}/users/check?username=${encodeURIComponent(username)}`),

  // Epay
  getEpayConfig: (): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('GET', `${API_BASE}/epay/config`),
  // 用户侧充值页信息（对齐 new-api /api/user/topup/info）
  getEpayInfo: (): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('GET', `${API_BASE}/epay/info`),
  updateEpayConfig: (data: Record<string, unknown>): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('PUT', `${API_BASE}/epay/config`, data),

  // Orders & Topup
  myOrders: (): Promise<ApiList<OrderItem>> =>
    request<ApiList<OrderItem>>('GET', `${API_BASE}/orders/me`),
  topup: (amount: number, payment_method: string): Promise<ApiResponse<OrderItem>> =>
    request<ApiResponse<OrderItem>>('POST', `${API_BASE}/topup`, { amount, payment_method }),
  /** 管理端手动补单（对齐 new-api AdminCompleteTopUp）：pending → paid 并入账 */
  completeOrder: (tradeNo: string): Promise<ApiResponse<OrderItem>> =>
    request<ApiResponse<OrderItem>>('POST', `${API_BASE}/orders/${encodeURIComponent(tradeNo)}/complete`),
  /** 充值试算（对齐 new-api RequestAmount）：amount → pay_money/quota */
  topupAmount: (amount: number): Promise<ApiResponse<{ amount: number; pay_money: number; quota: number }>> =>
    request<ApiResponse<{ amount: number; pay_money: number; quota: number }>>('POST', `${API_BASE}/topup/amount`, { amount }),

  // 通用渠道管理（page/page_size 缺省 = 后端全量返回，兼容旧行为）
  listChannels: (params: Record<string, string | number> = {}): Promise<ApiList<ChannelItem>> =>
    request<ApiList<ChannelItem>>('GET', `${API_BASE}/channels${Object.keys(params).length ? `?${buildQuery(params)}` : ''}`),
  addChannel: (data: Record<string, unknown>): Promise<ApiResponse<ChannelItem>> =>
    request<ApiResponse<ChannelItem>>('POST', `${API_BASE}/channels`, data),
  updateChannel: (id: string | number, data: Record<string, unknown>): Promise<ApiResponse<ChannelItem>> =>
    request<ApiResponse<ChannelItem>>('PUT', `${API_BASE}/channels/${id}`, data),
  patchChannel: (id: string | number, data: Record<string, unknown>): Promise<ApiResponse<ChannelItem>> =>
    request<ApiResponse<ChannelItem>>('PATCH', `${API_BASE}/channels/${id}`, data),
  deleteChannel: (id: string | number, password?: string): Promise<ApiResponse<MessageResult>> =>
    request<ApiResponse<MessageResult>>('DELETE', `${API_BASE}/channels/${id}`, password ? { password } : null),
  testChannel: (id: string | number): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('POST', `${API_BASE}/channels/${id}/test`),
  resetChannelCircuit: (id: string | number): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('POST', `${API_BASE}/channels/${id}/reset-circuit`),
  /** 拉取上游模型清单；removed_mappings = 本次同步顺带清理的失效映射条数（旧后端不带该字段） */
  fetchChannelModels: (data: Record<string, unknown>): Promise<ApiResponse<{ models?: string[]; removed_mappings?: number }>> =>
    request<ApiResponse<{ models?: string[]; removed_mappings?: number }>>('POST', `${API_BASE}/channels/fetch_models`, data),
  getChannelBalance: (id: string | number): Promise<ApiResponse<{ balance?: number; credit?: number; unit?: string; wallets?: Array<{ type?: string; displayName?: string; balance?: number; unit?: string }>; scheme?: string }>> =>
    request<ApiResponse<{ balance?: number; credit?: number; unit?: string; wallets?: Array<{ type?: string; displayName?: string; balance?: number; unit?: string }>; scheme?: string }>>('GET', `${API_BASE}/channels/${id}/balance`),
  // 账号型渠道账号池（渠道内账号级调度：优先级/冷却/并发/粘性）
  listAccountSessions: (params: Record<string, string | number> = {}): Promise<ApiList<AccountSessionItem>> =>
    request<ApiList<AccountSessionItem>>('GET', `${API_BASE}/account-sessions${Object.keys(params).length ? `?${buildQuery(params)}` : ''}`),
  /**
   * 连接数快照（GET /api/account-sessions/connections，页面 2 秒轮询）。
   * 该端点未上线 / 网络异常 / 401 时**静默返回 null**，页面降级用列表自带的
   * in_flight 字段展示，绝不崩溃；skipAuthRedirect 防止轮询把已登录用户误踢。
   */
  getAccountSessionConnections: async (): Promise<Record<string, number> | null> => {
    try {
      const json = await request<{ data?: { counts?: Record<string, unknown> } } | null>(
        'GET',
        `${API_BASE}/account-sessions/connections`,
        null,
        { skipAuthRedirect: true },
      );
      const counts = json?.data?.counts;
      if (!counts || typeof counts !== 'object') return null;
      const out: Record<string, number> = {};
      for (const [id, n] of Object.entries(counts)) {
        const num = Number(n);
        if (Number.isFinite(num) && num >= 0) out[id] = Math.floor(num);
      }
      return out;
    } catch {
      return null;
    }
  },
  createAccountSession: (data: Record<string, unknown>): Promise<ApiResponse<AccountSessionItem>> =>
    request<ApiResponse<AccountSessionItem>>('POST', `${API_BASE}/account-sessions`, data),
  updateAccountSession: (id: string, data: Record<string, unknown>): Promise<ApiResponse<AccountSessionItem>> =>
    request<ApiResponse<AccountSessionItem>>('PUT', `${API_BASE}/account-sessions/${encodeURIComponent(id)}`, data),
  // 部分更新（状态/优先级/权重/并发/名称），不会清空地址/模型/代理/凭证
  patchAccountSession: (id: string, data: Record<string, unknown>): Promise<ApiResponse<AccountSessionItem>> =>
    request<ApiResponse<AccountSessionItem>>('PATCH', `${API_BASE}/account-sessions/${encodeURIComponent(id)}`, data),
  // 桌面客户端登录态导入（#22 desktop 段；后端 desktop_import.rs——白名单
  // 只读 + 固定路径，请求体只有 source_id+name，无路径参数）
  getDesktopSources: (): Promise<ApiResponse<DesktopSourceItem[]>> =>
    request<ApiResponse<DesktopSourceItem[]>>('GET', `${API_BASE}/account-sessions/desktop/sources`),
  importDesktopAccount: (data: { source_id: string; name?: string }): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('POST', `${API_BASE}/account-sessions/desktop-import`, data),
  deleteAccountSession: (id: string): Promise<ApiResponse<MessageResult>> =>
    request<ApiResponse<MessageResult>>('DELETE', `${API_BASE}/account-sessions/${encodeURIComponent(id)}`),
  /**
   * 批量操作六类（POST /api/account-sessions/batch，#12-A1 端点）：
   * 单请求多账号 + 逐条回执。端点未上线（404/501）时抛错由页面捕获，
   * 降级回循环 PATCH/DELETE 路径（前端逐账号合成 payload，功能等价）。
   */
  batchAccountSessions: (data: AccountBatchRequest): Promise<ApiResponse<AccountBatchReceipt[]>> =>
    request<ApiResponse<AccountBatchReceipt[]>>('POST', `${API_BASE}/account-sessions/batch`, data),
  bulkImportAccountSessions: (data: Record<string, unknown>): Promise<ApiResponse<{ created?: number; skipped?: number }>> =>
    request<ApiResponse<{ created?: number; skipped?: number }>>('POST', `${API_BASE}/account-sessions/bulk`, data),
  testAccountSession: (id: string): Promise<ApiResponse<{ success?: boolean; latency_ms?: number; model?: string; message?: string }>> =>
    request<ApiResponse<{ success?: boolean; latency_ms?: number; model?: string; message?: string }>>('POST', `${API_BASE}/account-sessions/${encodeURIComponent(id)}/test`),
  /** 清冷却：传 model 只清该模型（`*` = 整号冷却），不传 = 全部清除 */
  resetAccountSessionCooldowns: (
    id: string,
    model?: string,
  ): Promise<ApiResponse<{ cleared?: number }>> =>
    request<ApiResponse<{ cleared?: number }>>(
      'POST',
      `${API_BASE}/account-sessions/${encodeURIComponent(id)}/reset-cooldown`,
      model ? { model } : undefined,
    ),
  /** 账号余额/额度（支持 12 家；不支持返回 400 错误信息） */
  getAccountSessionBalance: (id: string): Promise<ApiResponse<AccountBalance>> =>
    request<ApiResponse<AccountBalance>>('GET', `${API_BASE}/account-sessions/${encodeURIComponent(id)}/balance`),
  /** 手动刷新账号凭证（qoder/autoclaw/trae；force=true 无条件刷新） */
  refreshAccountSession: (id: string, force = false): Promise<ApiResponse<{ refreshed?: boolean; skipped?: boolean; message?: string }>> =>
    request<ApiResponse<{ refreshed?: boolean; skipped?: boolean; message?: string }>>('POST', `${API_BASE}/account-sessions/${encodeURIComponent(id)}/refresh`, { force }),
  /** 导出全部账号（含明文凭证，迁移/备份用，仅管理员） */
  exportAccountSessions: (): Promise<ApiResponse<AccountExportFile>> =>
    request<ApiResponse<AccountExportFile>>('GET', `${API_BASE}/account-sessions/export`),
  /** 导入导出文件中的账号（同渠道同名幂等跳过） */
  importAccountSessionsJson: (data: { channel_id?: string; entries: Array<Record<string, unknown>> }): Promise<ApiResponse<{ created?: number; skipped?: number; errors?: string[] }>> =>
    request<ApiResponse<{ created?: number; skipped?: number; errors?: string[] }>>('POST', `${API_BASE}/account-sessions/import/json`, data),
  accountProviders: (): Promise<ApiList<AccountProviderItem>> =>
    request<ApiList<AccountProviderItem>>('GET', `${API_BASE}/account-sessions/providers`),
  // 网络代理池（「网络代理」页：集中管理命名代理，账号 proxy 字段用 pool:/rotate 编码引用）
  listProxyPool: (): Promise<ApiResponse<ProxyPoolResult>> =>
    request<ApiResponse<ProxyPoolResult>>('GET', `${API_BASE}/proxy-pool`),
  createProxyEntry: (data: Record<string, unknown>): Promise<ApiResponse<ProxyPoolResult>> =>
    request<ApiResponse<ProxyPoolResult>>('POST', `${API_BASE}/proxy-pool`, data),
  updateProxyEntry: (data: Record<string, unknown>): Promise<ApiResponse<ProxyPoolResult>> =>
    request<ApiResponse<ProxyPoolResult>>('POST', `${API_BASE}/proxy-pool/update`, data),
  enableProxyEntry: (id: string, enabled: boolean): Promise<ApiResponse<ProxyPoolResult>> =>
    request<ApiResponse<ProxyPoolResult>>('POST', `${API_BASE}/proxy-pool/enable`, { id, enabled }),
  removeProxyEntry: (id: string): Promise<ApiResponse<ProxyPoolResult>> =>
    request<ApiResponse<ProxyPoolResult>>('POST', `${API_BASE}/proxy-pool/remove`, { id }),
  testProxyEntry: (id: string): Promise<ApiResponse<ProxyTestResult & { items?: ProxyPoolItem[] }>> =>
    request<ApiResponse<ProxyTestResult & { items?: ProxyPoolItem[] }>>('POST', `${API_BASE}/proxy-pool/test`, { id }),
  testAccountProxy: (data: { account_id?: string; proxy?: string | null }): Promise<ApiResponse<ProxyTestResult>> =>
    request<ApiResponse<ProxyTestResult>>('POST', `${API_BASE}/proxy-pool/test-account`, data),
  /** 上游模型目录发现：同步某账号（或某 provider 下全部账号）的模型清单 */
  syncAccountCatalog: (data: {
    account_id?: string;
    provider?: string;
    force?: boolean;
    apply?: boolean;
    replace?: boolean;
  }): Promise<ApiResponse<CatalogSyncResult>> =>
    request<ApiResponse<CatalogSyncResult>>('POST', `${API_BASE}/account-sessions/catalog/sync`, data),
  /** 扫描本机可导入的账号来源（cc-switch） */
  scanImportSources: (): Promise<ApiResponse<ImportSources>> =>
    request<ApiResponse<ImportSources>>('GET', `${API_BASE}/account-sessions/import/sources`),
  /** 执行导入：勾选的条目 → 渠道 */
  runImport: (entries: ImportEntry[]): Promise<ApiResponse<ImportRunResult>> =>
    request<ApiResponse<ImportRunResult>>('POST', `${API_BASE}/account-sessions/import`, { entries }),
  /** 支持模型目录发现的供应商清单 */
  catalogProviders: (): Promise<ApiResponse<{ providers?: Array<{ kind: string; label: string }> }>> =>
    request<ApiResponse<{ providers?: Array<{ kind: string; label: string }> }>>('GET', `${API_BASE}/account-sessions/catalog/providers`),
  /** 发起登录态供应商的网页登录（返回授权地址与任务 state） */
  startAccountLogin: (data: Record<string, unknown>): Promise<ApiResponse<{ state: string; authUrl: string; provider: string }>> =>
    request<ApiResponse<{ state: string; authUrl: string; provider: string }>>('POST', `${API_BASE}/account-sessions/login/start`, data),
  /** 轮询登录结果：pending / done+error / done+accountId */
  waitAccountLogin: (state: string): Promise<ApiResponse<{ pending?: boolean; done?: boolean; error?: string; accountId?: string }>> =>
    request<ApiResponse<{ pending?: boolean; done?: boolean; error?: string; accountId?: string }>>('GET', `${API_BASE}/account-sessions/login/wait?state=${encodeURIComponent(state)}`),
  /** 取消等待中的网页登录 */
  cancelAccountLogin: (state: string): Promise<ApiResponse<{ canceled?: boolean }>> =>
    request<ApiResponse<{ canceled?: boolean }>>('POST', `${API_BASE}/account-sessions/login/cancel`, { state }),
  /** 远程部署时手工提交浏览器地址栏里的回调地址 */
  submitAccountLoginCallback: (state: string, callbackUrl: string): Promise<ApiResponse<{ accountId?: string }>> =>
    request<ApiResponse<{ accountId?: string }>>('POST', `${API_BASE}/account-sessions/login/callback`, { state, callback_url: callbackUrl }),
  /** 粘贴式登录：网关归一化 + 校验一次后落账号（MonkeyCode / KukuAI / OrcaTerm / Command Code） */
  pasteAccountLogin: (data: Record<string, unknown>): Promise<ApiResponse<{ accountId?: string }>> =>
    request<ApiResponse<{ accountId?: string }>>('POST', `${API_BASE}/account-sessions/login/paste`, data),
  /** 发送手机验证码（返回 msgid，登录时带回） */
  sendAccountLoginSms: (data: Record<string, unknown>): Promise<ApiResponse<{ msgid?: string }>> =>
    request<ApiResponse<{ msgid?: string }>>('POST', `${API_BASE}/account-sessions/login/sms/send`, data),
  /** 手机验证码登录并落账号 */
  verifyAccountLoginSms: (data: Record<string, unknown>): Promise<ApiResponse<{ accountId?: string }>> =>
    request<ApiResponse<{ accountId?: string }>>('POST', `${API_BASE}/account-sessions/login/sms/verify`, data),
  accountSessionStats: (): Promise<ApiResponse<{ channels?: AccountSessionStatsItem[]; total?: number }>> =>
    request<ApiResponse<{ channels?: AccountSessionStatsItem[]; total?: number }>>('GET', `${API_BASE}/account-sessions/stats`),
  accountRotationPolicy: (): Promise<ApiResponse<AccountRotationPolicyItem>> =>
    request<ApiResponse<AccountRotationPolicyItem>>('GET', `${API_BASE}/account-sessions/rotation`),
  saveAccountRotationPolicy: (data: Partial<AccountRotationPolicyItem>): Promise<ApiResponse<AccountRotationPolicyItem>> =>
    request<ApiResponse<AccountRotationPolicyItem>>('PUT', `${API_BASE}/account-sessions/rotation`, data),
  getAgentSettings: (): Promise<ApiResponse<AgentSettingsItem>> =>
    request<ApiResponse<AgentSettingsItem>>('GET', `${API_BASE}/agent/settings`),
  saveAgentSettings: (data: Record<string, unknown>): Promise<ApiResponse<AgentSettingsItem>> =>
    request<ApiResponse<AgentSettingsItem>>('PUT', `${API_BASE}/agent/settings`, data),
  /** 聊天设置（/api/chat/settings，#10）：GET 响应 camelCase，PUT body snake_case（同 agent 面请求侧）；保存即热生效 */
  getChatSettings: (): Promise<ApiResponse<ChatSettingsItem>> =>
    request<ApiResponse<ChatSettingsItem>>('GET', `${API_BASE}/chat/settings`),
  saveChatSettings: (data: Record<string, unknown>): Promise<ApiResponse<ChatSettingsItem>> =>
    request<ApiResponse<ChatSettingsItem>>('PUT', `${API_BASE}/chat/settings`, data),
  /** 报表时区设置（/api/report/settings，#20 批3）：GET 响应 camelCase（六键），
   *  PUT body snake_case（同 chat/agent 面；也收 camelCase 别名），缺省字段保持原值，
   *  保存即热生效（后端写进程快照，无需重启）。越界 400。 */
  getReportSettings: (): Promise<ApiResponse<ReportSettingsItem>> =>
    request<ApiResponse<ReportSettingsItem>>('GET', `${API_BASE}/report/settings`),
  saveReportSettings: (data: Record<string, unknown>): Promise<ApiResponse<ReportSettingsItem>> =>
    request<ApiResponse<ReportSettingsItem>>('PUT', `${API_BASE}/report/settings`, data),
  /** 存量聚合行重算（逃生门，写操作，幂等）：POST /api/logs/agg/tz-migrate。
   *  offset_to 缺省＝当前生效口径（须与已保存一致，否则报错零写入）；offset_from 缺省 0。 */
  migrateAggTimezone: (data: Record<string, unknown>): Promise<TzMigrateResult> =>
    request<TzMigrateResult>('POST', `${API_BASE}/logs/agg/tz-migrate`, data),
  getGatewaySettings: (): Promise<ApiResponse<GatewaySettingsItem>> =>
    request<ApiResponse<GatewaySettingsItem>>('GET', `${API_BASE}/gateway/settings`),
  saveGatewaySettings: (data: Record<string, unknown>): Promise<ApiResponse<GatewaySettingsItem>> =>
    request<ApiResponse<GatewaySettingsItem>>('PUT', `${API_BASE}/gateway/settings`, data),
  // 安全策略（登录失败锁定 / 限流阈值；对照 Agent2API「设置 → 安全」）
  // 阈值与开关保存即生效，锁定时长改后需重启服务。
  getSecurityPolicy: (): Promise<ApiResponse<SecurityPolicyItem>> =>
    request<ApiResponse<SecurityPolicyItem>>('GET', `${API_BASE}/security/policy`),
  saveSecurityPolicy: (data: Record<string, unknown>): Promise<ApiResponse<SecurityPolicyItem>> =>
    request<ApiResponse<SecurityPolicyItem>>('PUT', `${API_BASE}/security/policy`, data),
  // 调试模式：上游原始报文（对照 Agent2API「设置 → 网关 → 调试模式」）。
  // 开关走 /api/limits 的 debug_traffic 字段；这里只做读取与清空。
  getDebugTraffic: (): Promise<ApiResponse<DebugTrafficView>> =>
    request<ApiResponse<DebugTrafficView>>('GET', `${API_BASE}/debug/traffic`),
  clearDebugTraffic: (): Promise<ApiResponse<{ cleared: number }>> =>
    request<ApiResponse<{ cleared: number }>>('DELETE', `${API_BASE}/debug/traffic`),
  // models.dev 模型元数据（对照 Agent2API `/api/models/metadata`）：开关 + 状态 + 手动刷新。
  getModelMetadata: (): Promise<ApiResponse<ModelMetadataStatus>> =>
    request<ApiResponse<ModelMetadataStatus>>('GET', `${API_BASE}/models/metadata`),
  saveModelMetadata: (enabled: boolean): Promise<ApiResponse<ModelMetadataStatus>> =>
    request<ApiResponse<ModelMetadataStatus>>('PUT', `${API_BASE}/models/metadata`, { enabled }),
  refreshModelMetadata: (): Promise<ApiResponse<ModelMetadataRefreshResult>> =>
    request<ApiResponse<ModelMetadataRefreshResult>>('POST', `${API_BASE}/models/metadata/refresh`),
  // 路由策略（对照 Agent2API「设置 → 路由」；保存即热生效，无需重启）
  getRoutingPolicy: (): Promise<ApiResponse<RoutingPolicyWithProfiles>> =>
    request<ApiResponse<RoutingPolicyWithProfiles>>('GET', `${API_BASE}/routing`),
  saveRoutingPolicy: (data: Record<string, unknown>): Promise<ApiResponse<RoutingPolicyItem>> =>
    request<ApiResponse<RoutingPolicyItem>>('PUT', `${API_BASE}/routing`, data),
  // #40 多规则总开关：PUT /api/routing 是读-改-写，只传 enabled 不会碰其他字段
  saveRoutingEnabled: (enabled: boolean): Promise<ApiResponse<RoutingPolicyItem>> =>
    request<ApiResponse<RoutingPolicyItem>>('PUT', `${API_BASE}/routing`, { enabled }),
  // #40 整份档案读写（契约 §5）。PUT 端点由后端 be-routing 提供；
  // 未就绪时返回 404（调用方按「端点未就绪」降级，不谎报保存成功）。
  saveRoutingProfiles: (profiles: RoutingProfiles): Promise<ApiResponse<RoutingProfiles>> =>
    request<ApiResponse<RoutingProfiles>>('PUT', `${API_BASE}/routing/profiles`, profiles),
  getRoutingDefaults: (): Promise<ApiResponse<RoutingPolicyItem>> =>
    request<ApiResponse<RoutingPolicyItem>>('GET', `${API_BASE}/routing/defaults`),
  // 路由候选队列预演（对照 Agent2API「设置 → 路由 → 队列」：只读，不推进轮询游标）
  getRoutingQueue: (model: string): Promise<ApiResponse<RoutingQueueItem & RoutingQueueWithLayers>> =>
    request<ApiResponse<RoutingQueueItem & RoutingQueueWithLayers>>(
      'GET',
      `${API_BASE}/routing/queue?model=${encodeURIComponent(model)}`,
    ),
  // #40 档案预演（契约 §5）：与真实转发走同一个 plan_route，返回命中层与逐字段来源。
  // 后端未就绪 → 404 / 405，调用方按「未就绪」降级，绝不在界面上自己算候选。
  previewRouting: (req: RoutingPreviewRequest): Promise<ApiResponse<RoutingPreviewItem>> =>
    request<ApiResponse<RoutingPreviewItem>>('POST', `${API_BASE}/routing/preview`, req),
  // 路由回放快照（对照 Agent2API「路由回放」：只读渲染请求当时保存的选路事实，
  // 不以当前配置重算历史）。后端 GET /api/routing/replay/{request_id} 由 be-fix
  // 任务 #12-B 提供；未就绪返回 404（err.status 可辨），调用方按空态降级。
  getRouteReplay: (requestId: string | number): Promise<ApiResponse<RouteReplaySnapshot | null>> =>
    request<ApiResponse<RouteReplaySnapshot | null>>(
      'GET',
      `${API_BASE}/routing/replay/${encodeURIComponent(String(requestId))}`,
    ),
  // 粘滞会话管理（对照 Agent2API api/routing_workspace.rs get_sticky/clear_sticky；
  // 后端 be-fix #38 已入库）。query 按会话标识/模型/渠道 id 模糊过滤；
  // 后端按 500 条快照截断（无分页参数），超出部分看 data.total / data.truncated。
  getRouteSticky: (query = ''): Promise<ApiResponse<RouteStickyList>> =>
    request<ApiResponse<RouteStickyList>>(
      'GET',
      `${API_BASE}/routing/sticky${query ? `?query=${encodeURIComponent(query)}` : ''}`,
    ),
  // 清除口径：id = 列表原样回传的内部编码（session\u{1f}model）；all: true 全清。
  // 二者至少给一个，否则后端 400。
  clearRouteSticky: (body: {
    id?: string;
    all?: boolean;
  }): Promise<ApiResponse<{ cleared: number }>> =>
    request<ApiResponse<{ cleared: number }>>('POST', `${API_BASE}/routing/sticky/clear`, body),
  // Cline 伪装头覆盖表（对照 Agent2API「设置 → 网关 → Cline 伪装头」；保存即生效）
  getClineHeaders: (): Promise<ApiResponse<ClineHeadersItem>> =>
    request<ApiResponse<ClineHeadersItem>>('GET', `${API_BASE}/cline-headers`),
  saveClineHeaders: (overrides: Record<string, string>): Promise<ApiResponse<ClineHeadersItem>> =>
    request<ApiResponse<ClineHeadersItem>>('PUT', `${API_BASE}/cline-headers`, { overrides }),
  // 系统提示词注入（对照 Agent2API「设置 → 网关 → 系统提示词」；热生效）
  getGatewayPrompt: (): Promise<ApiResponse<GatewayPromptItem>> =>
    request<ApiResponse<GatewayPromptItem>>('GET', `${API_BASE}/gateway/prompt`),
  saveGatewayPrompt: (data: Record<string, unknown>): Promise<ApiResponse<GatewayPromptItem>> =>
    request<ApiResponse<GatewayPromptItem>>('PUT', `${API_BASE}/gateway/prompt`, data),
  // 额外脱敏请求头名单（对照 Agent2API「设置 → 网关 → 指纹脱敏」；热生效）
  getSanitize: (): Promise<ApiResponse<SanitizeHeadersItem>> =>
    request<ApiResponse<SanitizeHeadersItem>>('GET', `${API_BASE}/sanitize`),
  saveSanitize: (data: Record<string, unknown>): Promise<ApiResponse<SanitizeHeadersItem>> =>
    request<ApiResponse<SanitizeHeadersItem>>('PUT', `${API_BASE}/sanitize`, data),
  // ZCode 验证码令牌池（对照 Agent2API「网关 → 验证码令牌池」）
  getZcodeCaptcha: (): Promise<ApiResponse<ZcodeCaptchaPoolStats>> =>
    request<ApiResponse<ZcodeCaptchaPoolStats>>('GET', `${API_BASE}/zcode/captcha`),
  pushZcodeCaptchaTokens: (
    tokens: { param: string; region?: string }[],
  ): Promise<ApiResponse<ZcodeCaptchaPushResult>> =>
    request<ApiResponse<ZcodeCaptchaPushResult>>('POST', `${API_BASE}/zcode/captcha`, {
      tokens,
    }),
  getZcodeCaptchaConfig: (
    accountId?: string,
  ): Promise<ApiResponse<{ account_id: string; captcha: ZcodeCaptchaConfig | null }>> =>
    request<ApiResponse<{ account_id: string; captcha: ZcodeCaptchaConfig | null }>>(
      'GET',
      `${API_BASE}/zcode/captcha/config${
        accountId ? `?account_id=${encodeURIComponent(accountId)}` : ''
      }`,
    ),
  getZcodeCaptchaPool: (): Promise<ApiResponse<ZcodeCaptchaPoolSettings>> =>
    request<ApiResponse<ZcodeCaptchaPoolSettings>>('GET', `${API_BASE}/zcode/captcha-pool`),
  saveZcodeCaptchaPool: (
    data: Partial<Pick<ZcodeCaptchaPoolSettings, 'target' | 'pollMs' | 'maxPerRound'>>,
  ): Promise<ApiResponse<ZcodeCaptchaPoolSettings>> =>
    request<ApiResponse<ZcodeCaptchaPoolSettings>>('PUT', `${API_BASE}/zcode/captcha-pool`, data),
  // 数据管理（对照 Agent2API「设置 → 数据」：存储概览 + 全量备份/恢复）
  getStorageOverview: (): Promise<ApiResponse<StorageOverviewItem>> =>
    request<ApiResponse<StorageOverviewItem>>('GET', `${API_BASE}/storage`),
  exportBackup: (includeLogs: boolean): Promise<unknown> =>
    request<unknown>('GET', `${API_BASE}/backup/export?include_logs=${includeLogs}`),
  importBackup: (doc: Record<string, unknown>): Promise<ApiResponse<BackupImportResult>> =>
    request<ApiResponse<BackupImportResult>>('POST', `${API_BASE}/backup/import`, doc),
  // 渠道对话调试：流式（text/event-stream）返回 { stream: [{ content }] }，
  // 非流式返回后端 JSON。与 Playground 页共用 SSE 解析模式。
  testChannelChat: async (data: PlaygroundChatRequest): Promise<PlaygroundChatResult> => {
    // 非流式走原路径；流式转发给 testChannelChatStream（真·增量渲染）
    if (data.stream) {
      let acc = '';
      await testChannelChatStream(data, (d) => {
        if (!d.isEnd && d.kind !== 'reasoning') acc += d.content;
      });
      return acc ? { stream: [{ content: acc }] } : { stream: [] };
    }
    const headers: Record<string, string> = {
      'Content-Type': 'application/json',
      ...authHeaders(),
    };
    const res = await fetch(`${API_BASE}/channels/chat_test`, {
      method: 'POST',
      headers,
      body: JSON.stringify(data),
    });
    if (res.status === 401) {
      const token = getToken();
      if (token && !window.location.pathname.startsWith('/login')) {
        try {
          localStorage.removeItem('token');
          localStorage.removeItem('email');
          localStorage.removeItem('username');
          localStorage.removeItem('expires_at');
        } catch {
          // ignore
        }
        if (window.location.pathname !== '/login') {
          window.location.href = '/login';
        }
      }
      throw new Error('Unauthorized');
    }
    const contentType = res.headers.get('Content-Type') || '';
    if (contentType.includes('text/event-stream')) {
      // 流式：把 SSE 逐条解析为 { content } 增量数组
      const text = await res.text();
      const chunks: { content: string }[] = [];
      let buf = '';
      const pushBuf = () => {
        // 单帧可能含多条 data: 行（ANSI 续行协议），把每条 data: 行独立解析
        const lines = buf
          .split('\n')
          .map((l) => l.trim())
          .filter((l) => l.startsWith('data:') && l.length > 5)
          .map((l) => l.slice(5).trim())
          .filter((l) => l.length > 0);
        for (const data of lines) {
          if (data === '[DONE]') continue;
          try {
            const parsed: Record<string, unknown> = JSON.parse(data) as Record<string, unknown>;
            const choices = parsed.choices as Array<{ delta?: { content?: string; text?: string; reasoning_content?: string; reasoning?: string } }> | undefined;
            const firstDelta = choices && choices[0] && choices[0].delta;
            const delta = parsed.delta as { text?: string } | undefined;
            const contentBlocks = parsed.content as Array<{ text?: string }> | undefined;
            const content =
              (firstDelta &&
                (firstDelta.content || firstDelta.text || firstDelta.reasoning_content || firstDelta.reasoning)) ||
              (delta && delta.text) ||
              (contentBlocks && contentBlocks[0] && contentBlocks[0].text) ||
              '';
            if (content) chunks.push({ content });
          } catch {
            // 忽略非 JSON 帧
          }
        }
        buf = '';
      };
      for (const line of text.split('\n')) {
        if (line.trim() === '') {
          pushBuf();
        } else {
          buf += line + '\n';
        }
      }
      pushBuf();
      return { stream: chunks };
    }
    // 非流式：解析 JSON，错误时抛出后端错误信息
    const text = await res.text();
    let parsed: unknown = null;
    if (text) {
      try {
        parsed = JSON.parse(text);
      } catch {
        // ignore
      }
    }
    if (!res.ok) {
      const errMsg: unknown =
        (parsed && typeof parsed === 'object' && (((parsed as Record<string, unknown>).error as string) || ((parsed as Record<string, unknown>).message as string))) ||
        text ||
        `Request failed with status ${res.status}`;
      throw new Error(typeof errMsg === 'string' ? errMsg : String(errMsg));
    }
    return parsed as PlaygroundChatResult;
  },

  // 令牌管理（all=true 仅管理员生效：默认所有人只看自己的，管理员显式请求查看全系统）
  listTokens: (all = false): Promise<ApiList<TokenItem>> =>
    request<ApiList<TokenItem>>('GET', `${API_BASE}/tokens${all ? '?all=1' : ''}`),
  addToken: (data: Record<string, unknown>): Promise<ApiResponse<TokenItem>> =>
    request<ApiResponse<TokenItem>>('POST', `${API_BASE}/tokens`, data),
  updateToken: (id: string | number, data: Record<string, unknown>): Promise<ApiResponse<TokenItem>> =>
    request<ApiResponse<TokenItem>>('PUT', `${API_BASE}/tokens/${id}`, data),
  deleteToken: (id: string | number): Promise<ApiResponse<MessageResult>> =>
    request<ApiResponse<MessageResult>>('DELETE', `${API_BASE}/tokens/${id}`),
  // 按需取回明文密钥：登录用户可随时查看/复制自己的令牌（对齐 new-api）
  getTokenKey: (id: string | number): Promise<ApiResponse<TokenKeyResult>> =>
    request<ApiResponse<TokenKeyResult>>('GET', `${API_BASE}/tokens/${id}/key`),
  resetTokenUsed: (id: string | number): Promise<ApiResponse<MessageResult>> =>
    request<ApiResponse<MessageResult>>('POST', `${API_BASE}/tokens/${id}/reset_used`),
  rotateToken: (id: string | number): Promise<ApiResponse<TokenKeyResult>> =>
    request<ApiResponse<TokenKeyResult>>('POST', `${API_BASE}/tokens/${id}/rotate`),

  // 模型定价目录
  upsertPrice: (data: Record<string, unknown>): Promise<ApiResponse<PriceEntry>> =>
    request<ApiResponse<PriceEntry>>('POST', `${API_BASE}/prices`, data),
  deletePrice: (model: string): Promise<ApiResponse<MessageResult>> =>
    request<ApiResponse<MessageResult>>('DELETE', `${API_BASE}/prices/${encodeURIComponent(model)}`),

  // 倍率配置
  getRatios: (): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('GET', `${API_BASE}/ratios`),
  updateRatios: (data: Record<string, unknown>): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('PUT', `${API_BASE}/ratios`, data),

  // 用户分组管理
  upsertGroup: (data: Record<string, unknown>): Promise<ApiResponse<GroupItem>> =>
    request<ApiResponse<GroupItem>>('POST', `${API_BASE}/groups`, data),
  deleteGroup: (name: string): Promise<ApiResponse<MessageResult>> =>
    request<ApiResponse<MessageResult>>('DELETE', `${API_BASE}/groups/${encodeURIComponent(name)}`),

  // 日志与审计
  listRequestLogs: (params: Record<string, string | number> = {}): Promise<ApiList<Record<string, unknown>>> =>
    request<ApiList<Record<string, unknown>>>('GET', `${API_BASE}/logs/requests?${buildQuery(params)}`),
  /** 在途请求（管理员）：正在跑、尚未落日志的请求 */
  listInflightRequests: (): Promise<ApiList<InflightRequestItem>> =>
    request<ApiList<InflightRequestItem>>('GET', `${API_BASE}/logs/inflight`),
  /** 终止一条在途请求 */
  terminateInflightRequest: (id: string): Promise<ApiResponse<null>> =>
    request<ApiResponse<null>>('POST', `${API_BASE}/logs/inflight/${encodeURIComponent(id)}/terminate`),
  /** 清空前的预览：会删多少条、时间范围 */
  clearPreview: (days = 0): Promise<ApiResponse<{
    days: number;
    total_all: number;
    would_remove: number;
    oldest?: number | null;
    newest?: number | null;
  }>> =>
    request('GET', `${API_BASE}/logs/requests/clear-preview?days=${days}`),
  listAuditLogs: (params: Record<string, string | number> = {}): Promise<ApiList<Record<string, unknown>>> =>
    request<ApiList<Record<string, unknown>>>('GET', `${API_BASE}/logs/audits?${buildQuery(params)}`),
  deleteRequestLogs: (ids: (string | number)[]): Promise<ApiResponse<{ removed?: number }>> =>
    request<ApiResponse<{ removed?: number }>>('DELETE', `${API_BASE}/logs/requests`, { ids }),
  clearRequestLogs: (): Promise<ApiResponse<{ removed?: number }>> =>
    request<ApiResponse<{ removed?: number }>>('DELETE', `${API_BASE}/logs/requests/clear`),
  deleteAuditLogs: (ids: (string | number)[]): Promise<ApiResponse<{ removed?: number }>> =>
    request<ApiResponse<{ removed?: number }>>('DELETE', `${API_BASE}/logs/audits`, { ids }),
  clearAuditLogs: (): Promise<ApiResponse<{ removed?: number }>> =>
    request<ApiResponse<{ removed?: number }>>('DELETE', `${API_BASE}/logs/audits/clear`),
  getLogRetention: (): Promise<ApiResponse<{ max_age_days?: number; max_capacity?: number; audit_max_age_days?: number; daily_max_age_days?: number }>> =>
    request<ApiResponse<{ max_age_days?: number; max_capacity?: number; audit_max_age_days?: number; daily_max_age_days?: number }>>('GET', `${API_BASE}/logs/retention`),
  updateLogRetention: (data: { max_age_days?: number; max_capacity?: number; audit_max_age_days?: number; daily_max_age_days?: number }): Promise<ApiResponse<{ daily_pruned?: number } | null>> =>
    request<ApiResponse<{ daily_pruned?: number } | null>>('PUT', `${API_BASE}/logs/retention`, data),
  cleanupLogs: (): Promise<ApiResponse<{ removed?: number; requests?: number; audits?: number }>> =>
    request<ApiResponse<{ removed?: number; requests?: number; audits?: number }>>('POST', `${API_BASE}/logs/cleanup`, {}),

  // 兑换码
  batchRedemptions: (data: Record<string, unknown>): Promise<ApiResponse<{ count?: number; data?: RedemptionItem[] }>> =>
    request<ApiResponse<{ count?: number; data?: RedemptionItem[] }>>('POST', `${API_BASE}/redemptions/batch`, data),
  deleteRedemption: (id: string | number): Promise<ApiResponse<MessageResult>> =>
    request<ApiResponse<MessageResult>>('DELETE', `${API_BASE}/redemptions/${id}`),
  redeem: (code: string): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('POST', `${API_BASE}/redemptions/redeem`, { code }),

  // 限流配置
  getRateLimitConfig: (): Promise<ApiResponse<RateLimitConfigItem>> =>
    request<ApiResponse<RateLimitConfigItem>>('GET', `${API_BASE}/ratelimit/config`),
  updateRateLimitConfig: (data: Record<string, unknown>): Promise<ApiResponse<RateLimitConfigItem>> =>
    request<ApiResponse<RateLimitConfigItem>>('PUT', `${API_BASE}/ratelimit/config`, data),

  // 通知系统（Telegram + SMTP + Slack + Webhook）
  getNotifyConfig: (): Promise<ApiResponse<NotifyConfigItem>> =>
    request<ApiResponse<NotifyConfigItem>>('GET', `${API_BASE}/notify/config`),
  updateNotifyConfig: (data: Record<string, unknown>): Promise<ApiResponse<NotifyConfigItem>> =>
    request<ApiResponse<NotifyConfigItem>>('PUT', `${API_BASE}/notify/config`, data),
  testTelegram: (): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('POST', `${API_BASE}/notify/test-telegram`),
  testEmail: (to: string): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('POST', `${API_BASE}/notify/test-email`, { to }),
  testSlack: (): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('POST', `${API_BASE}/notify/test-slack`),
  testWebhook: (message: string): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('POST', `${API_BASE}/notify/test-webhook`, { message }),

  // 告警规则
  getAlertRules: (): Promise<ApiList<AlertRule>> =>
    request<ApiList<AlertRule>>('GET', `${API_BASE}/alerts/rules`),
  updateAlertRules: (rules: AlertRule[]): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('PUT', `${API_BASE}/alerts/rules`, { rules }),
  getActiveAlerts: (): Promise<ApiList<AlertEvent>> =>
    request<ApiList<AlertEvent>>('GET', `${API_BASE}/alerts/active`),
  /** 标记活跃告警已处理（移入历史） */
  dismissAlert: (id: string | number): Promise<ApiResponse<{ resolved: string; message: string }>> =>
    request<ApiResponse<{ resolved: string; message: string }>>('POST', `${API_BASE}/alerts/resolve`, { id }),
  getAlertHistory: (limit = 100): Promise<ApiList<AlertEvent>> =>
    request<ApiList<AlertEvent>>('GET', `${API_BASE}/alerts/history?limit=${limit}`),
  testAlert: (kind: string, value: unknown): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('POST', `${API_BASE}/alerts/test`, { kind, value }),

  // 诊断（管理员）：系统体检摘要 / 渠道批量测活 / 熔断器快照
  getDiagnosticsSummary: (): Promise<ApiResponse<DiagnosticsSummary>> =>
    request<ApiResponse<DiagnosticsSummary>>('GET', `${API_BASE}/diagnostics/summary`),
  getDiagnosticsChannels: (): Promise<ApiResponse<DiagnosticsChannelProbes>> =>
    request<ApiResponse<DiagnosticsChannelProbes>>('GET', `${API_BASE}/diagnostics/channels`),
  getDiagnosticsBreakers: (): Promise<ApiResponse<DiagnosticsBreakers>> =>
    request<ApiResponse<DiagnosticsBreakers>>('GET', `${API_BASE}/diagnostics/breakers`),

  // ── 插件管理（管理员；后端契约对齐 Agent2API api/plugins.rs，AIGX 后端建设中）──
  // 端点未上线（404）时页面整页降级「后端未就绪」空态，这里不吞错。
  listPlugins: (): Promise<ApiResponse<PluginsPayload>> =>
    request<ApiResponse<PluginsPayload>>('GET', `${API_BASE}/plugins`),
  /** 启停整个插件包 */
  setPluginEnabled: (id: string, enabled: boolean): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('POST', `${API_BASE}/plugins/enable`, { id, enabled }),
  /** 启停包内单家提供商 */
  setPluginProviderEnabled: (id: string, provider: string, enabled: boolean): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('POST', `${API_BASE}/plugins/provider`, { id, provider, enabled }),
  /** 卸载插件包（账号数据保留，重装可续用） */
  uninstallPlugin: (id: string): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('POST', `${API_BASE}/plugins/uninstall`, { id }),
  /** 检查插件更新（返回 updates 快照） */
  checkPluginUpdates: (): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('GET', `${API_BASE}/plugins/update`),
  /** 刷新发布目录（返回 catalog 快照） */
  refreshPluginRelease: (): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('GET', `${API_BASE}/plugins/release`),
  /** 在途操作进度（安装/更新任务的阶段与字节读数） */
  getPluginProgress: (): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('GET', `${API_BASE}/plugins/progress`),

  // 系统监控
  getSystemMonitor: (): Promise<ApiResponse<SystemMonitorItem>> =>
    request<ApiResponse<SystemMonitorItem>>('GET', `${API_BASE}/monitor/system`),

  // Playground
  playgroundChat: (data: PlaygroundChatRequest): Promise<ApiResponse<PlaygroundChatData>> =>
    request<ApiResponse<PlaygroundChatData>>('POST', `${API_BASE}/playground/chat`, data),
  playgroundImages: (data: PlaygroundImagesRequest): Promise<ApiResponse<PlaygroundRawResult>> =>
    request<ApiResponse<PlaygroundRawResult>>('POST', `${API_BASE}/playground/images`, data),
  /** Playground TTS：文本转语音，返回 base64 音频 */
  playgroundTts: (data: { model: string; input: string; voice?: string }): Promise<ApiResponse<{ audio_base64: string; content_type: string }>> =>
    request<ApiResponse<{ audio_base64: string; content_type: string }>>('POST', `${API_BASE}/playground/tts`, data),
  /** Playground 语音转文字：multipart 上传音频 blob，返回 { text } */
  playgroundTranscribe: async (blob: Blob, model: string): Promise<{ text?: string }> => {
    const form = new FormData();
    form.append('file', blob, 'audio.webm');
    form.append('model', model);
    const res = await fetch(`${API_BASE}/playground/transcriptions`, {
      method: 'POST',
      headers: authHeaders(),
      body: form,
    });
    if (res.status === 401) {
      throw new Error('登录已过期，请重新登录');
    }
    const j = (await res.json()) as { success?: boolean; data?: { text?: string }; message?: string; error?: string };
    if (!res.ok || !j.success) {
      throw new Error(j.message || j.error || `转写失败 (HTTP ${res.status})`);
    }
    return j.data ?? {};
  },

  // 安全监控
  getSecurityOverview: (): Promise<ApiResponse<DashboardItem>> =>
    request<ApiResponse<DashboardItem>>('GET', `${API_BASE}/monitor/security`),
  getSecurityEvents: (params: Record<string, string | number> = {}): Promise<ApiList<AlertEvent>> =>
    request<ApiList<AlertEvent>>('GET', `${API_BASE}/monitor/security/events?${buildQuery(params)}`),

  // IP 管理
  getIpFilter: (): Promise<ApiResponse<IpFilterConfigItem>> =>
    request<ApiResponse<IpFilterConfigItem>>('GET', `${API_BASE}/ip/filter`),
  updateIpFilter: (data: Record<string, unknown>): Promise<ApiResponse<IpFilterConfigItem>> =>
    request<ApiResponse<IpFilterConfigItem>>('PUT', `${API_BASE}/ip/filter`, data),
  addWhitelist: (pattern: string, note: string): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('POST', `${API_BASE}/ip/whitelist`, { pattern, note }),
  removeWhitelist: (pattern: string): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('DELETE', `${API_BASE}/ip/whitelist/${encodeURIComponent(pattern)}`),
  addBlacklist: (pattern: string, note: string): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('POST', `${API_BASE}/ip/blacklist`, { pattern, note }),
  removeBlacklist: (pattern: string): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('DELETE', `${API_BASE}/ip/blacklist/${encodeURIComponent(pattern)}`),

  // 缓存管理
  getCacheStats: (): Promise<ApiResponse<CacheStatsItem>> =>
    request<ApiResponse<CacheStatsItem>>('GET', `${API_BASE}/cache/stats`),
  clearCache: (): Promise<ApiResponse<MessageResult>> =>
    request<ApiResponse<MessageResult>>('POST', `${API_BASE}/cache/clear`),

  // 忘记密码/重置密码
  resetPassword: (token: string, password: string): Promise<ApiResponse<MessageResult>> =>
    request<ApiResponse<MessageResult>>('POST', `${API_BASE}/auth/reset-password`, { token, password }, { skipAuthRedirect: true }),

  // 2FA/TOTP（P1）：用户侧自助管理
  totpSetup: (): Promise<ApiResponse<TotpSetupResult>> =>
    request<ApiResponse<TotpSetupResult>>('POST', `${API_BASE}/auth/totp/setup`),
  totpEnable: (code: string): Promise<ApiResponse<TotpEnableResult>> =>
    request<ApiResponse<TotpEnableResult>>('POST', `${API_BASE}/auth/totp/enable`, { code }, { skipAuthRedirect: true }),
  totpDisable: (password: string): Promise<ApiResponse<TotpDisableResult>> =>
    request<ApiResponse<TotpDisableResult>>('POST', `${API_BASE}/auth/totp/disable`, { password }, { skipAuthRedirect: true }),

  // 价格同步
  getPriceSyncConfig: (): Promise<ApiResponse<PricingSyncConfigItem>> =>
    request<ApiResponse<PricingSyncConfigItem>>('GET', `${API_BASE}/pricing/sync-config`),
  updatePriceSyncConfig: (data: Record<string, unknown>): Promise<ApiResponse<PricingSyncConfigItem>> =>
    request<ApiResponse<PricingSyncConfigItem>>('PUT', `${API_BASE}/pricing/sync-config`, data),
  triggerPriceSync: (): Promise<ApiResponse<MessageResult>> =>
    request<ApiResponse<MessageResult>>('POST', `${API_BASE}/pricing/sync`),

  // 汇率配置
  getExchangeRates: (): Promise<ApiResponse<ExchangeRatesItem>> =>
    request<ApiResponse<ExchangeRatesItem>>('GET', `${API_BASE}/pricing/exchange-rates`),
  updateExchangeRates: (data: Record<string, unknown>): Promise<ApiResponse<ExchangeRatesItem>> =>
    request<ApiResponse<ExchangeRatesItem>>('PUT', `${API_BASE}/pricing/exchange-rates`, data),

  // ==================== AI 运维 Agent（管理员专属 /admin/agent） ====================
  listAgentSessions: (): Promise<ApiResponse<unknown[]>> =>
    request<ApiResponse<unknown[]>>('GET', `${API_BASE}/agent/sessions`),
  createAgentSession: (title: string, role?: 'observer' | 'operator'): Promise<ApiResponse<{ id: string; title: string }>> =>
    request<ApiResponse<{ id: string; title: string }>>('POST', `${API_BASE}/agent/sessions`, { title, role }),
  getAgentSession: (id: string): Promise<ApiResponse<unknown>> =>
    request<ApiResponse<unknown>>('GET', `${API_BASE}/agent/sessions/${encodeURIComponent(id)}`),
  deleteAgentSession: (id: string): Promise<ApiResponse<null>> =>
    request<ApiResponse<null>>('DELETE', `${API_BASE}/agent/sessions/${encodeURIComponent(id)}`),
  agentApprove: (requestId: string, action: 'allow' | 'deny' | 'remember'): Promise<ApiResponse<null>> =>
    request<ApiResponse<null>>('POST', `${API_BASE}/agent/approvals/${encodeURIComponent(requestId)}`, { action }),
  getAgentConfig: (): Promise<ApiResponse<{
    model?: string;
    channel?: string;
    /** 空 model 时后端自动发现的实际生效模型 */
    effectiveModel?: string | null;
    /** 工具清单（name/label/risk），审批卡中文名与设置面板共用 */
    tools?: Array<{ name: string; label: string; risk: string }>;
  }>> =>
    request<ApiResponse<{ model?: string; channel?: string; effectiveModel?: string | null; tools?: Array<{ name: string; label: string; risk: string }> }>>('GET', `${API_BASE}/agent/config`),
  // ==================== 聊天会话（按用户隔离的对话记录） ====================
  listChatSessions: (): Promise<ApiResponse<Array<ChatSessionSummary>>> =>
    request<ApiResponse<Array<ChatSessionSummary>>>('GET', `${API_BASE}/chat/sessions`),
  createChatSession: (data: { title?: string; model?: string; updated_at?: number } = {}): Promise<ApiResponse<{ id: string; title: string; model: string }>> =>
    request<ApiResponse<{ id: string; title: string; model: string }>>('POST', `${API_BASE}/chat/sessions`, data),
  getChatSession: (id: string): Promise<ApiResponse<{ session: { id: string; title: string; model: string }; messages: Array<{ role: string; content: string; extra?: unknown }> }>> =>
    request<ApiResponse<{ session: { id: string; title: string; model: string }; messages: Array<{ role: string; content: string; extra?: unknown }> }>>('GET', `${API_BASE}/chat/sessions/${encodeURIComponent(id)}`),
  deleteChatSession: (id: string): Promise<ApiResponse<null>> =>
    request<ApiResponse<null>>('DELETE', `${API_BASE}/chat/sessions/${encodeURIComponent(id)}`),
  renameChatSession: (id: string, title: string): Promise<ApiResponse<null>> =>
    request<ApiResponse<null>>('POST', `${API_BASE}/chat/sessions/${encodeURIComponent(id)}/rename`, { title }),
  /**
   * 会话 fork（#37 后端端点；#58 前端接线）。
   * 从 `message_seq`（0-based，**含该条**，与 GET 详情的消息下标同序）分叉；
   * 后端有界复制（200 条 / 128KiB，超限保尾部），`dropped` 如实返回不静默。
   * 响应 `session` 带 parent_session_id / title_source（父子追溯）。
   */
  forkChatSession: (id: string, messageSeq: number): Promise<ApiResponse<{ session: ChatSessionSummary; copied: number; dropped: number }>> =>
    request<ApiResponse<{ session: ChatSessionSummary; copied: number; dropped: number }>>('POST', `${API_BASE}/chat/sessions/${encodeURIComponent(id)}/fork`, { message_seq: messageSeq }),
  /** 整表替换消息（编辑/重发后 debounce 保存） */
  replaceChatMessages: (id: string, messages: Array<{ role: string; content: string; extra?: unknown }>, touch = true): Promise<ApiResponse<{ count: number }>> =>
    request<ApiResponse<{ count: number }>>('PUT', `${API_BASE}/chat/sessions/${encodeURIComponent(id)}/messages`, { messages, touch }),
  /** 追加一条消息（导入旧 localStorage 会话用） */
  appendChatMessage: (id: string, message: { role: string; content: string; extra?: unknown }): Promise<ApiResponse<{ seq: number }>> =>
    request<ApiResponse<{ seq: number }>>('POST', `${API_BASE}/chat/sessions/${encodeURIComponent(id)}/messages`, message),
  /**
   * 终端用户工具策略（#65；设计稿 §5.1 三端点，后端未落地前 404 → 页面降级）。
   * PUT 超限（组数/工具数 caps）返回 409 + 原因文案。
   */
  getUserToolPolicy: (): Promise<ApiResponse<UserToolPolicy>> =>
    request<ApiResponse<UserToolPolicy>>('GET', `${API_BASE}/user-tools/policy`),
  saveUserToolPolicy: (policy: UserToolPolicy): Promise<ApiResponse<UserToolPolicy>> =>
    request<ApiResponse<UserToolPolicy>>('PUT', `${API_BASE}/user-tools/policy`, policy),
  getUserToolCatalog: (): Promise<ApiResponse<{ tools: UserToolCatalogItem[] }>> =>
    request<ApiResponse<{ tools: UserToolCatalogItem[] }>>('GET', `${API_BASE}/user-tools/catalog`),
    // ==================== 公开提示词源（Prompts 页「拉取公开源」） ====================
  listPromptSources: (): Promise<ApiResponse<PromptSource[]>> =>
    request<ApiResponse<PromptSource[]>>('GET', `${API_BASE}/prompts/sources`),
  /** 抓取公开源：成功时 data 为条目数组；truncated 表示因体积闸丢弃过尾部条目 */
  fetchPromptSource: (source: string): Promise<ApiResponse<PromptSourceItem[]>> =>
    request<ApiResponse<PromptSourceItem[]>>('POST', `${API_BASE}/prompts/fetch`, { source }),
  /** Agent 对话：SSE 流式，逐事件回调（AgentEvent） */
  agentChatStream: async (
    id: string,
    message: string,
    onEvent: (ev: { type: string; content?: string; name?: string; ok?: boolean; message?: string }) => void,
    signal?: AbortSignal,
    model?: string,
    reasoningEffort?: string,
    role?: 'observer' | 'operator',
  ): Promise<void> => {
    const body: { message: string; model?: string; reasoning_effort?: string; role?: string } = { message };
    if (model) body.model = model;
    if (reasoningEffort) body.reasoning_effort = reasoningEffort;
    if (role) body.role = role;
    const res = await fetch(`${API_BASE}/agent/sessions/${encodeURIComponent(id)}/chat`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', ...authHeaders() },
      body: JSON.stringify(body),
      signal,
    });
    if (!res.ok || !res.body) {
      const text = await res.text().catch(() => '');
      throw new Error(text || `Agent chat failed (HTTP ${res.status})`);
    }
    const reader = res.body.getReader();
    const decoder = new TextDecoder('utf-8');
    let buffer = '';
    for (;;) {
      if (signal?.aborted) throw new DOMException('Aborted', 'AbortError');
      const { done, value } = await reader.read();
      if (done) break;
      buffer += decoder.decode(value, { stream: true }).replace(/\r/g, '');
      let idx = buffer.indexOf('\n\n');
      while (idx !== -1) {
        const frame = buffer.slice(0, idx);
        buffer = buffer.slice(idx + 2);
        for (const line of frame.split('\n')) {
          const l = line.trim();
          if (!l.startsWith('data:')) continue;
          const data = l.slice(5).trim();
          if (!data || data === '[DONE]') continue;
          try {
            onEvent(JSON.parse(data) as { type: string; content?: string; name?: string; ok?: boolean; message?: string });
          } catch {
            // 忽略非 JSON 帧
          }
        }
        idx = buffer.indexOf('\n\n');
      }
    }
  },
};

/**
 * 解析单个 SSE 帧（OpenAI / Anthropic 协议）为增量回调序列。
 * 纯函数，便于单元测试。返回是否有终止帧（[DONE] / message_stop / error）。
 */
export function parseSseFrame(
  frame: string,
  onDelta: ChatStreamCallback,
  opts: { toolCallMode?: ToolCallWireMode } = {},
): boolean {
  const toolCallMode = opts.toolCallMode ?? 'auto';
  let ended = false;
  const lines = frame
    .split('\n')
    .map((l) => l.trim())
    .filter((l) => l.startsWith('data:') && l.length > 5)
    .map((l) => l.slice(5).trim())
    .filter((l) => l.length > 0);
  for (const data of lines) {
    if (data === '[DONE]') {
      if (!ended) onDelta({ content: '', isEnd: true });
      ended = true;
      continue;
    }
    try {
      const parsed = JSON.parse(data) as Record<string, unknown>;
      // Anthropic message_stop
      if (parsed.type === 'message_stop') {
        if (!ended) onDelta({ content: '', isEnd: true });
        ended = true;
        continue;
      }
      if (parsed.type === 'error' || parsed.error) {
        const raw = parsed.error as Record<string, unknown> | string | undefined;
        const msg = typeof raw === 'string' ? raw : (raw?.message as string | undefined) ?? '上游流式错误';
        // 标记 kind=error：ChatDebugger 据此以「⚠️」收尾而不是当正文拼接
        onDelta({ content: msg, isEnd: true, kind: 'error' });
        ended = true;
        continue;
      }
      // 工具调用分片 / 用量收尾（#63）：形状全部走适配层归一化，
      // 回灌层落地后只改 toolCallAdapter，不动解析与 UI。
      for (const tc of extractToolCalls(parsed, toolCallMode)) {
        onDelta({ content: '', isEnd: false, kind: 'tool_call', toolCall: tc });
      }
      const usage = extractUsage(parsed);
      if (usage) onDelta({ content: '', isEnd: false, kind: 'usage', usage });
      // Anthropic delta
      const anthropicDelta = parsed.delta as { text?: string } | undefined;
      if (anthropicDelta && typeof anthropicDelta.text === 'string' && anthropicDelta.text) {
        onDelta({ content: anthropicDelta.text, isEnd: false });
        continue;
      }
      // OpenAI delta（content / reasoning_content / text）
      const choices = parsed.choices as Array<{ delta?: Record<string, unknown> }> | undefined;
      const firstDelta = choices?.[0]?.delta;
      if (firstDelta) {
        const reasoning = [firstDelta.reasoning_content, firstDelta.reasoning]
          .find((v) => typeof v === 'string') as string | undefined;
        if (reasoning) onDelta({ content: reasoning, isEnd: false, kind: 'reasoning' });
        const content = ([firstDelta.content, firstDelta.text]
          .find((v) => typeof v === 'string') ?? '') as string;
        if (content) onDelta({ content, isEnd: false, kind: 'content' });
      }
    } catch {
      // 非 JSON 帧（心跳/注释）忽略
    }
  }
  return ended;
}

/**
 * 真·流式对话调试：逐块读取 text/event-stream 并增量解析。
 *
 * 与 testChannelChat 的区别：不等整个响应结束，每解析到一个增量
 * （OpenAI choices[0].delta 或 Anthropic delta.text）立即回调，
 * ChatDebugger 用它在界面上逐 token 渲染。
 */
export async function testChannelChatStream(
  data: PlaygroundChatRequest,
  onDelta: ChatStreamCallback,
  signal?: AbortSignal,
  opts: { toolCallMode?: ToolCallWireMode } = {},
): Promise<void> {
  const res = await fetch(`${API_BASE}/channels/chat_test`, {
    method: 'POST',
    headers: {
      'Content-Type': 'application/json',
      ...authHeaders(),
    },
    body: JSON.stringify(data),
    signal,
  });

  if (res.status === 401) {
    // 与 request() 对齐：会话失效时清理本地登录态，避免 SSE 通道被 401 后
    // 页面停留在「已登录但令牌已死」的状态。
    try {
      localStorage.removeItem('token');
      localStorage.removeItem('email');
      localStorage.removeItem('username');
      localStorage.removeItem('role');
      localStorage.removeItem('expires_at');
    } catch {
      // ignore
    }
    throw new Error('Unauthorized');
  }

  if (!res.ok || !res.body) {
    const text = await res.text().catch(() => '');
    let msg = text;
    if (text) {
      try {
        const parsed = JSON.parse(text) as Record<string, unknown>;
        const raw = parsed.error ?? parsed.message;
        if (typeof raw === 'string') msg = raw;
      } catch {
        // 非 JSON 错误体，直接抛原文
      }
    }
    throw new Error(msg || `Request failed with status ${res.status}`);
  }

  await readSseResponse(res, onDelta, signal, opts);
}

/**
 * 读取 SSE 响应并逐帧解析为增量回调。
 *
 * 由两条路径共用：testChannelChatStream（经后端中转）与 ChatDebugger
 * 直连模式（无密钥渠道由浏览器直接请求上游）。非流式 JSON（上游降级）
 * 时解析后一次性回调。
 */
export async function readSseResponse(
  res: Response,
  onDelta: ChatStreamCallback,
  signal?: AbortSignal,
  opts: { toolCallMode?: ToolCallWireMode } = {},
): Promise<void> {
  const contentType = res.headers.get('Content-Type') || '';
  if (!contentType.includes('text/event-stream')) {
    // 非流式 JSON（例如上游降级或后端报错），解析后一次性回调
    const text = await res.text();
    let parsed: unknown = null;
    if (text) {
      try {
        parsed = JSON.parse(text);
      } catch {
        // ignore
      }
    }
    const p = (parsed ?? {}) as Record<string, unknown>;
    const data = (p.data ?? {}) as Record<string, unknown>;
    // 工具调用 / 用量：非流式整帧也要走同一适配层（上游降级时常见）
    for (const tc of extractToolCalls(p, opts.toolCallMode ?? 'auto')) {
      onDelta({ content: '', isEnd: false, kind: 'tool_call', toolCall: tc });
    }
    const usage = extractUsage(p);
    if (usage) onDelta({ content: '', isEnd: false, kind: 'usage', usage });
    // 中转路径包一层 data.content；直连上游是 OpenAI 原生 choices 形状
    let content = typeof data.content === 'string' ? data.content : '';
    if (!content) {
      const choices = p.choices as Array<{ message?: { content?: string } }> | undefined;
      content = choices?.[0]?.message?.content ?? '';
    }
    const error = typeof data.error === 'string' ? data.error : typeof p.error === 'string' ? p.error : '';
    if (error) {
      onDelta({ content: error, isEnd: true });
    } else if (content) {
      onDelta({ content, isEnd: true });
    }
    return;
  }

  const reader = res.body?.getReader();
  if (!reader) return;
  const decoder = new TextDecoder('utf-8');
  let buffer = '';
  let ended = false;

  const handleFrame = (frame: string): void => {
    ended = parseSseFrame(frame, onDelta, opts) || ended;
  };

  for (;;) {
    if (signal?.aborted) throw new DOMException('Aborted', 'AbortError');
    const { done, value } = await reader.read();
    if (done) break;
    buffer += decoder.decode(value, { stream: true });
    // 统一行尾：剥离所有 \r（上游透传可能用 CRLF）。
    // SSE 数据行内不应有孤立 \r，strip 是安全的。
    buffer = buffer.replace(/\r/g, '');
    // 空行 = SSE 帧边界，只处理完整帧，避免半个 JSON 被解析失败
    let idx = buffer.indexOf('\n\n');
    while (idx !== -1) {
      const frame = buffer.slice(0, idx);
      buffer = buffer.slice(idx + 2);
      handleFrame(frame);
      idx = buffer.indexOf('\n\n');
    }
  }
  // 尾部残留帧
  if (buffer.trim()) handleFrame(buffer);
  if (!ended) onDelta({ content: '', isEnd: true });
}

/**
 * 无密钥渠道浏览器直连上游：不经 AIGX 后端中转，请求从用户本机直接发往
 * 上游 base_url。仅用于渠道「对话调试」弹窗中 api_key 为空的渠道。
 *
 * 协议复刻后端 handle_channel_chat_test 的 URL 构造：
 * - openai：normalize_base_url（无路径时补 /v1）+ /chat/completions
 * - anthropic：base 原样 + /v1/messages（x-api-key 空则省略）
 *
 * 注意：浏览器直连受上游 CORS 策略约束，跨域被拒时抛出带提示的错误。
 */
export async function directChannelChatStream(
  channel: { base_url?: string; channel_type?: string },
  data: PlaygroundChatRequest,
  onDelta: ChatStreamCallback,
  signal?: AbortSignal,
  opts: { toolCallMode?: ToolCallWireMode } = {},
): Promise<void> {
  if (!channel.base_url || !channel.base_url.trim()) {
    throw new Error('渠道未配置 Base URL，无法直连');
  }
  const isAnthropic = (channel.channel_type || '').toLowerCase() === 'anthropic';
  const rawBase = channel.base_url.trim().replace(/\/+$/, '');
  const hasPath = /^https?:\/\/[^/]+\/.+/.test(rawBase);
  const base = isAnthropic || hasPath ? rawBase : `${rawBase}/v1`;

  const messages: unknown[] = Array.isArray(data.history) ? data.history.slice() : [];
  messages.push({ role: 'user', content: data.message ?? '' });

  let url: string;
  let payload: Record<string, unknown>;
  if (isAnthropic) {
    url = `${base}/v1/messages`;
    payload = {
      model: data.model,
      max_tokens: data.max_tokens ?? 1024,
      messages,
    };
    if (data.stream) payload.stream = true;
    if (data.system_prompt) payload.system = data.system_prompt;
  } else {
    url = `${base}/chat/completions`;
    payload = {
      model: data.model,
      messages,
      stream: data.stream ?? false,
    };
    if (data.temperature !== undefined) payload.temperature = data.temperature;
    if (data.max_tokens !== undefined) payload.max_tokens = data.max_tokens;
    if (data.top_p !== undefined) payload.top_p = data.top_p;
    if (data.frequency_penalty !== undefined) payload.frequency_penalty = data.frequency_penalty;
    if (data.presence_penalty !== undefined) payload.presence_penalty = data.presence_penalty;
    if (data.system_prompt) {
      payload.messages = [{ role: 'system', content: data.system_prompt }, ...messages];
    }
  }

  let res: Response;
  try {
    res = await fetch(url, {
      method: 'POST',
      headers: isAnthropic
        ? { 'Content-Type': 'application/json', 'anthropic-version': '2023-06-01' }
        : { 'Content-Type': 'application/json' },
      body: JSON.stringify(payload),
      signal,
    });
  } catch (err) {
    // fetch 网络层失败（上游不可达或 CORS 拦截）——TypeError 不带原始原因
    if (signal?.aborted) throw err;
    throw new Error(
      `直连失败（请求未发或被 CORS 拦截）：上游 ${base} 不允许浏览器跨域访问。` +
      '可改用经服务器中转的普通渠道，或在上游网关配置 CORS 允许本站来源。',
    );
  }

  if (!res.ok) {
    const text = await res.text().catch(() => '');
    let msg = text;
    try {
      const parsed = JSON.parse(text) as Record<string, unknown>;
      const raw = parsed.error ?? parsed.message;
      if (typeof raw === 'string') msg = raw;
      else if (raw && typeof raw === 'object') {
        const m = (raw as Record<string, unknown>).message;
        if (typeof m === 'string') msg = m;
      }
    } catch {
      // 非 JSON 错误体，直接抛原文
    }
    throw new Error(msg || `Upstream HTTP ${res.status}`);
  }

  await readSseResponse(res, onDelta, signal, opts);
}

export default api;

/** 调度实时状态（/api/dashboard/scheduler_status，NetworkLayer 调度地图用） */
export async function getSchedulerStatus(): Promise<ApiResponse<SchedulerStatusItem>> {
  return request<ApiResponse<SchedulerStatusItem>>('GET', `${API_BASE}/dashboard/scheduler_status`);
}

/** 自环翻译提示词（/api/prompts/translate，走 AIGX 自己的渠道） */
export async function translatePrompts(
  items: Array<{ content: string }>,
  target = '简体中文',
): Promise<ApiResponse<PromptTranslateResult>> {
  return request<ApiResponse<PromptTranslateResult>>('POST', `${API_BASE}/prompts/translate`, {
    target,
    items,
  });
}
