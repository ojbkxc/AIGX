import { useEffect, useState, type FormEvent } from 'react';
import { useTranslation } from 'react-i18next';
import { FlaskConical, Pencil, Plus, Power, RotateCcw, SlidersHorizontal, Trash2, Upload } from 'lucide-react';
import { api } from '../api';
import { useToast } from '../components/Toast';
import ConfirmDialog, { type ConfirmState } from '../components/ConfirmDialog';
import { Button, Card, EmptyState, Input, Select, SkeletonTable, Tabs, Textarea } from '../components/ui';
import type {
  AccountProviderItem,
  AccountSessionItem,
  AccountSessionStatsItem,
  ChannelItem,
} from '../types';
import './AccountSessions.css';

/**
 * 供应商选择分段 —— 对齐 Agent2API 的「Agent（内置）/ 预置 API / 自定义」：
 * - Agent：网关内置 provider 目录（默认上游地址开箱可用）；
 * - 预置 API：官方/托管端点模板，点卡片把协议与地址预填进表单；
 * - 自定义：自己填上游地址（OpenAI / Anthropic / Gemini 三种协议）。
 */
type ProviderSegment = 'agent' | 'preset' | 'custom';

type ProviderProtocol = 'openai' | 'anthropic' | 'gemini';

interface PresetProviderItem {
  key: string;
  name: string;
  protocol: ProviderProtocol;
  base_url: string;
  hint?: string;
}

/** 预置端点模板（取自 Agent2API preset 目录；与内置目录同地址的会在渲染时去重） */
const PRESET_PROVIDERS: PresetProviderItem[] = [
  {
    key: 'openai',
    name: 'OpenAI',
    protocol: 'openai',
    base_url: 'https://api.openai.com/v1',
  },
  {
    key: 'anthropic',
    name: 'Anthropic',
    protocol: 'anthropic',
    base_url: 'https://api.anthropic.com',
  },
  {
    key: 'openrouter',
    name: 'OpenRouter',
    protocol: 'openai',
    base_url: 'https://openrouter.ai/api/v1',
  },
  {
    key: 'huggingface',
    name: 'HuggingFace',
    protocol: 'openai',
    base_url: 'https://router.huggingface.co/v1',
  },
  {
    key: 'nvidia',
    name: 'NVIDIA',
    protocol: 'openai',
    base_url: 'https://integrate.api.nvidia.com/v1',
  },
  {
    key: 'deepseek-anthropic',
    name: 'DeepSeek（Anthropic 端点）',
    protocol: 'anthropic',
    base_url: 'https://api.deepseek.com/anthropic',
  },
  {
    key: 'glm-cn',
    name: 'GLM 中国',
    protocol: 'openai',
    base_url: 'https://open.bigmodel.cn/api/coding/paas/v4',
  },
  {
    key: 'zai-anthropic',
    name: 'Z.ai（Anthropic 端点）',
    protocol: 'anthropic',
    base_url: 'https://api.z.ai/api/anthropic',
  },
  {
    key: 'siliconflow-intl',
    name: 'SiliconFlow 国际',
    protocol: 'openai',
    base_url: 'https://api.siliconflow.com/v1',
  },
  {
    key: 'volcengine-ark',
    name: '火山方舟 Coding',
    protocol: 'openai',
    base_url: 'https://ark.cn-beijing.volces.com/api/coding/v3',
  },
  {
    key: 'alicode-cn',
    name: '阿里云百炼 Coding（国内）',
    protocol: 'openai',
    base_url: 'https://coding.dashscope.aliyuncs.com/v1',
  },
  {
    key: 'alicode-intl',
    name: '阿里云百炼 Coding（国际）',
    protocol: 'openai',
    base_url: 'https://coding-intl.dashscope.aliyuncs.com/v1',
  },
  {
    key: 'alims-intl',
    name: '阿里云百炼模型服务（国际）',
    protocol: 'openai',
    base_url: 'https://dashscope-intl.aliyuncs.com/compatible-mode/v1',
  },
  {
    key: 'alitp-intl',
    name: '阿里云 Token Plan（国际）',
    protocol: 'openai',
    base_url: 'https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1',
  },
  {
    key: 'xiaomi-mimo',
    name: '小米 MiMo Token Plan',
    protocol: 'openai',
    base_url: 'https://token-plan-sgp.xiaomimimo.com/v1',
  },
  {
    key: 'minimax-cn',
    name: 'MiniMax 中国',
    protocol: 'openai',
    base_url: 'https://api.minimaxi.com/v1',
  },
  {
    key: 'sensenova',
    name: '商汤日日新',
    protocol: 'openai',
    base_url: 'https://api.sensenova.cn/compatible-mode/v1',
  },
];

/** 自定义分段：三种协议，地址由用户自己填。 */
const CUSTOM_PROTOCOLS: Array<{ kind: string; labelKey: string; hintKey: string }> = [
  { kind: 'openai_compatible', labelKey: 'OpenAI 兼容（自定义地址）', hintKey: '按 {base}/chat/completions 转发' },
  { kind: 'anthropic', labelKey: 'Anthropic 兼容（自定义地址）', hintKey: '按 {base}/v1/messages 转发' },
  { kind: 'gemini', labelKey: 'Gemini 兼容（自定义地址）', hintKey: '按 {base}/models 转发' },
];

/** 协议 → 落库 provider kind（预置模板按协议落到对应兼容 kind）。 */
function presetKind(protocol: ProviderProtocol): string {
  if (protocol === 'anthropic') return 'anthropic';
  if (protocol === 'gemini') return 'gemini';
  return 'openai_compatible';
}

/** URL 归一（去尾斜杠 / 小写），用于内置与预置同地址去重。 */
function normalizeUrl(url: string): string {
  return url.trim().replace(/\/+$/, '').toLowerCase();
}

interface SessionFormState {
  channel_id: string;
  name: string;
  provider: string;
  base_url: string;
  api_key: string;
  proxy: string;
  status: string;
  priority: string;
  weight: string;
  max_concurrency: string;
  models: string;
  expires_at: string;
  clear_key: boolean;
  clear_proxy: boolean;
}

const EMPTY_FORM: SessionFormState = {
  channel_id: '',
  name: '',
  provider: 'openai_compatible',
  base_url: '',
  api_key: '',
  proxy: '',
  status: 'enabled',
  priority: '0',
  weight: '1',
  max_concurrency: '3',
  models: '',
  expires_at: '',
  clear_key: false,
  clear_proxy: false,
};

interface BulkFormState {
  channel_id: string;
  provider: string;
  base_url: string;
  proxy: string;
  name_prefix: string;
  priority: string;
  weight: string;
  max_concurrency: string;
  models: string;
  credentials: string;
}

const EMPTY_BULK: BulkFormState = {
  channel_id: '',
  provider: 'openai_compatible',
  base_url: '',
  proxy: '',
  name_prefix: 'account',
  priority: '0',
  weight: '1',
  max_concurrency: '3',
  models: '',
  credentials: '',
};

/** 运行态 → badge 样式 */
function statusClass(status?: string): string {
  if (status === 'enabled') return 'badge-success';
  if (status === 'expired') return 'badge-warning';
  return 'badge-neutral';
}

export default function AccountSessions(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();

  const [sessions, setSessions] = useState<AccountSessionItem[]>([]);
  const [accountChannels, setAccountChannels] = useState<ChannelItem[]>([]);
  const [providers, setProviders] = useState<AccountProviderItem[]>([]);
  const [stats, setStats] = useState<AccountSessionStatsItem[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');

  const [channelFilter, setChannelFilter] = useState('');
  const [search, setSearch] = useState('');

  const [showModal, setShowModal] = useState(false);
  const [editing, setEditing] = useState<AccountSessionItem | null>(null);
  const [form, setForm] = useState<SessionFormState>(EMPTY_FORM);
  const [saving, setSaving] = useState(false);

  // 供应商选择：Agent（内置）/ 预置 API / 自定义（仅新增时展示，编辑保留原下拉）
  const [pickSegment, setPickSegment] = useState<ProviderSegment>('agent');
  const [providerSearch, setProviderSearch] = useState('');

  // 账号路由（轮换策略）：冷却窗口与单请求最大换号次数
  const [showRotation, setShowRotation] = useState(false);
  const [rotation, setRotation] = useState({ base: '60', max: '1800', auth: '900', attempts: '5' });
  const [rotationSaving, setRotationSaving] = useState(false);
  const [rotationError, setRotationError] = useState('');

  const [showBulk, setShowBulk] = useState(false);
  const [bulk, setBulk] = useState<BulkFormState>(EMPTY_BULK);

  const [testingId, setTestingId] = useState<string | null>(null);
  const [confirmState, setConfirmState] = useState<ConfirmState | null>(null);

  const load = async (): Promise<void> => {
    setLoading(true);
    setError('');
    try {
      const [sessionsRes, channelsRes, providersRes, statsRes] = await Promise.all([
        api.listAccountSessions(),
        api.listChannels(),
        api.accountProviders(),
        api.accountSessionStats(),
      ]);
      setSessions(sessionsRes.data ?? []);
      const channels = channelsRes.data ?? [];
      setAccountChannels(channels.filter((c) => c.channel_type === 'account'));
      setProviders(providersRes.data ?? []);
      setStats(statsRes.data?.channels ?? []);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    void load();
  }, []);

  const openCreate = (): void => {
    setEditing(null);
    setForm({
      ...EMPTY_FORM,
      channel_id: channelFilter || accountChannels[0]?.id || '',
    });
    setPickSegment('agent');
    setProviderSearch('');
    setShowModal(true);
  };

  const openEdit = (session: AccountSessionItem): void => {
    setEditing(session);
    setForm({
      channel_id: session.channel_id,
      name: session.name,
      provider: session.provider || 'openai_compatible',
      base_url: session.base_url || '',
      api_key: '',
      // 接口返回的代理已脱敏（userinfo 打码），回填会被误存；留空 + 占位提示
      proxy: '',
      // 表单只提供启用/停用两态；已过期账号按停用处理（保存即显式置为停用）
      status: (session.configured_status || session.status) === 'enabled' ? 'enabled' : 'disabled',
      priority: String(session.priority ?? 0),
      weight: String(session.weight ?? 1),
      max_concurrency: String(session.max_concurrency ?? 0),
      models: (session.models || []).join(', '),
      expires_at: session.expires_at ? String(session.expires_at) : '',
      clear_key: false,
      clear_proxy: false,
    });
    setShowModal(true);
  };

  const closeModal = (): void => {
    setShowModal(false);
    setEditing(null);
    setSaving(false);
  };

  const openBulk = (): void => {
    setBulk({
      ...EMPTY_BULK,
      channel_id: channelFilter || accountChannels[0]?.id || '',
    });
    setShowBulk(true);
  };

  // ── 账号路由（轮换策略）──
  const openRotation = async (): Promise<void> => {
    setRotationError('');
    try {
      const res = await api.accountRotationPolicy();
      const d = res?.data;
      if (d) {
        setRotation({
          base: String(d.base_cooldown_secs),
          max: String(d.max_cooldown_secs),
          auth: String(d.auth_cooldown_secs),
          attempts: String(d.max_attempts),
        });
      }
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    }
    setShowRotation(true);
  };

  const saveRotation = async (e: FormEvent): Promise<void> => {
    e.preventDefault();
    setRotationError('');
    setRotationSaving(true);
    try {
      const res = await api.saveAccountRotationPolicy({
        base_cooldown_secs: Math.max(5, Number(rotation.base) || 60),
        max_cooldown_secs: Math.max(5, Number(rotation.max) || 1800),
        auth_cooldown_secs: Math.max(60, Number(rotation.auth) || 900),
        max_attempts: Math.max(1, Number(rotation.attempts) || 5),
      });
      const d = res?.data;
      if (d) {
        setRotation({
          base: String(d.base_cooldown_secs),
          max: String(d.max_cooldown_secs),
          auth: String(d.auth_cooldown_secs),
          attempts: String(d.max_attempts),
        });
      }
      addToast(t('账号路由设置已保存'));
      setShowRotation(false);
    } catch (err) {
      setRotationError(err instanceof Error ? err.message : String(err));
    } finally {
      setRotationSaving(false);
    }
  };

  const splitModels = (raw: string): string[] =>
    raw
      .split(/[,\s]+/)
      .map((m) => m.trim())
      .filter((m) => m.length > 0);

  const handleSave = async (e: FormEvent): Promise<void> => {
    e.preventDefault();
    setError('');
    setSaving(true);
    try {
      let channelId = form.channel_id;
      // 没有账号型渠道时：按所选供应商自动创建渠道，避免「添加账号」点不动
      if (!editing && !channelId) {
        const name = `${selectedProviderLabel} ${t('账号池')}`.trim();
        const created = await api.addChannel({
          name,
          channel_type: 'account',
          base_url: '',
          models: [],
        });
        channelId = created?.data?.id != null ? String(created.data.id) : '';
        if (!channelId) {
          throw new Error(t('创建账号型渠道失败'));
        }
      }
      const payload: Record<string, unknown> = {
        channel_id: channelId,
        name: form.name.trim(),
        provider: form.provider,
        base_url: form.base_url.trim(),
        status: form.status,
        priority: Number(form.priority || 0),
        // u32 字段：前端先夹紧，避免负数触发后端 422
        weight: Math.max(1, Number(form.weight || 1)),
        max_concurrency: Math.max(0, Number(form.max_concurrency || 0)),
        models: splitModels(form.models),
        expires_at: form.expires_at.trim() ? Number(form.expires_at) : null,
      };
      // 代理三态：新增直传；更新时缺省保持原值、"" 清除、非空覆盖
      if (!editing) {
        payload.proxy = form.proxy.trim();
      } else if (form.clear_proxy) {
        payload.proxy = '';
      } else if (form.proxy.trim()) {
        payload.proxy = form.proxy.trim();
      }
      if (editing) {
        if (form.clear_key) {
          payload.api_key = '';
        } else if (form.api_key.trim()) {
          payload.api_key = form.api_key.trim();
        }
        await api.updateAccountSession(editing.id, payload);
        addToast(t('账号已保存'));
      } else {
        payload.api_key = form.api_key.trim();
        await api.createAccountSession(payload);
        addToast(t('账号已创建'));
      }
      closeModal();
      await load();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  };

  const handleBulk = async (e: FormEvent): Promise<void> => {
    e.preventDefault();
    setError('');
    if (!bulk.channel_id) {
      setError(t('请选择渠道'));
      return;
    }
    if (!bulk.credentials.trim()) {
      setError(t('请粘贴至少一条凭证'));
      return;
    }
    setSaving(true);
    try {
      const res = await api.bulkImportAccountSessions({
        channel_id: bulk.channel_id,
        provider: bulk.provider,
        base_url: bulk.base_url.trim(),
        proxy: bulk.proxy.trim(),
        name_prefix: bulk.name_prefix.trim(),
        priority: Number(bulk.priority || 0),
        weight: Math.max(1, Number(bulk.weight || 1)),
        max_concurrency: Math.max(0, Number(bulk.max_concurrency || 0)),
        models: splitModels(bulk.models),
        credentials: bulk.credentials,
      });
      addToast(
        t('导入完成：成功 {{created}} 个，跳过 {{skipped}} 个', {
          created: res.data?.created ?? 0,
          skipped: res.data?.skipped ?? 0,
        })
      );
      setShowBulk(false);
      await load();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  };

  const handleTest = async (session: AccountSessionItem): Promise<void> => {
    setTestingId(session.id);
    setError('');
    try {
      const res = await api.testAccountSession(session.id);
      const data = res.data;
      if (data?.success) {
        addToast(`${t('测试成功')} · ${data.model || ''} · ${data.latency_ms ?? 0}ms`);
      } else {
        addToast(`${t('测试失败')}: ${data?.message || ''}`, 'error');
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setTestingId(null);
    }
  };

  const handleResetCooldowns = async (session: AccountSessionItem): Promise<void> => {
    setError('');
    try {
      await api.resetAccountSessionCooldowns(session.id);
      addToast(t('冷却已清除'));
      await load();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  };

  // 列表内快捷启停（PATCH 只改状态，不触碰地址/模型/代理/凭证）
  const handleToggleStatus = async (session: AccountSessionItem): Promise<void> => {
    const next = session.status === 'enabled' ? 'disabled' : 'enabled';
    setError('');
    try {
      await api.patchAccountSession(session.id, { status: next });
      addToast(next === 'enabled' ? t('已启用') : t('已停用'));
      await load();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  };

  const handleDelete = (session: AccountSessionItem): void => {
    setConfirmState({
      title: t('删除账号'),
      message: t('确定删除账号「{{name}}」？该账号的凭证与冷却记录会一并删除。', {
        name: session.name,
      }),
      confirmText: t('删除'),
      danger: true,
      onConfirm: async () => {
        setError('');
        try {
          await api.deleteAccountSession(session.id);
          addToast(t('账号已删除'));
          await load();
        } catch (err) {
          setError(err instanceof Error ? err.message : String(err));
        }
      },
    });
  };

  const cooldownSummary = (session: AccountSessionItem): string => {
    const entries = Object.entries(session.cooldowns || {});
    if (entries.length === 0) return '—';
    return entries
      .slice(0, 3)
      .map(([model, secs]) => `${model} ${secs}s`)
      .join(' · ');
  };

  // 运行态 → 本地化文案（与渠道管理的启用/禁用口径一致）
  const statusLabel = (status?: string): string => {
    if (status === 'enabled') return t('启用');
    if (status === 'expired') return t('凭证已过期');
    return t('禁用');
  };

  const successRate = (session: AccountSessionItem): string => {
    const ok = Number(session.success_count || 0);
    const failed = Number(session.error_count || 0);
    const total = ok + failed;
    if (total === 0) return '—';
    return `${((ok / total) * 100).toFixed(0)}%`;
  };

  // 最近错误过长会撑破单元格：截断展示，完整内容放 title
  const shortError = (raw?: string | null): string => {
    if (!raw) return '';
    return raw.length > 48 ? `${raw.slice(0, 48)}…` : raw;
  };

  const needle = search.trim().toLowerCase();
  const visible = sessions.filter((s) => {
    if (channelFilter && s.channel_id !== channelFilter) return false;
    if (!needle) return true;
    return (
      s.name.toLowerCase().includes(needle) ||
      (s.channel_name || '').toLowerCase().includes(needle) ||
      (s.provider || '').toLowerCase().includes(needle) ||
      (s.base_url || '').toLowerCase().includes(needle)
    );
  });

  // ── 供应商选择（仅新增账号时使用）──
  const providerLabel = (kind: string): string =>
    providers.find((p) => p.kind === kind)?.label ?? kind;

  const providerSearchNeedle = providerSearch.trim().toLowerCase();

  // 内置目录里已有同地址的，预置段不再重复出现（Agent 段已经覆盖）
  const builtinUrls = new Set(
    providers
      .map((p) => normalizeUrl(p.default_base_url || ''))
      .filter((url) => url.length > 0),
  );

  // 「自定义地址」的落点（openai_compatible）不在 Agent 段重复展示
  const agentCards = providers
    .filter((p) => p.kind !== 'openai_compatible')
    .filter(
      (p) =>
        !providerSearchNeedle ||
        p.label.toLowerCase().includes(providerSearchNeedle) ||
        p.kind.toLowerCase().includes(providerSearchNeedle) ||
        (p.default_base_url || '').toLowerCase().includes(providerSearchNeedle),
    );

  const presetCards = PRESET_PROVIDERS.filter(
    (p) => !builtinUrls.has(normalizeUrl(p.base_url)),
  ).filter(
    (p) =>
      !providerSearchNeedle ||
      p.name.toLowerCase().includes(providerSearchNeedle) ||
      p.base_url.toLowerCase().includes(providerSearchNeedle),
  );

  const pickProvider = (kind: string, baseUrl: string): void => {
    setForm((prev) => ({ ...prev, provider: kind, base_url: baseUrl }));
  };

  const selectedProviderLabel = providerLabel(form.provider);
  const effectiveEndpoint =
    form.base_url.trim()
    || providers.find((p) => p.kind === form.provider)?.default_base_url
    || '';

  if (loading) return <SkeletonTable columns={7} rows={5} />;

  return (
    <div>
      <div className="page-header">
        <h1>{t('账号池管理')}</h1>
        <p>
          {t(
            '为账号型渠道管理多把上游凭证：账号级优先级/权重/并发上限与按上游模型的冷却，失败自动轮换'
          )}
        </p>
      </div>

      {error && <div className="error-message">{error}</div>}

      {accountChannels.length === 0 && (
        <div className="error-message">
          {t('还没有账号型渠道：添加账号并保存时会按所选供应商自动创建账号型渠道')}
        </div>
      )}

      {stats.length > 0 && (
        <div style={{ display: 'flex', flexWrap: 'wrap', gap: 8, marginBottom: 12 }}>
          {stats.map((st) => (
            <span key={st.channel_id} className="badge badge-info">
              {st.channel_name || st.channel_id}: {st.enabled}/{st.total}
              {st.cooling > 0 ? ` · ${t('冷却中')} ${st.cooling}` : ''}
              {st.in_flight > 0 ? ` · ${t('并发')} ${st.in_flight}` : ''}
            </span>
          ))}
        </div>
      )}

      <Card
        title={`${t('账号池')} (${visible.length})`}
        actions={
          <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap' }}>
            <Button variant="outline" onClick={() => void openRotation()}>
              <SlidersHorizontal size={14} /> {t('账号路由')}
            </Button>
            <Button variant="outline" onClick={openBulk} disabled={accountChannels.length === 0}>
              <Upload size={14} /> {t('批量导入')}
            </Button>
            <Button onClick={openCreate}>
              <Plus size={14} /> {t('添加账号')}
            </Button>
          </div>
        }
      >
        <div style={{ display: 'flex', gap: 12, flexWrap: 'wrap', marginBottom: 12 }}>
          <Select
            value={channelFilter}
            onChange={(e) => setChannelFilter(e.target.value)}
            aria-label={t('选择渠道')}
          >
            <option value="">{t('全部渠道')}</option>
            {accountChannels.map((c) => (
              <option key={c.id} value={c.id}>
                {c.name}
              </option>
            ))}
          </Select>
          <Input
            placeholder={t('搜索名称 / provider / 地址')}
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            aria-label={t('搜索')}
          />
        </div>

        {visible.length === 0 ? (
          <EmptyState
            message={
              accountChannels.length === 0
                ? t('还没有账号：点击「添加账号」，保存时会按所选供应商自动创建账号型渠道')
                : t('暂无账号，点击「添加账号」或「批量导入」开始')
            }
            action={<Button onClick={openCreate}>{t('添加账号')}</Button>}
          />
        ) : (
          <div className="table-wrapper">
            <table>
              <thead>
                <tr>
                  <th>{t('名称')}</th>
                  <th>{t('渠道')}</th>
                  <th>{t('Provider')}</th>
                  <th>{t('状态')}</th>
                  <th>{t('优先级')}</th>
                  <th>{t('并发')}</th>
                  <th>{t('成功率')}</th>
                  <th>{t('冷却')}</th>
                  <th>{t('操作')}</th>
                </tr>
              </thead>
              <tbody>
                {visible.map((session) => (
                  <tr key={session.id}>
                    <td>
                      <div>{session.name}</div>
                      <div className="form-hint">{session.api_key || '—'}</div>
                      {session.proxy && <div className="form-hint">{session.proxy}</div>}
                      {session.last_error && (
                        <div className="form-hint" title={session.last_error}>
                          {shortError(session.last_error)}
                        </div>
                      )}
                    </td>
                    <td>{session.channel_name || session.channel_id}</td>
                    <td>
                      <div>{session.provider}</div>
                      {session.base_url && <div className="form-hint">{session.base_url}</div>}
                    </td>
                    <td>
                      <span className={`badge ${statusClass(session.status)}`}>
                        {statusLabel(session.status)}
                      </span>
                      {(session.in_flight ?? 0) > 0 && (
                        <div className="form-hint">
                          {t('进行中')} {session.in_flight}
                        </div>
                      )}
                    </td>
                    <td>
                      {session.priority ?? 0} / w{session.weight ?? 1}
                    </td>
                    <td>
                      {session.max_concurrency
                        ? `${session.in_flight ?? 0}/${session.max_concurrency}`
                        : t('不限')}
                    </td>
                    <td>{successRate(session)}</td>
                    <td>{cooldownSummary(session)}</td>
                    <td>
                      <div className="actions-cell">
                        <Button
                          variant="outline"
                          size="sm"
                          onClick={() => handleTest(session)}
                          disabled={testingId === session.id}
                        >
                          <FlaskConical size={14} />
                          {testingId === session.id ? t('测试中…') : t('测试')}
                        </Button>
                        <Button
                          variant="outline"
                          size="sm"
                          onClick={() => openEdit(session)}
                          title={t('编辑')}
                        >
                          <Pencil size={14} />
                        </Button>
                        <Button
                          variant="outline"
                          size="sm"
                          onClick={() => handleToggleStatus(session)}
                          disabled={session.status === 'expired'}
                          title={
                            session.status === 'expired'
                              ? t('凭证已过期，请先更新凭证或到期时间')
                              : session.status === 'enabled'
                                ? t('停用')
                                : t('启用')
                          }
                        >
                          <Power size={14} />
                        </Button>
                        <Button
                          variant="outline"
                          size="sm"
                          onClick={() => handleResetCooldowns(session)}
                          disabled={Object.keys(session.cooldowns || {}).length === 0}
                          title={t('重置冷却')}
                        >
                          <RotateCcw size={14} />
                        </Button>
                        <Button
                          variant="danger"
                          size="sm"
                          onClick={() => handleDelete(session)}
                          title={t('删除')}
                        >
                          <Trash2 size={14} />
                        </Button>
                      </div>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </Card>

      {showModal && (
        <div className="modal-overlay">
          <form className="modal" onSubmit={handleSave}>
            <div className="modal-header">
              <h3>{editing ? t('编辑账号') : t('添加账号')}</h3>
              <button type="button" className="modal-close" onClick={closeModal}>
                &times;
              </button>
            </div>
            <div className="modal-body">
              <Select
                label={t('渠道')}
                value={form.channel_id}
                onChange={(e) => setForm({ ...form, channel_id: e.target.value })}
                disabled={Boolean(editing)}
              >
                {accountChannels.length === 0 && (
                  <option value="">{t('保存时自动创建（按所选供应商）')}</option>
                )}
                {accountChannels.map((c) => (
                  <option key={c.id} value={c.id}>
                    {c.name}
                  </option>
                ))}
              </Select>
              <Input
                label={t('账号名（可选）')}
                placeholder="account-1"
                value={form.name}
                onChange={(e) => setForm({ ...form, name: e.target.value })}
              />
              {editing ? (
                <Select
                  label={t('Provider')}
                  value={form.provider}
                  onChange={(e) => setForm({ ...form, provider: e.target.value })}
                >
                  {providers.map((p) => (
                    <option key={p.kind} value={p.kind}>
                      {p.label}
                    </option>
                  ))}
                </Select>
              ) : (
                <div className="provider-picker">
                  <div className="provider-picker-head">
                    <span className="provider-picker-label">{t('选择供应商')}</span>
                    <span className="provider-picker-selected" title={effectiveEndpoint}>
                      {t('已选')}：{selectedProviderLabel}
                      {effectiveEndpoint ? ` · ${effectiveEndpoint}` : ''}
                    </span>
                  </div>
                  <Tabs<ProviderSegment>
                    items={[
                      { key: 'agent', label: t('Agent（内置）') },
                      { key: 'preset', label: t('预置 API') },
                      { key: 'custom', label: t('自定义') },
                    ]}
                    active={pickSegment}
                    onChange={(key) => {
                      setPickSegment(key);
                      setProviderSearch('');
                    }}
                    ariaLabel={t('供应商类型')}
                    className="provider-picker-tabs"
                  />
                  {pickSegment !== 'custom' && (
                    <Input
                      placeholder={t('搜索供应商')}
                      value={providerSearch}
                      onChange={(e) => setProviderSearch(e.target.value)}
                    />
                  )}
                  <div className="provider-grid" role="listbox" aria-label={t('供应商列表')}>
                    {pickSegment === 'agent' &&
                      agentCards.map((p) => {
                        const active =
                          form.provider === p.kind &&
                          normalizeUrl(form.base_url) === normalizeUrl(p.default_base_url);
                        return (
                          <button
                            type="button"
                            key={p.kind}
                            role="option"
                            aria-selected={active}
                            className={`provider-card${active ? ' active' : ''}`}
                            onClick={() => pickProvider(p.kind, '')}
                          >
                            <span className="provider-card-title">{p.label}</span>
                            <span className="provider-card-meta">
                              {p.default_base_url || t('自定义地址')}
                            </span>
                          </button>
                        );
                      })}
                    {pickSegment === 'preset' &&
                      presetCards.map((p) => {
                        const kind = presetKind(p.protocol);
                        const active =
                          form.provider === kind &&
                          normalizeUrl(form.base_url) === normalizeUrl(p.base_url);
                        return (
                          <button
                            type="button"
                            key={p.key}
                            role="option"
                            aria-selected={active}
                            className={`provider-card${active ? ' active' : ''}`}
                            onClick={() => pickProvider(kind, p.base_url)}
                          >
                            <span className="provider-card-title">{p.name}</span>
                            <span className="provider-card-meta">{p.base_url}</span>
                          </button>
                        );
                      })}
                    {pickSegment === 'custom' &&
                      CUSTOM_PROTOCOLS.map((p) => {
                        const active = form.provider === p.kind;
                        return (
                          <button
                            type="button"
                            key={p.kind}
                            role="option"
                            aria-selected={active}
                            className={`provider-card${active ? ' active' : ''}`}
                            onClick={() => pickProvider(p.kind, '')}
                          >
                            <span className="provider-card-title">{t(p.labelKey)}</span>
                            <span className="provider-card-meta">{t(p.hintKey)}</span>
                          </button>
                        );
                      })}
                    {pickSegment !== 'custom' &&
                      ((pickSegment === 'agent' && !agentCards.length) ||
                        (pickSegment === 'preset' && !presetCards.length)) && (
                        <div className="provider-grid-empty">{t('没有匹配的供应商')}</div>
                      )}
                  </div>
                </div>
              )}
              <Input
                label={t('上游地址（可选，留空用 provider 默认）')}
                placeholder="https://api.deepseek.com"
                value={form.base_url}
                onChange={(e) => setForm({ ...form, base_url: e.target.value })}
              />
              <Input
                label={t('凭证')}
                type="password"
                placeholder={editing ? t('留空表示不修改') : 'sk-...'}
                hint={editing ? t('留空保持原凭证；勾选下方可清除') : undefined}
                value={form.api_key}
                onChange={(e) => setForm({ ...form, api_key: e.target.value })}
              />
              {editing && (
                <label style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
                  <input
                    type="checkbox"
                    checked={form.clear_key}
                    onChange={(e) => setForm({ ...form, clear_key: e.target.checked })}
                  />
                  <span>{t('清除现有凭证')}</span>
                </label>
              )}
              <Input
                label={t('出口代理（可选）')}
                placeholder={editing?.proxy || 'socks5://127.0.0.1:1080'}
                hint={editing?.proxy ? t('留空保持当前代理；勾选下方可清除') : undefined}
                value={form.proxy}
                onChange={(e) => setForm({ ...form, proxy: e.target.value })}
              />
              {editing?.proxy && (
                <label style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
                  <input
                    type="checkbox"
                    checked={form.clear_proxy}
                    onChange={(e) => setForm({ ...form, clear_proxy: e.target.checked })}
                  />
                  <span>{t('清除出口代理')}</span>
                </label>
              )}
              <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: 12 }}>
                <Input
                  label={t('优先级')}
                  type="number"
                  value={form.priority}
                  onChange={(e) => setForm({ ...form, priority: e.target.value })}
                />
                <Input
                  label={t('权重')}
                  type="number"
                  min="1"
                  value={form.weight}
                  onChange={(e) => setForm({ ...form, weight: e.target.value })}
                />
              </div>
              <Input
                label={t('并发上限')}
                type="number"
                min="0"
                hint={t('(0 = 不限)')}
                value={form.max_concurrency}
                onChange={(e) => setForm({ ...form, max_concurrency: e.target.value })}
              />
              <Input
                label={t('模型白名单（可选，逗号分隔）')}
                placeholder="deepseek-chat, deepseek-reasoner"
                value={form.models}
                onChange={(e) => setForm({ ...form, models: e.target.value })}
              />
              <Input
                label={t('凭证到期时间（可选）')}
                type="number"
                hint={t('(留空则永不过期)')}
                value={form.expires_at}
                onChange={(e) => setForm({ ...form, expires_at: e.target.value })}
              />
              <Select
                label={t('状态')}
                value={form.status}
                onChange={(e) => setForm({ ...form, status: e.target.value })}
              >
                <option value="enabled">{t('启用')}</option>
                <option value="disabled">{t('停用')}</option>
              </Select>
            </div>
            <div className="modal-footer">
              <Button variant="secondary" onClick={closeModal}>
                {t('取消')}
              </Button>
              <Button type="submit" disabled={saving}>
                {saving ? t('保存中…') : t('保存修改')}
              </Button>
            </div>
          </form>
        </div>
      )}

      {showBulk && (
        <div className="modal-overlay">
          <form className="modal" onSubmit={handleBulk}>
            <div className="modal-header">
              <h3>{t('批量导入')}</h3>
              <button type="button" className="modal-close" onClick={() => setShowBulk(false)}>
                &times;
              </button>
            </div>
            <div className="modal-body">
              <Select
                label={t('渠道')}
                value={bulk.channel_id}
                onChange={(e) => setBulk({ ...bulk, channel_id: e.target.value })}
              >
                {accountChannels.map((c) => (
                  <option key={c.id} value={c.id}>
                    {c.name}
                  </option>
                ))}
              </Select>
              <Select
                label={t('Provider')}
                value={bulk.provider}
                onChange={(e) => setBulk({ ...bulk, provider: e.target.value })}
              >
                {providers.map((p) => (
                  <option key={p.kind} value={p.kind}>
                    {p.label}
                  </option>
                ))}
              </Select>
              <Input
                label={t('上游地址（可选）')}
                placeholder="https://api.deepseek.com"
                value={bulk.base_url}
                onChange={(e) => setBulk({ ...bulk, base_url: e.target.value })}
              />
              <Input
                label={t('名称前缀')}
                value={bulk.name_prefix}
                onChange={(e) => setBulk({ ...bulk, name_prefix: e.target.value })}
              />
              <Input
                label={t('出口代理（可选）')}
                placeholder="socks5://127.0.0.1:1080"
                value={bulk.proxy}
                onChange={(e) => setBulk({ ...bulk, proxy: e.target.value })}
              />
              <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: 12 }}>
                <Input
                  label={t('优先级')}
                  type="number"
                  value={bulk.priority}
                  onChange={(e) => setBulk({ ...bulk, priority: e.target.value })}
                />
                <Input
                  label={t('权重')}
                  type="number"
                  min="1"
                  value={bulk.weight}
                  onChange={(e) => setBulk({ ...bulk, weight: e.target.value })}
                />
              </div>
              <Input
                label={t('并发上限')}
                type="number"
                min="0"
                hint={t('(0 = 不限)')}
                value={bulk.max_concurrency}
                onChange={(e) => setBulk({ ...bulk, max_concurrency: e.target.value })}
              />
              <Input
                label={t('模型白名单（可选）')}
                placeholder="deepseek-chat"
                value={bulk.models}
                onChange={(e) => setBulk({ ...bulk, models: e.target.value })}
              />
              <Textarea
                label={t('凭证列表')}
                rows={8}
                placeholder={'sk-aaa\nname2,sk-bbb\n# 注释行会被忽略'}
                hint={t('每行一条：Name,Key 或仅 Key；# 开头为注释')}
                value={bulk.credentials}
                onChange={(e) => setBulk({ ...bulk, credentials: e.target.value })}
              />
            </div>
            <div className="modal-footer">
              <Button variant="secondary" onClick={() => setShowBulk(false)}>
                {t('取消')}
              </Button>
              <Button type="submit" disabled={saving}>
                {saving ? t('导入中…') : t('开始导入')}
              </Button>
            </div>
          </form>
        </div>
      )}

      {showRotation && (
        <div className="modal-overlay">
          <form className="modal" onSubmit={(e) => void saveRotation(e)}>
            <div className="modal-header">
              <h3>{t('账号路由')}</h3>
              <button type="button" className="modal-close" onClick={() => setShowRotation(false)}>
                &times;
              </button>
            </div>
            <div className="modal-body">
              <p style={{ fontSize: 13, color: 'var(--text-muted)' }}>
                {t('账号失败后的冷却窗口与单请求最多尝试几个账号；保存后立即生效并持久化。')}
              </p>
              {rotationError && <div className="error-message">{rotationError}</div>}
              <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: 12 }}>
                <Input
                  label={t('基础冷却（秒）')}
                  type="number"
                  min="5"
                  max="3600"
                  hint={t('连续失败按指数退避，从该值起算（5–3600）')}
                  value={rotation.base}
                  onChange={(e) => setRotation({ ...rotation, base: e.target.value })}
                />
                <Input
                  label={t('冷却上限（秒）')}
                  type="number"
                  min="5"
                  max="86400"
                  hint={t('单账号单模型的最长冷却（不超过 86400）')}
                  value={rotation.max}
                  onChange={(e) => setRotation({ ...rotation, max: e.target.value })}
                />
              </div>
              <Input
                label={t('认证失败冷却（秒）')}
                type="number"
                min="60"
                max="86400"
                hint={t('密钥失效/鉴权失败的长冷却（60–86400）')}
                value={rotation.auth}
                onChange={(e) => setRotation({ ...rotation, auth: e.target.value })}
              />
              <Input
                label={t('单请求最大换号次数')}
                type="number"
                min="1"
                max="20"
                hint={t('一次请求最多尝试几个账号（1–20）')}
                value={rotation.attempts}
                onChange={(e) => setRotation({ ...rotation, attempts: e.target.value })}
              />
            </div>
            <div className="modal-footer">
              <Button variant="secondary" onClick={() => setShowRotation(false)}>
                {t('取消')}
              </Button>
              <Button type="submit" disabled={rotationSaving}>
                {rotationSaving ? t('保存中…') : t('保存修改')}
              </Button>
            </div>
          </form>
        </div>
      )}

      <ConfirmDialog
        state={confirmState}
        onClose={() => setConfirmState(null)}
      />
    </div>
  );
}
