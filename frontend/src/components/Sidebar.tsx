import React from 'react';
import { NavLink, useLocation, useNavigate } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import { api } from '../api';
import { isAdmin } from '../lib/utils';
import {
  LayoutDashboard, Satellite, KeyRound, ArrowLeftRight, CircleDollarSign,
  Users, Tags, Wallet, Receipt, Ticket, ScrollText, CreditCard, Bell,
  Settings, Play, Shield, Globe, Network, Zap, ChevronDown, Menu,
  Code2, BarChart3, UserCircle2,
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
  { path: '/playground', labelKey: 'Playground', icon: Play },
  { path: '/channels', labelKey: '渠道管理', icon: Satellite, adminOnly: true },
  { path: '/keys', labelKey: 'API 密钥', icon: KeyRound },
  { path: '/mappings', labelKey: '模型映射', icon: ArrowLeftRight, adminOnly: true },
  { path: '/logs', labelKey: '日志审计', icon: ScrollText, adminOnly: true },
  { path: '/security', labelKey: '安全监控', icon: Shield, adminOnly: true },
  { path: '/ip-management', labelKey: 'IP 管理', icon: Globe, adminOnly: true },
  { path: '/wallet', labelKey: '钱包充值', icon: Wallet },
  { path: '/orders', labelKey: '订单记录', icon: Receipt, adminOnly: true },
  { path: '/redemptions', labelKey: '兑换码', icon: Ticket, adminOnly: true },
  { path: '/users', labelKey: '用户管理', icon: Users, adminOnly: true },
  { path: '/groups', labelKey: '用户分组', icon: Tags, adminOnly: true },
  { path: '/pricing', labelKey: '定价倍率', icon: CircleDollarSign, adminOnly: true },
  { path: '/epay', labelKey: '易支付', icon: CreditCard, adminOnly: true },
  { path: '/notify', labelKey: '通知设置', icon: Bell, adminOnly: true },
  { path: '/settings', labelKey: '系统设置', icon: Settings, adminOnly: true },
  { path: '/network-layer', labelKey: '网络层概览', icon: Network, adminOnly: true },
];

// 参照 app.ofox.ai 的分组平铺设计：短分组标签 + 组内直接平铺，
// 不再用「更多管理」大折叠组。普通用户只看到无 adminOnly 的项。
const navGroups: NavGroup[] = [
  {
    key: 'develop',
    labelKey: '开发',
    icon: Code2,
    items: [
      navItems[0],  // 仪表盘
      navItems[1],  // Playground
      navItems[2],  // 渠道管理
      navItems[3],  // API 密钥
      navItems[4],  // 模型映射
    ],
  },
  {
    key: 'usage',
    labelKey: '用量',
    icon: BarChart3,
    items: [
      navItems[5],  // 日志审计
      navItems[6],  // 安全监控
      navItems[7],  // IP 管理
    ],
  },
  {
    key: 'account',
    labelKey: '账户',
    icon: UserCircle2,
    items: [
      navItems[8],  // 钱包充值
      navItems[9],  // 订单记录
      navItems[10], // 兑换码
    ],
  },
  {
    key: 'admin',
    labelKey: '管理',
    icon: Settings,
    items: [
      navItems[11], // 用户管理
      navItems[12], // 用户分组
      navItems[13], // 定价倍率
      navItems[14], // 易支付
      navItems[15], // 通知设置
      navItems[16], // 系统设置
      navItems[17], // 网络层概览
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

  // 分组折叠状态：默认全展开（ofox 风格平铺）；
  // 当前路由所在组始终视为展开，但用户点击组头仍可手动收起。
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
      const next: Record<string, boolean> = { ...prev };
      next[key] = !next[key];
      try { localStorage.setItem('sidebar_collapsed', JSON.stringify(next)); } catch {}
      return next;
    });
  };

  // 判断分组是否含当前激活项（用于高亮组头）
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
          color: 'white',
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

      {/* Nav */}
      <nav style={{ flex: 1, overflowY: 'auto', display: 'flex', flexDirection: 'column', gap: '8px' }}>
        {navGroups.map((group) => {
          // 角色过滤：普通用户不渲染管理员专属菜单；整组为空则隐藏
          const visibleItems = roleFilter(group.items);
          if (visibleItems.length === 0) return null;
          const collapsed = collapsedGroups[group.key] ?? false;
          const groupActive = isGroupActive(visibleItems);
          return (
            <div key={group.key} style={{ display: 'flex', flexDirection: 'column', gap: '2px' }}>
              {/* 分组标签（ofox 风格：小号大写 muted 标签） */}
              <button
                onClick={() => toggleGroup(group.key)}
                aria-expanded={!collapsed}
                style={{
                  display: 'flex',
                  alignItems: 'center',
                  gap: '6px',
                  padding: '4px 10px',
                  cursor: 'pointer',
                  fontSize: '10.5px',
                  fontWeight: 600,
                  letterSpacing: '0.08em',
                  textTransform: 'uppercase',
                  color: groupActive ? 'var(--accent-color)' : 'var(--text-muted)',
                  background: 'transparent',
                  border: 'none',
                  width: '100%',
                  textAlign: 'left',
                  transition: 'color 0.2s ease',
                }}
              >
                {group.icon ? <group.icon size={13} strokeWidth={2} /> : null}
                <span style={{ flex: 1 }}>{t(group.labelKey || '')}</span>
                <span style={{
                  display: 'inline-flex',
                  transition: 'transform 0.2s ease',
                  transform: collapsed ? 'rotate(-90deg)' : 'rotate(0deg)',
                  opacity: 0.55,
                }}>
                  <ChevronDown size={12} strokeWidth={2} />
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
                    padding: '6px 10px 6px 12px',
                    borderRadius: '8px',
                    cursor: 'pointer',
                    fontSize: '12.5px',
                    fontWeight: 500,
                    color: isActive ? 'var(--text-main)' : 'var(--text-muted)',
                    background: isActive ? 'rgba(47, 111, 237, 0.12)' : 'transparent',
                    boxShadow: isActive
                      ? 'inset 2px 0 0 var(--accent-color)'
                      : 'none',
                    textDecoration: 'none',
                    transition: 'background 0.15s ease, color 0.15s ease',
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
              <path d="M2 12h20M12 2a15.3 15.3 0 0 1 4 10 15.3 15.3 0 0 1-4 10 15.3 15.3 0 0 1-4-10 15.3 15.3 0 0 1-4-10z" />
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
