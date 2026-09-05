import React, { useState } from 'react';

interface NotifyProps {
  children?: React.ReactNode;
}

export default function Notify(): JSX.Element {
  const [telegramConfig, setTelegramConfig] = useState({ apiKey: '', chatId: '' });
  const [smtpConfig, setSmtpConfig] = useState({
    enabled: false,
    host: '',
    port: 587,
    user: '',
    password: '',
    from: '',
  });

  return (
    <div>
      <div className="page-header">
        <h1>通知配置</h1>
        <p>配置消息通知</p>
      </div>

      {/* Telegram 配置 */}
      <div className="settings-section">
        <h2>Telegram 通知</h2>
        <div className="form-group">
          <label>API 密钥</label>
          <input
            type="text"
            value={telegramConfig.apiKey}
            onChange={(e) => setTelegramConfig({ ...telegramConfig, apiKey: e.target.value })}
            placeholder="输入 Telegram API 密钥"
          />
        </div>
        <div className="form-group">
          <label>Chat ID</label>
          <input
            type="text"
            value={telegramConfig.chatId}
            onChange={(e) => setTelegramConfig({ ...telegramConfig, chatId: e.target.value })}
            placeholder="输入收件人的 Chat ID"
          />
        </div>
      </div>

      {/* SMTP 配置 */}
      <div className="settings-section">
        <h2>Email 通知</h2>
        <div className="form-group">
          <label>SMTP 服务商</label>
          <select onChange={(e) => setSmtpConfig({ ...smtpConfig, enabled: e.target.value === 'enabled' })}>
            <option value="disabled">已禁用</option>
            <option value="enabled">已启用</option>
          </select>
        </div>
        <div className="form-group">
          <label>SMTP 主机</label>
          <input
            type="text"
            value={smtpConfig.host}
            onChange={(e) => setSmtpConfig({ ...smtpConfig, host: e.target.value })}
          />
        </div>
        <div className="form-group">
          <label>SMTP 端口</label>
          <input
            type="number"
            value={smtpConfig.port}
            onChange={(e) => setSmtpConfig({ ...smtpConfig, port: Number(e.target.value) })}
          />
        </div>
        <div className="form-group">
          <label>用户名</label>
          <input
            type="text"
            value={smtpConfig.user}
            onChange={(e) => setSmtpConfig({ ...smtpConfig, user: e.target.value })}
          />
        </div>
        <div className="form-group">
          <label>密码</label>
          <input
            type="password"
            value={smtpConfig.password}
            onChange={(e) => setSmtpConfig({ ...smtpConfig, password: e.target.value })}
          />
        </div>
        <div className="form-group">
          <label>发件人邮箱</label>
          <input
            type="email"
            value={smtpConfig.from}
            onChange={(e) => setSmtpConfig({ ...smtpConfig, from: e.target.value })}
            placeholder="noreply@yourdomain.com"
          />
        </div>
      </div>

      <button onClick={() => {/* 保存配置 */}}>保存配置</button>
    </div>
  );
}