// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//
// 图片生成配置面板（工具服务页 image-generation 服务卡内）——
// 对照 A2A tools-image-config.tsx：
//   - 生图目标（provider / model / account）+ 参考图编辑目标（可开关）
//   - 单次张数 / 并发 / 保留时长 / 存储上限（limits）
//   - 目录校验错误透传（后端 management_view 的 generation_error/editing_error）
// 契约：GET/PUT/DELETE /api/tools/image-generation/config（管理员）。
// 可选模型/账号目录由同一个 GET 返回（models / accounts），无需另拉模型管理页。

import { useCallback, useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { CheckCircle2, Loader2, RefreshCw, Trash2 } from 'lucide-react';
import { useToast } from '../Toast';
import { Badge, Button, EmptyState, Input, Select, Switch } from '../ui';
import { imageToolApi } from '../../api/endpoints/tools_image';
import ToolImageJobsPanel from './ToolImageJobsPanel';
import {
  emptyImageTarget,
  type ImageAccountOption,
  type ImageConfigView,
  type ImageGenerationConfig,
  type ImageModelOption,
  type ImageTarget,
} from '../../types/tools-image';
import './ToolImagePanel.css';

interface TargetFieldsProps {
  target: ImageTarget;
  view: ImageConfigView;
  editing: boolean;
  disabled: boolean;
  onChange: (target: ImageTarget) => void;
}

/** 生图/编辑目标三连选（provider → model → account）。 */
function TargetFields({ target, view, editing, disabled, onChange }: TargetFieldsProps): JSX.Element {
  const { t } = useTranslation();
  const supported = view.models.filter((model) => (editing ? model.editing : model.generation));

  const providers = useMemo(() => {
    const map = new Map<string, string>();
    for (const model of supported) {
      if (!map.has(model.provider)) map.set(model.provider, model.provider_label || model.provider);
    }
    return [...map.entries()].map(([id, label]) => ({ id, label }));
  }, [supported]);

  const models: ImageModelOption[] = supported.filter((model) => model.provider === target.provider);
  const accounts: ImageAccountOption[] = view.accounts.filter(
    (account) => account.provider === target.provider && account.enabled,
  );

  const staleProvider = target.provider !== '' && !providers.some((item) => item.id === target.provider);
  const staleModel = target.model !== '' && !models.some((item) => item.id === target.model);
  const staleAccount =
    target.account_id !== '' && !accounts.some((item) => item.id === target.account_id);

  return (
    <div className="tic-targets">
      <label className="tic-field">
        <span className="tic-label">{t('上游提供商')}</span>
        <Select
          value={target.provider}
          disabled={disabled}
          aria-label={t('上游提供商')}
          onChange={(e) => onChange({ provider: e.target.value, model: '', account_id: '' })}
        >
          <option value="">{t('请选择提供商')}</option>
          {staleProvider ? (
            <option value={target.provider} disabled>
              {target.provider}（{t('已失效')}）
            </option>
          ) : null}
          {providers.map((item) => (
            <option key={item.id} value={item.id}>
              {item.label}
            </option>
          ))}
        </Select>
      </label>

      <label className="tic-field">
        <span className="tic-label">{editing ? t('编辑模型') : t('生图模型')}</span>
        <Select
          value={target.model}
          disabled={disabled || !target.provider}
          aria-label={editing ? t('编辑模型') : t('生图模型')}
          onChange={(e) => onChange({ ...target, model: e.target.value })}
        >
          <option value="">{t('请选择模型')}</option>
          {staleModel ? (
            <option value={target.model} disabled>
              {target.model}（{t('已失效')}）
            </option>
          ) : null}
          {models.map((model) => (
            <option key={model.id} value={model.id}>
              {model.id}
            </option>
          ))}
        </Select>
      </label>

      <label className="tic-field">
        <span className="tic-label">{t('生图账号')}</span>
        <Select
          value={target.account_id || '__auto__'}
          disabled={disabled || !target.provider}
          aria-label={t('生图账号')}
          onChange={(e) =>
            onChange({ ...target, account_id: e.target.value === '__auto__' ? '' : e.target.value })
          }
        >
          <option value="__auto__">{t('自动选择账号')}</option>
          {staleAccount ? (
            <option value={target.account_id} disabled>
              {target.account_id}（{t('已不可用')}）
            </option>
          ) : null}
          {accounts.map((account) => (
            <option key={account.id} value={account.id}>
              {account.name}
            </option>
          ))}
        </Select>
        {accounts.length === 0 ? (
          <span className="tic-hint">{t('该提供商下没有启用的账号，将按路由自动选号。')}</span>
        ) : null}
      </label>
    </div>
  );
}

/**
 * ToolImageConfigPanel — 图片生成配置 + 最近任务。
 *
 * 后端未就绪（404）→ 空态降级；配置形状或目录语义校验失败时把后端文案原样展示。
 */
export default function ToolImageConfigPanel({
  serviceEnabled = true,
  onSharedError,
}: {
  /** 服务当前启用态：图片生成默认关闭，停用时给出「可先配置、启用后生效」提示 */
  serviceEnabled?: boolean;
  onSharedError?: (message: string) => void;
}): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();

  const [view, setView] = useState<ImageConfigView | null>(null);
  const [draft, setDraft] = useState<ImageGenerationConfig | null>(null);
  const [loading, setLoading] = useState(true);
  const [notReady, setNotReady] = useState(false);
  const [error, setError] = useState('');
  const [notice, setNotice] = useState('');
  const [saving, setSaving] = useState(false);

  const load = useCallback(async (): Promise<void> => {
    setLoading(true);
    try {
      const next = await imageToolApi.getConfig();
      setView(next);
      setDraft(next.config ?? next.defaults);
      setNotReady(false);
      setError('');
      onSharedError?.('');
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

  const setNumber = (
    key: 'max_images' | 'max_concurrency' | 'retention_hours' | 'storage_mb',
    value: string,
  ): void => {
    setDraft((current) => (current ? { ...current, [key]: Number(value) } : current));
  };

  const save = async (): Promise<void> => {
    if (!draft) return;
    if (!draft.generation.provider || !draft.generation.model) {
      addToast(t('请选择生图提供商与模型'), 'error');
      return;
    }
    if (draft.editing && (!draft.editing.provider || !draft.editing.model)) {
      addToast(t('请选择参考图编辑的提供商与模型'), 'error');
      return;
    }
    if (
      draft.max_images < 1 || draft.max_images > 10 ||
      draft.max_concurrency < 1 || draft.max_concurrency > 8 ||
      draft.retention_hours < 1 || draft.retention_hours > 168 ||
      draft.storage_mb < 64 || draft.storage_mb > 4096
    ) {
      addToast(
        t('数量须为 1–10，并发须为 1–8，保留时间须为 1–168 小时，存储须为 64–4096 MiB'),
        'error',
      );
      return;
    }
    setSaving(true);
    setError('');
    setNotice('');
    try {
      const next = await imageToolApi.saveConfig(draft);
      setView(next);
      setDraft(next.config ?? next.defaults);
      setNotice(t('图片生成配置已保存'));
      addToast(t('图片生成配置已保存'));
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      setError(msg);
      onSharedError?.(msg);
    } finally {
      setSaving(false);
    }
  };

  const clear = async (): Promise<void> => {
    setSaving(true);
    setError('');
    setNotice('');
    try {
      await imageToolApi.clearConfig();
      setView((current) => (current ? { ...current, config: null, generation_error: null, editing_error: null } : current));
      setDraft(view?.defaults ?? null);
      setNotice(t('图片生成配置已清除'));
      addToast(t('图片生成配置已清除'));
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      setError(msg);
      onSharedError?.(msg);
    } finally {
      setSaving(false);
    }
  };

  const disableSave = saving || !draft || !draft.generation.model ||
    Boolean(draft.editing && !draft.editing.model);

  if (loading && !view) {
    return (
      <div className="tic-loading">
        <Loader2 size={14} className="tip-spin" />
        {t('正在读取图片生成配置…')}
      </div>
    );
  }

  if (notReady) {
    return <EmptyState icon="🖼️" message={t('图片生成配置接口尚未就绪')} />;
  }

  return (
    <div className="tic-panel">
      <div className="tic-head">
        <div className="tic-head-title">
          <h3>{t('图片生成配置')}</h3>
          {view?.config ? (
            <Badge tone="success">{t('已配置')}</Badge>
          ) : (
            <Badge tone="neutral">{t('未配置')}</Badge>
          )}
        </div>
        <div className="tic-head-actions">
          <Button variant="outline" size="sm" onClick={() => void load()} disabled={loading}>
            {loading ? <Loader2 size={12} className="tip-spin" /> : <RefreshCw size={12} />}
            {t('刷新')}
          </Button>
          <Button variant="outline" size="sm" onClick={() => void clear()} disabled={saving || !view?.config}>
            <Trash2 size={12} /> {t('清除')}
          </Button>
        </div>
      </div>

      <p className="tic-hint">
        {t('复用已有账号生图，不改变 Agent 的对话模型。实际调用遵守接入 Key 的模型和提供商权限。')}
      </p>

      {!serviceEnabled ? (
        <div className="tic-warn">
          {t('该服务当前已停用，客户端的 MCP 调用会收到服务停用错误；配置可先行保存，启用后生效。')}
        </div>
      ) : null}

      {!view?.config && !error ? (
        <div className="tic-warn">{t('尚未配置图片生成，请先选择可用的生图模型并保存。')}</div>
      ) : null}
      {view?.generation_error ? <div className="tic-warn">{view.generation_error}</div> : null}
      {view?.editing_error ? <div className="tic-warn">{view.editing_error}</div> : null}
      {error ? <div className="error-message">{error}</div> : null}
      {notice ? (
        <div className="tic-notice">
          <CheckCircle2 size={12} /> {notice}
        </div>
      ) : null}

      {draft && view ? (
        <>
          <TargetFields
            target={draft.generation}
            view={view}
            editing={false}
            disabled={saving}
            onChange={(generation) => setDraft((current) => (current ? { ...current, generation } : current))}
          />
          <p className="tic-hint">
            {t('自动选择仅在所选提供商内遵守现有路由；固定账号仍受停用、冷却与额度限制，不会自动换号。')}
          </p>
          {view.models.length === 0 ? (
            <div className="tic-warn">
              {t('没有可用的生图模型，请先添加账号、启用对应插件并更新模型目录。')}
            </div>
          ) : null}

          <div className="tic-editing-row">
            <span className="tic-label">{t('参考图编辑')}</span>
            <Switch
              checked={Boolean(draft.editing)}
              disabled={saving}
              ariaLabel={t('参考图编辑')}
              onChange={(checked) =>
                setDraft((current) =>
                  current ? { ...current, editing: checked ? emptyImageTarget() : null } : current,
                )
              }
            />
          </div>
          {draft.editing ? (
            <TargetFields
              target={draft.editing}
              view={view}
              editing
              disabled={saving}
              onChange={(editing) => setDraft((current) => (current ? { ...current, editing } : current))}
            />
          ) : null}
          <p className="tic-hint">
            {t('编辑可使用不同模型；未配置可用编辑模型时，不向 Agent 提供编辑工具。')}
          </p>

          <div className="tic-limits">
            <label className="tic-field">
              <span className="tic-label">{t('单次图片上限')}</span>
              <Input
                type="number"
                min={1}
                max={10}
                step={1}
                value={String(draft.max_images)}
                disabled={saving}
                onChange={(e) => setNumber('max_images', e.target.value)}
              />
            </label>
            <label className="tic-field">
              <span className="tic-label">{t('最大生成并发')}</span>
              <Input
                type="number"
                min={1}
                max={8}
                step={1}
                value={String(draft.max_concurrency)}
                disabled={saving}
                onChange={(e) => setNumber('max_concurrency', e.target.value)}
              />
            </label>
            <label className="tic-field">
              <span className="tic-label">{t('结果保留（小时）')}</span>
              <Input
                type="number"
                min={1}
                max={168}
                step={1}
                value={String(draft.retention_hours)}
                disabled={saving}
                onChange={(e) => setNumber('retention_hours', e.target.value)}
              />
            </label>
            <label className="tic-field">
              <span className="tic-label">{t('存储上限（MiB）')}</span>
              <Input
                type="number"
                min={64}
                max={4096}
                step={1}
                value={String(draft.storage_mb)}
                disabled={saving}
                onChange={(e) => setNumber('storage_mb', e.target.value)}
              />
            </label>
          </div>
          <p className="tic-hint">
            {t('默认只生成一张。空间不足时先拒绝请求，不消耗生图额度；只清理过期结果，已保存到本机目录的文件不受影响。')}
          </p>

          <div className="tic-foot">
            <Button variant="primary" size="sm" disabled={disableSave} onClick={() => void save()}>
              {saving ? <Loader2 size={12} className="tip-spin" /> : null}
              {t('保存')}
            </Button>
            <span className="tic-hint">
              {t('生成较慢时先返回任务编号。断线后查询原任务，不自动重画；超时不代表没有扣费。')}
            </span>
          </div>
        </>
      ) : null}

      <ToolImageJobsPanel />
    </div>
  );
}
