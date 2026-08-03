import React, { useState } from 'react';
import { useNavigate, useLocation, Link } from 'react-router-dom';
import { api } from '../api';

export default function Login() {
  const [email, setEmail] = useState('');
  const [password, setPassword] = useState('');
  const [error, setError] = useState('');
  const [success, setSuccess] = useState('');
  const [loading, setLoading] = useState(false);
  const navigate = useNavigate();
  const location = useLocation();

  React.useEffect(() => {
    if (location.state?.registered) {
      setSuccess('注册成功，请登录');
      setEmail(location.state.email || '');
      // 清除 state 防止重复提示
      window.history.replaceState({}, document.title);
    }
  }, [location]);

  const handleSubmit = async (e) => {
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
        localStorage.setItem('expires_at', String(res.data.expires_at));
        navigate('/');
      } else {
        setError('登录失败：响应格式错误');
      }
    } catch (err) {
      setError(err.message || '登录失败');
    } finally {
      setLoading(false);
    }
  };

  const toggleTheme = () => {
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
        borderRadius: '20px',
        padding: '40px',
        width: '100%',
        maxWidth: '400px',
        boxShadow: 'var(--card-shadow)',
        backdropFilter: 'blur(var(--glass-blur))',
        WebkitBackdropFilter: 'blur(var(--glass-blur))',
        animation: 'fadeInUp 0.6s cubic-bezier(0.16, 1, 0.3, 1)',
      }}>
        <div style={{ textAlign: 'center', marginBottom: '28px' }}>
          <div style={{
            width: '56px',
            height: '56px',
            background: 'var(--primary-gradient)',
            borderRadius: '14px',
            display: 'flex',
            alignItems: 'center',
            justifyContent: 'center',
            fontSize: '28px',
            margin: '0 auto 16px',
            color: 'white',
            boxShadow: '0 4px 14px rgba(168, 85, 247, 0.25)',
          }}>
            ⚡
          </div>
          <h1 style={{ fontSize: '22px', fontWeight: 700, color: 'var(--text-main)', marginBottom: '4px', fontFamily: "'Outfit', sans-serif" }}>
            AIGX Gateway
          </h1>
          <p style={{ fontSize: '14px', color: 'var(--text-muted)' }}>
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
            style={{ width: '100%', justifyContent: 'center', padding: '12px', marginTop: '8px', fontSize: '15px' }}
          >
            {loading ? '登录中...' : '登录'}
          </button>
        </form>

        <div style={{ textAlign: 'center', marginTop: '16px' }}>
          <Link to="/register" style={{ fontSize: '13px', color: 'var(--primary-color)', textDecoration: 'none' }}>
            还没有账号？立即注册
          </Link>
        </div>
      </div>
    </div>
  );
}