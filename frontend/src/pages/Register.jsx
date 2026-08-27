import React, { useState } from 'react';
import { useNavigate, Link } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import { api } from '../api';

export default function Register() {
  const [email, setEmail] = useState('');
  const [username, setUsername] = useState('');
  const [password, setPassword] = useState('');
  const [confirmPassword, setConfirmPassword] = useState('');
  const [error, setError] = useState('');
  const [loading, setLoading] = useState(false);
  const navigate = useNavigate();
  const { t } = useTranslation();

  const handleSubmit = async (e) => {
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
      const res = await api.register(email, password, username || undefined);
      if (res.success) {
        // 注册成功后自动跳转登录页
        navigate('/login', { state: { registered: true, email } });
      } else {
        setError(res.error || t('注册失败'));
      }
    } catch (err) {
      setError(err.message || t('注册失败'));
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
        title={t('切换主题')}
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
            {t('创建账号')}
          </h1>
          <p style={{ fontSize: '14px', color: 'var(--text-muted)' }}>
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
              onChange={(e) => setUsername(e.target.value)}
              disabled={loading}
            />
          </div>
          <div className="form-group">
            <label htmlFor="password">{t('密码 *')}</label>
            <input
              id="password"
              type="password"
              className="form-input"
              placeholder={t('至少6位密码')}
              value={password}
              onChange={(e) => setPassword(e.target.value)}
              disabled={loading}
            />
          </div>
          <div className="form-group">
            <label htmlFor="confirmPassword">{t('确认密码 *')}</label>
            <input
              id="confirmPassword"
              type="password"
              className="form-input"
              placeholder={t('再次输入密码')}
              value={confirmPassword}
              onChange={(e) => setConfirmPassword(e.target.value)}
              disabled={loading}
            />
          </div>
          <button
            type="submit"
            className="btn btn-primary"
            disabled={loading}
            style={{ width: '100%', justifyContent: 'center', padding: '12px', marginTop: '8px', fontSize: '15px' }}
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
