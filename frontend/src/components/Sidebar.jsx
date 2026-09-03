import React from 'react';
import { NavLink, useNavigate } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import { api } from '../api';

const navItems = [
  { path: '/', labelKey: '仪表盘', icon: '📊', end: true },

  { path: '/channels', labelKey: '渠道管理', icon: '🛰️' },
  { path: '/keys', labelKey: 'API 密钥', icon: '🔐' },
  { path: '/mappings', labelKey: '模型映射', icon: '🔄' },
  { path: '/pricing', labelKey: '定价倍率', icon: '💲' },
  { path: '/users', labelKey: '用户管理', icon: '👥' },
  { path: '/groups', labelKey: '用户分组', icon: '🏷️' },
  { path: '/wallet', labelKey: '钱包充值', icon: '💰' },
  { path: '/orders', labelKey: '订单记录', icon: '🧾' },
  { path: '/redemptions', labelKey: '兑换码', icon: '🎟️' },
  { path: '/logs', labelKey: '日志审计', icon: '📋' },
  { path: '/epay', labelKey: '易支付', icon: '💳' },
  { path: '/notify', labelKey: '通知设置', icon: '🔔' },
  { path: '/settings', labelKey: '系统设置', icon: '⚙️' },
];

// 分组定义：14 个一级菜单合并为 7 组，减少侧边栏长度。
// 仪表盘与日志审计保持独立（高频/独立职能），其余按职能合并。
const navGroups = [
  { key: 'top', items: [navItems[0]] }, // 仪表盘
  {
    key: 'access',
    labelKey: '接入与密钥',
    icon: '🔌',
    items: [navItems[1], navItems[2]], // 渠道管理、API 密钥
  },
  {
    key: 'model',
    labelKey: '模型与定价',
    icon: '🧠',
    items: [navItems[3], navItems[4]], // 模型映射、定价倍率
  },
  {
    key: 'user',
    labelKey: '用户与分组',
    icon: '👤',
    items: [navItems[5], navItems[6]], // 用户管理、用户分组
  },
  {
    key: 'finance',
    labelKey: '财务与额度',
    icon: '💼',
    items: [navItems[7], navItems[8], navItems[9]], // 钱包充值、订单记录、兑换码
  },
  { key: 'logs', items: [navItems[10]] }, // 日志审计
  {
    key: 'system',
    labelKey: '系统设置',
    icon: '🛠️',
    items: [navItems[11], navItems[12], navItems[13]], // 易支付、通知设置、系统设置
  },
];

export default function Sidebar() {
  const navigate = useNavigate();
  const { t, i18n } = useTranslation();
  // 分组折叠状态：默认全部展开，记忆到 localStorage
  const [collapsedGroups, setCollapsedGroups] = React.useState(() => {
    try {
      const saved = localStorage.getItem('sidebar_collapsed');
      return saved ? JSON.parse(saved) : {};
    } catch {
      return {};
    }
  });

  const toggleGroup = (key) => {
    setCollapsedGroups((prev) => {
      const next = { ...prev, [key]: !prev[key] };
      try { localStorage.setItem('sidebar_collapsed', JSON.stringify(next)); } catch {}
      return next;
    });
  };

  // 判断分组是否含当前激活项（用于自动展开）
  const isGroupActive = (items) => items.some((it) => window.location.pathname === it.path);

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
      padding: '20px 12px',
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
        gap: '10px',
        marginBottom: '24px',
        paddingLeft: '6px',
      }}>
        <div style={{
          width: '30px',
          height: '30px',
          borderRadius: '8px',
          background: 'var(--primary-gradient)',
          display: 'flex',
          alignItems: 'center',
          justifyContent: 'center',
          fontWeight: 'bold',
          color: 'white',
          fontSize: '14px',
          fontFamily: "'Outfit', sans-serif",
          boxShadow: '0 4px 12px rgba(99, 102, 241, 0.2)',
        }}>
          ⚡
        </div>
        <div>
          <div style={{
            fontSize: '14px',
            fontWeight: 700,
            fontFamily: "'Outfit', sans-serif",
            letterSpacing: '-0.5px',
            background: 'var(--primary-gradient)',
            WebkitBackgroundClip: 'text',
            WebkitTextFillColor: 'transparent',
          }}>
            AIGX
          </div>
          <div style={{ fontSize: '10px', color: 'var(--text-muted)', fontWeight: 500 }}>
            {t('AI 中转网关')}
          </div>
        </div>
      </div>

      {/* Navigation */}
      <nav style={{
        display: 'flex',
        flexDirection: 'column',
        gap: '4px',
        flex: 1,
      }}>
        {navGroups.map((group) => {
          // 无 label 的分组（仪表盘、日志）直接平铺，不渲染分组标题
          if (!group.labelKey) {
            return group.items.map((item) => (
              <NavLink
                key={item.path}
                to={item.path}
                end={item.end}
                className={({ isActive }) => `nav-item ${isActive ? 'active' : ''}`}
                style={({ isActive }) => ({
                  display: 'flex',
                  alignItems: 'center',
                  gap: '8px',
                  padding: '7px 10px',
                  borderRadius: '8px',
                  cursor: 'pointer',
                  fontSize: '12.5px',
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
                <span style={{ fontSize: '13px', width: '18px', textAlign: 'center', flexShrink: 0 }}>
                  {item.icon}
                </span>
                <span>{t(item.labelKey)}</span>
              </NavLink>
            ));
          }
          // 可折叠分组
          const collapsed = collapsedGroups[group.key] && !isGroupActive(group.items);
          const groupActive = isGroupActive(group.items);
          return (
            <div key={group.key} style={{ display: 'flex', flexDirection: 'column', gap: '2px' }}>
              <button
                onClick={() => toggleGroup(group.key)}
                style={{
                  display: 'flex',
                  alignItems: 'center',
                  gap: '8px',
                  padding: '6px 8px',
                  borderRadius: '7px',
                  cursor: 'pointer',
                  fontSize: '11px',
                  fontWeight: 600,
                  letterSpacing: '0.4px',
                  textTransform: 'uppercase',
                  color: groupActive ? 'var(--text-main)' : 'var(--text-muted)',
                  background: 'transparent',
                  border: 'none',
                  width: '100%',
                  textAlign: 'left',
                  transition: 'all 0.2s ease',
                }}
              >
                <span style={{ fontSize: '12px', width: '18px', textAlign: 'center', flexShrink: 0 }}>
                  {group.icon}
                </span>
                <span style={{ flex: 1 }}>{t(group.labelKey)}</span>
                <span style={{
                  fontSize: '9px',
                  transition: 'transform 0.25s ease',
                  transform: collapsed ? 'rotate(-90deg)' : 'rotate(0deg)',
                  opacity: 0.6,
                }}>▼</span>
              </button>
              {!collapsed && group.items.map((item) => (
                <NavLink
                  key={item.path}
                  to={item.path}
                  end={item.end}
                  className={({ isActive }) => `nav-item ${isActive ? 'active' : ''}`}
                  style={({ isActive }) => ({
                    display: 'flex',
                    alignItems: 'center',
                    gap: '8px',
                    padding: '6px 10px 6px 26px',
                    borderRadius: '8px',
                    cursor: 'pointer',
                    fontSize: '12.5px',
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
                  <span style={{ fontSize: '13px', width: '18px', textAlign: 'center', flexShrink: 0 }}>
                    {item.icon}
                  </span>
                  <span>{t(item.labelKey)}</span>
                </NavLink>
              ))}
            </div>
          );
        })}
      </nav>

      {/* Footer */}
      <div style={{
        display: 'flex',
        flexDirection: 'column',
        gap: '10px',
        borderTop: '1px solid var(--border-color)',
        paddingTop: '12px',
      }}>
        <div style={{ display: 'flex', alignItems: 'center', gap: '8px', padding: '0 4px' }}>
          <div style={{
            width: '24px',
            height: '24px',
            borderRadius: '50%',
            background: 'var(--primary-gradient)',
            color: 'white',
            display: 'flex',
            alignItems: 'center',
            justifyContent: 'center',
            fontSize: '11px',
            fontWeight: 600,
            flexShrink: 0,
          }}>
            {email.charAt(0).toUpperCase()}
          </div>
          <div style={{ display: 'flex', flexDirection: 'column', overflow: 'hidden' }}>
            <span style={{ fontSize: '12px', fontWeight: 500, color: 'var(--text-main)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
              {email}
            </span>
            {username && <span style={{ fontSize: '10px', color: 'var(--text-muted)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
              @{username}
            </span>}
          </div>
        </div>
        <div style={{ display: 'flex', gap: '5px' }}>
          <button
            className="btn btn-outline btn-sm"
            onClick={toggleTheme}
            style={{ flex: 1 }}
            title={t('切换主题')}
          >
            <svg xmlns="http://www.w3.org/2000/svg" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth="2" style={{ width: '12px', height: '12px' }}>
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
            <svg xmlns="http://www.w3.org/2000/svg" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth="2" style={{ width: '12px', height: '12px' }}>
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
