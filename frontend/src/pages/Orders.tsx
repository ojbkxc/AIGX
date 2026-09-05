import React, { useState } from 'react';
import { api } from '../api';

interface OrdersProps {
  children?: React.ReactNode;
}

export default function Orders(): JSX.Element {
  const [orders, setOrders] = useState<Order[]>([]);
  const [loading, setLoading] = useState(true);

  const loadOrders = async () => {
    setLoading(true);
    try {
      const res = await api.listOrders();
      setOrders(res.data || []);
    } catch (err) {
      console.error('加载订单失败:', err);
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    loadOrders();
  }, []);

  return (
    <div>
      <div className="page-header">
        <h1>订单管理</h1>
        <p>查看和管理订单</p>
      </div>

      {/* 订单列表 */}
      <div className="orders-list">
        {loading ? (
          <div className="loading">加载中...</div>
        ) : orders.length > 0 ? (
          orders.map((order) => (
            <div key={order.id} className="order-item">
              <div className="order-header">
                <span>订单号: {order.id}</span>
                <span className={`order-status order-status-${order.status}`}>
                  {order.status}
                </span>
              </div>
              <div className="order-details">
                <span>金额: ¥{order.amount}</span>
                <span>支付方式: {order.method}</span>
                <span>创建时间: {order.created_at}</span>
              </div>
            </div>
          ))
        ) : (
          <div className="empty-state">
            <p>暂无订单</p>
            <button onClick={() => {/* 创建订单逻辑 */}}>
              + 创建订单
            </button>
          </div>
        )}
      </div>
    </div>
  );
}