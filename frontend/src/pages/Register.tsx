import React, { useState } from 'react';
import { useNavigate } from 'react-router-dom';
import { api } from '../api';

export default function Register(): JSX.Element {
  const [username, setUsername] = useState<string>('');
  const [email, setEmail] = useState<string>('');
  const [password, setPassword] = useState<string>('');
  const [error, setError] = useState<string>('');
  const [success, setSuccess] = useState<string>('');
  const [loading, setLoading] = useState(false);
  const navigate = useNavigate();

  const handleSubmit = async (e: React.FormEvent): Promise<void> => {
    e.preventDefault();
    setError('');
    if (!username || !email || !password) {
      setError('请填写所有字段');
      return;
    }
    setLoading(true);
    try {
      await api.register(username, email, password);
      setSuccess('注册成功，请登录');
      // 跳转回登录页并带上注册成功的提示
      navigate('/login', { state: { registered: true, email } });
    } catch (err: any) {
      setError(err.message || '注册失败');
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
            ✨
          </div>
          <h1 style={{ fontSize: '20px', fontWeight: 700, color: 'var(--text-main)', marginBottom: '4px', fontFamily: "'Outfit', sans-serif" }}>
            AIGX Gateway
          </h1>
          <p style={{ fontSize: '13px', color: 'var(--text-muted)' }}>
            创建新账号
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
            <label htmlFor="username">用户名</label>
            <input
              id="username"
              type="text"
              className="form-input"
              placeholder="请输入用户名"
              value={username}
              onChange={(e) => setUsername(e.target.value)}
              autoFocus
              disabled={loading}
            />
          </div>
          <div className="form-group">
            <label htmlFor="register-email">邮箱</label>
            <input
              id="register-email"
              type="email"
              className="form-input"
              placeholder="请输入邮箱"
              value={email}
              onChange={(e) => setEmail(e.target.value)}
              disabled={loading}
            />
          </div>
          <div className="form-group">
            <label htmlFor="register-password">密码</label>
            <input
              id="register-password"
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
            {loading ? '注册中...' : '注册'}
          </button>
        </form>

        <div style={{ textAlign: 'center', marginTop: '16px' }}>
          <a href="/login" style={{ fontSize: '13px', color: 'var(--accent-color)', textDecoration: 'none' }}>
            已有账号？立即登录
          </a>
        </div>
      </div>
    </div>
  );
}
