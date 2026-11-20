// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//
// AIGX × Agent2API v3.5.0-beta.1 对齐新增的类型面：插件 provider 设置、
// 账号排序规则、隐藏账号摘要、签到活动福利、工具服务、维护任务明细。
// 契约锚点均在各段注释里注明对应的 A2A 源文件。

// ==================== 插件 provider 设置（/api/plugins/provider-settings） ====================
// 契约锚点：api/plugin_provider_settings.rs + core/plugin_host/provider_settings/schema.rs。
// 字段声明由插件上报，后端原样透传 `schema`；前端只按类型渲染表单，不改结构。

/** provider 设置字段声明（插件上报，serde 原样透传） */
export interface PluginSettingsField {
  key: string;
  /** 多语标签：字符串或 {lang: 文案}（渲染用 pickLocalized 取值） */
  label: unknown;
  description?: unknown;
  /** text / password / number / boolean / choice / secret */
  type: string;
  default?: unknown;
  trim?: boolean;
  empty_is_default?: boolean;
  min_length?: number | null;
  max_length?: number | null;
  pattern?: string | null;
  minimum?: number | null;
  maximum?: number | null;
  options?: Array<{ value: unknown; label: unknown }>;
}

/** 一家 provider 的设置行（GET /api/plugins/provider-settings 的 settings[]） */
export interface PluginProviderSettingsRow {
  provider: string;
  label: string;
  /** 分组（同 owner 的多家 provider 共用一个设置表单） */
  group?: {
    id: string;
    label?: unknown;
    providers: Array<{ id: string; label: string; enabled: boolean }>;
  };
  schema: {
    version: number;
    title: unknown;
    description?: unknown;
    fields: PluginSettingsField[];
  };
  /** 已存的公开值（secret 类字段后端回 null/空串） */
  values?: Record<string, unknown>;
  /** 归一后的生效值（含默认值） */
  effective?: Record<string, unknown>;
  /** 字段键 → true 表示非法 */
  invalid?: Record<string, boolean>;
  /** 运行时覆盖（source + value），只读展示 */
  overrides?: Record<string, { source: string; value: unknown }>;
  /** 乐观锁版本串（保存时原样回传；过期后端 409） */
  revision: string;
  error?: string;
}

/** GET /api/plugins/provider-settings 响应 data */
export interface PluginProviderSettingsPayload {
  settings: PluginProviderSettingsRow[];
}

// ==================== 账号排序规则（GET/PUT /api/accounts/sorting） ====================
// 契约锚点：config/account_sort.rs（tag=key, camelCase, deny_unknown_fields）。

export type AccountSortRule =
  | { key: 'credit'; on: boolean; dir: 'soon' | 'late' }
  | { key: 'balance'; on: boolean; dir: 'high' | 'low' }
  | { key: 'added'; on: boolean; dir: 'new' | 'old' };

/** GET /api/accounts/sorting 响应 data（provider → 规则数组；最多三条） */
export type AccountSortSettings = Record<string, AccountSortRule[]>;

/** 隐藏账号摘要（对齐 A2A HiddenAccountsSummary；账号列表快照的 hiddenAccounts 字段） */
export interface HiddenAccountProviderItem {
  id: string;
  label: string;
  count: number;
  /** not_installed / pack_disabled / provider_disabled / not_loaded / loading / load_failed / load_timeout */
  reason: string;
  packId?: string | null;
}

export interface HiddenAccountsSummary {
  total: number;
  byProvider?: Record<string, number>;
  providers: HiddenAccountProviderItem[];
}

/** 导单文件（POST /api/accounts/export-files {id|provider} 的 data，逐账号凭证文件） */
export interface AccountFileExportResult {
  /** 文件名 → 内容（明文凭证，提示用户当作密码本保管） */
  files: Array<{ name: string; content: string }>;
  /** 导出格式版本 */
  format?: string;
  [key: string]: unknown;
}

// ==================== 签到中心：活动福利 / 月度赠送（/api/accounts/{id}/campaigns） ====================
// 契约锚点：api/campaign_rewards.rs + ui-shared/checkin-campaign-types.ts。

export interface CheckinCampaignReward {
  campaignId: string;
  title: string;
  titleEn?: string;
  description?: string;
  descriptionEn?: string;
  amount?: number | null;
  unit?: string;
  /** claimable / claimed / inactive / ineligible / confirming */
  status: string;
  startsAt?: number | null;
  endsAt?: number | null;
  claimedAt?: number | null;
  expiresAt?: number | null;
  validDays?: number | null;
}

export interface CheckinMonthlyGift {
  id: string;
  orderId?: string;
  period?: number;
  amount?: number | null;
  scheduledAt?: number | null;
  grantedAt?: number | null;
  /** pending / granted / failed / cancelled / processing / unknown */
  status: string;
}

/** GET /api/accounts/{id}/campaigns 响应 */
export interface CheckinCampaignPreview {
  campaigns: CheckinCampaignReward[];
  riskNote?: string | null;
  notice?: string | null;
  monthlyGifts?: CheckinMonthlyGift[] | null;
  redemption?: { supported: boolean };
}

/** POST /api/accounts/{id}/campaigns/claim 响应 */
export interface CheckinCampaignClaimResult {
  campaignId: string;
  outcome: string;
  campaign?: CheckinCampaignReward | null;
}

// ==================== 工具服务（/api/tools/*，后端 tools 模块） ====================
// 契约锚点：A2A api/tools.rs + core/tool_services。

export type ToolProtocol = 'skill' | 'mcp' | 'api';

export interface ToolServiceItem {
  /** web-search / vision / image-generation */
  id: string;
  protocol: ToolProtocol;
  /** 唯一键（同一服务可有多协议行：id + protocol） */
  key: string;
  enabled: boolean;
  capability_count: number;
  endpoint: string;
  package_name: string;
  version: string;
  timeout_seconds: number;
  recommended_variant?: string;
  variants?: ToolVariantItem[];
}

export interface ToolVariantItem {
  id: string;
  label: string;
  runtime: string;
  recommended: boolean;
  version: string;
  entrypoint: string;
}

// ── v3.5.0-beta.2：provider 多密钥 / 额度状态（tool_services::provider_keys）──
// 契约锚点：api/tools.rs put_provider（keys: ProviderKeyUpdate[]，最多 100）、
// test_provider?key_id=&operation=search|fetch。每把密钥按优先级从上到下使用，
// 额度耗尽 / 限流 / 无效状态与上游恢复时间（reset_at，unix 毫秒）随行返回。

/** 单把密钥在某操作维度（搜索 / 抓取）上的可用状态 */
export interface ProviderKeyStatus {
  status: 'ready' | 'quota_exhausted' | 'rate_limited' | 'invalid_provider_key';
  scope: 'all' | 'search' | 'fetch' | null;
  /** 上游给出的额度恢复时间（unix 毫秒）；null = 上游未提供，不自动重试 */
  reset_at: number | null;
  message: string | null;
}

/** 一把已保存的密钥行（明文不回，只回 key_hint） */
export interface ProviderKeyRow {
  id: string;
  priority: number;
  key_hint: string;
  updated_at: number | null;
  search: ProviderKeyStatus;
  fetch: ProviderKeyStatus;
}

/** 保存时的密钥更新项（id 缺省 = 新增；api_key 留空 = 保持原值） */
export interface ProviderKeyUpdate {
  id?: string;
  api_key?: string;
}

/** 搜索提供商行（密钥只回 configured / key_hint，不回明文） */
export interface ToolProviderRow {
  provider: string;
  display_name: string;
  auth_type?: 'api_key' | 'account';
  supports_search?: boolean;
  supports_fetch?: boolean;
  available?: boolean;
  configured: boolean;
  key_hint?: string | null;
  enabled: boolean;
  updated_at?: number | null;
  account_id?: string;
  account_name?: string;
  accounts?: Array<{ id: string; name: string; enabled: boolean }>;
  max_query_chars?: number | null;
  unavailable_reason?: string | null;
  // ── beta.2 多密钥扩展 ──
  /** 搜索维度是否可用（enabled && configured && 至少一把可用密钥） */
  usable?: boolean;
  fetch_usable?: boolean;
  /** 搜索 / 抓取当前优先使用的密钥 id（高亮用） */
  active_key_id?: string | null;
  active_fetch_key_id?: string | null;
  available_key_count?: number;
  available_fetch_key_count?: number;
  /** 单家最多可保存的密钥数 */
  max_keys?: number;
  /** 已保存的多密钥清单（单密钥后端可能不返回此字段） */
  keys?: ProviderKeyRow[];
}

export interface ToolProviderSelection {
  search_providers: string[];
  fetch_provider: string;
}

export interface ToolProviderTestResult {
  success: boolean;
  provider?: string;
  code?: string;
  error?: string;
  result_count?: number;
  duration_ms?: number;
}

/** 视觉分析绑定（provider + upstream_model + 思考等级 + 内部 Key） */
export interface ToolVisionConfig {
  provider: string;
  provider_label?: string;
  upstream_model: string;
  downstream_model?: string;
  reasoning?: string;
  api_key_id?: string;
  api_key_name?: string;
  configured?: boolean;
  key_available?: boolean;
}

export interface ToolVisionKeyOption {
  id: string;
  name: string;
  key_prefix?: string;
}

export interface ToolVisionPayload {
  config: ToolVisionConfig | null;
  keys: ToolVisionKeyOption[];
}

export interface ToolInstallTarget {
  key: string;
  label: string;
  path: string;
  detected: boolean;
  installed: boolean;
  supported?: boolean;
  note?: string;
}

export interface ToolInstallOutcome {
  clientId: string;
  label: string;
  ok: boolean;
  path: string;
  error?: string | null;
}

export interface ToolApiKeyOption {
  id: string;
  name: string;
  masked: string;
}

export interface ToolInstallInfo {
  base_url: string;
  service: string;
  name: string;
  version: string;
  timeout_seconds: number;
  mcp: { server_name: string; url: string; tools: string[]; bridge_url: string };
  skill?: {
    name: string;
    version: string;
    recommended_variant: string;
    variants?: ToolVariantItem[];
  };
}

// ==================== 维护任务明细（/api/maintenance，后端 maintenance 模块） ====================
// 契约锚点：A2A api/maintenance.rs + core/maintenance/types.rs（serde camelCase）。

export type MaintenanceTaskId = 'credentialMaintenance' | 'modelRefresh';
export type MaintenanceRunStatus = 'running' | 'completed' | 'partial' | 'failed' | 'skipped' | 'interrupted';

export interface MaintenanceTaskView {
  id: MaintenanceTaskId;
  enabled: boolean;
  interval: number;
  min: number;
  max: number;
  running: boolean;
  lastRunAt?: number | null;
  lastResult?: string | null;
  lastError?: string | null;
  nextRunAt?: number | null;
  retryAt?: number | null;
}

/** 单条账号级结果（明细表一行） */
export interface MaintenanceItemView {
  id: string;
  provider: string;
  providerLabel: string;
  accountId?: string | null;
  accountName?: string | null;
  /** refreshed / skipped / failed */
  status: string;
  message?: string | null;
  count?: number | null;
  refreshedAt?: number | null;
  expiresBefore?: number | null;
  expiresAfter?: number | null;
  credentialChanged?: boolean | null;
  durationMs?: number | null;
  fixed?: boolean | null;
  retryAt?: number | null;
  forced?: boolean;
  /** AIGX 扩展：refresh_token 已失效，单条重试无意义（后端序列化时 true 才出现） */
  invalid?: boolean;
}

/** 一次维护执行（记录列表一行） */
export interface MaintenanceRunView {
  id: string;
  taskId: MaintenanceTaskId;
  /** automatic / manual / passive */
  source: string;
  retryOf?: string | null;
  startedAt: number;
  finishedAt?: number | null;
  durationMs?: number | null;
  status: MaintenanceRunStatus;
  refreshed: number;
  skipped: number;
  failed: number;
  total: number;
  error?: string | null;
  metadata?: MaintenanceItemView | null;
}

/** 一次执行的完整明细（带 results 数组） */
export interface MaintenanceDetailView extends MaintenanceRunView {
  results: MaintenanceItemView[];
}

export interface MaintenanceOverviewView {
  tasks: Array<MaintenanceTaskView | null>;
  latest?: Partial<Record<MaintenanceTaskId, MaintenanceRunView>>;
  records: { items: MaintenanceRunView[]; total: number; offset: number; limit: number };
  retentionDays: number;
  maxRecords: number;
}

/** GET /api/maintenance 的查询参数 */
export interface MaintenanceQueryParams {
  taskId?: string;
  source?: string;
  status?: string;
  keyword?: string;
  offset?: number;
  limit?: number;
}