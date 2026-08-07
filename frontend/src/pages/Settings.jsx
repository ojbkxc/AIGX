import React, { useState, useEffect } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api';
import { useToast } from '../components/Toast';
import './Settings.css';

export default function Settings() {
  const [limits, setLimits] = useState({
    daily_limit: '',
    monthly_limit: '',
    threshold: '',
    api_timeout_secs: '',
    max_retries: '',
  });
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState('');
  const addToast = useToast();
  const { t } = useTranslation();

  // 限流配置
  const [rlConfig, setRlConfig] = useState(null);
  const [rlLoading, setRlLoading] = useState(false);
  const [rlSaving, setRlSaving] = useState(false);

  // 通知配置
  const [notify, setNotify] = useState(null);
  const [notifyLoading, setNotifyLoading] = useState(false);
  const [notifySaving, setNotifySaving] = useState(false);
  const [testEmailTo, setTestEmailTo] = useState('');
  const [testingTg, setTestingTg] = useState(false);
  const [testingEmail, setTestingEmail] = useState(false);

  useEffect(() => {
    loadLimits();
    loadRateLimitConfig();
    loadNotifyConfig();
  }, []);

  const loadRateLimitConfig = async () => {
    setRlLoading(true);
    try {
      const res = await api.getRateLimitConfig();
      setRlConfig(res.data || res);
    } catch (err) {
      // 限流配置可能未启用，静默处理
    } finally {
      setRlLoading(false);
    }
  };

  const handleRlChange = (field, value) => {
    setRlConfig({ ...rlConfig, [field]: value === '' ? null : Number(value) });
  };

  const handleRlSave = async () => {
    setRlSaving(true);
    setError('');
    try {
      await api.updateRateLimitConfig(rlConfig);
      addToast(t('限流配置更新成功'));
    } catch (err) {
      setError(err.message);
    } finally {
      setRlSaving(false);
    }
  };

  const loadLimits = async () => {
    setLoading(true);
    setError('');
    try {
      const res = await api.getLimits();
      const data = res.data || res;
      setLimits({
        daily_limit: data.daily_limit ?? '',
        monthly_limit: data.monthly_limit ?? '',
        threshold: data.threshold != null ? data.threshold * 100 : '',
        api_timeout_secs: data.api_timeout_secs ?? '',
        max_retries: data.max_retries ?? '',
      });
    } catch (err) {
      setError(err.message);
    } finally {
      setLoading(false);
    }
  };

  // ── 通知配置 ──
  const loadNotifyConfig = async () => {
    setNotifyLoading(true);
    try {
      const res = await api.getNotifyConfig();
      setNotify(res.data || res);
    } catch (err) {
      // 静默处理
    } finally {
      setNotifyLoading(false);
    }
  };

  const handleNotifyChange = (field, value) => {
    setNotify({ ...notify, [field]: value });
  };

  const handleNotifySave = async () => {
    setNotifySaving(true);
    setError('');
    try {
      await api.updateNotifyConfig({
        enabled: notify.enabled,
        telegram_bot_token: notify.telegram_bot_token,
        telegram_chat_id: notify.telegram_chat_id,
        smtp_host: notify.smtp_host,
        smtp_port: notify.smtp_port ? Number(notify.smtp_port) : 0,
        smtp_username: notify.smtp_username,
        smtp_password: notify.smtp_password,
        smtp_from: notify.smtp_from,
      });
      addToast(t('通知配置保存成功'));
      loadNotifyConfig();
    } catch (err) {
      setError(err.message);
    } finally {
      setNotifySaving(false);
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

  const handleChange = (field, value) => {
    setLimits({ ...limits, [field]: value });
  };

  const handleSave = async () => {
    const payload = {};
    if (limits.daily_limit !== '') payload.daily_limit = Number(limits.daily_limit);
    if (limits.monthly_limit !== '') payload.monthly_limit = Number(limits.monthly_limit);
    if (limits.threshold !== '') payload.threshold = Number(limits.threshold) / 100;
    if (limits.api_timeout_secs !== '') payload.api_timeout_secs = Number(limits.api_timeout_secs);
    if (limits.max_retries !== '') payload.max_retries = Number(limits.max_retries);

    if (payload.daily_limit < 0 || payload.monthly_limit < 0 || (payload.threshold != null && (payload.threshold < 0 || payload.threshold > 1))) {
      setError(t('请输入有效值。日/月限额必须 >= 0。阈值必须在 0-100 之间。'));
      return;
    }
    if (payload.api_timeout_secs != null && payload.api_timeout_secs < 5) {
      setError(t('API 超时时间至少为 5 秒'));
      return;
    }
    if (payload.max_retries != null && (payload.max_retries < 0 || payload.max_retries > 10)) {
      setError(t('最大重试次数必须在 0-10 之间'));
      return;
    }

    setSaving(true);
    setError('');
    try {
      await api.updateLimits(payload);
      addToast(t('设置更新成功'));
    } catch (err) {
      setError(err.message);
    } finally {
      setSaving(false);
    }
  };

  if (loading) return <div className="loading">{t('加载设置')}</div>;

  return (
    <div>
      <div className="page-header">
        <h1>{t('系统设置')}</h1>
        <p>{t('配置使用限额、API 超时与重试策略')}</p>
      </div>

      {error && <div className="error-message">{error}</div>}

      <div className="card">
        <div className="card-header">
          <h2>{t('使用限额')}</h2>
        </div>
        <div className="card-body">
          <div className="settings-form">
            <div className="form-group">
              <label>{t('每日 Token 限额')}</label>
              <input className="form-input" type="number" min="0" placeholder={t('settingsPlaceholderDailyLimit')} value={limits.daily_limit} onChange={(e) => handleChange('daily_limit', e.target.value)} />
              <span className="form-hint">{t('每天允许的最大 Token 数。0 或空 = 无限制。')}</span>
            </div>
            <div className="form-group">
              <label>{t('每月 Token 限额')}</label>
              <input className="form-input" type="number" min="0" placeholder={t('settingsPlaceholderMonthlyLimit')} value={limits.monthly_limit} onChange={(e) => handleChange('monthly_limit', e.target.value)} />
              <span className="form-hint">{t('每月允许的最大 Token 数。0 或空 = 无限制。')}</span>
            </div>
            <div className="form-group">
              <label>{t('告警阈值 (%)')}</label>
              <input className="form-input" type="number" min="0" max="100" placeholder={t('settingsPlaceholderThreshold')} value={limits.threshold} onChange={(e) => handleChange('threshold', e.target.value)} />
              <span className="form-hint">{t('触发告警的限额使用百分比（0-100）。')}</span>
            </div>
          </div>
        </div>
      </div>

      <div className="card" style={{ marginTop: 16 }}>
        <div className="card-header">
          <h2>{t('API 配置')}</h2>
        </div>
        <div className="card-body">
          <div className="settings-form">
            <div className="form-group">
              <label>{t('API 超时时间 (秒)')}</label>
              <input className="form-input" type="number" min="5" max="300" placeholder={t('settingsPlaceholderApiTimeout')} value={limits.api_timeout_secs} onChange={(e) => handleChange('api_timeout_secs', e.target.value)} />
              <span className="form-hint">{t('向 Cloudflare API 发送请求的超时时间，默认 120 秒。')}</span>
            </div>
            <div className="form-group">
              <label>{t('最大重试次数')}</label>
              <input className="form-input" type="number" min="0" max="10" placeholder={t('settingsPlaceholderMaxRetries')} value={limits.max_retries} onChange={(e) => handleChange('max_retries', e.target.value)} />
              <span className="form-hint">{t('API 请求失败时的最大重试次数，0 表示不重试。')}</span>
            </div>
            <div className="settings-actions">
              <button className="btn btn-primary" onClick={handleSave} disabled={saving}>
                {saving ? t('保存中...') : t('保存更改')}
              </button>
            </div>
          </div>
        </div>
      </div>
      <div className="card" style={{ marginTop: 16 }}>
        <div className="card-header">
          <h2>{t('限流配置')}</h2>
        </div>
        <div className="card-body">
          {rlLoading ? (
            <div className="loading">{t('加载限流配置')}</div>
          ) : rlConfig ? (
            <div className="settings-form">
              <p style={{ fontSize: 13, color: 'var(--text-muted)', marginBottom: 16 }}>
                {t('配置多维度 RPM（每分钟请求数）/ TPM（每分钟 Token 数）限流。留空或 0 = 不限制。')}
              </p>
              <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fill, minmax(220px, 1fr))', gap: 16 }}>
                <div className="form-group">
                  <label>{t('每 Key RPM')}</label>
                  <input className="form-input" type="number" min="0" placeholder="0"
                    value={rlConfig.per_key_rpm ?? ''}
                    onChange={(e) => handleRlChange('per_key_rpm', e.target.value)} />
                  <span className="form-hint">{t('单个 API Key 每分钟最大请求数')}</span>
                </div>
                <div className="form-group">
                  <label>{t('每 Key TPM')}</label>
                  <input className="form-input" type="number" min="0" placeholder="0"
                    value={rlConfig.per_key_tpm ?? ''}
                    onChange={(e) => handleRlChange('per_key_tpm', e.target.value)} />
                  <span className="form-hint">{t('单个 API Key 每分钟最大 Token 数')}</span>
                </div>
                <div className="form-group">
                  <label>{t('每模型 RPM')}</label>
                  <input className="form-input" type="number" min="0" placeholder="0"
                    value={rlConfig.per_model_rpm ?? ''}
                    onChange={(e) => handleRlChange('per_model_rpm', e.target.value)} />
                  <span className="form-hint">{t('单个模型每分钟最大请求数')}</span>
                </div>
                <div className="form-group">
                  <label>{t('每模型 TPM')}</label>
                  <input className="form-input" type="number" min="0" placeholder="0"
                    value={rlConfig.per_model_tpm ?? ''}
                    onChange={(e) => handleRlChange('per_model_tpm', e.target.value)} />
                  <span className="form-hint">{t('单个模型每分钟最大 Token 数')}</span>
                </div>
                <div className="form-group">
                  <label>{t('每用户 RPM')}</label>
                  <input className="form-input" type="number" min="0" placeholder="0"
                    value={rlConfig.per_user_rpm ?? ''}
                    onChange={(e) => handleRlChange('per_user_rpm', e.target.value)} />
                  <span className="form-hint">{t('单个用户每分钟最大请求数')}</span>
                </div>
                <div className="form-group">
                  <label>{t('每用户 TPM')}</label>
                  <input className="form-input" type="number" min="0" placeholder="0"
                    value={rlConfig.per_user_tpm ?? ''}
                    onChange={(e) => handleRlChange('per_user_tpm', e.target.value)} />
                  <span className="form-hint">{t('单个用户每分钟最大 Token 数')}</span>
                </div>
                <div className="form-group">
                  <label>{t('每 IP RPM')}</label>
                  <input className="form-input" type="number" min="0" placeholder="0"
                    value={rlConfig.per_ip_rpm ?? ''}
                    onChange={(e) => handleRlChange('per_ip_rpm', e.target.value)} />
                  <span className="form-hint">{t('单个 IP 每分钟最大请求数')}</span>
                </div>
                <div className="form-group">
                  <label>{t('每 IP TPM')}</label>
                  <input className="form-input" type="number" min="0" placeholder="0"
                    value={rlConfig.per_ip_tpm ?? ''}
                    onChange={(e) => handleRlChange('per_ip_tpm', e.target.value)} />
                  <span className="form-hint">{t('单个 IP 每分钟最大 Token 数')}</span>
                </div>
                <div className="form-group">
                  <label>{t('全局 RPM')}</label>
                  <input className="form-input" type="number" min="0" placeholder="0"
                    value={rlConfig.global_rpm ?? ''}
                    onChange={(e) => handleRlChange('global_rpm', e.target.value)} />
                  <span className="form-hint">{t('全系统每分钟最大请求数')}</span>
                </div>
                <div className="form-group">
                  <label>{t('全局 TPM')}</label>
                  <input className="form-input" type="number" min="0" placeholder="0"
                    value={rlConfig.global_tpm ?? ''}
                    onChange={(e) => handleRlChange('global_tpm', e.target.value)} />
                  <span className="form-hint">{t('全系统每分钟最大 Token 数')}</span>
                </div>
              </div>
              <div className="settings-actions" style={{ marginTop: 16 }}>
                <button className="btn btn-primary" onClick={handleRlSave} disabled={rlSaving}>
                  {rlSaving ? t('保存中...') : t('保存限流配置')}
                </button>
              </div>
            </div>
          ) : (
            <div className="empty-state">
              <p>{t('限流配置未启用或加载失败')}</p>
            </div>
          )}
        </div>
      </div>

      {/* 通知配置 */}
      <div className="card" style={{ marginTop: 16 }}>
        <div className="card-header">
          <h2>{t('通知配置')}</h2>
        </div>
        <div className="card-body">
          {notifyLoading ? (
            <div className="loading">{t('加载通知配置')}</div>
          ) : notify ? (
            <div className="settings-form">
              <p style={{ fontSize: 13, color: 'var(--text-muted)', marginBottom: 16 }}>
                {t('配置 Telegram Bot 与 SMTP 邮件通知。事件触发时推送：充值成功 / 额度不足 / 渠道故障。')}
              </p>

              <div className="form-group">
                <label style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
                  <input
                    type="checkbox"
                    checked={notify.enabled ?? false}
                    onChange={(e) => handleNotifyChange('enabled', e.target.checked)}
                  />
                  {t('启用通知')}
                </label>
                <span className="form-hint">{t('总开关，关闭后不发送任何通知')}</span>
              </div>

              <h3 style={{ marginTop: 20, marginBottom: 12, fontSize: 15 }}>{t('Telegram 通知')}</h3>
              <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: 16 }}>
                <div className="form-group">
                  <label>Bot Token</label>
                  <input
                    className="form-input"
                    type="text"
                    placeholder="123456:ABC-DEF..."
                    value={notify.telegram_bot_token ?? ''}
                    onChange={(e) => handleNotifyChange('telegram_bot_token', e.target.value)}
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
                    onChange={(e) => handleNotifyChange('telegram_chat_id', e.target.value)}
                  />
                  <span className="form-hint">{t('群组/频道 Chat ID')}</span>
                </div>
              </div>
              <div className="settings-actions" style={{ marginTop: 8 }}>
                <button
                  className="btn btn-secondary"
                  onClick={handleTestTelegram}
                  disabled={testingTg}
                >
                  {testingTg ? t('发送中...') : t('测试 Telegram')}
                </button>
              </div>

              <h3 style={{ marginTop: 24, marginBottom: 12, fontSize: 15 }}>{t('SMTP 邮件通知')}</h3>
              <p style={{ fontSize: 12, color: 'var(--text-muted)', marginBottom: 12 }}>
                {t('注：当前为原生 TCP SMTP（AUTH LOGIN，明文），适用于本地邮件中继或内网 SMTP（端口 25）。TLS（465/587）后续支持。')}
              </p>
              <div style={{ display: 'grid', gridTemplateColumns: '2fr 1fr', gap: 16 }}>
                <div className="form-group">
                  <label>SMTP Host</label>
                  <input
                    className="form-input"
                    type="text"
                    placeholder="smtp.example.com"
                    value={notify.smtp_host ?? ''}
                    onChange={(e) => handleNotifyChange('smtp_host', e.target.value)}
                  />
                </div>
                <div className="form-group">
                  <label>SMTP Port</label>
                  <input
                    className="form-input"
                    type="number"
                    placeholder="25"
                    value={notify.smtp_port ?? ''}
                    onChange={(e) => handleNotifyChange('smtp_port', e.target.value)}
                  />
                </div>
              </div>
              <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: 16 }}>
                <div className="form-group">
                  <label>{t('用户名')}</label>
                  <input
                    className="form-input"
                    type="text"
                    value={notify.smtp_username ?? ''}
                    onChange={(e) => handleNotifyChange('smtp_username', e.target.value)}
                  />
                </div>
                <div className="form-group">
                  <label>{t('密码')}</label>
                  <input
                    className="form-input"
                    type="password"
                    placeholder={t('留空表示不修改')}
                    value={notify.smtp_password ?? ''}
                    onChange={(e) => handleNotifyChange('smtp_password', e.target.value)}
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
                  onChange={(e) => handleNotifyChange('smtp_from', e.target.value)}
                />
              </div>
              <div style={{ display: 'flex', gap: 12, alignItems: 'flex-end', flexWrap: 'wrap' }}>
                <div className="form-group" style={{ flex: 1, minWidth: 240 }}>
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
                  className="btn btn-secondary"
                  onClick={handleTestEmail}
                  disabled={testingEmail}
                >
                  {testingEmail ? t('发送中...') : t('测试邮件')}
                </button>
              </div>

              <div className="settings-actions" style={{ marginTop: 16 }}>
                <button className="btn btn-primary" onClick={handleNotifySave} disabled={notifySaving}>
                  {notifySaving ? t('保存中...') : t('保存通知配置')}
                </button>
              </div>
            </div>
          ) : (
            <div className="empty-state">
              <p>{t('通知配置加载失败')}</p>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
