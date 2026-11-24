// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//
// 视觉分析绑定面板（工具服务页第三段）—— 对照 A2A tools-image-config.tsx /
// tools-config.tsx 的「视觉分析」段：
//   - 绑定 provider + upstream_model + 思考等级 + 内部调用 API Key
//   - 真实调用一次（走网关内部 Key）作为连通性验证
//   - 清除绑定
// 契约：GET/PUT/DELETE /api/tools/vision/config、POST /api/tools/vision/config/test。
// 后端校验「该 provider 下这个模型必须已启用」，所以候选取自模型管理视图。

import { useCallback, useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { CheckCircle2, Eye, Loader2, RefreshCw, Trash2 } from 'lucide-react';
import { api } from '../../api';
import { useToast } from '../Toast';
import { Badge, Button, Card, EmptyState, Select } from '../ui';
import { toolPayload } from './tools-meta';
import type { ManageView, ToolVisionConfig, ToolVisionKeyOption, ToolVisionPayload } from '../../types';
import './ToolVisionPanel.css';

/** 思考等级候选（与上游 is_reasoning_level 一致；空 = 不指定） */
const REASONING_LEVELS = ['', 'low', 'medium', 'high'];

interface ToolVisionPanelProps {
  /** 与页面共享错误展示（不阻断自身渲染） */
  onSharedError?: (message: string) => void;
}

/**
 * ToolVisionPanel — 视觉分析的 provider / model / 思考等级绑定。
 *
 * 只展示**已启用**的模型（后端保存时也按这条判据校验）；模型清单读失败时
 * 下拉为空并提示去模型管理页确认，而不是让用户以为后端坏了。
 */
export default function ToolVisionPanel({ onSharedError }: ToolVisionPanelProps): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();

  const [config, setConfig] = useState<ToolVisionConfig | null>(null);
  const [keys, setKeys] = useState<ToolVisionKeyOption[]>([]);
  const [view, setView] = useState<ManageView | null>(null);
  const [loading, setLoading] = useState(true);
  const [notReady, setNotReady] = useState(false);
  const [error, setError] = useState('');

  const [provider, setProvider] = useState('');
  const [model, setModel] = useState('');
  const [reasoning, setReasoning] = useState('');
  const [apiKeyId, setApiKeyId] = useState('');
  const [saving, setSaving] = useState(false);
  const [testing, setTesting] = useState(false);
  const [testResult, setTestResult] = useState<{ success: boolean; duration_ms?: number; reply?: string; error?: string } | null>(null);

  const load = useCallback(async (): Promise<void> => {
    setLoading(true);
    try {
      const [visionRes, modelRes] = await Promise.all([
        api.getToolVisionConfig(),
        api.listManageModels().catch(() => null),
      ]);
      const payload = toolPayload<ToolVisionPayload>(visionRes);
      setConfig(payload?.config ?? null);
      setKeys(payload?.keys ?? []);
      setView(modelRes?.data ?? null);
      if (payload?.config) {
        setProvider(payload.config.provider);
        setModel(payload.config.upstreamModel);
        setReasoning(payload.config.reasoning ?? '');
        setApiKeyId(payload.config.apiKeyId ?? '');
      }
      setNotReady(false);
      setError('');
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      const is404 = /404|not found/i.test(msg);
      setNotReady(is404);
      setError(is404 ? '' : msg);
      onSharedError?.(is404 ? '' : msg);
    } finally {
      setLoading(false);
    }
  }, [onSharedError]);

  useEffect(() => {
    void load();
  }, [load]);

  /** provider 候选：模型视图里出现过的 provider（去重保序） */
  const providerOptions = useMemo(() => {
    if (!view?.providers?.length) return [];
    return view.providers
      .filter((group) => group.total > 0 && group.enabled > 0)
      .map((group) => ({ id: group.id, label: group.label || group.id }));
  }, [view]);

  /** 当前 provider 下已启用的非图片模型 */
  const modelOptions = useMemo(() => {
    if (!view || !provider) return [];
    return view.models
      .filter((m) => m.provider === provider && m.enabled)
      .map((m) => m.id)
      .sort();
  }, [view, provider]);

  const save = async (): Promise<void> => {
    if (!provider || !model) {
      addToast(t('请选择提供商与上游模型'), 'error');
      return;
    }
    if (!apiKeyId) {
      addToast(t('请选择内部调用 API Key'), 'error');
      return;
    }
    setSaving(true);
    try {
      const res = await api.saveToolVisionConfig({
        provider,
        upstream_model: model,
        ...(reasoning ? { reasoning_effort: reasoning } : {}),
        api_key_id: apiKeyId,
      });
      setConfig(toolPayload<ToolVisionConfig>(res) ?? null);
      addToast(t('视觉分析绑定已保存'));
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      addToast(msg, 'error');
      onSharedError?.(msg);
    } finally {
      setSaving(false);
    }
  };

  const clear = async (): Promise<void> => {
    setSaving(true);
    try {
      await api.clearToolVisionConfig();
      setConfig(null);
      setProvider('');
      setModel('');
      setReasoning('');
      setApiKeyId('');
      setTestResult(null);
      addToast(t('视觉分析绑定已清除'));
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    } finally {
      setSaving(false);
    }
  };

  const test = async (): Promise<void> => {
    setTesting(true);
    try {
      const res = await api.testToolVision();
      const result = toolPayload<{ success: boolean; duration_ms?: number; reply?: string; error?: string }>(res) ?? null;
      setTestResult(result);
      if (result) {
        addToast(
          result.success ? t('视觉绑定可用 · {{ms}}ms', { ms: result.duration_ms ?? 0 }) : result.error || t('视觉绑定测试失败'),
          result.success ? 'success' : 'error',
        );
      }
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    } finally {
      setTesting(false);
    }
  };

  if (loading) {
    return (
      <Card title={t('视觉模型配置')}>
        <div className="tvp-loading">
          <Loader2 size={14} className="tvp-spin" />
          {t('正在读取视觉绑定…')}
        </div>
      </Card>
    );
  }

  if (notReady) {
    return (
      <Card title={t('视觉模型配置')}>
        <EmptyState icon="👁️" message={t('视觉分析接口尚未就绪')} />
      </Card>
    );
  }

  return (
    <Card
      title={t('视觉模型配置')}
      actions={
        <div className="tvp-head-actions">
          {config ? <Badge tone="success">{t('已绑定')}</Badge> : <Badge tone="neutral">{t('未绑定')}</Badge>}
          <Button variant="outline" size="sm" onClick={() => void load()} disabled={loading}>
            <RefreshCw size={13} /> {t('刷新')}
          </Button>
        </div>
      }
    >
      {error && <div className="error-message">{error}</div>}
      <p className="tvp-hint">
        {t('把图片与提示词交给这里配置的视觉模型分析，再把模型回复返回给调用方的大模型。')}
      </p>

      <div className="tvp-form">
        <label className="tvp-field">
          <span className="tvp-label">{t('提供商')}</span>
          <Select
            value={provider}
            onChange={(e) => {
              setProvider(e.target.value);
              setModel('');
            }}
            aria-label={t('提供商')}
          >
            <option value="">—</option>
            {providerOptions.map((p) => (
              <option key={p.id} value={p.id}>
                {p.label}
              </option>
            ))}
          </Select>
        </label>

        <label className="tvp-field">
          <span className="tvp-label">{t('上游模型')}</span>
          <Select value={model} onChange={(e) => setModel(e.target.value)} aria-label={t('上游模型')}>
            <option value="">—</option>
            {modelOptions.map((id) => (
              <option key={id} value={id}>
                {id}
              </option>
            ))}
          </Select>
          {provider && modelOptions.length === 0 ? (
            <span className="tvp-hint-error">{t('该提供商下没有已启用的模型，请先在模型管理页确认')}</span>
          ) : null}
        </label>

        <label className="tvp-field">
          <span className="tvp-label">{t('思考等级')}</span>
          <Select value={reasoning} onChange={(e) => setReasoning(e.target.value)} aria-label={t('思考等级')}>
            {REASONING_LEVELS.map((level) => (
              <option key={level} value={level}>
                {level ? level : t('不指定')}
              </option>
            ))}
          </Select>
        </label>

        <label className="tvp-field">
          <span className="tvp-label">{t('内部调用 API Key')}</span>
          <Select value={apiKeyId} onChange={(e) => setApiKeyId(e.target.value)} aria-label={t('内部调用 API Key')}>
            <option value="">—</option>
            {keys.map((k) => (
              <option key={k.id} value={k.id}>
                {k.name} {k.key_prefix ? `(${k.key_prefix}…)` : ''}
              </option>
            ))}
          </Select>
        </label>
      </div>

      <div className="tvp-actions">
        <Button variant="primary" size="sm" disabled={saving} onClick={() => void save()}>
          {saving ? <Loader2 size={12} className="tvp-spin" /> : null}
          {t('保存绑定')}
        </Button>
        <Button variant="outline" size="sm" disabled={testing || !config} onClick={() => void test()}>
          {testing ? <Loader2 size={12} className="tvp-spin" /> : <Eye size={12} />}
          {t('测试一次')}
        </Button>
        <Button variant="outline" size="sm" disabled={saving || !config} onClick={() => void clear()}>
          <Trash2 size={12} /> {t('清除绑定')}
        </Button>
      </div>

      {config && (
        <div className="tvp-current">
          <span className="tvp-current-label">{t('当前绑定')}</span>
          <span>
            {config.providerLabel || config.provider} · {config.upstreamModel}
            {config.reasoning ? ` · ${config.reasoning}` : ''}
          </span>
          {config.apiKeyName ? <span className="tvp-current-key">{config.apiKeyName}</span> : null}
          {config.key_available === false ? (
            <Badge tone="warning">{t('所选 Key 已不可用')}</Badge>
          ) : null}
        </div>
      )}

      {testResult && (
        <div className={`tvp-test ${testResult.success ? 'is-ok' : 'is-fail'}`}>
          {testResult.success ? (
            <>
              <CheckCircle2 size={12} /> {t('测试通过')}
              {testResult.duration_ms != null ? ` · ${testResult.duration_ms}ms` : ''}
              {testResult.reply ? <div className="tvp-test-reply">{testResult.reply}</div> : null}
            </>
          ) : (
            <>{t('测试失败')}：{testResult.error || '—'}</>
          )}
        </div>
      )}
    </Card>
  );
}