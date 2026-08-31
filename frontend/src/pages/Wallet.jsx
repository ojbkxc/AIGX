import React, { useState, useEffect } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api';
import { useToast } from '../components/Toast';

export default function Wallet() {
  const [me, setMe] = useState(null);
  const [epay, setEpay] = useState(null);
  const [orders, setOrders] = useState([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  const addToast = useToast();
  const { t } = useTranslation();

  const [amount, setAmount] = useState(10);
  const [method, setMethod] = useState('alipay');
  const [submitting, setSubmitting] = useState(false);

  // 兑换码
  const [redeemCode, setRedeemCode] = useState('');
  const [redeeming, setRedeeming] = useState(false);

  useEffect(() => {
    load();
  }, []);

  const load = async () => {
    setLoading(true);
    setError('');
    try {
      const [meRes, epayRes, orderRes] = await Promise.all([
        api.getMe().catch(() => null),
        api.getEpayConfig().catch(() => null),
        api.myOrders().catch(() => null),
      ]);
      if (meRes) setMe(meRes.data || null);
      if (epayRes) setEpay(epayRes.data || null);
      if (orderRes) setOrders(orderRes.data || []);
    } catch (err) {
      setError(err.message);
    } finally {
      setLoading(false);
    }
  };

  const fmtQuota = (q) => {
    const n = Number(q || 0);
    if (n >= 1_000_000) return (n / 1_000_000).toFixed(2) + 'M';
    if (n >= 1_000) return (n / 1_000).toFixed(2) + 'K';
    return String(n);
  };

  const handleTopup = async () => {
    const amt = Math.floor(Number(amount));

    if (!amt || amt <= 0) {
      setError(t('请输入有效金额'));
      return;
    }
    if (epay && amt < (epay.min_topup || 1)) {
      setError(`${t('最低充值')} ${epay.min_topup} ${t('元')}`);
      return;
    }
    setSubmitting(true);
    setError('');
    try {
      const res = await api.topup(amt, method);
      const params = res.data || {};
      const url = res.url;
      if (!url) {
        setError(t('支付网关未返回跳转地址，请检查易支付配置'));
        setSubmitting(false);
        return;
      }
      // 构造表单并提交
      const formEl = document.createElement('form');
      formEl.method = 'POST';
      formEl.action = url;
      for (const [k, v] of Object.entries(params)) {
        const input = document.createElement('input');
        input.type = 'hidden';
        input.name = k;
        input.value = v;
        formEl.appendChild(input);
      }
      document.body.appendChild(formEl);
      formEl.submit();
    } catch (err) {
      setError(err.message);
    } finally {
      setSubmitting(false);
    }
  };

  // 兑换码兑换（放在 loading 早退之前，保证 hooks 与事件处理函数定义顺序稳定）
  const handleRedeem = async () => {
    if (!redeemCode.trim()) {
      setError(t('请输入兑换码'));
      return;
    }
    setRedeeming(true);
    setError('');
    try {
      const res = await api.redeem(redeemCode.trim());
      const msg = res.message || res.msg || t('兑换成功');
      addToast(msg);
      setRedeemCode('');
      // 刷新账户信息
      const meRes = await api.getMe();
      if (meRes) setMe(meRes.data || null);
    } catch (err) {
      setError(err.message);
    } finally {
      setRedeeming(false);
    }
  };

  if (loading) return <div className="loading">{t('加载钱包')}</div>;

  const remaining = me ? (me.quota || 0) - (me.used_quota || 0) : 0;
  const methods = (epay && epay.pay_methods) || ['alipay', 'wxpay'];

  return (
    <div>
      <div className="page-header">
        <h1>{t('钱包充值')}</h1>
        <p>{t('通过易支付为账户充值配额')}</p>
      </div>

      {error && <div className="error-message">{error}</div>}

      {!epay || !epay.pay_address ? (
        <div className="card">
          <div className="card-body">
            <div className="empty-state">
              <p>{t('管理员尚未配置易支付，暂无法充值。请联系管理员在「易支付」页面完成配置。')}</p>
            </div>
          </div>
        </div>
      ) : (
        <>
          <div className="card" style={{ marginBottom: 16 }}>
            <div className="card-body" style={{ display: 'flex', justifyContent: 'space-between', flexWrap: 'wrap', gap: 16 }}>
              <div>
                <div style={{ fontSize: 13, color: 'var(--text-muted)' }}>{t('当前账户')}</div>
                <div style={{ fontSize: 18, fontWeight: 600 }}>{me?.email || '—'}</div>
                {me?.username && <div style={{ fontSize: 12, color: 'var(--text-muted)' }}>@{me.username}</div>}
              </div>
              <div>
                <div style={{ fontSize: 13, color: 'var(--text-muted)' }}>{t('剩余配额')}</div>
                <div style={{ fontSize: 22, fontWeight: 700, background: 'var(--primary-gradient)', WebkitBackgroundClip: 'text', WebkitTextFillColor: 'transparent' }}>
                  {fmtQuota(remaining)}
                </div>
              </div>
              <div>
                <div style={{ fontSize: 13, color: 'var(--text-muted)' }}>{t('兑换倍率')}</div>
                <div style={{ fontSize: 16, fontWeight: 600 }}>{t('1 元 =')} {epay.price || 1} {t('配额')}</div>
              </div>
            </div>
          </div>

          <div className="card" style={{ marginBottom: 16 }}>
            <div className="card-header"><h2>{t('充值')}</h2></div>
            <div className="card-body">
              <div style={{ display: 'grid', gap: 16, maxWidth: 480 }}>
                <div className="form-group">
                  <label>{t('充值金额（元）')}</label>
                  <input className="form-input" type="number" step="1" min={epay.min_topup || 1} value={amount}
                    onChange={(e) => setAmount(e.target.value)} />
                  <div style={{ fontSize: 12, color: 'var(--text-muted)', marginTop: 4 }}>
                    {t('最低')} {epay.min_topup || 1} {t('元，将获得')} {fmtQuota((Number(amount) || 0) * (epay.price || 1))} {t('配额')}
                  </div>
                </div>
                <div className="form-group">
                  <label>{t('支付方式')}</label>
                  <select className="form-input" value={method} onChange={(e) => setMethod(e.target.value)}>
                    {methods.map((m) => (
                      <option key={m} value={m}>
                        {m === 'alipay' ? t('支付宝') : m === 'wxpay' ? t('微信支付') : m}
                      </option>
                    ))}
                  </select>
                </div>
                <button className="btn btn-primary" onClick={handleTopup} disabled={submitting}>
                  {submitting ? t('正在跳转...') : t('立即充值')}
                </button>
              </div>
            </div>
          </div>
        </>
      )}

      <div className="card" style={{ marginBottom: 16 }}>
        <div className="card-header"><h2>{t('兑换码充值')}</h2></div>
        <div className="card-body">
          <div style={{ display: 'grid', gap: 16, maxWidth: 480 }}>
            <div className="form-group">
              <label>{t('兑换码')}</label>
              <input
                className="form-input"
                value={redeemCode}
                onChange={(e) => setRedeemCode(e.target.value)}
                placeholder={t('输入兑换码直接充值配额')}
                style={{ fontFamily: 'monospace', letterSpacing: 1 }}
              />
              <span className="form-hint">{t('输入管理员发放的兑换码，即可将对应配额充入账户。')}</span>
            </div>
            <button className="btn btn-primary" onClick={handleRedeem} disabled={redeeming}>
              {redeeming ? t('兑换中...') : t('立即兑换')}
            </button>
          </div>
        </div>
      </div>

      <div className="card">
        <div className="card-header"><h2>{t('我的订单')} ({orders.length})</h2></div>
        <div className="card-body">
          {orders.length === 0 ? (
            <div className="empty-state"><p>{t('暂无订单')}</p></div>
          ) : (
            <div className="table-wrapper">
              <table>
                <thead>
                  <tr>
                    <th>{t('订单号')}</th>
                    <th>{t('金额')}</th>
                    <th>{t('配额')}</th>
                    <th>{t('支付方式')}</th>
                    <th>{t('状态')}</th>
                    <th>{t('创建时间')}</th>
                  </tr>
                </thead>
                <tbody>
                  {orders.map((o) => (
                    <tr key={o.trade_no}>
                      <td><code className="key-value" style={{ maxWidth: 240 }}>{o.trade_no}</code></td>
                      <td>¥{Number(o.money || 0).toFixed(2)}</td>
                      <td>{fmtQuota(o.quota != null ? o.quota : o.amount * (epay?.price || 1))}</td>
                      <td>{o.payment_method}</td>
                      <td>
                        <span className={
                          o.status === 'paid' ? 'badge badge-success'
                            : o.status === 'expired' ? 'badge badge-neutral'
                              : 'badge badge-warning'
                        }>
                          {o.status === 'paid' ? t('已支付') : o.status === 'expired' ? t('已过期') : t('待支付')}
                        </span>
                      </td>
                      <td>{o.create_time ? new Date(o.create_time * 1000).toLocaleString() : '—'}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
