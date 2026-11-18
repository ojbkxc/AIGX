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
import { API_BASE, request } from '../client';

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
  /** 写一家搜索提供商的密钥 / 启用态 / 账号绑定（api_key 只写不回） */
  saveToolProvider: (
    provider: string,
    body: { api_key?: string; account_id?: string | null; enabled?: boolean },
  ): Promise<ApiResponse<ToolProviderRow>> =>
    request<ApiResponse<ToolProviderRow>>('PUT', `${API_BASE}/tools/providers/${encodeURIComponent(provider)}`, body),
  /** 清除一家搜索提供商的密钥 */
  clearToolProviderKey: (provider: string): Promise<ApiResponse<{ success: boolean }>> =>
    request<ApiResponse<{ success: boolean }>>('DELETE', `${API_BASE}/tools/providers/${encodeURIComponent(provider)}/key`),
  /** 连通性测试一家搜索提供商 */
  testToolProvider: (provider: string): Promise<ApiResponse<ToolProviderTestResult>> =>
    request<ApiResponse<ToolProviderTestResult>>('POST', `${API_BASE}/tools/providers/${encodeURIComponent(provider)}/test`, {}),
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
};
