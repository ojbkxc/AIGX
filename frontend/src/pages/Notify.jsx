import React, { useState, useEffect } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api';
import { useToast } from '../components/Toast';
import './Notify.css';

export default function Notify() {
  const { t } = useTranslation();
  const addToast = useToast();
  const [notify, setNotify] = useState(null);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState('');

  const [testEmailTo, setTestEmailTo] = useState('');
  const [testingTg, setTestingTg] = useState(false);
  const [testingEmail, setTestingEmail] = useState(false);

  useEffect(() => { loadConfig(); }, []);

  const loadConfig = async () => {
    setLoading(true);
    setError('');
    try {
      const res = await api.getNotifyConfig();
      setNotify(res.data || res);
    } catch (err) {
      setError(err.message);
    } finally {
      setLoading(false);
    }
  };

  const handleChange = (field, value) => {
    setNotify({ ...notify, [field]: value });
  };

  // payload 与后端 UpdateNotifyConfigRequest 对齐
  const handleSave = async () => {
    setSaving(true);
    setError('');
    try {
      const payload = {
        enabled: notify.enabled,
        telegram_chat_id: notify.telegram_chat_id,
        smtp_host: notify.smtp_host,
        smtp_port: notify.smtp_port ? Number(notify.smtp_port) : 0,
        smtp_username: notify.smtp_username,
        smtp_from: notify.smtp_from,
      };
      // 敏感凭据字段：仅当输入了新的非脱敏值才提交；空串/未修改的脱敏占位不带该字段，
      // 避免把真实 Bot Token / SMTP 密码覆盖为空（后端契约：缺省字段保持原值）
      const botToken = (notify.telegram_bot_token || '').trim();
      if (botToken && !botToken.includes('***')) {
        payload.telegram_bot_token = botToken;
      }
      const smtpPassword = (notify.smtp_password || '').trim();
      if (smtpPassword && !smtpPassword.includes('***')) {
        payload.smtp_password = smtpPassword;
      }
      await api.updateNotifyConfig(payload);
      addToast(t('通知配置保存成功'));
      loadConfig();
    } catch (err) {
      setError(err.message);
    } finally {
      setSaving(false);
    }
  };

  const handleTestTelegram = async () => {
    setTestingTg(true);
    setError('');
    try {
      const res = await api.testTelegram();
      addToast(res.data || t('Telegram 测试消息已发送'));
    } catch (err) {
      setError(err.message);
    } finally {
      setTestingTg(false);
    }
  };

  const handleTestEmail = async () => {
    if (!testEmailTo) {
      setError(t('请输入收件邮箱'));
      return;
    }
    setTestingEmail(true);
    setError('');
    try {
      const res = await api.testEmail(testEmailTo);
      addToast(res.data || t('测试邮件已发送'));
    } catch (err) {
      setError(err.message);
    } finally {
      setTestingEmail(false);
    }
  };

  if (loading) return <div className="loading">{t('加载通知配置')}</div>;

  return (
    <div className="notify-shell">
      {/* PageIntro 标题区 */}
      <div className="page-header">
        <div>
          <h1>{t('通知设置')}</h1>
          <p>{t('配置 Telegram Bot 与 SMTP 邮件通知。事件触发时推送：充值成功 / 额度不足 / 渠道故障。')}</p>
        </div>
      </div>

      {error && <div className="error-message">{error}</div>}

      <div className="notify-content">
        {/* 总开关 */}
        <div className="card">
          <div className="card-header">
            <h2>{t('通知配置')}</h2>
          </div>
          <div className="card-body">
            <div className="notify-toggle-row">
              <label className="notify-toggle">
                <input
                  type="checkbox"
                  checked={notify.enabled ?? false}
                  onChange={(e) => handleChange('enabled', e.target.checked)}
                />
                <span className="toggle-label">{t('启用通知')}</span>
              </label>
              <span className="form-hint">{t('总开关，关闭后不发送任何通知')}</span>
            </div>
          </div>
        </div>

        {/* Telegram 通知 */}
        <div className="card">
          <div className="card-header">
            <h2>{t('Telegram 通知')}</h2>
            {notify.telegram_ready && (
              <span className="badge badge-success">{t('已就绪')}</span>
            )}
          </div>
          <div className="card-body">
            <div className="notify-form-grid">
              <div className="form-group">
                <label>Bot Token</label>
                <input
                  className="form-input"
                  type="text"
                  placeholder="123456:ABC-DEF..."
                  value={notify.telegram_bot_token ?? ''}
                  onChange={(e) => handleChange('telegram_bot_token', e.target.value)}
                />
                <span className="form-hint">{t('从 @BotFather 获取的 Bot Token')}</span>
              </div>
              <div className="form-group">
                <label>Chat ID</label>
                <input
                  className="form-input"
                  type="text"
                  placeholder="-1001234567890"
                  value={notify.telegram_chat_id ?? ''}
                  onChange={(e) => handleChange('telegram_chat_id', e.target.value)}
                />
                <span className="form-hint">{t('群组/频道 Chat ID')}</span>
              </div>
            </div>
            <div className="notify-actions">
              <button
                className="btn btn-outline"
                onClick={handleTestTelegram}
                disabled={testingTg || !notify.telegram_ready}
              >
                {testingTg ? t('发送中...') : t('测试 Telegram')}
              </button>
            </div>
          </div>
        </div>

        {/* SMTP 邮件通知 */}
        <div className="card">
          <div className="card-header">
            <h2>{t('SMTP 邮件通知')}</h2>
            {notify.smtp_ready && (
              <span className="badge badge-success">{t('已就绪')}</span>
            )}
          </div>
          <div className="card-body">
            <p className="notify-note">
              {t('注：当前为原生 TCP SMTP（AUTH LOGIN，明文），适用于本地邮件中继或内网 SMTP（端口 25）。TLS（465/587）后续支持。')}
            </p>
            <div className="notify-form-grid-2">
              <div className="form-group" style={{ flex: '2 1 200px' }}>
                <label>SMTP Host</label>
                <input
                  className="form-input"
                  type="text"
                  placeholder="smtp.example.com"
                  value={notify.smtp_host ?? ''}
                  onChange={(e) => handleChange('smtp_host', e.target.value)}
                />
              </div>
              <div className="form-group" style={{ flex: '1 1 100px' }}>
                <label>SMTP Port</label>
                <input
                  className="form-input"
                  type="number"
                  placeholder="25"
                  value={notify.smtp_port ?? ''}
                  onChange={(e) => handleChange('smtp_port', e.target.value)}
                />
              </div>
            </div>
            <div className="notify-form-grid">
              <div className="form-group">
                <label>{t('用户名')}</label>
                <input
                  className="form-input"
                  type="text"
                  value={notify.smtp_username ?? ''}
                  onChange={(e) => handleChange('smtp_username', e.target.value)}
                />
              </div>
              <div className="form-group">
                <label>{t('密码')}</label>
                <input
                  className="form-input"
                  type="password"
                  placeholder={t('留空表示不修改')}
                  value={notify.smtp_password ?? ''}
                  onChange={(e) => handleChange('smtp_password', e.target.value)}
                />
              </div>
            </div>
            <div className="form-group">
              <label>{t('发件人地址')}</label>
              <input
                className="form-input"
                type="text"
                placeholder="noreply@example.com"
                value={notify.smtp_from ?? ''}
                onChange={(e) => handleChange('smtp_from', e.target.value)}
              />
            </div>
            <div className="notify-test-email-row">
              <div className="form-group" style={{ flex: 1, margin: 0 }}>
                <label>{t('测试收件邮箱')}</label>
                <input
                  className="form-input"
                  type="email"
                  placeholder="you@example.com"
                  value={testEmailTo}
                  onChange={(e) => setTestEmailTo(e.target.value)}
                />
              </div>
              <button
                className="btn btn-outline"
                onClick={handleTestEmail}
                disabled={testingEmail || !notify.smtp_ready}
              >
                {testingEmail ? t('发送中...') : t('测试邮件')}
              </button>
            </div>
          </div>
        </div>

        {/* 保存按钮 */}
        <div className="notify-save-bar">
          <button className="btn btn-primary" onClick={handleSave} disabled={saving}>
            {saving ? t('保存中...') : t('保存通知配置')}
          </button>
        </div>
      </div>
    </div>
  );
}