import { useState, useEffect, useRef, type ChangeEvent, type FormEvent } from 'react';
import { useNavigate, Link } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import { Eye, EyeOff } from 'lucide-react';
import { api } from '../api';
import { cycleTheme } from '../lib/theme';

type UsernameStatus = 'idle' | 'checking' | 'available' | 'taken' | 'error';

interface UsernameCheckResponse {
  available?: boolean;
  exists?: boolean;
  data?: { available?: boolean; exists?: boolean };
}

interface RegisterResponse {
  success?: boolean;
  error?: string;
}

export default function Register() {
  const [email, setEmail] = useState('');
  const [username, setUsername] = useState('');
  const [password, setPassword] = useState('');
  const [confirmPassword, setConfirmPassword] = useState('');
  const [error, setError] = useState('');
  const [loading, setLoading] = useState(false);
  const [showPassword, setShowPassword] = useState(false);
  const navigate = useNavigate();
  const { t } = useTranslation();

  // 邀请码：从 ?aff= 或 ?aff_code= 查询参数读入（new-api 前端从 localStorage 读）
  const [affCode, setAffCode] = useState('');
  useEffect(() => {
    const params = new URLSearchParams(window.location.search);
    const aff = params.get('aff') || params.get('aff_code') || '';
    if (aff) setAffCode(aff);
  }, []);

  // ── 用户名实时可用性检查（防抖 300ms）──
  // 状态：'idle' 未检查 / 'checking' 检查中 / 'available' 可用 / 'taken' 已占用 / 'error' 检查失败
  const [usernameStatus, setUsernameStatus] = useState<UsernameStatus>('idle');
  const usernameTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const usernameCheckedRef = useRef('');

  // 防抖检查用户名是否可用，300ms 内若再次输入则取消上次未发出的请求
  const checkUsernameAvailability = (value: string) => {
    if (usernameTimerRef.current) {
      clearTimeout(usernameTimerRef.current);
    }
    if (!value.trim()) {
      setUsernameStatus('idle');
      return;
    }
    setUsernameStatus('checking');
    usernameTimerRef.current = setTimeout(async () => {
      try {
        const res = (await api.checkUsername(value.trim())) as UsernameCheckResponse;
        // 后端返回 { available: true/false } 或 { data: { available: ... } }
        const data = res?.data ?? res;
        const available = data?.available ?? !data?.exists;
        setUsernameStatus(available ? 'available' : 'taken');
        usernameCheckedRef.current = value.trim();
      } catch {
        setUsernameStatus('error');
      }
    }, 300);
  };

  const handleUsernameChange = (e: ChangeEvent<HTMLInputElement>) => {
    const value = e.target.value;
    setUsername(value);
    checkUsernameAvailability(value);
  };

  // 渲染用户名输入框右侧的状态提示
  const renderUsernameHint = () => {
    if (!username.trim() || usernameStatus === 'idle') {
      return null;
    }
    if (usernameStatus === 'checking') {
      return (
        <span style={{ fontSize: 12, color: 'var(--text-muted)', marginLeft: 6 }}>
          {t('检查中...')}
        </span>
      );
    }
    if (usernameStatus === 'available') {
      return (
        <span style={{ fontSize: 12, color: 'rgb(34,197,94)', marginLeft: 6 }}>
          ✓ {t('可用')}
        </span>
      );
    }
    if (usernameStatus === 'taken') {
      return (
        <span style={{ fontSize: 12, color: 'rgb(239,68,68)', marginLeft: 6 }}>
          ✗ {t('已占用')}
        </span>
      );
    }
    return (
      <span style={{ fontSize: 12, color: 'var(--text-muted)', marginLeft: 6 }}>
        {t('检查失败')}
      </span>
    );
  };

  // 组件卸载时清理防抖定时器
  useEffect(() => {
    return () => {
      if (usernameTimerRef.current) clearTimeout(usernameTimerRef.current);
    };
  }, []);

  const handleSubmit = async (e: FormEvent<HTMLFormElement>) => {
    e.preventDefault();
    setError('');

    if (!email || !password) {
      setError(t('请填写邮箱和密码'));
      return;
    }
    if (password.length < 6) {
      setError(t('密码长度至少6位'));
      return;
    }
    if (password !== confirmPassword) {
      setError(t('两次密码输入不一致'));
      return;
    }

    setLoading(true);
    try {
      const res = (await api.register(email, password, username || undefined, affCode || undefined)) as RegisterResponse;
      if (res.success) {
        // 注册成功后自动跳转登录页
        navigate('/login', { state: { registered: true, email } });
      } else {
        setError(res.error || t('注册失败'));
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : t('注册失败'));
    } finally {
      setLoading(false);
    }
  };

  const toggleTheme = (): void => {
    void cycleTheme();
  };

  return (
    <div style={{
      minHeight: '100vh',
      display: 'flex',
      alignItems: 'center',
      justifyContent: 'center',
      background: 'var(--bg-color)',
      padding: '16px',
      position: 'relative',
      overflow: 'hidden',
    }}>

      <button
        onClick={toggleTheme}
        style={{
          position: 'fixed',
          top: '20px',
          right: '20px',
          width: '44px',
          height: '44px',
          borderRadius: '12px',
          background: 'var(--card-bg)',
          border: '1px solid var(--border-color)',
          color: 'var(--text-main)',
          cursor: 'pointer',
          display: 'flex',
          alignItems: 'center',
          justifyContent: 'center',
          boxShadow: 'var(--card-shadow)',
          backdropFilter: 'blur(var(--glass-blur))',
          zIndex: 1000,
          transition: 'all 0.3s cubic-bezier(0.16, 1, 0.3, 1)',
        }}
        title={t('切换主题')}
      >
        <svg xmlns="http://www.w3.org/2000/svg" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth="2" style={{ width: '20px', height: '20px' }}>
          <path d="M12 3a6 6 0 0 0 9 9 9 9 0 1 1-9-9Z" />
        </svg>
      </button>

      <div style={{
        background: 'var(--card-bg)',
        border: '1px solid var(--border-color)',
        borderRadius: '16px',
        padding: '32px',
        width: '100%',
        maxWidth: '380px',
        boxShadow: 'var(--card-shadow)',
        backdropFilter: 'blur(var(--glass-blur))',
        WebkitBackdropFilter: 'blur(var(--glass-blur))',
        animation: 'fadeIn 0.25s ease',
      }}>
        <div style={{ textAlign: 'center', marginBottom: '22px' }}>
          <div style={{
            width: '44px',
            height: '44px',
            display: 'flex',
            alignItems: 'center',
            justifyContent: 'center',
            margin: '0 auto 14px',
          }}>
            <img src="/logo.png" alt="AIGX" width={44} height={44} />
          </div>
          <h1 style={{ fontSize: '20px', fontWeight: 700, color: 'var(--text-main)', marginBottom: '4px', fontFamily: "'Inter', sans-serif" }}>
            {t('创建账号')}
          </h1>
          <p style={{ fontSize: '13px', color: 'var(--text-muted)' }}>
            {t('注册 AIGX Gateway')}
          </p>
        </div>

        {error && (
          <div className="error-message">{error}</div>
        )}

        <form onSubmit={handleSubmit} style={{ display: 'flex', flexDirection: 'column', gap: '4px' }}>
          <div className="form-group">
            <label htmlFor="email">{t('邮箱 *')}</label>
            <input
              id="email"
              type="email"
              className="form-input"
              placeholder={t('请输入邮箱')}
              value={email}
              onChange={(e) => setEmail(e.target.value)}
              autoFocus
              disabled={loading}
            />
          </div>
          <div className="form-group">
            <label htmlFor="username">{t('昵称（可选）')}</label>
            <input
              id="username"
              type="text"
              className="form-input"
              placeholder={t('设置一个昵称（选填）')}
              value={username}
              onChange={handleUsernameChange}
              disabled={loading}
            />
            {renderUsernameHint()}
          </div>
          <div className="form-group">
            <label htmlFor="password">{t('密码 *')}</label>
            <div className="password-input-wrap">
              <input
                id="password"
                type={showPassword ? 'text' : 'password'}
                className="form-input password-input"
                placeholder={t('至少6位密码')}
                autoComplete="new-password"
                value={password}
                onChange={(e) => setPassword(e.target.value)}
                disabled={loading}
              />
              <button
                type="button"
                className="password-input-toggle"
                tabIndex={-1}
                onClick={() => setShowPassword((v) => !v)}
                aria-label={showPassword ? t('隐藏密码') : t('显示密码')}
                title={showPassword ? t('隐藏密码') : t('显示密码')}
              >
                {showPassword ? <EyeOff size={14} /> : <Eye size={14} />}
              </button>
            </div>
          </div>
          <div className="form-group">
            <label htmlFor="confirmPassword">{t('确认密码 *')}</label>
            <input
              id="confirmPassword"
              type={showPassword ? 'text' : 'password'}
              className="form-input"
              placeholder={t('再次输入密码')}
              autoComplete="new-password"
              value={confirmPassword}
              onChange={(e) => setConfirmPassword(e.target.value)}
              disabled={loading}
            />
          </div>
          <div className="form-group">
            <label htmlFor="affCode">{t('邀请码（可选）')}</label>
            <input
              id="affCode"
              type="text"
              className="form-input"
              placeholder={t('填写邀请码，双方均可获得奖励')}
              value={affCode}
              onChange={(e) => setAffCode(e.target.value.trim())}
              disabled={loading}
              style={{ fontFamily: 'monospace', letterSpacing: 1 }}
            />
          </div>
          <button
            type="submit"
            className="btn btn-primary"
            disabled={loading}
            style={{ width: '100%', justifyContent: 'center', padding: '9px', marginTop: '8px', fontSize: '13px' }}
          >
            {loading ? t('注册中...') : t('注册')}
          </button>
        </form>

        <div style={{ textAlign: 'center', marginTop: '16px' }}>
          <Link to="/login" style={{ fontSize: '13px', color: 'var(--accent-color)', textDecoration: 'none' }}>
            {t('已有账号？立即登录')}
          </Link>
        </div>
      </div>
    </div>
  );
}
