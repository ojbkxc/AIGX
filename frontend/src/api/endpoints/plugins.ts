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
  AlertEvent,
  CacheStatsItem,
  SystemMonitorItem,
  PluginPackSummary,
  PluginsPayload,
  PluginOperation,
  ScriptCatalogPayload,
  ScriptPackageView,
  ScriptPackagesPayload,
  ScriptSettingsView,
  PricingSyncConfigItem,
  ExchangeRatesItem,
  IpFilterConfigItem,
  TotpSetupResult,
  TotpEnableResult,
  TotpDisableResult,
  DashboardItem,
} from '@/types';
import { API_BASE, authHeaders, buildQuery, request } from '../client';

export const endpoints = {
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
  /** 在线安装（下载）商店里的插件包：body {url, sha256?} → data:{pack} */
  installPlugin: (url: string, sha256?: string): Promise<ApiResponse<{ pack: PluginPackSummary }>> =>
    request<ApiResponse<{ pack: PluginPackSummary }>>('POST', `${API_BASE}/plugins/install`, { url, ...(sha256 ? { sha256 } : {}) }),
  /**
   * 从发布目录起一次后台安装/更新（对齐 A2A `install-from-release`）：
   * 服务端解析当前平台下载地址与版本，立即返回初始 Operation（stage=waiting），
   * 进度经 getPluginProgress / 列表 operations 轮询。
   */
  runPluginInstallFromRelease: (body: {
    id: string;
    action?: 'install' | 'update';
    sha256?: string;
    target?: string;
  }): Promise<ApiResponse<{ operation: PluginOperation }>> =>
    request<ApiResponse<{ operation: PluginOperation }>>('POST', `${API_BASE}/plugins/install-from-release`, body),
  /**
   * 从本地 zip 安装（`POST /api/plugins/install-file`，原始字节上传）。
   * 体积上限由后端的路由级 body limit 兜底（`payload.maxPackBytes`，默认 24 MiB）。
   */
  installPluginFile: (file: File): Promise<ApiResponse<{ pack: PluginPackSummary }>> =>
    (async () => {
      const res = await fetch(`${API_BASE}/plugins/install-file`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/octet-stream', ...authHeaders() },
        body: file,
      });
      const text = await res.text();
      let json: { success?: boolean; data?: { pack: PluginPackSummary }; message?: string; error?: string } | null = null;
      try {
        json = text ? JSON.parse(text) : null;
      } catch {
        json = null;
      }
      if (!res.ok || json?.success === false) {
        const message = json?.message || json?.error || text.slice(0, 200) || `安装失败 (HTTP ${res.status})`;
        throw new Error(message);
      }
      return json as ApiResponse<{ pack: PluginPackSummary }>;
    })(),
  /** 检查插件更新（返回 updates 快照） */
  checkPluginUpdates: (): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('GET', `${API_BASE}/plugins/update`),
  /** 刷新发布目录（返回 catalog 快照） */
  refreshPluginRelease: (): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('GET', `${API_BASE}/plugins/release`),

  /**
   * 批量生命周期（beta.2 Operation 异步进度模型）：POST /api/plugins/batch。
   *
   * 两种请求体：
   * - `{items: [{id, action, url?, sha256?, catalog?}]}` —— 逐条完整（install 必须带 url）；
   * - `{action: 'update'|'enable'|'disable'|'uninstall', ids: [...]}` —— 批量简写
   *   （update 时后端从当前目录快照按 id 解析下载地址）。
   * 立即返回初始操作快照（stage=waiting），进度经 getPluginProgress 轮询。
   */
  batchPlugins: (body: {
    items?: Array<{ id: string; action: string; url?: string; sha256?: string; catalog?: unknown }>;
    action?: string;
    ids?: string[];
  }): Promise<ApiResponse<{ operations: PluginOperation[] }>> =>
    request<ApiResponse<{ operations: PluginOperation[] }>>('POST', `${API_BASE}/plugins/batch`, body),
  /** 批量操作进度（GET /api/plugins/progress）—— operations[] 同源快照 */
  getPluginProgress: (): Promise<ApiResponse<{ operations: PluginOperation[] }>> =>
    request<ApiResponse<{ operations: PluginOperation[] }>>('GET', `${API_BASE}/plugins/progress`),

  // ── 脚本插件管理（Magpie 脚本包；一个 Bun 宿主承载全部 npm 风格脚本）──
  // 契约锚点：src/api/admin/script_plugins.rs
  /** 脚本插件列表：宿主状态 + 全部已装包（含 providerRouted） */
  listScriptPlugins: (): Promise<ApiResponse<ScriptPackagesPayload>> =>
    request<ApiResponse<ScriptPackagesPayload>>('GET', `${API_BASE}/plugins/scripts`),
  /** Magpie 目录：状态（三分类计数/阶段时间戳/错误）+ 条目（camelCase） */
  scriptCatalog: (): Promise<ApiResponse<ScriptCatalogPayload>> =>
    request<ApiResponse<ScriptCatalogPayload>>('GET', `${API_BASE}/plugins/scripts/catalog`),
  /** 手动刷新 Magpie 目录（force 无视 TTL；异步后台任务，立即返回） */
  refreshScriptCatalog: (): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('POST', `${API_BASE}/plugins/scripts/refresh`),
  /**
   * 安装脚本包。trusted=true 必须由二次确认弹窗传（脚本不是沙箱，
   * 将以 Bun 进程权限运行）；安装含下载/解包/依赖安装，可能几十秒。
   */
  installScriptPackage: (selector: string, trusted: boolean): Promise<ApiResponse<{ package: ScriptPackageView }>> =>
    request<ApiResponse<{ package: ScriptPackageView }>>('POST', `${API_BASE}/plugins/scripts/install`, { selector, trusted }),
  /** 脚本包启停（触发宿主热切换，失败后端自动回滚） */
  enableScriptPackage: (id: string, enabled: boolean, version?: string): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('POST', `${API_BASE}/plugins/scripts/enable`, { id, ...(version ? { version } : {}), enabled }),
  /**
   * 脚本包内单家 provider 启停（包内 disabled_providers；启用中的包会重建宿主）。
   *
   * 契约：`POST /api/plugins/scripts/change` `{id, action:'provider', provider,
   * enabled}`。**不要**与内置包的 `setPluginProviderEnabled`（`/plugins/provider`）
   * 混用：那个端点只服务内置目录包。
   */
  setScriptProviderEnabled: (id: string, provider: string, enabled: boolean, version?: string): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('POST', `${API_BASE}/plugins/scripts/change`, {
      id,
      action: 'provider',
      provider,
      enabled,
      ...(version ? { version } : {}),
    }),
  /** 卸载脚本包（先停用再删盘） */
  uninstallScriptPackage: (id: string, version?: string): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('POST', `${API_BASE}/plugins/scripts/uninstall`, { id, ...(version ? { version } : {}) }),
  /** 保存脚本包配置（values 为逐键对象；启用中的包会重建宿主让脚本读到新值） */
  saveScriptPackageOptions: (id: string, values: Record<string, unknown>, version?: string): Promise<ApiResponse<{ package: ScriptPackageView }>> =>
    request<ApiResponse<{ package: ScriptPackageView }>>('POST', `${API_BASE}/plugins/scripts/options`, { id, ...(version ? { version } : {}), values }),
  /** 手动重启脚本宿主（排障用；无已启用包时等于关停宿主） */
  rebuildScriptHost: (): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('POST', `${API_BASE}/plugins/scripts/rebuild`),
  /** 检查脚本插件更新（A2A `GET /api/plugins/scripts/check`；与原生包检查一起问） */
  checkScriptUpdates: (): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('GET', `${API_BASE}/plugins/scripts/check`),
  /**
   * 读取脚本包设置视图（`POST /api/plugins/scripts/settings`，只带 id = 只读）：
   * 返回 options / providerOptions / 宿主回填的 settingsSchema。
   */
  getScriptSettings: (id: string, version?: string): Promise<ApiResponse<ScriptSettingsView>> =>
    request<ApiResponse<ScriptSettingsView>>('POST', `${API_BASE}/plugins/scripts/settings`, {
      id,
      ...(version ? { version } : {}),
    }),
  /**
   * 保存脚本包设置：`options`（包级高级参数，变化会重建宿主）与
   * `providerOptions`（逐 provider 开关，运行中实时生效）。两者都是补丁。
   */
  saveScriptSettings: (
    id: string,
    body: { options?: Record<string, unknown>; providerOptions?: Record<string, Record<string, boolean>> },
    version?: string,
  ): Promise<ApiResponse<{ package: ScriptPackageView }>> =>
    request<ApiResponse<{ package: ScriptPackageView }>>('POST', `${API_BASE}/plugins/scripts/settings`, {
      id,
      ...(version ? { version } : {}),
      ...body,
    }),

  // 系统监控
  getSystemMonitor: (): Promise<ApiResponse<SystemMonitorItem>> =>
    request<ApiResponse<SystemMonitorItem>>('GET', `${API_BASE}/monitor/system`),

  // Playground
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
};
