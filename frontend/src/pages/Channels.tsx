import { useState, useEffect } from 'react';
import { useTranslation } from 'react-i18next';
import { Eye, EyeOff, ArrowLeftRight } from 'lucide-react';
import { api } from '../api';
import type { ChannelItem as ApiChannelItem } from '../types';
import { useToast } from '../components/Toast';
import ConfirmDialog, { type ConfirmState } from '../components/ConfirmDialog';
import ChatDebugger from '../components/ChatDebugger';
import ModelMappingEditor from '../components/ModelMappingEditor';
import CostPricingEditor from '../components/CostPricingEditor';
import { GuideEmptyState } from '../components/ui';
import './Channels.css';

interface ChannelItem {
  id: string | number;
  name: string;
  channel_type: string;
  base_url?: string;
  api_key?: string;
  priority: number;
  weight: number;
  status: string;
  models?: string[];
  account_id?: string;
  model_mapping?: Record<string, string>;
  cost_pricing?: Record<string, { input_price?: number; output_price?: number; price_type?: string }>;
  last_error?: string | null;
  last_used_at?: number | null;
  created_at?: number;
  updated_at?: number;
}

interface ChannelFormState {
  name: string;
  channel_type: string;
  base_url: string;
  api_key: string;
  priority: number | string;
  weight: number | string;
  status: string;
  models: string;
  account_id: string;
  model_mapping: Record<string, string>;
  cost_pricing: Record<string, { input_price?: number; output_price?: number; price_type?: string }>;
}

// 渠道类型选项 — 与后端 ChannelType 枚举对齐（snake_case）
const CHANNEL_TYPES = [
  { value: 'cloudflare', labelKey: 'Cloudflare Workers AI', isRaw: true },
  { value: 'openai_compatible', labelKey: 'OpenAI 兼容 (DeepSeek/OpenRouter/...)' },
  { value: 'anthropic', labelKey: 'Anthropic 兼容' },
  { value: 'gemini', labelKey: 'Gemini (Google AI)', isRaw: true },
  { value: 'zai', labelKey: 'Zai (智谱AI)', isRaw: true },
];

// 各渠道类型的默认 Base URL（创建时自动填充，减少用户手动输入）
const DEFAULT_BASE_URL = {
  gemini: 'https://generativelanguage.googleapis.com/v1beta',
  zai: 'https://api.z.ai/api/v2',
};

// 各渠道类型的鉴权方式提示
const AUTH_HINT = {
  gemini: '鉴权方式：x-goog-api-key（在 API Key 字段填入 Google AI Studio 密钥）',
  zai: '鉴权方式：Bearer token（在 API Key 字段填入智谱 API Key）',
};

export default function Channels(): JSX.Element {
  const [channels, setChannels] = useState<ChannelItem[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  const addToast = useToast();
  const { t } = useTranslation();

  const [showModal, setShowModal] = useState(false);
  const [editChannel, setEditChannel] = useState<ChannelItem | null>(null);
  const [form, setForm] = useState<ChannelFormState>(defaultForm());
  const [saving, setSaving] = useState(false);
  const [testingId, setTestingId] = useState<string | number | null>(null);
  const [fetchingModels, setFetchingModels] = useState(false);
  // API Key 输入明文切换（防输错无法核对）
  const [showApiKey, setShowApiKey] = useState(false);

  // ── 确认弹窗状态 ──
  const [confirmState, setConfirmState] = useState<ConfirmState | null>(null);

  // ── 对话调试器状态 ──
  const [showChat, setShowChat] = useState(false);
  const [chatChannel, setChatChannel] = useState<ChannelItem | null>(null);

  function defaultForm(): ChannelFormState {
    return {
      name: '',
      channel_type: 'openai_compatible',
      base_url: '',
      api_key: '',
      priority: 0,
      weight: 1,
      status: 'enabled',
      models: '',
      account_id: '',
      model_mapping: {},
      cost_pricing: {},
    };
  }

  // 挂载时加载一次渠道列表
  useEffect(() => {
    loadChannels().catch((err: unknown) => {
      setError(err instanceof Error ? err.message : String(err));
    });
  }, []);

  const loadChannels = async (): Promise<void> => {
    setLoading(true);
    setError('');
    try {
      const res = await api.listChannels();
      const items: ChannelItem[] = (res?.data ?? []).map((ch: ApiChannelItem) => ({
        id: ch.id as string | number,
        name: ch.name,
        channel_type: ch.channel_type || ch.type || 'openai_compatible',
        base_url: ch.base_url,
        api_key: ch.api_key,
        priority: ch.priority ?? 0,
        weight: ch.weight ?? 1,
        status: ch.status || (ch.enabled ? 'enabled' : 'disabled'),
        models: ch.models,
        model_mapping: ch.model_mapping,
        cost_pricing: ch.cost_pricing || {},
        last_used_at: typeof ch.last_used_at === 'number' ? ch.last_used_at : null,
        created_at: typeof ch.created_at === 'number' ? ch.created_at : undefined,
        updated_at: typeof ch.updated_at === 'number' ? ch.updated_at : undefined,
      }));
      setChannels(items);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  };

  const openAdd = (): void => {
    setEditChannel(null);
    setForm(defaultForm());
    setShowModal(true);
  };

  const openEdit = (ch: ChannelItem): void => {
    setEditChannel(ch);
    setForm({
      name: ch.name || '',
      channel_type: ch.channel_type || 'openai_compatible',
      base_url: ch.base_url || '',
      api_key: '',
      priority: ch.priority ?? 0,
      weight: ch.weight ?? 1,
      status: ch.status || 'enabled',
      models: (ch.models || []).join(', '),
      account_id: ch.account_id || '',
      model_mapping: ch.model_mapping || {},
      cost_pricing: ch.cost_pricing || {},
    });
    setShowModal(true);
  };

  // 关闭渠道弹窗：保存中禁止关闭（防误触丢请求/丢表单）；正常关闭时重置表单
  const closeModal = (): void => {
    if (saving) return;
    setShowModal(false);
    setEditChannel(null);
    setForm(defaultForm());
  };

  // 构建请求 payload — 与后端 ChannelRequest 对齐
  const buildPayload = () => ({
    name: form.name,
    channel_type: form.channel_type,
    base_url: form.base_url,
    api_key: form.api_key,
    priority: parseInt(String(form.priority), 10) || 0,
    weight: parseInt(String(form.weight), 10) || 1,
    status: form.status,
    models: form.models.split(',').map((s) => s.trim()).filter(Boolean),
    account_id: form.account_id,
    model_mapping: form.model_mapping,
    cost_pricing: form.cost_pricing,
  });

  const handleSave = async (): Promise<void> => {
    if (!form.name) { setError(t('名称为必填项')); return; }
    if (form.channel_type !== 'cloudflare' && !form.base_url) {
      setError(t('非 Cloudflare 渠道需填写 Base URL'));
      return;
    }
    setSaving(true);
    setError('');
    try {
      const payload = buildPayload();
      if (editChannel) {
        await api.updateChannel(editChannel.id, payload);
        addToast(t('渠道更新成功'));
      } else {
        if (!form.api_key) { setError(t('新渠道必填 API Key')); setSaving(false); return; }
        await api.addChannel(payload);
        addToast(t('渠道添加成功'));
      }
      closeModal();
      loadChannels();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  };

  const handleTest = async (id: string | number): Promise<void> => {
    setTestingId(id);
    setError('');
    try {
      const res = await api.testChannel(id);
      const data = res.data || {};
      addToast(data.success
        ? `${t('连通')}: ${data.message || ''} (${data.latency_ms || 0}ms)`
        : `${t('失败')}: ${data.message || ''}`);
      loadChannels();
    } catch (err) {
      addToast(`${t('测试失败')}: ${err instanceof Error ? err.message : String(err)}`);
    } finally {
      setTestingId(null);
    }
  };

  // 手动重置渠道断路器（渠道被熔断后恢复）— 恢复放行可能仍有故障的渠道流量，需确认
  const handleResetCircuit = (id: string | number): void => {
    setConfirmState({
      title: t('重置断路器'),
      message: t('重置后将立即恢复向该渠道放行流量。若渠道仍存在故障，可能再次触发熔断。确定继续？'),
      confirmText: t('重置'),
      onConfirm: async () => {
        setError('');
        try {
          await api.resetChannelCircuit(id);
          addToast(t('断路器已重置'));
          loadChannels();
        } catch (err) {
          addToast(`${t('重置失败')}: ${err instanceof Error ? err.message : String(err)}`, 'error');
        }
      },
    });
  };

  // 拉取上游模型列表 — 后端代理转发（避免浏览器 CORS）
  const handleFetchModels = async (): Promise<void> => {
    if (form.channel_type !== 'cloudflare' && !form.base_url.trim()) {
      setError(t('请先填写 Base URL'));
      return;
    }
    setFetchingModels(true);
    setError('');
    try {
      const res = await api.fetchChannelModels({
        channel_type: form.channel_type,
        base_url: form.base_url,
        api_key: form.api_key,
        channel_id: editChannel ? editChannel.id : '',
      });
      const models = res.data?.models || [];
      if (models.length === 0) {
        addToast(t('未拉取到模型（上游未返回模型列表）'));
      } else {
        setForm((f) => ({ ...f, models: models.join(', ') }));
        addToast(`${t('已拉取')} ${models.length} ${t('个模型')}`);
      }
    } catch (err) {
      addToast(`${t('拉取失败')}: ${err instanceof Error ? err.message : String(err)}`);
    } finally {
      setFetchingModels(false);
    }
  };

  // ── 对话调试器 ──
  // 弹窗内渲染统一的 ChatDebugger 组件（与 Playground 共用同一后端入口
  // /api/channels/chat_test），协议/模型/流式/多模态行为完全一致。
  const openChat = (ch: ChannelItem): void => {
    setChatChannel(ch);
    setShowChat(true);
  };

  const closeChat = (): void => {
    setShowChat(false);
    setChatChannel(null);
  };

  // PATCH 部分更新 — 仅传 status 字段，避免脱敏 api_key 覆盖真实密钥。
  // 停用直接影响生产流量 → 确认弹窗；启用方向直接执行。
  const handleToggle = (ch: ChannelItem): void => {
    const disabling = ch.status === 'enabled';
    const doToggle = async (): Promise<void> => {
      setError('');
      try {
        const newStatus = disabling ? 'disabled' : 'enabled';
        await api.patchChannel(ch.id, { status: newStatus });
        addToast(disabling ? t('渠道已停用') : t('渠道已启用'));
        loadChannels();
      } catch (err) {
        setError(err instanceof Error ? err.message : String(err));
      }
    };
    if (disabling) {
      setConfirmState({
        title: t('停用渠道'),
        message: t('停用后该渠道不再接收新请求。确定继续？'),
        confirmText: t('停用'),
        danger: true,
        onConfirm: doToggle,
      });
      return;
    }
    void doToggle();
  };

  const handleDelete = (id: string | number): void => {
    setConfirmState({
      title: t('删除渠道'),
      message: t('确定删除此渠道？'),
      confirmText: t('删除'),
      danger: true,
      onConfirm: async () => {
        setError('');
        try {
          await api.deleteChannel(id);
          addToast(t('渠道已删除'));
          loadChannels();
        } catch (err) {
          setError(err instanceof Error ? err.message : String(err));
        }
      },
    });
  };

  const typeLabel = (val: string): string => {
    const found = CHANNEL_TYPES.find((x) => x.value === val);
    if (!found) return val;
    return found.isRaw ? found.labelKey : t(found.labelKey);
  };

  return (
    <div className="channels-shell">
      {/* PageIntro 标题区 */}
      <div className="page-header">
        <div>
          <h1>{t('渠道管理')}</h1>
          <p>{t('管理上游 AI 渠道（支持混用 Cloudflare + 第三方 OpenAI 兼容上游）')}</p>
        </div>
      </div>

      {error && <div className="error-message">{error}</div>}

      <div className="channels-content">
        {loading ? (
          <div className="loading">{t('加载渠道列表')}</div>
        ) : (
          <div className="card">
            <div className="card-header">
              <h2>{t('所有渠道')} ({channels.length})</h2>
              <button className="btn btn-primary" onClick={openAdd}>{t('+ 添加渠道')}</button>
            </div>
            <div className="card-body">
              {channels.length === 0 ? (
                <GuideEmptyState
                  icon="🛰️"
                  title={t('暂无渠道')}
                  hint={t('渠道是上游 AI 服务的接入点：填入 Base URL 与密钥后即可转发请求。')}
                  action={<button className="btn btn-primary" onClick={openAdd}>{t('添加第一个渠道')}</button>}
                />
              ) : (
                <div className="table-wrapper">
                  <table>
                    <thead>
                      <tr>
                        <th>{t('名称')}</th>
                        <th>{t('类型')}</th>
                        <th>Base URL</th>
                        <th>{t('优先级/权重')}</th>
                        <th>{t('模型')}</th>
                        <th>{t('状态')}</th>
                        <th>{t('操作')}</th>
                      </tr>
                    </thead>
                    <tbody>
                      {channels.map((ch) => (
                        <tr key={ch.id}>
                          <td><strong>{ch.name}</strong></td>
                          <td>
                            <span className="channel-type-badge" data-type={ch.channel_type}>
                              {typeLabel(ch.channel_type)}
                            </span>
                          </td>
                          <td style={{ fontSize: 13, color: 'var(--text-muted)' }}>
                            {ch.base_url || ch.account_id || '—'}
                          </td>
                          <td>
                            <span className="priority-weight">
                              <span className="pw-priority">{ch.priority}</span>
                              <span className="pw-sep">/</span>
                              <span className="pw-weight">{ch.weight}</span>
                            </span>
                          </td>
                          <td style={{ fontSize: 13, color: 'var(--text-muted)' }}>
                            {(ch.models || []).length > 6
                              ? <span title={(ch.models || []).join(', ')}>
                                  {(ch.models || []).slice(0, 6).join(', ')} +{ch.models!.length - 6}
                                </span>
                              : (ch.models || []).join(', ') || t('全部')}
                            {Object.keys(ch.model_mapping || {}).length > 0 && (
                              <div className="channel-mapping-count" title={Object.entries(ch.model_mapping!).map(([k, v]) => `${k} → ${v}`).join('\n')}>
                                <ArrowLeftRight size={11} />
                                {Object.keys(ch.model_mapping!).length} {t('条映射')}
                              </div>
                            )}
                          </td>
                          <td>
                            {ch.status === 'enabled'
                              ? <span className="badge badge-success">{t('启用')}</span>
                              : <span className="badge badge-danger" title={ch.last_error || ''}>{t('禁用')}</span>}
                          </td>
                          <td>
                            <div className="actions-cell">
                              <button
                                className="btn btn-outline btn-sm"
                                onClick={() => openChat(ch)}
                                title={t('对话调试')}
                              >
                                {t('对话')}
                              </button>
                              <button
                                className="btn btn-outline btn-sm"
                                onClick={() => handleTest(ch.id)}
                                disabled={testingId === ch.id}
                              >
                                {testingId === ch.id ? '...' : t('连通')}
                              </button>
                              <button className="btn btn-outline btn-sm" onClick={() => handleToggle(ch)}>
                                {ch.status === 'enabled' ? t('停用') : t('启用')}
                              </button>
                              <button
                                className="btn btn-outline btn-sm"
                                onClick={() => handleResetCircuit(ch.id)}
                                title={t('重置断路器（渠道被熔断后恢复）')}
                              >
                                {t('重置')}
                              </button>
                              <button className="btn btn-outline btn-sm" onClick={() => openEdit(ch)}>{t('编辑')}</button>
                              <button className="btn btn-danger btn-sm" onClick={() => handleDelete(ch.id)}>{t('删除')}</button>
                            </div>
                          </td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
              )}
            </div>
          </div>
        )}
      </div>

      {showModal && (
        <div className="modal-overlay">
          <div className="modal">
            <div className="modal-header">
              <h3>{editChannel ? t('编辑渠道') : t('添加渠道')}</h3>
              <button className="modal-close" onClick={closeModal}>&times;</button>
            </div>
            <div className="modal-body">
              <div className="form-group">
                <label>{t('名称')}</label>
                <input className="form-input" placeholder={t('名称')} value={form.name}
                  onChange={(e) => setForm({ ...form, name: e.target.value })} />
              </div>
              <div className="form-group">
                <label>{t('渠道类型')}</label>
                <select className="form-input" value={form.channel_type}
                  onChange={(e) => {
                    const newType = e.target.value;
                    // 切换类型时：若当前 base_url 为空或是某个类型的默认值，则自动填充新类型的默认 URL
                    const isUsingDefault = Object.values(DEFAULT_BASE_URL).includes(form.base_url) || !form.base_url;
                    setForm({
                      ...form,
                      channel_type: newType,
                      base_url: isUsingDefault ? (DEFAULT_BASE_URL[newType as keyof typeof DEFAULT_BASE_URL] || '') : form.base_url,
                    });
                  }}>
                  {CHANNEL_TYPES.map((tp) => (
                    <option key={tp.value} value={tp.value}>
                      {tp.isRaw ? tp.labelKey : t(tp.labelKey)}
                    </option>
                  ))}
                </select>
                {/* Gemini / Zai 鉴权方式提示 */}
                {AUTH_HINT[form.channel_type as keyof typeof AUTH_HINT] && (
                  <div className="form-hint" style={{ color: 'var(--accent-color)' }}>
                    {t(AUTH_HINT[form.channel_type as keyof typeof AUTH_HINT]!)}
                  </div>
                )}
              </div>
              {form.channel_type === 'cloudflare' ? (
                <div className="form-group">
                  <label>{t('Cloudflare 账号 ID')}</label>
                  <input className="form-input" placeholder={t('Cloudflare 账号 ID')} value={form.account_id}
                    onChange={(e) => setForm({ ...form, account_id: e.target.value })} />
                </div>
              ) : (
                <div className="form-group">
                  <label>Base URL</label>
                  <input className="form-input" placeholder="https://cf-ai-gw.pages.dev 或 https://api.deepseek.com/v1" value={form.base_url}
                    onChange={(e) => setForm({ ...form, base_url: e.target.value })} />
                  <div className="form-hint">{t('未带 /v1 时会自动补齐；例如 cf-ai-gw 填 https://cf-ai-gw.pages.dev 即可')}</div>
                </div>
              )}
              <div className="form-group">
                <label>API Key {editChannel && t('（留空则保持不变）')}</label>
                <div className="password-input-wrap">
                  <input
                    className="form-input password-input"
                    type={showApiKey ? 'text' : 'password'}
                    placeholder={editChannel ? t('留空保持当前值') : 'API Key'}
                    value={form.api_key}
                    onChange={(e) => setForm({ ...form, api_key: e.target.value })}
                  />
                  <button
                    type="button"
                    className="password-input-toggle"
                    tabIndex={-1}
                    onClick={() => setShowApiKey((v) => !v)}
                    aria-label={showApiKey ? t('隐藏') : t('显示')}
                    title={showApiKey ? t('隐藏') : t('显示')}
                  >
                    {showApiKey ? <EyeOff size={14} /> : <Eye size={14} />}
                  </button>
                </div>
              </div>
              <div className="form-group">
                <label>{t('支持的模型（逗号分隔，留空=全部）')}</label>
                <div style={{ display: 'flex', gap: 8 }}>
                  <input className="form-input" placeholder="deepseek-chat, deepseek-coder" value={form.models}
                    onChange={(e) => setForm({ ...form, models: e.target.value })} />
                  <button
                    type="button"
                    className="btn btn-outline"
                    style={{ whiteSpace: 'nowrap', flexShrink: 0 }}
                    onClick={handleFetchModels}
                    disabled={fetchingModels}
                    title={t('从上游拉取模型列表')}
                  >
                    {fetchingModels ? t('拉取中...') : t('拉取模型')}
                  </button>
                </div>
              </div>
              <div className="form-group">
                <label>{t('模型映射')}</label>
                <div className="form-hint" style={{ marginBottom: 8 }}>
                  {t('用户请求的模型名 → 实际转发给上游的模型名。优先级：渠道级映射 > 全局映射 > 原名透传。')}
                </div>
                <ModelMappingEditor
                  value={form.model_mapping}
                  onChange={(mapping) => setForm((f) => ({ ...f, model_mapping: mapping }))}
                  sourceModelOptions={form.models.split(',').map((s) => s.trim()).filter(Boolean)}
                  disabled={saving}
                />
              </div>
              <div className="form-group">
                <label>{t('成本价（可选）')}</label>
                <div className="form-hint" style={{ marginBottom: 8 }}>
                  {t('按映射后的上游模型名填成本价（每 1k token）。可留空——填了管理员可在日志看利润，不填利润显示「—」。')}
                </div>
                <CostPricingEditor
                  value={form.cost_pricing}
                  mappingTargets={Object.values(form.model_mapping)}
                  onChange={(cp) => setForm((f) => ({ ...f, cost_pricing: cp }))}
                  disabled={saving}
                />
              </div>
              <div style={{ display: 'flex', gap: 12 }}>
                <div className="form-group" style={{ flex: 1 }}>
                  <label>{t('优先级（越大越优先）')}</label>
                  <input className="form-input" type="number" value={form.priority}
                    onChange={(e) => setForm({ ...form, priority: e.target.value })} />
                </div>
                <div className="form-group" style={{ flex: 1 }}>
                  <label>{t('权重')}</label>
                  <input className="form-input" type="number" value={form.weight}
                    onChange={(e) => setForm({ ...form, weight: e.target.value })} />
                </div>
              </div>
              <div className="form-group">
                <label>{t('状态')}</label>
                <select className="form-input" value={form.status}
                  onChange={(e) => setForm({ ...form, status: e.target.value })}>
                  <option value="enabled">{t('启用')}</option>
                  <option value="disabled">{t('禁用')}</option>
                </select>
              </div>
            </div>
            <div className="modal-footer">
              <button className="btn btn-outline" onClick={closeModal} disabled={saving}>{t('取消')}</button>
              <button className="btn btn-primary" onClick={handleSave} disabled={saving}>
                {saving ? t('保存中...') : (editChannel ? t('更新') : t('添加'))}
              </button>
            </div>
          </div>
        </div>
      )}

      <ConfirmDialog state={confirmState} onClose={() => setConfirmState(null)} />

      {showChat && chatChannel && (
        <div className="modal-overlay" onClick={closeChat}>
          <div className="modal modal-chat" onClick={(e) => e.stopPropagation()}>
            <div className="modal-header">
              <h3>{t('对话调试')} — {chatChannel.name}</h3>
              <button className="modal-close" onClick={closeChat}>&times;</button>
            </div>
            <div className="modal-body chat-modal-body">
              <ChatDebugger
                channelId={String(chatChannel.id)}
                channelModels={chatChannel.models || []}
                initialProtocol={chatChannel.channel_type === 'anthropic' ? 'anthropic' : 'openai'}
                compact
              />
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
