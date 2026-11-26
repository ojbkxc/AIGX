import React from 'react';
import { NavLink, useLocation, useNavigate } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import { SUPPORTED_LANGS, setLanguage } from '../i18n';
import { cycleTheme } from '../lib/theme';
import { api } from '../api';
import { isAdmin } from '../lib/utils';
import {
  ChevronDown, Menu,
  PanelLeftClose, PanelLeftOpen, Sun, Languages, LogOut,
  EyeOff, RotateCcw,
} from 'lucide-react';
import MobileDrawer from './ui/MobileDrawer';
import GlobalSearch from './GlobalSearch';
import ConfirmDialog, { type ConfirmState } from './ConfirmDialog';
import { useSidebarLayout } from '../hooks/useSidebarLayout';
import {
  SIDEBAR_MENUS,
  cloneLayout,
  groupHasFixed,
  isBuiltinGroupLabel,
  menuOf,
  setGroupHidden,
  type LayoutGroup,
  type SidebarLayout,
  type SidebarMenu,
} from '../lib/sidebar-layout';
import './Sidebar.css';

// 角色过滤：普通用户仅见无 adminOnly 标记的菜单（展示层过滤，权限由后端强制）。
function roleFilter(menus: SidebarMenu[]): SidebarMenu[] {
  return isAdmin() ? menus : menus.filter((m) => !m.adminOnly);
}

/** 一个分组在本角色下「可见的菜单」（已去掉单独隐藏项）。 */
function visibleMenusOf(group: LayoutGroup, layout: SidebarLayout): SidebarMenu[] {
  const menus = group.items
    .map((key) => menuOf(key))
    .filter((m): m is SidebarMenu => Boolean(m));
  return roleFilter(menus).filter((m) => !layout.hidden.includes(m.key));
}

/** 一个分组在本角色下「拥有的菜单」（含被单独隐藏项，用于判断该不该显示组）。 */
function ownedMenusOf(group: LayoutGroup): SidebarMenu[] {
  const menus = group.items
    .map((key) => menuOf(key))
    .filter((m): m is SidebarMenu => Boolean(m));
  return roleFilter(menus);
}

/** 恢复所有隐藏：清空逐项隐藏 + 取消整组隐藏，保留顺序与自建分组。 */
function restoreHidden(layout: SidebarLayout): SidebarLayout {
  const next = cloneLayout(layout);
  next.hidden = [];
  for (const group of next.groups) group.hidden = false;
  return next;
}

interface SidebarContentProps {
  collapsed: boolean;
  onToggleCollapsed: () => void;
  /** 移动端抽屉形态：隐藏收缩按钮（抽屉本身即开合） */
  isDrawer: boolean;
  layout: SidebarLayout;
  onApply: (next: SidebarLayout | null) => boolean;
}

/** 侧栏内容（桌面常驻 / 移动抽屉共用） */
function SidebarContent({ collapsed, onToggleCollapsed, isDrawer, layout, onApply }: SidebarContentProps): JSX.Element {
  const navigate = useNavigate();
  const { t, i18n } = useTranslation();
  const [userMenuOpen, setUserMenuOpen] = React.useState<boolean>(false);

  // 点击外部关闭用户菜单
  React.useEffect(() => {
    if (!userMenuOpen) return;
    const onDoc = (): void => setUserMenuOpen(false);
    document.addEventListener('click', onDoc);
    return () => document.removeEventListener('click', onDoc);
  }, [userMenuOpen]);

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

  // 退出登录二次确认：误触不应直接登出丢上下文
  const [logoutConfirmOpen, setLogoutConfirmOpen] = React.useState<boolean>(false);

  const toggleTheme = (): void => {
    void cycleTheme();
  };

  const toggleLanguage = (): void => {
    const idx = SUPPORTED_LANGS.findIndex((l) => l.code === (i18n.language || 'zh'));
    const next = SUPPORTED_LANGS[(idx + 1) % SUPPORTED_LANGS.length].code;
    setLanguage(next);
  };

  const email = localStorage.getItem('email') || 'Admin';
  const username = localStorage.getItem('username') || '';

  // 「已隐藏 N 项」只数当前角色真能看见的组/项：普通用户看不到管理组，
  // 管理员隐藏了「管理」后切到普通账号，提示条不该冒出个点不动的计数。
  const hiddenGroups = layout.groups.filter(
    (g) => g.hidden && ownedMenusOf(g).length > 0,
  ).length;
  const hiddenItems = layout.hidden.filter((key) => {
    const menu = menuOf(key);
    return Boolean(menu) && roleFilter([menu as SidebarMenu]).length > 0;
  }).length;
  const hiddenCount = hiddenGroups + hiddenItems;

  // 自定义分组的名字：内置名查词典，自建名原样显示（用户自己写的字）。
  const groupTitle = (group: LayoutGroup): string =>
    isBuiltinGroupLabel(group.label) ? t(group.label) : group.label;

  return (
    <aside
      className={`sidebar-aside ${collapsed ? 'sidebar-aside-collapsed' : ''} ${isDrawer ? 'sidebar-aside-drawer' : ''}`}
    >
      {/* Logo（收缩态仅剩居中 logo；容器透明保留 PNG 半透明） */}
      <div className="sidebar-head">
        <div className="sidebar-logo sidebar-logo-img">
          <img src="/logo.png" alt="AIGX" width={22} height={22} style={{ borderRadius: 4 }} />
        </div>
        <div className="sidebar-head-text">
          <div className="sidebar-title">AIGX</div>
          <div className="sidebar-subtitle">{t('AI 中转网关')}</div>
        </div>
      </div>

      {/* Nav：展开态分组平铺；收缩态仅图标 + title tooltip */}
      <nav className="sidebar-nav">
        {layout.groups.map((group) => {
          const owned = ownedMenusOf(group);
          if (owned.length === 0) return null;        // 本角色看不到这一组
          if (group.hidden) return null;              // 整组隐藏（恢复入口在 nav 底部）
          const visibleItems = visibleMenusOf(group, layout);
          if (visibleItems.length === 0) return null; // 组内全被隐藏，画空组会像加载失败
          const label = groupTitle(group);
          return (
            <div key={group.id} className="sidebar-group">
              <div className="sidebar-group-head">
                <div className="sidebar-group-label" title={label}>
                  {label}
                </div>
                {/* 组内计数：只数本角色可见项 */}
                <span className="sidebar-group-count" title={t('{{n}} 项', { n: visibleItems.length })}>
                  {visibleItems.length}
                </span>
                {/* 含固定项的组不可整组隐藏：不渲染按钮，避免「点了没反应」 */}
                {!groupHasFixed(group) && (
                  <button
                    type="button"
                    className="sidebar-group-hide"
                    title={t('隐藏「{{name}}」分组', { name: label })}
                    aria-label={t('隐藏「{{name}}」分组', { name: label })}
                    onClick={(e) => { e.stopPropagation(); onApply(setGroupHidden(layout, group.id, true)); }}
                  >
                    <EyeOff size={12} strokeWidth={1.8} />
                  </button>
                )}
              </div>
              {visibleItems.map((item) => (
                <NavLink
                  key={item.key}
                  to={item.key}
                  end={item.end}
                  className={({ isActive }) => `nav-item ${isActive ? 'active' : ''}`}
                  title={t(item.labelKey)}
                >
                  <item.icon size={15} strokeWidth={1.8} />
                  <span>{t(item.labelKey)}</span>
                </NavLink>
              ))}
            </div>
          );
        })}
        {/* 恢复入口：隐藏整组/单项后必须给一条回来的路 */}
        {hiddenCount > 0 && (
          <button type="button" className="sidebar-restore-groups" onClick={() => onApply(restoreHidden(layout))}>
            <RotateCcw size={12} strokeWidth={1.8} />
            <span>{t('已隐藏 {{n}} 项 · 恢复', { n: hiddenCount })}</span>
          </button>
        )}
      </nav>

      {/* 唯一收缩开关：nav 与 footer 之间的固定行，整行（含提示文字）可点击 */}
      {!isDrawer && (
        <button
          type="button"
          className="sidebar-collapse-row"
          onClick={onToggleCollapsed}
          title={collapsed ? t('展开侧边栏') : t('收起侧边栏')}
          aria-label={collapsed ? t('展开侧边栏') : t('收起侧边栏')}
        >
          <span className="sidebar-collapse-btn">
            {collapsed ? <PanelLeftOpen size={15} /> : <PanelLeftClose size={15} />}
          </span>
          {!collapsed && <span className="sidebar-collapse-hint">{t('收起侧边栏')}</span>}
        </button>
      )}

      {/* Footer：用户区（收缩态仅头像）+ 收进菜单的主题/语言/退出 */}
      <div className="sidebar-footer">
        <div className="sidebar-user">
          <button
            type="button"
            className="sidebar-user-btn"
            title={t('账户菜单')}
            onClick={(e) => { e.stopPropagation(); setUserMenuOpen((v) => !v); }}
          >
            <span className="sidebar-avatar">{email.charAt(0).toUpperCase()}</span>
            {!collapsed && (
              <>
                <span className="sidebar-user-info">
                  <span className="sidebar-user-email">{email}</span>
                  {username && <span className="sidebar-user-name">{username}</span>}
                </span>
                <ChevronDown size={13} className="sidebar-user-chevron" />
              </>
            )}
          </button>
          {userMenuOpen && (
            <div className="sidebar-user-menu" onClick={(e) => e.stopPropagation()}>
              <button type="button" onClick={() => { setUserMenuOpen(false); toggleTheme(); }}>
                <Sun size={13} />
                <span>{t('切换主题')}</span>
              </button>
              <button type="button" onClick={() => { setUserMenuOpen(false); toggleLanguage(); }}>
                <Languages size={13} />
                <span>{t('切换语言')}</span>
                <span className="sidebar-user-menu-hint">{(() => {
                  const idx = SUPPORTED_LANGS.findIndex((l) => l.code === (i18n.language || 'zh'));
                  return SUPPORTED_LANGS[(idx + 1) % SUPPORTED_LANGS.length].label;
                })()}</span>
              </button>
              <button type="button" className="sidebar-user-menu-danger" onClick={() => { setUserMenuOpen(false); setLogoutConfirmOpen(true); }}>
                <LogOut size={13} />
                <span>{t('退出登录')}</span>
              </button>
            </div>
          )}
        </div>
      </div>

      {/* 退出登录二次确认：误触不应直接登出 */}
      <ConfirmDialog
        state={logoutConfirmOpen ? {
          title: t('退出登录'),
          message: t('确定要退出当前账号吗？'),
          confirmText: t('退出'),
          danger: true,
          onConfirm: () => handleLogout(),
        } as ConfirmState : null}
        onClose={() => setLogoutConfirmOpen(false)}
      />
    </aside>
  );
}

export default function Sidebar(): JSX.Element {
  const location = useLocation();
  const { t } = useTranslation();
  // 移动端抽屉开关：仅 ≤768px 由汉堡按钮触发
  const [mobileOpen, setMobileOpen] = React.useState<boolean>(false);
  // Ctrl/Cmd+K 全局搜索面板
  const [searchOpen, setSearchOpen] = React.useState<boolean>(false);
  // 侧边栏展开/收缩：唯一开关，收缩后为窄图标栏
  const [collapsed, setCollapsed] = React.useState<boolean>(() => {
    try {
      return localStorage.getItem('sidebar_collapsed_global') === 'true';
    } catch {
      return false;
    }
  });
  // 布局状态提到这里（而不是各 SidebarContent 自己 useState）：
  // 桌面栏与移动抽屉是同一个组件的两个实例，状态留在实例里就会出现
  // 「抽屉里改了、桌面栏还显示」。
  const { layout, apply } = useSidebarLayout();

  // 切换展开/收缩并持久化（全局布局联动 .main-content 边距）
  const toggleCollapsed = (): void => {
    setCollapsed((prev) => {
      const next = !prev;
      try { localStorage.setItem('sidebar_collapsed_global', next ? 'true' : 'false'); } catch { /* 忽略持久化失败 */ }
      return next;
    });
  };

  // 同步 data 属性（刷新后保持收缩态；随 collapsed 变化保持全局布局一致）
  React.useEffect(() => {
    document.documentElement.dataset.sidebarCollapsed = collapsed ? 'true' : 'false';
  }, [collapsed]);

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
      <div className="sidebar-desktop">
        <SidebarContent collapsed={collapsed} onToggleCollapsed={toggleCollapsed} isDrawer={false}
          layout={layout} onApply={apply} />
      </div>
      {/* 移动端抽屉（恒为展开形态） */}
      <MobileDrawer open={mobileOpen} onClose={() => setMobileOpen(false)} ariaLabel={t('导航菜单')}>
        <SidebarContent collapsed={false} onToggleCollapsed={toggleCollapsed} isDrawer
          layout={layout} onApply={apply} />
      </MobileDrawer>
      <GlobalSearch
        open={searchOpen}
        onClose={() => setSearchOpen(false)}
        navItems={SIDEBAR_MENUS.map(({ key, labelKey, adminOnly }) => ({ path: key, labelKey, adminOnly }))}
      />
    </>
  );
}
