import { Fragment, useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api';
import { presetIcon, providerIcon } from '../lib/providerIcons';
import {
  ADD_METHOD_LABELS,
  addConfigOf,
  CUSTOM_PROTOCOLS,
  editionOf,
  effectiveProvider,
  isCardHidden,
  normalizeUrl,
  PRESET_PROVIDERS,
  presetKind,
  type AddField,
  type AddMethodId,
  type PresetProviderItem,
  type ProviderAddConfig,
} from '../lib/addAccountConfigs';
import { useToast } from './Toast';
import ProxySelector from './ProxySelector';
import { Button, Input, Select, Tabs, Textarea } from './ui';
import type { AccountProviderItem, AccountSessionItem, ChannelItem } from '../types';
import type { DesktopSourceItem } from '../types/desktop';

type Step = 'pick' | 'form';
/** 第 1 步分档（对齐 Agent2API add-provider-pick）：全部 / Agent / Web / API 模板 / 已建 API */
type Segment = 'all' | 'agent' | 'web' | 'preset' | 'existing';
type GroupSegment = Exclude<Segment, 'all'>;
/** 供应商形态分类（后端 C1 起在 providers 下发 category 字段） */
type ProviderCategory = 'agent' | 'web' | 'api';
/** 「新建自定义提供商」的协议（原三张协议卡的 kind） */
type CustomProtocolKind = 'openai_compatible' | 'anthropic' | 'gemini';

const GROUPED_SEGMENTS: GroupSegment[] = ['agent', 'web', 'preset', 'existing'];

/** 供应商形态分类：后端带 category 时直接用；旧后端没有该字段时回退
 *  「登录态型（auth != null）= agent、API Key 型 = api」，Web 档自然空态降级。 */
function categoryOf(p: AccountProviderItem | undefined): ProviderCategory {
  if (!p) return 'api';
  if (p.category === 'web' || p.category === 'agent' || p.category === 'api') {
    return p.category;
  }
  return p.auth != null ? 'agent' : 'api';
}

/** 协议下拉的展示名（CUSTOM_PROTOCOLS 的 labelKey 带「（自定义地址）」后缀，下拉里用短名） */
const CUSTOM_PROTOCOL_LABELS: Record<string, string> = {
  openai_compatible: 'OpenAI 兼容',
  anthropic: 'Anthropic 兼容',
  gemini: 'Gemini 兼容',
};
/** Base URL 占位随协议变化（对齐上游 add-custom-provider.tsx 的提示语义） */
const CUSTOM_BASE_PLACEHOLDER: Record<string, string> = {
  openai_compatible: 'https://open.bigmodel.cn/api/paas/v4',
  anthropic: 'https://api.anthropic.com',
  gemini: 'https://generativelanguage.googleapis.com/v1beta',
};
/** Base URL 提示随协议变化：Anthropic 填根地址、OpenAI 兼容带 /v1、Gemini 走 /models */
const CUSTOM_BASE_HINT: Record<string, string> = {
  openai_compatible: 'OpenAI 兼容填到 /v1，按 {base}/chat/completions 转发',
  anthropic: 'Anthropic 协议填根地址，不要带 /v1，按 {base}/v1/messages 转发',
  gemini: 'Gemini 协议填到 /v1beta，按 {base}/models 转发',
};

/** 「已建 API」档卡片：现有账号池（account 型渠道） */
interface PoolCard {
  id: string;
  name: string;
  /** 池内首个账号的 provider kind（空池为 ''） */
  provider: string;
  count: number;
}

export interface AddAccountDialogProps {
  providers: AccountProviderItem[];
  /** 现有账号：给「已建 API」档推断各账号池的供应商，保存时按供应商复用账号池 */
  sessions: AccountSessionItem[];
  /** account 型渠道（账号池）清单：「已建 API」档的数据源 */
  channels: ChannelItem[];
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
  sessions,
  channels,
  onClose,
  onCreated,
}: AddAccountDialogProps): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();

  const [step, setStep] = useState<Step>('pick');
  const [segment, setSegment] = useState<Segment>('all');
  const [search, setSearch] = useState('');
  const [provider, setProvider] = useState('');
  /** 卡内「账号版本」档位（如 WorkBuddy 的 国内版/国际版），空 = 用 defaults */
  const [edition, setEdition] = useState('');
  const [baseUrlHint, setBaseUrlHint] = useState('');
  /** 预置卡的地址栏占位提示（Sub2API/NewAPI 等自建面板没有固定地址） */
  const [baseUrlPlaceholder, setBaseUrlPlaceholder] = useState('');
  /** 「该上游无需鉴权」（预置卡 no_auth 预勾：Ollama / OpenCode Zen 免费档） */
  const [noAuth, setNoAuth] = useState(false);
  const [method, setMethod] = useState<AddMethodId>('manual');
  const [values, setValues] = useState<Record<string, string>>({});
  const [advanced, setAdvanced] = useState<AdvancedState>({ ...EMPTY_ADVANCED, channel_id: '' });
  const [rawHeaders, setRawHeaders] = useState('');
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState('');
  // 「新建自定义提供商」：三张协议卡合成一个协议下拉（对齐上游 add-custom-provider）
  const [customNew, setCustomNew] = useState(false);
  const [protocol, setProtocol] = useState<CustomProtocolKind>('openai_compatible');
  const [customName, setCustomName] = useState('');
  // 「已建 API」档：点的是现有账号池卡（渠道归属随之固定）
  const [poolName, setPoolName] = useState('');
  const [needsProviderPick, setNeedsProviderPick] = useState(false);
  const [channelExplicit, setChannelExplicit] = useState(false);
  // 网页登录：任务 state / 等待态 / 回调地址（远程部署兜底）
  const [loginState, setLoginState] = useState('');
  const [loginWaiting, setLoginWaiting] = useState(false);
  const [loginError, setLoginError] = useState('');
  const [callbackUrl, setCallbackUrl] = useState('');
  const pollRef = useRef<number | null>(null);
  // 手机验证码登录（Loomy）
  const [smsPhone, setSmsPhone] = useState('');
  const [smsCode, setSmsCode] = useState('');
  const [smsMsgid, setSmsMsgid] = useState('');
  const [smsBusy, setSmsBusy] = useState(false);
  const [smsHint, setSmsHint] = useState('');
  // 粘贴式登录（网关归一化 + 校验一次后落账号）
  const [pasteBusy, setPasteBusy] = useState(false);
  const [pasteHint, setPasteHint] = useState('');
  /** Cookies SQLite 文件（#41）：chatglm/kuku 桌面客户端 Cookies 库 base64，
   *  随 login/paste body 的 auth_file_b64 提交（后端 account_login.rs:199-215
   *  专用分支：解析出令牌 + 续期令牌一并落库）。非这两家不显示入口。 */
  const [cookieFileB64, setCookieFileB64] = useState('');
  const [cookieFileName, setCookieFileName] = useState('');
  // 桌面客户端登录态导入（#22 desktop 段；provider 无关——后端白名单分发 +
  // auto_channel 原子建号。supported 由后端按 cfg!(windows)+encrypted 给）
  const [desktopPick, setDesktopPick] = useState(false);
  const [desktopSources, setDesktopSources] = useState<DesktopSourceItem[] | null>(null);
  const [desktopName, setDesktopName] = useState('');
  const [desktopBusyId, setDesktopBusyId] = useState('');
  const [desktopError, setDesktopError] = useState('');
  // 重复提供商检测（#22）：新建自定义提供商提交时命中同协议+同归一化地址 →
  // 警告条二次确认（AIGX 本就自动并入同池，这里是把行为显式告知）
  const [dupAck, setDupAck] = useState(false);

  const selectedProvider = providers.find((p) => p.kind === provider);
  const addConfig = addConfigOf(provider);
  /** 卡内版本档位（无 editions 时 undefined） */
  const selectedEdition = addConfig?.editions?.length ? editionOf(provider, edition) : undefined;
  /** 真正落账号 / 起登录的 provider id：版本卡把档位解析成 workbuddy / workbuddy-intl */
  const activeProvider = effectiveProvider(provider, edition);
  const baseWeb = addConfig?.web ?? null;
  const webConfig = baseWeb && selectedEdition
    ? {
        ...baseWeb,
        url: selectedEdition.url || baseWeb.url,
        note: selectedEdition.note || baseWeb.note,
        hint: selectedEdition.hint ?? baseWeb.hint,
        button: selectedEdition.button || baseWeb.button,
      }
    : baseWeb;
  const label =
    addConfig?.label
    || selectedProvider?.label
    || CUSTOM_PROTOCOLS.find((c) => c.kind === provider)?.labelKey
    || provider;

  const providerLabel = (p: AccountProviderItem): string => p.label || p.kind;

  const needle = search.trim().toLowerCase();
  /** 跨字段命中（搜索为空 = 全命中） */
  const hit = (...haystacks: string[]): boolean =>
    !needle || haystacks.some((h) => h.toLowerCase().includes(needle));

  // 判重口径：只统计非 API Key 型家的地址（对齐 Agent2API：API Key 型内置不单独
  // 成卡，预置段的 OpenAI/HuggingFace 等卡不该被后端目录判重滤掉）
  const builtinUrls = useMemo(
    () => new Set(
      providers
        .filter((p) => categoryOf(p) !== 'api')
        .map((p) => normalizeUrl(p.default_base_url || '')),
    ),
    [providers],
  );
  // Agent 档：登录态家（后端 category === 'agent'；旧后端回退 auth != null）
  const agentCards = providers.filter(
    (p) =>
      categoryOf(p) === 'agent'
      && !isCardHidden(p.kind)
      && hit(providerLabel(p), p.kind, p.default_base_url || ''),
  );
  // Web 档：网页形态家（后端 category === 'web'；后端未就绪时为空 → 空态降级）
  const webCards = providers.filter(
    (p) =>
      categoryOf(p) === 'web'
      && !isCardHidden(p.kind)
      && hit(providerLabel(p), p.kind, p.default_base_url || ''),
  );
  // API 模板档：纯前端常量表（对齐 Agent2API preset-providers）
  const presetCards = PRESET_PROVIDERS.filter((p) => !builtinUrls.has(normalizeUrl(p.base_url))).filter(
    (p) => hit(p.name, p.base_url, p.hint || ''),
  );
  // 已建 API 档：现有账号池（account 型渠道）。只列 API Key 型上游的池
  // （登录态家的池与 Agent 档同源，不在这里重复出现）；空池也列出（正要往里
  // 加第一个账号）。
  const poolCards: PoolCard[] = channels
    .map((ch) => {
      const rows = sessions.filter((s) => s.channel_id === String(ch.id));
      const provider = rows[0]?.provider || '';
      return { id: String(ch.id), name: ch.name, provider, count: rows.length };
    })
    .filter((c) => !c.provider || categoryOf(providers.find((p) => p.kind === c.provider)) === 'api')
    .filter((c) => hit(c.name, c.provider, providers.find((p) => p.kind === c.provider)?.label || ''));

  const pick = (kind: string, baseUrl: string, config?: ProviderAddConfig, preset?: PresetProviderItem): void => {
    setProvider(kind);
    setEdition(config?.editions?.[0]?.id || '');
    setBaseUrlHint(baseUrl);
    setBaseUrlPlaceholder(preset?.placeholder ?? '');
    setNoAuth(preset?.no_auth ?? false);
    setCustomNew(false);
    setDesktopPick(false);
    setProtocol('openai_compatible');
    setCustomName('');
    setPoolName('');
    setNeedsProviderPick(false);
    setChannelExplicit(false);
    setValues({});
    setRawHeaders('');
    setError('');
    // 预置卡随带的客户端头预填进「额外请求头」（如 Cline Pass 的客户端形态
    // 头是上游硬门槛，缺了一律 403——对齐 Agent2API quirks.headers）
    const headersText = preset?.headers
      ? Object.entries(preset.headers).map(([k, v]) => `${k}: ${v}`).join('\n')
      : '';
    setAdvanced((prev) => ({
      ...EMPTY_ADVANCED,
      channel_id: prev.channel_id,
      base_url: baseUrl,
      extra_headers: headersText,
    }));
    const methods = config?.methods?.length ? config.methods : (['manual'] as AddMethodId[]);
    setMethod(methods[0]);
    setStep('form');
  };

  /** 「新建自定义提供商」：原三张协议卡合成一个协议下拉（对齐上游 add-custom-provider
   *  的单 Select），Base URL 提示随所选协议变化 */
  const pickCustomNew = (): void => {
    setProvider('openai_compatible');
    setEdition('');
    setCustomNew(true);
    setDupAck(false);
    setProtocol('openai_compatible');
    setCustomName('');
    setPoolName('');
    setNeedsProviderPick(false);
    setChannelExplicit(false);
    setBaseUrlHint('');
    setBaseUrlPlaceholder(CUSTOM_BASE_PLACEHOLDER.openai_compatible);
    setNoAuth(false);
    setValues({});
    setRawHeaders('');
    setError('');
    setAdvanced((prev) => ({ ...EMPTY_ADVANCED, channel_id: prev.channel_id, base_url: '' }));
    setMethod('manual');
    setStep('form');
  };

  /** 协议切换：落库 kind 随协议走，Base URL 占位/提示回到该协议的默认
   *  （对齐上游：预置覆盖在用户改协议时失效） */
  const onProtocolChange = (next: string): void => {
    setProtocol(next as CustomProtocolKind);
    setProvider(next);
    setBaseUrlPlaceholder(CUSTOM_BASE_PLACEHOLDER[next] || '');
  };

  /** 「已建 API」档：往现有账号池里再加一个账号（渠道归属固定为该池） */
  const pickPool = (pool: PoolCard): void => {
    const rows = sessions.filter((s) => s.channel_id === pool.id);
    const provider = rows[0]?.provider || '';
    const rowBase = rows[0]?.base_url || '';
    setProvider(provider);
    setEdition('');
    setCustomNew(false);
    setDesktopPick(false);
    setProtocol('openai_compatible');
    setCustomName('');
    setPoolName(pool.name);
    setNeedsProviderPick(!provider);
    setChannelExplicit(true);
    setBaseUrlHint(rowBase);
    setBaseUrlPlaceholder('');
    setNoAuth(false);
    setValues({});
    setRawHeaders('');
    setError('');
    setAdvanced({ ...EMPTY_ADVANCED, channel_id: pool.id, base_url: rowBase });
    setMethod('manual');
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

  const stopPolling = (): void => {
    if (pollRef.current !== null) {
      window.clearTimeout(pollRef.current);
      pollRef.current = null;
    }
  };

  useEffect(() => stopPolling, []);

  const pollLogin = (taskState: string): void => {
    api.waitAccountLogin(taskState)
      .then((res) => {
        const data = res.data;
        if (!data) return;
        if (data.pending) {
          pollRef.current = window.setTimeout(() => pollLogin(taskState), 2000);
          return;
        }
        stopPolling();
        setLoginWaiting(false);
        if (data.error) {
          setLoginError(data.error);
          return;
        }
        addToast(t('账号已添加'));
        void onCreated();
        onClose();
      })
      .catch((err) => {
        stopPolling();
        setLoginWaiting(false);
        setLoginError(err instanceof Error ? err.message : String(err));
      });
  };

  const startLogin = async (): Promise<void> => {
    setLoginError('');
    setLoginWaiting(true);
    try {
      // channel_id 缺省时登录引擎按 provider 找同名账号池，找不到就建
      //（与 auto_channel 同语义，渠道对用户隐形）
      const res = await api.startAccountLogin({ provider: activeProvider, channel_id: advanced.channel_id });
      const data = res.data;
      if (!data?.state || !data.authUrl) throw new Error(t('登录发起失败'));
      setLoginState(data.state);
      window.open(data.authUrl, '_blank', 'noopener,noreferrer');
      pollLogin(data.state);
    } catch (err) {
      setLoginWaiting(false);
      setLoginError(err instanceof Error ? err.message : String(err));
    }
  };

  const cancelLogin = async (): Promise<void> => {
    stopPolling();
    setLoginWaiting(false);
    if (loginState) {
      try {
        await api.cancelAccountLogin(loginState);
      } catch {
        // 取消失败不影响本地等待退出
      }
    }
  };

  const submitCallback = async (): Promise<void> => {
    if (!loginState || !callbackUrl.trim()) return;
    setLoginError('');
    try {
      await api.submitAccountLoginCallback(loginState, callbackUrl.trim());
      setCallbackUrl('');
      setLoginWaiting(false);
      stopPolling();
      addToast(t('账号已添加'));
      await onCreated();
      onClose();
    } catch (err) {
      setLoginError(err instanceof Error ? err.message : String(err));
    }
  };

  const sendSmsCode = async (): Promise<void> => {
    setSmsHint('');
    setSmsBusy(true);
    try {
      const res = await api.sendAccountLoginSms({ provider: activeProvider, phone: smsPhone.trim() });
      const msgid = res.data?.msgid || '';
      if (!msgid) throw new Error(t('验证码发送失败'));
      setSmsMsgid(msgid);
      setSmsHint(t('验证码已发送，请填入收到的 6 位数字'));
    } catch (err) {
      setSmsHint(err instanceof Error ? err.message : String(err));
    } finally {
      setSmsBusy(false);
    }
  };

  const verifySmsCode = async (): Promise<void> => {
    setSmsHint('');
    setSmsBusy(true);
    try {
      await api.verifyAccountLoginSms({
        provider: activeProvider,
        phone: smsPhone.trim(),
        code: smsCode.trim(),
        msgid: smsMsgid,
        channel_id: advanced.channel_id,
      });
      addToast(t('账号已添加'));
      await onCreated();
      onClose();
    } catch (err) {
      setSmsHint(err instanceof Error ? err.message : String(err));
    } finally {
      setSmsBusy(false);
    }
  };

  /** Cookies SQLite 文件解析提交（#41）：chatglm/kuku 专用——body 带
   *  auth_file_b64、secret 留空（后端文件分支只看 auth_file_b64）。 */
  const submitCookieFile = async (): Promise<void> => {
    setPasteHint('');
    if (!provider) {
      setPasteHint(t('请选择供应商'));
      return;
    }
    if (!cookieFileB64) {
      setPasteHint(t('请先选择 Cookies SQLite 文件'));
      return;
    }
    setPasteBusy(true);
    try {
      await api.pasteAccountLogin({
        provider: activeProvider,
        channel_id: advanced.channel_id,
        secret: '',
        name: advanced.name.trim(),
        auth_file_b64: cookieFileB64,
      });
      addToast(t('账号已添加'));
      await onCreated();
      onClose();
    } catch (err) {
      setPasteHint(err instanceof Error ? err.message : String(err));
    } finally {
      setPasteBusy(false);
    }
  };

  const submitPaste = async (): Promise<void> => {
    setPasteHint('');
    if (!provider) {
      setPasteHint(t('请选择供应商'));
      return;
    }
    // 凭证字段：取 config 里 target=credential 的那一格
    const credField = addConfig?.fields.find((f) => f.target === 'credential');
    const secret = credField ? (values[credField.key] || '').trim() : '';
    if (!secret) {
      setPasteHint(t('请粘贴凭证'));
      return;
    }
    setPasteBusy(true);
    try {
      await api.pasteAccountLogin({
        provider: activeProvider,
        channel_id: advanced.channel_id,
        secret,
        name: (addConfig?.fields.find((f) => f.target === 'name')
          ? values[addConfig.fields.find((f) => f.target === 'name')!.key] || ''
          : advanced.name
        ).trim(),
      });
      addToast(t('账号已添加'));
      await onCreated();
      onClose();
    } catch (err) {
      setPasteHint(err instanceof Error ? err.message : String(err));
    } finally {
      setPasteBusy(false);
    }
  };

  const submit = async (): Promise<void> => {
    setError('');
    if (!provider) {
      setError(t('请选择供应商'));
      return;
    }
    if (addConfig && method === 'paste') {
      // 粘贴式登录有自己的提交入口（「校验并添加」按钮）
      setError(t('请点击「校验并添加」完成添加'));
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
    // 「新建自定义提供商」必须自填上游地址（anthropic / gemini 内置默认地址不适用）
    if (customNew && !(advanced.base_url || '').trim()) {
      setError(t('该供应商需要填写上游地址'));
      return;
    }
    // 重复提供商检测（#22）：同协议 + 同归一化地址的已有账号 → 首次提交只
    // 弹警告条二次确认（本次添加会自动并入同一账号池，不重复建池——提交链
    // 的 sibling 复用逻辑不变）；用户点「仍要新建」后放行
    if (customNew && !dupAck) {
      const dup = findDuplicate();
      if (dup) {
        setError(t('检测到同协议、同上游地址的已有账号（渠道「{{name}}」，{{n}} 个账号）——确认并入同一账号池请点上方「仍要新建」', { name: dup.channelName, n: dup.count }));
        return;
      }
    }
    if (needsProviderPick && !provider) {
      setError(t('请先选择供应商'));
      return;
    }
    setSaving(true);
    try {
      let credential = '';
      if (method === 'manual' || !addConfig) {
        credential = addConfig ? buildCredential(addConfig, values) : (values.credential || '').trim();
      }
      // 渠道并入账号池添加（A3 契约，后端 auto_channel 原子建号）：
      // 1) 「已建 API」档点卡进来：channel_id 就是所选账号池（后端校验归属）；
      // 2) 否则复用同供应商且同上游地址的现有账号池（幂等，避免「XX 账号池
      //    1/2/3」碎片；通用兼容 kind 同 kind 可能对应多家上游，再按地址区分）；
      // 3) 再按自动命名（「{label} 账号池」）兜底复用；
      // 4) 都没有才走 auto_channel 让服务端原子建渠道 + 建账号。
      const poolLabel = customNew ? (customName.trim() || label) : label;
      const poolName2 = `${poolLabel} ${t('账号池')}`.trim();
      let channelId = '';
      if (
        channelExplicit
        && advanced.channel_id
        && channels.some((ch) => String(ch.id) === advanced.channel_id)
      ) {
        channelId = advanced.channel_id;
      }
      if (!channelId) {
        const base = normalizeUrl((advanced.base_url || baseUrlHint).trim());
        const sibling = sessions.find(
          (s) => s.provider === activeProvider
            && !!s.channel_id
            && normalizeUrl(s.base_url || '') === base,
        );
        if (sibling?.channel_id) channelId = sibling.channel_id;
      }
      if (!channelId) {
        const named = channels.find((ch) => ch.name === poolName2);
        if (named) channelId = String(named.id);
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
        name: (nameField ? values[nameField.key] || '' : advanced.name).trim(),
        provider: activeProvider,
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
      if (channelId) {
        payload.channel_id = channelId;
      } else {
        // 没有可复用的账号池：auto_channel 让服务端原子建渠道 + 建账号
        // （后端幂等复用同名 account 型渠道；替代前端两段式，失败可整体回滚）
        payload.auto_channel = { name: poolName2 };
      }
      if (method === 'manual' || !addConfig) {
        payload.api_key = noAuth ? '' : credential;
      }
      // 免鉴权：凭证形态落 none（转发时一个鉴权头都不发——Ollama /
      // OpenCode Zen 免费档等；对齐 Agent2API account.noAuth）
      if (noAuth) {
        payload.credential_kind = 'none';
      }
      // 续期令牌（refreshToken / Coding Plan JWT 等）：表单里 key 为 refreshToken
      // 或 jwt 时落到 refresh_token（AIGX 暂不自动续期，先存下来供后续使用）
      const refreshKey = addConfig?.fields.find(
        (f) => f.key === 'refreshToken' || f.key === 'jwt',
      )?.key;
      if (refreshKey) {
        const refreshValue = (values[refreshKey] || '').trim();
        if (refreshValue) payload.refresh_token = refreshValue;
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

  /** 「桌面客户端登录态」入口：provider 无关（后端白名单分发 + auto_channel
   *  原子建号），sources 的 supported 由后端按平台/加密态给出 */
  const pickDesktop = (): void => {
    setDesktopPick(true);
    setCustomNew(false);
    setPoolName('');
    setDesktopSources(null);
    setDesktopName('');
    setDesktopBusyId('');
    setDesktopError('');
    setDupAck(false);
    setStep('form');
    void (async () => {
      try {
        const res = await api.getDesktopSources();
        setDesktopSources(res?.data ?? []);
      } catch (err) {
        setDesktopError(err instanceof Error ? err.message : String(err));
      }
    })();
  };

  /** 执行桌面导入：POST {source_id, name}（固定路径白名单在后端；前端无路径参数） */
  const runDesktopImport = async (sourceId: string): Promise<void> => {
    setDesktopBusyId(sourceId);
    setDesktopError('');
    try {
      await api.importDesktopAccount({
        source_id: sourceId,
        name: desktopName.trim() || undefined,
      });
      addToast(t('桌面登录态已导入，账号已创建'));
      await onCreated();
      onClose();
    } catch (err) {
      setDesktopError(err instanceof Error ? err.message : String(err));
    } finally {
      setDesktopBusyId('');
    }
  };

  /** 「新建自定义提供商」提交前的重复检测（#22）：同协议 + 同归一化地址的
   *  已有账号 → 警告条二次确认（本次添加会自动并入同一账号池，不重复建池） */
  const findDuplicate = (): { channelName: string; count: number } | null => {
    const base = normalizeUrl((advanced.base_url || '').trim());
    if (!base) return null;
    const rows = sessions.filter(
      (s) => s.provider === activeProvider && normalizeUrl(s.base_url || '') === base,
    );
    if (!rows.length) return null;
    const ch = channels.find((c) => String(c.id) === rows[0]?.channel_id);
    return { channelName: ch?.name || `#${String(rows[0]?.channel_id ?? '')}`, count: rows.length };
  };
  /** 当前是否命中重复（仅 customNew 形态检测；渲染警告条与提交拦截共用） */
  const dupInfo = customNew ? findDuplicate() : null;

  const cardIcon = (kind: string): string | null => providerIcon(kind);

  const methods: AddMethodId[] = addConfig?.methods?.length ? addConfig.methods : ['manual'];
  // file 段（#22）：有粘贴/手填能力的家统一追加「凭证文件」入口——文件内容
  // 读入粘贴域后走同一校验链（后端 #23：前端解析 + 既有创建端点，无独立端点）
  if (addConfig && (methods.includes('paste') || methods.includes('manual')) && !methods.includes('file')) {
    methods.push('file');
  }

  // ── 第 1 步：分档与分组渲染（对齐 Agent2API add-provider-pick）──

  const segLabel = (seg: GroupSegment): string =>
    seg === 'agent' ? 'Agent'
      : seg === 'web' ? 'Web'
        : seg === 'preset' ? t('API 模板')
          : t('已建 API');

  /** 每个分组的卡片节点（「全部」档的分组带与单档共用同一份渲染） */
  const groupCards = (seg: GroupSegment): JSX.Element[] => {
    if (seg === 'agent' || seg === 'web') {
      const list = seg === 'agent' ? agentCards : webCards;
      return list.map((p) => (
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
      ));
    }
    if (seg === 'preset') {
      return presetCards.map((p) => (
        <button
          type="button"
          key={p.key}
          className="add-card"
          onClick={() => pick(presetKind(p.protocol), p.base_url, undefined, p)}
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
            <span className="add-card-meta">{p.hint || p.base_url || t('地址自填')}</span>
          </span>
          <span className="add-card-go">›</span>
        </button>
      ));
    }
    return poolCards.map((c) => (
      <button type="button" key={c.id} className="add-card" onClick={() => pickPool(c)}>
        <span className="add-card-initial">
          {(providers.find((p) => p.kind === c.provider)?.label || c.name).slice(0, 1).toUpperCase()}
        </span>
        <span className="add-card-info">
          <span className="add-card-name">{c.name}</span>
          <span className="add-card-meta">
            {c.count ? t('{n} 个账号，点击添加', { n: c.count }) : t('空账号池，点击添加账号')}
          </span>
        </span>
        <span className="add-card-go">›</span>
      </button>
    ));
  };

  /** 「新建自定义提供商」卡：住在「已建 API」组第一张（搜索时收起——用户在找的是一家） */
  const newProviderCard = (
    <button type="button" key="__new__" className="add-card is-new" onClick={pickCustomNew}>
      <span className="add-card-initial is-new">＋</span>
      <span className="add-card-info">
        <span className="add-card-name">{t('新建自定义提供商')}</span>
        <span className="add-card-meta">{t('接入一个兼容上游')}</span>
      </span>
      <span className="add-card-go">›</span>
    </button>
  );

  // 「桌面客户端登录态」卡（#22 desktop 段）：与自定义卡同住「已建 API」组，
  // provider 无关（后端白名单 4 来源固定路径只读 + auto_channel 原子建号）
  const desktopCard = (
    <button type="button" key="__desktop__" className="add-card is-new" onClick={pickDesktop}>
      <span className="add-card-initial is-new">🖥</span>
      <span className="add-card-info">
        <span className="add-card-name">{t('桌面客户端登录态')}</span>
        <span className="add-card-meta">{t('读取本机客户端登录态文件（固定路径白名单，仅 Windows）')}</span>
      </span>
      <span className="add-card-go">›</span>
    </button>
  );

  const grouped = segment === 'all';
  const activeSegments: GroupSegment[] = grouped ? GROUPED_SEGMENTS : [segment as GroupSegment];
  // 「全部」档：空组整组不出（连分组带一起收）；唯一例外是「已建 API」——
  // 「新建自定义提供商」那张卡住在里面，没有搜索时永远可达
  const shownGroups = activeSegments
    .map((seg) => ({ seg, cards: groupCards(seg) }))
    .filter((g) => g.cards.length > 0 || (g.seg === 'existing' && !needle));
  const currentCount = shownGroups.reduce((sum, g) => sum + g.cards.length, 0);
  const allHitCount = GROUPED_SEGMENTS.reduce((sum, seg) => sum + groupCards(seg).length, 0);
  // 当前档搜不到而别档有时：给「看全部匹配」提示（对齐上游跨分类命中提示）
  const otherHitCount = needle && !grouped && currentCount === 0 ? allHitCount : 0;

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
                    { key: 'all', label: t('全部') },
                    { key: 'agent', label: 'Agent' },
                    { key: 'web', label: 'Web' },
                    { key: 'preset', label: t('API 模板') },
                    { key: 'existing', label: t('已建 API') },
                  ]}
                  active={segment}
                  onChange={setSegment}
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
              {/* 跨分类命中提示：拦在卡片网格上方，点一下直接切到「全部」 */}
              {otherHitCount > 0 ? (
                <div className="add-cross-hint">
                  <span>
                    {t('当前分类里没有「{keyword}」，其它分类有 {n} 个匹配', {
                      keyword: search.trim(),
                      n: otherHitCount,
                    })}
                  </span>
                  <Button type="button" variant="outline" size="sm" onClick={() => setSegment('all')}>
                    {t('看全部匹配')}
                  </Button>
                </div>
              ) : null}
              <div className="add-cards" role="listbox" aria-label={t('供应商列表')}>
                {shownGroups.map(({ seg, cards }) => (
                  <Fragment key={seg}>
                    {grouped ? (
                      <div className="add-card-band" role="group" aria-label={segLabel(seg)}>
                        <span className="add-card-band-name">{segLabel(seg)}</span>
                        {cards.length ? (
                          <span className="add-card-band-stat">{t('{count} 家', { count: cards.length })}</span>
                        ) : null}
                        <span className="add-card-band-fill" />
                        <button type="button" className="add-card-band-only" onClick={() => setSegment(seg)}>
                          {t('只看这一类')}
                        </button>
                      </div>
                    ) : null}
                    {seg === 'existing' && !needle ? [newProviderCard, desktopCard] : null}
                    {cards}
                  </Fragment>
                ))}
                {needle && currentCount === 0 ? (
                  <div className="add-cards-empty">
                    {t('没有匹配「{keyword}」的提供商', { keyword: search.trim() })}
                  </div>
                ) : null}
                {!needle && currentCount === 0 && (segment === 'agent' || segment === 'web' || segment === 'preset') ? (
                  <div className="add-cards-empty">
                    {segment === 'web'
                      ? t('内置暂未收录 Web 形态的供应商；可先用 Agent / API 模板添加。')
                      : t('暂无可用供应商，请刷新后重试')}
                  </div>
                ) : null}
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
                {poolName
                  ? t('添加账号到「{name}」', { name: poolName })
                  : customNew
                    ? t('新建自定义提供商')
                    : addConfig
                      ? t('登录 / 添加 {name} 账号', { name: label })
                      : t('添加 {name} 账号', { name: label })}
              </h3>
              <button type="button" className="modal-close" onClick={onClose}>
                &times;
              </button>
            </div>
            <div className="modal-body">
              {/* ── desktop 段（#22）：provider 无关的白名单导入，独立形态 ── */}
              {desktopPick ? (
                <div className="add-block">
                  <h4>{t('桌面客户端登录态导入')}</h4>
                  <p className="form-hint">
                    {t('从本机客户端的登录态文件读取令牌（固定路径白名单只读，请求不带路径；仅管理员，仅 Windows）。导入走与粘贴式完全相同的落库链。')}
                  </p>
                  <Input
                    label={t('备注名（可选，作用于本次导入的所有账号）')}
                    placeholder={t('可选，留空则用来源展示名')}
                    value={desktopName}
                    onChange={(e) => setDesktopName(e.target.value)}
                  />
                  {desktopError ? <div className="error-message">{desktopError}</div> : null}
                  {desktopSources === null ? (
                    <p className="form-hint">{t('加载来源清单…')}</p>
                  ) : desktopSources.length === 0 ? (
                    <p className="form-hint">{t('后端未配置任何桌面来源')}</p>
                  ) : (
                    <div className="add-desktop-list">
                      {desktopSources.map((s) => (
                        <div key={s.id} className={`add-desktop-row${s.supported ? '' : ' is-off'}`}>
                          <div className="add-desktop-main">
                            <b>{s.label}</b>
                            <code title={s.path}>{s.path}</code>
                            {!s.supported && s.unsupportedReason ? (
                              <span className="form-hint">{s.unsupportedReason}</span>
                            ) : null}
                          </div>
                          <Button
                            type="button"
                            variant={s.supported ? 'primary' : 'outline'}
                            size="sm"
                            disabled={!s.supported || desktopBusyId !== ''}
                            onClick={() => void runDesktopImport(s.id)}
                          >
                            {desktopBusyId === s.id ? t('导入中…') : s.supported ? t('导入') : t('不支持')}
                          </Button>
                        </div>
                      ))}
                    </div>
                  )}
                </div>
              ) : addConfig ? (
                <>
                  {addConfig.editions?.length ? (
                    <div className="add-method-row">
                      <span className="add-method-label">{t('账号版本')}</span>
                      <Tabs<string>
                        items={addConfig.editions.map((item) => ({ key: item.id, label: item.label }))}
                        active={activeProvider}
                        onChange={setEdition}
                        ariaLabel={t('账号版本')}
                      />
                    </div>
                  ) : null}

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
                          onClick={() => void startLogin()}
                          disabled={loginWaiting}
                        >
                          {loginWaiting ? t('等待登录完成…') : webConfig.button}
                        </Button>
                        {loginWaiting ? (
                          <Button type="button" variant="outline" onClick={() => void cancelLogin()}>
                            {t('取消等待')}
                          </Button>
                        ) : null}
                        {webConfig.hint ? <span className="form-hint">{webConfig.hint}</span> : null}
                        <Button type="button" variant="outline" onClick={() => setMethod('manual')}>
                          {t('已登录，去填写凭证')}
                        </Button>
                      </div>
                      {loginWaiting && webConfig.callback ? (
                        <div className="add-block" style={{ marginTop: 10 }}>
                          <Input
                            label={t('远程部署：登录完成后把地址栏回调地址粘到这里')}
                            placeholder="http://127.0.0.1:…/oauth/callback?code=…&state=…"
                            value={callbackUrl}
                            onChange={(e) => setCallbackUrl(e.target.value)}
                          />
                          <div className="add-btn-row">
                            <Button
                              type="button"
                              variant="outline"
                              onClick={() => void submitCallback()}
                              disabled={!callbackUrl.trim()}
                            >
                              {t('提交回调地址')}
                            </Button>
                          </div>
                        </div>
                      ) : null}
                      {loginError ? (
                        <div className="error-message">
                          {loginError}
                          {loginError.includes('P-256') ? (
                            <Button
                              type="button"
                              variant="outline"
                              size="sm"
                              style={{ marginLeft: 8 }}
                              onClick={() => void startLogin()}
                            >
                              {t('重试')}
                            </Button>
                          ) : null}
                        </div>
                      ) : null}
                    </div>
                  ) : null}

                  {method === 'sms' && addConfig.sms ? (
                    <div className="add-block">
                      <h4>{t('手机验证码登录')}</h4>
                      <p className="form-hint">{addConfig.sms.note}</p>
                      <Input
                        label={t('手机号')}
                        placeholder={t('11 位大陆手机号')}
                        value={smsPhone}
                        onChange={(e) => setSmsPhone(e.target.value)}
                      />
                      <div className="add-btn-row">
                        <Button
                          type="button"
                          variant="outline"
                          onClick={() => void sendSmsCode()}
                          disabled={smsBusy || smsPhone.trim().length < 11}
                        >
                          {t('获取验证码')}
                        </Button>
                      </div>
                      <Input
                        label={t('验证码')}
                        placeholder={t('6 位数字')}
                        value={smsCode}
                        onChange={(e) => setSmsCode(e.target.value)}
                      />
                      <div className="add-btn-row">
                        <Button
                          type="button"
                          onClick={() => void verifySmsCode()}
                          disabled={smsBusy || !smsMsgid || smsCode.trim().length !== 6}
                        >
                          {t('登录并添加')}
                        </Button>
                        {smsHint ? <span className="form-hint">{smsHint}</span> : null}
                      </div>
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

                  {method === 'paste' ? (
                    <div className="add-block">
                      <h4>{addConfig.manualTitle}</h4>
                      <p className="form-hint">{addConfig.manualNote}</p>
                      {addConfig.web ? (
                        <div className="add-btn-row" style={{ marginBottom: 10 }}>
                          <Button
                            type="button"
                            variant="outline"
                            onClick={() => window.open(addConfig.web!.url, '_blank', 'noopener,noreferrer')}
                          >
                            {addConfig.web.button}
                          </Button>
                          <span className="form-hint">{addConfig.web.hint}</span>
                        </div>
                      ) : null}
                      {addConfig.fields.map((field) => (
                        <FieldInput
                          key={field.key}
                          field={field}
                          value={values[field.key] || ''}
                          onChange={(v) => setValue(field.key, v)}
                        />
                      ))}
                      <div className="add-btn-row" style={{ marginTop: 10 }}>
                        <Button type="button" onClick={() => void submitPaste()} disabled={pasteBusy}>
                          {pasteBusy ? t('校验并添加中…') : t('校验并添加')}
                        </Button>
                        {pasteHint ? <span className="form-hint">{pasteHint}</span> : null}
                      </div>
                    </div>
                  ) : null}

                  {/* ── file 段（#22）：选择凭据文件 → 读入粘贴域 → 走既有校验链。
                      后端 #23 结论：前端解析 + 既有创建端点，无独立端点。JSON
                      预检失败不拦（按纯令牌处理），由粘贴校验给最终结论。── */}
                  {method === 'file' && addConfig ? (
                    <div className="add-block">
                      <h4>{t('导入凭证文件')}</h4>
                      <p className="form-hint">{addConfig.fileImport?.note || t('选择保存下来的凭据 JSON / 文本文件，内容读入后走粘贴校验链。')}</p>
                      <div className="add-btn-row">
                        <label className="btn btn-primary" style={{ cursor: 'pointer' }}>
                          {t('选择文件')}
                          <input
                            type="file"
                            data-testid="file-input-json"
                            accept=".json,.txt,application/json,text/plain"
                            style={{ display: 'none' }}
                            onChange={(e) => {
                              const file = e.target.files?.[0];
                              e.target.value = '';
                              if (!file) return;
                              void file.text().then((content) => {
                                const credField = addConfig.fields.find((f) => f.target === 'credential');
                                if (!credField) return;
                                const trimmed = content.trim();
                                try {
                                  JSON.parse(trimmed);
                                } catch {
                                  setPasteHint(t('文件不是合法 JSON，按纯令牌处理（最终以校验结果为准）'));
                                }
                                setValue(credField.key, trimmed);
                                setMethod('paste');
                                setPasteHint(t('已读入 {{name}}（{{n}} 字符），确认后点「校验并添加」', { name: file.name, n: trimmed.length }));
                              });
                            }}
                          />
                        </label>
                        {/* ── Cookies SQLite 文件（#41）：chatglm/kuku 桌面客户端
                            的 Cookies 库走后端 auth_file_b64 专用分支（解析出
                            令牌 + 续期令牌一并落库），与 JSON 凭据文件是两条路。── */}
                        {(activeProvider === 'chatglm' || activeProvider === 'kuku') && (
                          <>
                            <label className="btn btn-outline" style={{ cursor: 'pointer' }}>
                              {t('选择 Cookies SQLite 文件')}
                              <input
                                type="file"
                                data-testid="file-input-cookies"
                                accept=".sqlite,.db,.sqlite3,application/octet-stream"
                                style={{ display: 'none' }}
                                onChange={(e) => {
                                  const file = e.target.files?.[0];
                                  e.target.value = '';
                                  if (!file) return;
                                  setCookieFileName(file.name);
                                  const reader = new FileReader();
                                  reader.onload = () => {
                                    const result = String(reader.result || '');
                                    // dataURL 前缀（data:...;base64,）剥掉只留 base64
                                    const b64 = result.includes(',') ? result.slice(result.indexOf(',') + 1) : result;
                                    setCookieFileB64(b64);
                                    setPasteHint(t('已读入 {{name}}（{{n}} 字节），点「解析 Cookies 库并添加」', { name: file.name, n: file.size }));
                                  };
                                  reader.readAsDataURL(file);
                                }}
                              />
                            </label>
                            {cookieFileB64 ? (
                              <Button
                                type="button"
                                onClick={() => void submitCookieFile()}
                                disabled={pasteBusy}
                              >
                                {pasteBusy ? t('解析中…') : t('解析 Cookies 库并添加')}
                              </Button>
                            ) : null}
                          </>
                        )}
                      </div>
                      {cookieFileName ? (
                        <div className="form-hint">{t('Cookies 库：{{name}}（base64 就绪）', { name: cookieFileName })}</div>
                      ) : null}
                    </div>
                  ) : null}

                  {/* ── guest 段（#22）：后端无 guest/access 端点（上游闭源插件），
                      形态 = 访客令牌指引 + 粘贴走既有校验链 ── */}
                  {method === 'guest' && addConfig?.guest ? (
                    <div className="add-block">
                      <h4>{t('访客模式')}</h4>
                      <p className="form-hint">{addConfig.guest.note}</p>
                      {addConfig.fields.map((field) => (
                        <FieldInput
                          key={field.key}
                          field={field}
                          value={values[field.key] || ''}
                          onChange={(v) => setValue(field.key, v)}
                        />
                      ))}
                      <div className="add-btn-row" style={{ marginTop: 10 }}>
                        <Button type="button" onClick={() => void submitPaste()} disabled={pasteBusy}>
                          {pasteBusy ? t('校验并添加中…') : t('校验并添加')}
                        </Button>
                        {pasteHint ? <span className="form-hint">{pasteHint}</span> : null}
                      </div>
                    </div>
                  ) : null}

                  {/* ── oauth 段（#22）：指引打开官方授权页 + 粘贴 accessToken
                      （上游 OauthLoginConfig 的 modes 即粘贴字段；网页语境无
                      滑块桥，指引如实写明）── */}
                  {method === 'oauth' && addConfig?.oauth ? (
                    <div className="add-block">
                      <h4>{addConfig.oauth.title || t('网页授权')}</h4>
                      <p className="form-hint">{addConfig.oauth.note}</p>
                      {addConfig.oauth.url ? (
                        <div className="add-btn-row" style={{ marginBottom: 10 }}>
                          <Button
                            type="button"
                            variant="outline"
                            onClick={() => window.open(addConfig.oauth!.url, '_blank', 'noopener,noreferrer')}
                          >
                            {addConfig.oauth.button || t('打开官方授权页')}
                          </Button>
                        </div>
                      ) : null}
                      {addConfig.fields.map((field) => (
                        <FieldInput
                          key={field.key}
                          field={field}
                          value={values[field.key] || ''}
                          onChange={(v) => setValue(field.key, v)}
                        />
                      ))}
                      <div className="add-btn-row" style={{ marginTop: 10 }}>
                        <Button type="button" onClick={() => void submitPaste()} disabled={pasteBusy}>
                          {pasteBusy ? t('校验并添加中…') : t('校验并添加')}
                        </Button>
                        {pasteHint ? <span className="form-hint">{pasteHint}</span> : null}
                      </div>
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
                  {/* 「新建自定义提供商」：原三张协议卡合成一个协议下拉（对齐上游
                      add-custom-provider 的单 Select），落库 kind 随协议走 */}
                  {customNew ? (
                    <>
                      <Select
                        label={t('协议')}
                        value={protocol}
                        onChange={(e) => onProtocolChange(e.target.value)}
                        aria-label={t('协议')}
                      >
                        {CUSTOM_PROTOCOLS.map((c) => (
                          <option key={c.kind} value={c.kind}>
                            {t(CUSTOM_PROTOCOL_LABELS[c.kind] || c.kind)}
                          </option>
                        ))}
                      </Select>
                      <Input
                        label={t('渠道名称（可选）')}
                        placeholder={t('例如：我的中转')}
                        value={customName}
                        onChange={(e) => setCustomName(e.target.value)}
                      />
                    </>
                  ) : null}
                  {/* 「已建 API」档：池内已有账号 → 供应商随池固定；空池 → 先选供应商 */}
                  {poolName && !needsProviderPick ? (
                    <p className="form-hint">{t('供应商')}：{label}</p>
                  ) : null}
                  {needsProviderPick ? (
                    <Select
                      label={t('供应商')}
                      value={provider}
                      aria-label={t('供应商')}
                      onChange={(e) => {
                        const kind = e.target.value;
                        setProvider(kind);
                        setBaseUrlHint(providers.find((p) => p.kind === kind)?.default_base_url || '');
                      }}
                    >
                      <option value="">{t('请选择供应商')}</option>
                      {providers
                        .filter((p) => categoryOf(p) === 'api')
                        .map((p) => (
                          <option key={p.kind} value={p.kind}>{providerLabel(p)}</option>
                        ))}
                    </Select>
                  ) : null}
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
                      placeholder: noAuth ? t('无需填写（该上游免鉴权）') : 'sk-...',
                      target: 'credential',
                    }}
                    value={noAuth ? '' : (values.credential || '')}
                    onChange={(v) => setValue('credential', v)}
                  />
                  {/* 免鉴权（对齐 Agent2API account.noAuth）：Ollama / OpenCode Zen
                      免费档等上游一个鉴权头都不发 */}
                  <label className="form-hint" style={{ display: 'inline-flex', gap: 6, cursor: 'pointer' }}>
                    <input
                      type="checkbox"
                      checked={noAuth}
                      onChange={(e) => setNoAuth(e.target.checked)}
                    />
                    {t('该上游无需鉴权（本地/免费端点）')}
                  </label>
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
                <summary>{t('账号设置（优先级 / 并发等）')}</summary>
                <div className="add-advanced-body">
                  {/* 渠道是内部容器（账号型渠道只用来给同一供应商的账号分组），
                      对齐 Agent2API：用户心智里只有「供应商」，不提供渠道选择。
                      保存时若没有可用渠道，后端自动按所选供应商创建。 */}
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
                    placeholder={baseUrlPlaceholder || selectedProvider?.default_base_url || baseUrlHint || 'https://api.deepseek.com'}
                    hint={customNew ? t(CUSTOM_BASE_HINT[protocol] || '') : undefined}
                    value={advanced.base_url}
                    onChange={(e) => { setAdvanced({ ...advanced, base_url: e.target.value }); setDupAck(false); }}
                  />
                  {/* 重复提供商检测警告条（#22）：customNew + 同协议同地址已有
                      账号时出现；「仍要新建」置 dupAck 放行提交。命中时 base_url
                      变更会因 dupAck 已置位跳过再检——重置由输入变化触发 */}
                  {customNew && dupInfo ? (
                    <div className="add-dup-warn" role="alert">
                      <span>
                        {t('已存在同协议、同上游地址的账号：渠道「{{name}}」（{{n}} 个账号）。本次添加会自动并入该账号池，不会重复建池。', { name: dupInfo.channelName, n: dupInfo.count })}
                      </span>
                      {dupAck ? (
                        <span className="form-hint">{t('已确认并入，可继续提交')}</span>
                      ) : (
                        <Button type="button" variant="outline" size="sm" onClick={() => setDupAck(true)}>
                          {t('仍要新建')}
                        </Button>
                      )}
                    </div>
                  ) : null}
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
                  <ProxySelector
                    value={advanced.proxy}
                    onChange={(next) => setAdvanced({ ...advanced, proxy: next })}
                    placeholder="socks5://127.0.0.1:1080"
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
