// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//
// 搜索提供商面板（工具服务页第二段）—— 对照 A2A tools-config.tsx / tools-detail.tsx：
//   - 四家搜索提供商的密钥管理（只写不回，UI 只显示 configured / key_hint）
//   - 搜索 / 抓取提供商选择（provider-selection）
//   - 连通性测试
// 契约：GET /api/tools/providers、GET/PUT /api/tools/provider-selection、
//       PUT /api/tools/providers/{provider}、DELETE .../key、POST .../test。

import { useCallback, useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { CheckCircle2, Loader2, Play, RefreshCw, Trash2 } from 'lucide-react';
import { api } from '../../api';
import { useToast } from '../Toast';
import { Badge, Button, Card, EmptyState, Input, Switch } from '../ui';
import type { ToolProviderRow, ToolProviderSelection, ToolProviderTestResult } from '../../types';
import './ToolProviderPanel.css';

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
 * ToolProviderPanel — 搜索提供商密钥与选择。
 *
 * 密钥「只写不回」：后端只回 configured / key_hint，输入框留空 = 不修改。
 * 选择（search_providers / fetch_provider）改完显式保存，避免一次输入就改配置。
 */
export default function ToolProviderPanel(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();

  const [providers, setProviders] = useState<ToolProviderRow[]>([]);
  const [selection, setSelection] = useState<ToolProviderSelection | null>(null);
  const [loading, setLoading] = useState(true);
  const [notReady, setNotReady] = useState(false);
  const [error, setError] = useState('');
  const [keyDrafts, setKeyDrafts] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState<string | null>(null);
  const [testResults, setTestResults] = useState<Record<string, ToolProviderTestResult>>({});

  const load = useCallback(async (): Promise<void> => {
    setLoading(true);
    try {
      const [providersRes, selectionRes] = await Promise.all([
        api.listToolProviders(),
        api.getToolProviderSelection(),
      ]);
      setProviders(providersRes?.data ?? []);
      setSelection(selectionRes?.data ?? null);
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

  const testProvider = async (provider: ToolProviderRow): Promise<void> => {
    setBusy(provider.provider);
    try {
      const res = await api.testToolProvider(provider.provider);
      const result = res?.data;
      if (result) {
        setTestResults((prev) => ({ ...prev, [provider.provider]: result }));
        addToast(
          result.success
            ? t('测试通过 · {{n}} 条结果 · {{ms}}ms', {
                n: result.result_count ?? 0,
                ms: result.duration_ms ?? 0,
              })
            : result.error || t('测试失败'),
          result.success ? 'success' : 'error',
        );
      }
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    } finally {
      setBusy(null);
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
    setBusy('selection');
    try {
      await api.saveToolProviderSelection(selection);
      addToast(t('提供商选择已保存'));
      await load();
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    } finally {
      setBusy(null);
    }
  };

  if (loading) {
    return (
      <Card title={t('搜索提供商')}>
        <div className="tpp-loading">
          <Loader2 size={14} className="tpp-spin" />
          {t('正在读取提供商…')}
        </div>
      </Card>
    );
  }

  if (notReady) {
    return (
      <Card title={t('搜索提供商')}>
        <EmptyState icon="🔎" message={t('搜索提供商接口尚未就绪')} />
      </Card>
    );
  }

  const searchCandidates = providers.filter((p) => p.supports_search !== false);
  const fetchCandidates = providers.filter((p) => p.supports_fetch !== false);

  return (
    <Card
      title={t('搜索提供商')}
      actions={
        <Button variant="outline" size="sm" onClick={() => void load()} disabled={loading}>
          <RefreshCw size={13} /> {t('刷新')}
        </Button>
      }
    >
      {error && <div className="error-message">{error}</div>}

      <div className="tpp-list">
        {providers.map((provider) => {
          const result = testResults[provider.provider];
          const isBusy = busy === provider.provider;
          return (
            <div key={provider.provider} className={`tpp-row${provider.enabled ? '' : ' is-off'}`}>
              <div className="tpp-row-head">
                <div className="tpp-name">
                  <strong>{provider.display_name || provider.provider}</strong>
                  <Badge tone={provider.configured ? 'success' : 'neutral'}>
                    {provider.configured ? t('已配置') : t('未配置')}
                  </Badge>
                  {provider.auth_type === 'account' ? <Badge tone="info">{t('账号态')}</Badge> : null}
                  {provider.available === false ? (
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
                <span>
                  {t('密钥')}{' '}
                  {provider.configured ? (
                    <code>{provider.key_hint || '••••••'}</code>
                  ) : (
                    t('未设置')
                  )}
                </span>
                <span>{t('更新于 {{time}}', { time: fmtTime(provider.updated_at) })}</span>
                {provider.max_query_chars ? (
                  <span>{t('单次查询上限 {{n}} 字符', { n: provider.max_query_chars })}</span>
                ) : null}
                {PROVIDER_KEY_PAGES[provider.provider] ? (
                  <a href={PROVIDER_KEY_PAGES[provider.provider]} target="_blank" rel="noreferrer noopener">
                    {t('获取 API Key')}
                  </a>
                ) : null}
              </div>

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
              </div>

              {result && (
                <div className={`tpp-test ${result.success ? 'is-ok' : 'is-fail'}`}>
                  {result.success ? (
                    <>
                      <CheckCircle2 size={12} /> {t('通过')}
                      {result.result_count != null ? ` · ${t('{{n}} 条结果', { n: result.result_count })}` : ''}
                      {result.duration_ms != null ? ` · ${result.duration_ms}ms` : ''}
                    </>
                  ) : (
                    <>
                      {t('失败')}：{result.error || result.code || '—'}
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