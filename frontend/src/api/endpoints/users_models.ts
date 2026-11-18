/**
 * 域端点（design 1.4：从 `api/index.ts` 的 `api` 对象按域搬出，纯搬移）。
 *
 * 方法原样保留，由 `index.ts` 展开合并进同一个 `api` 对象 —— 调用方仍写
 * `api.<method>()`，导入路径不变。
 */

import type {
  ApiResponse,
  User,
  ApiList,
  MessageResult,
  SettingsItem,
  ModelInfo,
  ModelMetaOverride,
  ManageView,
  ModelTestPayload,
  ModelTestResult,
  CheckinCenterData,
  CheckinRecord,
  CheckinSchedule,
  CheckinSettings,
  CheckinTravelState,
  CheckinRunResult,
  UpdateStatusItem,
  UpdateSettingsItem,
  CheckinBenefitsData,
  BenefitResult,
  KeepaliveConfig,
  ScheduledTaskList,
  ScheduledTaskItem,
  ScheduledTaskPatch,
  ScheduledTaskRunResult,
  BrandingData,
  BrandingAdminData,
} from '@/types';
import { API_BASE, buildQuery, request } from '../client';

export const endpoints = {
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
  // 批量映射前缀（对齐 Agent2API 批量加映射：前缀 + 同名合并到多家）
  // 契约锚点：src/api/admin/model_manage.rs::handle_add_alias_prefix（POST /api/models/alias/prefix）
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
    /** 1.0.49：记录类型 manual | schedule | makeup | retry（缺省不过滤；后端对存量记录回退按 source 匹配） */
    kind?: string;
    /** 1.0.49：起始 unix 秒（闭区间；缺省不限） */
    start?: number;
    /** 1.0.49：结束 unix 秒（闭区间；缺省不限） */
    end?: number;
    keyword?: string;
    offset?: number;
    limit?: number;
  }): Promise<ApiResponse<{ records: CheckinRecord[]; total: number }>> => {
    const query = new URLSearchParams();
    if (params.days != null) query.set('days', String(params.days));
    if (params.provider) query.set('provider', params.provider);
    if (params.account_id) query.set('account_id', params.account_id);
    if (params.outcome) query.set('outcome', params.outcome);
    if (params.kind) query.set('kind', params.kind);
    if (params.start != null) query.set('start', String(params.start));
    if (params.end != null) query.set('end', String(params.end));
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
    /* ── 1.0.49 契约字段（POST 全部可选：未传 = 继承存量；前端 feature-detect 门控）── */
    /** 窗口结束 HH:MM（空 = 无窗口）；randomize=true 时须晚于 time，否则后端 400 */
    time_end?: string;
    /** true = 每账号在 [time, time_end] 内取确定性随机时刻 */
    randomize?: boolean;
    /** 每账号每天重试上限（0 = 关闭；缺省 3） */
    retry_limit?: number;
    /** 重试间隔分钟（缺省 5；<=0 回落全局 retry_gap_ms） */
    retry_gap_minutes?: number;
    /** 补签轮时刻 HH:MM（空 = 不跑补签轮） */
    makeup_time?: string;
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

  // 面板品牌（公开 GET 免鉴权；管理员 GET/PUT）
  getPublicBranding: (): Promise<ApiResponse<BrandingData>> =>
    request<ApiResponse<BrandingData>>('GET', `${API_BASE}/public/branding`, null, { skipAuthRedirect: true }),
  getBranding: (): Promise<ApiResponse<BrandingAdminData>> =>
    request<ApiResponse<BrandingAdminData>>('GET', `${API_BASE}/branding`),
  updateBranding: (data: { panel_name?: string; logo_url?: string; favicon_url?: string }): Promise<ApiResponse<BrandingData>> =>
    request<ApiResponse<BrandingData>>('PUT', `${API_BASE}/branding`, data),

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

  // 内容护栏（输入/输出黑名单 + 200 假答复门控；GET/PUT 配置 + POST 解除降级）
  getGuardrail: (): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('GET', `${API_BASE}/guardrail`),
  saveGuardrail: (settings: Record<string, unknown>): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('PUT', `${API_BASE}/guardrail`, { settings }),
  clearGuardrailDegrade: (): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('POST', `${API_BASE}/guardrail/degrade/clear`),

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
};
