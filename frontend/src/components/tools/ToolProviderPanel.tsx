// Ported from Agent2API v3.5.0-beta.1 / beta.2 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//
// 搜索提供商面板（工具服务页第二段）—— 对照 A2A tools-config.tsx / tools-detail.tsx：
//   - 四家搜索提供商的密钥管理（只写不回，UI 只显示 configured / key_hint）
//   - **beta.2 多密钥**：按优先级从上到下使用，逐把显示状态（可用/额度耗尽/限流/无效）
//     与上游恢复时间（reset_at）；支持增删、上下移、单密钥 search/fetch 检查
//   - 搜索 / 抓取提供商选择（provider-selection）
//   - 连通性测试
// 契约：GET /api/tools/providers、GET/PUT /api/tools/provider-selection、
//       PUT /api/tools/providers/{provider}（含 keys[]）、DELETE .../key、
//       POST .../test?key_id=&operation=search|fetch。

import { useCallback, useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { CheckCircle2, ChevronDown, ChevronUp, Loader2, Play, Plus, RefreshCw, Trash2, X } from 'lucide-react';
import { api } from '../../api';
import { useToast } from '../Toast';
import { Badge, Button, Card, EmptyState, Input, Switch } from '../ui';
import { toolPayload } from './tools-meta';
import type {
  ProviderKeyRow,
  ProviderKeyStatus,
  ProviderKeyUpdate,
  ToolProviderRow,
  ToolProviderSelection,
  ToolProviderTestResult,
} from '../../types';
import './ToolProviderPanel.css';

/** 翻译函数（允许插值参数，便于模块级 helper 复用） */
type TFn = (key: string, values?: Record<string, string | number>) => string;

/**
 * 管理面视图的**局部**扩展（`types/a2a.ts` 不在本波写入范围，这里按结构补齐）。
 *
 * A2A v3.5.1 的两处形状比 AIGX 的既有类型多几个键：
 *   · selection 多了 `max_global_sources`（20~1000，默认 200）与
 *     `long_query_policy`（truncate / skip / error，默认 truncate）；
 *   · provider 行多了 `supports_max_results` / `max_results` /
 *     `default_max_results` / `max_results_limit`（单次最多返回条数）。
 * 后端这两个端点已按同一形状返回（`tool_services::provider_selection`、
 * `provider_keys::provider_row`），所以这里只补类型、不兜造数据。
 */
type ProviderRowView = ToolProviderRow & {
  supports_max_results?: boolean | null;
  max_results?: number | null;
  default_max_results?: number | null;
  max_results_limit?: number | null;
};

type SelectionView = ToolProviderSelection & {
  max_global_sources?: number;
  long_query_policy?: 'truncate' | 'skip' | 'error';
};

const LONG_QUERY_POLICY_LABEL: Record<'truncate' | 'skip' | 'error', string> = {
  truncate: '自动截断后搜索（推荐）',
  skip: '跳过超限提供商',
  error: '保留报错提示',
};

/** 各家官方 API Key 页面（新标签打开） */
const PROVIDER_KEY_PAGES: Record<string, string> = {
  tavily: 'https://app.tavily.com/home',
  querit: 'https://app.querit.ai/en/dashboard/api-keys',
  firecrawl: 'https://www.firecrawl.app/api-keys',
  ollama: 'https://ollama.com/settings/keys',
};

function fmtTime(value: number | null | undefined): string {
  if (!value || !Number.isFinite(value)) return '—';
  const ms = value < 1e11 ? value * 1000 : value;
  try {
    return new Date(ms).toLocaleString();
  } catch {
    return '—';
  }
}

/**
 * 稳定错误码 → 界面文案（A2A tools-config.tsx `TEST_FAILURE_TEXT` 同表）。
 *
 * 后端回的是机器码（`provider_errors.rs` 的 SearchErrorCode 投影），直接把码
 * 甩到界面上运维看不懂「quota_exhausted 到底是没额度还是密钥错」；表里没有的码
 * 回落到后端附带的 `error` 文本，再空就落到「连接测试失败」。
 */
function failureText(result: ToolProviderTestResult, t: TFn): string {
  const table: Record<string, string> = {
    invalid_provider_key: t('密钥或账号登录态无效'),
    quota_exhausted: t('额度已用完'),
    rate_limited: t('触发上游限流'),
    timed_out: t('上游请求超时'),
    no_results: t('上游没有返回结果'),
    provider_unavailable: t('提供商暂不可用或配置有误'),
    configuration: t('搜索来源配置不完整'),
    permission_denied: t('无权使用所选来源'),
    invalid_input: t('请求参数无效'),
  };
  const code = result.code ? table[result.code] : '';
  return result.error || code || t('连接测试失败');
}

/** 该家是否可用于搜索（A2A `providerReady`：启用 + 已配置 + 密钥/账号可用） */
function providerReady(provider: ToolProviderRow): boolean {
  return provider.enabled && provider.configured && provider.usable !== false;
}

/** 密钥状态文案（对齐 A2A providerKeyStatusText） */
function keyStatusText(status: ProviderKeyStatus, t: TFn): string {
  switch (status.status) {
    case 'quota_exhausted':
      return t('额度已用完');
    case 'rate_limited':
      return t('暂时限流');
    case 'invalid_provider_key':
      return t('密钥无效');
    default:
      return t('可用');
  }
}

/** 恢复时间说明（额度耗尽/限流才给；无效/可用不给） */
function recoveryText(status: ProviderKeyStatus, t: TFn): string {
  if (status.status === 'ready' || status.status === 'invalid_provider_key') return '';
  return status.reset_at
    ? t('恢复时间：{{time}}', { time: fmtTime(status.reset_at) })
    : t('上游未提供恢复时间');
}

/** 单维度（搜索 / 抓取）状态徽标 + 恢复时间 */
function KeyStatusLine({ status, label, t }: { status: ProviderKeyStatus; label?: string; t: TFn }) {
  const ok = status.status === 'ready';
  return (
    <div className={ok ? 'tpp-key-status is-ok' : 'tpp-key-status is-warn'}>
      <span>
        {label ? `${label} · ` : ''}
        {keyStatusText(status, t)}
      </span>
      {recoveryText(status, t) ? <span className="tpp-key-recovery">{recoveryText(status, t)}</span> : null}
    </div>
  );
}

/** 一把密钥的搜索/抓取状态（相同则合并一行，否则分两行） */
function KeyStates({ row, t }: { row: ProviderKeyRow; t: TFn }) {
  const same =
    row.search.status === row.fetch.status &&
    row.search.reset_at === row.fetch.reset_at &&
    row.search.scope === row.fetch.scope;
  if (same) return <KeyStatusLine status={row.search} t={t} />;
  return (
    <>
      <KeyStatusLine label={t('搜索')} status={row.search} t={t} />
      <KeyStatusLine label={t('抓取')} status={row.fetch} t={t} />
    </>
  );
}

/** 多密钥清单（只读展示：优先级 / hint / 状态 / 当前优先使用标记） */
function ProviderKeyList({ provider, t }: { provider: ToolProviderRow; t: TFn }) {
  const keys = provider.keys ?? [];
  return (
    <div className="tpp-keys">
      <div className="tpp-keys-head">
        {t('可用密钥：{{available}} / {{total}}', { available: provider.available_key_count ?? 0, total: keys.length })}
      </div>
      {keys.map((row) => (
        <div key={row.id} className="tpp-key-row">
          <div className="tpp-key-row-head">
            <strong>{t('优先级 {{index}}', { index: row.priority })}</strong>
            <code className="tpp-key-hint">{row.key_hint}</code>
            {row.id === provider.active_key_id ? <Badge tone="success">{t('搜索优先使用')}</Badge> : null}
            {row.id === provider.active_fetch_key_id && row.id !== provider.active_key_id ? (
              <Badge tone="info">{t('抓取优先使用')}</Badge>
            ) : null}
          </div>
          <KeyStates row={row} t={t} />
        </div>
      ))}
    </div>
  );
}

/** 编辑草稿行（新增行 localId；已存行带 id / row） */
interface KeyDraft {
  localId: string;
  id?: string;
  apiKey: string;
  row?: ProviderKeyRow;
}

function draftsFromProvider(provider: ToolProviderRow): KeyDraft[] {
  return (provider.keys ?? []).map((row) => ({ localId: row.id, id: row.id, apiKey: '', row }));
}

type KeyTestState = Record<string, { busy?: boolean; result?: ToolProviderTestResult }>;

/**
 * ToolProviderPanel — 搜索提供商密钥与选择。
 *
 * 密钥「只写不回」：后端只回 configured / key_hint，输入框留空 = 不修改。
 * 选择（search_providers / fetch_provider）改完显式保存，避免一次输入就改配置。
 * 多密钥：provider.keys 存在时展示密钥清单与编辑器（按序使用、逐把检查）。
 */
export default function ToolProviderPanel(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();

  const [providers, setProviders] = useState<ProviderRowView[]>([]);
  const [selection, setSelection] = useState<SelectionView | null>(null);
  const [loading, setLoading] = useState(true);
  const [notReady, setNotReady] = useState(false);
  const [error, setError] = useState('');
  const [keyDrafts, setKeyDrafts] = useState<Record<string, string>>({});
  /** 账号型来源的绑定草稿：provider → account_id（未改动时缺省 = 后端当前绑定） */
  const [accountDrafts, setAccountDrafts] = useState<Record<string, string>>({});
  /** 单次最多返回条数的编辑草稿：provider → 文本（空串 = 清空回默认） */
  const [maxResultsDrafts, setMaxResultsDrafts] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState<string | null>(null);
  const [testResults, setTestResults] = useState<Record<string, ToolProviderTestResult>>({});
  /** 多密钥编辑器：provider → 草稿行（null = 编辑器关闭） */
  const [editors, setEditors] = useState<Record<string, KeyDraft[] | null>>({});
  /** 单密钥检查在途状态：`${provider}:${keyId}:${operation}` */
  const [keyTesting, setKeyTesting] = useState<string | null>(null);
  const [keyTests, setKeyTests] = useState<KeyTestState>({});
  const nextLocalId = useRef(0);

  const load = useCallback(async (): Promise<void> => {
    setLoading(true);
    try {
      const [providersRes, selectionRes] = await Promise.all([
        api.listToolProviders(),
        api.getToolProviderSelection(),
      ]);
      setProviders(toolPayload<ProviderRowView[]>(providersRes) ?? []);
      const rawSelection = toolPayload<SelectionView>(selectionRes) ?? null;
      // 后端给的是真值；缺省键（更老的后端）按 A2A 默认值补齐，避免界面空着
      setSelection(
        rawSelection
          ? {
              ...rawSelection,
              max_global_sources: rawSelection.max_global_sources ?? 200,
              long_query_policy: rawSelection.long_query_policy ?? 'truncate',
            }
          : null,
      );
      setNotReady(false);
      setError('');
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      const is404 = /404|not found/i.test(msg);
      setNotReady(is404);
      setError(is404 ? '' : msg);
      setProviders([]);
      setSelection(null);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const saveKey = async (provider: ToolProviderRow): Promise<void> => {
    const apiKey = (keyDrafts[provider.provider] ?? '').trim();
    if (!apiKey) {
      addToast(t('请先填写 API Key'), 'error');
      return;
    }
    setBusy(provider.provider);
    try {
      await api.saveToolProvider(provider.provider, { api_key: apiKey });
      setKeyDrafts((prev) => ({ ...prev, [provider.provider]: '' }));
      addToast(t('密钥已保存'));
      await load();
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    } finally {
      setBusy(null);
    }
  };

  const clearKey = async (provider: ToolProviderRow): Promise<void> => {
    setBusy(provider.provider);
    try {
      await api.clearToolProviderKey(provider.provider);
      addToast(t('密钥已清除'));
      await load();
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    } finally {
      setBusy(null);
    }
  };

  const toggleProvider = async (provider: ToolProviderRow, enabled: boolean): Promise<void> => {
    setBusy(provider.provider);
    try {
      await api.saveToolProvider(provider.provider, { enabled });
      await load();
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
      await load();
    } finally {
      setBusy(null);
    }
  };

  /**
   * 保存账号型来源的绑定（A2A `saveAccount`）。
   *
   * 账号型来源（`auth_type === 'account'`，如 Kiro / WorkBuddy）不存密钥，改存
   * 「复用哪一个已有账号的登录态」；后端 `update_provider` 对这类来源只接受
   * `account_id`/`enabled`，传 api_key 会直接 400（"该搜索来源使用已有账号"）。
   * 传空串 = 解除绑定（后端把 account_id 清成 ""）。
   */
  const saveAccount = async (provider: ToolProviderRow): Promise<void> => {
    const accountId = accountDrafts[provider.provider] ?? provider.account_id ?? '';
    setBusy(provider.provider);
    try {
      await api.saveToolProvider(provider.provider, { account_id: accountId || null });
      addToast(accountId ? t('搜索账号已绑定') : t('已解除账号绑定'));
      await load();
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    } finally {
      setBusy(null);
    }
  };

  const testProvider = async (provider: ToolProviderRow): Promise<void> => {
    setBusy(provider.provider);
    try {
      const res = await api.testToolProvider(provider.provider);
      const result = toolPayload<ToolProviderTestResult>(res);
      if (result) {
        setTestResults((prev) => ({ ...prev, [provider.provider]: result }));
        addToast(
          result.success
            ? t('测试通过 · {{n}} 条结果 · {{ms}}ms', {
                n: result.result_count ?? 0,
                ms: result.duration_ms ?? 0,
              })
            : failureText(result, t),
          result.success ? 'success' : 'error',
        );
      }
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    } finally {
      setBusy(null);
    }
  };

  // ── 多密钥编辑器 ──────────────────────────────
  const openEditor = (provider: ToolProviderRow): void => {
    setEditors((prev) => ({ ...prev, [provider.provider]: draftsFromProvider(provider) }));
  };
  const closeEditor = (provider: ToolProviderRow): void => {
    setEditors((prev) => ({ ...prev, [provider.provider]: null }));
  };
  const editorOf = (provider: ToolProviderRow): KeyDraft[] | null => editors[provider.provider] ?? null;
  const patchDrafts = (provider: ToolProviderRow, next: KeyDraft[]): void => {
    setEditors((prev) => ({ ...prev, [provider.provider]: next }));
  };
  const moveDraft = (provider: ToolProviderRow, index: number, offset: -1 | 1): void => {
    const drafts = editorOf(provider);
    if (!drafts) return;
    const next = [...drafts];
    const other = index + offset;
    if (other < 0 || other >= next.length) return;
    [next[index], next[other]] = [next[other], next[index]];
    patchDrafts(provider, next);
  };
  const saveKeyList = async (provider: ToolProviderRow): Promise<void> => {
    const drafts = editorOf(provider);
    if (!drafts) return;
    const keys: ProviderKeyUpdate[] = drafts.map((d) => ({
      id: d.id,
      api_key: d.apiKey.trim() || undefined,
    }));
    setBusy(provider.provider);
    try {
      await api.saveToolProvider(provider.provider, { keys });
      addToast(t('密钥列表已保存'));
      closeEditor(provider);
      await load();
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    } finally {
      setBusy(null);
    }
  };

  /** 单密钥检查（search / fetch）；成功后回读刷新状态 */
  const testSingleKey = async (
    provider: ToolProviderRow,
    keyId: string,
    operation: 'search' | 'fetch',
  ): Promise<void> => {
    const token = `${provider.provider}:${keyId}:${operation}`;
    setKeyTesting(token);
    try {
      const res = await api.testToolProvider(provider.provider, { keyId, operation });
      const result = toolPayload<ToolProviderTestResult>(res);
      if (result) {
        setKeyTests((prev) => ({ ...prev, [`${provider.provider}:${keyId}`]: { result } }));
        addToast(
          result.success
            ? t('{{op}}检查通过', { op: operation === 'fetch' ? t('抓取') : t('搜索') })
            : result.error || t('检查失败'),
          result.success ? 'success' : 'error',
        );
        await load();
      }
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    } finally {
      setKeyTesting(null);
    }
  };

  const toggleSearch = (providerId: string): void => {
    if (!selection) return;
    const set = new Set(selection.search_providers);
    if (set.has(providerId)) set.delete(providerId);
    else set.add(providerId);
    setSelection({ ...selection, search_providers: [...set] });
  };

  const saveSelection = async (): Promise<void> => {
    if (!selection) return;
    // 与后端同一条值域（20~1000）：先在前端挡，错误消息才指得出是哪个字段
    const maxGlobal = Number(selection.max_global_sources ?? 200);
    if (!Number.isInteger(maxGlobal) || maxGlobal < 20 || maxGlobal > 1000) {
      addToast(t('单次汇总总条数上限需在 20～1000 之间'), 'error');
      return;
    }
    setBusy('selection');
    try {
      const body: SelectionView = {
        search_providers: selection.search_providers,
        fetch_provider: selection.fetch_provider,
        max_global_sources: maxGlobal,
        long_query_policy: selection.long_query_policy ?? 'truncate',
      };
      await api.saveToolProviderSelection(body);
      addToast(t('提供商选择已保存'));
      await load();
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    } finally {
      setBusy(null);
    }
  };

  /**
   * 保存一家的「单次最多返回条数」（A2A `saveProvider` 的 max_results 分支）。
   *
   * 空串 = 清空（提交 `null` → 后端回落到该来源默认值）；否则必须落在
   * `1..=max_results_limit`。账号型来源的边界来自插件申报，同样用这个区间。
   */
  const saveMaxResults = async (provider: ProviderRowView): Promise<void> => {
    const raw = (maxResultsDrafts[provider.provider] ?? '').trim();
    const upper = provider.max_results_limit ?? 100;
    let value: number | null = null;
    if (raw !== '') {
      const parsed = Number(raw);
      if (!Number.isInteger(parsed) || parsed < 1 || parsed > upper) {
        addToast(t('单次最多返回条数需在 1～{{max}} 之间', { max: upper }), 'error');
        return;
      }
      value = parsed;
    }
    setBusy(provider.provider);
    try {
      // `max_results` 是三态键（缺省不改 / null 清空 / 数字设定）；`api.saveToolProvider`
      // 的声明参数还没带这个键（`api/endpoints/misc.ts` 不在本次写入范围），
      // 这里按同一 PUT 的实参形状局部放宽，不新增第二条请求路径。
      await api.saveToolProvider(provider.provider, { max_results: value } as unknown as {
        api_key?: string;
        account_id?: string | null;
        enabled?: boolean;
        keys?: Array<{ id?: string; api_key?: string }>;
      });
      addToast(t('单次最多返回条数已保存'));
      await load();
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    } finally {
      setBusy(null);
    }
  };

  if (loading) {
    return (
      <Card title={t('上游提供商配置')}>
        <div className="tpp-loading">
          <Loader2 size={14} className="tpp-spin" />
          {t('正在读取提供商…')}
        </div>
      </Card>
    );
  }

  if (notReady) {
    return (
      <Card title={t('上游提供商配置')}>
        <EmptyState icon="🔎" message={t('搜索提供商接口尚未就绪')} />
      </Card>
    );
  }

  const searchCandidates = providers.filter((p) => p.supports_search !== false);
  const fetchCandidates = providers.filter((p) => p.supports_fetch !== false);

  return (
    <Card
      title={t('上游提供商配置')}
      actions={
        <Button variant="outline" size="sm" onClick={() => void load()} disabled={loading}>
          <RefreshCw size={13} /> {t('刷新')}
        </Button>
      }
    >
      {error && <div className="error-message">{error}</div>}

      {/* 就绪聚合（A2A WebSearchConfig 的两条 alert）：所选来源里有几家真的能用。
          只提示不拦保存——停用/未配置的来源照样能选上，运行时按名单跳过，
          所以这里说「哪些会被跳过」比禁掉勾选更有用。 */}
      {selection ? (() => {
        const selected = providers.filter((p) => selection.search_providers.includes(p.provider));
        const ready = selected.filter(providerReady);
        const unavailable = selected.filter((p) => !providerReady(p));
        if (!selected.length) {
          return (
            <p className="tpp-key-help">
              {t('还没有选择搜索来源：批量网页搜索会直接报「搜索来源配置不完整」。')}
            </p>
          );
        }
        return (
          <p className={`tpp-key-help${ready.length ? '' : ' is-warn'}`}>
            {ready.length
              ? t('批量网页搜索可用：将使用 {{ready}}。', {
                  ready: ready.map((p) => p.display_name || p.provider).join('、'),
                }) +
                (unavailable.length
                  ? ` ${t('暂不可用的 {{missing}} 会自动跳过。', {
                      missing: unavailable.map((p) => p.display_name || p.provider).join('、'),
                    })}`
                  : '')
              : t('批量网页搜索不可用：请启用来源并配置密钥或绑定可用账号。')}
          </p>
        );
      })() : null}

      <div className="tpp-list">
        {providers.map((provider) => {
          const result = testResults[provider.provider];
          const isBusy = busy === provider.provider;
          const drafts = editorOf(provider);
          const multiKey = Array.isArray(provider.keys);
          const isAccount = provider.auth_type === 'account';
          const maxKeys = provider.max_keys ?? 100;
          return (
            <div key={provider.provider} className={`tpp-row${provider.enabled ? '' : ' is-off'}`}>
              <div className="tpp-row-head">
                <div className="tpp-name">
                  <strong>{provider.display_name || provider.provider}</strong>
                  <Badge tone={provider.configured ? 'success' : 'neutral'}>
                    {provider.configured ? t('已配置') : t('未配置')}
                  </Badge>
                  {provider.auth_type === 'account' ? <Badge tone="info">{t('账号态')}</Badge> : null}
                  {provider.usable === false && provider.enabled ? (
                    <span title={provider.unavailable_reason || undefined}>
                      <Badge tone="warning">{t('不可用')}</Badge>
                    </span>
                  ) : null}
                </div>
                <Switch
                  checked={provider.enabled}
                  disabled={isBusy}
                  onChange={(v) => void toggleProvider(provider, v)}
                  ariaLabel={`${t('启用')} ${provider.display_name}`}
                />
              </div>

              <div className="tpp-row-meta">
                {isAccount ? (
                  <span>
                    {t('搜索账号')}{' '}
                    <strong>{provider.account_name || t('未选择账号')}</strong>
                  </span>
                ) : (
                  <span>
                    {t('密钥')}{' '}
                    {provider.configured ? (
                      <code>{provider.key_hint || '••••••'}</code>
                    ) : (
                      t('未设置')
                    )}
                  </span>
                )}
                <span>{t('更新于 {{time}}', { time: fmtTime(provider.updated_at) })}</span>
                {provider.max_query_chars ? (
                  <span>{t('单次查询上限 {{n}} 字符', { n: provider.max_query_chars })}</span>
                ) : null}
                {isAccount ? (
                  <span>{t('仅搜索，不支持网页抓取')}</span>
                ) : PROVIDER_KEY_PAGES[provider.provider] ? (
                  <a href={PROVIDER_KEY_PAGES[provider.provider]} target="_blank" rel="noreferrer noopener">
                    {t('获取 API Key')}
                  </a>
                ) : null}
              </div>

              {/* 不可用原因（账号停用 / 插件未装载 / 密钥全失效）：A2A 把它压在
                  提供商标头下，这里跟「更新于」同一行之后独占一行，长文本不挤。 */}
              {provider.usable === false && provider.unavailable_reason ? (
                <p className="tpp-key-help is-warn">{provider.unavailable_reason}</p>
              ) : null}

              {/* 多密钥清单（beta.2）：provider.keys 存在且非空才展示 */}
              {!isAccount && multiKey && (provider.keys?.length ?? 0) > 0 ? (
                <ProviderKeyList provider={provider} t={t} />
              ) : null}

              {/* 账号型来源：绑定已有账号（复用登录态，不填密钥） */}
              {isAccount ? (
                <div className="tpp-row-edit">
                  <select
                    className="form-input tpp-fetch-select"
                    value={accountDrafts[provider.provider] ?? provider.account_id ?? ''}
                    onChange={(e) =>
                      setAccountDrafts((prev) => ({ ...prev, [provider.provider]: e.target.value }))
                    }
                    disabled={isBusy}
                    aria-label={t('搜索账号')}
                  >
                    <option value="">{t('未选择账号')}</option>
                    {(provider.accounts ?? [])
                      .filter((account) => account.enabled || account.id === provider.account_id)
                      .map((account) => (
                        <option key={account.id} value={account.id}>
                          {account.name || account.id}
                          {account.enabled ? '' : t('（已停用）')}
                        </option>
                      ))}
                  </select>
                  <Button variant="outline" size="sm" disabled={isBusy} onClick={() => void saveAccount(provider)}>
                    {isBusy ? <Loader2 size={12} className="tpp-spin" /> : null}
                    {t('保存绑定')}
                  </Button>
                  <Button variant="primary" size="sm" disabled={isBusy} onClick={() => void testProvider(provider)}>
                    {isBusy ? <Loader2 size={12} className="tpp-spin" /> : <Play size={12} />}
                    {t('测试')}
                  </Button>
                </div>
              ) : null}

              {/* 单密钥快捷输入（单密钥后端 / 追加一把新密钥共用） */}
              {!isAccount ? (
                <div className="tpp-row-edit">
                <Input
                  type="password"
                  placeholder={provider.configured ? t('输入新 Key 覆盖（留空 = 不修改）') : t('粘贴 API Key')}
                  value={keyDrafts[provider.provider] ?? ''}
                  onChange={(e) =>
                    setKeyDrafts((prev) => ({ ...prev, [provider.provider]: e.target.value }))
                  }
                  disabled={isBusy}
                />
                <Button variant="outline" size="sm" disabled={isBusy} onClick={() => void saveKey(provider)}>
                  {isBusy ? <Loader2 size={12} className="tpp-spin" /> : null}
                  {t('保存密钥')}
                </Button>
                <Button
                  variant="outline"
                  size="sm"
                  disabled={isBusy || !provider.configured}
                  onClick={() => void clearKey(provider)}
                >
                  <Trash2 size={12} /> {t('清除')}
                </Button>
                <Button variant="primary" size="sm" disabled={isBusy} onClick={() => void testProvider(provider)}>
                  {isBusy ? <Loader2 size={12} className="tpp-spin" /> : <Play size={12} />}
                  {t('测试')}
                </Button>
                {multiKey ? (
                  drafts ? (
                    <Button variant="outline" size="sm" disabled={isBusy} onClick={() => closeEditor(provider)}>
                      <X size={12} /> {t('关闭编辑')}
                    </Button>
                  ) : (
                    <Button variant="outline" size="sm" disabled={isBusy} onClick={() => openEditor(provider)}>
                      {t('多密钥管理')}
                    </Button>
                  )
                ) : null}
              </div>
              ) : null}

              {/* 单次最多返回条数（A2A ProviderDialog 的 max_results 字段）：
                  只有来源申报了边界才出现；空串 = 清空回默认值。 */}
              {provider.supports_max_results ? (
                <div className="tpp-row-edit">
                  <Input
                    type="number"
                    min={1}
                    max={provider.max_results_limit ?? 100}
                    placeholder={
                      provider.default_max_results != null
                        ? t('默认 {{count}}', { count: provider.default_max_results })
                        : t('不填按来源默认')
                    }
                    value={
                      maxResultsDrafts[provider.provider] ??
                      (provider.max_results != null ? String(provider.max_results) : '')
                    }
                    disabled={isBusy}
                    aria-label={t('单次最多返回条数')}
                    onChange={(e) =>
                      setMaxResultsDrafts((prev) => ({ ...prev, [provider.provider]: e.target.value }))
                    }
                  />
                  <Button variant="outline" size="sm" disabled={isBusy} onClick={() => void saveMaxResults(provider)}>
                    {t('保存条数')}
                  </Button>
                  <span className="tpp-key-help">
                    {t('范围 1～{{max}}，留空恢复默认', { max: provider.max_results_limit ?? 100 })}
                  </span>
                </div>
              ) : null}

              {/* 多密钥编辑器：按序使用、可上下移 / 移除 / 新增 / 单密钥检查 */}
              {drafts ? (
                <div className="tpp-key-editor">
                  <p className="tpp-key-help">
                    {t('按列表从上到下使用密钥。额度用完后自动切换；恢复时间到了，下次请求会重新优先使用排在前面的密钥。')}
                  </p>
                  {drafts.map((draft, index) => {
                    const testKey = draft.id ? keyTests[`${provider.provider}:${draft.id}`]?.result : undefined;
                    const canTest = Boolean(draft.id) && !draft.apiKey.trim();
                    return (
                      <div key={draft.localId} className="tpp-key-draft">
                        <div className="tpp-key-draft-head">
                          <span className="tpp-key-prio">{t('优先级 {{index}}', { index: index + 1 })}</span>
                          {draft.row ? <code className="tpp-key-hint">{draft.row.key_hint}</code> : null}
                          <div className="tpp-key-draft-actions">
                            <Button
                              variant="outline"
                              size="sm"
                              disabled={isBusy || index === 0}
                              onClick={() => moveDraft(provider, index, -1)}
                              aria-label={t('上移密钥')}
                            >
                              <ChevronUp size={12} /> {t('上移')}
                            </Button>
                            <Button
                              variant="outline"
                              size="sm"
                              disabled={isBusy || index === drafts.length - 1}
                              onClick={() => moveDraft(provider, index, 1)}
                              aria-label={t('下移密钥')}
                            >
                              <ChevronDown size={12} /> {t('下移')}
                            </Button>
                            <Button
                              variant="outline"
                              size="sm"
                              disabled={isBusy}
                              onClick={() => patchDrafts(provider, drafts.filter((_, i) => i !== index))}
                            >
                              <Trash2 size={12} /> {t('移除')}
                            </Button>
                          </div>
                        </div>
                        <Input
                          type="password"
                          autoComplete="new-password"
                          value={draft.apiKey}
                          disabled={isBusy}
                          aria-label={t('优先级 {{index}} 的 API Key', { index: index + 1 })}
                          placeholder={draft.id ? t('输入新密钥；留空保持不变') : t('输入 API Key')}
                          onChange={(e) => {
                            const value = e.target.value;
                            patchDrafts(
                              provider,
                              drafts.map((d, i) => (i === index ? { ...d, apiKey: value } : d)),
                            );
                          }}
                        />
                        {canTest && draft.row ? (
                          <>
                            <KeyStates row={draft.row} t={t} />
                            <div className="tpp-key-test-btns">
                              <Button
                                variant="outline"
                                size="sm"
                                disabled={isBusy || Boolean(keyTesting)}
                                onClick={() => void testSingleKey(provider, draft.id as string, 'search')}
                              >
                                {keyTesting === `${provider.provider}:${draft.id}:search` ? (
                                  <Loader2 size={12} className="tpp-spin" />
                                ) : null}
                                {t('检查搜索')}
                              </Button>
                              {draft.row.fetch.status !== 'ready' && draft.row.fetch.scope !== 'all' ? (
                                <Button
                                  variant="outline"
                                  size="sm"
                                  disabled={isBusy || Boolean(keyTesting)}
                                  onClick={() => void testSingleKey(provider, draft.id as string, 'fetch')}
                                >
                                  {keyTesting === `${provider.provider}:${draft.id}:fetch` ? (
                                    <Loader2 size={12} className="tpp-spin" />
                                  ) : null}
                                  {t('检查抓取')}
                                </Button>
                              ) : null}
                            </div>
                            {testKey ? (
                              <div className={`tpp-test ${testKey.success ? 'is-ok' : 'is-fail'}`}>
                                {testKey.success ? (
                                  <>
                                    <CheckCircle2 size={12} /> {t('通过')}
                                    {testKey.duration_ms != null ? ` · ${testKey.duration_ms}ms` : ''}
                                  </>
                                ) : (
                                  <>
                                    {t('失败')}：{failureText(testKey, t)}
                                  </>
                                )}
                              </div>
                            ) : null}
                          </>
                        ) : null}
                      </div>
                    );
                  })}
                  <div className="tpp-key-editor-foot">
                    <Button
                      variant="outline"
                      size="sm"
                      disabled={isBusy || drafts.length >= maxKeys}
                      onClick={() =>
                        patchDrafts(provider, [
                          ...drafts,
                          { localId: `new-${nextLocalId.current++}`, apiKey: '' },
                        ])
                      }
                    >
                      <Plus size={12} /> {t('添加密钥')}
                    </Button>
                    <Button variant="primary" size="sm" disabled={isBusy} onClick={() => void saveKeyList(provider)}>
                      {isBusy ? <Loader2 size={12} className="tpp-spin" /> : null}
                      {t('保存密钥列表')}
                    </Button>
                  </div>
                  <p className="tpp-key-help">
                    {t('已保存的密钥留空保持不变，调整顺序或移除后点击保存生效。')}
                  </p>
                </div>
              ) : null}

              {result && !drafts && (
                <div className={`tpp-test ${result.success ? 'is-ok' : 'is-fail'}`}>
                  {result.success ? (
                    <>
                      <CheckCircle2 size={12} /> {t('通过')}
                      {result.result_count != null ? ` · ${t('{{n}} 条结果', { n: result.result_count })}` : ''}
                      {result.duration_ms != null ? ` · ${result.duration_ms}ms` : ''}
                    </>
                  ) : (
                    <>
                      {t('失败')}：{failureText(result, t)}
                    </>
                  )}
                </div>
              )}
            </div>
          );
        })}
        {providers.length === 0 && <EmptyState icon="🔎" message={t('后端没有列出任何搜索提供商')} />}
      </div>

      {selection && (
        <div className="tpp-selection">
          <h4>{t('提供商选择')}</h4>
          <div className="tpp-selection-row">
            <span className="tpp-selection-label">{t('搜索引擎（可多选）')}</span>
            <div className="tpp-chips">
              {searchCandidates.map((p) => (
                <button
                  key={p.provider}
                  type="button"
                  className={`tpp-chip${selection.search_providers.includes(p.provider) ? ' is-on' : ''}`}
                  onClick={() => toggleSearch(p.provider)}
                  disabled={busy !== null}
                >
                  {p.display_name || p.provider}
                </button>
              ))}
            </div>
          </div>
          <div className="tpp-selection-row">
            <span className="tpp-selection-label">{t('网页抓取')}</span>
            <select
              className="form-input tpp-fetch-select"
              value={selection.fetch_provider}
              onChange={(e) => setSelection({ ...selection, fetch_provider: e.target.value })}
              aria-label={t('网页抓取提供商')}
            >
              <option value="">—</option>
              {fetchCandidates.map((p) => (
                <option key={p.provider} value={p.provider}>
                  {p.display_name || p.provider}
                </option>
              ))}
            </select>
          </div>
          {/* 两个搜索行为参数（A2A v3.5.1 WebSearchConfig 同两个字段、同提示文案） */}
          <div className="tpp-selection-row">
            <span className="tpp-selection-label">{t('单次汇总总条数上限')}</span>
            <Input
              type="number"
              min={20}
              max={1000}
              value={String(selection.max_global_sources ?? 200)}
              disabled={busy !== null}
              aria-label={t('单次汇总总条数上限')}
              onChange={(e) =>
                setSelection({ ...selection, max_global_sources: Number(e.target.value) })
              }
            />
            <span className="tpp-key-help">
              {t('单次调用去重合并后最多保留的来源数（20～1000，默认 200）。')}
            </span>
          </div>
          <div className="tpp-selection-row">
            <span className="tpp-selection-label">{t('搜索词超长处理')}</span>
            <select
              className="form-input tpp-fetch-select"
              value={selection.long_query_policy ?? 'truncate'}
              disabled={busy !== null}
              aria-label={t('搜索词超长处理')}
              onChange={(e) =>
                setSelection({
                  ...selection,
                  long_query_policy: e.target.value as 'truncate' | 'skip' | 'error',
                })
              }
            >
              {(['truncate', 'skip', 'error'] as const).map((policy) => (
                <option key={policy} value={policy}>
                  {t(LONG_QUERY_POLICY_LABEL[policy])}
                </option>
              ))}
            </select>
            <span className="tpp-key-help">
              {t('超出提供商字数限制（如 Kiro 200 字、Tavily 400 字、Firecrawl 500 字）时的策略。')}
            </span>
          </div>
          <div className="tpp-selection-actions">
            <Button variant="primary" size="sm" disabled={busy !== null} onClick={() => void saveSelection()}>
              {busy === 'selection' ? <Loader2 size={12} className="tpp-spin" /> : null}
              {t('保存选择')}
            </Button>
          </div>
        </div>
      )}
    </Card>
  );
}
