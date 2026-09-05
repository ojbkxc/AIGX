import React, { useState } from 'react';
import { api } from '../api';

interface PricingProps {
  children?: React.ReactNode;
}

export default function Pricing(): JSX.Element {
  const [prices, setPrices] = useState<any[]>([]);
  const [loading, setLoading] = useState(true);

  const loadPrices = async () => {
    setLoading(true);
    try {
      const res = await api.listPrices();
      setPrices(res.data || []);
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    loadPrices();
  }, []);

  return (
    <div>
      <div className="page-header">
        <h1>定价管理</h1>
        <p>管理 AI 模型定价</p>
      </div>

      {/* 定价列表 */}
      <div className="pricing-list">
        {loading ? (
          <div className="loading">加载中...</div>
        ) : prices.length > 0 ? (
          prices.map((price) => (
            <div key={price.id} className="pricing-item">
              <h3>{price.model}</h3>
              <p>输入: ¥{price.input_price} / 1K tokens</p>
              <p>输出: ¥{price.output_price} / 1K tokens</p>
              <button onClick={() => {/* 编辑定价 */}}>编辑</button>
            </div>
          ))
        ) : (
          <p>暂无定价信息</p>
        )}
      </div>
    </div>
  );
}