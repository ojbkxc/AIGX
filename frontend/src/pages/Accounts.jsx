import React, { useState, useEffect } from 'react';
import { api } from '../api';
import { useToast } from '../components/Toast';
import './Accounts.css';

export default function Accounts() {
  const [accounts, setAccounts] = useState([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  const addToast = useToast();

  const [showModal, setShowModal] = useState(false);
  const [editAccount, setEditAccount] = useState(null);
  const [form, setForm] = useState({ name: '', account_id: '', api_token: '' });
  const [saving, setSaving] = useState(false);
  const [testResult, setTestResult] = useState(null);

  useEffect(() => {
    loadAccounts();
  }, []);

  const loadAccounts = async () => {
    setLoading(true);
    setError('');
    try {
      const res = await api.listAccounts();
      setAccounts(res.data || res || []);
    } catch (err) {
      setError(err.message);
    } finally {
      setLoading(false);
    }
  };

  const openAdd = () => {
    setEditAccount(null);
    setForm({ name: '', account_id: '', api_token: '' });
    setTestResult(null);
    setShowModal(true);
  };

  const openEdit = (account) => {
    setEditAccount(account);
    setForm({ name: account.name || '', account_id: account.account_id || '', api_token: '' });
    setTestResult(null);
    setShowModal(true);
  };

  const closeModal = () => {
    setShowModal(false);
    setEditAccount(null);
    setForm({ name: '', account_id: '', api_token: '' });
    setTestResult(null);
  };

  const handleSave = async () => {
    if (!form.name || !form.account_id) {
      setError('名称和账号 ID 为必填项');
      return;
    }
    setSaving(true);
    setError('');
    try {
      if (editAccount) {
        const payload = { name: form.name, account_id: form.account_id };
        if (form.api_token) payload.api_token = form.api_token;
        await api.updateAccount(editAccount.id, payload);
        addToast('账号更新成功');
      } else {
        if (!form.api_token) {
          setError('新账号必填 API Token');
          setSaving(false);
          return;
        }
        await api.addAccount(form.name, form.account_id, form.api_token);
        addToast('账号添加成功');
      }
      closeModal();
      loadAccounts();
    } catch (err) {
      setError(err.message);
    } finally {
      setSaving(false);
    }
  };

  const handleTest = async () => {
    if (!form.name || !form.account_id || !form.api_token) {
      setError('测试连接需要填写名称、账号 ID 和 API Token');
      return;
    }
    setSaving(true);
    setError('');
    setTestResult(null);
    try {
      const res = await api.testAccount(form.name, form.account_id, form.api_token);
      setTestResult({ success: true, message: res.message || '连接成功！' });
    } catch (err) {
      setTestResult({ success: false, message: err.message });
    } finally {
      setSaving(false);
    }
  };

  const handleDelete = async (id) => {
    if (!window.confirm('确定删除此账号？')) return;
    setError('');
    try {
      await api.deleteAccount(id);
      addToast('账号已删除');
      loadAccounts();
    } catch (err) {
      setError(err.message);
    }
  };

  if (loading) return <div className="loading">加载账号列表</div>;

  return (
    <div>
      <div className="page-header">
        <h1>账号管理</h1>
        <p>管理 Cloudflare AI 网关账号</p>
      </div>

      {error && <div className="error-message">{error}</div>}

      <div className="card">
        <div className="card-header">
          <h2>所有账号 ({accounts.length})</h2>
          <button className="btn btn-primary" onClick={openAdd}>+ 添加账号</button>
        </div>
        <div className="card-body">
          {accounts.length === 0 ? (
            <div className="empty-state">
              <p>暂无账号</p>
              <button className="btn btn-primary" onClick={openAdd}>添加第一个账号</button>
            </div>
          ) : (
            <div className="table-wrapper">
              <table>
                <thead>
                  <tr>
                    <th>名称</th>
                    <th>账号 ID</th>
                    <th>状态</th>
                    <th>操作</th>
                  </tr>
                </thead>
                <tbody>
                  {accounts.map((acc) => (
                    <tr key={acc.id}>
                      <td><strong>{acc.name}</strong></td>
                      <td><code className="account-id">{acc.account_id}</code></td>
                      <td>
                        {acc.status === 'active' ? (
                          <span className="badge badge-success">正常</span>
                        ) : (
                          <span className="badge badge-warning">{acc.status || '未知'}</span>
                        )}
                      </td>
                      <td>
                        <div className="actions-cell">
                          <button className="btn btn-outline btn-sm" onClick={() => openEdit(acc)}>编辑</button>
                          <button className="btn btn-danger btn-sm" onClick={() => handleDelete(acc.id)}>删除</button>
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
              <h3>{editAccount ? '编辑账号' : '添加账号'}</h3>
              <button className="modal-close" onClick={closeModal}>&times;</button>
            </div>
            <div className="modal-body">
              {testResult && (
                <div className={testResult.success ? 'success-message' : 'error-message'}>
                  {testResult.message}
                </div>
              )}
              <div className="form-group">
                <label>名称</label>
                <input className="form-input" placeholder="例如：我的账号" value={form.name} onChange={(e) => setForm({ ...form, name: e.target.value })} />
              </div>
              <div className="form-group">
                <label>账号 ID</label>
                <input className="form-input" placeholder="Cloudflare 账号 ID" value={form.account_id} onChange={(e) => setForm({ ...form, account_id: e.target.value })} />
              </div>
              <div className="form-group">
                <label>API Token {editAccount && '（留空则保持不变）'}</label>
                <input className="form-input" type="password" placeholder={editAccount ? '留空保持当前值' : 'API Token'} value={form.api_token} onChange={(e) => setForm({ ...form, api_token: e.target.value })} />
              </div>
            </div>
            <div className="modal-footer">
              <button className="btn btn-outline" onClick={handleTest} disabled={saving}>
                {saving ? '测试中...' : '测试连接'}
              </button>
              <button className="btn btn-outline" onClick={closeModal}>取消</button>
              <button className="btn btn-primary" onClick={handleSave} disabled={saving}>
                {saving ? '保存中...' : (editAccount ? '更新' : '添加')}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}