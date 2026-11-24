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
import { Badge, Button, Card, Input, Select } from '../components/ui';

type ErrorKind = 'rate_limit' | 'quota' | 'auth' | 'balance' | 'transient' | 'unsupported' | 'bad_request' | 'content' | 'unknown';
type ErrorAction = 'switch_account' | 'retry_then_switch' | 'refresh_then_switch' | 'switch_provider' | 'return_error';
type CooldownMode = 'upstream' | 'fixed' | 'protection' | 'none';
type CooldownScope = 'auto' | 'model' | 'account' | 'provider';

interface ErrorRule {
  id: string;
  name: string;
  kind: ErrorKind;
  statuses: number[];
  code: string;
  provider: string;
  model: string;
  enabled: boolean;
  action: ErrorAction;
  cooldown: CooldownMode;
  fallbackSeconds: number;
  exponential: boolean;
  fallbackCapSeconds: number;
  scope: CooldownScope;
}

interface ErrorSettings {
  resends: number;
  switches: number;
  intervalSeconds: number;
  waitBudgetSeconds: number;
  cooldownFloorSeconds: number;
  backoff: boolean;
  backoffSeconds: number;
  backoffCapSeconds: number;
  sink: boolean;
  sinkSeconds: number;
  breaker: boolean;
  breakerFailures: number;
  breakerSeconds: number;
  breakerSuccesses: number;
  rules: ErrorRule[];
  overrides: ErrorRule[];
}

interface ErrorPolicyView {
  settings: ErrorSettings;
  defaults: ErrorSettings;
  revision: string;
  configured: boolean;
  legacy: boolean;
  providers: Array<{ id: string; label: string }>;
  /** beta.8：已启动插件在握手里申报的默认错误规则（只读展示 + 三层解析参考） */
  providerRules?: PluginErrorRule[];
}

/** 插件申报规则（camelCase；对齐后端 `PluginErrorRule` 序列化） */
interface PluginErrorRule extends ErrorRule {
  pluginRuleId: string;
  reason?: string;
}

const KIND_LABEL: Record<ErrorKind, string> = {
  rate_limit: '请求限流',
  quota: '额度耗尽',
  auth: '登录失效',
  balance: '余额不足',
  transient: '连接或服务异常',
  unsupported: '能力不支持',
  bad_request: '请求有误',
  content: '内容被拒绝',
  unknown: '其他错误',
};

const ACTION_LABEL: Record<ErrorAction, string> = {
  switch_account: '直接换账号',
  retry_then_switch: '先重发，再换账号',
  refresh_then_switch: '续期一次，失败再换号',
  switch_provider: '跳过提供商',
  return_error: '直接返回错误',
};

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
  /** 分类（对齐 A2A RoutingErrorsPanel：处理规则 / 失败保护；「安全边界」本仓无数据源，不落） */
  const [tab, setTab] = useState<'rules' | 'protection'>('rules');
  const edited = useRef(false);

  /**
   * 草稿与已存设置的差异。本卡有多个提前 return（读取中 / 端点未就绪 / 读失败），
   * hook 不能放在它们之后 —— 所以在这里先算好，再经 effect 上报给调用方，
   * 由路由页左栏给「错误处理」挂黄点（与 A2A 的 errorsDirty 同口径）。
   */
  const dirty = draft && view ? JSON.stringify(draft) !== JSON.stringify(view.settings) : false;
  useEffect(() => {
    onDirty?.(dirty);
  }, [dirty, onDirty]);
  // 卸载即丢草稿（草稿是组件内 state），黄点必须跟着消失，不能留下对不存在的草稿的提示
  useEffect(() => () => onDirty?.(false), [onDirty]);

  const adopt = (next: ErrorPolicyView): void => {
    setView(next);
    setDraft(structuredClone(next.settings));
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

  const patchRule = (index: number, next: Partial<ErrorRule>): void => {
    if (!draft) return;
    patch({ rules: draft.rules.map((r, i) => (i === index ? { ...r, ...next } : r)) });
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
        {/* 分类分段（A2A 的 SegmentedControl；本仓有 Tabs，但只两档时分段更省位） */}
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
        </div>
        <span className="form-hint">
          {t('上游失败后按规则换账号或跳过提供商；每条请求的重试与换号受下列上限约束。保存后下一个请求生效。')}
        </span>
        {view.legacy ? (
          <span className="form-hint">{t('已保留原重试与失败休息参数，错误处理已统一；429 也计入换号上限。')}</span>
        ) : null}

        {tab === 'protection' ? (
          <>
        {/* 重试 / 换号 / 冷却上限 */}
        <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fill, minmax(220px, 1fr))', gap: 16 }}>
          {LIMIT_FIELDS.map((f) => (
            <div key={f.key} className="form-group">
              <label>{t(f.label)}</label>
              <Input
                type="number"
                min={f.min}
                max={f.max}
                value={draft[f.key] as number}
                disabled={busy}
                onChange={(e) => patch({ [f.key]: Number(e.target.value) } as Partial<ErrorSettings>)}
              />
            </div>
          ))}
        </div>

        <div style={{ display: 'flex', gap: 16, flexWrap: 'wrap' }}>
          <label style={{ display: 'flex', alignItems: 'center', gap: 6 }}>
            <input type="checkbox" checked={draft.backoff} disabled={busy} onChange={(e) => patch({ backoff: e.target.checked })} />
            {t('失败后退避休息')}
          </label>
          <label style={{ display: 'flex', alignItems: 'center', gap: 6 }}>
            <input type="checkbox" checked={draft.breaker} disabled={busy} onChange={(e) => patch({ breaker: e.target.checked })} />
            {t('熔断保护')}
          </label>
        </div>

        </>
        ) : (
        <>
        {/* 按错误类型的处理规则 */}
        <div className="form-group">
          <label>{t('按错误类型的处理规则')}</label>
          <span className="form-hint" style={{ marginBottom: 8 }}>
            {t('每种错误类型的默认动作与冷却方式；规则由后端编译执行，启用/停用与动作调整即时生效。')}
          </span>
          <div style={{ display: 'flex', flexDirection: 'column', gap: 8 }}>
            {draft.rules.map((rule, index) => (
              <div
                key={rule.id}
                style={{ display: 'flex', alignItems: 'center', gap: 10, flexWrap: 'wrap', border: '1px solid var(--border-color)', borderRadius: 8, padding: '8px 10px' }}
              >
                <Badge tone={rule.enabled ? 'info' : 'neutral'}>{t(KIND_LABEL[rule.kind] || rule.name)}</Badge>
                <span className="form-hint">{rule.name}</span>
                <label style={{ display: 'flex', alignItems: 'center', gap: 6, marginLeft: 'auto' }}>
                  <input
                    type="checkbox"
                    checked={rule.enabled}
                    disabled={busy}
                    onChange={(e) => patchRule(index, { enabled: e.target.checked })}
                  />
                  {t('启用')}
                </label>
                <Select
                  style={{ maxWidth: 180 }}
                  value={rule.action}
                  disabled={busy}
                  aria-label={t('处理动作')}
                  onChange={(e) => patchRule(index, { action: e.target.value as ErrorAction })}
                >
                  {(Object.keys(ACTION_LABEL) as ErrorAction[]).map((a) => (
                    <option key={a} value={a}>{t(ACTION_LABEL[a])}</option>
                  ))}
                </Select>
                {rule.cooldown !== 'none' ? (
                  <Input
                    type="number"
                    style={{ width: 100 }}
                    min={1}
                    max={3600}
                    value={rule.fallbackSeconds}
                    disabled={busy}
                    aria-label={t('冷却秒数')}
                    onChange={(e) => patchRule(index, { fallbackSeconds: Number(e.target.value) })}
                  />
                ) : null}
              </div>
            ))}
          </div>
        </div>

        {/* beta.8：插件申报的默认错误规则（只读）。解析顺序：我的例外 > 插件默认 > 全局 */}
        {(view.providerRules ?? []).length > 0 ? (
          <div className="form-group">
            <label>{t('插件申报的默认规则（只读）')}</label>
            <span className="form-hint" style={{ marginBottom: 8 }}>
              {t('已启动插件在握手时申报的默认错误处理；解析顺序为「我的例外 → 插件默认 → 全局规则」。如需改写某条，在例外中按相同规则标识配置即可覆盖。')}
            </span>
            <div style={{ display: 'flex', flexDirection: 'column', gap: 8 }}>
              {(view.providerRules ?? []).map((pr) => {
                const overridden = draft.overrides.some((o) => o.id === pr.id || o.id === pr.pluginRuleId);
                return (
                  <div
                    key={pr.id}
                    style={{ display: 'flex', alignItems: 'center', gap: 10, flexWrap: 'wrap', border: '1px dashed var(--border-color)', borderRadius: 8, padding: '8px 10px', opacity: overridden ? 0.6 : 1 }}
                    title={pr.reason || undefined}
                  >
                    <Badge tone={overridden ? 'neutral' : 'info'}>{t(KIND_LABEL[pr.kind] || pr.name)}</Badge>
                    <span className="form-hint">
                      {pr.name}
                      {pr.reason ? ` — ${pr.reason}` : ''}
                    </span>
                    {/* rule.provider 即申报插件的 provider id */}
                    <span className="form-hint" style={{ marginLeft: 'auto' }}>{pr.provider}</span>
                    <span className="form-hint">{t(ACTION_LABEL[pr.action] || pr.action)}</span>
                    {overridden ? (
                      <Badge tone="success">{t('已被我的例外覆盖')}</Badge>
                    ) : (
                      <Badge tone="info">{t('插件默认')}</Badge>
                    )}
                  </div>
                );
              })}
            </div>
          </div>
        ) : null}
        </>)}

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
