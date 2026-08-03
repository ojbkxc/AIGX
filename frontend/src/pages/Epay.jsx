import React, { useState, useEffect } from 'react';
import { api } from '../api';
import { useToast } from '../components/Toast';
import './Keys.css';

export default function Epay() {
  const [cfg, setCfg] = useState({
    pay_address: '', epay_id: '', epay_key: '',
    pay_methods: ['alipay', 'wxpay'], price: 1, min_topup: 1,
    custom_callback_address: '', server_address: '',
  });
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState('');
  const [showKey, setShowKey] = useState(false);
  const addToast = useToast();

  useEffect(() => {
    load();
  }, []);

  const load = async () => {
    setLoading(true);
    setError('');
    try {
      const res = await api.getEpayConfig();
      const d = res.data || {};
      setCfg({
        pay_address: d.pay_address || '',
        epay_id: String(d.epay_id || ''),
        epay_key: d.epay_key || '',
        pay_methods: d.pay_methods || ['alipay', 'wxpay'],
        price: d.price ?? 1,
        min_topup: d.min_topup ?? 1,
        custom_callback_address: d.custom_callback_address || '',
        server_address: d.server_address || '',
      });
    } catch (err) {
      setError(err.message);
    } finally {
      setLoading(false);
    }
  };

  const handleSave = async () => {
    setSaving(true);
    setError('');
    try {
      await api.updateEpayConfig({
        pay_address: cfg.pay_address,
        epay_id: cfg.epay_id,
        epay_key: cfg.epay_key,
        pay_methods: cfg.pay_methods,
        price: Number(cfg.price),
        min_topup: Number(cfg.min_topup),
        custom_callback_address: cfg.custom_callback_address,
        server_address: cfg.server_address,
      });
      addToast('易支付配置已保存');
      load();
    } catch (err) {
      setError(err.message);
    } finally {
      setSaving(false);
    }
  };

  const toggleMethod = (m) => {
    setCfg((c) => {
      const has = c.pay_methods.includes(m);
      return { ...c, pay_methods: has ? c.pay_methods.filter((x) => x !== m) : [...c.pay_methods, m] };
    });
  };

  if (loading) return <div className="loading">加载易支付配置</div>;

  return (
    <div>
      <div className="page-header">
        <h1>易支付配置</h1>
        <p>对接易支付（Epay）以支持在线充值，签名规则与 new-api 一致</p>
      </div>

      {error && <div className="error-message">{error}</div>}

      <div className="card">
        <div className="card-header"><h2>商户参数</h2></div>
        <div className="card-body">
          <div style={{ display: 'grid', gap: 16, maxWidth: 640 }}>
            <div className="form-group">
              <label>易支付网关地址 (pay_address)</label>
              <input className="form-input" placeholder="https://pay.example.com"
                value={cfg.pay_address}
                onChange={(e) => setCfg({ ...cfg, pay_address: e.target.value })} />
            </div>
            <div className="form-group">
              <label>商户 ID (PID)</label>
              <input className="form-input" value={cfg.epay_id}
                onChange={(e) => setCfg({ ...cfg, epay_id: e.target.value })} />
            </div>
            <div className="form-group">
              <label>商户密钥 (KEY)</label>
              <div style={{ display: 'flex', gap: 8 }}>
                <input className="form-input" type={showKey ? 'text' : 'password'}
                  placeholder="留空则不修改"
                  value={cfg.epay_key}
                  onChange={(e) => setCfg({ ...cfg, epay_key: e.target.value })} />
                <button className="btn btn-outline" onClick={() => setShowKey(!showKey)}>
                  {showKey ? '隐藏' : '显示'}
                </button>
              </div>
            </div>

            <div className="form-group">
              <label>启用的支付方式</label>
              <div style={{ display: 'flex', gap: 10, flexWrap: 'wrap' }}>
                {['alipay', 'wxpay', 'qqpay', 'bank'].map((m) => (
                  <label key={m} style={{
                    display: 'flex', alignItems: 'center', gap: 6,
                    padding: '8px 14px', borderRadius: 10, cursor: 'pointer',
                    border: `1px solid ${cfg.pay_methods.includes(m) ? 'var(--primary)' : 'var(--border-color)'}`,
                    background: cfg.pay_methods.includes(m) ? 'rgba(99,102,241,0.12)' : 'transparent',
                  }}>
                    <input type="checkbox" checked={cfg.pay_methods.includes(m)} onChange={() => toggleMethod(m)} />
                    {m === 'alipay' ? '支付宝' : m === 'wxpay' ? '微信' : m === 'qqpay' ? 'QQ' : m === 'bank' ? '网银' : m}
                  </label>
                ))}
              </div>
            </div>

            <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: 16 }}>
              <div className="form-group">
                <label>兑换倍率 (1 元 = ? 配额)</label>
                <input className="form-input" type="number" step="0.01" value={cfg.price}
                  onChange={(e) => setCfg({ ...cfg, price: e.target.value })} />
              </div>
              <div className="form-group">
                <label>最低充值 (元)</label>
                <input className="form-input" type="number" value={cfg.min_topup}
                  onChange={(e) => setCfg({ ...cfg, min_topup: e.target.value })} />
              </div>
            </div>

            <div className="form-group">
              <label>站点对外访问地址 (server_address)</label>
              <input className="form-input" placeholder="https://your-aigx.example.com"
                value={cfg.server_address}
                onChange={(e) => setCfg({ ...cfg, server_address: e.target.value })} />
              <div style={{ fontSize: 12, color: 'var(--text-muted)', marginTop: 4 }}>
                用于构造易支付异步通知 (notify_url) 与同步跳转 (return_url)
              </div>
            </div>

            <div className="form-group">
              <label>自定义回调地址 (可留空)</label>
              <input className="form-input" placeholder="留空则使用站点地址"
                value={cfg.custom_callback_address}
                onChange={(e) => setCfg({ ...cfg, custom_callback_address: e.target.value })} />
            </div>

            <div>
              <button className="btn btn-primary" onClick={handleSave} disabled={saving}>
                {saving ? '保存中...' : '保存配置'}
              </button>
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}
