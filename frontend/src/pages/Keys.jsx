import React, { useState, useEffect } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api';
import { useToast } from '../components/Toast';
import ConfirmDialog from '../components/ConfirmDialog';
import './Keys.css';

const EMPTY_FORM = {
  name: '',
  group: 'default',
  allowed_models: '',
  expires_at: '',
  quota_limit: '',
  ip_limit: '',
  status: 'active',
};

export default function Keys() {
  const [tokens, setTokens] = useState([]);
  const [groups, setGroups] = useState([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  const addToast = useToast();
  const { t } = useTranslation();

  const [confirmState, setConfirmState] = useState(null);

  const [showModal, setShowModal] = useState(false);
  const [editing, setEditing] = useState(null);
  const [form, setForm] = useState(EMPTY_FORM);
  const [saving, setSaving] = useState(false);
  const [generatedKey, setGeneratedKey] = useState(null);

  // ── 令牌轮换后展示的新密钥（一次性显示，提示用户立即保存）──
  const [rotatedKey, setRotatedKey] = useState(null);

  useEffect(() => {
    load();
  }, []);

  const load = async () => {
    setLoading(true);
    setError('');
    try {
      const [tokenRes, groupRes] = await Promise.all([
        api.listTokens(),
        api.listGroups().catch(() => null),
      ]);
      setTokens(tokenRes.data || tokenRes || []);
      if (groupRes) setGroups(groupRes.data || groupRes || []);
    } catch (err) {
      setError(err.message);
    } finally {
      setLoading(false);
    }
  };

  const openCreate = () => {
    setEditing(null);
    setForm(EMPTY_FORM);
    setGeneratedKey(null);
    setShowModal(true);
  };

  const openEdit = (tk) => {
    setEditing(tk);
    setForm({
      name: tk.name || '',
      group: tk.group || 'default',
      allowed_models: Array.isArray(tk.allowed_models) ? tk.allowed_models.join(', ') : (tk.allowed_models || ''),
      expires_at: tk.expires_at ? String(tk.expires_at) : '',
      quota_limit: tk.quota_limit != null ? String(tk.quota_limit) : '',
      ip_limit: Array.isArray(tk.ip_limit) ? tk.ip_limit.join(', ') : (tk.ip_limit || ''),
      status: tk.status || (tk.is_active === false ? 'disabled' : 'active'),
    });
    setGeneratedKey(null);
    setShowModal(true);
  };

  const closeModal = () => {
    setShowModal(false);
    setEditing(null);
    setGeneratedKey(null);
  };

  const handleSave = async () => {
    if (!form.name.trim()) {
      addToast(t('密钥名称为必填项'), 'error');
      return;
    }
    setSaving(true);
    setError('');
    try {
      const allowedModels = form.allowed_models
        .split(',')
        .map((s) => s.trim())
        .filter(Boolean);
      const ipLimit = form.ip_limit
        .split(',')
        .map((s) => s.trim())
        .filter(Boolean);
      const payload = {
        name: form.name.trim(),
        group: form.group || 'default',
        allowed_models: allowedModels,
        ip_limit: ipLimit,
        status: form.status,
      };
      if (form.expires_at.trim()) payload.expires_at = Number(form.expires_at);
      if (form.quota_limit.trim()) payload.quota_limit = Number(form.quota_limit);

      if (editing) {
        await api.updateToken(editing.id, payload);
        addToast(t('令牌已更新'));
      } else {
        const res = await api.addToken(payload);
        const created = res.data || res;
        // 契约：创建响应额外返回 plain_key（一次性明文），优先展示明文而非脱敏密钥
        if (created && (created.plain_key || created.key || created.api_key)) {
          setGeneratedKey({ ...created, key: created.plain_key || created.key || created.api_key });
        }
        addToast(t('令牌创建成功'));
      }
      load();
      // 编辑保存后关闭弹窗；创建路径保持弹窗以展示一次性密钥
      if (editing) closeModal();
    } catch (err) {
      setError(err.message);
    } finally {
      setSaving(false);
    }
  };

  const handleDelete = async (id) => {
    setConfirmState({
      title: t('删除令牌'),
      message: t('确定删除此令牌？'),
      confirmText: t('删除'),
      danger: true,
      onConfirm: async () => {
        setError('');
        try {
          await api.deleteToken(id);
          addToast(t('令牌已删除'));
          load();
        } catch (err) {
          setError(err.message);
        }
      },
    });
  };

  const handleToggleStatus = async (tk) => {
    const newStatus = tk.status === 'disabled' ? 'active' : 'disabled';
    setError('');
    try {
      await api.updateToken(tk.id, { status: newStatus });
      addToast(newStatus === 'active' ? t('已启用') : t('已禁用'));
      load();
    } catch (err) {
      setError(err.message);
    }
  };

  const handleResetUsed = async (id) => {
    setConfirmState({
      title: t('重置已用'),
      message: t('确定重置已用额度为 0？'),
      confirmText: t('重置已用'),
      danger: false,
      onConfirm: async () => {
        setError('');
        try {
          await api.resetTokenUsed(id);
          addToast(t('已重置已用额度'));
          load();
        } catch (err) {
          setError(err.message);
        }
      },
    });
  };

  // 令牌轮换：生成新密钥，旧密钥立即失效；新密钥仅此一次展示
  const handleRotate = (tk) => {
    setConfirmState({
      title: t('轮换令牌密钥'),
      message: t('轮换将生成新密钥，旧密钥立即失效。确定继续？'),
      confirmText: t('轮换'),
      danger: true,
      onConfirm: async () => {
        setError('');
        try {
          const res = await api.rotateToken(tk.id);
          const data = res?.data || res || {};
          // 后端返回新密钥明文（plain_key / key / api_key）
          const newKey = data.plain_key || data.key || data.api_key;
          if (newKey) {
            setRotatedKey({ name: tk.name, key: newKey });
          }
          addToast(t('令牌已轮换，请立即保存新密钥'));
          load();
        } catch (err) {
          setError(err.message);
        }
      },
    });
  };

  const copyToClipboard = (text) => {
    navigator.clipboard.writeText(text).then(() => {
      addToast(t('已复制到剪贴板'));
    }).catch(() => {
      addToast(t('复制失败'), 'error');
    });
  };

  const fmtQuota = (q) => {
    const n = Number(q || 0);
    if (n >= 1_000_000) return (n / 1_000_000).toFixed(2) + 'M';
    if (n >= 1_000) return (n / 1_000).toFixed(2) + 'K';
    return String(n);
  };

  const isExpired = (tk) => {
    if (!tk.expires_at) return false;
    return Number(tk.expires_at) < Math.floor(Date.now() / 1000);
  };

  if (loading) return <div className="loading">{t('加载令牌列表')}</div>;

  return (
    <div>
      <div className="page-header">
        <h1>{t('API 令牌')}</h1>
        <p>{t('管理 API 令牌：分组、模型白名单、额度、过期与 IP 限制')}</p>
      </div>

      {error && <div className="error-message">{error}</div>}

      <div className="card">
        <div className="card-header">
          <h2>{t('所有令牌')} ({tokens.length})</h2>
          <button className="btn btn-primary" onClick={openCreate}>{t('+ 创建令牌')}</button>
        </div>
        <div className="card-body">
          {tokens.length === 0 ? (
            <div className="empty-state">
              <p>{t('暂无 API 令牌')}</p>
              <button className="btn btn-primary" onClick={openCreate}>{t('创建第一个令牌')}</button>
            </div>
          ) : (
            <div className="table-wrapper">
              <table>
                <thead>
                  <tr>
                    <th>{t('名称')}</th>
                    <th>{t('分组')}</th>
                    <th>{t('模型白名单')}</th>
                    <th>{t('额度')}</th>
                    <th>{t('过期')}</th>
                    <th>{t('状态')}</th>
                    <th>{t('创建时间')}</th>
                    <th>{t('操作')}</th>
                  </tr>
                </thead>
                <tbody>
                  {tokens.map((tk) => {
                    const expired = isExpired(tk);
                    const disabled = tk.status === 'disabled' || tk.is_active === false;
                    return (
                      <tr key={tk.id}>
                        <td><strong>{tk.name}</strong></td>
                        <td>{tk.group || 'default'}</td>
                        <td style={{ fontSize: 12, color: 'var(--text-muted)' }}>
                          {Array.isArray(tk.allowed_models) && tk.allowed_models.length > 0
                            ? tk.allowed_models.join(', ')
                            : (tk.allowed_models || t('全部'))}
                        </td>
                        <td style={{ fontSize: 12 }}>
                          {tk.quota_limit
                            ? `${fmtQuota(tk.used_quota)} / ${fmtQuota(tk.quota_limit)}`
                            : `${fmtQuota(tk.used_quota)} / ∞`}
                        </td>
                        <td style={{ fontSize: 12, color: expired ? 'rgb(239,68,68)' : 'var(--text-muted)' }}>
                          {tk.expires_at
                            ? new Date(tk.expires_at * 1000).toLocaleDateString()
                            : t('永不过期')}
                          {expired && t('(已过期)')}
                        </td>
                        <td>
                          <span className={disabled ? 'badge badge-danger' : 'badge badge-success'}>
                            {disabled ? t('禁用') : t('启用')}
                          </span>
                        </td>
                        <td style={{ fontSize: 12, color: 'var(--text-muted)' }}>
                          {tk.created_at
                            ? new Date(typeof tk.created_at === 'number' && tk.created_at > 1e12 ? tk.created_at : tk.created_at * 1000).toLocaleDateString()
                            : '—'}
                        </td>
                        <td>
                          <div className="actions-cell">
                            <button className="btn btn-outline btn-sm" onClick={() => openEdit(tk)}>{t('编辑')}</button>
                            <button className="btn btn-outline btn-sm" onClick={() => handleToggleStatus(tk)}>
                              {disabled ? t('启用') : t('禁用')}
                            </button>
                            {tk.quota_limit && (
                              <button className="btn btn-outline btn-sm" onClick={() => handleResetUsed(tk.id)}>
                                {t('重置已用')}
                              </button>
                            )}
                            {/* 令牌轮换：生成新密钥，旧密钥立即失效 */}
                            <button className="btn btn-outline btn-sm" onClick={() => handleRotate(tk)}>
                              {t('轮换')}
                            </button>
                            <button className="btn btn-danger btn-sm" onClick={() => handleDelete(tk.id)}>{t('删除')}</button>
                          </div>
                        </td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            </div>
          )}
        </div>
      </div>

      <ConfirmDialog state={confirmState} onClose={() => setConfirmState(null)} />

      {/* 令牌轮换后新密钥展示模态框：新密钥仅此一次显示 */}
      {rotatedKey && (
        <div className="modal-overlay" onClick={() => setRotatedKey(null)}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <div className="modal-header">
              <h3>{t('令牌已轮换')} — {rotatedKey.name}</h3>
              <button className="modal-close" onClick={() => setRotatedKey(null)}>&times;</button>
            </div>
            <div className="modal-body">
              <div className="success-message">{t('轮换成功！请立即保存新密钥，此为最后一次显示。')}</div>
              <div className="form-group">
                <label>{t('新 API 密钥')}</label>
                <div className="generated-key-box">
                  <code className="generated-key">{rotatedKey.key}</code>
                </div>
                <p className="key-warning">{t('旧密钥已立即失效，请将新密钥保存到安全位置，关闭后将无法再次查看。')}</p>
              </div>
              <button
                className="btn btn-primary"
                onClick={() => copyToClipboard(rotatedKey.key)}
                style={{ width: '100%' }}
              >
                {t('复制到剪贴板')}
              </button>
            </div>
            <div className="modal-footer">
              <button className="btn btn-primary" onClick={() => setRotatedKey(null)}>{t('我已保存')}</button>
            </div>
          </div>
        </div>
      )}

      {showModal && (
        <div className="modal-overlay" onClick={closeModal}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <div className="modal-header">
              <h3>{editing ? t('编辑令牌') : t('创建 API 令牌')}</h3>
              <button className="modal-close" onClick={closeModal}>&times;</button>
            </div>
            <div className="modal-body">
              {generatedKey && !editing ? (
                <div>
                  <div className="success-message">{t('令牌创建成功！')}</div>
                  <div className="form-group">
                    <label>{t('您的 API 密钥')}</label>
                    <div className="generated-key-box">
                      <code className="generated-key">{generatedKey.key || generatedKey.api_key}</code>
                    </div>
                    <p className="key-warning">{t('请立即复制此密钥，关闭后将无法再次查看。')}</p>
                  </div>
                  <button className="btn btn-primary" onClick={() => copyToClipboard(generatedKey.key || generatedKey.api_key)} style={{ width: '100%' }}>
                    {t('复制到剪贴板')}
                  </button>
                </div>
              ) : (
                <>
                  <div className="form-group">
                    <label>{t('名称 *')}</label>
                    <input className="form-input" placeholder={t('例如：开发环境令牌')}
                      value={form.name}
                      onChange={(e) => setForm({ ...form, name: e.target.value })}
                      autoFocus />
                  </div>
                  <div className="form-group">
                    <label>{t('分组')}</label>
                    <select className="form-input" value={form.group}
                      onChange={(e) => setForm({ ...form, group: e.target.value })}>
                      <option value="default">default</option>
                      {groups.filter((g) => g.name && g.name !== 'default').map((g) => (
                        <option key={g.name} value={g.name}>{g.name}</option>
                      ))}
                    </select>
                  </div>
                  <div className="form-group">
                    <label>{t('模型白名单')} <span style={{ color: 'var(--text-muted)' }}>{t('(逗号分隔，留空则允许全部)')}</span></label>
                    <input className="form-input" placeholder={editing ? t('留空表示不修改') : "glm-5.2, deepseek-v3, kimi-k2.6"}
                      value={form.allowed_models}
                      onChange={(e) => setForm({ ...form, allowed_models: e.target.value })} />
                  </div>
                  <div className="form-group">
                    <label>{t('过期时间')} <span style={{ color: 'var(--text-muted)' }}>{t('(Unix 时间戳，留空则永不过期)')}</span></label>
                    <input className="form-input" type="number" placeholder={editing ? t('留空表示不修改') : t('keysPlaceholderExpiresAt')}
                      value={form.expires_at}
                      onChange={(e) => setForm({ ...form, expires_at: e.target.value })} />
                  </div>
                  <div className="form-group">
                    <label>{t('额度上限')} <span style={{ color: 'var(--text-muted)' }}>{t('(留空则无上限)')}</span></label>
                    <input className="form-input" type="number" placeholder={editing ? t('留空表示不修改') : t('keysPlaceholderQuotaLimit')}
                      value={form.quota_limit}
                      onChange={(e) => setForm({ ...form, quota_limit: e.target.value })} />
                  </div>
                  <div className="form-group">
                    <label>{t('IP 限制')} <span style={{ color: 'var(--text-muted)' }}>{t('(逗号分隔，留空则不限制)')}</span></label>
                    <input className="form-input" placeholder={editing ? t('留空表示不修改') : t('keysPlaceholderIpLimit')}
                      value={form.ip_limit}
                      onChange={(e) => setForm({ ...form, ip_limit: e.target.value })} />
                  </div>
                  <div className="form-group">
                    <label>{t('状态')}</label>
                    <select className="form-input" value={form.status}
                      onChange={(e) => setForm({ ...form, status: e.target.value })}>
                      <option value="active">{t('启用')}</option>
                      <option value="disabled">{t('禁用')}</option>
                    </select>
                  </div>
                </>
              )}
            </div>
            <div className="modal-footer">
              {generatedKey && !editing ? (
                <button className="btn btn-primary" onClick={closeModal}>{t('完成')}</button>
              ) : (
                <>
                  <button className="btn btn-outline" onClick={closeModal}>{t('取消')}</button>
                  <button className="btn btn-primary" onClick={handleSave} disabled={saving}>
                    {saving ? t('保存中...') : (editing ? t('保存') : t('创建'))}
                  </button>
                </>
              )}
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
