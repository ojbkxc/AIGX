import React, { useState, useEffect } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api';
import { useToast } from '../components/Toast';
import './Channels.css';

// 渠道类型选项 — 与后端 ChannelType 枚举对齐（snake_case）
const CHANNEL_TYPES = [
  { value: 'cloudflare', labelKey: 'Cloudflare Workers AI', isRaw: true },
  { value: 'openai_compatible', labelKey: 'OpenAI 兼容 (DeepSeek/OpenRouter/...)' },
  { value: 'anthropic', labelKey: 'Anthropic 兼容' },
];

export default function Channels() {
  const [channels, setChannels] = useState([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  const addToast = useToast();
  const { t } = useTranslation();

  const [showModal, setShowModal] = useState(false);
  const [editChannel, setEditChannel] = useState(null);
  const [form, setForm] = useState(defaultForm());
  const [saving, setSaving] = useState(false);
  const [testingId, setTestingId] = useState(null);
  const [fetchingModels, setFetchingModels] = useState(false);

  function defaultForm() {
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

  useEffect(() => { loadChannels(); }, []);

  const loadChannels = async () => {
    setLoading(true);
    setError('');
    try {
      const res = await api.listChannels();
      setChannels(res.data || []);
    } catch (err) {
      setError(err.message);
    } finally {
      setLoading(false);
    }
  };

  const openAdd = () => {
    setEditChannel(null);
    setForm(defaultForm());
    setShowModal(true);
  };

  const openEdit = (ch) => {
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

  const closeModal = () => {
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
    priority: parseInt(form.priority, 10) || 0,
    weight: parseInt(form.weight, 10) || 1,
    status: form.status,
    models: form.models.split(',').map((s) => s.trim()).filter(Boolean),
    account_id: form.account_id,
  });

  const handleSave = async () => {
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
      setError(err.message);
    } finally {
      setSaving(false);
    }
  };

  const handleTest = async (id) => {
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
      addToast(`${t('测试失败')}: ${err.message}`);
    } finally {
      setTestingId(null);
    }
  };

  // 拉取上游模型列表 — 后端代理转发（避免浏览器 CORS）
  const handleFetchModels = async () => {
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
      addToast(`${t('拉取失败')}: ${err.message}`);
    } finally {
      setFetchingModels(false);
    }
  };

  // PATCH 部分更新 — 仅传 status 字段，避免脱敏 api_key 覆盖真实密钥
  const handleToggle = async (ch) => {
    try {
      const newStatus = ch.status === 'enabled' ? 'disabled' : 'enabled';
      await api.patchChannel(ch.id, { status: newStatus });
      loadChannels();
    } catch (err) {
      setError(err.message);
    }
  };

  const handleDelete = async (id) => {
    if (!window.confirm(t('确定删除此渠道？'))) return;
    setError('');
    try {
      await api.deleteChannel(id);
      addToast(t('渠道已删除'));
      loadChannels();
    } catch (err) {
      setError(err.message);
    }
  };

  const typeLabel = (val) => {
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
                                onClick={() => handleTest(ch.id)}
                                disabled={testingId === ch.id}
                              >
                                {testingId === ch.id ? '...' : t('测试')}
                              </button>
                              <button className="btn btn-outline btn-sm" onClick={() => handleToggle(ch)}>
                                {ch.status === 'enabled' ? t('停用') : t('启用')}
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
                  onChange={(e) => setForm({ ...form, channel_type: e.target.value })}>
                  {CHANNEL_TYPES.map((tp) => (
                    <option key={tp.value} value={tp.value}>
                      {tp.isRaw ? tp.labelKey : t(tp.labelKey)}
                    </option>
                  ))}
                </select>
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
    </div>
  );
}