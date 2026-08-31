import React, { useState, useEffect } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api';

export default function Orders() {
  const [orders, setOrders] = useState([]);
  const [epay, setEpay] = useState(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  const { t } = useTranslation();

  useEffect(() => {
    load();
  }, []);

  const load = async () => {
    setLoading(true);
    setError('');
    try {
      // 并行拉取订单与易支付配置（旧订单无 quota 字段时按 amount × price 回退计算配额）
      const [orderRes, epayRes] = await Promise.all([
        api.listOrders(),
        api.getEpayConfig().catch(() => null),
      ]);
      setOrders(orderRes.data || []);
      if (epayRes) setEpay(epayRes.data || null);
    } catch (err) {
      setError(err.message);
    } finally {
      setLoading(false);
    }
  };

  // 配额数值格式化（与 Wallet 页保持一致）
  const fmtQuota = (q) => {
    const n = Number(q || 0);
    if (n >= 1_000_000) return (n / 1_000_000).toFixed(2) + 'M';
    if (n >= 1_000) return (n / 1_000).toFixed(2) + 'K';
    return String(n);
  };

  if (loading) return <div className="loading">{t('加载订单')}</div>;

  return (
    <div>
      <div className="page-header">
        <h1>{t('订单记录')}</h1>
        <p>{t('所有用户的充值订单（管理员视图）')}</p>
      </div>

      {error && <div className="error-message">{error}</div>}

      <div className="card">
        <div className="card-header"><h2>{t('所有订单')} ({orders.length})</h2></div>
        <div className="card-body">
          {orders.length === 0 ? (
            <div className="empty-state"><p>{t('暂无订单')}</p></div>
          ) : (
            <div className="table-wrapper">
              <table>
                <thead>
                  <tr>
                    <th>{t('订单号')}</th>
                    <th>{t('用户ID')}</th>
                    <th>{t('金额')}</th>
                    <th>{t('配额')}</th>
                    <th>{t('支付方式')}</th>
                    <th>{t('状态')}</th>
                    <th>{t('创建时间')}</th>
                    <th>{t('支付时间')}</th>
                  </tr>
                </thead>
                <tbody>
                  {orders.map((o) => (
                    <tr key={o.trade_no}>
                      <td><code className="key-value" style={{ maxWidth: 240 }}>{o.trade_no}</code></td>
                      <td style={{ fontSize: 12 }}>{o.user_id?.slice(0, 8)}…</td>
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
                      <td>{o.paid_time ? new Date(o.paid_time * 1000).toLocaleString() : '—'}</td>
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
