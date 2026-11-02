import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api';
import { useToast } from '../components/Toast';

/**
 * 安全策略（系统设置 → 安全）
 *
 * 对照 Agent2API「设置 → 安全」，把原先硬编码在登录 handler 里的三个门槛
 * 抽成可配项：
 * - 连续失败锁定：同 IP 连续失败达阈值即锁定（默认开 / 5 次）
 * - 锁定时长：默认 5 分钟
 * - 每分钟登录尝试上限：默认 10 次
 * - 机器人校验：登录/注册/验证码登录前的 ALTCHA 工作量证明（默认开）
 *
 * 生效时机：阈值 / 上限 / 开关走进程级快照，**保存即生效**；
 * 锁定时长是失败计数缓存的 TTL（建缓存时固定），**改后需重启服务生效**
 * —— 与网关监听地址/端口同口径。
 */
export default function SecurityPolicyPanel(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState('');
  const [lockoutEnabled, setLockoutEnabled] = useState(true);
  const [failThreshold, setFailThreshold] = useState('5');
  const [lockMinutes, setLockMinutes] = useState('5');
  const [rateLimit, setRateLimit] = useState('10');
  const [captchaEnabled, setCaptchaEnabled] = useState(true);

  useEffect(() => {
    void (async () => {
      try {
        const res = await api.getSecurityPolicy();
        const d = res?.data;
        if (!d) return;
        setLockoutEnabled(Boolean(d.lockoutEnabled));
        setFailThreshold(String(d.failThreshold ?? 5));
        setLockMinutes(String(d.lockMinutes ?? 5));
        setRateLimit(String(d.rateLimitPerMinute ?? 10));
        setCaptchaEnabled(d.captchaEnabled !== false);
      } catch (err) {
        setError(err instanceof Error ? err.message : String(err));
      } finally {
        setLoading(false);
      }
    })();
  }, []);

  const save = async (): Promise<void> => {
    setSaving(true);
    setError('');
    try {
      const failVal = Number(failThreshold);
      const lockVal = Number(lockMinutes);
      const rateVal = Number(rateLimit);
      if (!Number.isInteger(failVal) || failVal < 1 || failVal > 100) {
        setError(t('连续失败阈值必须在 1-100 之间'));
        return;
      }
      if (!Number.isInteger(lockVal) || lockVal < 1 || lockVal > 1440) {
        setError(t('锁定时长必须在 1-1440 分钟之间'));
        return;
      }
      if (!Number.isInteger(rateVal) || rateVal < 1 || rateVal > 1000) {
        setError(t('每分钟登录上限必须在 1-1000 之间'));
        return;
      }
      const res = await api.saveSecurityPolicy({
        lockout_enabled: lockoutEnabled,
        fail_threshold: failVal,
        lock_minutes: lockVal,
        rate_limit_per_minute: rateVal,
        captcha_enabled: captchaEnabled,
      });
      const d = res?.data;
      if (d) {
        setLockoutEnabled(Boolean(d.lockoutEnabled));
        setFailThreshold(String(d.failThreshold ?? failVal));
        setLockMinutes(String(d.lockMinutes ?? lockVal));
        setRateLimit(String(d.rateLimitPerMinute ?? rateVal));
        setCaptchaEnabled(d.captchaEnabled !== false);
      }
      addToast(t('安全策略已保存（阈值/上限/开关立即生效，锁定时长需重启服务）'));
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  };

  if (loading) {
    return <div className="loading">{t('加载中')}</div>;
  }

  return (
    <div className="card">
      <div className="card-header">
        <h2>{t('安全策略')}</h2>
        <p className="card-subtitle">
          {t('登录失败锁定、登录限流与人机校验的门槛；阈值/上限/开关保存即生效，锁定时长改后需重启服务。')}
        </p>
      </div>
      <div className="card-body">
        {error && <div className="error-message">{error}</div>}
        <div className="settings-form">
          <div className="form-group">
            <label style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
              <input
                type="checkbox"
                checked={lockoutEnabled}
                onChange={(e) => setLockoutEnabled(e.target.checked)}
              />
              <span>{t('启用连续登录失败锁定')}</span>
            </label>
            <div className="form-hint">
              {t('关闭后仅保留每分钟登录次数限流，不再按连续失败锁定来源 IP。')}
            </div>
          </div>

          <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr 1fr', gap: 12 }}>
            <div className="form-group">
              <label>{t('连续失败阈值')}</label>
              <input
                className="form-input"
                type="number"
                min={1}
                max={100}
                value={failThreshold}
                disabled={!lockoutEnabled}
                onChange={(e) => setFailThreshold(e.target.value)}
              />
              <div className="form-hint">{t('同 IP 连续失败达该次数即锁定（1-100）。')}</div>
            </div>
            <div className="form-group">
              <label>{t('锁定时长（分钟）')}</label>
              <input
                className="form-input"
                type="number"
                min={1}
                max={1440}
                value={lockMinutes}
                disabled={!lockoutEnabled}
                onChange={(e) => setLockMinutes(e.target.value)}
              />
              <div className="form-hint">{t('1-1440 分钟；改后需重启服务生效。')}</div>
            </div>
            <div className="form-group">
              <label>{t('每分钟登录上限')}</label>
              <input
                className="form-input"
                type="number"
                min={1}
                max={1000}
                value={rateLimit}
                onChange={(e) => setRateLimit(e.target.value)}
              />
              <div className="form-hint">{t('同一 IP 每分钟允许的登录尝试次数（1-1000）。')}</div>
            </div>
          </div>

          <div className="form-group">
            <label style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
              <input
                type="checkbox"
                checked={captchaEnabled}
                onChange={(e) => setCaptchaEnabled(e.target.checked)}
              />
              <span>{t('启用机器人校验（无感人机验证）')}</span>
            </label>
            <div className="form-hint">
              {t('开启后登录、注册、邮箱验证码登录前需完成 ALTCHA 工作量证明；真人不需操作，脚本会被拦在密码校验之前。')}
            </div>
          </div>

          <div className="settings-actions">
            <button className="btn btn-primary" onClick={() => void save()} disabled={saving}>
              {saving ? t('保存中...') : t('保存更改')}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
