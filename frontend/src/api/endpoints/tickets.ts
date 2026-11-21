/**
 * 域端点（design 1.4：从 `api/index.ts` 的 `api` 对象按域搬出，纯搬移）。
 *
 * 方法原样保留，由 `index.ts` 展开合并进同一个 `api` 对象 —— 调用方仍写
 * `api.<method>()`，导入路径不变。
 */

import type {
  AccountBalance,
  AccountBatchReceipt,
  AccountBatchRequest,
  AccountExportFile,
  AccountProviderItem,
  AccountSessionItem,
  ProxyPoolItem,
  ProxyPoolResult,
  ProxySettings,
  ProxyTestResult,
  SystemProxySnapshot,
  AccountSessionStatsItem,
  ApiResponse,
  ApiList,
  MessageResult,
  ChannelItem,
  OrderItem,
  TicketItem,
  CatalogSyncResult,
  ImportEntry,
  ImportSources,
  ImportRunResult,
  AccountRotationPolicyItem,
  AgentSettingsItem,
  ChatSettingsItem,
  GatewaySettingsItem,
  SecurityPolicyItem,
  DebugTrafficView,
  DebugTrafficDetail,
  ModelMetadataStatus,
  ModelMetadataRefreshResult,
  RoutingPolicyItem,
  RoutingQueueItem,
} from '@/types';
import type { RouteReplaySnapshot, RouteStickyList } from '@/types/routing-replay';
import type {
  RoutingPreviewItem,
  RoutingPreviewRequest,
  RoutingProfiles,
  RoutingPolicyWithProfiles,
  RoutingQueueWithLayers,
} from '@/types/routing-profile';
import type { DesktopSourceItem } from '@/types/desktop';
import type { ReportSettingsItem, TzMigrateResult } from '@/types/report-tz';
import type { ImageCompatSettingsBody, ImageCompatSettingsItem } from '@/types/image-compat';
import { API_BASE, buildQuery, request } from '../client';

/**
 * 归一 `{success,data}` 外壳。
 *
 * `/api/routing`、`/routing/{profiles,queue,preview,defaults}` 这几个策略端点
 * 后端返回的是**裸 JSON**（例如 `{strategy,sticky,profiles,...}`），而本仓
 * 其余端点（工作台 / 特殊路由 / 视觉兼容 / 图片兼容）返回 `{success,data}`。
 * 两种形状在这里统一收敛成 `{success,data}`，调用方继续按 `res.data` 读取 ——
 * 避免「接口明明有数据、页面却因 `res.data` 为空而静默显示默认值」。
 */
function asEnvelope<T>(raw: unknown): ApiResponse<T> {
  if (raw && typeof raw === 'object' && !Array.isArray(raw) && 'success' in raw) {
    return raw as ApiResponse<T>;
  }
  return { success: true, data: raw as T };
}

/** 兼容两种外壳，取出业务体。 */
function bodyOf(raw: unknown): Record<string, unknown> | null {
  const body =
    raw && typeof raw === 'object' && 'data' in raw
      ? (raw as { data?: unknown }).data
      : raw;
  return body && typeof body === 'object' && !Array.isArray(body)
    ? (body as Record<string, unknown>)
    : null;
}

/**
 * POST /api/routing/preview 后端把生效值放在 `resolved` 下
 * （strategy/sticky/scope/sortRules/defaultGroup/provenance/appliedLayers），
 * 且组号键名是 `requestedGroupId`。这里摊平成前端 `RoutingPreviewItem`
 * 的扁平形状；其余字段（ordered/blocked/skipped/pickedId）原样带过。
 */
function flattenPreview(raw: unknown): RoutingPreviewItem {
  const body = bodyOf(raw);
  if (!body) return {} as RoutingPreviewItem;
  const resolved = (body.resolved ?? {}) as Record<string, unknown>;
  const profile = {
    scope: resolved.scope ?? null,
    strategy: resolved.strategy ?? null,
    sticky: resolved.sticky ?? null,
    needs: resolved.needs ?? null,
    sort_rules: resolved.sortRules ?? null,
    default_group: resolved.defaultGroup ?? null,
  } as RoutingPreviewItem['profile'];
  return {
    model: String(body.model ?? ''),
    strategy: resolved.strategy as RoutingPreviewItem['strategy'],
    sticky: resolved.sticky as RoutingPreviewItem['sticky'],
    groupId: (body.requestedGroupId as string | null | undefined) ?? null,
    appliedLayers: (resolved.appliedLayers as RoutingPreviewItem['appliedLayers']) ?? [],
    provenance: (resolved.provenance as RoutingPreviewItem['provenance']) ?? undefined,
    profile,
    pickedId: (body.pickedId as string | null | undefined) ?? null,
    ordered: (body.ordered as RoutingPreviewItem['ordered']) ?? [],
    blocked: (body.blocked as RoutingPreviewItem['blocked']) ?? [],
    skipped: (body.skipped as RoutingPreviewItem['skipped']) ?? undefined,
  };
}

export const endpoints = {
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
  /** 代理连通性测试设置（A2A `/api/proxies/settings`） */
  proxySettings: (): Promise<ApiResponse<ProxySettings>> =>
    request<ApiResponse<ProxySettings>>('GET', `${API_BASE}/proxies/settings`),
  saveProxySettings: (data: { testUrl: string }): Promise<ApiResponse<ProxySettings>> =>
    request<ApiResponse<ProxySettings>>('POST', `${API_BASE}/proxies/settings`, data),
  /** 系统代理快照（A2A `/api/proxies/system`；账号代理表单「从系统代理导入」用） */
  systemProxy: (): Promise<ApiResponse<SystemProxySnapshot>> =>
    request<ApiResponse<SystemProxySnapshot>>('GET', `${API_BASE}/proxies/system`),
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
  // 会话图片压缩与视觉兼容（/api/image-compat/settings，#7）：GET 响应 camelCase，
  // PUT body camelCase（后端收别名），缺省字段保持原值；保存即热生效。越界 400。
  getImageCompatSettings: (): Promise<ApiResponse<ImageCompatSettingsItem>> =>
    request<ApiResponse<ImageCompatSettingsItem>>('GET', `${API_BASE}/image-compat/settings`),
  saveImageCompatSettings: (data: ImageCompatSettingsBody): Promise<ApiResponse<ImageCompatSettingsItem>> =>
    request<ApiResponse<ImageCompatSettingsItem>>('PUT', `${API_BASE}/image-compat/settings`, data),
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
  // 单条详情（对照 Agent2API GET /api/debug/traffic?id=）：404 = 该请求没有保存原始报文
  getDebugTrafficDetail: (id: string): Promise<ApiResponse<DebugTrafficDetail>> =>
    request<ApiResponse<DebugTrafficDetail>>('GET', `${API_BASE}/debug/traffic?id=${encodeURIComponent(id)}`),
  clearDebugTraffic: (): Promise<ApiResponse<{ cleared: number }>> =>
    request<ApiResponse<{ cleared: number }>>('DELETE', `${API_BASE}/debug/traffic`),
  // models.dev 模型元数据（对照 Agent2API `/api/models/metadata`）：开关 + 状态 + 手动刷新。
  getModelMetadata: (): Promise<ApiResponse<ModelMetadataStatus>> =>
    request<ApiResponse<ModelMetadataStatus>>('GET', `${API_BASE}/models/metadata`),
  saveModelMetadata: (enabled: boolean): Promise<ApiResponse<ModelMetadataStatus>> =>
    request<ApiResponse<ModelMetadataStatus>>('PUT', `${API_BASE}/models/metadata`, { enabled }),
  refreshModelMetadata: (): Promise<ApiResponse<ModelMetadataRefreshResult>> =>
    request<ApiResponse<ModelMetadataRefreshResult>>('POST', `${API_BASE}/models/metadata/refresh`),
  // 路由策略（对照 Agent2API「设置 → 路由」；保存即热生效，无需重启）。
  // 这些端点后端返回**裸 JSON** —— 经 asEnvelope 归一成 {success,data}。
  getRoutingPolicy: async (): Promise<ApiResponse<RoutingPolicyWithProfiles>> =>
    asEnvelope<RoutingPolicyWithProfiles>(await request('GET', `${API_BASE}/routing`)),
  saveRoutingPolicy: async (data: Record<string, unknown>): Promise<ApiResponse<RoutingPolicyItem>> =>
    asEnvelope<RoutingPolicyItem>(await request('PUT', `${API_BASE}/routing`, data)),
  // #40 多规则总开关：PUT /api/routing 是读-改-写，只传 enabled 不会碰其他字段
  saveRoutingEnabled: async (enabled: boolean): Promise<ApiResponse<RoutingPolicyItem>> =>
    asEnvelope<RoutingPolicyItem>(await request('PUT', `${API_BASE}/routing`, { enabled })),
  // #40 整份档案读写（契约 §5）。**请求体是 `{ profiles }`**（后端
  // `RoutingProfilesUpdate { profiles: Option<_> }`）—— 发裸档案对象会被
  // serde 读成 profiles=None，从而变成「200 成功但什么都没改」的静默
  // no-op。响应是 `{ enabled, profiles }`，这里取出 profiles 回填。
  saveRoutingProfiles: async (profiles: RoutingProfiles): Promise<ApiResponse<RoutingProfiles>> => {
    const raw = await request<unknown>('PUT', `${API_BASE}/routing/profiles`, { profiles });
    const body = bodyOf(raw);
    const saved = (body?.profiles as RoutingProfiles | undefined) ?? profiles;
    return { success: true, data: saved };
  },
  getRoutingDefaults: async (): Promise<ApiResponse<RoutingPolicyItem>> =>
    asEnvelope<RoutingPolicyItem>(await request('GET', `${API_BASE}/routing/defaults`)),
  // 路由候选队列预演（对照 Agent2API「设置 → 路由 → 队列」：只读，不推进轮询游标）
  getRoutingQueue: async (model: string): Promise<ApiResponse<RoutingQueueItem & RoutingQueueWithLayers>> =>
    asEnvelope<RoutingQueueItem & RoutingQueueWithLayers>(
      await request('GET', `${API_BASE}/routing/queue?model=${encodeURIComponent(model)}`),
    ),
  // #40 档案预演（契约 §5）：与真实转发走同一个 plan_route，返回命中层与逐字段来源。
  // 后端把生效值放在 `resolved` 下、组号键名 requestedGroupId —— flattenPreview 摊平。
  previewRouting: async (req: RoutingPreviewRequest): Promise<ApiResponse<RoutingPreviewItem>> =>
    asEnvelope<RoutingPreviewItem>(flattenPreview(await request('POST', `${API_BASE}/routing/preview`, req))),
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
};
