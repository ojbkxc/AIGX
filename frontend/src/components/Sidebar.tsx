import React from 'react';
import { NavLink, useLocation, useNavigate } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import { api } from '../api';
import { isAdmin } from '../lib/utils';
import {
  LayoutDashboard, Satellite, KeyRound, ArrowLeftRight, CircleDollarSign,
  Users, Tags, Wallet, Receipt, Ticket, ScrollText, CreditCard, Bell,
  Settings, Play, Shield, Globe, Network, Zap, ChevronDown, Menu,
} from 'lucide-react';
import type { LucideIcon } from 'lucide-react';
import MobileDrawer from './ui/MobileDrawer';

interface NavItem {
  path: string;
  labelKey: string;
  icon: LucideIcon;
  end?: boolean;
  adminOnly?: boolean;
}

interface NavGroup {
  key: string;
  labelKey?: string;
  icon?: LucideIcon;
  items: NavItem[];
}

const navItems: NavItem[] = [
  { path: '/', labelKey: '仪表盘', icon: LayoutDashboard, end: true },

  { path: '/channels', labelKey: '渠道管理', icon: Satellite, adminOnly: true },
  { path: '/keys', labelKey: 'API 密钥', icon: KeyRound, adminOnly: true },
  { path: '/mappings', labelKey: '模型映射', icon: ArrowLeftRight, adminOnly: true },
  { path: '/pricing', labelKey: '定价倍率', icon: CircleDollarSign, adminOnly: true },
  { path: '/users', labelKey: '用户管理', icon: Users, adminOnly: true },
  { path: '/groups', labelKey: '用户分组', icon: Tags, adminOnly: true },
  { path: '/wallet', labelKey: '钱包充值', icon: Wallet },
  { path: '/orders', labelKey: '订单记录', icon: Receipt, adminOnly: true },
  { path: '/redemptions', labelKey: '兑换码', icon: Ticket, adminOnly: true },
  { path: '/logs', labelKey: '日志审计', icon: ScrollText, adminOnly: true },
  { path: '/epay', labelKey: '易支付', icon: CreditCard, adminOnly: true },
  { path: '/notify', labelKey: '通知设置', icon: Bell, adminOnly: true },
  { path: '/settings', labelKey: '系统设置', icon: Settings, adminOnly: true },
  { path: '/playground', labelKey: 'Playground', icon: Play, adminOnly: true },
  { path: '/security', labelKey: '安全监控', icon: Shield, adminOnly: true },
  { path: '/ip-management', labelKey: 'IP 管理', icon: Globe, adminOnly: true },
  { path: '/network-layer', labelKey: '网络层概览', icon: Network, adminOnly: true },
];

// 精简分组：高频入口（仪表盘/渠道/密钥/Playground/日志）平铺直达，
// 低频管理合并进「更多管理」单一折叠组，侧栏总高度减半。
// 手风琴模式：同组子项展开时其余组自动收起，点击组头永远可切换收/展。
const navGroups: NavGroup[] = [
  { key: 'top', items: [navItems[0]] },                                    // 仪表盘
  { key: 'quick1', items: [navItems[1]] },                                 // 渠道管理
  { key: 'quick2', items: [navItems[2]] },                                 // API 密钥
  { key: 'quick3', items: [navItems[14]] },                                // Playground
  { key: 'logs', items: [navItems[10]] },                                   // 日志审计
  {
    key: 'more',
    labelKey: '更多管理',
    icon: Layers,
    items: [
      navItems[3],   // 模型映射
      navItems[4],   // 定价倍率
      navItems[5],   // 用户管理
      navItems[6],   // 用户分组
      navItems[7],   // 钱包充值
      navItems[8],   // 订单记录
      navItems[9],   // 兑换码
      navItems[15],  // 安全监控
      navItems[16],  // IP 管理
      navItems[17],  // 网络层概览
      navItems[11],  // 易支付
      navItems[12],  // 通知设置
      navItems[13],  // 系统设置
    ],
  },
];

// 角色过滤：普通用户仅见无 adminOnly 标记的菜单（展示层过滤，权限由后端强制）
function roleFilter(items: NavItem[]): NavItem[] {
  if (isAdmin()) return items;
  return items.filter((it) => !it.adminOnly);
}

export default function Sidebar(): JSX.Element {
  const navigate = useNavigate();
  const location = useLocation();
  const { t, i18n } = useTranslation();
  // 移动端抽屉开关：仅 ≤768px 由汉堡按钮触发
  const [mobileOpen, setMobileOpen] = React.useState<boolean>(false);

  // 分组折叠状态：手风琴语义——默认全收起，点开一个组时其余组自动收起。
  // 当前路由所在的组始终视为展开（isGroupActive），但用户点击组头仍可手动收起。
  const [collapsedGroups, setCollapsedGroups] = React.useState<Record<string, boolean>>(() => {
    try {
      const saved = localStorage.getItem('sidebar_collapsed');
      return saved ? JSON.parse(saved) : {};
    } catch {
      return {};
    }
  });

  // 路由切换后自动收起移动端抽屉
  React.useEffect(() => {
    setMobileOpen(false);
  }, [location.pathname]);

  const toggleGroup = (key: string): void => {
    setCollapsedGroups((prev) => {
      // 手风琴：展开该组 = 收起其它所有组；再次点击 = 收起该组
      const next: Record<string, boolean> = {};
      for (const g of navGroups) {
        if (!g.labelKey) continue; // 平铺组无折叠语义
        next[g.key] = g.key === key ? !prev[key] : true;
      }
      try { localStorage.setItem('sidebar_collapsed', JSON.stringify(next)); } catch {}
      return next;
    });
  };

  // 判断分组是否含当前激活项（用于自动展开）
  const isGroupActive = (items: NavItem[]): boolean =>
    items.some((it) => location.pathname === it.path);

  const handleLogout = async (): Promise<void> => {
    try {
      await api.logout();
    } catch {
      // Ignore logout errors
    }
    localStorage.removeItem('token');
    localStorage.removeItem('email');
    localStorage.removeItem('username');
    localStorage.removeItem('role');
    localStorage.removeItem('expires_at');
    navigate('/login');
  };

  const toggleTheme = (): void => {
    const html = document.documentElement;
    const isLight = html.getAttribute('data-theme') === 'light';
    html.setAttribute('data-theme', isLight ? 'dark' : 'light');
    localStorage.setItem('theme', isLight ? 'dark' : 'light');
  };

  const toggleLanguage = (): void => {
    const next = i18n.language === 'zh' ? 'en' : 'zh';
    localStorage.setItem('i18n_lang', next);
    i18n.changeLanguage(next);
  };

  const email = localStorage.getItem('email') || 'Admin';
  const username = localStorage.getItem('username') || '';

  // 侧栏主体：桌面端常驻 fixed；移动端由 CSS media query 隐藏、抽屉承载
  const sidebarBody = (
    <aside style={{
      width: 'var(--sidebar-width)',
      background: 'var(--sidebar-bg)',
      borderRight: '1px solid var(--border-color)',
      display: 'flex',
      flexDirection: 'column',
      padding: '14px 10px',
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
        marginBottom: '14px',
        paddingLeft: '6px',
      }}>
        <div style={{
          width: '26px',
          height: '26px',
          borderRadius: '7px',
          background: 'var(--accent-color)',
          display: 'flex',
          alignItems: 'center',
          justifyContent: 'center',
          fontWeight: 'bold',
          color: 'white',
          fontSize: '13px',
          fontFamily: "'Inter', sans-serif",
          boxShadow: 'none',
        }}>
          <Zap size={14} strokeWidth={2} />
        </div>
        <div>
          <div style={{
            fontSize: '14px',
            fontWeight: 700,
            fontFamily: "'Inter', sans-serif",
            letterSpacing: '-0.5px',
            color: 'var(--text-main)',
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
        gap: '2px',
        flex: 1,
      }}>
        {navGroups.map((group) => {
          // 角色过滤：普通用户不渲染管理员专属菜单；整组为空则隐藏
          const visibleItems = roleFilter(group.items);
          if (visibleItems.length === 0) return null;
          // 无 label 的分组（高频直达项）平铺，不渲染分组标题
          if (!group.labelKey) {
            return visibleItems.map((item) => (
              <NavLink
                key={item.path}
                to={item.path}
                end={item.end}
                className={({ isActive }) => `nav-item ${isActive ? 'active' : ''}`}
                style={({ isActive }) => ({
                  display: 'flex',
                  alignItems: 'center',
                  gap: '8px',
                  padding: '6px 10px',
                  borderRadius: '8px',
                  cursor: 'pointer',
                  fontSize: '12.5px',
                  fontWeight: 500,
                  color: isActive ? 'var(--text-main)' : 'var(--text-muted)',
                  background: isActive ? 'rgba(47, 111, 237, 0.16)' : 'transparent',
                  boxShadow: isActive
                    ? 'inset 2px 0 0 var(--accent-color)'
                    : 'none',
                  textDecoration: 'none',
                  transition: 'all 0.25s cubic-bezier(0.16, 1, 0.3, 1)',
                  position: 'relative',
                  overflow: 'hidden',
                })}
              >
                <span style={{ fontSize: '14px', width: '18px', display: 'inline-flex', alignItems: 'center', justifyContent: 'center', flexShrink: 0 }}>
                  <item.icon size={15} strokeWidth={1.8} />
                </span>
                <span>{t(item.labelKey)}</span>
              </NavLink>
            ));
          }
          // 可折叠分组：collapsed 仅由用户状态决定——激活组也允许手动收起
          const collapsed = collapsedGroups[group.key] ?? true;
          const groupActive = isGroupActive(visibleItems);
          return (
            <div key={group.key} style={{ display: 'flex', flexDirection: 'column', gap: '2px' }}>
              <button
                onClick={() => toggleGroup(group.key)}
                aria-expanded={!collapsed}
                style={{
                  display: 'flex',
                  alignItems: 'center',
                  gap: '8px',
                  padding: '6px 10px',
                  borderRadius: '8px',
                  cursor: 'pointer',
                  fontSize: '12.5px',
                  fontWeight: groupActive ? 600 : 500,
                  color: groupActive ? 'var(--text-main)' : 'var(--text-muted)',
                  background: 'transparent',
                  border: 'none',
                  width: '100%',
                  textAlign: 'left',
                  transition: 'all 0.2s ease',
                }}
              >
                <span style={{ fontSize: '14px', width: '18px', display: 'inline-flex', alignItems: 'center', justifyContent: 'center', flexShrink: 0 }}>
                  {group.icon ? <group.icon size={15} strokeWidth={1.8} /> : null}
                </span>
                <span style={{ flex: 1 }}>{t(group.labelKey)}</span>
                <span style={{
                  display: 'inline-flex',
                  transition: 'transform 0.2s ease',
                  transform: collapsed ? 'rotate(-90deg)' : 'rotate(0deg)',
                  opacity: 0.6,
                }}>
                  <ChevronDown size={13} strokeWidth={1.8} />
                </span>
              </button>
              {!collapsed && visibleItems.map((item) => (
                <NavLink
                  key={item.path}
                  to={item.path}
                  end={item.end}
                  className={({ isActive }) => `nav-item ${isActive ? 'active' : ''}`}
                  style={({ isActive }) => ({
                    display: 'flex',
                    alignItems: 'center',
                    gap: '8px',
                    padding: '5px 10px 5px 28px',
                    borderRadius: '8px',
                    cursor: 'pointer',
                    fontSize: '12px',
                    fontWeight: 500,
                    color: isActive ? 'var(--text-main)' : 'var(--text-muted)',
                    background: isActive ? 'rgba(47, 111, 237, 0.16)' : 'transparent',
                    boxShadow: isActive
                      ? 'inset 2px 0 0 var(--accent-color)'
                      : 'none',
                    textDecoration: 'none',
                    transition: 'all 0.25s cubic-bezier(0.16, 1, 0.3, 1)',
                    position: 'relative',
                    overflow: 'hidden',
                  })}
                >
                  <span style={{ fontSize: '14px', width: '18px', display: 'inline-flex', alignItems: 'center', justifyContent: 'center', flexShrink: 0 }}>
                    <item.icon size={15} strokeWidth={1.8} />
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
            background: 'var(--accent-color)',
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

  return (
    <>
      {/* 移动端汉堡按钮 */}
      <button
        type="button"
        className="mobile-menu-btn"
        onClick={() => setMobileOpen(true)}
        aria-label={t('打开菜单')}
      >
        <Menu size={18} strokeWidth={2} />
      </button>
      {/* 桌面端常驻侧栏（≤768px 由 CSS 隐藏） */}
      <div className="sidebar-desktop">{sidebarBody}</div>
      {/* 移动端抽屉 */}
      <MobileDrawer open={mobileOpen} onClose={() => setMobileOpen(false)} ariaLabel={t('导航菜单')}>
        {sidebarBody}
      </MobileDrawer>
    </>
  );
}
