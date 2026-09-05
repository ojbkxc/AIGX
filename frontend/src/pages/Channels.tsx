import React, { useState } from 'react';
import type { Channel } from '../types';

interface ChannelsProps {
  children?: React.ReactNode;
}

export default function Channels(): JSX.Element {
  // 状态定义
  const [channels, setChannels] = useState<Channel[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string>('');

  // 表单状态
  const [editChannel, setEditChannel] = useState<Channel | null>(null);
  const [form, setForm] = useState<ChannelsFormState>({
    name: '',
    channel_type: 'openai_compatible',
    base_url: '',
    api_key: '',
    priority: 0,
    models: '',
  });
  const [saving, setSaving] = useState(false);

  return (
    <div>
      <div className="page-header">
        <h1>渠道管理</h1>
        <p>管理 AI 网关的渠道连接</p>
      </div>

      {/* 渠道列表 */}
      <div className="channels-list">
        {loading ? (
          <div className="loading">加载渠道中...</div>
        ) : error ? (
          <div className="error-message">{error}</div>
        ) : channels.length > 0 ? (
          channels.map((channel) => (
            <div key={channel.id} className="channel-item">
              <h3>{channel.name}</h3>
              <p>{channel.base_url}</p>
              <div className="channel-actions">
                <button onClick={() => {/* 编辑逻辑 */}}>
                  编辑
                </button>
                <button onClick={() => {/* 删除逻辑 */}}>
                  删除
                </button>
              </div>
            </div>
          ))
        ) : (
          <div className="empty-state">
            <p>暂无渠道</p>
            <button onClick={() => {/* 创建新渠道 */}}>
              + 添加渠道
            </button>
          </div>
        )}
      </div>
    </div>
  );
}

interface ChannelsFormState {
  name: string;
  channel_type: string;
  base_url: string;
  api_key: string;
  priority: number;
  models: string;
}