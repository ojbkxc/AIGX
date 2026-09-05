import { useState, useEffect } from 'react';
import type { Channel } from '../types';
import { api } from '../api';

export default function Channels(): JSX.Element {
  const [channels, setChannels] = useState<Channel[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string>('');

  useEffect(() => {
    void (async () => {
      setLoading(true);
      try {
        const res = await api.listChannels();
        setChannels(Array.isArray(res?.data) ? res.data : []);
      } catch (e) {
        setError(e instanceof Error ? e.message : '加载渠道失败');
      } finally {
        setLoading(false);
      }
    })();
  }, []);

  return (
    <div>
      <div className="page-header">
        <h1>渠道管理</h1>
        <p>管理 AI 网关的渠道连接</p>
      </div>

      {loading ? (
        <div className="loading">加载渠道中...</div>
      ) : error ? (
        <div className="error-message">{error}</div>
      ) : channels.length > 0 ? (
        <div className="channels-list">
          {channels.map((channel) => (
            <div key={channel.id} className="channel-item">
              <h3>{channel.name}</h3>
              <p>{channel.type}</p>
            </div>
          ))}
        </div>
      ) : (
        <div className="empty-state">
          <p>暂无渠道</p>
        </div>
      )}
    </div>
  );
}