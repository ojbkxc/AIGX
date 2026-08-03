import React, { useState, useEffect } from 'react';
import { api } from '../api';
import { useToast } from '../components/Toast';
import './Settings.css';

export default function Settings() {
  const [limits, setLimits] = useState({
    daily_limit: '',
    monthly_limit: '',
    threshold: '',
    api_timeout_secs: '',
    max_retries: '',
  });
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState('');
  const addToast = useToast();

  useEffect(() => {
    loadLimits();
  }, []);

  const loadLimits = async () => {
    setLoading(true);
    setError('');
    try {
      const res = await api.getLimits();
      const data = res.data || res;
      setLimits({
        daily_limit: data.daily_limit ?? '',
        monthly_limit: data.monthly_limit ?? '',
        threshold: data.threshold != null ? data.threshold * 100 : '',
        api_timeout_secs: data.api_timeout_secs ?? '',
        max_retries: data.max_retries ?? '',
      });
    } catch (err) {
      setError(err.message);
    } finally {
      setLoading(false);
    }
  };

  const handleChange = (field, value) => {
    setLimits({ ...limits, [field]: value });
  };

  const handleSave = async () => {
    const payload = {};
    if (limits.daily_limit !== '') payload.daily_limit = Number(limits.daily_limit);
    if (limits.monthly_limit !== '') payload.monthly_limit = Number(limits.monthly_limit);
    if (limits.threshold !== '') payload.threshold = Number(limits.threshold) / 100;
    if (limits.api_timeout_secs !== '') payload.api_timeout_secs = Number(limits.api_timeout_secs);
    if (limits.max_retries !== '') payload.max_retries = Number(limits.max_retries);

    if (payload.daily_limit < 0 || payload.monthly_limit < 0 || (payload.threshold != null && (payload.threshold < 0 || payload.threshold > 1))) {
      setError('请输入有效值。日/月限额必须 >= 0。阈值必须在 0-100 之间。');
      return;
    }
    if (payload.api_timeout_secs != null && payload.api_timeout_secs < 5) {
      setError('API 超时时间至少为 5 秒');
      return;
    }
    if (payload.max_retries != null && (payload.max_retries < 0 || payload.max_retries > 10)) {
      setError('最大重试次数必须在 0-10 之间');
      return;
    }

    setSaving(true);
    setError('');
    try {
      await api.updateLimits(payload);
      addToast('设置更新成功');
    } catch (err) {
      setError(err.message);
    } finally {
      setSaving(false);
    }
  };

  if (loading) return <div className="loading">加载设置</div>;

  return (
    <div>
      <div className="page-header">
        <h1>系统设置</h1>
        <p>配置使用限额、API 超时与重试策略</p>
      </div>

      {error && <div className="error-message">{error}</div>}

      <div className="card">
        <div className="card-header">
          <h2>使用限额</h2>
        </div>
        <div className="card-body">
          <div className="settings-form">
            <div className="form-group">
              <label>每日 Token 限额</label>
              <input className="form-input" type="number" min="0" placeholder="例如：1000000" value={limits.daily_limit} onChange={(e) => handleChange('daily_limit', e.target.value)} />
              <span className="form-hint">每天允许的最大 Token 数。0 或空 = 无限制。</span>
            </div>
            <div className="form-group">
              <label>每月 Token 限额</label>
              <input className="form-input" type="number" min="0" placeholder="例如：30000000" value={limits.monthly_limit} onChange={(e) => handleChange('monthly_limit', e.target.value)} />
              <span className="form-hint">每月允许的最大 Token 数。0 或空 = 无限制。</span>
            </div>
            <div className="form-group">
              <label>告警阈值 (%)</label>
              <input className="form-input" type="number" min="0" max="100" placeholder="例如：80" value={limits.threshold} onChange={(e) => handleChange('threshold', e.target.value)} />
              <span className="form-hint">触发告警的限额使用百分比（0-100）。</span>
            </div>
          </div>
        </div>
      </div>

      <div className="card" style={{ marginTop: 16 }}>
        <div className="card-header">
          <h2>API 配置</h2>
        </div>
        <div className="card-body">
          <div className="settings-form">
            <div className="form-group">
              <label>API 超时时间 (秒)</label>
              <input className="form-input" type="number" min="5" max="300" placeholder="默认：120" value={limits.api_timeout_secs} onChange={(e) => handleChange('api_timeout_secs', e.target.value)} />
              <span className="form-hint">向 Cloudflare API 发送请求的超时时间，默认 120 秒。</span>
            </div>
            <div className="form-group">
              <label>最大重试次数</label>
              <input className="form-input" type="number" min="0" max="10" placeholder="默认：2" value={limits.max_retries} onChange={(e) => handleChange('max_retries', e.target.value)} />
              <span className="form-hint">API 请求失败时的最大重试次数，0 表示不重试。</span>
            </div>
            <div className="settings-actions">
              <button className="btn btn-primary" onClick={handleSave} disabled={saving}>
                {saving ? '保存中...' : '保存更改'}
              </button>
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}