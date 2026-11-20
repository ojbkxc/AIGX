/**
 * 域端点（design 1.4：从 `api/index.ts` 的 `api` 对象按域搬出，纯搬移）。
 *
 * 方法原样保留，由 `index.ts` 展开合并进同一个 `api` 对象 —— 调用方仍写
 * `api.<method>()`，导入路径不变。
 */

import type {
  AccountFileExportResult,
  AccountSortRule,
  AccountSortSettings,
  CheckinCampaignClaimResult,
  CheckinCampaignPreview,
  MaintenanceDetailView,
  MaintenanceOverviewView,
  MaintenanceQueryParams,
  MaintenanceTaskId,
  PluginProviderSettingsPayload,
  ToolApiKeyOption,
  ToolInstallInfo,
  ToolInstallOutcome,
  ToolInstallTarget,
  ToolProtocol,
  ToolProviderRow,
  ToolProviderSelection,
  ToolProviderTestResult,
  ToolServiceItem,
  ToolVisionConfig,
  ToolVisionPayload,
  ApiResponse,
  ManageView,
} from '@/types';
import type { SystemInfo } from '@/types/system_info';
import { API_BASE, request } from '../client';

/** 账号分页页（A2A `core::accounts_list::page` 的响应，raw JSON）。 */
export interface AccountListPageView {
  accounts: Array<Record<string, unknown>>;
  matchedIds?: string[];
  displayIds?: string[];
  total: number;
  displayTotal?: number;
  offset?: number;
  pageSize?: number;
  summary?: Record<string, number>;
  counts?: Record<string, unknown>;
  mobileCounts?: Record<string, unknown> | null;
  groups?: Array<Record<string, unknown>>;
  sorting?: Record<string, unknown> | null;
  [key: string]: unknown;
}

/** 组余额汇总（`groups[].balance`，A2A `accounts_list/balance.rs`）。 */
export interface AccountGroupBalance {
  counted: number;
  total: number;
  sum: number | null;
  unlimited: number;
  unit: string;
  failed: number;
  uncountable: number;
  pending: number;
  partial: number;
  scope: 'group' | 'filtered';
  additive: boolean;
  ratio?: Record<string, unknown> | null;
  expiring: {
    count: number;
    todayCount: number;
    amounts: Array<{ unit: string; count: number; unknownCount: number; sum: number | null }>;
    todayAmounts: Array<{ unit: string; count: number; unknownCount: number; sum: number | null }>;
  };
  [key: string]: unknown;
}

export const endpoints = {
  // ── A2A v3.5.0-beta.1 对齐新增（T8）──────────────────────────────
  // 契约锚点见各方法注释；后端模块灌满前这些端点 404，调用方按 404 降级。

  /** 插件 provider 设置（插件上报 schema；GET 返回全部 provider 的设置行） */
  getPluginProviderSettings: (): Promise<ApiResponse<PluginProviderSettingsPayload>> =>
    request<ApiResponse<PluginProviderSettingsPayload>>('GET', `${API_BASE}/plugins/provider-settings`),
  /** 保存一家 provider 的设置：{provider, revision, values}（revision 过期 → 后端 409） */
  savePluginProviderSettings: (body: {
    provider: string;
    revision: string;
    values: Record<string, unknown>;
  }): Promise<ApiResponse<PluginProviderSettingsPayload>> =>
    request<ApiResponse<PluginProviderSettingsPayload>>('PATCH', `${API_BASE}/plugins/provider-settings`, body),
  /** 重载一个插件包（重启宿主进程） */
  reloadPlugin: (id: string): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('POST', `${API_BASE}/plugins/reload`, { id }),

  /** 账号服务端排序规则（provider → 规则数组） */
  getAccountSorting: (): Promise<ApiResponse<AccountSortSettings>> =>
    request<ApiResponse<AccountSortSettings>>('GET', `${API_BASE}/accounts/sorting`),
  /** 保存一家 provider 的排序规则；rules=null 恢复默认（后端校验最多三条、键不重复） */
  saveAccountSorting: (provider: string, rules: AccountSortRule[] | null): Promise<ApiResponse<AccountSortSettings>> =>
    request<ApiResponse<AccountSortSettings>>('PUT', `${API_BASE}/accounts/sorting`, { provider, rules }),
  /** 批量刷新全部已过期/临期的账号凭证（返回维护执行报告） */
  refreshExpiringAccounts: (): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('POST', `${API_BASE}/accounts/refresh-expiring`, {}),
  /** 导单文件：按账号 id 或 provider 分组导出逐账号凭证文件 */
  exportAccountFiles: (body: { id?: string; provider?: string }): Promise<ApiResponse<AccountFileExportResult>> =>
    request<ApiResponse<AccountFileExportResult>>('POST', `${API_BASE}/accounts/export-files`, body),

  // ── 账号模块 canonical `/api/accounts*`（A2A 语义，raw JSON）────────────
  // 新端点返回 A2A 原始 JSON（无 {success,data} 信封），调用方按各方法
  // 注释里的形状断言；错误仍走管理面 {success:false,error}。

  /** 账号分页查询（POST /api/accounts/query，body = AccountListQuery） */
  queryAccounts: (body: Record<string, unknown>): Promise<AccountListPageView> =>
    request<AccountListPageView>('POST', `${API_BASE}/accounts/query`, body),
  /** 账号选中集（POST /api/accounts/selection，只回匹配 id） */
  selectAccounts: (body: Record<string, unknown>): Promise<{ ids: string[]; total: number }> =>
    request<{ ids: string[]; total: number }>('POST', `${API_BASE}/accounts/selection`, body),
  /** 单账号详情（GET /api/accounts/:id，含 detailsLoaded:true） */
  getAccountDetail: (id: string): Promise<Record<string, unknown>> =>
    request<Record<string, unknown>>('GET', `${API_BASE}/accounts/${encodeURIComponent(id)}`),
  /** 全局队列置顶（POST /api/accounts/current {id}） */
  setCurrentAccount: (id: string): Promise<{ id: string; changed: boolean; reason?: string }> =>
    request<{ id: string; changed: boolean; reason?: string }>('POST', `${API_BASE}/accounts/current`, { id }),
  /** 整队重排（POST /api/accounts/reorder {queue}；queue 必须是全部账号的排列） */
  reorderAccounts: (queue: string[]): Promise<{ updated: number; skipped: number; total: number }> =>
    request<{ updated: number; skipped: number; total: number }>('POST', `${API_BASE}/accounts/reorder`, { queue }),
  /** 相邻交换（POST /api/accounts/:id/move {direction: 'up'|'down'}） */
  moveAccount: (id: string, direction: 'up' | 'down'): Promise<{ id: string; moved: boolean; reason?: string }> =>
    request<{ id: string; moved: boolean; reason?: string }>(
      'POST',
      `${API_BASE}/accounts/${encodeURIComponent(id)}/move`,
      { direction },
    ),
  /** 批量操作（POST /api/accounts/batch；action = enable/disable/proxy/concurrency/usageQuery/limiters/remove） */
  batchAccounts: (body: {
    ids: string[];
    action: string;
    proxy?: string | null;
    maxConcurrent?: number;
    usageQuery?: Record<string, unknown>;
    limiterMode?: string;
    limiterGroups?: unknown;
    limiters?: unknown;
  }): Promise<{ action: string; ok: unknown[]; failed: unknown[]; skipped: unknown[]; total: number }> =>
    request<{ action: string; ok: unknown[]; failed: unknown[]; skipped: unknown[]; total: number }>(
      'POST',
      `${API_BASE}/accounts/batch`,
      body,
    ),
  /** 导入账号（POST /api/accounts/import {accounts:[…]}；同 id 合并更新） */
  importAccounts: (payload: Record<string, unknown>): Promise<Record<string, unknown>> =>
    request<Record<string, unknown>>('POST', `${API_BASE}/accounts/import`, payload),
  /** 导出全部账号（GET /api/accounts/export，含 token） */
  exportAccounts: (): Promise<{ version: number; exportedAt: number; accounts: Array<Record<string, unknown>> }> =>
    request<{ version: number; exportedAt: number; accounts: Array<Record<string, unknown>> }>(
      'GET',
      `${API_BASE}/accounts/export`,
    ),
  /** 刷新指定/当前账号 token（POST /api/accounts/refresh {id?, force?}） */
  refreshAccount: (body: { id?: string; force?: boolean } = {}): Promise<{ refreshedId: string; result: unknown }> =>
    request<{ refreshedId: string; result: unknown }>('POST', `${API_BASE}/accounts/refresh`, body),
  /** 逐账号余额同步查询（GET /api/accounts/usage[?id=]） */
  queryAccountsUsage: (id?: string): Promise<{ results: Array<Record<string, unknown>>; skipped: number }> =>
    request<{ results: Array<Record<string, unknown>>; skipped: number }>(
      'GET',
      `${API_BASE}/accounts/usage${id ? `?id=${encodeURIComponent(id)}` : ''}`,
    ),
  /** 逐账号活跃连接数（GET /api/accounts/connections；只含非零项） */
  getAccountsConnections: (): Promise<{ counts: Record<string, number> }> =>
    request<{ counts: Record<string, number> }>('GET', `${API_BASE}/accounts/connections`),
  /** 账号页签到（POST /api/accounts/checkin {id?}） */
  runAccountsCheckin: (id?: string): Promise<Record<string, unknown>> =>
    request<Record<string, unknown>>('POST', `${API_BASE}/accounts/checkin`, id ? { id } : {}),

  /** 签到中心：某账号的活动福利 / 月度赠送预览 */
  getCheckinCampaigns: (accountId: string): Promise<ApiResponse<CheckinCampaignPreview>> =>
    request<ApiResponse<CheckinCampaignPreview>>('GET', `${API_BASE}/accounts/${encodeURIComponent(accountId)}/campaigns`),
  /** 领取一个活动奖励（campaignId 必须是预览返回的项） */
  claimCheckinCampaign: (accountId: string, campaignId: string): Promise<ApiResponse<CheckinCampaignClaimResult>> =>
    request<ApiResponse<CheckinCampaignClaimResult>>('POST', `${API_BASE}/accounts/${encodeURIComponent(accountId)}/campaigns/claim`, { campaignId }),

  /** 工具服务清单（可按协议过滤：skill / mcp / api） */
  listToolServices: (protocol?: ToolProtocol): Promise<ApiResponse<ToolServiceItem[]>> =>
    request<ApiResponse<ToolServiceItem[]>>(
      'GET',
      `${API_BASE}/tools/servers${protocol ? `?protocol=${encodeURIComponent(protocol)}` : ''}`,
    ),
  /** 工具服务启停与超时（PATCH {protocol, enabled?, timeout_seconds?}） */
  patchToolService: (id: string, body: { protocol: ToolProtocol; enabled?: boolean; timeout_seconds?: number }): Promise<ApiResponse<ToolServiceItem>> =>
    request<ApiResponse<ToolServiceItem>>('PATCH', `${API_BASE}/tools/servers/${encodeURIComponent(id)}`, body),
  /** 搜索提供商清单（密钥只回 configured / key_hint） */
  listToolProviders: (): Promise<ApiResponse<ToolProviderRow[]>> =>
    request<ApiResponse<ToolProviderRow[]>>('GET', `${API_BASE}/tools/providers`),
  /** 搜索/抓取提供商选择 */
  getToolProviderSelection: (): Promise<ApiResponse<ToolProviderSelection>> =>
    request<ApiResponse<ToolProviderSelection>>('GET', `${API_BASE}/tools/provider-selection`),
  saveToolProviderSelection: (body: ToolProviderSelection): Promise<ApiResponse<ToolProviderSelection>> =>
    request<ApiResponse<ToolProviderSelection>>('PUT', `${API_BASE}/tools/provider-selection`, body),
  /** 写一家搜索提供商的密钥 / 启用态 / 账号绑定（api_key 只写不回）。
   *  beta.2 多密钥：`keys` = `{id?, api_key?}[]`（id 缺省新增，留空保持原值，最多 100）。 */
  saveToolProvider: (
    provider: string,
    body: {
      api_key?: string;
      account_id?: string | null;
      enabled?: boolean;
      keys?: Array<{ id?: string; api_key?: string }>;
    },
  ): Promise<ApiResponse<ToolProviderRow>> =>
    request<ApiResponse<ToolProviderRow>>('PUT', `${API_BASE}/tools/providers/${encodeURIComponent(provider)}`, body),
  /** 清除一家搜索提供商的密钥 */
  clearToolProviderKey: (provider: string): Promise<ApiResponse<{ success: boolean }>> =>
    request<ApiResponse<{ success: boolean }>>('DELETE', `${API_BASE}/tools/providers/${encodeURIComponent(provider)}/key`),
  /** 连通性测试一家搜索提供商（beta.2：可按 key_id + operation=search|fetch 单密钥检查） */
  testToolProvider: (
    provider: string,
    opts: { keyId?: string; operation?: 'search' | 'fetch' } = {},
  ): Promise<ApiResponse<ToolProviderTestResult>> => {
    const query = new URLSearchParams();
    if (opts.keyId) query.set('key_id', opts.keyId);
    if (opts.operation) query.set('operation', opts.operation);
    const suffix = query.toString();
    return request<ApiResponse<ToolProviderTestResult>>(
      'POST',
      `${API_BASE}/tools/providers/${encodeURIComponent(provider)}/test${suffix ? `?${suffix}` : ''}`,
      {},
    );
  },
  /** 视觉分析绑定（provider / upstream_model / 思考等级 / 内部 Key + 可选 Key 清单） */
  getToolVisionConfig: (): Promise<ApiResponse<ToolVisionPayload>> =>
    request<ApiResponse<ToolVisionPayload>>('GET', `${API_BASE}/tools/vision/config`),
  saveToolVisionConfig: (body: {
    provider: string;
    upstream_model: string;
    reasoning_effort?: string;
    api_key_id: string;
  }): Promise<ApiResponse<ToolVisionConfig>> =>
    request<ApiResponse<ToolVisionConfig>>('PUT', `${API_BASE}/tools/vision/config`, body),
  clearToolVisionConfig: (): Promise<ApiResponse<{ success: boolean }>> =>
    request<ApiResponse<{ success: boolean }>>('DELETE', `${API_BASE}/tools/vision/config`),
  /** 用当前绑定做一次真实视觉调用（走网关内部 Key） */
  testToolVision: (): Promise<ApiResponse<{ success: boolean; duration_ms?: number; reply?: string; error?: string }>> =>
    request<ApiResponse<{ success: boolean; duration_ms?: number; reply?: string; error?: string }>>(
      'POST',
      `${API_BASE}/tools/vision/config/test`,
      {},
    ),
  /** 可用网关 Key（「接入」下拉用；只回 id/name/掩码） */
  listToolApiKeys: (): Promise<ApiResponse<{ keys: ToolApiKeyOption[] }>> =>
    request<ApiResponse<{ keys: ToolApiKeyOption[] }>>('GET', `${API_BASE}/tools/api-keys`),
  /** 安装目标探测（service=web-search&protocol=skill|mcp） */
  getToolInstallTargets: (
    service: string,
    protocol: 'skill' | 'mcp',
  ): Promise<ApiResponse<{ targets: ToolInstallTarget[] }>> =>
    request<ApiResponse<{ targets: ToolInstallTarget[] }>>(
      'GET',
      `${API_BASE}/tools/install-targets?service=${encodeURIComponent(service)}&protocol=${encodeURIComponent(protocol)}`,
    ),
  /** 安装信息（运行时地址 / MCP 端点 / Skill 变体） */
  getToolInstallInfo: (service: string): Promise<ApiResponse<ToolInstallInfo>> =>
    request<ApiResponse<ToolInstallInfo>>('GET', `${API_BASE}/tools/install-info?service=${encodeURIComponent(service)}`),
  /** 安装 Skill 到本机客户端目录 */
  installToolSkill: (body: {
    variant: string;
    api_key_id: string;
    clients: string[];
  }): Promise<ApiResponse<{ results: ToolInstallOutcome[] }>> =>
    request<ApiResponse<{ results: ToolInstallOutcome[] }>>('POST', `${API_BASE}/tools/install-skill`, body),
  /** 安装 MCP 配置到本机客户端目录 */
  installToolMcp: (body: {
    service_id: string;
    mode?: string;
    api_key_id: string;
    clients: string[];
    output_directory?: string;
    reference_directories?: string[];
  }): Promise<ApiResponse<{ results: ToolInstallOutcome[] }>> =>
    request<ApiResponse<{ results: ToolInstallOutcome[] }>>('POST', `${API_BASE}/tools/install-mcp`, body),
  /** 卸载 Skill */
  uninstallToolSkill: (body: { service_id: string; clients: string[] }): Promise<ApiResponse<{ results: ToolInstallOutcome[] }>> =>
    request<ApiResponse<{ results: ToolInstallOutcome[] }>>('POST', `${API_BASE}/tools/uninstall-skill`, body),
  /** 卸载 MCP 配置 */
  uninstallToolMcp: (body: { service_id: string; clients: string[] }): Promise<ApiResponse<{ results: ToolInstallOutcome[] }>> =>
    request<ApiResponse<{ results: ToolInstallOutcome[] }>>('POST', `${API_BASE}/tools/uninstall-mcp`, body),

  /** 维护任务总览（两类任务 + 最新执行 + 执行记录分页） */
  getMaintenanceOverview: (params: MaintenanceQueryParams = {}): Promise<ApiResponse<MaintenanceOverviewView>> => {
    const query = new URLSearchParams();
    for (const [key, value] of Object.entries(params)) {
      if (value === undefined || value === null || value === '') continue;
      query.set(key, String(value));
    }
    const suffix = query.toString();
    return request<ApiResponse<MaintenanceOverviewView>>(
      'GET',
      `${API_BASE}/maintenance${suffix ? `?${suffix}` : ''}`,
    );
  },
  /** 单次执行明细（带 results 数组） */
  getMaintenanceRecord: (id: string): Promise<ApiResponse<MaintenanceDetailView>> =>
    request<ApiResponse<MaintenanceDetailView>>('GET', `${API_BASE}/maintenance/records/${encodeURIComponent(id)}`),
  /** 立即执行一类维护任务 */
  runMaintenance: (taskId: MaintenanceTaskId): Promise<ApiResponse<MaintenanceDetailView>> =>
    request<ApiResponse<MaintenanceDetailView>>('POST', `${API_BASE}/maintenance/run/${encodeURIComponent(taskId)}`, {}),
  /** 重试一次执行（可只重试某一条：itemId） */
  retryMaintenance: (recordId: string, itemId?: string): Promise<ApiResponse<MaintenanceDetailView>> =>
    request<ApiResponse<MaintenanceDetailView>>(
      'POST',
      `${API_BASE}/maintenance/records/${encodeURIComponent(recordId)}/retry`,
      itemId ? { itemId } : {},
    ),

  /** 能力位三态（provider × model）：POST /api/models/capabilities {provider, id, capabilities} */
  setModelCapabilities: (
    provider: string,
    id: string,
    capabilities: Record<string, boolean | number | null>,
  ): Promise<ApiResponse<ManageView>> =>
    request<ApiResponse<ManageView>>('POST', `${API_BASE}/models/capabilities`, {
      provider,
      id,
      capabilities,
    }),

  /** 手动全量刷新模型清单（POST /api/models/refresh） */
  refreshModels: (): Promise<ApiResponse<{ results: unknown[]; refreshed: number; skipped: number; failed: number; message?: string }>> =>
    request<ApiResponse<{ results: unknown[]; refreshed: number; skipped: number; failed: number; message?: string }>>(
      'POST',
      `${API_BASE}/models/refresh`,
      {},
    ),

  // ── A2A v3.5.0-beta.2 配置面新增（契约锚点见各方法注释）───────────────────
  // 后端由 config-align 同步实施；端点未就绪（404）时组件整块降级「后端未就绪」，
  // 这里不吞错。返回值按各组件本地定义的视图类型断言（data 形状见组件）。

  /** 运行容量快照（GET /api/plugins/capacity）—— settings/revision/effective/resources/usage */
  getRuntimeCapacity: (): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('GET', `${API_BASE}/plugins/capacity`),
  /** 保存运行容量（PATCH /api/plugins/capacity `{settings, revision}`，revision 过期 → 409） */
  saveRuntimeCapacity: (body: { settings: unknown; revision: string }): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('PATCH', `${API_BASE}/plugins/capacity`, body),

  /** 错误处理策略（GET /api/routing/errors）—— settings/defaults/revision/providers */
  getRoutingErrors: (): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('GET', `${API_BASE}/routing/errors`),
  /** 保存错误处理策略（PUT /api/routing/errors `{settings, revision}`，revision 过期 → 409） */
  saveRoutingErrors: (body: { settings: unknown; revision: string }): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('PUT', `${API_BASE}/routing/errors`, body),

  /** 网页工具调用模拟开关（GET /api/web-tool-emulation）—— {enabled, providers[]} */
  getWebToolEmulation: (): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('GET', `${API_BASE}/web-tool-emulation`),
  /** 保存网页工具调用模拟开关（PUT /api/web-tool-emulation `{enabled}`） */
  saveWebToolEmulation: (enabled: boolean): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('PUT', `${API_BASE}/web-tool-emulation`, { enabled }),

  /** 系统提示词规则引擎（GET /api/prompt/rules）—— config/revision/providers/clients/models/… */
  getPromptRules: (): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('GET', `${API_BASE}/prompt/rules`),
  /** 保存提示词规则（PUT /api/prompt/rules `{config, revision}`，revision 过期 → 409） */
  savePromptRules: (body: { config: unknown; revision: string }): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('PUT', `${API_BASE}/prompt/rules`, body),
  /** 规则命中预览（POST /api/prompt/rules/preview `{config, sample}`） */
  previewPromptRules: (body: { config: unknown; sample: { provider: string; client: string; model: string } }): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('POST', `${API_BASE}/prompt/rules/preview`, body),

  /**
   * 系统信息（GET /api/system-info；对照 Agent2API 同名端点）。
   * `hideUsername` = 后端把主目录前缀替换成 `~`（复制/截图分享用）。
   */
  getSystemInfo: (hideUsername = false): Promise<ApiResponse<SystemInfo>> =>
    request<ApiResponse<SystemInfo>>(
      'GET',
      `${API_BASE}/system-info${hideUsername ? '?hideUsername=1' : ''}`,
    ),
};
