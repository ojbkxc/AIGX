import { useState, useEffect } from 'react';
import { useTranslation } from 'react-i18next';
import { Copy } from 'lucide-react';
import { api } from '../api';
import { useToast } from '../components/Toast';
import { Card, Input } from '../components/ui';
import QRCode from 'react-qr-code';

interface Me {
  email?: string;
  username?: string;
  role?: string;
  quota?: number | null;
  used_quota?: number;
  created_at?: number;
  totp_enabled?: boolean;
}

export default function Profile(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();

  const [me, setMe] = useState<Me | null>(null);
  const [loading, setLoading] = useState(true);

  const [oldPw, setOldPw] = useState('');
  const [newPw, setNewPw] = useState('');
  const [confirmPw, setConfirmPw] = useState('');
  const [pwSaving, setPwSaving] = useState(false);
  const [pwError, setPwError] = useState('');

  // 2FA/TOTP 状态（P1）
  const [totpEnabled, setTotpEnabled] = useState(false);
  const [totpSetup, setTotpSetup] = useState<{ secret: string; otpauth_uri: string } | null>(null);
  const [totpCode, setTotpCode] = useState('');
  const [totpPw, setTotpPw] = useState('');
  const [totpBusy, setTotpBusy] = useState(false);
  const [totpError, setTotpError] = useState('');

  useEffect(() => {
    void load();
  }, []);

  const load = async () => {
    setLoading(true);
    try {
      const res = await api.getMe();
      const data = (res?.data ?? null) as Me | null;
      setMe(data);
      setTotpEnabled(Boolean(data?.totp_enabled));
    } catch {
      setMe(null);
    } finally {
      setLoading(false);
    }
  };

  const handleCopySecret = async (): Promise<void> => {
    if (!totpSetup?.secret) return;
    try {
      await navigator.clipboard.writeText(totpSetup.secret);
      addToast(t('已复制到剪贴板'));
    } catch {
      addToast(t('复制失败，请手动选择复制'), 'error');
    }
  };

  const handleChangePassword = async (): Promise<void> => {
    setPwError('');
    if (!oldPw) { setPwError(t('请输入当前密码')); return; }
    if (newPw.length < 6) { setPwError(t('新密码至少 6 位')); return; }
    if (newPw !== confirmPw) { setPwError(t('两次输入的新密码不一致')); return; }
    setPwSaving(true);
    try {
      await api.changePassword(oldPw, newPw);
      addToast(t('密码已修改，下次登录请使用新密码'));
      setOldPw(''); setNewPw(''); setConfirmPw('');
    } catch (err) {
      setPwError(err instanceof Error ? err.message : String(err));
    } finally {
      setPwSaving(false);
    }
  };

  const fmtQuota = (q: number | undefined | null): string => {
    // isFinite 兜底：后端返回异常值（字符串/null）时按 0 展示
    const n = Number.isFinite(Number(q)) ? Number(q) : 0;
    if (n >= 1_000_000) return (n / 1_000_000).toFixed(2) + 'M';
    if (n >= 1_000) return (n / 1_000).toFixed(2) + 'K';
    return String(n);
  };

  // ── 2FA/TOTP ──────────────────────────────────────────────

  // 生成新 secret（不落库）：显示 secret + otpauth URI 供认证器录入
  const handleTotpSetup = async (): Promise<void> => {
    setTotpError('');
    setTotpBusy(true);
    try {
      const res = await api.totpSetup();
      const data = (res?.data ?? {}) as { secret?: string; otpauth_uri?: string; otpauth_url?: string };
      if (data.secret) {
        setTotpSetup({ secret: data.secret, otpauth_uri: data.otpauth_uri || data.otpauth_url || '' });
        setTotpCode('');
      } else {
        setTotpError(t('生成密钥失败'));
      }
    } catch (err) {
      setTotpError(err instanceof Error ? err.message : String(err));
    } finally {
      setTotpBusy(false);
    }
  };

  // 提交一次验证码启用（secret 落库）
  const handleTotpEnable = async (): Promise<void> => {
    setTotpError('');
    if (!totpCode.trim()) { setTotpError(t('请输入验证码')); return; }
    setTotpBusy(true);
    try {
      await api.totpEnable(totpCode.trim());
      setTotpEnabled(true);
      setTotpSetup(null);
      setTotpCode('');
      addToast(t('两步验证已启用'));
    } catch (err) {
      setTotpError(err instanceof Error ? err.message : String(err));
    } finally {
      setTotpBusy(false);
    }
  };

  // 停用（需当前密码）
  const handleTotpDisable = async (): Promise<void> => {
    setTotpError('');
    if (!totpPw) { setTotpError(t('请输入当前密码')); return; }
    setTotpBusy(true);
    try {
      await api.totpDisable(totpPw);
      setTotpEnabled(false);
      setTotpPw('');
      addToast(t('两步验证已停用'));
    } catch (err) {
      setTotpError(err instanceof Error ? err.message : String(err));
    } finally {
      setTotpBusy(false);
    }
  };

  return (
    <div>
      <div className="page-header">
        <h1>{t('个人中心')}</h1>
        <p>{t('查看账户信息与安全设置')}</p>
      </div>

      <Card title={t('账户信息')}>
        {loading ? (
          <div className="loading">{t('加载中…')}</div>
        ) : me ? (
          <div className="settings-form">
            <div className="form-group">
              <label>{t('邮箱')}</label>
              <Input value={me.email || ''} disabled />
            </div>
            <div className="form-group">
              <label>{t('用户名')}</label>
              <Input value={me.username || '—'} disabled />
            </div>
            <div className="form-group">
              <label>{t('角色')}</label>
              <Input value={me.role === 'admin' ? t('管理员') : t('普通用户')} disabled />
            </div>
            <div className="form-group">
              <label>{t('配额')}</label>
              <Input
                value={`${t('已用')} ${fmtQuota(me.used_quota)} / ${t('总量')} ${me.quota != null ? fmtQuota(me.quota) : '∞'} / ${t('剩余配额')} ${me.quota != null ? fmtQuota(me.quota - Number(me.used_quota || 0)) : '∞'}`}
                disabled
              />
            </div>
            <div className="form-group">
              <label>{t('注册时间')}</label>
              <Input
                value={me.created_at ? new Date(me.created_at > 1e12 ? me.created_at : me.created_at * 1000).toLocaleString() : '—'}
                disabled
              />
            </div>
          </div>
        ) : (
          <div className="empty-state"><p>{t('无法加载账户信息')}</p></div>
        )}
      </Card>

      <Card title={t('修改密码')}>
        <form className="settings-form" onSubmit={(e) => { e.preventDefault(); void handleChangePassword(); }}>
          <div className="form-group">
            <label>{t('当前密码')}</label>
            <input className="form-input" type="password" value={oldPw}
              onChange={(e) => setOldPw(e.target.value)} autoComplete="current-password" />
          </div>
          <div className="form-group">
            <label>{t('新密码')}</label>
            <input className="form-input" type="password" value={newPw}
              onChange={(e) => setNewPw(e.target.value)} autoComplete="new-password" />
            <span className="form-hint">{t('至少 6 位，建议混合字母与数字')}</span>
          </div>
          <div className="form-group">
            <label>{t('确认新密码')}</label>
            <input className="form-input" type="password" value={confirmPw}
              onChange={(e) => setConfirmPw(e.target.value)} autoComplete="new-password" />
          </div>
          {pwError && <div className="error-message">{pwError}</div>}
          <div className="settings-actions">
            <button type="submit" className="btn btn-primary" disabled={pwSaving}>
              {pwSaving ? t('修改中...') : t('修改密码')}
            </button>
          </div>
        </form>
      </Card>

      <Card title={t('两步验证（TOTP）')}>
        <div className="settings-form">
          <div className="form-group">
            <label>{t('状态')}</label>
            <Input value={totpEnabled ? t('已启用') : t('未启用')} disabled />
            <span className="form-hint">
              {totpEnabled
                ? t('登录时需输入认证器中的 6 位验证码')
                : t('启用后登录需密码 + 动态验证码双重确认')}
            </span>
          </div>

          {!totpEnabled && !totpSetup && (
            <div className="settings-actions">
              <button className="btn btn-primary" onClick={() => void handleTotpSetup()} disabled={totpBusy}>
                {totpBusy ? t('生成中...') : t('开始设置')}
              </button>
            </div>
          )}

          {totpSetup && (
            <>
              <div className="form-group">
                <label>{t('密钥（Base32）')}</label>
                <div style={{ display: 'flex', gap: 8 }}>
                  <Input value={totpSetup.secret} disabled />
                  <button
                    type="button"
                    className="btn btn-outline"
                    onClick={() => void handleCopySecret()}
                    title={t('复制')}
                    aria-label={t('复制')}
                  >
                    <Copy size={14} />
                  </button>
                </div>
                <span className="form-hint">{t('在认证器中选择「手动录入」，粘贴上方密钥')}</span>
              </div>
              {totpSetup.otpauth_uri && (
                <div className="form-group">
                  <label>{t('扫码录入')}</label>
                  <div style={{
                    display: 'inline-flex', padding: 12, background: '#ffffff',
                    border: '1px solid var(--border-color)', borderRadius: 10,
                    boxShadow: '0 1px 3px rgba(0, 0, 0, 0.06)', margin: '6px 0 2px',
                  }}>
                    <QRCode
                      value={totpSetup.otpauth_uri}
                      size={176}
                      bgColor="#ffffff"
                      fgColor="#111827"
                      level="M"
                      aria-label={t('两步验证二维码')}
                    />
                  </div>
                  <span className="form-hint">
                    {t('用认证器扫码录入（5 分钟内有效），或手动输入上方密钥')}
                  </span>
                </div>
              )}
              <div className="form-group">
                <label>{t('验证码')}</label>
                <input className="form-input" type="text" value={totpCode}
                  onChange={(e) => setTotpCode(e.target.value)} maxLength={6}
                  inputMode="numeric" autoComplete="one-time-code" />
                <span className="form-hint">{t('输入认证器显示的 6 位验证码完成启用')}</span>
              </div>
              {totpError && <div className="error-message">{totpError}</div>}
              <div className="settings-actions">
                <button className="btn btn-primary" onClick={() => void handleTotpEnable()} disabled={totpBusy}>
                  {totpBusy ? t('验证中...') : t('确认启用')}
                </button>
                <button className="btn btn-outline" onClick={() => { setTotpSetup(null); setTotpError(''); }} disabled={totpBusy}>
                  {t('取消')}
                </button>
              </div>
            </>
          )}

          {totpEnabled && (
            <>
              <div className="form-group">
                <label>{t('当前密码')}</label>
                <input className="form-input" type="password" value={totpPw}
                  onChange={(e) => setTotpPw(e.target.value)} autoComplete="current-password" />
                <span className="form-hint">{t('停用两步验证需确认密码')}</span>
              </div>
              {totpError && <div className="error-message">{totpError}</div>}
              <div className="settings-actions">
                <button className="btn btn-danger" onClick={() => void handleTotpDisable()} disabled={totpBusy}>
                  {totpBusy ? t('停用中...') : t('停用两步验证')}
                </button>
              </div>
            </>
          )}
        </div>
      </Card>
    </div>
  );
}
