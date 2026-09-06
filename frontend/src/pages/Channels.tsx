import { useState, useEffect, useRef, type KeyboardEvent } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api';
import { useToast } from '../components/Toast';
import ConfirmDialog, { type ConfirmState } from '../components/ConfirmDialog';
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
}

interface ChatMsg {
  role: 'user' | 'assistant';
  content: string;
}

interface TestChatStreamChunk {
  content?: string;
}

interface TestChatResponse {
  stream?: TestChatStreamChunk[];
  data?: { content?: string; error?: string };
}

// 支持的对话协议选项 — 与后端 ChatTester 对齐
const CHAT_PROTOCOLS = [
  { value: 'openai', labelKey: 'OpenAI /v1/chat/completions' },
  { value: 'anthropic', labelKey: 'Anthropic /v1/messages' },
];

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

  // ── 确认弹窗状态 ──
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const [confirmState, setConfirmState] = useState<ConfirmState | null>(null);

  // ── 对话调试器状态 ──
  const [showChat, setShowChat] = useState(false);
  const [chatChannel, setChatChannel] = useState<ChannelItem | null>(null);
  const [chatProtocol, setChatProtocol] = useState('openai');
  const [chatModel, setChatModel] = useState('glm-4.7-flash');
  const [chatMessages, setChatMessages] = useState<ChatMsg[]>([]);
  const [chatInput, setChatInput] = useState('');
  const [chatBusy, setChatBusy] = useState(false);
  const [chatModels, setChatModels] = useState<string[]>([]);
  const chatEndRef = useRef<HTMLDivElement | null>(null);

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
      setChannels(res.data || []);
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
    });
    setShowModal(true);
  };

  const closeModal = (): void => {
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

  // 手动重置渠道断路器（渠道被熔断后恢复）
  const handleResetCircuit = async (id: string | number): Promise<void> => {
    setError('');
    try {
      await api.resetChannelCircuit(id);
      addToast(t('断路器已重置'));
      loadChannels();
    } catch (err) {
      addToast(`${t('重置失败')}: ${err instanceof Error ? err.message : String(err)}`);
    }
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
  // 打开聊天时异步拉取网关可用模型列表（/v1/models），与渠道自身 models 合并；
  // 拉取失败则静默退回渠道 models，避免调试被模型列表接口拖累。
  const openChat = (ch: ChannelItem): void => {
    setChatChannel(ch);
    setChatProtocol(ch.channel_type === 'anthropic' ? 'anthropic' : 'openai');
    const models: string[] = ch.models || [];
    setChatModel(models.length ? models[0] : 'glm-4.7-flash');
    setChatMessages([]);
    setChatInput('');
    setShowChat(true);
    setChatModels([]);
    api.listModels()
      .then((res) => {
        const list = res?.data || res || [];
        if (Array.isArray(list) && list.length) {
          setChatModels(list);
          // 若渠道 models 为空，用网关模型列表的第一个作为默认
          if (!models.length && chatModel === 'glm-4.7-flash') {
            setChatModel(list[0]);
          }
        }
      })
      .catch(() => {
        // 忽略模型列表接口失败，退回渠道 models
      });
  };

  const closeChat = (): void => {
    setShowChat(false);
    setChatChannel(null);
    setChatMessages([]);
    setChatModels([]);
  };

  const chatModelOptions = (): string[] => {
    const ch = chatChannel;
    if (!ch) return [];
    const list = (ch.models || []).slice();
    // 合并网关可用模型（去重，渠道配置优先）
    for (const m of chatModels) {
      if (!list.includes(m)) list.push(m);
    }
    return list.length ? list : ['glm-4.7-flash'];
  };

  const appendChatMessage = (role: 'user' | 'assistant', content: string): void => {
    setChatMessages((prev) => [...prev, { role, content }]);
  };

  const handleChatSend = async (): Promise<void> => {
    const ch = chatChannel;
    const text = chatInput.trim();
    if (!ch || !text || chatBusy) return;
    // 用函数式更新追加用户消息，避免与流式累积的 assistant 消息发生竞态
    setChatMessages((prev) => [...prev, { role: 'user', content: text }]);
    setChatInput('');
    setChatBusy(true);
    try {
      const history = chatMessages.map((m) => ({ role: m.role, content: m.content }));
      const res = (await api.testChannelChat({
        channel_id: ch.id,
        protocol: chatProtocol,
        model: chatModel,
        message: text,
        history,
        stream: true,
      })) as TestChatResponse;
      if (res && res.stream) {
        // 流式：逐增量累积
        let acc = '';
        for (const chk of res.stream) {
          acc += chk.content || '';
          setChatMessages((prev) => {
            const next = prev.slice();
            if (next.length && next[next.length - 1].role === 'assistant') {
              next[next.length - 1] = { role: 'assistant', content: acc };
            } else {
              next.push({ role: 'assistant', content: acc });
            }
            return next;
          });
        }
        setChatMessages((prev) => {
          if (!prev.length || prev[prev.length - 1].role !== 'assistant') {
            return [...prev, { role: 'assistant', content: acc }];
          }
          return prev;
        });
      } else {
        const data = (res && res.data) || {};
        if (data.content) {
          appendChatMessage('assistant', data.content);
        } else if (data.error) {
          appendChatMessage('assistant', `⚠️ ${data.error}`);
        }
      }
    } catch (err) {
      appendChatMessage('assistant', `⚠️ ${err instanceof Error ? err.message : String(err)}`);
    } finally {
      setChatBusy(false);
    }
  };

  const handleChatKeyDown = (e: KeyboardEvent<HTMLTextAreaElement>): void => {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      handleChatSend();
    }
  };

  useEffect(() => {
    if (chatEndRef.current) chatEndRef.current.scrollIntoView({ behavior: 'smooth', block: 'end' });
  }, [chatMessages]);

  // PATCH 部分更新 — 仅传 status 字段，避免脱敏 api_key 覆盖真实密钥
  const handleToggle = async (ch: ChannelItem): Promise<void> => {
    try {
      const newStatus = ch.status === 'enabled' ? 'disabled' : 'enabled';
      await api.patchChannel(ch.id, { status: newStatus });
      loadChannels();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
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
                <div className="empty-state">
                  <p>{t('暂无渠道')}</p>
                  <button className="btn btn-primary" onClick={openAdd}>{t('添加第一个渠道')}</button>
                </div>
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
                            {(ch.models || []).join(', ') || t('全部')}
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
        <div className="modal-overlay" onClick={closeModal}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
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
                <input className="form-input" type="password"
                  placeholder={editChannel ? t('留空保持当前值') : 'API Key'} value={form.api_key}
                  onChange={(e) => setForm({ ...form, api_key: e.target.value })} />
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
              <button className="btn btn-outline" onClick={closeModal}>{t('取消')}</button>
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
              <div className="chat-toolbar">
                <label className="chat-field">
                  <span>{t('协议')}</span>
                  <select className="form-input" value={chatProtocol}
                    onChange={(e) => {
                      setChatProtocol(e.target.value);
                      setChatMessages([]);
                    }}>
                    {CHAT_PROTOCOLS.map((p) => (
                      <option key={p.value} value={p.value}>{t(p.labelKey)}</option>
                    ))}
                  </select>
                </label>
                <label className="chat-field">
                  <span>{t('模型')}</span>
                  <select className="form-input" value={chatModel}
                    onChange={(e) => {
                      setChatModel(e.target.value);
                      setChatMessages([]);
                    }}>
                    {chatModelOptions().map((m) => (
                      <option key={m} value={m}>
                        {chatChannel.name} / {m}
                      </option>
                    ))}
                  </select>
                </label>
              </div>
              <div className="chat-messages">
                {chatMessages.length === 0 && (
                  <div className="chat-empty">{t('输入消息开始对话，用于验证当前渠道能否正常对话')}</div>
                )}
                {chatMessages.map((m, i) => (
                  <div key={i} className={`chat-msg chat-msg-${m.role}`}>
                    <span className="chat-msg-role">{m.role === 'user' ? t('用户') : t('助手')}</span>
                    <div className="chat-msg-content">{m.content}</div>
                  </div>
                ))}
                {chatBusy && <div className="chat-busy">{t('思考中...')}</div>}
                <div ref={chatEndRef} />
              </div>
              <div className="chat-input-row">
                <textarea
                  className="form-input chat-input"
                  rows={2}
                  placeholder={t('输入消息，Enter 发送，Shift+Enter 换行')}
                  value={chatInput}
                  onChange={(e) => setChatInput(e.target.value)}
                  onKeyDown={handleChatKeyDown}
                  disabled={chatBusy}
                />
                <button className="btn btn-primary" onClick={handleChatSend} disabled={chatBusy || !chatInput.trim()}>
                  {t('发送')}
                </button>
              </div>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
