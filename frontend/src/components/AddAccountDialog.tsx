import { useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api';
import { presetIcon, providerIcon } from '../lib/providerIcons';
import {
  ADD_METHOD_LABELS,
  addConfigOf,
  CUSTOM_PROTOCOLS,
  normalizeUrl,
  PRESET_PROVIDERS,
  presetKind,
  type AddField,
  type AddMethodId,
  type ProviderAddConfig,
} from '../lib/addAccountConfigs';
import { useToast } from './Toast';
import { Button, Input, Select, Tabs, Textarea } from './ui';
import type { AccountProviderItem, ChannelItem } from '../types';

type Step = 'pick' | 'form';
type Segment = 'agent' | 'preset' | 'custom';

export interface AddAccountDialogProps {
  providers: AccountProviderItem[];
  channels: ChannelItem[];
  defaultChannelId: string;
  onClose: () => void;
  onCreated: () => Promise<void> | void;
}

interface AdvancedState {
  name: string;
  channel_id: string;
  base_url: string;
  chat_path: string;
  priority: string;
  weight: string;
  max_concurrency: string;
  proxy: string;
  models: string;
  extra_headers: string;
  expires_at: string;
  status: string;
}

const EMPTY_ADVANCED: AdvancedState = {
  name: '',
  channel_id: '',
  base_url: '',
  chat_path: '',
  priority: '0',
  weight: '1',
  max_concurrency: '3',
  proxy: '',
  models: '',
  extra_headers: '',
  expires_at: '',
  status: 'enabled',
};

function parseHeaders(raw: string): Record<string, string> {
  const out: Record<string, string> = {};
  for (const line of raw.split('\n')) {
    const idx = line.indexOf(':');
    if (idx <= 0) continue;
    const name = line.slice(0, idx).trim();
    const value = line.slice(idx + 1).trim();
    if (name) out[name] = value;
  }
  return out;
}

function splitModels(raw: string): string[] {
  return raw
    .split(/[,\s]+/)
    .map((m) => m.trim())
    .filter((m) => m.length > 0);
}

/** 从 CodeArts 的整份凭据 JSON 里剥出 AK/SK/STS（网关只认 `AK:SK[:STS]`）。 */
function codeartsCredential(raw: string): string {
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    throw new Error('凭据 JSON 不是合法 JSON');
  }
  if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) {
    throw new Error('凭据 JSON 要是一个对象');
  }
  const root = parsed as Record<string, unknown>;
  const nested = root.codearts_provider_credential;
  const source =
    nested && typeof nested === 'object' && !Array.isArray(nested)
      ? (nested as Record<string, unknown>)
      : root;
  const ak = String(source.access_key_id ?? source.accessKeyId ?? '').trim();
  const sk = String(source.secret_access_key ?? source.secretAccessKey ?? '').trim();
  const sts = String(source.security_token ?? source.securityToken ?? '').trim();
  if (!ak || !sk) throw new Error('凭据 JSON 缺少 access_key_id / secret_access_key');
  return sts ? `${ak}:${sk}:${sts}` : `${ak}:${sk}`;
}

function base64Of(value: string): string {
  // 只用于 ASCII 的 key:secret；避免 btoa 对非 Latin1 抛异常
  try {
    return btoa(value);
  } catch {
    return value;
  }
}

/** 手填字段 → 账号请求体（逐家差异只收在这一处） */
function buildCredential(config: ProviderAddConfig, values: Record<string, string>): string {
  if (config.provider === 'codearts') {
    return codeartsCredential(values.credentialJson || '');
  }
  if (config.provider === 'officeace') {
    const key = (values.modelAppKey || '').trim();
    const secret = (values.modelAppSecret || '').trim();
    if (!key || !secret) throw new Error('请填写网关 Key 与网关 Secret');
    return base64Of(`${key}:${secret}`);
  }
  const field = config.fields.find((f) => f.target === 'credential');
  if (!field) return '';
  return (values[field.key] || '').trim();
}

export default function AddAccountDialog({
  providers,
  channels,
  defaultChannelId,
  onClose,
  onCreated,
}: AddAccountDialogProps): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();

  const [step, setStep] = useState<Step>('pick');
  const [segment, setSegment] = useState<Segment>('agent');
  const [search, setSearch] = useState('');
  const [provider, setProvider] = useState('');
  const [baseUrlHint, setBaseUrlHint] = useState('');
  const [method, setMethod] = useState<AddMethodId>('manual');
  const [values, setValues] = useState<Record<string, string>>({});
  const [advanced, setAdvanced] = useState<AdvancedState>({
    ...EMPTY_ADVANCED,
    channel_id: defaultChannelId,
  });
  const [rawHeaders, setRawHeaders] = useState('');
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState('');

  const selectedProvider = providers.find((p) => p.kind === provider);
  const addConfig = addConfigOf(provider);
  const webConfig = addConfig?.web ?? null;
  const label =
    addConfig?.label
    || selectedProvider?.label
    || CUSTOM_PROTOCOLS.find((c) => c.kind === provider)?.labelKey
    || provider;

  const providerLabel = (p: AccountProviderItem): string => p.label || p.kind;

  const builtinUrls = useMemo(
    () => new Set(providers.map((p) => normalizeUrl(p.default_base_url || ''))),
    [providers],
  );
  const needle = search.trim().toLowerCase();
  const agentCards = providers.filter(
    (p) =>
      !needle
      || providerLabel(p).toLowerCase().includes(needle)
      || p.kind.toLowerCase().includes(needle)
      || (p.default_base_url || '').toLowerCase().includes(needle),
  );
  const presetCards = PRESET_PROVIDERS.filter((p) => !builtinUrls.has(normalizeUrl(p.base_url))).filter(
    (p) => !needle || p.name.toLowerCase().includes(needle) || p.base_url.toLowerCase().includes(needle),
  );

  useEffect(() => {
    // 弹窗打开时复位渠道选择（外层可能在筛选后重开）
    setAdvanced((prev) => ({ ...prev, channel_id: defaultChannelId || channels[0]?.id || '' }));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const pick = (kind: string, baseUrl: string, config?: ProviderAddConfig): void => {
    setProvider(kind);
    setBaseUrlHint(baseUrl);
    setValues({});
    setRawHeaders('');
    setError('');
    setAdvanced((prev) => ({
      ...EMPTY_ADVANCED,
      channel_id: prev.channel_id,
      base_url: baseUrl,
    }));
    const methods = config?.methods?.length ? config.methods : (['manual'] as AddMethodId[]);
    setMethod(methods[0]);
    setStep('form');
  };

  const setValue = (key: string, value: string): void => {
    setValues((prev) => ({ ...prev, [key]: value }));
  };

  /** 抓包请求头 → 凭证 + 额外请求头（与旧版行为一致） */
  const applyRawHeaders = (): void => {
    const extra = parseHeaders(advanced.extra_headers);
    let credential = '';
    let credKey = '';
    if (addConfig) {
      const field = addConfig.fields.find((f) => f.target === 'credential');
      if (field) {
        credKey = field.key;
        credential = values[field.key] || '';
      }
    }
    for (const line of rawHeaders.split('\n')) {
      const idx = line.indexOf(':');
      if (idx <= 0) continue;
      const rawName = line.slice(0, idx).trim();
      const value = line.slice(idx + 1).trim();
      if (!rawName || !value) continue;
      const lower = rawName.toLowerCase();
      if (lower === 'cookie') {
        credential = value;
        continue;
      }
      if (lower === 'authorization') {
        const bearer = /^bearer\s+(.+)$/i.exec(value);
        if (bearer) {
          credential = bearer[1].trim();
          continue;
        }
      }
      extra[rawName] = value;
    }
    if (!credKey) credKey = 'credential';
    setValues((prev) => ({ ...prev, [credKey]: credential }));
    setAdvanced((prev) => ({
      ...prev,
      extra_headers: Object.entries(extra)
        .map(([k, v]) => `${k}: ${v}`)
        .join('\n'),
    }));
    setRawHeaders('');
    addToast(t('已填充凭证与额外请求头'));
  };

  const submit = async (): Promise<void> => {
    setError('');
    if (!provider) {
      setError(t('请选择供应商'));
      return;
    }
    if (addConfig && method !== 'manual') {
      setError(t('网页登录完成后，请切到「填写凭证」把登录态粘进来再添加'));
      return;
    }
    // 必填校验（json 字段按整段校验）
    if (method === 'manual' && addConfig) {
      for (const field of addConfig.fields) {
        if (field.optional) continue;
        if (!(values[field.key] || '').trim()) {
          setError(t('请填写 {label}', { label: field.label }));
          return;
        }
      }
    }
    if (selectedProvider?.requires_base_url && !(advanced.base_url || baseUrlHint).trim()) {
      setError(t('该供应商需要填写上游地址'));
      return;
    }
    setSaving(true);
    try {
      let credential = '';
      if (method === 'manual' || !addConfig) {
        credential = addConfig ? buildCredential(addConfig, values) : (values.credential || '').trim();
      }
      let channelId = advanced.channel_id;
      if (!channelId) {
        const created = await api.addChannel({
          name: `${label} ${t('账号池')}`.trim(),
          channel_type: 'account',
          base_url: '',
          models: [],
        });
        channelId = created?.data?.id != null ? String(created.data.id) : '';
        if (!channelId) throw new Error(t('创建账号型渠道失败'));
      }
      const headers = parseHeaders(advanced.extra_headers);
      if (addConfig) {
        for (const field of addConfig.fields) {
          if (field.target !== 'header') continue;
          const value = (values[field.key] || '').trim();
          if (value) headers[field.header || field.key] = value;
        }
      }
      const nameField = addConfig?.fields.find((f) => f.target === 'name');
      const payload: Record<string, unknown> = {
        channel_id: channelId,
        name: (nameField ? values[nameField.key] || '' : advanced.name).trim(),
        provider,
        base_url: (advanced.base_url || baseUrlHint).trim(),
        chat_path: advanced.chat_path.trim(),
        status: advanced.status,
        priority: Number(advanced.priority || 0),
        weight: Math.max(1, Number(advanced.weight || 1)),
        max_concurrency: Math.max(0, Number(advanced.max_concurrency || 0)),
        models: splitModels(advanced.models),
        extra_headers: headers,
        expires_at: advanced.expires_at.trim() ? Number(advanced.expires_at) : null,
        proxy: advanced.proxy.trim(),
      };
      if (method === 'manual' || !addConfig) {
        payload.api_key = credential;
      }
      await api.createAccountSession(payload);
      addToast(t('账号已创建'));
      await onCreated();
      onClose();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  };

  const cardIcon = (kind: string): string | null => providerIcon(kind);

  const methods: AddMethodId[] = addConfig?.methods?.length ? addConfig.methods : ['manual'];

  return (
    <div className="add-account-root">
      {/* 第 1 步：选供应商 */}
      {step === 'pick' ? (
        <div className="modal-overlay">
          <div className="modal add-modal-wide">
            <div className="modal-header">
              <h3>{t('添加账号')}</h3>
              <button type="button" className="modal-close" onClick={onClose}>
                &times;
              </button>
            </div>
            <div className="modal-body">
              <div className="add-pick-row">
                <Tabs<Segment>
                  items={[
                    { key: 'agent', label: 'Agent' },
                    { key: 'preset', label: t('预置 API') },
                    { key: 'custom', label: t('自定义') },
                  ]}
                  active={segment}
                  onChange={(key) => {
                    setSegment(key);
                    setSearch('');
                  }}
                  ariaLabel={t('供应商类型')}
                />
                <div className="add-search">
                  <Input
                    placeholder={t('搜索提供商…')}
                    value={search}
                    onChange={(e) => setSearch(e.target.value)}
                  />
                </div>
              </div>
              <div className="add-cards" role="listbox" aria-label={t('供应商列表')}>
                {segment === 'agent' &&
                  agentCards.map((p) => (
                    <button
                      type="button"
                      key={p.kind}
                      className="add-card"
                      onClick={() => pick(p.kind, p.default_base_url || '', addConfigOf(p.kind))}
                    >
                      {cardIcon(p.kind) ? (
                        <img className="add-card-logo" src={cardIcon(p.kind) as string} alt="" loading="lazy" />
                      ) : (
                        <span className="add-card-initial">{p.kind.slice(0, 1).toUpperCase()}</span>
                      )}
                      <span className="add-card-info">
                        <span className="add-card-name">
                          {providerLabel(p)}
                          {p.auth ? <span className="add-card-badge">{t('登录态')}</span> : null}
                          {p.native_supported ? <span className="add-card-badge">{t('专用适配')}</span> : null}
                        </span>
                        <span className="add-card-meta">{p.default_base_url || t('需自填地址')}</span>
                      </span>
                      <span className="add-card-go">›</span>
                    </button>
                  ))}
                {segment === 'preset' &&
                  presetCards.map((p) => (
                    <button
                      type="button"
                      key={p.key}
                      className="add-card"
                      onClick={() => pick(presetKind(p.protocol), p.base_url)}
                    >
                      {presetIcon(p.key) ? (
                        <img
                          className="add-card-logo"
                          src={presetIcon(p.key) as string}
                          alt=""
                          loading="lazy"
                        />
                      ) : (
                        <span className="add-card-initial">{p.name.slice(0, 1).toUpperCase()}</span>
                      )}
                      <span className="add-card-info">
                        <span className="add-card-name">{p.name}</span>
                        <span className="add-card-meta">{p.base_url}</span>
                      </span>
                      <span className="add-card-go">›</span>
                    </button>
                  ))}
                {segment === 'custom' &&
                  CUSTOM_PROTOCOLS.map((p) => (
                    <button
                      type="button"
                      key={p.kind}
                      className="add-card"
                      onClick={() => pick(p.kind, '')}
                    >
                      <span className="add-card-initial">{p.kind.slice(0, 1).toUpperCase()}</span>
                      <span className="add-card-info">
                        <span className="add-card-name">{t(p.labelKey)}</span>
                        <span className="add-card-meta">{t(p.hintKey)}</span>
                      </span>
                      <span className="add-card-go">›</span>
                    </button>
                  ))}
                {((segment === 'agent' && !agentCards.length)
                  || (segment === 'preset' && !presetCards.length)) && (
                  <div className="add-cards-empty">{t('没有匹配的供应商')}</div>
                )}
              </div>
            </div>
          </div>
        </div>
      ) : null}

      {/* 第 2 步：选方式 + 填凭证 */}
      {step === 'form' ? (
        <div className="modal-overlay">
          <div className="modal add-modal-form">
            <div className="modal-header">
              <h3>
                {addConfig ? t('登录 / 添加 {name} 账号', { name: label }) : t('添加 {name} 账号', { name: label })}
              </h3>
              <button type="button" className="modal-close" onClick={onClose}>
                &times;
              </button>
            </div>
            <div className="modal-body">
              {addConfig ? (
                <>
                  <div className="add-method-row">
                    <span className="add-method-label">{t('添加方式')}</span>
                    <Tabs<AddMethodId>
                      items={methods.map((id) => ({ key: id, label: ADD_METHOD_LABELS[id] }))}
                      active={method}
                      onChange={setMethod}
                      ariaLabel={t('{label} 账号的添加方式', { label })}
                    />
                  </div>

                  {method === 'web' && webConfig ? (
                    <div className="add-block">
                      <h4>{t('网页登录')}</h4>
                      <p className="form-hint">{webConfig.note}</p>
                      <div className="add-btn-row">
                        <Button
                          type="button"
                          onClick={() => window.open(webConfig.url, '_blank', 'noopener,noreferrer')}
                        >
                          {webConfig.button}
                        </Button>
                        {webConfig.hint ? <span className="form-hint">{webConfig.hint}</span> : null}
                        <Button type="button" variant="outline" onClick={() => setMethod('manual')}>
                          {t('已登录，去填写凭证')}
                        </Button>
                      </div>
                    </div>
                  ) : null}

                  {method === 'sms' && addConfig.sms ? (
                    <div className="add-block">
                      <h4>{t('手机验证码登录')}</h4>
                      <p className="form-hint">{addConfig.sms.note}</p>
                    </div>
                  ) : null}

                  {method === 'manual' ? (
                    <div className="add-block">
                      <h4>{addConfig.manualTitle}</h4>
                      <p className="form-hint">{addConfig.manualNote}</p>
                      {addConfig.fields.map((field) => (
                        <FieldInput
                          key={field.key}
                          field={field}
                          value={values[field.key] || ''}
                          onChange={(v) => setValue(field.key, v)}
                        />
                      ))}
                    </div>
                  ) : null}

                  {method === 'manual' ? (
                    <div className="add-block">
                      <h4>{t('从抓包请求头导入（可选）')}</h4>
                      <Textarea
                        rows={3}
                        placeholder={'Cookie: sessionid=...\nAuthorization: Bearer eyJ...\nx-token: ...'}
                        value={rawHeaders}
                        onChange={(e) => setRawHeaders(e.target.value)}
                      />
                      <div className="add-btn-row">
                        <Button
                          type="button"
                          variant="outline"
                          size="sm"
                          onClick={applyRawHeaders}
                          disabled={!rawHeaders.trim()}
                        >
                          {t('解析并填充凭证与请求头')}
                        </Button>
                      </div>
                      <p className="form-hint">
                        {t('从浏览器开发者工具「请求头」整段复制：Cookie / Bearer 自动填入凭证，其余进入额外请求头。')}
                      </p>
                    </div>
                  ) : null}
                </>
              ) : (
                <div className="add-block">
                  <h4>{t('填写凭证')}</h4>
                  <p className="form-hint">{selectedProvider?.credential_hint || ''}</p>
                  <FieldInput
                    field={{
                      key: 'credential',
                      label:
                        selectedProvider?.auth === 'cookie'
                          ? t('登录 Cookie')
                          : selectedProvider?.auth === 'bearer'
                            ? t('访问令牌（JWT）')
                            : t('API Key'),
                      rows: 3,
                      placeholder: 'sk-...',
                      target: 'credential',
                    }}
                    value={values.credential || ''}
                    onChange={(v) => setValue('credential', v)}
                  />
                  <div className="add-block">
                    <h4>{t('从抓包请求头导入（可选）')}</h4>
                    <Textarea
                      rows={3}
                      placeholder={'Cookie: sessionid=...\nAuthorization: Bearer eyJ...'}
                      value={rawHeaders}
                      onChange={(e) => setRawHeaders(e.target.value)}
                    />
                    <div className="add-btn-row">
                      <Button
                        type="button"
                        variant="outline"
                        size="sm"
                        onClick={applyRawHeaders}
                        disabled={!rawHeaders.trim()}
                      >
                        {t('解析并填充凭证与请求头')}
                      </Button>
                    </div>
                  </div>
                </div>
              )}

              <details className="add-advanced">
                <summary>{t('账号设置（渠道 / 优先级 / 并发等）')}</summary>
                <div className="add-advanced-body">
                  {channels.length > 0 ? (
                    <Select
                      label={t('渠道')}
                      value={advanced.channel_id}
                      onChange={(e) => setAdvanced({ ...advanced, channel_id: e.target.value })}
                    >
                      {channels.map((c) => (
                        <option key={c.id} value={c.id}>
                          {c.name}
                        </option>
                      ))}
                    </Select>
                  ) : (
                    <p className="form-hint">{t('还没有账号型渠道：保存时会按所选供应商自动创建一个')}</p>
                  )}
                  {!addConfig ? (
                    <Input
                      label={t('账号名（可选）')}
                      placeholder="account-1"
                      value={advanced.name}
                      onChange={(e) => setAdvanced({ ...advanced, name: e.target.value })}
                    />
                  ) : null}
                  <Input
                    label={t('上游地址（可选，留空用 provider 默认）')}
                    placeholder={selectedProvider?.default_base_url || baseUrlHint || 'https://api.deepseek.com'}
                    value={advanced.base_url}
                    onChange={(e) => setAdvanced({ ...advanced, base_url: e.target.value })}
                  />
                  <Input
                    label={t('对话路径（可选，默认 /chat/completions）')}
                    placeholder={selectedProvider?.chat_path || '/api/chat'}
                    value={advanced.chat_path}
                    onChange={(e) => setAdvanced({ ...advanced, chat_path: e.target.value })}
                  />
                  <div className="add-two-col">
                    <Input
                      label={t('优先级')}
                      type="number"
                      value={advanced.priority}
                      onChange={(e) => setAdvanced({ ...advanced, priority: e.target.value })}
                    />
                    <Input
                      label={t('权重')}
                      type="number"
                      min="1"
                      value={advanced.weight}
                      onChange={(e) => setAdvanced({ ...advanced, weight: e.target.value })}
                    />
                  </div>
                  <Input
                    label={t('并发上限')}
                    type="number"
                    min="0"
                    hint={t('(0 = 不限)')}
                    value={advanced.max_concurrency}
                    onChange={(e) => setAdvanced({ ...advanced, max_concurrency: e.target.value })}
                  />
                  <Input
                    label={t('模型白名单（可选，逗号分隔）')}
                    placeholder="deepseek-chat, deepseek-reasoner"
                    value={advanced.models}
                    onChange={(e) => setAdvanced({ ...advanced, models: e.target.value })}
                  />
                  <Textarea
                    label={t('额外请求头（每行 Key: Value）')}
                    rows={3}
                    placeholder={'origin: https://example.com\nx-token: {credential}'}
                    value={advanced.extra_headers}
                    onChange={(e) => setAdvanced({ ...advanced, extra_headers: e.target.value })}
                  />
                  <Input
                    label={t('出口代理（可选）')}
                    placeholder="socks5://127.0.0.1:1080"
                    value={advanced.proxy}
                    onChange={(e) => setAdvanced({ ...advanced, proxy: e.target.value })}
                  />
                  <Input
                    label={t('凭证到期时间（可选）')}
                    type="number"
                    value={advanced.expires_at}
                    onChange={(e) => setAdvanced({ ...advanced, expires_at: e.target.value })}
                  />
                  <Select
                    label={t('状态')}
                    value={advanced.status}
                    onChange={(e) => setAdvanced({ ...advanced, status: e.target.value })}
                  >
                    <option value="enabled">{t('启用')}</option>
                    <option value="disabled">{t('停用')}</option>
                  </Select>
                </div>
              </details>

              {error ? <div className="error-message">{error}</div> : null}
            </div>
            <div className="modal-footer">
              <Button variant="secondary" type="button" onClick={() => setStep('pick')}>
                {t('返回')}
              </Button>
              <div style={{ flex: 1 }} />
              <Button variant="secondary" type="button" onClick={onClose}>
                {t('取消')}
              </Button>
              <Button type="button" onClick={() => void submit()} disabled={saving}>
                {saving ? t('保存中…') : t('添加账号')}
              </Button>
            </div>
          </div>
        </div>
      ) : null}
    </div>
  );
}

/** 单个手填字段（多行/单行按 rows 判断） */
function FieldInput({
  field,
  value,
  onChange,
}: {
  field: AddField;
  value: string;
  onChange: (value: string) => void;
}): JSX.Element {
  const { t } = useTranslation();
  const label = `${field.label}${field.optional ? '' : t('（必填）')}`;
  if (field.rows) {
    return (
      <Textarea
        label={label}
        rows={field.rows}
        placeholder={field.placeholder}
        value={value}
        onChange={(e) => onChange(e.target.value)}
      />
    );
  }
  return (
    <Input
      label={label}
      placeholder={field.placeholder}
      maxLength={field.key === 'name' ? 100 : 256}
      value={value}
      onChange={(e) => onChange(e.target.value)}
    />
  );
}
