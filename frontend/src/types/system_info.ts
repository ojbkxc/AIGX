/**
 * 系统信息（GET /api/system-info）的类型定义。
 *
 * 字段对齐 Agent2API v3.5.0-beta.3 `api/system_info.rs` 的 camelCase 形状；
 * 分节全部可选 —— 后端某个分项失败时该节可能降级/缺省，页面按「—」渲染。
 */

export interface SystemInfoApp {
  version?: string;
  license?: string;
  sourceUrl?: string;
  licenseUrl?: string;
  copyright?: string;
  /** 更新是否包含预发布版本（AIGX 没有 stable/beta 双通道，只有这个开关） */
  includePrerelease?: boolean;
}

export interface SystemInfoRuntime {
  /** foreground / docker / systemd / windows-service */
  deployment?: string;
  container?: boolean;
  os?: string;
  osVersion?: string;
  arch?: string;
  startedAt?: number;
  uptimeSeconds?: number;
  pid?: number;
  debugBuild?: boolean;
}

export interface SystemInfoDeployment {
  host?: string;
  port?: number;
  adminBind?: string | null;
  configFile?: string;
  dataDir?: string;
  logDir?: string;
  /** sqlite / postgres */
  dbBackend?: string;
  dbFile?: string | null;
  dbBytes?: number | null;
  dbAvailable?: boolean | null;
  exePath?: string;
  uiHosted?: boolean;
  selfUpdate?: boolean;
}

export interface SystemInfoSecurity {
  adminRegistered?: boolean;
  apiKeyCount?: number;
  dataPlaneKeyRequired?: boolean;
  trustProxyHeaders?: boolean;
  allowInternalUpstream?: boolean;
}

export interface SystemInfoFeatures {
  debugMode?: boolean;
  logBody?: boolean;
  sanitizeFingerprints?: boolean;
  cors?: boolean;
  autoContinue?: boolean;
  autoContinueMaxRounds?: number;
  captchaEnabled?: boolean;
  updateBeta?: boolean;
}

export interface SystemInfoStats {
  accounts?: number;
  activeAccounts?: number;
  channels?: number;
  enabledChannels?: number;
  modelAliases?: number;
  apiKeys?: number;
  requests?: number;
  inFlight?: number;
}

export interface SystemInfo {
  app?: SystemInfoApp;
  runtime?: SystemInfoRuntime;
  deployment?: SystemInfoDeployment;
  security?: SystemInfoSecurity;
  features?: SystemInfoFeatures;
  stats?: SystemInfoStats;
}
