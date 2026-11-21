/**
 * 域端点（design 1.4：从 `api/index.ts` 的 `api` 对象按域搬出，纯搬移）。
 *
 * 方法原样保留，由 `index.ts` 展开合并进同一个 `api` 对象 —— 调用方仍写
 * `api.<method>()`，导入路径不变。
 */

import type {
  ApiResponse,
  ApiList,
  MessageResult,
  TokenItem,
  TokenKeyResult,
  PriceEntry,
  GroupItem,
  RedemptionItem,
  AlertRule,
  AlertEvent,
  NotifyConfigItem,
  DiagnosticsSummary,
  DiagnosticsChannelProbes,
  DiagnosticsBreakers,
  RateLimitConfigItem,
  InflightRequestItem,
  PlaygroundChatRequest,
  PlaygroundChatResult,
  ClineHeadersItem,
  GatewayPromptItem,
  SanitizeHeadersItem,
  PurgeData,
  PurgePreviewResult,
  PurgeSettings,
  PurgePresetApplyResult,
  PurgePresetApplyTarget,
  PurgePresetDetail,
  PurgePresetList,
  PurgeSkillImportResult,
  ZcodeCaptchaConfig,
  ZcodeCaptchaPoolStats,
  ZcodeCaptchaPushResult,
  ZcodeCaptchaPoolSettings,
  StorageOverviewItem,
  BackupImportResult,
} from '@/types';
import type {
  CompactRoutesConfig,
  CompactRoutesView,
  RoutingFlowQuery,
  RoutingFlowSnapshot,
  RoutingReplaysList,
  RoutingReplaysQuery,
  RoutingTraceList,
  RoutingTraceQuery,
  RoutingTraceRow,
  RoutingWorkspaceView,
  TitleRoutesConfig,
  TitleRoutesView,
  VisionCompatView,
  VisionSettingsConfig,
} from '@/types/routing-workspace';
import { API_BASE, authHeaders, buildQuery, getToken, request } from '../client';
import { testChannelChatStream } from '../stream';

export const endpoints = {
  // ── 路由工作台（be-routing）：workspace 覆盖对比 / flow 请求流向 / trace 跟踪 ──
  // 契约形状见 types/routing-workspace.ts；端点未就绪时 404，调用方按空态降级。
  getRoutingWorkspace: (model?: string): Promise<ApiResponse<RoutingWorkspaceView>> =>
    request<ApiResponse<RoutingWorkspaceView>>(
      'GET',
      `${API_BASE}/routing/workspace${model ? `?model=${encodeURIComponent(model)}` : ''}`,
    ),
  getRoutingFlow: (query: RoutingFlowQuery): Promise<ApiResponse<RoutingFlowSnapshot>> =>
    request<ApiResponse<RoutingFlowSnapshot>>('GET', `${API_BASE}/routing/flow?${buildQuery({ ...query })}`),
  getRoutingTrace: (query: RoutingTraceQuery): Promise<ApiResponse<RoutingTraceList>> =>
    request<ApiResponse<RoutingTraceList>>('GET', `${API_BASE}/routing/trace?${buildQuery({ ...query })}`),
  /** 可回放请求列表（route_replay 有捕获的终态日志；「最近命中」右栏复用）。 */
  getRoutingReplays: (query: RoutingReplaysQuery): Promise<ApiResponse<RoutingReplaysList>> =>
    request<ApiResponse<RoutingReplaysList>>('GET', `${API_BASE}/routing/replays?${buildQuery({ ...query })}`),
  getRoutingTraceRequest: (id: string, ts?: number): Promise<ApiResponse<RoutingTraceRow>> =>
    request<ApiResponse<RoutingTraceRow>>(
      'GET',
      `${API_BASE}/routing/trace/request?id=${encodeURIComponent(id)}${ts != null ? `&ts=${encodeURIComponent(String(ts))}` : ''}`,
    ),
  // ── 压缩路由（be-special）。canonical 路径对齐 Agent2API v3.5.0-beta.1：
  //    GET/PUT /api/models/compact-routes。PUT body `{ config, revision }`；
  //    修订号不符 → 409（err.status 可辨）。**不做 404 回退**：端点未注册
  //    直接报错可见，避免每次请求多打一次、也避免掩盖接线问题。
  getCompactRoutes: (): Promise<ApiResponse<CompactRoutesView>> =>
    request<ApiResponse<CompactRoutesView>>('GET', `${API_BASE}/models/compact-routes`),
  saveCompactRoutes: (config: CompactRoutesConfig, revision: string): Promise<ApiResponse<CompactRoutesView>> =>
    request<ApiResponse<CompactRoutesView>>('PUT', `${API_BASE}/models/compact-routes`, {
      config,
      revision,
    }),
  // ── 标题路由（be-special）。canonical：GET/PUT /api/models/title-routes ──
  getTitleRoutes: (): Promise<ApiResponse<TitleRoutesView>> =>
    request<ApiResponse<TitleRoutesView>>('GET', `${API_BASE}/models/title-routes`),
  saveTitleRoutes: (config: TitleRoutesConfig, revision: string): Promise<ApiResponse<TitleRoutesView>> =>
    request<ApiResponse<TitleRoutesView>>('PUT', `${API_BASE}/models/title-routes`, {
      config,
      revision,
    }),
  // ── 视觉兼容（be-vision）：GET/PUT /api/vision-compatibility（整块 + revision）──
  getVisionCompatibility: (): Promise<ApiResponse<VisionCompatView>> =>
    request<ApiResponse<VisionCompatView>>('GET', `${API_BASE}/vision-compatibility`),
  saveVisionCompatibility: (
    config: VisionSettingsConfig,
    revision: string,
  ): Promise<ApiResponse<VisionCompatView>> =>
    request<ApiResponse<VisionCompatView>>('PUT', `${API_BASE}/vision-compatibility`, {
      config,
      revision,
    }),
  // Cline 伪装头覆盖表（对照 Agent2API「设置 → 网关 → Cline 伪装头」；保存即生效）
  getClineHeaders: (): Promise<ApiResponse<ClineHeadersItem>> =>
    request<ApiResponse<ClineHeadersItem>>('GET', `${API_BASE}/cline-headers`),
  saveClineHeaders: (overrides: Record<string, string>): Promise<ApiResponse<ClineHeadersItem>> =>
    request<ApiResponse<ClineHeadersItem>>('PUT', `${API_BASE}/cline-headers`, { overrides }),
  // ── 客户端识别（对照 Agent2API core/client）：规则 CRUD + 预览 ──
  getClientId: (): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('GET', `${API_BASE}/client-id`),
  createClientRule: (rule: Record<string, unknown>): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('POST', `${API_BASE}/client-id/rules`, rule),
  updateClientRule: (id: string, rule: Record<string, unknown>): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('PUT', `${API_BASE}/client-id/rules/${encodeURIComponent(id)}`, rule),
  deleteClientRule: (id: string): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('DELETE', `${API_BASE}/client-id/rules/${encodeURIComponent(id)}`),
  previewClientId: (sample: { userAgent?: string; headers?: Record<string, string> }): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('POST', `${API_BASE}/client-id/preview`, sample),
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
  // 破甲控制台（请求治理总控：归一化/改写/降档/异源/技能注入；热生效）
  getPurge: (): Promise<ApiResponse<PurgeData>> =>
    request<ApiResponse<PurgeData>>('GET', `${API_BASE}/purge`),
  savePurge: (data: PurgeSettings): Promise<ApiResponse<PurgeData>> =>
    request<ApiResponse<PurgeData>>('PUT', `${API_BASE}/purge`, data),
  // 破甲改写试跑（dry-run：不写统计、不落库）
  previewPurge: (payload: {
    settings?: PurgeSettings;
    messages: Array<{ role: string; content: string }>;
  }): Promise<ApiResponse<PurgePreviewResult>> =>
    request<ApiResponse<PurgePreviewResult>>('POST', `${API_BASE}/purge/preview`, payload),
  // 破甲预设库（内置 prompt 集合：检索 / 详情 / 应用）
  listPurgePresets: (params?: {
    q?: string;
    vendor?: string;
    /** 默认模型：后端据此给每条附 `model_match`，不筛选、不重排。空白不发送。 */
    model?: string;
    limit?: number;
  }): Promise<ApiResponse<PurgePresetList>> => {
    const search = new URLSearchParams();
    if (params?.q) search.set('q', params.q);
    if (params?.vendor) search.set('vendor', params.vendor);
    if (params?.model) search.set('model', params.model);
    if (params?.limit) search.set('limit', String(params.limit));
    const qs = search.toString();
    return request<ApiResponse<PurgePresetList>>(
      'GET',
      `${API_BASE}/purge/presets${qs ? `?${qs}` : ''}`,
    );
  },
  getPurgePreset: (id: string): Promise<ApiResponse<PurgePresetDetail>> =>
    request<ApiResponse<PurgePresetDetail>>(
      'GET',
      `${API_BASE}/purge/presets/${encodeURIComponent(id)}`,
    ),
  applyPurgePreset: (
    id: string,
    target: PurgePresetApplyTarget,
    activate = false,
  ): Promise<ApiResponse<PurgePresetApplyResult>> =>
    request<ApiResponse<PurgePresetApplyResult>>(
      'POST',
      `${API_BASE}/purge/presets/${encodeURIComponent(id)}/apply`,
      { target, activate },
    ),
  // 破甲技能包导入（multipart 上传 ZIP/TAR/TGZ，后端安全解压到技能目录）
  importPurgeSkills: (
    file: File,
    opts: { name?: string; overwrite?: boolean } = {},
  ): Promise<ApiResponse<PurgeSkillImportResult>> => {
    const form = new FormData();
    form.append('file', file, file.name);
    if (opts.name && opts.name.trim()) form.append('name', opts.name.trim());
    if (opts.overwrite) form.append('overwrite', 'true');
    return request<ApiResponse<PurgeSkillImportResult>>(
      'POST',
      `${API_BASE}/purge/skills/import`,
      form,
    );
  },
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
  // 数据管理（对照 Agent2API「设置 → 数据」：存储概览 + 加密备份/恢复）
  getStorageOverview: (): Promise<ApiResponse<StorageOverviewItem>> =>
    request<ApiResponse<StorageOverviewItem>>('GET', `${API_BASE}/storage`),
  /** 生成口令加密备份包（服务端写 backups/ 文件）。口令只经请求体，不落盘不进日志 */
  exportEncryptedBackup: (
    password: string,
    includeLogs: boolean,
  ): Promise<ApiResponse<{ file: string; records: number; bytes: number }>> =>
    request<ApiResponse<{ file: string; records: number; bytes: number }>>(
      'POST',
      `${API_BASE}/backup/export-encrypted`,
      { password, include_logs: includeLogs },
    ),
  /** 下载加密备份包（GET /api/backup/download?file=export-….aigxbak）→ blob */
  downloadBackup: async (file: string): Promise<Blob> => {
    const res = await fetch(
      `${API_BASE}/backup/download?file=${encodeURIComponent(file)}`,
      { headers: authHeaders() },
    );
    if (!res.ok) {
      const text = await res.text().catch(() => '');
      let msg = text;
      try {
        const parsed = JSON.parse(text) as { error?: string; message?: string };
        msg = parsed.error || parsed.message || text;
      } catch {
        // 非 JSON 错误体，直接抛原文
      }
      throw new Error(msg || `下载失败 (HTTP ${res.status})`);
    }
    return res.blob();
  },
  /** 导入加密备份（multipart：file + password）；后端解密全量校验先行再写回 */
  importEncryptedBackup: async (
    file: File,
    password: string,
  ): Promise<ApiResponse<BackupImportResult>> => {
    const form = new FormData();
    form.append('file', file);
    form.append('password', password);
    const res = await fetch(`${API_BASE}/backup/import-encrypted`, {
      method: 'POST',
      headers: authHeaders(),
      body: form,
    });
    const j = (await res.json().catch(() => null)) as
      | {
          success?: boolean;
          data?: BackupImportResult;
          error?: string;
          message?: string;
        }
      | null;
    if (!res.ok || !j?.success) {
      throw new Error(j?.error || j?.message || `导入失败 (HTTP ${res.status})`);
    }
    return { success: true, data: j.data };
  },
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
};
