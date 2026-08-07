import React, { useState, useEffect } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api';
import { useToast } from '../components/Toast';
import './Keys.css';

export default function Users() {
  const [users, setUsers] = useState([]);
  const [groups, setGroups] = useState([]);
  const [me, setMe] = useState(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  const addToast = useToast();
  const { t } = useTranslation();

  const [showModal, setShowModal] = useState(false);
  const [editing, setEditing] = useState(null);
  const [form, setForm] = useState({ email: '', username: '', password: '', role: 'user', quota: 0, status: 'active', group: 'default' });
  const [saving, setSaving] = useState(false);

  // ── 用户分组管理状态 ──
  const [showGroupModal, setShowGroupModal] = useState(false);
  const [groupForm, setGroupForm] = useState({ name: '', ratio: '1', allowed_models: '', description: '' });
  const [savingGroup, setSavingGroup] = useState(false);

  useEffect(() => {
    load();
  }, []);

  const load = async () => {
    setLoading(true);
    setError('');
    try {
      const [listRes, meRes, groupRes] = await Promise.all([
        api.listUsers(),
        api.getMe().catch(() => null),
        api.listGroups().catch(() => null),
      ]);
      setUsers(listRes.data || []);
      if (meRes) setMe(meRes.data || null);
      if (groupRes) setGroups(groupRes.data || groupRes || []);
    } catch (err) {
      setError(err.message);
    } finally {
      setLoading(false);
    }
  };

  const openCreate = () => {
    setEditing(null);
    setForm({ email: '', username: '', password: '', role: 'user', quota: 0, status: 'active', group: 'default' });
    setShowModal(true);
  };

  const openEdit = (u) => {
    setEditing(u);
    setForm({ email: u.email || '', username: u.username || '', password: '', role: u.role || 'user', quota: u.quota || 0, status: u.status || 'active', group: u.group || 'default' });
    setShowModal(true);
  };

  const closeModal = () => {
    setShowModal(false);
    setEditing(null);
  };

  const isValidEmail = (email) => /^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(email);

  const handleSave = async () => {
    if (!form.email.trim()) {
      setError(t('邮箱为必填项'));
      return;
    }
    if (!isValidEmail(form.email.trim())) {
      setError(t('邮箱格式不正确'));
      return;
    }
    if (!editing && !form.password) {
      setError(t('密码为必填项'));
      return;
    }
    setSaving(true);
    setError('');
    try {
      if (editing) {
        const payload = { role: form.role, quota: Number(form.quota), status: form.status, group: form.group };
        if (form.email.trim() !== editing.email) payload.email = form.email.trim();
        if (form.username.trim()) payload.username = form.username.trim();
        if (form.password) payload.password = form.password;
        await api.updateUser(editing.id, payload);
        addToast(t('用户已更新'));
      } else {
        await api.createUser({
          email: form.email.trim(),
          username: form.username.trim() || undefined,
          password: form.password,
          role: form.role,
          quota: Number(form.quota),
          group: form.group,
        });
        addToast(t('用户已创建'));
      }
      closeModal();
      load();
    } catch (err) {
      setError(err.message);
    } finally {
      setSaving(false);
    }
  };

  const handleDelete = async (id) => {
    if (!window.confirm(t('确定删除该用户？'))) return;
    setError('');
    try {
      await api.deleteUser(id);
      addToast(t('用户已删除'));
      load();
    } catch (err) {
      setError(err.message);
    }
  };

  // ── 用户分组管理 ──
  const openGroupCreate = () => {
    setGroupForm({ name: '', ratio: '1', allowed_models: '', description: '' });
    setShowGroupModal(true);
  };

  const openGroupEdit = (g) => {
    setGroupForm({
      name: g.name || '',
      ratio: String(g.ratio != null ? g.ratio : 1),
      allowed_models: Array.isArray(g.allowed_models) ? g.allowed_models.join(', ') : (g.allowed_models || ''),
      description: g.description || '',
    });
    setShowGroupModal(true);
  };

  const closeGroupModal = () => setShowGroupModal(false);

  const handleSaveGroup = async () => {
    if (!groupForm.name.trim()) {
      setError(t('分组名称为必填项'));
      return;
    }
    setSavingGroup(true);
    setError('');
    try {
      const allowedModels = groupForm.allowed_models
        .split(',')
        .map((s) => s.trim())
        .filter(Boolean);
      const payload = {
        name: groupForm.name.trim(),
        ratio: Number(groupForm.ratio) || 1,
        allowed_models: allowedModels,
        description: groupForm.description || '',
      };
      await api.upsertGroup(payload);
      addToast(t('用户分组已保存'));
      closeGroupModal();
      load();
    } catch (err) {
      setError(err.message);
    } finally {
      setSavingGroup(false);
    }
  };

  const handleDeleteGroup = async (name) => {
    if (name === 'default') {
      setError(t('不能删除默认分组'));
      return;
    }
    if (!window.confirm(`${t('确定删除分组')} ${name}?`)) return;
    setError('');
    try {
      await api.deleteGroup(name);
      addToast(t('用户分组已删除'));
      load();
    } catch (err) {
      setError(err.message);
    }
  };

  const fmtQuota = (q) => {
    const n = Number(q || 0);
    if (n >= 1_000_000) return (n / 1_000_000).toFixed(2) + 'M';
    if (n >= 1_000) return (n / 1_000).toFixed(2) + 'K';
    return String(n);
  };

  if (loading) return <div className="loading">{t('加载用户列表')}</div>;

  return (
    <div>
      <div className="page-header">
        <h1>{t('用户管理')}</h1>
        <p>{t('管理系统用户、角色与配额')}</p>
      </div>

      {error && <div className="error-message">{error}</div>}

      {me && (
        <div className="card" style={{ marginBottom: 16 }}>
          <div className="card-body" style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', flexWrap: 'wrap', gap: 12 }}>
            <div>
              <div style={{ fontSize: 13, color: 'var(--text-muted)' }}>{t('当前登录')}</div>
              <div style={{ fontSize: 18, fontWeight: 600 }}>{me.email}</div>
              {me.username && <div style={{ fontSize: 13, color: 'var(--text-muted)' }}>@{me.username}</div>}
            </div>
            <div style={{ display: 'flex', gap: 24, flexWrap: 'wrap' }}>
              <div>
                <div style={{ fontSize: 12, color: 'var(--text-muted)' }}>{t('角色')}</div>
                <div style={{ fontWeight: 600 }}>{me.role === 'admin' ? t('管理员') : t('普通用户')}</div>
              </div>
              <div>
                <div style={{ fontSize: 12, color: 'var(--text-muted)' }}>{t('剩余配额')}</div>
                <div style={{ fontWeight: 600 }}>{fmtQuota((me.quota || 0) - (me.used_quota || 0))}</div>
              </div>
            </div>
          </div>
        </div>
      )}

      {/* ── 用户分组管理 ── */}
      <div className="card" style={{ marginBottom: 16 }}>
        <div className="card-header">
          <h2>{t('用户分组')} ({groups.length})</h2>
          <button className="btn btn-primary" onClick={openGroupCreate}>{t('+ 新建分组')}</button>
        </div>
        <div className="card-body">
          {groups.length === 0 ? (
            <div className="empty-state"><p>{t('暂无自定义分组，使用默认分组 (倍率 1.0)')}</p></div>
          ) : (
            <div className="table-wrapper">
              <table>
                <thead>
                  <tr>
                    <th>{t('分组名称')}</th>
                    <th>{t('倍率')}</th>
                    <th>{t('允许模型')}</th>
                    <th>{t('描述')}</th>
                    <th>{t('操作')}</th>
                  </tr>
                </thead>
                <tbody>
                  {groups.map((g) => (
                    <tr key={g.name}>
                      <td><strong>{g.name}</strong></td>
                      <td>{g.ratio != null ? g.ratio : 1}</td>
                      <td style={{ fontSize: 12, color: 'var(--text-muted)' }}>
                        {Array.isArray(g.allowed_models) && g.allowed_models.length > 0
                          ? g.allowed_models.join(', ')
                          : t('全部')}
                      </td>
                      <td style={{ fontSize: 12, color: 'var(--text-muted)' }}>{g.description || '—'}</td>
                      <td>
                        <div className="actions-cell">
                          <button className="btn btn-outline btn-sm" onClick={() => openGroupEdit(g)}>{t('编辑')}</button>
                          {g.name !== 'default' && (
                            <button className="btn btn-danger btn-sm" onClick={() => handleDeleteGroup(g.name)}>{t('删除')}</button>
                          )}
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

      <div className="card">
        <div className="card-header">
          <h2>{t('所有用户')} ({users.length})</h2>
          <button className="btn btn-primary" onClick={openCreate}>{t('+ 新建用户')}</button>
        </div>
        <div className="card-body">
          {users.length === 0 ? (
            <div className="empty-state"><p>{t('暂无用户')}</p></div>
          ) : (
            <div className="table-wrapper">
              <table>
                <thead>
                  <tr>
                    <th>{t('邮箱')}</th>
                    <th>{t('昵称')}</th>
                    <th>{t('角色')}</th>
                    <th>{t('分组')}</th>
                    <th>{t('总配额')}</th>
                    <th>{t('已用')}</th>
                    <th>{t('剩余配额')}</th>
                    <th>{t('状态')}</th>
                    <th>{t('创建时间')}</th>
                    <th>{t('操作')}</th>
                  </tr>
                </thead>
                <tbody>
                  {users.map((u) => (
                    <tr key={u.id}>
                      <td><strong>{u.email}</strong></td>
                      <td>{u.username || '—'}</td>
                      <td>{u.role === 'admin' ? t('管理员') : t('普通用户')}</td>
                      <td>{u.group || 'default'}</td>
                      <td>{fmtQuota(u.quota)}</td>
                      <td>{fmtQuota(u.used_quota)}</td>
                      <td>{fmtQuota((u.quota || 0) - (u.used_quota || 0))}</td>
                      <td>
                        <span style={{
                          padding: '2px 10px', borderRadius: 999, fontSize: 12,
                          background: u.status === 'active' ? 'rgba(34,197,94,0.15)' : 'rgba(239,68,68,0.15)',
                          color: u.status === 'active' ? 'rgb(34,197,94)' : 'rgb(239,68,68)',
                        }}>
                          {u.status === 'active' ? t('启用') : t('禁用')}
                        </span>
                      </td>
                      <td>{u.created_at ? new Date(u.created_at * 1000).toLocaleString() : '—'}</td>
                      <td>
                        <div className="actions-cell">
                          <button className="btn btn-outline btn-sm" onClick={() => openEdit(u)}>{t('编辑')}</button>
                          <button className="btn btn-danger btn-sm" onClick={() => handleDelete(u.id)}>{t('删除')}</button>
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

      {showModal && (
        <div className="modal-overlay" onClick={closeModal}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <div className="modal-header">
              <h3>{editing ? t('编辑用户') : t('新建用户')}</h3>
              <button className="modal-close" onClick={closeModal}>&times;</button>
            </div>
            <div className="modal-body">
              <div className="form-group">
                <label>{t('邮箱')} *</label>
                <input className="form-input" placeholder="user@example.com"
                  value={form.email}
                  onChange={(e) => setForm({ ...form, email: e.target.value })}
                  autoFocus />
              </div>
              <div className="form-group">
                <label>{t('昵称')} <span style={{ color: 'var(--text-muted)' }}>{t('(可选)')}</span></label>
                <input className="form-input" placeholder={t('显示名称')}
                  value={form.username}
                  onChange={(e) => setForm({ ...form, username: e.target.value })} />
              </div>
              <div className="form-group">
                <label>{t('密码')} {editing ? <span style={{ color: 'var(--text-muted)' }}>{t('(留空则不修改)')}</span> : <span style={{ color: 'var(--text-danger)' }}>*</span>}</label>
                <input className="form-input" type="password"
                  value={form.password}
                  onChange={(e) => setForm({ ...form, password: e.target.value })} />
              </div>
              <div className="form-group">
                <label>{t('角色')}</label>
                <select className="form-input" value={form.role}
                  onChange={(e) => setForm({ ...form, role: e.target.value })}>
                  <option value="user">{t('普通用户')}</option>
                  <option value="admin">{t('管理员')}</option>
                </select>
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
                <label>{t('配额')}</label>
                <input className="form-input" type="number"
                  value={form.quota}
                  onChange={(e) => setForm({ ...form, quota: e.target.value })} />
              </div>
              {editing && (
                <div className="form-group">
                  <label>{t('状态')}</label>
                  <select className="form-input" value={form.status}
                    onChange={(e) => setForm({ ...form, status: e.target.value })}>
                    <option value="active">{t('启用')}</option>
                    <option value="disabled">{t('禁用')}</option>
                  </select>
                </div>
              )}
            </div>
            <div className="modal-footer">
              <button className="btn btn-outline" onClick={closeModal}>{t('取消')}</button>
              <button className="btn btn-primary" onClick={handleSave} disabled={saving}>
                {saving ? t('保存中...') : t('保存')}
              </button>
            </div>
          </div>
        </div>
      )}

      {/* ── 用户分组弹窗 ── */}
      {showGroupModal && (
        <div className="modal-overlay" onClick={closeGroupModal}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <div className="modal-header">
              <h3>{t('用户分组')}</h3>
              <button className="modal-close" onClick={closeGroupModal}>&times;</button>
            </div>
            <div className="modal-body">
              <div className="form-group">
                <label>{t('分组名称')} *</label>
                <input className="form-input" placeholder={t('例如：vip')}
                  value={groupForm.name}
                  onChange={(e) => setGroupForm({ ...groupForm, name: e.target.value })}
                  autoFocus />
              </div>
              <div className="form-group">
                <label>{t('计费倍率')}</label>
                <input className="form-input" type="number" step="0.01" placeholder="1.0"
                  value={groupForm.ratio}
                  onChange={(e) => setGroupForm({ ...groupForm, ratio: e.target.value })} />
              </div>
              <div className="form-group">
                <label>{t('允许模型')} <span style={{ color: 'var(--text-muted)' }}>{t('(逗号分隔，留空则允许全部)')}</span></label>
                <input className="form-input" placeholder="glm-5.2, deepseek-v3"
                  value={groupForm.allowed_models}
                  onChange={(e) => setGroupForm({ ...groupForm, allowed_models: e.target.value })} />
              </div>
              <div className="form-group">
                <label>{t('描述')} <span style={{ color: 'var(--text-muted)' }}>{t('(可选)')}</span></label>
                <input className="form-input" placeholder={t('分组描述')}
                  value={groupForm.description}
                  onChange={(e) => setGroupForm({ ...groupForm, description: e.target.value })} />
              </div>
            </div>
            <div className="modal-footer">
              <button className="btn btn-outline" onClick={closeGroupModal}>{t('取消')}</button>
              <button className="btn btn-primary" onClick={handleSaveGroup} disabled={savingGroup}>
                {savingGroup ? t('保存中...') : t('保存')}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
