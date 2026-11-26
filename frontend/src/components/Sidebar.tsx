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
  Puzzle, Route as RouteIcon, Flame, Wrench, CalendarClock, Bell, Gauge,
  EyeOff, RotateCcw,
} from 'lucide-react';
import type { LucideIcon } from 'lucide-react';
import MobileDrawer from './ui/MobileDrawer';
import GlobalSearch from './GlobalSearch';
import ConfirmDialog, { type ConfirmState } from './ConfirmDialog';
import './Sidebar.css';

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
  { path: '/maintenance', labelKey: '自动维护', icon: CalendarClock, adminOnly: true },
  { path: '/limit', labelKey: '限流', icon: Gauge, adminOnly: true },
  // 通知（A2A 对齐：从「系统设置」独立成顶级页）
  { path: '/notify', labelKey: '通知', icon: Bell, adminOnly: true },
  { path: '/agent', labelKey: 'AI 运维', icon: Bot, adminOnly: true },
  { path: '/wallet', labelKey: '钱包', icon: Wallet },
  { path: '/tickets', labelKey: '工单', icon: Headphones },
  { path: '/profile', labelKey: '个人中心', icon: UserRound },
  // 网络代理池（集中管理出口：账号 proxy 字段 pool:/rotate 编码引用）
  { path: '/proxy-pool', labelKey: '网络代理', icon: Globe, adminOnly: true },
  // 插件管理（契约对齐 Agent2API 插件页；后端建设中整页降级空态，就绪即生效）
  { path: '/plugins', labelKey: '插件管理', icon: Puzzle, adminOnly: true },
  // 工具服务（A2A v3.5.0-beta.1 对齐）：MCP & Skills 接入 / 搜索提供商 / 视觉分析绑定
  { path: '/tools', labelKey: 'MCP & Skills', icon: Wrench, adminOnly: true },
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
// - 签到中心 → 并入「供应商」页「签到」Tab（每日额度领取与账号池强耦合）
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
      navItemOf('/purge'),           // 破甲控制台（请求治理总控）
      navItemOf('/model-manage'),    // 模型管理（admin 可写；普通用户只读自己分组可见的模型与价格）
      navItemOf('/channels'),        // 供应商（统一列表 + 账号池 / 健康 / 签到 子 Tab）
      navItemOf('/proxy-pool'),      // 网络代理（出口代理池）
      navItemOf('/users'),           // 用户管理
      navItemOf('/products'),        // 套餐与兑换码（双 Tab）
      navItemOf('/routing'),         // 路由（分层档案 / 预演 / 回放 / 粘滞）
      // 签到中心 → 并入「供应商」页「签到」Tab（每日额度领取与账号池强耦合），顶级菜单下线
      // 定时任务 → 升级为顶级「自动维护」页（beta.7 对齐上游），入口见 /maintenance
      // 系统诊断 → 并入「供应商」页「健康」Tab（07 E.3：诊断三件套
      // 全部以渠道为粒度，看到问题就在旁边修），顶级菜单下线
      // 文档 → 并入「API 密钥」第二 Tab「接入文档」（07 E.3：密钥与文档是
      // 同一件事的两半；且全员可见——普通用户也要知道怎么填客户端），顶级菜单下线
      navItemOf('/plugins'),        // 插件管理（后端建设中降级空态）
      navItemOf('/tools'),          // 工具服务（Skill / MCP 接入、搜索提供商、视觉分析）
      navItemOf('/maintenance'),    // 自动维护（凭证/模型/软件/插件检查 + 记录）
      navItemOf('/limit'),          // 限流（从设置页迁出为顶级页：RPM/TPM + 缓存 + 运行容量）
      navItemOf('/notify'),         // 通知（从系统设置迁出为顶级页，对齐 A2A）
      navItemOf('/settings'),        // 系统设置
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

// ── 分组自定义（对齐 A2A sidebar-pane 的分组面板，只做其中两件）──────────
// A2A 的侧栏分组有拖动排序 / 重命名 / 隐藏组 / 组内计数四件；AIGX 这一波只落
// 「组内计数 + 隐藏组」：拖动排序与重命名需要一套完整的布局编辑态（拖动把手、
// 跨组落点、重命名输入、恢复默认），不是本波体量能做完的，宁可不做也不做浅。
//
// 持久化：隐藏的是**组 key**（'general' / 'admin' / 'personal'），不是下标 ——
// 下标会随菜单增删漂移，隐藏状态就会静默落到别的组上（与 navGroups 用
// navItemOf(path) 而非下标取条目同一个理由）。
//
// 恢复入口必须存在：隐藏整组之后若不提示，用户会以为菜单没了（A2A 的
// 「已隐藏 N 组」提示条同理）。这里在导航底部常驻一行「已隐藏 N 组 · 恢复」。
const HIDDEN_GROUPS_KEY = 'aigx-sidebar-hidden-groups';

function readHiddenGroups(): string[] {
  try {
    const raw = localStorage.getItem(HIDDEN_GROUPS_KEY);
    if (!raw) return [];
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    // 只收仍在 navGroups 里的 key：菜单下线后残留的 key 不该继续占位
    const known = new Set(navGroups.map((g) => g.key));
    return parsed.filter((k): k is string => typeof k === 'string' && known.has(k));
  } catch {
    return [];
  }
}

function writeHiddenGroups(keys: string[]): void {
  try {
    localStorage.setItem(HIDDEN_GROUPS_KEY, JSON.stringify(keys));
  } catch {
    /* 存储不可用只影响下次打开，本次会话照常生效 */
  }
}

interface SidebarContentProps {
  collapsed: boolean;
  onToggleCollapsed: () => void;
  /** 移动端抽屉形态：隐藏收缩按钮（抽屉本身即开合） */
  isDrawer: boolean;
  /** 被隐藏的分组 key（桌面栏与移动抽屉共用同一份状态） */
  hiddenGroups: string[];
  onToggleGroup: (key: string) => void;
  onRestoreGroups: () => void;
}

/** 侧栏内容（桌面常驻 / 移动抽屉共用） */
function SidebarContent({ collapsed, onToggleCollapsed, isDrawer, hiddenGroups, onToggleGroup, onRestoreGroups }: SidebarContentProps): JSX.Element {
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

  // 「已隐藏 N 组」只数当前角色真能看见的组：普通用户看不到管理组，
  // 管理员隐藏了「管理」后切到普通账号，提示条不该冒出个点不动的计数。
  const visibleHiddenCount = navGroups.filter((g) => {
    if (g.adminOnly && !isAdmin()) return false;
    if (roleFilter(g.items).length === 0) return false;
    return hiddenGroups.includes(g.key);
  }).length;

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
          // 隐藏组：整组不渲染（恢复入口在 nav 底部，见下）
          if (hiddenGroups.includes(group.key)) return null;
          const label = t(group.labelKey || '');
          return (
            <div key={group.key} className="sidebar-group">
              <div className="sidebar-group-head">
                <div className="sidebar-group-label" title={label}>
                  {label}
                </div>
                {/* 组内计数：A2A 分组面板的「{n} 项 · {shown} 显示」，这里只剩「项」——
                    可见性由「隐藏整组」承载，没有逐项隐藏，两个数会永远相等 */}
                <span className="sidebar-group-count" title={t('{{n}} 项', { n: visibleItems.length })}>
                  {visibleItems.length}
                </span>
                <button
                  type="button"
                  className="sidebar-group-hide"
                  title={t('隐藏「{{name}}」分组', { name: label })}
                  aria-label={t('隐藏「{{name}}」分组', { name: label })}
                  onClick={(e) => { e.stopPropagation(); onToggleGroup(group.key); }}
                >
                  <EyeOff size={12} strokeWidth={1.8} />
                </button>
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
        {/* 恢复入口：隐藏整组后必须给一条回来的路（A2A 的「已隐藏 N 组」提示条同位） */}
        {visibleHiddenCount > 0 && (
          <button type="button" className="sidebar-restore-groups" onClick={onRestoreGroups}>
            <RotateCcw size={12} strokeWidth={1.8} />
            <span>{t('已隐藏 {{n}} 组 · 恢复', { n: visibleHiddenCount })}</span>
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
  // 分组隐藏状态提到这里（而不是各 SidebarContent 自己 useState）：
  // 桌面栏与移动抽屉是同一个组件的两个实例，状态留在实例里就会出现
  // 「抽屉里隐藏了、桌面栏还显示」。
  const [hiddenGroups, setHiddenGroups] = React.useState<string[]>(readHiddenGroups);

  const toggleGroup = (key: string): void => {
    setHiddenGroups((prev) => {
      const next = prev.includes(key) ? prev.filter((k) => k !== key) : [...prev, key];
      writeHiddenGroups(next);
      return next;
    });
  };

  const restoreGroups = (): void => {
    setHiddenGroups([]);
    writeHiddenGroups([]);
  };

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
          hiddenGroups={hiddenGroups} onToggleGroup={toggleGroup} onRestoreGroups={restoreGroups} />
      </div>
      {/* 移动端抽屉（恒为展开形态） */}
      <MobileDrawer open={mobileOpen} onClose={() => setMobileOpen(false)} ariaLabel={t('导航菜单')}>
        <SidebarContent collapsed={false} onToggleCollapsed={toggleCollapsed} isDrawer
          hiddenGroups={hiddenGroups} onToggleGroup={toggleGroup} onRestoreGroups={restoreGroups} />
      </MobileDrawer>
      <GlobalSearch
        open={searchOpen}
        onClose={() => setSearchOpen(false)}
        navItems={navItems.map(({ path, labelKey, adminOnly }) => ({ path, labelKey, adminOnly }))}
      />
    </>
  );
}
