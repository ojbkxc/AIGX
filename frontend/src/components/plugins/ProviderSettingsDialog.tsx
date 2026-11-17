// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//
// provider 设置表单（schema 驱动）：对照 A2A ui-islands plugins-settings.tsx +
// ui-shared provider-settings-panel.tsx 的信息结构，按 AIGX 自研组件重写。
//
// 契约：GET /api/plugins/provider-settings → data.settings[]（每行一家 provider，
// 带 schema.fields / values / revision）；保存走 PATCH {provider, revision, values}。
// `revision` 是乐观锁：后端发现过期回 409，这里提示用户刷新后重来。

import { useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Loader2, RotateCcw } from 'lucide-react';
import { api } from '../../api';
import { useToast } from '../Toast';
import { Badge, Button, EmptyState, Input, Loading, Select, Switch } from '../ui';
import type { PluginProviderSettingsRow, PluginSettingsField } from '../../types';
import './ProviderSettingsDialog.css';

/** 多语标签取值：字符串直接用；对象按 lang 优先 zh/zh-CN/en，再取第一个非空 */
function pickLocalized(value: unknown, lang: string): string {
  if (typeof value === 'string') return value;
  if (!value || typeof value !== 'object') return '';
  const map = value as Record<string, unknown>;
  const candidates = [lang, lang.split('-')[0], 'zh', 'zh-CN', 'en'];
  for (const key of candidates) {
    const hit = map[key];
    if (typeof hit === 'string' && hit.trim()) return hit;
  }
  for (const hit of Object.values(map)) {
    if (typeof hit === 'string' && hit.trim()) return hit;
  }
  return '';
}

/** 字段值 → 表单草稿（null/undefined 一律空串/空值，保存时再按字段声明归一） */
function draftOf(field: PluginSettingsField, row: PluginProviderSettingsRow): unknown {
  const values = row.values ?? {};
  if (Object.prototype.hasOwnProperty.call(values, field.key)) return values[field.key];
  return row.effective?.[field.key] ?? field.default ?? null;
}

interface ProviderSettingsDialogProps {
  open: boolean;
  onClose: () => void;
  /** 只展示这些 provider（分组里同 owner 的成员）；空 = 全部 */
  providers?: string[];
  onSaved?: () => void;
}

/**
 * ProviderSettingsDialog — 插件 provider 设置表单。
 *
 * 一家 provider 一张卡（同 owner 的多家共用一份设置，后端已按 group 聚合，
 * 这里按 provider 逐张渲染并共享 group 标签）。表单控件按 `field.type` 选：
 * text/password/number/boolean/choice，其余按文本输入兜底。secret 类字段后端
 * 不回值（values 里是 null）——输入框留空 = 不修改。
 */
export default function ProviderSettingsDialog({
  open,
  onClose,
  providers,
  onSaved,
}: ProviderSettingsDialogProps): JSX.Element | null {
  const { t, i18n } = useTranslation();
  const addToast = useToast();
  const lang = i18n.language || 'zh';

  const [rows, setRows] = useState<PluginProviderSettingsRow[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState('');
  const [notReady, setNotReady] = useState(false);
  /** 每家 provider 的草稿：{provider: {fieldKey: value}} */
  const [drafts, setDrafts] = useState<Record<string, Record<string, unknown>>>({});
  const [savingId, setSavingId] = useState<string | null>(null);

  const load = async (): Promise<void> => {
    setLoading(true);
    try {
      const res = await api.getPluginProviderSettings();
      const list = res?.data?.settings ?? [];
      setRows(list);
      setNotReady(false);
      setError('');
      const nextDrafts: Record<string, Record<string, unknown>> = {};
      for (const row of list) {
        const draft: Record<string, unknown> = {};
        for (const field of row.schema?.fields ?? []) draft[field.key] = draftOf(field, row);
        nextDrafts[row.provider] = draft;
      }
      setDrafts(nextDrafts);
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      const is404 = /404|not found/i.test(msg);
      setNotReady(is404);
      setError(is404 ? '' : msg);
      setRows([]);
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    if (!open) return;
    void load();
  }, [open]);

  const visibleRows = useMemo(() => {
    if (!providers?.length) return rows;
    const allowed = new Set(providers);
    return rows.filter((row) => allowed.has(row.provider));
  }, [rows, providers]);

  const setField = (provider: string, key: string, value: unknown): void => {
    setDrafts((prev) => ({ ...prev, [provider]: { ...(prev[provider] ?? {}), [key]: value } }));
  };

  const save = async (row: PluginProviderSettingsRow): Promise<void> => {
    setSavingId(row.provider);
    try {
      const values = drafts[row.provider] ?? {};
      await api.savePluginProviderSettings({ provider: row.provider, revision: row.revision, values });
      addToast(t('设置已保存'));
      onSaved?.();
      await load();
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      addToast(msg, 'error');
      // 409 = 版本过期：刷新回读，用户重填
      if (/409|已变化|refresh/i.test(msg)) await load();
    } finally {
      setSavingId(null);
    }
  };

  const renderField = (row: PluginProviderSettingsRow, field: PluginSettingsField): JSX.Element => {
    const value = drafts[row.provider]?.[field.key];
    const label = pickLocalized(field.label, lang) || field.key;
    const description = pickLocalized(field.description, lang);
    const invalid = row.invalid?.[field.key] === true;
    const override = row.overrides?.[field.key];
    const hint = override
      ? t('由 {{source}} 运行时覆盖', { source: override.source })
      : description;

    if (field.type === 'boolean') {
      return (
        <label key={field.key} className="ps-row ps-row-switch">
          <span className="ps-label">
            {label}
            {override && <Badge tone="info">{t('已覆盖')}</Badge>}
          </span>
          <Switch
            checked={value === true}
            onChange={(next) => setField(row.provider, field.key, next)}
            ariaLabel={label}
          />
          {hint && <span className="ps-hint">{hint}</span>}
        </label>
      );
    }

    if (field.type === 'choice' && field.options?.length) {
      return (
        <label key={field.key} className="ps-row">
          <span className="ps-label">{label}</span>
          <Select
            value={String(value ?? '')}
            onChange={(e) => {
              const hit = field.options?.find((opt) => String(opt.value) === e.target.value);
              setField(row.provider, field.key, hit ? hit.value : e.target.value);
            }}
          >
            <option value="">—</option>
            {field.options.map((opt) => (
              <option key={String(opt.value)} value={String(opt.value)}>
                {pickLocalized(opt.label, lang) || String(opt.value)}
              </option>
            ))}
          </Select>
          {hint && <span className="ps-hint">{hint}</span>}
        </label>
      );
    }

    const isSecret = field.type === 'password' || field.type === 'secret';
    return (
      <label key={field.key} className="ps-row">
        <span className="ps-label">
          {label}
          {isSecret && <Badge tone="warning">{t('只写')}</Badge>}
        </span>
        <Input
          type={isSecret ? 'password' : field.type === 'number' ? 'number' : 'text'}
          value={value == null ? '' : String(value)}
          placeholder={isSecret ? t('留空 = 不修改') : undefined}
          onChange={(e) => setField(row.provider, field.key, e.target.value)}
          aria-invalid={invalid || undefined}
        />
        {invalid ? (
          <span className="ps-hint ps-hint-error">{t('取值不符合插件声明（长度 / 格式 / 范围）')}</span>
        ) : (
          hint && <span className="ps-hint">{hint}</span>
        )}
      </label>
    );
  };

  if (!open) return null;

  return (
    <div className="ps-overlay" role="dialog" aria-modal="true" aria-label={t('插件提供商设置')}>
      <div className="ps-panel">
        <div className="ps-head">
          <div>
            <h2>{t('插件提供商设置')}</h2>
            <p>{t('这些字段由插件声明；保存后立即生效，运行时覆盖项只读。')}</p>
          </div>
          <div className="ps-head-actions">
            <Button variant="outline" size="sm" onClick={() => void load()} disabled={loading}>
              {loading ? <Loader2 size={13} className="ps-spin" /> : <RotateCcw size={13} />}
              {t('刷新')}
            </Button>
            <Button variant="outline" size="sm" onClick={onClose}>
              {t('关闭')}
            </Button>
          </div>
        </div>

        <div className="ps-body">
          {error && <div className="error-message">{error}</div>}
          {loading ? (
            <Loading text={t('读取插件设置')} />
          ) : notReady ? (
            <EmptyState
              icon="⚙️"
              message={t('插件设置接口尚未就绪')}
              action={<p className="ps-empty-hint">{t('后端插件管理面正在补齐（provider-settings）。就绪后本页自动可用。')}</p>}
            />
          ) : visibleRows.length === 0 ? (
            <EmptyState icon="⚙️" message={t('这些插件没有声明可编辑的设置')} />
          ) : (
            visibleRows.map((row) => (
              <section key={row.provider} className="ps-card">
                <header className="ps-card-head">
                  <div>
                    <h3>{pickLocalized(row.schema?.title, lang) || row.label || row.provider}</h3>
                    <p>
                      {row.group?.providers?.length
                        ? `${t('适用于')}：${row.group.providers.map((p) => p.label || p.id).join('、')}`
                        : row.label || row.provider}
                      <span className="ps-rev">rev {row.revision.slice(0, 8)}</span>
                    </p>
                  </div>
                  <Button
                    variant="primary"
                    size="sm"
                    disabled={savingId === row.provider || !!row.error}
                    onClick={() => void save(row)}
                  >
                    {savingId === row.provider ? <Loader2 size={13} className="ps-spin" /> : null}
                    {t('保存')}
                  </Button>
                </header>
                {row.error ? (
                  <p className="ps-card-err">{row.error}</p>
                ) : (
                  <div className="ps-fields">
                    {(row.schema?.fields ?? []).map((field) => renderField(row, field))}
                  </div>
                )}
              </section>
            ))
          )}
        </div>
      </div>
    </div>
  );
}