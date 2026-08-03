import React, { useState, useEffect } from 'react';
import { api } from '../api';
import './Keys.css';

export default function Orders() {
  const [orders, setOrders] = useState([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');

  useEffect(() => {
    load();
  }, []);

  const load = async () => {
    setLoading(true);
    setError('');
    try {
      const res = await api.listOrders();
      setOrders(res.data || []);
    } catch (err) {
      setError(err.message);
    } finally {
      setLoading(false);
    }
  };

  if (loading) return <div className="loading">加载订单</div>;

  return (
    <div>
      <div className="page-header">
        <h1>订单记录</h1>
        <p>所有用户的充值订单（管理员视图）</p>
      </div>

      {error && <div className="error-message">{error}</div>}

      <div className="card">
        <div className="card-header"><h2>所有订单 ({orders.length})</h2></div>
        <div className="card-body">
          {orders.length === 0 ? (
            <div className="empty-state"><p>暂无订单</p></div>
          ) : (
            <div className="table-wrapper">
              <table>
                <thead>
                  <tr>
                    <th>订单号</th>
                    <th>用户ID</th>
                    <th>金额</th>
                    <th>配额</th>
                    <th>支付方式</th>
                    <th>状态</th>
                    <th>创建时间</th>
                    <th>支付时间</th>
                  </tr>
                </thead>
                <tbody>
                  {orders.map((o) => (
                    <tr key={o.trade_no}>
                      <td><code className="key-value" style={{ maxWidth: 240 }}>{o.trade_no}</code></td>
                      <td style={{ fontSize: 12 }}>{o.user_id?.slice(0, 8)}…</td>
                      <td>¥{Number(o.money || 0).toFixed(2)}</td>
                      <td>{o.amount}</td>
                      <td>{o.payment_method}</td>
                      <td>
                        <span style={{
                          padding: '2px 10px', borderRadius: 999, fontSize: 12,
                          background: o.status === 'paid' ? 'rgba(34,197,94,0.15)' : o.status === 'expired' ? 'rgba(148,163,184,0.15)' : 'rgba(234,179,8,0.15)',
                          color: o.status === 'paid' ? 'rgb(34,197,94)' : o.status === 'expired' ? 'rgb(148,163,184)' : 'rgb(234,179,8)',
                        }}>
                          {o.status === 'paid' ? '已支付' : o.status === 'expired' ? '已过期' : '待支付'}
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
