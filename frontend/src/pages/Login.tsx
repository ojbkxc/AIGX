import React, { useState, useRef, useEffect } from 'react';
import { useNavigate, useLocation } from 'react-router-dom';
import { api } from '../api';

interface LoginFormData {
  email: string;
  password: string;
}

interface ForgotPasswordFormData {
  email: string;
}

interface LoginPageState {
  email: string;
  password: string;
  error: string;
  success: string;
  loading: boolean;
  showForgot: boolean;
  forgotEmail: string;
  forgotLoading: boolean;
  forgotError: string;
  forgotSuccess: string;
}

export default function Login(): JSX.Element {
  const [email, setEmail] = useState<string>('');
  const [password, setPassword] = useState<string>('');
  const [error, setError] = useState<string>('');
  const [success, setSuccess] = useState<string>('');
  const [loading, setLoading] = useState(false);
  const navigate = useNavigate();
  const location = useLocation();
  const registeredHandled = useRef(false);

  // 忘记密码模态框状态
  const [showForgot, setShowForgot] = useState(false);
  const [forgotEmail, setForgotEmail] = useState('');
  const [forgotLoading, setForgotLoading] = useState(false);
  const [forgotError, setForgotError] = useState('');
  const [forgotSuccess, setForgotSuccess] = useState('');

  // 打开忘记密码弹窗，预填当前邮箱
  const openForgot = (): void => {
    setForgotEmail(email || '');
    setForgotError('');
    setForgotSuccess('');
    setShowForgot(true);
  };

  // 提交忘记密码请求，调用后端发送重置链接
  const handleForgotSubmit = async (e: React.FormEvent): Promise<void> => {
    e.preventDefault();
    setForgotError('');
    if (!forgotEmail) {
      setForgotError('请输入邮箱');
      return;
    }
    setForgotLoading(true);
    try {
      await api.forgotPassword(forgotEmail);
      setForgotSuccess('重置链接已发送，请查收邮件');
    } catch (err: any) {
      setForgotError(err.message || '发送重置链接失败');
    } finally {
      setForgotLoading(false);
    }
  };

  // Google OAuth 登录
  const handleGoogleLogin = (): void => {
    window.location.href = '/api/auth/google';
  };

  useEffect(() => {
    if (location.state?.registered && !registeredHandled.current) {
      registeredHandled.current = true;
      setSuccess('注册成功，请登录');
      setEmail(location.state.email || '');
      // 清除 state 防止重复提示
      window.history.replaceState({}, document.title);
    }
  }, [location, t]);

  const handleSubmit = async (e: React.FormEvent): Promise<void> => {
    e.preventDefault();
    setError('');
    if (!email || !password) {
      setError('请输入邮箱和密码');
      return;
    }
    setLoading(true);
    try {
      const res = await api.login(email, password);
      if (res.success && res.data) {
        localStorage.setItem('token', res.data.token);
        localStorage.setItem('email', res.data.email);
        localStorage.setItem('username', res.data.username || res.data.email);
        localStorage.setItem('expires_at', String(Number(res.data.expires_at) * 1000));
        navigate('/');
      } else {
        setError('登录失败：响应格式错误');
      }
    } catch (err: any) {
      setError(err.message || '登录失败');
    } finally {
      setLoading(false);
    }
  };

  const toggleTheme = (): void => {
    const html = document.documentElement;
    const isLight = html.getAttribute('data-theme') === 'light';
    html.setAttribute('data-theme', isLight ? 'dark' : 'light');
    localStorage.setItem('theme', isLight ? 'dark' : 'light');
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
      <div className="bg-orbs-container">
        <div className="bg-orb bg-orb-1"></div>
        <div className="bg-orb bg-orb-2"></div>
      </div>

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
        title="切换主题"
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
        animation: 'fadeInUp 0.6s cubic-bezier(0.16, 1, 0.3, 1)',
      }}>
        <div style={{ textAlign: 'center', marginBottom: '22px' }}>
          <div style={{
            width: '48px',
            height: '48px',
            background: 'var(--primary-gradient)',
            borderRadius: '12px',
            display: 'flex',
            alignItems: 'center',
            justifyContent: 'center',
            fontSize: '24px',
            margin: '0 auto 14px',
            color: 'white',
            boxShadow: '0 4px 14px rgba(168, 85, 247, 0.25)',
          }}>
            ⚡
          </div>
          <h1 style={{ fontSize: '20px', fontWeight: 700, color: 'var(--text-main)', marginBottom: '4px', fontFamily: "'Outfit', sans-serif" }}>
            AIGX Gateway
          </h1>
          <p style={{ fontSize: '13px', color: 'var(--text-muted)' }}>
            登录管理面板
          </p>
        </div>

        {error && (
          <div className="error-message">{error}</div>
        )}

        {success && (
          <div className="success-message">{success}</div>
        )}

        <form onSubmit={handleSubmit} style={{ display: 'flex', flexDirection: 'column', gap: '4px' }}>
          <div className="form-group">
            <label htmlFor="email">邮箱</label>
            <input
              id="email"
              type="email"
              className="form-input"
              placeholder="请输入邮箱"
              value={email}
              onChange={(e) => setEmail(e.target.value)}
              autoFocus
              disabled={loading}
            />
          </div>
          <div className="form-group">
            <label htmlFor="password">密码</label>
            <input
              id="password"
              type="password"
              className="form-input"
              placeholder="请输入密码"
              value={password}
              onChange={(e) => setPassword(e.target.value)}
              disabled={loading}
            />
          </div>
          <button
            type="submit"
            className="btn btn-primary"
            disabled={loading}
            style={{ width: '100%', justifyContent: 'center', padding: '9px', marginTop: '8px', fontSize: '13px' }}
          >
            {loading ? '登录中...' : '登录'}
          </button>
        </form>

        <div style={{ textAlign: 'right', marginTop: '8px' }}>
          <button
            type="button"
            onClick={openForgot}
            style={{
              fontSize: '12px',
              color: 'var(--accent-color)',
              background: 'none',
              border: 'none',
              cursor: 'pointer',
              padding: 0,
            }}
          >
            忘记密码？
          </button>
        </div>

        <div style={{ display: 'flex', alignItems: 'center', gap: '12px', margin: '16px 0' }}>
          <div style={{ flex: 1, height: '1px', background: 'var(--border-color)' }} />
          <span style={{ fontSize: '11px', color: 'var(--text-muted)' }}>或</span>
          <div style={{ flex: 1, height: '1px', background: 'var(--border-color)' }} />
        </div>

        <button
          type="button"
          onClick={handleGoogleLogin}
          className="btn btn-outline"
          style={{ width: '100%', justifyContent: 'center', padding: '9px', fontSize: '13px', gap: '8px' }}
        >
          <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" style={{ width: '16px', height: '16px' }}>
            <path fill="#4285F4" d="M22.56 12.25c0-.78-.07-1.53-.2-2.25H12v4.26h5.92c-.26 1.37-1.04 2.53-2.21 3.31v2.77h3.57c2.08-1.92 3.28-4.74 3.28-8.09z" />
            <path fill="#34A853" d="M12 23c2.97 0 5.46-.98 7.28-2.66l-3.57-2.77c-.98.66-2.23 1.06-3.71 1.06-2.86 0-5.29-1.93-6.16-4.53H2.18v2.84C3.99 20.53 7.7 23 12 23z" />
            <path fill="#FBBC05" d="M5.84 14.09c-.22-.66-.35-1.36-.35-2.09s.13-1.43.35-2.09V7.07H2.18C1.43 8.55 1 10.22 1 12s.43 3.45 1.18 4.93l3.66-2.84z" />
            <path fill="#EA4335" d="M12 5.38c1.62 0 3.06.56 4.21 1.64l3.15-3.15C17.45 2.09 14.97 1 12 1 7.7 1 3.99 3.47 2.18 7.07l3.66 2.84c.87-2.6 3.3-4.53 6.16-4.53z" />
          </svg>
          使用 Google 登录
        </button>

        <div style={{ textAlign: 'center', marginTop: '16px' }}>
          <a href="/register" style={{ fontSize: '13px', color: 'var(--accent-color)', textDecoration: 'none' }}>
            还没有账号？立即注册
          </a>
        </div>

        <div style={{
          marginTop: '20px',
          padding: '10px 12px',
          fontSize: '12px',
          color: 'var(--text-muted)',
          background: 'var(--bg-color)',
          border: '1px solid var(--border-color)',
          borderRadius: '8px',
          textAlign: 'center',
          lineHeight: '1.5',
        }}>
          首次启动请查看服务日志获取初始管理员密码
        </div>
      </div>

      {showForgot && (
        <div className="modal-overlay" onClick={() => setShowForgot(false)}>
          <div className="modal" style={{ maxWidth: 380, width: '90%' }} onClick={(e) => e.stopPropagation()}>
            <div className="modal-header">
              <h3>忘记密码</h3>
              <button className="modal-close" onClick={() => setShowForgot(false)}>&times;</button>
            </div>
            <div className="modal-body">
              {forgotSuccess ? (
                <div className="success-message">{forgotSuccess}</div>
              ) : (
                <>
                  <p style={{ fontSize: 13, color: 'var(--text-muted)', marginBottom: 12 }}>
                    输入注册邮箱，我们将发送密码重置链接到您的邮箱
                  </p>
                  {forgotError && <div className="error-message">{forgotError}</div>}
                  <form onSubmit={handleForgotSubmit}>
                    <div className="form-group">
                      <label htmlFor="forgot-email">邮箱</label>
                      <input
                        id="forgot-email"
                        type="email"
                        className="form-input"
                        placeholder="请输入邮箱"
                        value={forgotEmail}
                        onChange={(e) => setForgotEmail(e.target.value)}
                        autoFocus
                        disabled={forgotLoading}
                      />
                    </div>
                    <button
                      type="submit"
                      className="btn btn-primary"
                      disabled={forgotLoading}
                      style={{ width: '100%', justifyContent: 'center', padding: '9px', fontSize: '13px' }}
                    >
                      {forgotLoading ? '发送中...' : '发送重置链接'}
                    </button>
                  </form>
                </>
              )}
            </div>
            <div className="modal-footer">
              <button className="btn btn-outline" onClick={() => setShowForgot(false)}>
                {forgotSuccess ? '关闭' : '取消'}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}