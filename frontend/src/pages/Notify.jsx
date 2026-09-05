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
  const [testingSlack, setTestingSlack] = useState(false);
  const [testingWebhook, setTestingWebhook] = useState(false);

  // 告警规则（批次5）
  const [rules, setRules] = useState([]);
  const [activeAlerts, setActiveAlerts] = useState([]);
  const [alertHistory, setAlertHistory] = useState([]);
  const [rulesSaving, setRulesSaving] = useState(false);
  const [testingAlertKind, setTestingAlertKind] = useState('memory_high');

  useEffect(() => { loadConfig(); loadAlerts(); }, []);

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
        smtp_starttls: notify.smtp_starttls ?? false,
        webhook_url: notify.webhook_url ?? '',
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
      const slackUrl = (notify.slack_webhook_url || '').trim();
      if (slackUrl && !slackUrl.includes('***')) {
        payload.slack_webhook_url = slackUrl;
      }
      const webhookSecret = (notify.webhook_secret || '').trim();
      if (webhookSecret && !webhookSecret.includes('***')) {
        payload.webhook_secret = webhookSecret;
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

  const handleTestSlack = async () => {
    setTestingSlack(true);
    setError('');
    try {
      const res = await api.testSlack();
      addToast(res.data || t('Slack 测试消息已发送'));
    } catch (err) {
      setError(err.message);
    } finally {
      setTestingSlack(false);
    }
  };

  const handleTestWebhook = async () => {
    setTestingWebhook(true);
    setError('');
    try {
      const res = await api.testWebhook('AIGX 测试告警');
      addToast(res.data || t('Webhook 测试消息已发送'));
    } catch (err) {
      setError(err.message);
    } finally {
      setTestingWebhook(false);
    }
  };

  // ── 告警规则 ──────────────────────────────────────────────
  const loadAlerts = async () => {
    try {
      const [rulesRes, activeRes, historyRes] = await Promise.all([
        api.getAlertRules(),
        api.getActiveAlerts(),
        api.getAlertHistory(50),
      ]);
      setRules((rulesRes.data || []));
      setActiveAlerts(activeRes.data || []);
      setAlertHistory((historyRes.data?.items) || (historyRes.data) || []);
    } catch {
      // 告警 API 失败不阻塞通知配置页
    }
  };

  const updateRule = (idx, field, value) => {
    setRules((prev) => prev.map((r, i) => (i === idx ? { ...r, [field]: value } : r)));
  };

  const handleSaveRules = async () => {
    setRulesSaving(true);
    setError('');
    try {
      await api.updateAlertRules(rules);
      addToast(t('告警规则已保存'));
      loadAlerts();
    } catch (err) {
      setError(err.message);
    } finally {
      setRulesSaving(false);
    }
  };

  const handleTestAlert = async () => {
    setError('');
    try {
      const res = await api.testAlert(testingAlertKind, 99);
      const d = res.data || {};
      addToast(d.triggered ? `🚨 ${d.message}` : t('未触发（低于阈值或静默期）'));
      loadAlerts();
    } catch (err) {
      setError(err.message);
    }
  };

  if (loading) return <div className="loading">{t('加载通知配置')}</div>;

  return (
    <div className="notify-shell">
      {/* PageIntro 标题区 */}
      <div className="page-header">
        <div>
          <h1>{t('通知设置')}</h1>
          <p>{t('配置 Telegram Bot、SMTP 邮件、Slack 与 Webhook 通知。事件触发时推送：充值成功 / 额度不足 / 渠道故障 / 告警规则。')}</p>
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
              {t('原生 TCP SMTP（AUTH LOGIN）。明文适用于本地中继（25 端口）；勾选 STARTTLS 用于 TLS 中继场景（465/587 由中继终结 TLS）。')}
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
              <div className="form-group" style={{ flex: '1 1 120px', display: 'flex', alignItems: 'flex-end' }}>
                <label className="notify-toggle" style={{ marginBottom: '8px' }}>
                  <input
                    type="checkbox"
                    checked={notify.smtp_starttls ?? false}
                    onChange={(e) => handleChange('smtp_starttls', e.target.checked)}
                  />
                  <span className="toggle-label">STARTTLS</span>
                </label>
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

        {/* Slack 通知（批次5） */}
        <div className="card">
          <div className="card-header">
            <h2>{t('Slack 通知')}</h2>
            {notify.slack_ready && (
              <span className="badge badge-success">{t('已就绪')}</span>
            )}
          </div>
          <div className="card-body">
            <div className="form-group">
              <label>Webhook URL</label>
              <input
                className="form-input"
                type="text"
                placeholder="https://hooks.slack.com/services/T000/B000/XXXX"
                value={notify.slack_webhook_url ?? ''}
                onChange={(e) => handleChange('slack_webhook_url', e.target.value)}
              />
              <span className="form-hint">{t('Slack App Incoming Webhook 地址')}</span>
            </div>
            <div className="notify-actions">
              <button
                className="btn btn-outline"
                onClick={handleTestSlack}
                disabled={testingSlack || !notify.slack_ready}
              >
                {testingSlack ? t('发送中...') : t('测试 Slack')}
              </button>
            </div>
          </div>
        </div>

        {/* 通用 Webhook 通知（批次5） */}
        <div className="card">
          <div className="card-header">
            <h2>{t('通用 Webhook')}</h2>
            {notify.webhook_ready && (
              <span className="badge badge-success">{t('已就绪')}</span>
            )}
          </div>
          <div className="card-body">
            <p className="notify-note">
              {t('告警触发时 POST 结构化 JSON（含 HMAC-SHA256 签名头 X-AIGX-Signature）。')}
            </p>
            <div className="notify-form-grid">
              <div className="form-group">
                <label>Webhook URL</label>
                <input
                  className="form-input"
                  type="text"
                  placeholder="https://your-service.example.com/hook"
                  value={notify.webhook_url ?? ''}
                  onChange={(e) => handleChange('webhook_url', e.target.value)}
                />
              </div>
              <div className="form-group">
                <label>{t('签名密钥（可选）')}</label>
                <input
                  className="form-input"
                  type="password"
                  placeholder={t('留空表示不修改')}
                  value={notify.webhook_secret ?? ''}
                  onChange={(e) => handleChange('webhook_secret', e.target.value)}
                />
              </div>
            </div>
            <div className="notify-actions">
              <button
                className="btn btn-outline"
                onClick={handleTestWebhook}
                disabled={testingWebhook || !notify.webhook_ready}
              >
                {testingWebhook ? t('发送中...') : t('测试 Webhook')}
              </button>
            </div>
          </div>
        </div>

        {/* 告警规则（批次5） */}
        <div className="card">
          <div className="card-header">
            <h2>{t('告警规则')}</h2>
            <span className="badge badge-neutral">{t('周期巡检 60s')}</span>
          </div>
          <div className="card-body">
            <p className="notify-note">
              {t('后台每 60 秒巡检一次：渠道断路器打开 / 渠道延迟 EMA / 进程内存。达到阈值触发告警并分发到已配置的通知渠道。')}
            </p>
            {rules.length === 0 ? (
              <p className="notify-note">{t('暂无规则')}</p>
            ) : (
              <table className="table" style={{ width: '100%' }}>
                <thead>
                  <tr>
                    <th>{t('规则')}</th>
                    <th>{t('类型')}</th>
                    <th>{t('阈值')}</th>
                    <th>{t('静默期(秒)')}</th>
                    <th>{t('级别')}</th>
                    <th>{t('启用')}</th>
                  </tr>
                </thead>
                <tbody>
                  {rules.map((r, i) => (
                    <tr key={r.name}>
                      <td>{r.name}</td>
                      <td style={{ fontSize: 11 }}>{(r.kind?.kind || '').replace(/_/g, ' ')}</td>
                      <td>
                        <input
                          className="form-input"
                          type="number"
                          style={{ width: 90 }}
                          value={r.threshold}
                          onChange={(e) => updateRule(i, 'threshold', Number(e.target.value))}
                        />
                      </td>
                      <td>
                        <input
                          className="form-input"
                          type="number"
                          style={{ width: 90 }}
                          value={r.silence_period_secs}
                          onChange={(e) => updateRule(i, 'silence_period_secs', Number(e.target.value))}
                        />
                      </td>
                      <td>
                        <select
                          className="form-input"
                          style={{ width: 100 }}
                          value={r.level}
                          onChange={(e) => updateRule(i, 'level', e.target.value)}
                        >
                          <option value="info">Info</option>
                          <option value="warning">Warning</option>
                          <option value="critical">Critical</option>
                        </select>
                      </td>
                      <td>
                        <input
                          type="checkbox"
                          checked={r.enabled}
                          onChange={(e) => updateRule(i, 'enabled', e.target.checked)}
                        />
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            )}
            <div className="notify-actions" style={{ display: 'flex', gap: 10, marginTop: 12 }}>
              <button className="btn btn-primary" onClick={handleSaveRules} disabled={rulesSaving}>
                {rulesSaving ? t('保存中...') : t('保存告警规则')}
              </button>
              <select
                className="form-input"
                style={{ width: 180 }}
                value={testingAlertKind}
                onChange={(e) => setTestingAlertKind(e.target.value)}
              >
                <option value="memory_high">MemoryHigh</option>
                <option value="channel_failure">ChannelFailure</option>
                <option value="channel_high_latency">ChannelHighLatency</option>
                <option value="channel_quota_low">ChannelQuotaLow</option>
                <option value="user_quota_exhausted">UserQuotaExhausted</option>
                <option value="queue_backlog">QueueBacklog</option>
                <option value="abnormal_traffic">AbnormalTraffic</option>
                <option value="cost_anomaly">CostAnomaly</option>
              </select>
              <button className="btn btn-outline" onClick={handleTestAlert}>
                {t('触发测试告警')}
              </button>
            </div>
            {activeAlerts.length > 0 && (
              <div style={{ marginTop: 14 }}>
                <h3 style={{ fontSize: 13, marginBottom: 6 }}>{t('活跃告警')}</h3>
                {activeAlerts.map((a) => (
                  <div key={a.id} className="notify-note" style={{ display: 'flex', gap: 8, alignItems: 'center' }}>
                    <span className={`badge ${a.level === 'critical' ? 'badge-danger' : a.level === 'warning' ? 'badge-warning' : 'badge-neutral'}`}>
                      {a.level}
                    </span>
                    <span>{a.message}</span>
                    <span style={{ color: 'var(--text-muted)', fontSize: 11 }}>×{a.trigger_count}</span>
                  </div>
                ))}
              </div>
            )}
            {alertHistory.length > 0 && (
              <div style={{ marginTop: 14 }}>
                <h3 style={{ fontSize: 13, marginBottom: 6 }}>{t('告警历史')} ({alertHistory.length})</h3>
                <div style={{ maxHeight: 220, overflowY: 'auto', display: 'flex', flexDirection: 'column', gap: 6 }}>
                  {alertHistory.map((h) => (
                    <div key={h.id} className="notify-note" style={{ display: 'flex', gap: 8, alignItems: 'center', fontSize: 12 }}>
                      <span className={`badge ${h.level === 'critical' ? 'badge-danger' : h.level === 'warning' ? 'badge-warning' : 'badge-neutral'}`}>
                        {h.level}
                      </span>
                      <span style={{ flex: 1 }}>{h.message}</span>
                      <span style={{ color: 'var(--text-muted)', fontSize: 11 }}>
                        {new Date(h.triggered_at * 1000).toLocaleString()}
                      </span>
                    </div>
                  ))}
                </div>
              </div>
            )}
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