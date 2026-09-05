import { useState, useEffect } from 'react';
import { api } from '../api';
import type { Redemption } from './types';

export default function Redemptions(): JSX.Element {
  const [redemptions, setRedemptions] = useState<Redemption[]>([]);
  const [loading, setLoading] = useState(true);

  const loadRedemptions = async () => {
    setLoading(true);
    try {
      const res = await api.listRedemptions();
      setRedemptions(res.data || []);
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    void loadRedemptions();
  }, []);

  return (
    <div>
      <div className="page-header">
        <h1>兑换码管理</h1>
        <p>生成和使用兑换码</p>
      </div>

      {/* 兑换码列表 */}
      <div className="redemptions-list">
        {loading ? (
          <div className="loading">加载中...</div>
        ) : redemptions.length > 0 ? (
          redemptions.map((redemption) => (
            <div key={redemption.id} className="redemption-item">
              <h3>{redemption.code}</h3>
              <p>使用次数: {redemption.usage_count}</p>
              <span className={`redemption-status redemption-status-${redemption.status}`}>
                {redemption.status}
              </span>
            </div>
          ))
        ) : (
          <div className="empty-state">
            <p>暂无兑换码</p>
            <button onClick={() => {/* 生成兑换码 */}}>
              + 生成兑换码
            </button>
          </div>
        )}
      </div>
    </div>
  );
}
