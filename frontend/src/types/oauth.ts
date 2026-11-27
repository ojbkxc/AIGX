/**
 * OAuth 登录配置（GitHub / Google / LinuxDO）类型。
 *
 * 契约来源（后端，逐字段对齐，勿凭记忆改）：
 * - `GET  /api/oauth/config`  → `src/api/admin/settings.rs:943-971`
 * - `PUT  /api/oauth/config`  → `src/api/admin/settings.rs:980-1052`
 *
 * 响应形如 `{ success, data: { github: {...}, google: {...}, linuxdo: {...} } }`，
 * 三家**同构**，键名固定为 `github` / `google` / `linuxdo`。
 *
 * `client_secret` 回读值是**掩码**：已配置为 `"..."`，未配置为 `""`
 * （`mask_secret`，settings.rs:932-940）。所以读写**不对称**：
 * - 读：只能知道「有没有配过」，拿不到原值；
 * - 写：提交 `"..."` 会被 `is_masked_secret` 守卫丢弃（settings.rs:975-977, 991-995），
 *   即「原样回传 = 不修改」；提交空串 `""` 不命中守卫，会**真的把 secret 清空**。
 *
 * `ready` 是后端算好的就绪判据，但三家口径不同（见 `readyRequiresRedirectUri`）：
 * - github / linuxdo：`client_id` + `client_secret` 非空即就绪（`src/oauth/github.rs:18-20`、
 *   `src/oauth/linuxdo.rs:27-29`）；
 * - google：另外还要求 `redirect_uri` 非空（`src/oauth/google.rs:35-39`）。
 */

/** 三家 provider 的固定键名（后端 match 分支即这三个值） */
export type OauthProviderKey = 'github' | 'google' | 'linuxdo';

/** 单个 provider 的配置快照（GET 响应里的一项） */
export interface OauthProviderConfig {
  client_id: string;
  /** 掩码值：`"..."` = 已配置，`""` = 未配置；**永远不是明文** */
  client_secret: string;
  redirect_uri: string;
  /** 后端 `ready()` 的结果（判据见文件头注释，三家口径不同） */
  ready: boolean;
}

/** `GET /api/oauth/config` 的 data */
export interface OauthConfig {
  github: OauthProviderConfig;
  google: OauthProviderConfig;
  linuxdo: OauthProviderConfig;
}

/** 单个 provider 的写入载荷：**省略某字段 = 保持原值** */
export interface OauthProviderUpdate {
  client_id?: string;
  /** 传 `""` = 清空；传 `"..."` 会被后端守卫丢弃（等于不修改） */
  client_secret?: string;
  redirect_uri?: string;
}

/** `PUT /api/oauth/config` 的请求体 */
export interface OauthConfigUpdate {
  github?: OauthProviderUpdate;
  google?: OauthProviderUpdate;
  linuxdo?: OauthProviderUpdate;
}

/** 回调路径形态：后端长/短两条路由都指向同一个 handler */
export type OauthCallbackShape = 'long' | 'short' | 'other' | 'empty';
