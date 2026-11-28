// Ported from Agent2API routing-errors-{rules,exceptions,preview,simulation,types}.
// AIGX 使用原生表单控件与 modal-* 样式承载上游同等交互。
import { useEffect, useMemo, useState } from 'react';
import { ChevronDown, Clock3, Plus, Route, Settings2, ShieldCheck, X } from 'lucide-react';
import { Badge, Button, Input, Select } from './ui';

export type ErrorKind = 'rate_limit' | 'quota' | 'auth' | 'balance' | 'transient' | 'unsupported' | 'bad_request' | 'content' | 'unknown';
export type ErrorAction = 'switch_account' | 'retry_then_switch' | 'refresh_then_switch' | 'switch_provider' | 'return_error';
export type CooldownMode = 'upstream' | 'fixed' | 'protection' | 'deprioritize' | 'none';
export type CooldownScope = 'auto' | 'model' | 'account' | 'provider';
export interface ErrorRule {
  id: string; name: string; kind: ErrorKind; statuses: number[]; code: string;
  provider: string; model: string; enabled: boolean; action: ErrorAction;
  cooldown: CooldownMode; fallbackSeconds: number; exponential: boolean;
  fallbackCapSeconds: number; scope: CooldownScope;
}
export interface PluginErrorRule extends ErrorRule { pluginRuleId: string; reason?: string }
export interface ErrorSettings {
  resends: number; switches: number; intervalSeconds: number; waitBudgetSeconds: number;
  cooldownFloorSeconds: number; backoff: boolean; backoffSeconds: number; backoffCapSeconds: number;
  sink: boolean; sinkSeconds: number; breaker: boolean; breakerFailures: number;
  breakerSeconds: number; breakerSuccesses: number; rules: ErrorRule[]; overrides: ErrorRule[];
}
export type Provider = { id: string; label: string };

export const KIND_LABEL: Record<ErrorKind, string> = {
  rate_limit: '请求限流', quota: '额度耗尽', auth: '登录失效', balance: '余额不足',
  transient: '连接或服务异常', unsupported: '能力不支持', bad_request: '请求有误',
  content: '内容被拒绝', unknown: '其他错误',
};
export const ACTION_LABEL: Record<ErrorAction, string> = {
  switch_account: '直接换账号', retry_then_switch: '先重发，再换账号',
  refresh_then_switch: '续期一次，失败再换号', switch_provider: '跳过提供商', return_error: '直接返回错误',
};
const SCOPE_LABEL: Record<CooldownScope, string> = {
  auto: '跟随上游限制范围', model: '当前账号的当前模型', account: '当前账号的全部模型', provider: '整个提供商',
};

function ruleLabel(rule: ErrorRule): string {
  return rule.id === rule.kind ? KIND_LABEL[rule.kind] : rule.name;
}
function duration(seconds: number): string {
  if (!seconds) return '不冷却';
  if (seconds >= 3600 && seconds % 3600 === 0) return `${seconds / 3600} 小时`;
  if (seconds >= 60 && seconds % 60 === 0) return `${seconds / 60} 分钟`;
  return `${seconds} 秒`;
}
function cooldownSummary(rule: ErrorRule, settings: ErrorSettings): string {
  if (rule.cooldown === 'none') return '不冷却';
  if (rule.cooldown === 'deprioritize') return `优先级调到最低（${duration(rule.fallbackSeconds)} 后恢复）`;
  if (rule.cooldown === 'protection') return settings.backoff ? `失败后休息 ${duration(settings.backoffSeconds)}` : '不冷却';
  if (rule.cooldown === 'fixed') return `固定 ${duration(rule.fallbackSeconds)}`;
  return `上游时间，缺失用 ${duration(rule.fallbackSeconds)}`;
}
function ruleSummary(rule: ErrorRule, settings: ErrorSettings): string {
  return [rule.statuses.join(', ') || KIND_LABEL[rule.kind], rule.code, rule.model,
    ACTION_LABEL[rule.action], cooldownSummary(rule, settings)].filter(Boolean).join(' · ');
}
interface ProviderRuleEntry {
  id: string; rule: ErrorRule; custom: boolean; pluginDefault?: PluginErrorRule;
  overridden: boolean; overrideIndex: number;
}
export function mergeProviderRules(settings: ErrorSettings, providerRules: PluginErrorRule[] = []): ProviderRuleEntry[] {
  const consumed = new Set<number>();
  const pluginEntries: ProviderRuleEntry[] = [];
  for (const pluginRule of providerRules) {
    const overrideIndex = settings.overrides.findIndex((item, index) => !consumed.has(index)
      && (item.id === pluginRule.id || (item.provider === pluginRule.provider && item.id === pluginRule.pluginRuleId)));
    if (overrideIndex >= 0) consumed.add(overrideIndex);
    const overrideRule = overrideIndex >= 0 ? settings.overrides[overrideIndex] : undefined;
    pluginEntries.push({ id: pluginRule.id, rule: overrideRule ? { ...overrideRule } : { ...pluginRule },
      custom: false, pluginDefault: pluginRule, overridden: Boolean(overrideRule), overrideIndex });
  }
  const customEntries: ProviderRuleEntry[] = [];
  settings.overrides.forEach((rule, overrideIndex) => {
    if (!consumed.has(overrideIndex)) customEntries.push({ id: rule.id, rule, custom: true, overridden: false, overrideIndex });
  });
  return [...customEntries, ...pluginEntries];
}

const fieldStyle = { display: 'grid', gridTemplateColumns: 'repeat(auto-fit, minmax(190px, 1fr))', gap: 12 } as const;
function TextField({ label, value, onChange, placeholder = '' }: { label: string; value: string; onChange(value: string): void; placeholder?: string }): JSX.Element {
  return <div className="form-group"><label>{label}</label><Input aria-label={label} value={value} placeholder={placeholder} onChange={e => onChange(e.target.value)} /></div>;
}
function NumberField({ label, value, onChange, min = 0, max = 300, disabled = false }: { label: string; value: number; onChange(value: number): void; min?: number; max?: number; disabled?: boolean }): JSX.Element {
  return <div className="form-group"><label>{label}</label><Input aria-label={label} type="number" value={value} min={min} max={max} step={1} disabled={disabled}
    onChange={e => { const next = Number(e.target.value); if (Number.isFinite(next)) onChange(next); }} /></div>;
}
function Choice<Value extends string>({ label, value, options, onChange, disabled = false }: { label: string; value: Value; options: ReadonlyArray<{ value: Value; label: string }>; onChange(value: Value): void; disabled?: boolean }): JSX.Element {
  return <div className="form-group"><label>{label}</label><Select aria-label={label} value={value} disabled={disabled} onChange={e => onChange(e.target.value as Value)}>
    {options.map(item => <option key={item.value} value={item.value}>{item.label}</option>)}
  </Select></div>;
}
function Toggle({ label, description, checked, onChange }: { label: string; description?: string; checked: boolean; onChange(value: boolean): void }): JSX.Element {
  return <label className="rt-special-check" style={{ alignItems: 'flex-start' }}><input type="checkbox" checked={checked} onChange={e => onChange(e.target.checked)} />
    <span>{label}{description ? <small className="form-hint" style={{ display: 'block', marginTop: 4 }}>{description}</small> : null}</span></label>;
}

export function ErrorRuleEditor({ rule, providers, exception, pluginDefault, onClose, onSave }: {
  rule: ErrorRule; providers: Provider[]; exception: boolean; pluginDefault?: PluginErrorRule;
  onClose(): void; onSave(rule: ErrorRule): void;
}): JSX.Element {
  const [draft, setDraft] = useState(() => structuredClone(rule));
  const [statuses, setStatuses] = useState(rule.statuses.join(', '));
  const [error, setError] = useState('');
  useEffect(() => {
    const close = (event: KeyboardEvent) => { if (event.key === 'Escape') onClose(); };
    document.addEventListener('keydown', close);
    return () => document.removeEventListener('keydown', close);
  }, [onClose]);
  const patch = (next: Partial<ErrorRule>) => setDraft(current => {
    const updated = { ...current, ...next };
    if (updated.kind !== 'auth' && updated.action === 'refresh_then_switch') updated.action = 'switch_account';
    if (updated.fallbackCapSeconds < updated.fallbackSeconds) updated.fallbackCapSeconds = updated.fallbackSeconds;
    return updated;
  });
  const actions = (Object.entries(ACTION_LABEL) as Array<[ErrorAction, string]>)
    .filter(([value]) => (value !== 'return_error' || draft.action === 'return_error') && (draft.kind === 'auth' || value !== 'refresh_then_switch'))
    .map(([value, label]) => ({ value, label }));
  const modes: Array<{ value: CooldownMode; label: string }> = [
    { value: 'upstream', label: '优先上游时间' }, { value: 'fixed', label: '固定冷却时间' },
    { value: 'protection', label: '使用失败保护' }, { value: 'deprioritize', label: '优先级调到最低' },
    ...(draft.cooldown === 'none' ? [{ value: 'none' as const, label: '不冷却' }] : []),
  ];
  const save = () => {
    if (!draft.name.trim()) { setError('请填写规则名称。'); return; }
    const values = statuses.split(/[,，\s]+/).filter(Boolean);
    if (values.some(text => !/^\d{3}$/.test(text) || Number(text) < 400 || Number(text) > 599)) {
      setError('状态码请填写 400–599 的整数，多个值用逗号分隔。'); return;
    }
    if (![draft.fallbackSeconds, draft.fallbackCapSeconds].every(value => Number.isInteger(value) && value >= 1 && value <= 86400)
      || draft.fallbackCapSeconds < draft.fallbackSeconds) {
      setError('冷却时间必须在 1–86400 秒范围，上限不能短于首次时间。'); return;
    }
    onSave({ ...draft, name: draft.name.trim(), provider: draft.provider.trim(), code: draft.code.trim(), model: draft.model.trim(), statuses: [...new Set(values.map(Number))] });
    onClose();
  };
  const deprioritize = draft.cooldown === 'deprioritize';
  return <div className="modal-overlay" onClick={onClose} role="dialog" aria-modal="true" aria-label={exception ? '提供商处理规则' : '编辑处理规则'}>
    <div className="modal" style={{ width: 'min(680px, 94vw)', maxHeight: '90vh', overflow: 'auto' }} onClick={e => e.stopPropagation()}>
      <div className="modal-header"><h3><Settings2 size={16} style={{ verticalAlign: '-3px', marginRight: 6 }} />{exception ? '提供商处理规则' : '编辑处理规则'} · {ruleLabel(rule)}</h3><button className="modal-close" onClick={onClose} aria-label="关闭"><X size={16} /></button></div>
      <div className="modal-body">
        {pluginDefault?.reason ? <p className="form-hint" style={{ display: 'block', marginBottom: 12 }}>{pluginDefault.reason}</p> : null}
        {exception ? <>
          <TextField label="规则名称" value={draft.name} onChange={name => patch({ name })} />
          <div style={fieldStyle}>
            <Choice label="提供商" value={draft.provider} disabled={Boolean(pluginDefault)} options={[{ value: '*', label: '全部提供商' }, ...providers.map(item => ({ value: item.id, label: item.label }))]} onChange={provider => patch({ provider })} />
            <Choice label="错误类型" value={draft.kind} options={(Object.entries(KIND_LABEL) as Array<[ErrorKind, string]>).map(([value, label]) => ({ value, label }))} onChange={kind => patch({ kind })} />
            <TextField label="上游状态码" placeholder="留空匹配此类全部状态码" value={statuses} onChange={setStatuses} />
            <TextField label="业务码（可选）" placeholder="准确匹配，不按错误文案猜测" value={draft.code} onChange={code => patch({ code })} />
          </div>
          <TextField label="模型（可选）" placeholder="留空适用全部模型" value={draft.model} onChange={model => patch({ model })} />
          <Toggle label="启用规则" checked={draft.enabled} onChange={enabled => patch({ enabled })} />
        </> : <div style={{ marginBottom: 12 }}><Badge tone="neutral">{rule.statuses.join(', ') || '提供商已识别'}</Badge></div>}
        <Choice label="遇到错误时" value={draft.action} options={actions} onChange={action => patch({ action })} />
        <div style={fieldStyle}>
          <Choice label="冷却方式" value={draft.cooldown} options={modes} onChange={cooldown => patch({ cooldown })} />
          <Choice label="影响范围" value={draft.scope} options={(Object.entries(SCOPE_LABEL) as Array<[CooldownScope, string]>).map(([value, label]) => ({ value, label }))} onChange={scope => patch({ scope })} />
        </div>
        {draft.scope !== 'auto' ? <span className="form-hint" style={{ display: 'block', margin: '8px 0 12px' }}>已确认的共享限制范围不能缩小；指定范围只会扩大影响。</span> : null}
        {draft.cooldown === 'upstream' || draft.cooldown === 'fixed' || deprioritize ? <div style={{ border: '1px solid var(--border-color)', borderRadius: 8, padding: 12, margin: '12px 0' }}>
          <NumberField label={draft.cooldown === 'upstream' ? '上游未给时间时' : deprioritize ? '降级保留时长' : '固定冷却时长'} value={draft.fallbackSeconds} min={1} max={86400} onChange={fallbackSeconds => patch({ fallbackSeconds })} />
          <Toggle label={deprioritize ? '连续失败延长降级' : '连续失败延长冷却'} description={deprioritize ? '连续失败时按比例延长降级时间，成功一次后清除。' : '只延长自行估算的时间，不修改上游明确的额度重置时间。'} checked={draft.exponential} onChange={exponential => patch({ exponential })} />
          {draft.exponential ? <NumberField label={deprioritize ? '最长降级时长' : '自行估算的冷却上限'} value={draft.fallbackCapSeconds} min={draft.fallbackSeconds} max={86400} onChange={fallbackCapSeconds => patch({ fallbackCapSeconds })} /> : null}
        </div> : null}
        {draft.cooldown === 'fixed' ? <span className="form-hint" style={{ display: 'block', color: '#d97706' }}>固定时间也不能早于上游明确要求的恢复时间。</span> : null}
        <p className="form-hint" style={{ marginTop: 12 }}><ShieldCheck size={13} style={{ verticalAlign: '-2px' }} /> 已经输出、任务结果不确定、本机校验或风控安全边界仍会停止；其余错误均支持冷却或降级处理。</p>
        {error ? <div role="alert" className="error-message">{error}</div> : null}
      </div>
      <div className="modal-footer"><Button variant="outline" onClick={onClose}>取消</Button><Button variant="primary" onClick={save}>应用</Button></div>
    </div>
  </div>;
}

function ErrorProviderExceptions({ settings, providers, providerRules, onChange, onAdd, onEdit }: {
  settings: ErrorSettings; providers: Provider[]; providerRules: PluginErrorRule[];
  onChange(settings: ErrorSettings): void; onAdd(provider: string): void;
  onEdit(rule: ErrorRule, pluginDefault?: PluginErrorRule): void;
}): JSX.Element {
  const [filter, setFilter] = useState('__all__');
  const [openGroups, setOpenGroups] = useState<Record<string, boolean>>({});
  const merged = useMemo(() => mergeProviderRules(settings, providerRules), [settings, providerRules]);
  const label = (id: string) => id === '*' ? '通用规则' : providers.find(item => item.id === id)?.label || id;
  const groupIds = useMemo(() => {
    const present = new Set(merged.map(item => item.rule.provider || '*'));
    return [...providers.map(item => item.id), ...merged.map(item => item.rule.provider || '*')].filter((id, index, all) => id && present.has(id) && all.indexOf(id) === index);
  }, [providers, merged]);
  const options = [{ value: '__all__', label: '全部提供商' }, ...groupIds.map(id => ({ value: id, label: label(id) }))];
  const effectiveFilter = options.some(item => item.value === filter) ? filter : '__all__';
  const groups = groupIds.filter(id => effectiveFilter === '__all__' || id === effectiveFilter).map(id => {
    const entries = merged.filter(item => (item.rule.provider || '*') === id);
    return { id, entries, customized: entries.filter(item => item.custom || item.overridden).length };
  }).filter(group => group.entries.length > 0);
  return <div className="rt-group-card">
    <div style={{ display: 'flex', alignItems: 'center', gap: 8, marginBottom: 10 }}>
      {groupIds.length ? <Select aria-label="按提供商筛选" value={effectiveFilter} onChange={e => setFilter(e.target.value)} style={{ maxWidth: 240 }}>{options.map(item => <option key={item.value} value={item.value}>{item.label}</option>)}</Select> : null}
      <Button variant="outline" size="sm" style={{ marginLeft: 'auto' }} onClick={() => onAdd(effectiveFilter === '__all__' ? (providers[0]?.id || '*') : effectiveFilter)}><Plus size={13} />添加规则</Button>
    </div>
    {groups.length ? groups.map(group => {
      const open = effectiveFilter !== '__all__' || Boolean(openGroups[group.id]);
      return <div key={group.id} style={{ border: '1px solid var(--border-color)', borderRadius: 8, marginBottom: 8 }}>
        <button type="button" className="rt-seg-btn" style={{ width: '100%', display: 'flex', alignItems: 'center', padding: 10 }} disabled={effectiveFilter !== '__all__'} onClick={() => setOpenGroups(current => ({ ...current, [group.id]: !current[group.id] }))}>
          <strong>{label(group.id)}</strong><span className="form-hint" style={{ marginLeft: 'auto' }}>{group.entries.length} 条规则</span>
          {group.customized ? <Badge tone="info">{group.customized} 项已改</Badge> : null}<ChevronDown size={13} style={{ transform: open ? 'rotate(180deg)' : undefined }} />
        </button>
        {open ? <div style={{ borderTop: '1px solid var(--border-color)' }}>{group.entries.map(entry => <div key={entry.id} style={{ display: 'flex', alignItems: 'center', gap: 6, padding: 10, borderBottom: '1px solid var(--border-color)' }}>
          <div style={{ flex: 1, minWidth: 0 }}><div style={{ display: 'flex', gap: 6, flexWrap: 'wrap' }}><strong>{entry.rule.name}</strong>
            {entry.custom ? <Badge tone="neutral">自定义</Badge> : entry.overridden ? <Badge tone="info">{entry.rule.enabled ? '已修改' : '已关闭'}</Badge> : <Badge tone="neutral">插件默认</Badge>}
            {entry.custom && !entry.rule.enabled ? <Badge tone="neutral">已关闭</Badge> : null}</div><span className="form-hint">{ruleSummary(entry.rule, settings)}</span></div>
          {entry.custom ? <Button variant="outline" size="sm" disabled={entry.overrideIndex <= 0} aria-label="提高规则优先级" onClick={() => { const overrides = [...settings.overrides]; const i = entry.overrideIndex; [overrides[i - 1], overrides[i]] = [overrides[i], overrides[i - 1]]; onChange({ ...settings, overrides }); }}>↑</Button> : null}
          <Button variant="outline" size="sm" onClick={() => onEdit(entry.rule, entry.pluginDefault)}>编辑</Button>
          {entry.overridden ? <Button variant="outline" size="sm" onClick={() => onChange({ ...settings, overrides: settings.overrides.filter(item => item.id !== entry.id && item.id !== entry.pluginDefault?.pluginRuleId) })}>恢复默认</Button> : null}
          {entry.custom ? <Button variant="outline" size="sm" aria-label={`移除规则${entry.rule.name}`} onClick={() => onChange({ ...settings, overrides: settings.overrides.filter(item => item.id !== entry.id) })}><X size={12} /></Button> : null}
        </div>)}</div> : null}
      </div>;
    }) : <p className="form-hint">暂无提供商处理规则，全部跟随全局规则</p>}
  </div>;
}

export function ErrorRules({ settings, providers, providerRules = [], scope, onChange, onPreview }: {
  settings: ErrorSettings; providers: Provider[]; providerRules?: PluginErrorRule[]; scope: 'global' | 'provider';
  onChange(settings: ErrorSettings): void; onPreview(kind: ErrorKind): void;
}): JSX.Element {
  const [editor, setEditor] = useState<{ rule: ErrorRule; exception: boolean; pluginDefault?: PluginErrorRule } | null>(null);
  const apply = (rule: ErrorRule, exception: boolean) => {
    const key = exception ? 'overrides' : 'rules';
    const list = settings[key];
    onChange({ ...settings, [key]: list.some(item => item.id === rule.id) ? list.map(item => item.id === rule.id ? rule : item) : [...list, rule] });
  };
  const add = (provider: string) => {
    const source = settings.rules.find(rule => rule.kind === 'unsupported') || settings.rules[0];
    if (source) {
      const id = typeof crypto !== 'undefined' && 'randomUUID' in crypto ? crypto.randomUUID() : `${Date.now()}-${Math.random().toString(36).slice(2)}`;
      setEditor({ exception: true, rule: { ...structuredClone(source), id: `rule-${id}`, name: '新提供商规则', provider, statuses: [501] } });
    }
  };
  return <>
    {scope === 'global' ? <div className="form-group"><div className="rt-table-wrap"><table className="rt-table rt-error-rules"><thead><tr><th>错误类型</th><th>处理方式</th><th>冷却</th><th><span className="sr-only">编辑</span></th></tr></thead><tbody>
      {settings.rules.map(rule => <tr key={rule.id}><td><div className="rt-error-rule-name"><strong>{ruleLabel(rule)}</strong><span className="form-hint">{rule.kind === 'unknown' ? '未命中以上分类' : rule.statuses.join(', ') || '提供商已识别'}</span></div></td>
        <td><Button variant="outline" size="sm" onClick={() => onPreview(rule.kind)}>{ACTION_LABEL[rule.action]}</Button></td>
        <td><span className="form-hint">{cooldownSummary(rule, settings)}</span></td>
        <td><Button variant="outline" size="sm" aria-label={`编辑${ruleLabel(rule)}`} onClick={() => setEditor({ rule, exception: false })}><Settings2 size={14} /></Button></td></tr>)}
    </tbody></table></div><span className="form-hint">501 来自网关缺少能力时直接返回；账号模型权限不足只跳过对应账号与模型。</span></div>
      : <ErrorProviderExceptions settings={settings} providers={providers} providerRules={providerRules} onChange={onChange} onAdd={add} onEdit={(rule, pluginDefault) => setEditor({ rule, exception: true, pluginDefault })} />}
    {editor ? <ErrorRuleEditor key={editor.rule.id} rule={editor.rule} providers={providers} exception={editor.exception} pluginDefault={editor.pluginDefault} onClose={() => setEditor(null)} onSave={rule => apply(rule, editor.exception)} /> : null}
  </>;
}

export interface ErrorPreviewInput {
  scenario: string; provider: string; model: string; started: boolean; uncertain: boolean; pinned: boolean;
  switchesUsed: number; resendsUsed: number; strikes: number; customStatus?: string; customCode?: string;
}
interface Scenario { id: string; label: string; status: number; kind: ErrorKind; code: string; seconds: number | null; scope: 'model' | 'account' | 'provider'; local?: boolean }
const SCENARIOS: Scenario[] = [
  { id: 'rate', label: '429 · 有等待时间', status: 429, kind: 'rate_limit', code: 'rate_limit_error', seconds: 90, scope: 'model' },
  { id: 'rate-unknown', label: '429 · 没有时间', status: 429, kind: 'rate_limit', code: 'rate_limit_error', seconds: null, scope: 'model' },
  { id: 'quota', label: '429 · 额度用完', status: 429, kind: 'quota', code: 'usage_limit_reached', seconds: 21600, scope: 'account' },
  { id: 'quota-unknown', label: '429 · 额度重置未知', status: 429, kind: 'quota', code: 'quota_exhausted', seconds: null, scope: 'account' },
  { id: 'unsupported', label: '501 · 上游不支持', status: 501, kind: 'unsupported', code: 'unsupported', seconds: null, scope: 'provider' },
  { id: 'gateway', label: '501 · 网关缺能力', status: 501, kind: 'unsupported', code: 'gateway_capability_missing', seconds: null, scope: 'model', local: true },
  { id: 'transient', label: '503 · 服务异常', status: 503, kind: 'transient', code: 'service_unavailable', seconds: null, scope: 'model' },
  { id: 'shared', label: '429 · 共享限制', status: 429, kind: 'rate_limit', code: 'provider_rate_limit', seconds: 90, scope: 'provider' },
  { id: 'auth', label: '401 · 登录过期', status: 401, kind: 'auth', code: 'token_expired', seconds: null, scope: 'account' },
  { id: 'input', label: '413 · 请求太大', status: 413, kind: 'bad_request', code: 'payload_too_large', seconds: null, scope: 'model' },
  { id: 'content', label: '内容被拒绝', status: 400, kind: 'content', code: 'content_policy_violation', seconds: null, scope: 'model' },
  { id: 'unknown', label: '其他未识别错误', status: 418, kind: 'unknown', code: 'unknown_upstream_error', seconds: null, scope: 'model' },
  { id: 'balance', label: '402 · 余额不足', status: 402, kind: 'balance', code: 'insufficient_balance', seconds: null, scope: 'account' },
];
function ruleMatches(rule: ErrorRule, kind: ErrorKind, provider: string, model: string, code: string, status: number): boolean {
  return rule.enabled && rule.kind === kind && (rule.provider === '*' || rule.provider === provider)
    && (!rule.model || rule.model === model) && (!rule.code || rule.code === code) && (!rule.statuses.length || rule.statuses.includes(status));
}
function specificity(rule: ErrorRule): number { return (rule.model ? 8 : 0) + (rule.provider !== '*' ? 4 : 0) + (rule.code ? 2 : 0) + (rule.statuses.length ? 1 : 0); }
function simulateError(settings: ErrorSettings, input: ErrorPreviewInput, providerRules: PluginErrorRule[]) {
  const scenario = SCENARIOS.find(item => item.id === input.scenario) || SCENARIOS[0];
  const parsed = Number(input.customStatus?.trim());
  const status = Number.isInteger(parsed) && parsed >= 400 && parsed <= 599 ? parsed : scenario.status;
  const code = input.customCode?.trim() || scenario.code;
  const overriddenIds = new Set(settings.overrides.map(item => item.id));
  const tier1 = settings.overrides.find(item => ruleMatches(item, scenario.kind, input.provider, input.model, code, status));
  const tier2 = providerRules.filter(item => !overriddenIds.has(item.id) && !overriddenIds.has(item.pluginRuleId))
    .filter(item => ruleMatches(item, scenario.kind, input.provider, input.model, code, status)).sort((a, b) => specificity(b) - specificity(a))[0];
  const tier3 = settings.rules.find(item => item.enabled && item.kind === scenario.kind) || settings.rules.find(item => item.kind === 'unknown') || settings.rules[0];
  const rule = tier1 || tier2 || tier3;
  const result = { title: ACTION_LABEL[rule.action], stopped: false, cooldown: 0, deprioritize: 0, source: '不冷却', steps: [] as Array<{ title: string; detail: string }>,
    resendsLeft: Math.max(0, settings.resends - input.resendsUsed), switchesLeft: Math.max(0, settings.switches - input.switchesUsed) };
  const stop = (detail: string) => { result.title = '结束本次请求'; result.stopped = true; result.steps.push({ title: result.title, detail }); return result; };
  if (input.started) return stop('回答已经交付给客户端，不从头重发或换号拼接另一份回答。');
  if (input.uncertain) return stop('任务可能已执行。没有可靠的安全重放依据，不再次提交或换账号重复执行。');
  if (scenario.local) return stop('错误来自网关自身缺少能力。换其他账号或提供商不能补齐宿主功能。');
  result.steps.push({ title: '识别错误', detail: `HTTP ${status} · ${KIND_LABEL[scenario.kind]} · ${code}` });
  result.steps.push({ title: `${ruleLabel(rule)}（${tier1 ? '自定义规则' : tier2 ? '插件默认' : '全局规则'}）`, detail: ACTION_LABEL[rule.action] });
  const scopeOrder = { auto: 0, model: 0, account: 1, provider: 2 };
  const scope = rule.scope === 'auto' || scopeOrder[rule.scope] < scopeOrder[scenario.scope] ? scenario.scope : rule.scope;
  if (rule.cooldown === 'deprioritize') {
    let seconds = rule.fallbackSeconds;
    if (rule.exponential) seconds = Math.min(seconds * 2 ** Math.max(0, input.strikes - 1), rule.fallbackCapSeconds);
    result.deprioritize = seconds; result.source = '优先级调到最低（成功一次提前恢复）';
  } else {
    let cooling = rule.cooldown === 'protection' ? (settings.backoff ? settings.backoffSeconds : 0) : rule.cooldown === 'none' ? 0 : rule.fallbackSeconds;
    if (cooling && (rule.exponential || rule.cooldown === 'protection')) cooling = Math.min(cooling * 2 ** Math.max(0, input.strikes - 1), rule.cooldown === 'protection' ? settings.backoffCapSeconds : rule.fallbackCapSeconds);
    const floor = scenario.kind === 'rate_limit' || scenario.kind === 'quota' ? settings.cooldownFloorSeconds : 0;
    result.cooldown = cooling ? Math.max(floor, scenario.seconds || 0, rule.cooldown === 'upstream' && scenario.seconds !== null ? 0 : cooling) : 0;
    result.source = result.cooldown ? (scenario.seconds !== null ? '上游恢复时间' : '当前规则的兜底时间（不是上游重置时间）') : '不冷却';
  }
  const next = () => {
    if (result.deprioritize) result.steps.push({ title: `优先级调到最低（${duration(result.deprioritize)} 后恢复）`, detail: `${SCOPE_LABEL[scope]} · ${result.source}` });
    else if (result.cooldown) result.steps.push({ title: `记录 ${duration(result.cooldown)}冷却`, detail: `${SCOPE_LABEL[scope]} · ${result.source}` });
    if (rule.action === 'return_error') return stop('当前规则直接返回错误；必要的状态记录保留，不再发送。');
    if (input.pinned) return stop('这份请求已明确指定账号，不擅自更换指定对象；状态记录仍保留。');
    if (!result.switchesLeft) return stop('换号上限已经用完，429 同样受此上限约束。');
    result.title = scope === 'provider' || rule.action === 'switch_provider' ? '跳过提供商，继续选路' : '换账号，继续请求';
    result.steps.push({ title: result.title, detail: `选择当前路由中可用且不共享本次限制的对象，消耗 1 次换号次数，剩余 ${result.switchesLeft - 1} 次。` });
    return result;
  };
  if (rule.action === 'retry_then_switch') {
    const delay = Math.max(settings.intervalSeconds, scenario.seconds || 0);
    const count = Math.min(result.resendsLeft, delay ? Math.floor(settings.waitBudgetSeconds / delay) : result.resendsLeft);
    if (count) result.steps.push({ title: `原账号最多再发 ${count} 次`, detail: `重发间隔 ${duration(delay)}，累计等待不超过 ${settings.waitBudgetSeconds} 秒；所有账号共用重发次数。` });
    else result.steps.push({ title: '跳过原账号重发', detail: '重发次数已用完或等待超限，不缩短上游时间提前发送。' });
  }
  if (rule.action === 'refresh_then_switch' && result.resendsLeft && settings.intervalSeconds <= settings.waitBudgetSeconds) result.steps.push({ title: '先续期一次', detail: '仅使用已授权的有效续期材料；续期后的再发送扣 1 次原账号重发次数。' });
  return next();
}

export function ErrorPreviewPanel({ settings, providers, providerRules = [], input, onChange }: {
  settings: ErrorSettings; providers: Provider[]; providerRules?: PluginErrorRule[]; input: ErrorPreviewInput; onChange(input: ErrorPreviewInput): void;
}): JSX.Element {
  const [more, setMore] = useState(false);
  const patch = (next: Partial<ErrorPreviewInput>) => onChange({ ...input, ...next });
  const result = simulateError(settings, input, providerRules);
  return <div className="rt-group-card">
    <div className="rt-group-head"><Route size={15} /><div className="rt-group-title"><b>处理预览</b></div><Badge tone="neutral">模拟</Badge></div>
    <Choice label="选择模拟错误" value={input.scenario} options={SCENARIOS.map(item => ({ value: item.id, label: item.label }))} onChange={scenario => patch({ scenario })} />
    <div style={{ padding: 10, borderRadius: 8, background: 'var(--hover-bg)', color: result.stopped ? 'var(--danger-color)' : 'var(--primary-color)', margin: '8px 0' }}><strong>{result.title}</strong></div>
    {result.cooldown ? <div style={{ display: 'flex', gap: 8, marginBottom: 10 }}><Clock3 size={15} /><div><strong>{duration(result.cooldown)}后恢复参与</strong><span className="form-hint" style={{ display: 'block' }}>{result.source}</span></div></div> : null}
    {result.deprioritize ? <div style={{ display: 'flex', gap: 8, marginBottom: 10 }}><Clock3 size={15} /><div><strong>优先级调到最低（{duration(result.deprioritize)} 后恢复）</strong><span className="form-hint" style={{ display: 'block' }}>{result.source}</span></div></div> : null}
    <ol style={{ listStyle: 'none', padding: 0, margin: 0 }}>{result.steps.map((step, index) => <li key={index} style={{ display: 'flex', gap: 8, padding: '7px 0', borderBottom: '1px solid var(--border-color)' }}><Badge tone="neutral">{index + 1}</Badge><div><strong>{step.title}</strong><span className="form-hint" style={{ display: 'block' }}>{step.detail}</span></div></li>)}</ol>
    <div style={{ display: 'flex', justifyContent: 'space-between', gap: 8, fontSize: 12, color: 'var(--text-muted)', margin: '10px 0' }}><span>原账号可重发 {result.resendsLeft} 次</span><span>还可换 {result.switchesLeft} 个账号</span></div>
    <Toggle label="已经开始输出" checked={input.started} onChange={started => patch({ started })} />
    <Toggle label="任务结果不确定" checked={input.uncertain} onChange={uncertain => patch({ uncertain })} />
    <button type="button" className="rt-seg-btn" style={{ width: '100%', marginTop: 10, display: 'flex', alignItems: 'center', gap: 6 }} onClick={() => setMore(!more)}><Settings2 size={14} />更多模拟条件<ChevronDown size={13} style={{ marginLeft: 'auto', transform: more ? 'rotate(180deg)' : undefined }} /></button>
    {more ? <div style={{ marginTop: 10 }}><Choice label="模拟提供商" value={input.provider} options={providers.map(item => ({ value: item.id, label: item.label }))} onChange={provider => patch({ provider })} />
      <TextField label="模拟模型" value={input.model} onChange={model => patch({ model })} />
      <div style={fieldStyle}><TextField label="指定状态码" placeholder="默认" value={input.customStatus || ''} onChange={customStatus => patch({ customStatus })} /><TextField label="指定业务码" placeholder="默认" value={input.customCode || ''} onChange={customCode => patch({ customCode })} /></div>
      <Toggle label="明确指定账号" checked={input.pinned} onChange={pinned => patch({ pinned })} />
      <div style={fieldStyle}><NumberField label="已经换过的账号数" value={input.switchesUsed} max={100} onChange={switchesUsed => patch({ switchesUsed })} /><NumberField label="已经重发的次数" value={input.resendsUsed} max={10} onChange={resendsUsed => patch({ resendsUsed })} /><NumberField label="连续失败次数" value={input.strikes} min={1} max={10} onChange={strikes => patch({ strikes })} /></div>
    </div> : null}
    <span className="form-hint" style={{ display: 'block', marginTop: 12 }}>只演示当前规则，不发送请求，也不读取真实账号。</span>
  </div>;
}
