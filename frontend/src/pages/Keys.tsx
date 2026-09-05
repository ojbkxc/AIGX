import { useState, useEffect } from 'react';
import { api } from '../api';
import type { ApiKey } from '../types';

export default function Keys(): JSX.Element {
  const [keys, setKeys] = useState<ApiKey[]>([]);
  const [loading, setLoading] = useState(true);

  const loadKeys = async () => {
    setLoading(true);
    try {
      const res = await api.listKeys();
      setKeys(res.data || []);
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    void loadKeys();
  }, []);

  return (
    <div>
      <div className="page-header">
        <h1>API 密钥管理</h1>
        <p>管理系统 API 密钥</p>
      </div>

      <div className="keys-list">
        {loading ? (
          <div className="loading">加载中...</div>
        ) : keys.length > 0 ? (
          keys.map((key) => (
            <div key={key.id} className="key-item">
              <h3>{key.name}</h3>
              <p>{key.key}</p>
              <span className="key-meta">
                创建时间: {key.created_at}
              </span>
            </div>
          ))
        ) : (
          <div className="empty-state">
            <p>暂无 API 密钥</p>
            <button onClick={() => {/* 创建密钥逻辑 */}}>
              + 添加密钥
            </button>
          </div>
        )}
      </div>
    </div>
  );
}
