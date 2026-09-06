import { useState, useEffect } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api';
import { useToast } from '../components/Toast';
import { Card, Input } from '../components/ui';

interface Me {
  email?: string;
  username?: string;
  role?: string;
  quota?: number | null;
  used_quota?: number;
  created_at?: number;
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

  useEffect(() => {
    void load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const load = async () => {
    setLoading(true);
    try {
      const res = await api.getMe();
      setMe(res?.data || res || null);
    } catch {
      setMe(null);
    } finally {
      setLoading(false);
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
    const n = Number(q || 0);
    if (n >= 1_000_000) return (n / 1_000_000).toFixed(2) + 'M';
    if (n >= 1_000) return (n / 1_000).toFixed(2) + 'K';
    return String(n);
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
                value={`${fmtQuota(me.used_quota)} / ${me.quota != null ? fmtQuota(me.quota) : '∞'}`}
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
        <div className="settings-form">
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
            <button className="btn btn-primary" onClick={() => void handleChangePassword()} disabled={pwSaving}>
              {pwSaving ? t('修改中...') : t('修改密码')}
            </button>
          </div>
        </div>
      </Card>
    </div>
  );
}
