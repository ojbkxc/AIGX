import React, { useState, useEffect } from 'react';
import { api } from '../api';
import { useToast } from '../components/Toast';
import './Keys.css';

export default function Keys() {
  const [keys, setKeys] = useState([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  const addToast = useToast();

  const [showModal, setShowModal] = useState(false);
  const [keyName, setKeyName] = useState('');
  const [generating, setGenerating] = useState(false);
  const [generatedKey, setGeneratedKey] = useState(null);

  useEffect(() => {
    loadKeys();
  }, []);

  const loadKeys = async () => {
    setLoading(true);
    setError('');
    try {
      const res = await api.listKeys();
      setKeys(res.data || res || []);
    } catch (err) {
      setError(err.message);
    } finally {
      setLoading(false);
    }
  };

  const openGenerate = () => {
    setKeyName('');
    setGeneratedKey(null);
    setShowModal(true);
  };

  const closeModal = () => {
    setShowModal(false);
    setKeyName('');
    setGeneratedKey(null);
  };

  const handleGenerate = async () => {
    if (!keyName.trim()) {
      setError('密钥名称为必填项');
      return;
    }
    setGenerating(true);
    setError('');
    try {
      const res = await api.generateKey(keyName.trim());
      setGeneratedKey(res.data || res);
      addToast('API 密钥生成成功');
      loadKeys();
    } catch (err) {
      setError(err.message);
    } finally {
      setGenerating(false);
    }
  };

  const handleDelete = async (id) => {
    if (!window.confirm('确定删除此 API 密钥？')) return;
    setError('');
    try {
      await api.deleteKey(id);
      addToast('API 密钥已删除');
      loadKeys();
    } catch (err) {
      setError(err.message);
    }
  };

  const copyToClipboard = (text) => {
    navigator.clipboard.writeText(text).then(() => {
      addToast('已复制到剪贴板');
    }).catch(() => {
      setError('复制失败');
    });
  };

  if (loading) return <div className="loading">加载密钥列表</div>;

  return (
    <div>
      <div className="page-header">
        <h1>API 密钥</h1>
        <p>管理 API 密钥以进行程序化访问</p>
      </div>

      {error && <div className="error-message">{error}</div>}

      <div className="card">
        <div className="card-header">
          <h2>所有密钥 ({keys.length})</h2>
          <button className="btn btn-primary" onClick={openGenerate}>+ 生成密钥</button>
        </div>
        <div className="card-body">
          {keys.length === 0 ? (
            <div className="empty-state">
              <p>暂无 API 密钥</p>
              <button className="btn btn-primary" onClick={openGenerate}>生成第一个密钥</button>
            </div>
          ) : (
            <div className="table-wrapper">
              <table>
                <thead>
                  <tr>
                    <th>名称</th>
                    <th>密钥</th>
                    <th>创建时间</th>
                    <th>最后使用</th>
                    <th>操作</th>
                  </tr>
                </thead>
                <tbody>
                  {keys.map((key) => (
                    <tr key={key.id}>
                      <td><strong>{key.name}</strong></td>
                      <td>
                        <code className="key-value">{key.key || key.api_key || '—'}</code>
                      </td>
                      <td>
                        {key.created_at
                          ? new Date(key.created_at).toLocaleDateString()
                          : '—'}
                      </td>
                      <td style={{ fontSize: 13, color: 'var(--text-muted)' }}>
                        {key.last_used_at
                          ? new Date(key.last_used_at * 1000).toLocaleString()
                          : '—'}
                      </td>
                      <td>
                        <div className="actions-cell">
                          <button className="btn btn-outline btn-sm" onClick={() => copyToClipboard(key.key || key.api_key)}>
                            复制
                          </button>
                          <button className="btn btn-danger btn-sm" onClick={() => handleDelete(key.id)}>
                            删除
                          </button>
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
              <h3>生成 API 密钥</h3>
              <button className="modal-close" onClick={closeModal}>&times;</button>
            </div>
            <div className="modal-body">
              {generatedKey ? (
                <div>
                  <div className="success-message">密钥生成成功！</div>
                  <div className="form-group">
                    <label>您的 API 密钥</label>
                    <div className="generated-key-box">
                      <code className="generated-key">{generatedKey.key || generatedKey.api_key || JSON.stringify(generatedKey)}</code>
                    </div>
                    <p className="key-warning">请立即复制此密钥，关闭后将无法再次查看。</p>
                  </div>
                  <button className="btn btn-primary" onClick={() => copyToClipboard(generatedKey.key || generatedKey.api_key)} style={{ width: '100%' }}>
                    复制到剪贴板
                  </button>
                </div>
              ) : (
                <div className="form-group">
                  <label>密钥名称</label>
                  <input className="form-input" placeholder="例如：开发环境密钥" value={keyName} onChange={(e) => setKeyName(e.target.value)} autoFocus />
                </div>
              )}
            </div>
            <div className="modal-footer">
              {generatedKey ? (
                <button className="btn btn-primary" onClick={closeModal}>完成</button>
              ) : (
                <>
                  <button className="btn btn-outline" onClick={closeModal}>取消</button>
                  <button className="btn btn-primary" onClick={handleGenerate} disabled={generating}>
                    {generating ? '生成中...' : '生成'}
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