// OAuth 登录配置端点 + 界面所需的纯计算（GitHub / Google / LinuxDO）。
//
// 后端契约（源码为准，勿凭记忆改）：
// - `GET /api/oauth/config`  → `src/api/admin/settings.rs:943` handle_get_oauth_config
// - `PUT /api/oauth/config`  → `src/api/admin/settings.rs:980` handle_update_oauth_config
// - `verify_admin`：非管理员 403（`src/api/admin/common.rs:216-233` 的 update_admin_config
//   与 verify_admin 同口径）——所以整个面板只对管理员可用。
//
// 回调路由（`src/main.rs:1878-1939`）**两套都有效，指向同一个 handler**：
// - 长：`{站点}/api/auth/{provider}/callback`
// - 短：`{站点}/oauth/{provider}`（v1.0.123 新增）
// 登录发起入口只有一个：`{站点}/api/auth/{provider}`（`Login.tsx:276` 即跳这里）。
//
// 本文件里的纯函数（`buildOauthUpdate` / `readiness` / `authorizeUrlPreview` …）
// 与渲染组件分开，便于单测钉住这些「界面写错就会毁掉 secret / 误导回调地址」的判据。
import { API_BASE, request } from '../client';
import type { ApiResponse } from '@/types';
import type {
  OauthCallbackShape,
  OauthConfig,
  OauthConfigUpdate,
  OauthProviderConfig,
  OauthProviderKey,
  OauthProviderUpdate,
} from '@/types/oauth';

export const endpoints = {
  /** 读三家 OAuth 配置（secret 为掩码，`...` = 已配置） */
  getOauthConfig: (): Promise<ApiResponse<OauthConfig>> =>
    request<ApiResponse<OauthConfig>>('GET', `${API_BASE}/oauth/config`),
  /** 写三家 OAuth 配置（省略字段 = 保持原值；`client_secret: ""` = 清空） */
  updateOauthConfig: (body: OauthConfigUpdate): Promise<ApiResponse<OauthConfig>> =>
    request<ApiResponse<OauthConfig>>('PUT', `${API_BASE}/oauth/config`, body),
};

/** provider 的展示元数据 */
export interface OauthProviderMeta {
  key: OauthProviderKey;
  label: string;
  /** 到该平台创建 / 管理 OAuth App 的直达链接 */
  consoleUrl: string;
  /** 平台侧「回调地址」字段的官方叫法（各平台文案不同，写错就填错框） */
  callbackFieldName: string;
  /** 控制台里创建应用的大致路径提示 */
  consoleHint: string;
}

export const OAUTH_PROVIDERS: OauthProviderMeta[] = [
  {
    key: 'github',
    label: 'GitHub',
    consoleUrl: 'https://github.com/settings/developers',
    callbackFieldName: 'Authorization callback URL',
    consoleHint: 'OAuth Apps → New OAuth App（或点已有 App 的 Settings）',
  },
  {
    key: 'google',
    label: 'Google',
    consoleUrl: 'https://console.cloud.google.com/apis/credentials',
    callbackFieldName: 'Authorized redirect URIs',
    consoleHint: '凭据 → 创建凭据 → OAuth 客户端 ID（Web 应用）',
  },
  {
    key: 'linuxdo',
    label: 'LinuxDO',
    // 后端 OAuth 端点全部在 connect.linux.do（`src/oauth/linuxdo.rs:59/93`），
    // 应用创建入口同域；本仓没有更细的控制台路径可引用，故只给到域根。
    consoleUrl: 'https://connect.linux.do',
    callbackFieldName: '回调地址 / Redirect URI',
    consoleHint: 'Connect 开放平台 → 应用管理 → 创建应用',
  },
];

export function providerMeta(key: OauthProviderKey): OauthProviderMeta {
  const found = OAUTH_PROVIDERS.find((p) => p.key === key);
  if (!found) throw new Error(`unknown oauth provider: ${key}`);
  return found;
}

// ── 回调地址：两套形态 ────────────────────────────────────────────────

/** 长回调（历史形态，AIGX 文档与既有部署都登记它） */
export function longCallbackPath(provider: OauthProviderKey): string {
  return `/api/auth/${provider}/callback`;
}

/** 短回调（v1.0.123 别名，同一 handler） */
export function shortCallbackPath(provider: OauthProviderKey): string {
  return `/oauth/${provider}`;
}

/** 去掉尾部斜杠，便于与站点根拼接 */
function trimTrailingSlashes(s: string): string {
  return s.replace(/\/+$/, '');
}

/** `{站点}/api/auth/{provider}/callback` */
export function longCallbackUrl(siteOrigin: string, provider: OauthProviderKey): string {
  return `${trimTrailingSlashes(siteOrigin.trim())}${longCallbackPath(provider)}`;
}

/** `{站点}/oauth/{provider}` */
export function shortCallbackUrl(siteOrigin: string, provider: OauthProviderKey): string {
  return `${trimTrailingSlashes(siteOrigin.trim())}${shortCallbackPath(provider)}`;
}

/**
 * 判定某条已配置的 redirect_uri 属于哪套形态。
 *
 * 只按**路径尾段**认，不管协议/域名：配置里写的是 `https://api.lxseek.com/...`
 * 而控制台在 `https://lxseek.com` 时，用户仍需要看到「你现在配的是长/短回调」，
 * 而不是「不认识」。
 */
export function classifyCallback(redirectUri: string, provider: OauthProviderKey): OauthCallbackShape {
  const raw = redirectUri.trim();
  if (!raw) return 'empty';
  // 去掉 query / fragment 后比较，容忍平台侧多加的参数
  const withoutQuery = raw.split(/[?#]/)[0];
  const normalized = trimTrailingSlashes(withoutQuery);
  if (normalized.endsWith(longCallbackPath(provider))) return 'long';
  if (normalized.endsWith(shortCallbackPath(provider))) return 'short';
  return 'other';
}

/** 形态的展示文案（短形态要标出它是 v1.0.123 新增的别名） */
export const CALLBACK_SHAPE_LABEL: Record<OauthCallbackShape, string> = {
  long: '长回调 /api/auth/<provider>/callback',
  short: '短回调 /oauth/<provider>（v1.0.123 别名）',
  other: '既不是长回调也不是短回调',
  empty: '未填写',
};

/**
 * 从已配置的 redirect_uri 反推站点根。
 *
 * 为什么需要：控制台在 `https://lxseek.com`、API 在 `https://api.lxseek.com` 时，
 * 用 `window.location.origin` 拼出来的候选回调地址是**另一个域**，界面就会把
 * 「当前实际会用」标到一条用户根本没用过的地址上——恰好把本任务要防的事
 * （两边不一致）演示成对的。所以默认站点地址优先取配置里那串的根。
 *
 * 返回 `null` 表示这串不像这两套回调之一（或为空），交给下一个来源。
 */
export function deriveSiteOrigin(
  redirectUri: string,
  provider: OauthProviderKey,
): string | null {
  const raw = redirectUri.trim();
  if (!raw) return null;
  const shape = classifyCallback(raw, provider);
  if (shape !== 'long' && shape !== 'short') return null;
  const path = shape === 'long' ? longCallbackPath(provider) : shortCallbackPath(provider);
  const withoutQuery = trimTrailingSlashes(raw.split(/[?#]/)[0]);
  const idx = withoutQuery.lastIndexOf(path);
  if (idx <= 0) return null;
  return withoutQuery.slice(0, idx);
}

// ── 就绪判据 ─────────────────────────────────────────────────────────

/**
 * 后端 `ready()` 是否额外要求 redirect_uri。
 *
 * 三家**不一致**（后端现状如实反映，不是前端自选口径）：
 * - github `src/oauth/github.rs:18-20`、linuxdo `src/oauth/linuxdo.rs:27-29`：
 *   只要 client_id + client_secret；
 * - google `src/oauth/google.rs:35-39`：还要 redirect_uri。
 *
 * 后果：GitHub / LinuxDO 只要 id+secret 齐了后端就报 ready，但 redirect_uri 为空时
 * 拼出的 authorize URL 里 `redirect_uri=` 是空值，登录在平台侧必然被拒。
 * 界面因此在 `readiness()` 里单独给 redirect_uri 一条**警告**（不是 ready 判据）。
 */
export function readyRequiresRedirectUri(provider: OauthProviderKey): boolean {
  return provider === 'google';
}

/** 表单里的可编辑字段 */
export interface OauthDraft {
  client_id: string;
  /** 用户新输入的 secret；空串 = 不修改（后端契约） */
  client_secret: string;
  redirect_uri: string;
}

/** 后端就绪判据 + 前端补的缺项清单 */
export interface OauthReadiness {
  /** 后端口径的 ready（据此显示绿/灰） */
  ready: boolean;
  /** 缺哪些字段（人话，逐条） */
  missing: string[];
  /** 不阻止 ready、但会让登录失败或难以排查的告警 */
  warnings: string[];
}

export function emptyDraft(config?: OauthProviderConfig): OauthDraft {
  return {
    client_id: config?.client_id ?? '',
    client_secret: '',
    redirect_uri: config?.redirect_uri ?? '',
  };
}

/**
 * 计算某 provider 的就绪状态。
 *
 * 入参是**表单草稿**而不是服务端快照：用户改了 client_id 还没保存时，界面必须跟着变，
 * 否则会出现「显示已就绪、点登录必失败」。
 * `secretConfigured` = 服务端已有 secret（掩码为 `...`）**或** 用户刚输入了新 secret。
 */
export function readiness(
  provider: OauthProviderKey,
  draft: OauthDraft,
  secretConfigured: boolean,
): OauthReadiness {
  const missing: string[] = [];
  const warnings: string[] = [];
  const hasId = draft.client_id.trim() !== '';
  const hasSecret = secretConfigured || draft.client_secret.trim() !== '';
  const hasRedirect = draft.redirect_uri.trim() !== '';

  if (!hasId) missing.push('client_id 为空');
  if (!hasSecret) missing.push('client_secret 为空');
  if (!hasRedirect) {
    if (readyRequiresRedirectUri(provider)) {
      missing.push('redirect_uri 为空（Google 的 ready 判据要求它）');
    } else {
      // 后端 ready 不看它，但 authorize URL 会带一个空的 redirect_uri=
      warnings.push(
        'redirect_uri 为空：后端 ready 判据不拦它，但授权 URL 里 redirect_uri= 会是空值，平台侧会直接拒绝（redirect_uri 缺失 / 未注册）',
      );
    }
  }

  if (hasRedirect) {
    if (!/^https?:\/\//i.test(draft.redirect_uri.trim())) {
      warnings.push('redirect_uri 必须带 http:// 或 https:// 前缀，否则平台侧无法匹配');
    }
    if (classifyCallback(draft.redirect_uri, provider) === 'other') {
      warnings.push(
        '这个 redirect_uri 既不是 /api/auth/<provider>/callback 也不是 /oauth/<provider>：后端虽然会照着发给平台，但回来时打不到 AIGX 的回调路由',
      );
    }
  }

  const ready = missing.length === 0;
  return { ready, missing, warnings };
}

// ── 保存载荷：省略 = 不改，空串 = 清空 ────────────────────────────────

/**
 * 由「表单草稿 + 服务端基线」构造 PUT 载荷。
 *
 * 三条硬规则（都出自后端 `handle_update_oauth_config`，settings.rs:986-1027）：
 * 1. 省略字段 → 后端保持原值（`if let Some(v)`）；
 * 2. `client_secret` 传掩码 `"..."` → 被 `is_masked_secret` 丢弃（等于不修改）；
 * 3. `client_secret` 传 `""` → **真的清空**（空串不命中掩码守卫）。
 *
 * 所以「留空表示不修改」必须靠**不发送该字段**实现，不能靠发送空串。
 * 这也是 `OauthDraft.client_secret` 默认空串的原因：默认状态下 payload 里没有它。
 */
export function buildOauthUpdate(
  draft: OauthDraft,
  baseline: OauthProviderConfig,
  opts: { clearSecret?: boolean } = {},
): OauthProviderUpdate | null {
  const update: OauthProviderUpdate = {};
  const id = draft.client_id.trim();
  const redirect = draft.redirect_uri.trim();
  const secret = draft.client_secret.trim();

  if (id !== baseline.client_id) update.client_id = id;
  if (redirect !== baseline.redirect_uri) update.redirect_uri = redirect;

  if (opts.clearSecret) {
    // 显式清除：允许清空已配置的 secret（后端会写入空串）
    if (baseline.client_secret !== '') update.client_secret = '';
  } else if (secret !== '') {
    // 新输入的 secret 才提交；空串表示「不动」
    update.client_secret = secret;
  }
  return Object.keys(update).length > 0 ? update : null;
}

/** 掩码值判定（与后端 `is_masked_secret` 同语义：`...` 或含 `***`） */
export function isMaskedSecret(v: string): boolean {
  const t = v.trim();
  return t === '...' || v.includes('***');
}

/** 服务端是否已配置 secret（回读只有掩码，非空即已配置） */
export function secretConfigured(serverValue: string): boolean {
  return serverValue.trim() !== '';
}

// ── 只读诊断：后端会发出的 authorize URL ─────────────────────────────

/** 预览里用于占位的 state（真实值是后端 `Uuid::new_v4()`） */
export const STATE_PLACEHOLDER = '<state:uuid-v4>';

/**
 * 拼出「后端实际会发出的 authorize URL」（只读诊断用）。
 *
 * 逐字节对齐后端，**包括一个已知缺陷**：redirect_uri 不做百分号编码，
 * 而是直接插值（`src/api/admin/auth.rs:1237-1240`、`1233-1240` 的 google 走
 * `src/oauth/google.rs:46-54`、linuxdo 走 auth.rs:1254-1257）。所以预览里也**不编码**——
 * 预览若「好看」而实际「难看」，诊断就失去意义。redirect_uri 含 `&` / `?` 之类的
 * 情况由 `authorizeUrlPreviewWarnings()` 另行点名。
 *
 * Google 的 scope 取后端默认值 `openid email profile`（`src/oauth/google.rs:28-30`）：
 * `GET /api/oauth/config` 不返回 scope，AIGX 也没有编辑它的入口，因此只能按默认值展示。
 */
export function authorizeUrlPreview(
  provider: OauthProviderKey,
  draft: OauthDraft,
): string {
  const clientId = draft.client_id.trim();
  const redirect = draft.redirect_uri.trim();
  switch (provider) {
    case 'github':
      // auth.rs:1457-1462（/api/auth/github 的真实实现）
      return `https://github.com/login/oauth/authorize?client_id=${clientId}&redirect_uri=${redirect}&scope=user:email&state=${STATE_PLACEHOLDER}`;
    case 'google':
      // google.rs:47-53（build_authorize_url），scope 默认 openid email profile
      return `https://accounts.google.com/o/oauth2/v2/auth?client_id=${clientId}&redirect_uri=${redirect}&response_type=code&scope=openid email profile&state=${STATE_PLACEHOLDER}`;
    case 'linuxdo':
      // auth.rs:1501（/api/auth/linuxdo 的真实实现）
      return `https://connect.linux.do/oauth2/authorize?response_type=code&client_id=${clientId}&redirect_uri=${redirect}&state=${STATE_PLACEHOLDER}`;
    default:
      throw new Error(`unknown oauth provider: ${String(provider)}`);
  }
}

/**
 * 预览/配置里那些「会让授权 URL 变形」的字符告警。
 *
 * 后端把 redirect_uri 原样拼进 query，所以 `&`、`?`、`#`、空格都会把参数拆断——
 * 这正是「控制台登记正确、AIGX 也填了同一串，登录却被拒」的另一条隐蔽成因。
 */
export function authorizeUrlPreviewWarnings(redirectUri: string): string[] {
  const out: string[] = [];
  const raw = redirectUri.trim();
  if (!raw) return out;
  if (/[&\s]/.test(raw)) {
    out.push('redirect_uri 含 & 或空格：后端不做百分号编码、原样拼进 query，授权 URL 会被拆坏');
  }
  if (raw.includes('?')) {
    out.push('redirect_uri 含 ?：后端不做百分号编码，query 会串位，平台侧匹配失败');
  }
  if (raw.includes('#')) {
    out.push('redirect_uri 含 #：fragment 不会发给服务端，平台侧匹配失败');
  }
  return out;
}
