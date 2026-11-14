import React from 'react';
import { NavLink, useLocation, useNavigate } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import { SUPPORTED_LANGS, setLanguage } from '../i18n';
import { cycleTheme } from '../lib/theme';
import { api } from '../api';
import { isAdmin } from '../lib/utils';
import {
  LayoutDashboard, Satellite, KeyRound,
  Users, Wallet, ScrollText, Package,
  Settings, ChevronDown, Menu, UserRound,
  MessageSquare, PanelLeftClose, PanelLeftOpen, Boxes, BookOpen, Sun,
  Languages, LogOut, Bot, Headphones, Globe,
  Puzzle, Route as RouteIcon, Flame,
} from 'lucide-react';
import type { LucideIcon } from 'lucide-react';
import MobileDrawer from './ui/MobileDrawer';
import GlobalSearch from './GlobalSearch';
import ConfirmDialog, { type ConfirmState } from './ConfirmDialog';

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
  adminOnly?: boolean;
  items: NavItem[];
}

const navItems: NavItem[] = [
  { path: '/', labelKey: '仪表盘', icon: LayoutDashboard, end: true },
  { path: '/chat', labelKey: '聊天', icon: MessageSquare },
  { path: '/prompts', labelKey: '提示词库', icon: BookOpen },
  { path: '/keys', labelKey: 'API 密钥', icon: KeyRound },
  { path: '/logs', labelKey: '用量日志', icon: ScrollText },
  { path: '/model-manage', labelKey: '模型管理', icon: Boxes, adminOnly: true },
  // 供应商页：统一列表（API Key 型 + 账号池型）+ 账号池 / 健康 / 签到 子 Tab
  { path: '/channels', labelKey: '供应商', icon: Satellite, adminOnly: true },
  { path: '/users', labelKey: '用户管理', icon: Users, adminOnly: true },
  { path: '/products', labelKey: '套餐与兑换码', icon: Package, adminOnly: true },
  // 路由域（#40）：分层档案 / 基础策略 / 预演 / 回放 / 粘滞单页多模式
  { path: '/routing', labelKey: '路由', icon: RouteIcon, adminOnly: true },
  // 破甲控制台：请求治理总控（归一化/改写/降档/异源/技能注入）
  { path: '/purge', labelKey: '破甲控制台', icon: Flame, adminOnly: true },
  { path: '/settings', labelKey: '系统设置', icon: Settings, adminOnly: true },
  { path: '/agent', labelKey: 'AI 运维', icon: Bot, adminOnly: true },
  { path: '/wallet', labelKey: '钱包', icon: Wallet },
  { path: '/tickets', labelKey: '工单', icon: Headphones },
  { path: '/profile', labelKey: '个人中心', icon: UserRound },
  // 网络代理池（集中管理出口：账号 proxy 字段 pool:/rotate 编码引用）
  { path: '/proxy-pool', labelKey: '网络代理', icon: Globe, adminOnly: true },
  // 插件管理（契约对齐 Agent2API 插件页；后端建设中整页降级空态，就绪即生效）
  { path: '/plugins', labelKey: '插件管理', icon: Puzzle, adminOnly: true },
];
// 分组平铺（new-api NavGroup 同款：短分组标签 + 组内平铺菜单，无折叠交互）。
// 管理员 16 项 / 客户 8 项。合并历史：
// - 模型映射 → 模型管理页统一维护（渠道表单里已移除，避免三处可改）
// - 易支付 + 用户分组 + 通知设置 + 安全监控 + IP 管理 → 系统设置 tab
// - 定价倍率曾并入系统设置计费 tab，现拆回独立页（/pricing）
// - 订单记录 → 钱包页内 tab
// - 系统信息（AI 网关网络层管理）→ 系统设置运维 tab 网络层子标签（已下沉）
// - 账号池 → 并入「供应商」页「账号池」Tab（账号池是供应商的凭证形态）
// - 签到中心 → 并入「供应商」页「签到」Tab（每日额度领取与账号池强耦合）
// - 用户工具权限 → 并入「用户管理」页「工具权限」Tab
// - 定时任务 → 并入「系统设置」分类（scheduled）
// - 定价倍率 → 并入模型管理「定价倍率」Tab（模型价格/别名/倍率是同一条记录的不同侧面）
// - 元信息覆盖 → 并入模型管理「模型目录」行内能力弹窗 + 工具栏「覆盖管理」
//
// 分组按路径取条目而非下标：下标一旦因增删菜单漂移，就会静默错配到别的菜单。
function navItemOf(path: string): NavItem {
  const found = navItems.find((it) => it.path === path);
  if (!found) throw new Error(`nav item not found: ${path}`);
  return found;
}

const navGroups: NavGroup[] = [
  {
    key: 'general',
    labelKey: '通用',
    items: [
      navItemOf('/'),          // 仪表盘
      navItemOf('/chat'),      // 聊天
      navItemOf('/prompts'),   // 提示词库
      navItemOf('/keys'),      // API 密钥
      navItemOf('/logs'),      // 用量日志（全员可见，admin 看全部，普通用户看自己）
    ],
  },
  {
    key: 'admin',
    labelKey: '管理',
    adminOnly: true,
    items: [
      navItemOf('/model-manage'),    // 模型管理（admin 可写；普通用户只读自己分组可见的模型与价格）
      navItemOf('/channels'),        // 供应商（统一列表 + 账号池 / 健康 / 签到 子 Tab）
      navItemOf('/proxy-pool'),      // 网络代理（出口代理池）
      navItemOf('/users'),           // 用户管理（含「工具权限」Tab）
      navItemOf('/products'),        // 套餐与兑换码（双 Tab）
      navItemOf('/routing'),         // 路由（分层档案 / 预演 / 回放 / 粘滞）
      navItemOf('/purge'),           // 破甲控制台（请求治理总控）
      // 签到中心 → 并入「供应商」页「签到」Tab（每日额度领取与账号池强耦合），顶级菜单下线
      // 用户工具权限 → 并入「用户管理」页「工具权限」Tab，顶级菜单下线
      // 定时任务 → 并入「系统设置」分类（scheduled），顶级菜单下线
      // 系统诊断 → 并入「供应商」页「健康」Tab（07 E.3：诊断三件套
      // 全部以渠道为粒度，看到问题就在旁边修），顶级菜单下线
      // 文档 → 并入「API 密钥」第二 Tab「接入文档」（07 E.3：密钥与文档是
      // 同一件事的两半；且全员可见——普通用户也要知道怎么填客户端），顶级菜单下线
      navItemOf('/plugins'),        // 插件管理（后端建设中降级空态）
      navItemOf('/settings'),        // 系统设置（含「定时任务」分类）
      navItemOf('/agent'),           // AI 运维
    ],
  },
  {
    key: 'personal',
    labelKey: '个人',
    items: [
      navItemOf('/wallet'),  // 钱包
      navItemOf('/tickets'), // 工单
      navItemOf('/profile'), // 个人中心
    ],
  },
];

// 角色过滤：普通用户仅见无 adminOnly 标记的菜单（展示层过滤，权限由后端强制）
function roleFilter(items: NavItem[]): NavItem[] {
  if (isAdmin()) return items;
  return items.filter((it) => !it.adminOnly);
}

interface SidebarContentProps {
  collapsed: boolean;
  onToggleCollapsed: () => void;
  /** 移动端抽屉形态：隐藏收缩按钮（抽屉本身即开合） */
  isDrawer: boolean;
}

/** 侧栏内容（桌面常驻 / 移动抽屉共用） */
function SidebarContent({ collapsed, onToggleCollapsed, isDrawer }: SidebarContentProps): JSX.Element {
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
        {navGroups.map((group) => {
          if (group.adminOnly && !isAdmin()) return null;
          const visibleItems = roleFilter(group.items);
          if (visibleItems.length === 0) return null;
          return (
            <div key={group.key} className="sidebar-group">
              <div className="sidebar-group-label" title={t(group.labelKey || '')}>
                {t(group.labelKey || '')}
              </div>
              {visibleItems.map((item) => (
                <NavLink
                  key={item.path}
                  to={item.path}
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
        <SidebarContent collapsed={collapsed} onToggleCollapsed={toggleCollapsed} isDrawer={false} />
      </div>
      {/* 移动端抽屉（恒为展开形态） */}
      <MobileDrawer open={mobileOpen} onClose={() => setMobileOpen(false)} ariaLabel={t('导航菜单')}>
        <SidebarContent collapsed={false} onToggleCollapsed={toggleCollapsed} isDrawer />
      </MobileDrawer>
      <GlobalSearch
        open={searchOpen}
        onClose={() => setSearchOpen(false)}
        navItems={navItems.map(({ path, labelKey, adminOnly }) => ({ path, labelKey, adminOnly }))}
      />
    </>
  );
}
