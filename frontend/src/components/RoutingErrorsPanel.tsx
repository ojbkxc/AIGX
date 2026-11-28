// Ported from Agent2API v3.5.0-beta.2 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//
// 错误处理面板（系统设置 → 重试）—— 对照 A2A ui-shared/routing-errors-panel.tsx +
// routing-errors-types.ts：上游失败后的重试上限、换号上限、冷却与失败保护，
// 以及按错误类型（kind）配置的处理动作（切换账号 / 跳过提供商 / 返回错误…）。
// 参考树通过 window.workbuddyDesktop 桥接；此处改用 AIGX 的 api.* 直连后端：
//   GET /api/routing/errors、PUT /api/routing/errors `{settings, revision}`。
// 端点未就绪（404）时整块降级。

import { useCallback, useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Loader2, RefreshCw } from 'lucide-react';
import { api } from '../api';
import { useToast } from '../components/Toast';
import { Button, Card, Input } from '../components/ui';
import { ErrorPreviewPanel, ErrorRules, mergeProviderRules } from './RoutingErrorsFeatures';
import type { ErrorKind, ErrorPreviewInput, ErrorSettings, PluginErrorRule } from './RoutingErrorsFeatures';
import '../components/routing/trace-exec.css';

interface ErrorPolicyView {
  settings: ErrorSettings;
  defaults: ErrorSettings;
  revision: string;
  configured: boolean;
  legacy: boolean;
  providers: Array<{ id: string; label: string }>;
  /** 已启动插件在握手里申报的默认错误规则（可被提供商规则覆盖） */
  providerRules?: PluginErrorRule[];
}

const LIMIT_FIELDS: Array<{ key: keyof ErrorSettings; label: string; min: number; max: number }> = [
  { key: 'resends', label: '原账号重发（次）', min: 0, max: 10 },
  { key: 'switches', label: '最多再换账号（个）', min: 0, max: 100 },
  { key: 'intervalSeconds', label: '重发间隔（秒）', min: 0, max: 300 },
  { key: 'waitBudgetSeconds', label: '累计等待上限（秒）', min: 0, max: 300 },
  { key: 'cooldownFloorSeconds', label: '最短限流冷却（秒）', min: 1, max: 60 },
  { key: 'backoffSeconds', label: '首次休息时间（秒）', min: 1, max: 3600 },
  { key: 'backoffCapSeconds', label: '连续失败最长休息（秒）', min: 1, max: 3600 },
];

function validate(settings: ErrorSettings, t: (k: string, v?: Record<string, string | number>) => string): string | null {
  for (const { key, label, min, max } of LIMIT_FIELDS) {
    const v = settings[key] as number;
    if (!Number.isFinite(v) || v < min || v > max) {
      return t('{{name}}必须是 {{min}}–{{max}} 的整数', { name: t(label), min, max });
    }
  }
  for (const rule of [...settings.rules, ...settings.overrides]) {
    if (!rule.name.trim()) return t('请填写规则名称。');
    if (rule.fallbackCapSeconds < rule.fallbackSeconds) return t('冷却上限不能短于首次冷却时间。');
  }
  return null;
}

export default function RoutingErrorsPanel({
  onDirty,
}: {
  /** 把「有未保存修改」上报给调用方（路由页左栏的路由项要挂黄点，与 A2A errorsDirty 同口径） */
  onDirty?: (dirty: boolean) => void;
} = {}): JSX.Element {
  return (
    <>
      <ErrorsCard onDirty={onDirty} />
      <div style={{ marginTop: 16 }}>
        {/* v1.0.93：原系统设置「重试」「超时」两分类并入（设置菜单对齐 A2A，超时/重试归路由错误处理） */}
        <RetryTimeoutCard />
      </div>
    </>
  );
}

/** 原系统设置「重试」字段（/limits 读写），v1.0.93 迁入本面板。 */
const RETRY_FIELDS: Array<{
  field: 'max_retries' | 'retry_self_count' | 'retry_interval_ms' | 'retry_on_status';
  label: string;
  hint: string;
  type: 'number' | 'text';
  min?: number;
  max?: number;
  placeholder: string;
}> = [
  { field: 'max_retries', label: '最大重试次数（供应商切换）', hint: '供应商级失败切换的最大额外尝试次数（0 = 只尝试首选供应商，最多 10）；保存后立即生效。', type: 'number', min: 0, max: 10, placeholder: '2' },
  { field: 'retry_self_count', label: '同账号重试次数', hint: '账号池内同一账号原地重试的额外次数（0 = 失败即换号，最多 10）；保存后立即生效。换号次数在账号池「账号路由」里配置。', type: 'number', min: 0, max: 10, placeholder: '1' },
  { field: 'retry_interval_ms', label: '重试间隔 (毫秒)', hint: '账号池换号重试之间的等待时间，0 = 立即换号（保存后立即生效）。', type: 'number', min: 0, max: 10000, placeholder: '0' },
  { field: 'retry_on_status', label: '直接换号/换供应商状态码（逗号分隔）', hint: '命中这些上游状态码时不做指数退避冷却（只给基础冷却）并直接换号，同时在供应商级重试中视为可重试；默认 402（余额不足）。', type: 'text', placeholder: '402' },
];

/** 原系统设置「超时」字段（/limits 读写），v1.0.93 迁入本面板。 */
const TIMEOUT_FIELDS: Array<{
  field: 'api_timeout_secs' | 'connect_timeout_secs' | 'headers_timeout_secs' | 'stream_idle_secs' | 'body_timeout_secs';
  label: string;
  hint: string;
  min: number;
  max: number;
  placeholder: string;
}> = [
  { field: 'api_timeout_secs', label: '请求总超时 (秒)', hint: '单次上游请求的总时长上限，默认 300 秒（修改后重启生效）。', min: 5, max: 3600, placeholder: '300' },
  { field: 'connect_timeout_secs', label: '连接超时 (秒)', hint: '与上游建立 TCP/TLS 连接的上限，默认 10 秒（修改后重启生效）。', min: 1, max: 300, placeholder: '10' },
  { field: 'headers_timeout_secs', label: '响应头等待 / 首事件上限 (秒)', hint: '等待上游响应头与流式首个事件的上限，默认 60 秒（保存后立即生效）。', min: 1, max: 600, placeholder: '60' },
  { field: 'stream_idle_secs', label: '流空闲超时 (秒)', hint: '流式响应两次读取之间的最长静默，默认 30 秒（修改后重启生效）。', min: 1, max: 600, placeholder: '30' },
  { field: 'body_timeout_secs', label: '非流式响应体超时 (秒)', hint: '非流式请求读完整响应体的上限，默认 300 秒（保存后立即生效）。流式请求不受此项约束。', min: 1, max: 3600, placeholder: '300' },
];

interface RetryTimeoutForm {
  max_retries: string;
  retry_self_count: string;
  retry_interval_ms: string;
  retry_on_status: string;
  api_timeout_secs: string;
  connect_timeout_secs: string;
  headers_timeout_secs: string;
  stream_idle_secs: string;
  body_timeout_secs: string;
}

const EMPTY_RTFORM: RetryTimeoutForm = {
  max_retries: '',
  retry_self_count: '',
  retry_interval_ms: '',
  retry_on_status: '',
  api_timeout_secs: '',
  connect_timeout_secs: '',
  headers_timeout_secs: '',
  stream_idle_secs: '',
  body_timeout_secs: '',
};

/** 重试 / 超时（原系统设置两分类整卡迁入；随 /limits 读写，保存即生效口径不变）。 */
function RetryTimeoutCard(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();
  const [form, setForm] = useState<RetryTimeoutForm>({ ...EMPTY_RTFORM });
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState('');

  useEffect(() => {
    let alive = true;
    void (async () => {
      try {
        const res = await api.getLimits();
        const d = (res?.data ?? res) as Partial<RetryTimeoutForm> & { retry_on_status?: number[] } | undefined;
        if (!alive) return;
        setForm({
          max_retries: d?.max_retries != null ? String(d.max_retries) : '',
          retry_self_count: d?.retry_self_count != null ? String(d.retry_self_count) : '',
          retry_interval_ms: d?.retry_interval_ms != null ? String(d.retry_interval_ms) : '',
          retry_on_status: Array.isArray(d?.retry_on_status) ? d.retry_on_status.join(', ') : '',
          api_timeout_secs: d?.api_timeout_secs != null ? String(d.api_timeout_secs) : '',
          connect_timeout_secs: d?.connect_timeout_secs != null ? String(d.connect_timeout_secs) : '',
          headers_timeout_secs: d?.headers_timeout_secs != null ? String(d.headers_timeout_secs) : '',
          stream_idle_secs: d?.stream_idle_secs != null ? String(d.stream_idle_secs) : '',
          body_timeout_secs: d?.body_timeout_secs != null ? String(d.body_timeout_secs) : '',
        });
      } catch (err) {
        if (alive) setError(err instanceof Error ? err.message : String(err));
      } finally {
        if (alive) setLoading(false);
      }
    })();
    return () => { alive = false; };
  }, []);

  const handleChange = (field: keyof RetryTimeoutForm, value: string): void => {
    setForm({ ...form, [field]: value });
  };

  const save = async (): Promise<void> => {
    setSaving(true);
    setError('');
    try {
      const payload: Record<string, number | number[]> = {};
      // [字段名, 人类标签, 输入串, min, max]（标签取自字段定义，报错可读）
      const FIELD_LABEL: Record<string, string> = {};
      for (const f of [...RETRY_FIELDS, ...TIMEOUT_FIELDS]) FIELD_LABEL[f.field] = f.label;
      const nums: Array<[string, string, number, number, number]> = [
        ['max_retries', FIELD_LABEL.max_retries, Number(form.max_retries), 0, 10],
        ['retry_self_count', FIELD_LABEL.retry_self_count, Number(form.retry_self_count), 0, 10],
        ['retry_interval_ms', FIELD_LABEL.retry_interval_ms, Number(form.retry_interval_ms), 0, 10000],
        ['api_timeout_secs', FIELD_LABEL.api_timeout_secs, Number(form.api_timeout_secs), 5, 3600],
        ['connect_timeout_secs', FIELD_LABEL.connect_timeout_secs, Number(form.connect_timeout_secs), 1, 300],
        ['headers_timeout_secs', FIELD_LABEL.headers_timeout_secs, Number(form.headers_timeout_secs), 1, 600],
        ['stream_idle_secs', FIELD_LABEL.stream_idle_secs, Number(form.stream_idle_secs), 1, 600],
        ['body_timeout_secs', FIELD_LABEL.body_timeout_secs, Number(form.body_timeout_secs), 1, 3600],
      ];
      for (const [name, label, v, min, max] of nums) {
        if (form[name as keyof RetryTimeoutForm] !== '') {
          if (!Number.isFinite(v) || v < min || v > max) {
            setError(t('{{name}}必须在 {{min}}–{{max}} 之间', { name: t(label), min, max }));
            setSaving(false);
            return;
          }
          payload[name] = v;
        }
      }
      const statuses = form.retry_on_status
        .split(/[,，\s]+/)
        .map((s) => Number(s.trim()))
        .filter((n) => Number.isInteger(n) && n >= 100 && n < 600);
      if (statuses.length > 0) payload.retry_on_status = statuses;
      await api.updateLimits(payload);
      addToast(t('重试与超时设置已保存'));
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  };

  return (
    <Card title={t('重试 / 超时')}>
      <div className="settings-form">
        <span className="form-hint">
          {t('上游失败后的重试/换号策略与各阶段等待上限（原系统设置「重试」「超时」两分类，已并入路由错误处理）。')}
        </span>
        {loading ? (
          <span style={{ display: 'inline-flex', alignItems: 'center', gap: 6 }}>
            <Loader2 size={14} className="confirm-dialog-spin" /> {t('读取中')}
          </span>
        ) : (
          <>
            {RETRY_FIELDS.map((f) => (
              <div key={f.field} className="form-group">
                <label>{t(f.label)}</label>
                <input
                  className="form-input"
                  type={f.type}
                  min={f.min}
                  max={f.max}
                  placeholder={f.placeholder}
                  value={form[f.field]}
                  onChange={(e) => handleChange(f.field, e.target.value)}
                  disabled={saving}
                />
                <span className="form-hint">{t(f.hint)}</span>
              </div>
            ))}
            {TIMEOUT_FIELDS.map((f) => (
              <div key={f.field} className="form-group">
                <label>{t(f.label)}</label>
                <input
                  className="form-input"
                  type="number"
                  min={f.min}
                  max={f.max}
                  placeholder={f.placeholder}
                  value={form[f.field]}
                  onChange={(e) => handleChange(f.field, e.target.value)}
                  disabled={saving}
                />
                <span className="form-hint">{t(f.hint)}</span>
              </div>
            ))}
            {error ? <div className="error-message">{error}</div> : null}
            <div className="settings-actions">
              <Button variant="primary" onClick={() => void save()} disabled={saving}>
                {saving ? t('保存中…') : t('保存更改')}
              </Button>
            </div>
          </>
        )}
      </div>
    </Card>
  );
}

function ErrorsCard({ onDirty }: { onDirty?: (dirty: boolean) => void } = {}): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();
  const [view, setView] = useState<ErrorPolicyView | null>(null);
  const [draft, setDraft] = useState<ErrorSettings | null>(null);
  const [loading, setLoading] = useState(true);
  const [notReady, setNotReady] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  /** 分类（对齐 A2A RoutingErrorsPanel：处理规则 / 失败保护 / 安全边界） */
  const [tab, setTab] = useState<'rules' | 'protection' | 'boundaries'>('rules');
  const [ruleScope, setRuleScope] = useState<'global' | 'provider'>('global');
  const [previewInput, setPreviewInput] = useState<ErrorPreviewInput>({ scenario: 'rate', provider: '', model: '', started: false, uncertain: false, pinned: false, switchesUsed: 0, resendsUsed: 0, strikes: 1, customStatus: '', customCode: '' });
  const dirtyRef = useRef(false);
  const edited = useRef(false);

  /**
   * 草稿与已存设置的差异。本卡有多个提前 return（读取中 / 端点未就绪 / 读失败），
   * hook 不能放在它们之后 —— 所以在这里先算好，再经 effect 上报给调用方，
   * 由路由页左栏给「错误处理」挂黄点（与 A2A 的 errorsDirty 同口径）。
   */
  const dirty = draft && view ? JSON.stringify(draft) !== JSON.stringify(view.settings) : false;
  dirtyRef.current = dirty;
  useEffect(() => {
    onDirty?.(dirty);
  }, [dirty, onDirty]);
  useEffect(() => {
    const protectDraft = (event: BeforeUnloadEvent) => {
      if (dirtyRef.current) {
        event.preventDefault();
        event.returnValue = '';
      }
    };
    window.addEventListener('beforeunload', protectDraft);
    return () => window.removeEventListener('beforeunload', protectDraft);
  }, []);
  // 卸载即丢草稿（草稿是组件内 state），黄点必须跟着消失，不能留下对不存在的草稿的提示
  useEffect(() => () => onDirty?.(false), [onDirty]);

  const adopt = (next: ErrorPolicyView): void => {
    setView(next);
    setDraft(structuredClone(next.settings));
    setPreviewInput(current => ({ ...current, provider: current.provider || next.providers[0]?.id || '*' }));
    edited.current = false;
  };

  const load = useCallback(async (): Promise<void> => {
    try {
      const res = await api.getRoutingErrors();
      const data = (res?.data ?? null) as unknown as ErrorPolicyView | null;
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

  const patch = (next: Partial<ErrorSettings>): void => {
    if (!draft) return;
    setDraft({ ...draft, ...next });
    edited.current = true;
  };

  const inspect = (kind: ErrorKind): void => {
    const map: Record<ErrorKind, string> = { rate_limit: 'rate', quota: 'quota', auth: 'auth', balance: 'balance', transient: 'transient', unsupported: 'unsupported', bad_request: 'input', content: 'content', unknown: 'unknown' };
    setPreviewInput(current => ({ ...current, scenario: map[kind], started: false, uncertain: false }));
  };

  const save = async (): Promise<void> => {
    if (!draft || !view || busy) return;
    const invalid = validate(draft, t);
    if (invalid) {
      setError(invalid);
      return;
    }
    setBusy(true);
    setError('');
    try {
      const res = await api.saveRoutingErrors({ settings: draft, revision: view.revision });
      const data = (res?.data ?? null) as unknown as ErrorPolicyView | null;
      if (data) adopt(data);
      addToast(t('错误处理设置已保存，下一个请求生效'));
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  };

  if (loading) {
    return (
      <Card title={t('错误处理')}>
        <div className="settings-form">
          <Loader2 size={14} className="confirm-dialog-spin" /> {t('读取中')}
        </div>
      </Card>
    );
  }
  if (notReady) {
    return (
      <Card title={t('错误处理')}>
        <div className="empty-state">
          <p>{t('错误处理接口尚未就绪')}</p>
        </div>
      </Card>
    );
  }
  if (!draft || !view) {
    return (
      <Card title={t('错误处理')}>
        <div className="empty-state">
          <p>{t('错误处理设置读取失败')}</p>
        </div>
      </Card>
    );
  }

  return (
    <Card
      title={t('错误处理')}
      actions={
        <>
          <Button
            variant="outline"
            size="sm"
            onClick={() => {
              const next = structuredClone(view.defaults);
              setDraft(next);
              addToast(t('已填入建议值（保存后才生效，已有账号与冷却记录保持不变）'));
            }}
            disabled={busy}
          >
            {t('恢复建议值')}
          </Button>
          <Button variant="outline" size="sm" onClick={() => void load()} disabled={busy || dirty}>
            <RefreshCw size={13} /> {t('重新读取')}
          </Button>
        </>
      }
    >
      <div className="settings-form">
        {/* 分类分段（A2A 的 SegmentedControl：处理规则 / 失败保护 / 安全边界） */}
        <div className="rt-seg" role="tablist" aria-label={t('错误处理分类')} style={{ marginBottom: 12 }}>
          <button
            type="button"
            role="tab"
            aria-selected={tab === 'rules'}
            className={`rt-seg-btn${tab === 'rules' ? ' is-on' : ''}`}
            onClick={() => setTab('rules')}
          >
            {t('处理规则')}
          </button>
          <button
            type="button"
            role="tab"
            aria-selected={tab === 'protection'}
            className={`rt-seg-btn${tab === 'protection' ? ' is-on' : ''}`}
            onClick={() => setTab('protection')}
          >
            {t('失败保护')}
          </button>
          <button
            type="button"
            role="tab"
            aria-selected={tab === 'boundaries'}
            className={`rt-seg-btn${tab === 'boundaries' ? ' is-on' : ''}`}
            onClick={() => setTab('boundaries')}
          >
            {t('安全边界')}
          </button>
        </div>
        {/* 每条请求的重试上限（对齐 A2A `re-limit-strip`）：四个最常调的字段常驻在
            分类之上 —— 切到哪个分类都看得到，不必为了改「最多再换账号」先切回某个 tab。 */}
        <div className="re-limit-strip" role="group" aria-label={t('每条请求的重试上限')}>
          <div className="re-compact-fields">
            {LIMIT_FIELDS.slice(0, 4).map((f) => (
              <div key={f.key} className="form-group">
                <label>{t(f.label)}</label>
                <Input
                  type="number"
                  min={f.min}
                  max={f.max}
                  value={draft[f.key] as number}
                  disabled={busy}
                  aria-label={t(f.label)}
                  onChange={(e) => patch({ [f.key]: Number(e.target.value) } as Partial<ErrorSettings>)}
                />
              </div>
            ))}
          </div>
        </div>
        <span className="form-hint">
          {t('上游失败后按规则换账号或跳过提供商；每条请求的重试与换号受下列上限约束。保存后下一个请求生效。')}
        </span>
        {view.legacy ? (
          <span className="form-hint">{t('已保留原重试与失败休息参数，错误处理已统一；429 也计入换号上限。')}</span>
        ) : null}

        <div style={{ display: 'grid', gridTemplateColumns: 'minmax(0, 1fr) minmax(260px, 340px)', gap: 16, alignItems: 'start' }}>
          <div style={{ minWidth: 0 }}>
        {tab === 'protection' ? (
          <>
        {/* 失败保护（对齐 A2A `ErrorProtection` 的四张卡：失败退避 / 限流后降低优先级 /
            连续失败保护 / 限流恢复保护）。前四个「每条请求的重试上限」已提到分类之上，
            这里只剩冷却与保护三格。 */}
        <div className="rt-card-group">
          <div className="rt-group-card">
            <div className="rt-group-head">
              <div className="rt-group-title"><b>{t('冷却与保护')}</b></div>
            </div>
            <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fill, minmax(220px, 1fr))', gap: 16, marginTop: 10 }}>
              {LIMIT_FIELDS.slice(4).map((f) => (
                <div key={f.key} className="form-group">
                  <label>{t(f.label)}</label>
                  <Input
                    type="number"
                    min={f.min}
                    max={f.max}
                    value={draft[f.key] as number}
                    disabled={busy}
                    aria-label={t(f.label)}
                    onChange={(e) => patch({ [f.key]: Number(e.target.value) } as Partial<ErrorSettings>)}
                  />
                </div>
              ))}
            </div>
          </div>

          <div className="rt-group-card">
            <div className="rt-group-head">
              <div className="rt-group-title"><b>{t('失败退避')}</b></div>
            </div>
            <label className="rt-special-check" style={{ marginTop: 8 }}>
              <input type="checkbox" checked={draft.backoff} disabled={busy} onChange={(e) => patch({ backoff: e.target.checked })} />
              <span>{t('失败后暂时休息')}</span>
            </label>
            <span className="form-hint">{t('只统计连接和服务异常，不惩罚请求参数、内容拒绝或能力缺失。连续失败逐次延长；成功后清零。')}</span>
          </div>

          <div className="rt-group-card">
            <div className="rt-group-head">
              <div className="rt-group-title"><b>{t('限流后降低优先级')}</b></div>
            </div>
            <label className="rt-special-check" style={{ marginTop: 8 }}>
              <input type="checkbox" checked={draft.sink} disabled={busy} onChange={(e) => patch({ sink: e.target.checked })} />
              <span>{t('限流后暂时排到后面')}</span>
            </label>
            <div className="form-group" style={{ marginTop: 8 }}>
              <label>{t('最长保留时间')}</label>
              <Input
                type="number"
                min={30}
                max={3600}
                value={draft.sinkSeconds}
                disabled={busy || !draft.sink}
                aria-label={t('最长保留时间')}
                onChange={(e) => patch({ sinkSeconds: Number(e.target.value) })}
              />
            </div>
            <span className="form-hint">{t('只影响选路顺序，不延长上游明确的冷却时间；到点自动恢复原顺序，成功一次可提前恢复。')}</span>
          </div>

          <div className="rt-group-card">
            <div className="rt-group-head">
              <div className="rt-group-title"><b>{t('连续失败保护')}</b></div>
            </div>
            <label className="rt-special-check" style={{ marginTop: 8 }}>
              <input type="checkbox" checked={draft.breaker} disabled={busy} onChange={(e) => patch({ breaker: e.target.checked })} />
              <span>{t('启用连续失败保护')}</span>
            </label>
            <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fill, minmax(180px, 1fr))', gap: 12, marginTop: 8 }}>
              <div className="form-group">
                <label>{t('连续失败阈值')}</label>
                <Input
                  type="number"
                  min={1}
                  max={20}
                  value={draft.breakerFailures}
                  disabled={busy || !draft.breaker}
                  aria-label={t('连续失败阈值')}
                  onChange={(e) => patch({ breakerFailures: Number(e.target.value) })}
                />
              </div>
              <div className="form-group">
                <label>{t('保护时长')}</label>
                <Input
                  type="number"
                  min={10}
                  max={600}
                  value={draft.breakerSeconds}
                  disabled={busy || !draft.breaker}
                  aria-label={t('保护时长')}
                  onChange={(e) => patch({ breakerSeconds: Number(e.target.value) })}
                />
              </div>
              <div className="form-group">
                <label>{t('恢复所需成功次数')}</label>
                <Input
                  type="number"
                  min={1}
                  max={10}
                  value={draft.breakerSuccesses}
                  disabled={busy || !draft.breaker}
                  aria-label={t('恢复所需成功次数')}
                  onChange={(e) => patch({ breakerSuccesses: Number(e.target.value) })}
                />
              </div>
            </div>
            <span className="form-hint">{t('只统计连接和服务异常；生效时间取较晚的一项，不叠加等待。')}</span>
          </div>
        </div>

        </>
        ) : tab === 'boundaries' ? (
        /* 安全边界（对齐 A2A `ErrorBoundaries`）：静态说明，没有可编辑字段 —— 这些是
           网关无论如何都不会做的四件事，写出来是为了让人知道「重试/换号」的能力边界。 */
        <div className="rt-group-card">
          <div className="rt-group-head">
            <div className="rt-group-title"><b>{t('始终保留的保护')}</b></div>
          </div>
          <ul className="rt-boundaries">
            <li>
              <strong>{t('已经开始输出')}</strong>
              <span>{t('不从头重发，不拼接另一个账号的回答。')}</span>
            </li>
            <li>
              <strong>{t('任务或计费结果不确定')}</strong>
              <span>{t('没有可靠的安全重放依据时，不重复提交。')}</span>
            </li>
            <li>
              <strong>{t('验证、风控与明确拒绝')}</strong>
              <span>{t('不通过换号绕过；保留原因，返回客户端。')}</span>
            </li>
            <li>
              <strong>{t('既有权限与路由范围')}</strong>
              <span>{t('不扩大网关密钥授权，不擅自改指定账号，不新增付费操作。')}</span>
            </li>
          </ul>
        </div>
        ) : (
        <>
          <div className="rt-seg" role="tablist" aria-label="处理规则范围" style={{ marginBottom: 12 }}>
            <button type="button" role="tab" aria-selected={ruleScope === 'global'} className={`rt-seg-btn${ruleScope === 'global' ? ' is-on' : ''}`} onClick={() => setRuleScope('global')}>全局处理规则</button>
            <button type="button" role="tab" aria-selected={ruleScope === 'provider'} className={`rt-seg-btn${ruleScope === 'provider' ? ' is-on' : ''}`} onClick={() => setRuleScope('provider')}>提供商处理规则（{mergeProviderRules(draft, view.providerRules ?? []).length}）</button>
          </div>
          <ErrorRules settings={draft} providers={view.providers} providerRules={view.providerRules ?? []} scope={ruleScope} onChange={next => { setDraft(next); edited.current = true; }} onPreview={inspect} />
        </>)}

          </div>
          <ErrorPreviewPanel settings={draft} providers={view.providers} providerRules={view.providerRules ?? []} input={previewInput} onChange={setPreviewInput} />
        </div>

        {error ? <div className="error-message">{error}</div> : null}

        <div className="settings-actions">
          <Button variant="primary" disabled={!dirty || busy} onClick={() => void save()}>
            {busy ? t('保存中…') : t('保存修改')}
          </Button>
        </div>
      </div>
    </Card>
  );
}
