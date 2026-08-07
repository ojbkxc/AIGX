import React from 'react';
import { NavLink, useNavigate } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import { api } from '../api';

const navItems = [
  { path: '/', labelKey: '仪表盘', icon: '📊', end: true },
  { path: '/accounts', labelKey: '账号管理', icon: '🔑' },
  { path: '/keys', labelKey: 'API 密钥', icon: '🔐' },
  { path: '/mappings', labelKey: '模型映射', icon: '🔄' },
  { path: '/users', labelKey: '用户管理', icon: '👥' },
  { path: '/wallet', labelKey: '钱包充值', icon: '💰' },
  { path: '/orders', labelKey: '订单记录', icon: '🧾' },
  { path: '/redemptions', labelKey: '兑换码', icon: '🎟️' },
  { path: '/logs', labelKey: '日志审计', icon: '📋' },
  { path: '/epay', labelKey: '易支付', icon: '💳' },
  { path: '/settings', labelKey: '系统设置', icon: '⚙️' },
];

export default function Sidebar() {
  const navigate = useNavigate();
  const { t, i18n } = useTranslation();

  const handleLogout = async () => {
    try {
      await api.logout();
    } catch (e) {
      // Ignore logout errors
    }
    localStorage.removeItem('token');
    localStorage.removeItem('email');
    localStorage.removeItem('username');
    localStorage.removeItem('expires_at');
    navigate('/login');
  };

  const toggleTheme = () => {
    const html = document.documentElement;
    const isLight = html.getAttribute('data-theme') === 'light';
    html.setAttribute('data-theme', isLight ? 'dark' : 'light');
    localStorage.setItem('theme', isLight ? 'dark' : 'light');
  };

  const toggleLanguage = () => {
    const next = i18n.language === 'zh' ? 'en' : 'zh';
    localStorage.setItem('i18n_lang', next);
    i18n.changeLanguage(next);
  };

  const email = localStorage.getItem('email') || 'Admin';
  const username = localStorage.getItem('username') || '';

  return (
    <aside style={{
      width: 'var(--sidebar-width)',
      background: 'var(--sidebar-bg)',
      borderRight: '1px solid var(--border-color)',
      display: 'flex',
      flexDirection: 'column',
      padding: '30px 16px',
      position: 'fixed',
      top: 0,
      bottom: 0,
      left: 0,
      zIndex: 100,
      backdropFilter: 'blur(var(--glass-blur))',
      WebkitBackdropFilter: 'blur(var(--glass-blur))',
      transition: 'transform 0.3s cubic-bezier(0.16, 1, 0.3, 1)',
    }}>
      {/* Logo */}
      <div style={{
        display: 'flex',
        alignItems: 'center',
        gap: '12px',
        marginBottom: '36px',
        paddingLeft: '8px',
      }}>
        <div style={{
          width: '38px',
          height: '38px',
          borderRadius: '10px',
          background: 'var(--primary-gradient)',
          display: 'flex',
          alignItems: 'center',
          justifyContent: 'center',
          fontWeight: 'bold',
          color: 'white',
          fontSize: '18px',
          fontFamily: "'Outfit', sans-serif",
          boxShadow: '0 4px 12px rgba(99, 102, 241, 0.2)',
        }}>
          ⚡
        </div>
        <div>
          <div style={{
            fontSize: '18px',
            fontWeight: 700,
            fontFamily: "'Outfit', sans-serif",
            letterSpacing: '-0.5px',
            background: 'var(--primary-gradient)',
            WebkitBackgroundClip: 'text',
            WebkitTextFillColor: 'transparent',
          }}>
            AIGX
          </div>
          <div style={{ fontSize: '11px', color: 'var(--text-muted)', fontWeight: 500 }}>
            {t('AI 中转网关')}
          </div>
        </div>
      </div>

      {/* Navigation */}
      <nav style={{
        display: 'flex',
        flexDirection: 'column',
        gap: '6px',
        flex: 1,
      }}>
        {navItems.map((item) => (
          <NavLink
            key={item.path}
            to={item.path}
            end={item.end}
            className={({ isActive }) => `nav-item ${isActive ? 'active' : ''}`}
            style={({ isActive }) => ({
              display: 'flex',
              alignItems: 'center',
              gap: '12px',
              padding: '12px 14px',
              borderRadius: '10px',
              cursor: 'pointer',
              fontSize: '14px',
              fontWeight: 500,
              color: isActive ? 'white' : 'var(--text-muted)',
              background: isActive ? 'var(--primary-gradient)' : 'transparent',
              boxShadow: isActive
                ? '0 0 18px rgba(168, 85, 247, 0.35), 0 0 40px rgba(99, 102, 241, 0.15), inset 0 1px 0 rgba(255, 255, 255, 0.1)'
                : 'none',
              textDecoration: 'none',
              transition: 'all 0.25s cubic-bezier(0.16, 1, 0.3, 1)',
              position: 'relative',
              overflow: 'hidden',
            })}
          >
            <span style={{ fontSize: '16px', width: '20px', textAlign: 'center', flexShrink: 0 }}>
              {item.icon}
            </span>
            <span>{t(item.labelKey)}</span>
          </NavLink>
        ))}
      </nav>

      {/* Footer */}
      <div style={{
        display: 'flex',
        flexDirection: 'column',
        gap: '12px',
        borderTop: '1px solid var(--border-color)',
        paddingTop: '16px',
      }}>
        <div style={{ display: 'flex', alignItems: 'center', gap: '10px', padding: '0 6px' }}>
          <div style={{
            width: '28px',
            height: '28px',
            borderRadius: '50%',
            background: 'var(--primary-gradient)',
            color: 'white',
            display: 'flex',
            alignItems: 'center',
            justifyContent: 'center',
            fontSize: '12px',
            fontWeight: 600,
            flexShrink: 0,
          }}>
            {email.charAt(0).toUpperCase()}
          </div>
          <div style={{ display: 'flex', flexDirection: 'column', overflow: 'hidden' }}>
            <span style={{ fontSize: '13px', fontWeight: 500, color: 'var(--text-main)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
              {email}
            </span>
            {username && <span style={{ fontSize: '11px', color: 'var(--text-muted)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
              @{username}
            </span>}
          </div>
        </div>
        <div style={{ display: 'flex', gap: '6px' }}>
          <button
            className="btn btn-outline btn-sm"
            onClick={toggleTheme}
            style={{ flex: 1 }}
            title={t('切换主题')}
          >
            <svg xmlns="http://www.w3.org/2000/svg" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth="2" style={{ width: '14px', height: '14px' }}>
              <path d="M12 3a6 6 0 0 0 9 9 9 9 0 1 1-9-9Z" />
            </svg>
            {t('主题')}
          </button>
          <button
            className="btn btn-outline btn-sm"
            onClick={toggleLanguage}
            style={{ flex: 1 }}
            title={t('语言切换')}
          >
            <svg xmlns="http://www.w3.org/2000/svg" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth="2" style={{ width: '14px', height: '14px' }}>
              <circle cx="12" cy="12" r="10" />
              <path d="M2 12h20M12 2a15.3 15.3 0 0 1 4 10 15.3 15.3 0 0 1-4 10 15.3 15.3 0 0 1-4-10 15.3 15.3 0 0 1 4-10z" />
            </svg>
            {i18n.language === 'zh' ? 'EN' : '中'}
          </button>
          <button className="btn btn-outline btn-sm" onClick={handleLogout}>
            {t('退出')}
          </button>
        </div>
      </div>
    </aside>
  );
}
