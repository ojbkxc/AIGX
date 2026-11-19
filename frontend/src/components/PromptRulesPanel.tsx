// Ported from Agent2API v3.5.0-beta.2 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//
// 系统提示词规则引擎面板（系统设置 → 网关）—— 对照 A2A ui-shared/settings-prompt-panel.tsx
// + settings-prompt-types.ts：按「匹配规则（从上到下，第一条命中生效）+ 兜底」决定
// 客户端 system 提示词的处理方式（透传 / 替换 / 追加）。
// 参考树通过 window.workbuddyDesktop 桥接；此处改用 AIGX 的 api.* 直连后端：
//   GET /api/prompt/rules、PUT /api/prompt/rules `{config, revision}`、
//   POST /api/prompt/rules/preview `{config, sample}`。
// 端点未就绪（404）时整块降级；规则/条件上限由后端 maxRules/maxConditions 约束。

import { useCallback, useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Loader2, Plus, RefreshCw, Trash2, ArrowUp, ArrowDown } from 'lucide-react';
import { api } from '../api';
import { useToast } from '../components/Toast';
import { Badge, Button, Card, Input, Select } from '../components/ui';

type PromptMode = 'passthrough' | 'custom' | 'append';
type PromptField = 'provider' | 'client' | 'model';
type PromptOperator = 'is' | 'is_not' | 'contains' | 'not_contains' | 'glob' | 'regex';
type PromptLogic = 'all' | 'any';

interface PromptCondition {
  id: string;
  field: PromptField;
  op: PromptOperator;
  values: string[];
}

interface PromptRule {
  id: string;
  name: string;
  enabled: boolean;
  logic: PromptLogic;
  conditions: PromptCondition[];
  mode: PromptMode;
  file: string;
  text: string;
}

interface PromptContent {
  mode: PromptMode;
  file: string;
  text: string;
}

interface PromptConfig {
  rules: PromptRule[];
  fallback: PromptContent;
}

interface PromptView {
  config: PromptConfig;
  revision: string;
  providers: Array<{ id: string; label: string }>;
  clients: Array<{ id: string; label: string }>;
  models: string[];
  maxRules: number;
  maxConditions: number;
  maxValues: number;
  maxTextBytes: number;
  configError: string | null;
}

interface PromptPreview {
  ruleId: string | null;
  index: number | null;
  name: string | null;
  mode: PromptMode;
}

function uid(prefix: string): string {
  const bytes = crypto.getRandomValues(new Uint8Array(9));
  return `${prefix}-${Array.from(bytes, (b) => b.toString(16).padStart(2, '0')).join('')}`;
}

const MODE_LABEL: Record<PromptMode, string> = {
  passthrough: '透传',
  custom: '替换',
  append: '追加',
};

const FIELD_LABEL: Record<PromptField, string> = {
  provider: '提供商',
  client: '客户端',
  model: '模型',
};

const OP_LABEL: Record<PromptOperator, string> = {
  is: '是',
  is_not: '不是',
  contains: '包含',
  not_contains: '不包含',
  glob: '通配匹配',
  regex: '正则匹配',
};

function operatorsOf(field: PromptField): PromptOperator[] {
  return field === 'model'
    ? ['is', 'is_not', 'contains', 'not_contains', 'glob', 'regex']
    : ['is', 'is_not'];
}

/** 把界面草稿（含视图字段）收敛成后端 config 载荷 */
function toPayload(config: PromptConfig): PromptConfig {
  return {
    fallback: { mode: config.fallback.mode, file: config.fallback.file, text: config.fallback.text },
    rules: config.rules.map((r) => ({
      id: r.id,
      name: r.name,
      enabled: r.enabled,
      logic: r.logic,
      conditions: r.conditions.map((c) => ({ id: c.id, field: c.field, op: c.op, values: [...c.values] })),
      mode: r.mode,
      file: r.file,
      text: r.text,
    })),
  };
}

export default function PromptRulesPanel(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();
  const [view, setView] = useState<PromptView | null>(null);
  const [draft, setDraft] = useState<PromptConfig | null>(null);
  const [loading, setLoading] = useState(true);
  const [notReady, setNotReady] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [dirty, setDirty] = useState(false);
  const edited = useRef(false);
  // 预览
  const [sample, setSample] = useState({ provider: '', client: '', model: '' });
  const [preview, setPreview] = useState<PromptPreview | null>(null);

  const adopt = (next: PromptView): void => {
    setView(next);
    setDraft(structuredClone(next.config));
    setDirty(false);
    edited.current = false;
  };

  const load = useCallback(async (): Promise<void> => {
    try {
      const res = await api.getPromptRules();
      const data = (res?.data ?? null) as unknown as PromptView | null;
      if (!data) return;
      adopt(data);
      setNotReady(false);
      setError('');
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      const is404 = /404|not found/i.test(msg);
      setNotReady(is404);
      setError(is404 ? '' : msg);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const patch = (next: PromptConfig): void => {
    setDraft(next);
    setDirty(true);
    edited.current = true;
  };

  const commit = async (): Promise<void> => {
    if (!draft || !view || busy) return;
    setBusy(true);
    setError('');
    try {
      const res = await api.savePromptRules({ config: toPayload(draft), revision: view.revision });
      const data = (res?.data ?? null) as unknown as PromptView | null;
      if (data) adopt(data);
      addToast(t('提示词规则已保存'));
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  };

  const addRule = (): void => {
    if (!draft || !view) return;
    if (draft.rules.length >= view.maxRules) {
      addToast(t('已达规则数量上限（{{n}}）', { n: view.maxRules }), 'error');
      return;
    }
    const rule: PromptRule = {
      id: uid('prompt'),
      name: t('新规则'),
      enabled: true,
      logic: 'all',
      conditions: [{ id: uid('condition'), field: 'provider', op: 'is', values: [] }],
      mode: 'custom',
      file: '',
      text: '',
    };
    patch({ ...draft, rules: [...draft.rules, rule] });
  };

  const updateRule = (index: number, next: PromptRule): void => {
    if (!draft) return;
    patch({ ...draft, rules: draft.rules.map((r, i) => (i === index ? next : r)) });
  };

  const removeRule = (index: number): void => {
    if (!draft) return;
    patch({ ...draft, rules: draft.rules.filter((_, i) => i !== index) });
  };

  const moveRule = (index: number, delta: -1 | 1): void => {
    if (!draft) return;
    const next = [...draft.rules];
    const target = index + delta;
    if (target < 0 || target >= next.length) return;
    [next[index], next[target]] = [next[target], next[index]];
    patch({ ...draft, rules: next });
  };

  const runPreview = async (): Promise<void> => {
    if (!draft || busy) return;
    setBusy(true);
    setError('');
    try {
      const res = await api.previewPromptRules({ config: toPayload(draft), sample });
      const data = (res?.data ?? null) as unknown as PromptPreview | null;
      setPreview(data);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  };

  if (loading) {
    return (
      <Card title={t('系统提示词规则')}>
        <div className="settings-form">
          <Loader2 size={14} className="confirm-dialog-spin" /> {t('加载中…')}
        </div>
      </Card>
    );
  }
  if (notReady) {
    return (
      <Card title={t('系统提示词规则')}>
        <div className="empty-state">
          <p>{t('提示词规则接口尚未就绪')}</p>
        </div>
      </Card>
    );
  }
  if (!draft || !view) {
    return (
      <Card title={t('系统提示词规则')}>
        <div className="empty-state">
          <p>{t('系统提示词设置读取失败')}</p>
        </div>
      </Card>
    );
  }

  return (
    <Card
      title={t('系统提示词规则')}
      actions={
        <Button variant="outline" size="sm" onClick={() => void load()} disabled={busy || dirty}>
          <RefreshCw size={13} /> {t('刷新')}
        </Button>
      }
    >
      <div className="settings-form">
        {error ? <div className="error-message">{error}</div> : null}
        {view.configError ? (
          <p className="form-hint" style={{ color: 'var(--warning-color, #c98a00)' }}>{view.configError}</p>
        ) : null}

        <div style={{ display: 'flex', alignItems: 'center', gap: 12, flexWrap: 'wrap' }}>
          <strong>{t('匹配规则')}</strong>
          <span className="form-hint">{t('从上到下匹配，第一条命中的生效')}</span>
          <Button
            variant="outline"
            size="sm"
            className="ml-auto"
            style={{ marginLeft: 'auto' }}
            disabled={busy || draft.rules.length >= view.maxRules}
            onClick={addRule}
          >
            <Plus size={12} /> {t('新建规则')}
          </Button>
        </div>

        {draft.rules.length === 0 ? (
          <div className="empty-state">
            <p>{t('没有规则，所有请求使用兜底设置。')}</p>
          </div>
        ) : null}

        {draft.rules.map((rule, index) => (
          <RuleEditor
            key={rule.id}
            rule={rule}
            index={index}
            total={draft.rules.length}
            view={view}
            t={t}
            busy={busy}
            onChange={(next) => updateRule(index, next)}
            onRemove={() => removeRule(index)}
            onMove={(delta) => moveRule(index, delta)}
          />
        ))}

        {/* 兜底设置（未命中任何规则时） */}
        <div style={{ border: '1px dashed var(--border-color)', borderRadius: 8, padding: 12 }}>
          <div style={{ display: 'flex', alignItems: 'center', gap: 10, flexWrap: 'wrap' }}>
            <Badge tone="neutral">{t('兜底')}</Badge>
            <span className="form-hint">{t('未命中任何规则')}</span>
            <Select
              style={{ maxWidth: 140 }}
              value={draft.fallback.mode}
              disabled={busy}
              onChange={(e) => patch({ ...draft, fallback: { ...draft.fallback, mode: e.target.value as PromptMode } })}
            >
              {(['custom', 'append', 'passthrough'] as const).map((m) => (
                <option key={m} value={m}>{t(MODE_LABEL[m])}</option>
              ))}
            </Select>
          </div>
          {draft.fallback.mode !== 'passthrough' ? (
            <textarea
              className="form-input"
              rows={4}
              style={{ marginTop: 8 }}
              value={draft.fallback.text}
              disabled={busy}
              placeholder={t('兜底提示词正文（留空 = 回落提示词文件 / 内置默认）')}
              onChange={(e) => patch({ ...draft, fallback: { ...draft.fallback, text: e.target.value } })}
            />
          ) : null}
        </div>

        {/* 命中预览 */}
        <details style={{ borderTop: '1px dashed var(--border-color)', paddingTop: 10 }}>
          <summary style={{ cursor: 'pointer', fontSize: 13 }}>{t('命中预览')}</summary>
          <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fill, minmax(160px, 1fr))', gap: 12, marginTop: 10 }}>
            <div className="form-group">
              <label>{t('提供商')}</label>
              <Input
                value={sample.provider}
                disabled={busy}
                placeholder={view.providers[0]?.id || 'provider'}
                onChange={(e) => setSample({ ...sample, provider: e.target.value })}
              />
            </div>
            <div className="form-group">
              <label>{t('客户端')}</label>
              <Input
                value={sample.client}
                disabled={busy}
                placeholder={view.clients[0]?.id || 'client'}
                onChange={(e) => setSample({ ...sample, client: e.target.value })}
              />
            </div>
            <div className="form-group">
              <label>{t('模型')}</label>
              <Input
                value={sample.model}
                disabled={busy}
                placeholder={view.models[0] || 'model'}
                onChange={(e) => setSample({ ...sample, model: e.target.value })}
              />
            </div>
          </div>
          <div style={{ display: 'flex', alignItems: 'center', gap: 12 }}>
            <Button variant="outline" size="sm" disabled={busy} onClick={() => void runPreview()}>
              {t('预览命中')}
            </Button>
            {preview ? (
              <span className="form-hint">
                {preview.ruleId
                  ? t('命中规则：{{name}}（{{mode}}）', { name: preview.name || preview.ruleId, mode: t(MODE_LABEL[preview.mode]) })
                  : t('未命中任何规则，使用兜底（{{mode}}）', { mode: t(MODE_LABEL[preview.mode]) })}
              </span>
            ) : null}
          </div>
        </details>

        <div className="settings-actions">
          <Button variant="primary" disabled={!dirty || busy} onClick={() => void commit()}>
            {busy ? t('保存中…') : t('保存修改')}
          </Button>
        </div>
      </div>
    </Card>
  );
}

/** 单条规则编辑块：名称 / 启用 / 模式 / 逻辑 / 条件列表 / 正文 */
function RuleEditor({
  rule,
  index,
  total,
  view,
  t,
  busy,
  onChange,
  onRemove,
  onMove,
}: {
  rule: PromptRule;
  index: number;
  total: number;
  view: PromptView;
  t: (key: string, values?: Record<string, string | number>) => string;
  busy: boolean;
  onChange: (next: PromptRule) => void;
  onRemove: () => void;
  onMove: (delta: -1 | 1) => void;
}): JSX.Element {
  const patch = (next: Partial<PromptRule>): void => onChange({ ...rule, ...next });
  const updateCondition = (ci: number, next: Partial<PromptCondition>): void => {
    patch({ conditions: rule.conditions.map((c, i) => (i === ci ? { ...c, ...next } : c)) });
  };
  const addCondition = (): void => {
    if (rule.conditions.length >= view.maxConditions) return;
    patch({
      conditions: [...rule.conditions, { id: uid('condition'), field: 'provider', op: 'is', values: [] }],
    });
  };
  const removeCondition = (ci: number): void => {
    patch({ conditions: rule.conditions.filter((_, i) => i !== ci) });
  };

  return (
    <div style={{ border: '1px solid var(--border-color)', borderRadius: 8, padding: 12, display: 'flex', flexDirection: 'column', gap: 8 }}>
      <div style={{ display: 'flex', alignItems: 'center', gap: 8, flexWrap: 'wrap' }}>
        <span className="form-hint">#{index + 1}</span>
        <Input
          style={{ maxWidth: 220 }}
          value={rule.name}
          disabled={busy}
          placeholder={t('规则名称')}
          onChange={(e) => patch({ name: e.target.value })}
        />
        <label style={{ display: 'flex', alignItems: 'center', gap: 6 }}>
          <input type="checkbox" checked={rule.enabled} disabled={busy} onChange={(e) => patch({ enabled: e.target.checked })} />
          {t('启用')}
        </label>
        <Select
          style={{ maxWidth: 110 }}
          value={rule.mode}
          disabled={busy}
          onChange={(e) => patch({ mode: e.target.value as PromptMode })}
        >
          {(['custom', 'append', 'passthrough'] as const).map((m) => (
            <option key={m} value={m}>{t(MODE_LABEL[m])}</option>
          ))}
        </Select>
        {rule.conditions.length > 1 ? (
          <Select
            style={{ maxWidth: 110 }}
            value={rule.logic}
            disabled={busy}
            aria-label={t('条件逻辑')}
            onChange={(e) => patch({ logic: e.target.value as PromptLogic })}
          >
            <option value="all">{t('满足全部')}</option>
            <option value="any">{t('满足任一')}</option>
          </Select>
        ) : null}
        <div style={{ marginLeft: 'auto', display: 'flex', gap: 4 }}>
          <Button variant="outline" size="sm" disabled={busy || index === 0} onClick={() => onMove(-1)} aria-label={t('上移规则')}>
            <ArrowUp size={12} />
          </Button>
          <Button variant="outline" size="sm" disabled={busy || index === total - 1} onClick={() => onMove(1)} aria-label={t('下移规则')}>
            <ArrowDown size={12} />
          </Button>
          <Button variant="outline" size="sm" disabled={busy} onClick={onRemove}>
            <Trash2 size={12} /> {t('删除')}
          </Button>
        </div>
      </div>

      {/* 条件列表 */}
      {rule.conditions.map((cond, ci) => (
        <div key={cond.id} style={{ display: 'flex', alignItems: 'center', gap: 6, flexWrap: 'wrap' }}>
          <Select
            style={{ maxWidth: 100 }}
            value={cond.field}
            disabled={busy}
            aria-label={t('条件字段')}
            onChange={(e) => {
              const field = e.target.value as PromptField;
              const ops = operatorsOf(field);
              updateCondition(ci, { field, op: ops.includes(cond.op) ? cond.op : ops[0] });
            }}
          >
            {(['provider', 'client', 'model'] as const).map((f) => (
              <option key={f} value={f}>{t(FIELD_LABEL[f])}</option>
            ))}
          </Select>
          <Select
            style={{ maxWidth: 110 }}
            value={cond.op}
            disabled={busy}
            aria-label={t('条件运算符')}
            onChange={(e) => updateCondition(ci, { op: e.target.value as PromptOperator })}
          >
            {operatorsOf(cond.field).map((op) => (
              <option key={op} value={op}>{t(OP_LABEL[op])}</option>
            ))}
          </Select>
          <Input
            style={{ flex: '1 1 200px' }}
            value={cond.values.join(',')}
            disabled={busy}
            placeholder={t('匹配值（逗号分隔）')}
            onChange={(e) =>
              updateCondition(ci, { values: e.target.value.split(',').map((v) => v.trim()).filter(Boolean) })
            }
          />
          {rule.conditions.length > 1 ? (
            <Button variant="outline" size="sm" disabled={busy} onClick={() => removeCondition(ci)}>
              <Trash2 size={12} />
            </Button>
          ) : null}
        </div>
      ))}
      <div>
        <Button variant="outline" size="sm" disabled={busy || rule.conditions.length >= view.maxConditions} onClick={addCondition}>
          <Plus size={12} /> {t('添加条件')}
        </Button>
      </div>

      {rule.mode !== 'passthrough' ? (
        <textarea
          className="form-input"
          rows={3}
          value={rule.text}
          disabled={busy}
          placeholder={t('规则提示词正文（留空 = 回落提示词文件 / 内置默认）')}
          onChange={(e) => patch({ text: e.target.value })}
        />
      ) : null}
    </div>
  );
}
