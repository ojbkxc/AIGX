import React from 'react';
import { NavLink, useLocation, useNavigate } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import { cycleTheme } from '../lib/theme';
import { api } from '../api';
import { isAdmin } from '../lib/utils';
import {
  LayoutDashboard, Satellite, KeyRound, ArrowLeftRight, CircleDollarSign,
  Users, Tags, Wallet, Receipt, Ticket, ScrollText, CreditCard, Bell,
  Settings, Play, Shield, Globe, Network, Zap, ChevronDown, Menu,
  Code2, BarChart3, UserCircle2, UserRound, MessageSquare, PanelLeftClose,
  PanelLeftOpen,
} from 'lucide-react';
import type { LucideIcon } from 'lucide-react';
import MobileDrawer from './ui/MobileDrawer';
import GlobalSearch from './GlobalSearch';

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
  adminOnly?: boolean;
  items: NavItem[];
}

const navItems: NavItem[] = [
  { path: '/', labelKey: '仪表盘', icon: LayoutDashboard, end: true },
  { path: '/playground', labelKey: 'Playground', icon: Play },
  { path: '/chat', labelKey: '聊天', icon: MessageSquare },
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
  { path: '/profile', labelKey: '个人中心', icon: UserRound },
];

// 参照 app.ofox.ai 的分组平铺设计：短分组标签 + 组内直接平铺。
// 角色分层对齐 new-api：
// - 普通用户（user）：开发组只有 Playground/API 密钥，账户组含钱包 + 个人中心。
// - 管理员（admin）：开发组追加渠道/映射，用量/管理组完整展开。
const navGroups: NavGroup[] = [
  {
    key: 'develop',
    labelKey: '开发',
    icon: Code2,
    items: [
      navItems[0],  // 仪表盘
      navItems[1],  // Playground
      navItems[2],  // 聊天
      navItems[3],  // 渠道管理
      navItems[4],  // API 密钥
      navItems[5],  // 模型映射
    ],
  },
  {
    key: 'usage',
    labelKey: '用量',
    icon: BarChart3,
    adminOnly: true,
    items: [
      navItems[6],  // 日志审计
      navItems[7],  // 安全监控
      navItems[8],  // IP 管理
    ],
  },
  {
    key: 'account',
    labelKey: '账户',
    icon: UserCircle2,
    items: [
      navItems[9],  // 钱包充值
      navItems[10], // 订单记录
      navItems[11], // 兑换码
      navItems[19], // 个人中心
    ],
  },
  {
    key: 'admin',
    labelKey: '管理',
    icon: Settings,
    adminOnly: true,
    items: [
      navItems[12], // 用户管理
      navItems[13], // 用户分组
      navItems[14], // 定价倍率
      navItems[15], // 易支付
      navItems[16], // 通知设置
      navItems[17], // 系统设置
      navItems[18], // 网络层概览
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
  // Ctrl/Cmd+K 全局搜索面板
  const [searchOpen, setSearchOpen] = React.useState<boolean>(false);
  // 侧边栏展开/收缩（Open WebUI 形态）：收缩后仅保留窄图标栏
  const [collapsed, setCollapsed] = React.useState<boolean>(() => {
    try {
      return localStorage.getItem('sidebar_collapsed_global') === 'true';
    } catch {
      return false;
    }
  });

  // 切换展开/收缩并持久化（全局布局联动 .main-content 边距）
  const toggleCollapsed = (): void => {
    setCollapsed((prev) => {
      const next = !prev;
      try { localStorage.setItem('sidebar_collapsed_global', next ? 'true' : 'false'); } catch { /* 忽略持久化失败 */ }
      document.documentElement.dataset.sidebarCollapsed = next ? 'true' : 'false';
      return next;
    });
  };

  // 同步 data 属性（刷新后保持收缩态；随 collapsed 变化保持全局布局一致）
  React.useEffect(() => {
    document.documentElement.dataset.sidebarCollapsed = collapsed ? 'true' : 'false';
  }, [collapsed]);

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

  // Ctrl/Cmd+K 唤起全局搜索
  React.useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'k') {
        e.preventDefault();
        setSearchOpen((v) => !v);
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, []);

  const toggleGroup = (key: string): void => {
    setCollapsedGroups((prev) => {
      const next: Record<string, boolean> = { ...prev };
      next[key] = !next[key];
      try { localStorage.setItem('sidebar_collapsed', JSON.stringify(next)); } catch { /* 忽略持久化失败 */ }
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
    void cycleTheme();
  };

  const toggleLanguage = (): void => {
    const next = i18n.language === 'zh' ? 'en' : 'zh';
    localStorage.setItem('i18n_lang', next);
    i18n.changeLanguage(next);
  };

  const email = localStorage.getItem('email') || 'Admin';
  const username = localStorage.getItem('username') || '';

  // 侧栏主体：桌面端常驻 fixed；移动端由 CSS media query 隐藏、抽屉承载。
  // 收缩态（collapsed）：仅渲染图标栏（Open WebUI 收起形态），文字/hover 交给 CSS。
  const sidebarBody = (
    <aside
      className={`sidebar-aside ${collapsed ? 'sidebar-aside-collapsed' : ''}`}
      style={{
        width: collapsed ? 'var(--sidebar-width-collapsed)' : 'var(--sidebar-width)',
        background: 'var(--sidebar-bg)',
        borderRight: '1px solid var(--border-color)',
        display: 'flex',
        flexDirection: 'column',
        padding: collapsed ? '14px 6px' : '14px 10px',
        position: 'fixed',
        top: 0,
        bottom: 0,
        left: 0,
        zIndex: 100,
        transition: 'width 0.25s cubic-bezier(0.22, 1, 0.36, 1), padding 0.25s cubic-bezier(0.22, 1, 0.36, 1)',
        overflow: 'hidden',
      }}
    >
      {/* Logo + 收缩开关 */}
      <div className={`sidebar-head ${collapsed ? 'sidebar-head-collapsed' : ''}`} style={{
        display: 'flex',
        alignItems: 'center',
        gap: '10px',
        marginBottom: '14px',
        paddingLeft: '6px',
        minHeight: '26px',
      }}>
        <div style={{
          width: '26px',
          height: '26px',
          minWidth: '26px',
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
        <div className="sidebar-head-text" style={{
          display: 'flex',
          flexDirection: 'column',
          minWidth: 0,
          flex: 1,
          whiteSpace: 'nowrap',
          opacity: collapsed ? 0 : 1,
          transition: 'opacity 0.15s ease',
        }}>
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
        {!collapsed ? (
          <button
            type="button"
            className="sidebar-collapse-btn"
            onClick={toggleCollapsed}
            title={t('收起侧边栏')}
            aria-label={t('收起侧边栏')}
          >
            <PanelLeftClose size={14} />
          </button>
        ) : (
          <button
            type="button"
            className="sidebar-collapse-btn"
            onClick={toggleCollapsed}
            title={t('展开侧边栏')}
            aria-label={t('展开侧边栏')}
          >
            <PanelLeftOpen size={14} />
          </button>
        )}
      </div>

      {/* Nav */}
      <nav className={`sidebar-nav ${collapsed ? 'sidebar-nav-collapsed' : ''}`} style={{
        flex: 1,
        overflowY: 'auto',
        display: 'flex',
        flexDirection: 'column',
        gap: '8px',
        alignItems: collapsed ? 'center' : 'stretch',
      }}>
        {navGroups.map((group) => {
          // 角色过滤：普通用户不渲染管理员专属菜单；整组为空则隐藏
          if (group.adminOnly && !isAdmin()) return null;
          const visibleItems = roleFilter(group.items);
          if (visibleItems.length === 0) return null;
          const groupCollapsed = collapsedGroups[group.key] ?? false;
          const groupActive = isGroupActive(visibleItems);
          return (
            <div key={group.key} className="sidebar-group" style={{ display: 'flex', flexDirection: 'column', gap: '2px', width: '100%' }}>
              {/* 收缩态：分组图标置顶，各菜单项仅图标 + hover tooltip */}
              {collapsed ? (
                <>
                  <div className="sidebar-group-icon" title={t(group.labelKey || '')}>
                    {group.icon ? <group.icon size={16} strokeWidth={1.8} /> : <span className="sidebar-group-dot" />}
                  </div>
                  {visibleItems.map((item) => (
                    <NavLink
                      key={item.path}
                      to={item.path}
                      end={item.end}
                      className={({ isActive }) => `sidebar-icon-btn ${isActive ? 'active' : ''}`}
                      title={t(item.labelKey)}
                    >
                      <item.icon size={16} strokeWidth={1.8} />
                    </NavLink>
                  ))}
                </>
              ) : (
                <>
                  {/* 分组标签（ofox 风格：小号大写 muted 标签） */}
                  <button
                    onClick={() => toggleGroup(group.key)}
                    aria-expanded={!groupCollapsed}
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
                      transform: groupCollapsed ? 'rotate(-90deg)' : 'rotate(0deg)',
                      opacity: 0.55,
                    }}>
                      <ChevronDown size={12} strokeWidth={2} />
                    </span>
                  </button>
                  {!groupCollapsed && visibleItems.map((item) => (
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
                </>
              )}
            </div>
          );
        })}
      </nav>

      {/* Footer */}
      <div className={`sidebar-footer ${collapsed ? 'sidebar-footer-collapsed' : ''}`} style={{
        display: 'flex',
        flexDirection: 'column',
        gap: '10px',
        borderTop: '1px solid var(--border-color)',
        paddingTop: '12px',
      }}>
        <div style={{ display: 'flex', alignItems: 'center', gap: '8px', padding: '0 4px', justifyContent: collapsed ? 'center' : 'flex-start' }}>
          <div style={{
            width: '24px',
            height: '24px',
            minWidth: '24px',
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
          {!collapsed && (
            <div style={{ display: 'flex', flexDirection: 'column', overflow: 'hidden' }}>
              <span style={{ fontSize: '12px', fontWeight: 500, color: 'var(--text-main)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                {email}
              </span>
              {username && <span style={{ fontSize: '10px', color: 'var(--text-muted)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                {username}
              </span>}
            </div>
          )}
        </div>
        {!collapsed ? (
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
        ) : (
          <div style={{ display: 'flex', flexDirection: 'column', alignItems: 'center', gap: '8px' }}>
            <button
              className="sidebar-icon-btn"
              onClick={toggleTheme}
              title={t('切换主题')}
              aria-label={t('切换主题')}
            >
              <svg xmlns="http://www.w3.org/2000/svg" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth="2" style={{ width: '15px', height: '15px' }}>
                <path d="M12 3a6 6 0 0 0 9 9 9 9 0 1 1-9-9Z" />
              </svg>
            </button>
            <button
              className="sidebar-icon-btn"
              onClick={toggleLanguage}
              title={i18n.language === 'zh' ? 'EN' : '中'}
              aria-label={i18n.language === 'zh' ? 'EN' : '中'}
            >
              <svg xmlns="http://www.w3.org/2000/svg" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth="2" style={{ width: '15px', height: '15px' }}>
                <circle cx="12" cy="12" r="10" />
                <path d="M2 12h20M12 2a15.3 15.3 0 0 1 4 10 15.3 15.3 0 0 1-4 10 15.3 15.3 0 0 1-4-10 15.3 15.3 0 0 1-4-10z" />
              </svg>
            </button>
            <button
              className="sidebar-icon-btn sidebar-icon-danger"
              onClick={handleLogout}
              title={t('退出')}
              aria-label={t('退出')}
            >
              <svg xmlns="http://www.w3.org/2000/svg" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth="2" style={{ width: '15px', height: '15px' }}>
                <path d="M9 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h4M16 17l5-5-5-5M21 12H9" />
              </svg>
            </button>
          </div>
        )}
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
      <GlobalSearch
        open={searchOpen}
        onClose={() => setSearchOpen(false)}
        navItems={navItems.map(({ path, labelKey, adminOnly }) => ({ path, labelKey, adminOnly }))}
      />
    </>
  );
}
