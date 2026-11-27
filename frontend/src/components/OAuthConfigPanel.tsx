import { useCallback, useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import {
  AlertTriangle,
  CheckCircle2,
  Copy,
  ExternalLink,
  Eye,
  EyeOff,
  Info,
  KeyRound,
  LogIn,
} from 'lucide-react';
import { api } from '../api';
import {
  OAUTH_PROVIDERS,
  STATE_PLACEHOLDER,
  authorizeUrlPreview,
  authorizeUrlPreviewWarnings,
  buildOauthUpdate,
  classifyCallback,
  deriveSiteOrigin,
  emptyDraft,
  longCallbackUrl,
  readiness,
  secretConfigured,
  shortCallbackUrl,
  type OauthDraft,
  type OauthProviderMeta,
} from '../api/endpoints/oauth';
import { copyText } from '../lib/clipboard';
import { useToast } from './Toast';
import ConfirmDialog, { type ConfirmState } from './ConfirmDialog';
import type {
  OauthConfig,
  OauthProviderConfig,
  OauthProviderKey,
  OauthProviderUpdate,
} from '../types/oauth';
import './OAuthConfigPanel.css';

/** 站点根地址的本地记忆键：API 与前端不同域时用户要改一次，不该每次重填 */
const SITE_ORIGIN_KEY = 'aigx.oauth.siteOrigin';

const PROVIDER_KEYS: OauthProviderKey[] = ['github', 'google', 'linuxdo'];

function defaultSiteOrigin(): string {
  try {
    const saved = window.localStorage.getItem(SITE_ORIGIN_KEY);
    if (saved && saved.trim()) return saved.trim();
  } catch {
    // localStorage 在隐私模式/被禁用时会抛，退回当前源
  }
  if (typeof window === 'undefined') return '';
  return window.location.origin;
}

/**
 * 站点地址的取值顺序：**本地记忆 > 已配置 redirect_uri 反推的根 > 当前源**
 * （实际逻辑在 `applyServer`；这里只提供最后一级默认值）。
 *
 * 为什么反推优先于当前源：控制台可能在 `lxseek.com` 而接口在 `api.lxseek.com`，
 * 用当前源拼候选地址会把「当前实际会用」标到用户根本没登记的那条上
 * ——本面板要防的正是这种错位。
 */

/** 任一 provider 反推出的站点根（按 github → google → linuxdo 的顺序取第一个可用的） */
function deriveSiteOriginAny(server: OauthConfig): string | null {
  for (const key of ['github', 'google', 'linuxdo'] as OauthProviderKey[]) {
    const derived = deriveSiteOrigin(server[key].redirect_uri, key);
    if (derived) return derived;
  }
  return null;
}

function initialDrafts(server: OauthConfig | null): Record<OauthProviderKey, OauthDraft> {
  return {
    github: emptyDraft(server?.github),
    google: emptyDraft(server?.google),
    linuxdo: emptyDraft(server?.linuxdo),
  };
}

/**
 * OAuth 登录配置（系统设置 → OAuth 登录）。
 *
 * 这个面板存在的理由：后端 `GET/PUT /api/oauth/config` 一直存在
 * （`src/api/admin/settings.rs:943` / `:980`），但前端一个字段都没接，
 * 管理员在线上找不到 GitHub / Google / LinuxDO 的配置入口。
 *
 * 三条契约决定了界面的写法（详见 `api/endpoints/oauth.ts` 文件头）：
 * 1. `client_secret` 回读是掩码 `"..."` —— 界面**留空表示不修改**，靠「不发送该字段」
 *    实现；显式清除要单独按钮（后端会把空串真的写进去）。
 * 2. 没有 `enabled` 开关，「启用」就是 client_id + client_secret 都有值（后端 `ready()`）。
 * 3. 回调地址有两套等价路径（长 `/api/auth/{provider}/callback`、短 `/oauth/{provider}`），
 *    后端照配置原样发给平台 —— 界面必须把「当前实际会用的那条」标出来。
 */
export default function OAuthConfigPanel(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();

  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState('');
  const [server, setServer] = useState<OauthConfig | null>(null);
  const [drafts, setDrafts] = useState<Record<OauthProviderKey, OauthDraft>>(() =>
    initialDrafts(null),
  );
  /** 用户显式点了「清除已保存的密钥」的 provider */
  const [clearSecret, setClearSecret] = useState<Set<OauthProviderKey>>(new Set());
  /** 「清除已保存的密钥」二次确认（不可撤销，误点一次即失效） */
  const [clearConfirm, setClearConfirm] = useState<{ key: OauthProviderKey; label: string } | null>(null);
  /** 小眼睛：只控制**新输入**的可见性，掩码原值永远不显示 */
  const [revealSecret, setRevealSecret] = useState<Set<OauthProviderKey>>(new Set());
  const [siteOrigin, setSiteOrigin] = useState<string>(() => defaultSiteOrigin());

  const applyServer = useCallback((data: OauthConfig) => {
    setServer(data);
    setDrafts(initialDrafts(data));
    setClearSecret(new Set());
    // 站点根优先取「配置里那串」反推出来的：这样「当前实际会用」才会落在用户
    // 真的登记过的那条上。用户手动填过的地址（localStorage 有值）优先级更高——
    // 那是更强的意图，不该被后端快照盖掉。
    let remembered: string | null = null;
    try {
      remembered = window.localStorage.getItem(SITE_ORIGIN_KEY);
    } catch {
      remembered = null;
    }
    if (remembered && remembered.trim()) return;
    const derived = deriveSiteOriginAny(data);
    if (derived) setSiteOrigin(derived);
  }, []);

  useEffect(() => {
    void (async () => {
      try {
        const res = await api.getOauthConfig();
        applyServer(res?.data ?? emptyServerConfig());
      } catch (err) {
        setError(err instanceof Error ? err.message : String(err));
      } finally {
        setLoading(false);
      }
    })();
  }, [applyServer]);

  const setDraft = (key: OauthProviderKey, patch: Partial<OauthDraft>): void => {
    setDrafts((prev) => ({ ...prev, [key]: { ...prev[key], ...patch } }));
  };

  const toggleSet = (
    setter: (fn: (prev: Set<OauthProviderKey>) => Set<OauthProviderKey>) => void,
    key: OauthProviderKey,
  ): void => {
    setter((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  };

  const copy = async (text: string, okMsg: string): Promise<void> => {
    const ok = await copyText(text);
    addToast(ok ? okMsg : t('复制失败：浏览器拒绝了剪贴板访问，请手动选中复制'));
  };

  const onSiteOriginChange = (value: string): void => {
    setSiteOrigin(value);
    try {
      window.localStorage.setItem(SITE_ORIGIN_KEY, value);
    } catch {
      // 记忆失败不影响本次使用
    }
  };

  const pending = useMemo(() => {
    if (!server) return [] as Array<{ key: OauthProviderKey; update: OauthProviderUpdate }>;
    const out: Array<{ key: OauthProviderKey; update: OauthProviderUpdate }> = [];
    for (const key of PROVIDER_KEYS) {
      const update = buildOauthUpdate(drafts[key], server[key], {
        clearSecret: clearSecret.has(key),
      });
      if (update) out.push({ key, update });
    }
    return out;
  }, [server, drafts, clearSecret]);

  const save = async (): Promise<void> => {
    if (!server) return;
    if (pending.length === 0) {
      addToast(t('没有需要保存的改动'));
      return;
    }
    setSaving(true);
    setError('');
    try {
      const body: Partial<Record<OauthProviderKey, OauthProviderUpdate>> = {};
      for (const item of pending) body[item.key] = item.update;
      const res = await api.updateOauthConfig(body);
      applyServer(res?.data ?? server);
      addToast(t('OAuth 配置已保存（下一个登录请求即生效，无需重启）'));
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  };

  if (loading) {
    return <div className="loading">{t('加载中')}</div>;
  }

  return (
    <div className="oauth-panel">
      {error && <div className="error-message">{error}</div>}

      {/* ── 顶部：站点地址 + 两套回调路径的总说明 ── */}
      <div className="card">
        <div className="card-header">
          <h2>{t('OAuth 登录')}</h2>
          <p className="card-subtitle">
            {t(
              'GitHub / Google / LinuxDO 第三方登录的对接参数。「启用」不需要额外开关：client_id 与 client_secret 都填好即为启用。',
            )}
          </p>
        </div>
        <div className="card-body">
          <div className="settings-form">
            <div className="form-group">
              <label htmlFor="oauth-site-origin">{t('站点地址（用于拼回调地址与预览）')}</label>
              <input
                id="oauth-site-origin"
                className="form-input"
                value={siteOrigin}
                onChange={(e) => onSiteOriginChange(e.target.value)}
                placeholder="https://api.example.com"
                spellCheck={false}
              />
              <span className="form-hint">
                {t(
                  '默认取「已配置的 redirect_uri」反推出的站点根（一条都没配时用当前控制台地址）。API 与前端不同域时（例如前端 lxseek.com、接口 api.lxseek.com），这里必须是 API 的地址，否则拼出来的回调地址与平台侧登记的对不上，上面「当前实际会用」也会标错。',
                )}
              </span>
            </div>

            <div className="oauth-callout oauth-callout-info">
              <Info size={16} />
              <div>
                <strong>{t('回调地址有两套路径，都指向同一个后端处理函数：')}</strong>
                <ul className="oauth-callout-list">
                  <li>
                    <code>{t('长：{site}/api/auth/{provider}/callback')}</code>
                    {' — '}
                    {t('历史形态，既有部署与文档都登记它')}
                  </li>
                  <li>
                    <code>{t('短：{site}/oauth/{provider}')}</code>
                    {' — '}
                    {t('v1.0.123 新增的别名，与长路径完全等价')}
                  </li>
                </ul>
                <p>
                  {t(
                    '后端把「这里填的 redirect_uri」原样发给平台，所以平台侧登记的那一串必须与这里逐字符一致（协议、域名、路径、结尾斜杠都算）。',
                  )}
                </p>
              </div>
            </div>

            <div className="oauth-callout oauth-callout-warn">
              <AlertTriangle size={16} />
              <div>
                <strong>{t('最容易踩的一步：改完这里，要同步改平台侧的登记。')}</strong>{' '}
                {t(
                  '两边不一致时登录会被平台直接拒绝——GitHub 报 redirect_uri is not associated with this application，Google 报 redirect_uri_mismatch，LinuxDO 同样拒绝回跳。',
                )}
              </div>
            </div>
          </div>
        </div>
      </div>

      {/* ── 三家 provider ── */}
      {OAUTH_PROVIDERS.map((meta) => {
        const cfg = server ? server[meta.key] : emptyServerProvider();
        return (
          <ProviderCard
            key={meta.key}
            meta={meta}
            cfg={cfg}
            draft={drafts[meta.key]}
            siteOrigin={siteOrigin}
            clearSecret={clearSecret.has(meta.key)}
            reveal={revealSecret.has(meta.key)}
            onToggleReveal={() => toggleSet(setRevealSecret, meta.key)}
            onToggleClear={() => {
              if (clearSecret.has(meta.key)) {
                // 取消清除：无害操作，直接执行
                toggleSet(setClearSecret, meta.key);
                return;
              }
              // 勾上「清除」是不可撤销的高危操作（保存后登录入口立即失效），先二次确认
              setClearConfirm({ key: meta.key, label: meta.label });
            }}
            onChange={(patch) => setDraft(meta.key, patch)}
            onCopy={copy}
          />
        );
      })}

      {/* ── 保存（一次性提交所有改动过的 provider）── */}
      <div className="oauth-save-bar">
        <span className="oauth-save-hint">
          {pending.length === 0
            ? t('当前没有改动')
            : t('待保存：{{list}}', {
                list: pending.map((p) => p.key).join(' / '),
              })}
        </span>
        <button
          type="button"
          className="btn btn-primary"
          onClick={() => void save()}
          disabled={saving || pending.length === 0}
        >
          {saving ? t('保存中...') : t('保存更改')}
        </button>
      </div>

      {/* 清除已保存密钥的二次确认（不可撤销，确认后才标记 + 丢弃刚输入的新值） */}
      <ConfirmDialog
        state={
          clearConfirm
            ? ({
                title: t('清除已保存的密钥'),
                message: t(
                  '确定清除 {{provider}} 的 client_secret 吗？保存后对应登录入口会立即失效，且不可撤销。',
                  { provider: clearConfirm.label },
                ),
                confirmText: t('清除'),
                danger: true,
                onConfirm: () => {
                  const k = clearConfirm.key;
                  // 勾上「清除」时把刚输入的新值也丢掉：否则界面上还留着一串被忽略的
                  // 密码（输入框此时是 disabled），用户会以为它会生效。
                  toggleSet(setClearSecret, k);
                  setDraft(k, { client_secret: '' });
                },
              } as ConfirmState)
            : null
        }
        onClose={() => setClearConfirm(null)}
      />
    </div>
  );
}

function emptyServerProvider(): OauthProviderConfig {
  return { client_id: '', client_secret: '', redirect_uri: '', ready: false };
}

function emptyServerConfig(): OauthConfig {
  return {
    github: emptyServerProvider(),
    google: emptyServerProvider(),
    linuxdo: emptyServerProvider(),
  };
}

interface ProviderCardProps {
  meta: OauthProviderMeta;
  cfg: OauthProviderConfig;
  draft: OauthDraft;
  siteOrigin: string;
  clearSecret: boolean;
  reveal: boolean;
  onToggleReveal: () => void;
  onToggleClear: () => void;
  onChange: (patch: Partial<OauthDraft>) => void;
  onCopy: (text: string, okMsg: string) => Promise<void>;
}

function ProviderCard({
  meta,
  cfg,
  draft,
  siteOrigin,
  clearSecret,
  reveal,
  onToggleReveal,
  onToggleClear,
  onChange,
  onCopy,
}: ProviderCardProps): JSX.Element {
  const { t } = useTranslation();
  const key = meta.key;
  const hasSecret = secretConfigured(cfg.client_secret) && !clearSecret;
  const read = readiness(key, draft, hasSecret);
  const shape = classifyCallback(draft.redirect_uri, key);
  const preview = authorizeUrlPreview(key, draft);
  const previewWarn = authorizeUrlPreviewWarnings(draft.redirect_uri);
  const longUrl = longCallbackUrl(siteOrigin, key);
  const shortUrl = shortCallbackUrl(siteOrigin, key);
  const loginEntry = `${siteOrigin.replace(/\/+$/, '')}/api/auth/${key}`;

  const candidates: Array<{ label: string; url: string }> = [
    { label: t('长回调'), url: longUrl },
    { label: t('短回调'), url: shortUrl },
  ];

  return (
    <div className="card oauth-provider" data-provider={key}>
      <div className="card-header oauth-provider-header">
        <div>
          <h2>
            {meta.label}
            <span
              className={`badge ${read.ready ? 'badge-success' : 'badge-neutral'} oauth-status-badge`}
              data-ready={read.ready ? 'true' : 'false'}
            >
              {read.ready ? (
                <>
                  <CheckCircle2 size={12} /> {t('已启用')}
                </>
              ) : (
                t('未启用')
              )}
            </span>
          </h2>
          <p className="card-subtitle">
            {read.ready
              ? t('client_id 与 client_secret 均已配置，用户可在登录页看到 {{provider}} 入口。', {
                  provider: meta.label,
                })
              : t('缺项补齐后即为启用（后端判据：client_id + client_secret 非空）。')}
          </p>
        </div>
        <a
          className="btn btn-outline btn-sm oauth-console-link"
          href={meta.consoleUrl}
          target="_blank"
          rel="noreferrer"
        >
          <ExternalLink size={14} /> {t('到 {{provider}} 创建 OAuth App', { provider: meta.label })}
        </a>
      </div>

      <div className="card-body">
        <div className="settings-form">
          {/* 状态：缺什么 / 有什么隐患 */}
          {(read.missing.length > 0 || read.warnings.length > 0 || clearSecret) && (
            <ul className="oauth-issues">
              {read.missing.map((m) => (
                <li key={`miss-${m}`} className="oauth-issue oauth-issue-error">
                  <AlertTriangle size={13} />
                  <span>{t('缺少：{{item}}', { item: m })}</span>
                </li>
              ))}
              {read.warnings.map((w) => (
                <li key={`warn-${w}`} className="oauth-issue oauth-issue-warn">
                  <AlertTriangle size={13} />
                  <span>{w}</span>
                </li>
              ))}
              {clearSecret && (
                <li className="oauth-issue oauth-issue-error">
                  <AlertTriangle size={13} />
                  <span>
                    {t('保存后将清空该 provider 的 client_secret，对应登录入口会立即失效（且不可撤销）。')}
                  </span>
                </li>
              )}
            </ul>
          )}

          {/* client_id */}
          <div className="form-group">
            <label htmlFor={`oauth-${key}-client-id`}>client_id</label>
            <input
              id={`oauth-${key}-client-id`}
              className="form-input"
              value={draft.client_id}
              onChange={(e) => onChange({ client_id: e.target.value })}
              spellCheck={false}
              autoComplete="off"
              placeholder={meta.label === 'LinuxDO' ? 'WxSdu3yJ...' : 'Ov23lioj...'}
            />
            <span className="form-hint">
              {t('平台控制台里的 Client ID / 应用 ID，明文保存。')}
            </span>
          </div>

          {/* client_secret */}
          <div className="form-group">
            <label htmlFor={`oauth-${key}-client-secret`}>client_secret</label>
            <div className="oauth-secret-row">
              <input
                id={`oauth-${key}-client-secret`}
                className="form-input"
                type={reveal ? 'text' : 'password'}
                value={draft.client_secret}
                onChange={(e) => onChange({ client_secret: e.target.value })}
                spellCheck={false}
                autoComplete="new-password"
                disabled={clearSecret}
                placeholder={
                  hasSecret
                    ? t('已配置（留空表示不修改）')
                    : t('未配置，粘贴平台生成的 Client Secret')
                }
              />
              <button
                type="button"
                className="btn btn-outline btn-sm oauth-icon-btn"
                onClick={onToggleReveal}
                aria-label={reveal ? t('隐藏已输入的内容') : t('显示已输入的内容')}
                title={reveal ? t('隐藏已输入的内容') : t('显示已输入的内容')}
              >
                {reveal ? <EyeOff size={14} /> : <Eye size={14} />}
              </button>
              {secretConfigured(cfg.client_secret) && (
                <button
                  type="button"
                  className={`btn btn-sm ${clearSecret ? 'btn-secondary' : 'btn-outline'}`}
                  onClick={onToggleClear}
                >
                  <KeyRound size={14} />{' '}
                  {clearSecret ? t('取消清除') : t('清除已保存的密钥')}
                </button>
              )}
            </div>
            <span className="form-hint">
              {hasSecret
                ? t(
                    '本接口回读的是掩码（已配置显示为 ...，读不到原值），因此这里留空 = 保持不变；只有真的输入新值才会覆盖。',
                  )
                : t('后端回读永远是掩码，原值只在你这次输入时出现一次，保存后不再可读。')}
            </span>
          </div>

          {/* redirect_uri */}
          <div className="form-group">
            <label htmlFor={`oauth-${key}-redirect-uri`}>redirect_uri</label>
            <input
              id={`oauth-${key}-redirect-uri`}
              className="form-input"
              value={draft.redirect_uri}
              onChange={(e) => onChange({ redirect_uri: e.target.value })}
              spellCheck={false}
              autoComplete="off"
              placeholder={longUrl}
            />
            <div className="oauth-inline-actions">
              <button
                type="button"
                className="btn btn-outline btn-sm"
                onClick={() => void onCopy(draft.redirect_uri, t('redirect_uri 已复制'))}
                disabled={draft.redirect_uri.trim() === ''}
              >
                <Copy size={14} /> {t('复制')}
              </button>
              <span className="oauth-shape-tag" data-shape={shape}>
                {t('当前形态：{{shape}}', {
                  shape:
                    shape === 'long'
                      ? t('长回调 /api/auth/{{provider}}/callback', { provider: key })
                      : shape === 'short'
                        ? t('短回调 /oauth/{{provider}}（v1.0.123 别名）', { provider: key })
                        : shape === 'other'
                          ? t('既不是长回调也不是短回调')
                          : t('未填写'),
                })}
              </span>
            </div>
            <span className="form-hint">
              <AlertTriangle size={12} className="oauth-hint-icon" />
              {t(
                '改完这里，必须同步改平台侧的登记：两边要逐字符一致（协议、域名、路径、结尾斜杠），否则登录会被平台直接拒绝（GitHub 报 redirect_uri is not associated with this application，Google 报 redirect_uri_mismatch）。',
              )}
            </span>
            {previewWarn.map((w) => (
              <span key={w} className="form-hint oauth-hint-danger">
                {w}
              </span>
            ))}
          </div>

          {/* 平台侧要填的回调地址（两套形态，标出当前实际会用的那条） */}
          <div className="oauth-callback-block">
            <div className="oauth-block-title">
              {t('在 {{provider}} 的「{{field}}」里要填的就是下面这一串', {
                provider: meta.label,
                field: meta.callbackFieldName,
              })}
            </div>
            <div className="oauth-block-hint">{meta.consoleHint}</div>
            {candidates.map((c) => {
              const active = draft.redirect_uri.trim() !== '' && draft.redirect_uri.trim() === c.url;
              return (
                <div
                  key={c.label}
                  className={`oauth-callback-row ${active ? 'is-active' : ''}`}
                  data-active={active ? 'true' : 'false'}
                >
                  <span className="oauth-callback-tag">{c.label}</span>
                  <code className="oauth-callback-url">{c.url}</code>
                  {active && (
                    <span className="badge badge-success oauth-active-badge">
                      {t('当前实际会用')}
                    </span>
                  )}
                  <button
                    type="button"
                    className="btn btn-outline btn-sm"
                    onClick={() => void onCopy(c.url, t('回调地址已复制'))}
                  >
                    <Copy size={14} /> {t('复制')}
                  </button>
                  <button
                    type="button"
                    className="btn btn-secondary btn-sm"
                    onClick={() => onChange({ redirect_uri: c.url })}
                    disabled={active}
                  >
                    {active ? t('已填入') : t('填入这里')}
                  </button>
                </div>
              );
            })}
            {shape === 'empty' && (
              <div className="oauth-block-note oauth-hint-danger">
                {t(
                  'redirect_uri 还没填：点上面任一「填入这里」，再去平台侧登记同一串。两套路径只要两边一致就能用，但不要一边一套。',
                )}
              </div>
            )}
            {shape === 'other' && (
              <div className="oauth-block-note oauth-hint-danger">
                {t(
                  '当前填的既不是长回调也不是短回调：后端会把这一串原样发给平台，但平台回跳时打不到 AIGX 的回调路由，登录会失败。',
                )}
              </div>
            )}
            {/** 形态标签在 redirect_uri 旁边已给出；这里补一条「两套路径都有效」的确认 */}
            <div className="oauth-block-note">
              {t(
                '长/短两套路径在后端指向同一个处理函数（长路径在前、短路径是 v1.0.123 别名），任选一套都行；上面带「当前实际会用」标记的那条，就是后端此刻会发给平台的值。',
              )}
            </div>
          </div>

          {/* 只读诊断：后端会发出的 authorize URL */}
          <div className="oauth-diag">
            <div className="oauth-block-title">
              <LogIn size={14} /> {t('只读诊断：后端此刻会发出的授权 URL')}
            </div>
            <div className="oauth-diag-entry">
              {t('登录发起入口')}：<code>{loginEntry}</code>
              <button
                type="button"
                className="btn btn-outline btn-sm"
                onClick={() => void onCopy(loginEntry, t('登录入口已复制'))}
              >
                <Copy size={14} /> {t('复制')}
              </button>
            </div>
            <code className="oauth-diag-url">{preview}</code>
            <div className="oauth-block-note">
              {t(
                'state 是每次发起登录时后端生成的随机 UUID，这里用 {{ph}} 占位。请把 redirect_uri= 后面那一串拿去和平台侧登记逐字符比对——这是排查 redirect_uri is not associated with this application 最直接的办法。',
                { ph: STATE_PLACEHOLDER },
              )}
            </div>
            {key === 'google' && (
              <div className="oauth-block-note">
                {t(
                  'Google 的 scope 取后端默认值 openid email profile（接口不返回 scope，也没有编辑入口），因此预览里按默认值显示。',
                )}
              </div>
            )}
            <div className="oauth-diag-actions">
              <button
                type="button"
                className="btn btn-outline btn-sm"
                onClick={() => void onCopy(preview, t('授权 URL 已复制'))}
              >
                <Copy size={14} /> {t('复制授权 URL')}
              </button>
            </div>
          </div>

          {/* 后端形态说明：这里的「启用」是怎么判的 */}
          <div className="oauth-block-note oauth-muted">
            {t('后端判据：')}
            {key === 'google'
              ? t('client_id、client_secret、redirect_uri 三者都非空才算就绪（Google 的 ready 判据包含 redirect_uri）。')
              : t('client_id 与 client_secret 都非空即为就绪（后端 ready 判据不含 redirect_uri，所以上面单独为它给了告警）。')}
            {' '}
            {t('配置保存在 config.toml 的 [{{section}}] 段，保存后无需重启。', {
              section: `${key}_oauth`,
            })}
          </div>
        </div>
      </div>
    </div>
  );
}
