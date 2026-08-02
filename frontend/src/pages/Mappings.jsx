import React, { useState, useEffect } from 'react';
import { api } from '../api';
import { useToast } from '../components/Toast';
import './Mappings.css';

export default function Mappings() {
  const [mappings, setMappings] = useState({});
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState('');
  const addToast = useToast();
  const [entries, setEntries] = useState([]);

  useEffect(() => {
    loadMappings();
  }, []);

  const loadMappings = async () => {
    setLoading(true);
    setError('');
    try {
      const res = await api.getSettings();
      const data = res.data || res || {};
      const mappingsData = data.mappings || data;
      setMappings(mappingsData);
      setEntries(Object.entries(mappingsData).map(([key, value]) => ({ key, value })));
    } catch (err) {
      setError(err.message);
    } finally {
      setLoading(false);
    }
  };

  const addEntry = () => {
    setEntries([...entries, { key: '', value: '' }]);
  };

  const removeEntry = (index) => {
    setEntries(entries.filter((_, i) => i !== index));
  };

  const updateEntry = (index, field, val) => {
    const updated = entries.map((entry, i) =>
      i === index ? { ...entry, [field]: val } : entry
    );
    setEntries(updated);
  };

  const handleSave = async () => {
    const filtered = entries.filter((e) => e.key.trim() && e.value.trim());
    const newMappings = {};
    filtered.forEach((e) => {
      newMappings[e.key.trim()] = e.value.trim();
    });
    setSaving(true);
    setError('');
    try {
      await api.updateSettings(newMappings, true);
      addToast('模型映射更新成功');
      setMappings(newMappings);
    } catch (err) {
      setError(err.message);
    } finally {
      setSaving(false);
    }
  };

  if (loading) return <div className="loading">加载模型映射</div>;

  return (
    <div>
      <div className="page-header">
        <h1>模型映射</h1>
        <p>配置网关的模型名称映射</p>
      </div>

      {error && <div className="error-message">{error}</div>}

      <div className="card">
        <div className="card-header">
          <h2>映射 ({entries.length})</h2>
          <div className="mappings-actions">
            <button className="btn btn-outline" onClick={addEntry}>+ 添加映射</button>
            <button className="btn btn-primary" onClick={handleSave} disabled={saving}>
              {saving ? '保存中...' : '保存全部'}
            </button>
          </div>
        </div>
        <div className="card-body">
          {entries.length === 0 ? (
            <div className="empty-state">
              <p>暂无模型映射</p>
              <button className="btn btn-primary" onClick={addEntry}>添加第一个映射</button>
            </div>
          ) : (
            <div className="mappings-list">
              {entries.map((entry, index) => (
                <div className="mapping-row" key={index}>
                  <div className="mapping-field">
                    <label className="mapping-label">模型键</label>
                    <input className="form-input" placeholder="例如：gpt-4" value={entry.key} onChange={(e) => updateEntry(index, 'key', e.target.value)} />
                  </div>
                  <div className="mapping-arrow">→</div>
                  <div className="mapping-field">
                    <label className="mapping-label">映射值</label>
                    <input className="form-input" placeholder="例如：@cf/meta/llama-3" value={entry.value} onChange={(e) => updateEntry(index, 'value', e.target.value)} />
                  </div>
                  <button className="btn btn-danger btn-sm mapping-remove" onClick={() => removeEntry(index)} title="删除">
                    ✕
                  </button>
                </div>
              ))}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}