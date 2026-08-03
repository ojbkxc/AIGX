import React, { useState, useEffect } from 'react';
import { api } from '../api';
import { useToast } from '../components/Toast';
import './Keys.css';

export default function Users() {
  const [users, setUsers] = useState([]);
  const [me, setMe] = useState(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  const addToast = useToast();

  const [showModal, setShowModal] = useState(false);
  const [editing, setEditing] = useState(null);
  const [form, setForm] = useState({ username: '', password: '', role: 'user', quota: 0, status: 'active' });
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    load();
  }, []);

  const load = async () => {
    setLoading(true);
    setError('');
    try {
      const [listRes, meRes] = await Promise.all([api.listUsers(), api.getMe().catch(() => null)]);
      setUsers(listRes.data || []);
      if (meRes) setMe(meRes.data || null);
    } catch (err) {
      setError(err.message);
    } finally {
      setLoading(false);
    }
  };

  const openCreate = () => {
    setEditing(null);
    setForm({ username: '', password: '', role: 'user', quota: 0, status: 'active' });
    setShowModal(true);
  };

  const openEdit = (u) => {
    setEditing(u);
    setForm({ username: u.username, password: '', role: u.role || 'user', quota: u.quota || 0, status: u.status || 'active' });
    setShowModal(true);
  };

  const closeModal = () => {
    setShowModal(false);
    setEditing(null);
  };

  const handleSave = async () => {
    if (!form.username.trim()) {
      setError('用户名为必填项');
      return;
    }
    if (!editing && !form.password) {
      setError('密码为必填项');
      return;
    }
    setSaving(true);
    setError('');
    try {
      if (editing) {
        const payload = { role: form.role, quota: Number(form.quota), status: form.status };
        if (form.password) payload.password = form.password;
        await api.updateUser(editing.id, payload);
        addToast('用户已更新');
      } else {
        await api.createUser({
          username: form.username.trim(),
          password: form.password,
          role: form.role,
          quota: Number(form.quota),
        });
        addToast('用户已创建');
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
    if (!window.confirm('确定删除该用户？')) return;
    setError('');
    try {
      await api.deleteUser(id);
      addToast('用户已删除');
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

  if (loading) return <div className="loading">加载用户列表</div>;

  return (
    <div>
      <div className="page-header">
        <h1>用户管理</h1>
        <p>管理系统用户、角色与配额</p>
      </div>

      {error && <div className="error-message">{error}</div>}

      {me && (
        <div className="card" style={{ marginBottom: 16 }}>
          <div className="card-body" style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', flexWrap: 'wrap', gap: 12 }}>
            <div>
              <div style={{ fontSize: 13, color: 'var(--text-muted)' }}>当前登录</div>
              <div style={{ fontSize: 18, fontWeight: 600 }}>{me.username}</div>
            </div>
            <div style={{ display: 'flex', gap: 24, flexWrap: 'wrap' }}>
              <div>
                <div style={{ fontSize: 12, color: 'var(--text-muted)' }}>角色</div>
                <div style={{ fontWeight: 600 }}>{me.role === 'admin' ? '管理员' : '普通用户'}</div>
              </div>
              <div>
                <div style={{ fontSize: 12, color: 'var(--text-muted)' }}>剩余配额</div>
                <div style={{ fontWeight: 600 }}>{fmtQuota((me.quota || 0) - (me.used_quota || 0))}</div>
              </div>
            </div>
          </div>
        </div>
      )}

      <div className="card">
        <div className="card-header">
          <h2>所有用户 ({users.length})</h2>
          <button className="btn btn-primary" onClick={openCreate}>+ 新建用户</button>
        </div>
        <div className="card-body">
          {users.length === 0 ? (
            <div className="empty-state"><p>暂无用户</p></div>
          ) : (
            <div className="table-wrapper">
              <table>
                <thead>
                  <tr>
                    <th>用户名</th>
                    <th>角色</th>
                    <th>总配额</th>
                    <th>已用</th>
                    <th>剩余</th>
                    <th>状态</th>
                    <th>创建时间</th>
                    <th>操作</th>
                  </tr>
                </thead>
                <tbody>
                  {users.map((u) => (
                    <tr key={u.id}>
                      <td><strong>{u.username}</strong></td>
                      <td>{u.role === 'admin' ? '管理员' : '普通用户'}</td>
                      <td>{fmtQuota(u.quota)}</td>
                      <td>{fmtQuota(u.used_quota)}</td>
                      <td>{fmtQuota((u.quota || 0) - (u.used_quota || 0))}</td>
                      <td>
                        <span style={{
                          padding: '2px 10px', borderRadius: 999, fontSize: 12,
                          background: u.status === 'active' ? 'rgba(34,197,94,0.15)' : 'rgba(239,68,68,0.15)',
                          color: u.status === 'active' ? 'rgb(34,197,94)' : 'rgb(239,68,68)',
                        }}>
                          {u.status === 'active' ? '启用' : '禁用'}
                        </span>
                      </td>
                      <td>{u.created_at ? new Date(u.created_at * 1000).toLocaleString() : '—'}</td>
                      <td>
                        <div className="actions-cell">
                          <button className="btn btn-outline btn-sm" onClick={() => openEdit(u)}>编辑</button>
                          <button className="btn btn-danger btn-sm" onClick={() => handleDelete(u.id)}>删除</button>
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
              <h3>{editing ? '编辑用户' : '新建用户'}</h3>
              <button className="modal-close" onClick={closeModal}>&times;</button>
            </div>
            <div className="modal-body">
              <div className="form-group">
                <label>用户名</label>
                <input className="form-input" disabled={!!editing}
                  value={form.username}
                  onChange={(e) => setForm({ ...form, username: e.target.value })}
                  autoFocus />
              </div>
              <div className="form-group">
                <label>密码 {editing && <span style={{ color: 'var(--text-muted)' }}>(留空则不修改)</span>}</label>
                <input className="form-input" type="password"
                  value={form.password}
                  onChange={(e) => setForm({ ...form, password: e.target.value })} />
              </div>
              <div className="form-group">
                <label>角色</label>
                <select className="form-input" value={form.role}
                  onChange={(e) => setForm({ ...form, role: e.target.value })}>
                  <option value="user">普通用户</option>
                  <option value="admin">管理员</option>
                </select>
              </div>
              <div className="form-group">
                <label>配额（整数）</label>
                <input className="form-input" type="number"
                  value={form.quota}
                  onChange={(e) => setForm({ ...form, quota: e.target.value })} />
              </div>
              {editing && (
                <div className="form-group">
                  <label>状态</label>
                  <select className="form-input" value={form.status}
                    onChange={(e) => setForm({ ...form, status: e.target.value })}>
                    <option value="active">启用</option>
                    <option value="disabled">禁用</option>
                  </select>
                </div>
              )}
            </div>
            <div className="modal-footer">
              <button className="btn btn-outline" onClick={closeModal}>取消</button>
              <button className="btn btn-primary" onClick={handleSave} disabled={saving}>
                {saving ? '保存中...' : '保存'}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
