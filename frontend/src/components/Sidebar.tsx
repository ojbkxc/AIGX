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
  EyeOff, Pencil, GripVertical, RotateCcw,
} from 'lucide-react';
import MobileDrawer from './ui/MobileDrawer';
import GlobalSearch from './GlobalSearch';
import ConfirmDialog, { type ConfirmState } from './ConfirmDialog';
import { useSidebarLayout } from '../hooks/useSidebarLayout';
import {
  SIDEBAR_MENUS,
  cloneLayout,
  groupHasFixed,
  groupIsCollapsed,
  isBuiltinGroupLabel,
  menuOf,
  setGroupCollapsed,
  setGroupHidden,
  type LayoutGroup,
  type SidebarLayout,
  type SidebarMenu,
} from '../lib/sidebar-layout';
import {
  commitGroupRename,
  dropLineBefore,
  groupOrderIsCustomized,
  moveGroupAcrossVisible,
  moveGroupTo,
  resetGroupOrderAndNames,
} from '../lib/sidebar-groups';
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

  /* ─── 分组拖动排序 / 重命名 ─────────────────────────────────────
   * 拖动用指针事件而不是 HTML5 拖放：拖放会话绑在源节点上，一旦重绘（我们这个
   * 组头里还有计数、按钮、编辑框）就断；指针事件的状态只在 ref 里，节点换掉不影响。
   * 与设置页面板不同的是：这里**不**边拖边写盘（那样每跨过一行就是一次 localStorage
   * 写 + 一次 PUT /api/sidebar-layout），拖动期间只画插入线，松手才提交一次。
   * 收起态（44px 图标栏）不渲染把手与 ✎：那里没有落点，塞进去只会挤成一排点不动的方块。 */

  // 要渲染的组（本角色可见 + 未整组隐藏 + 组内有可见项）。
  // 拖动落点、插入线、键盘走一格全部按这份算 —— 布局里还躺着画面上看不见的组，
  // 按布局下标走格会「换了个看不见的顺序」，界面一动不动。
  //
  // 折叠（folded）不参与上面那份「可见组」判据：折叠只是把组内项临时收起来，
  // 组本身还在侧栏上占一行（点一下就能展开），把它算成「看不见的组」会让拖动落点
  // 与键盘走格跳过它 —— 用户拖过一行却落到更远的地方。
  const drawnGroups = layout.groups.flatMap((group) => {
    if (ownedMenusOf(group).length === 0) return [];   // 本角色看不到这一组
    if (group.hidden) return [];                        // 整组隐藏（恢复入口在 nav 底部）
    const visibleItems = visibleMenusOf(group, layout);
    if (visibleItems.length === 0) return [];           // 组内全被隐藏，画空组会像加载失败
    return [{
      group,
      visibleItems,
      label: groupTitle(group),
      // 44px 图标栏里没有「折叠」这回事：那一栏只画图标，折起来等于把整组菜单
      // 一起藏掉，而图标栏又没有任何展开入口。所以收起侧栏时一律展开。
      folded: !collapsed && groupIsCollapsed(group),
    }];
  });
  const drawnIds = drawnGroups.map(({ group }) => group.id);

  const navRef = React.useRef<HTMLElement | null>(null);
  const [draggingId, setDraggingId] = React.useState<string | null>(null);
  /** 拖动中的插入位（布局坐标，0..groups.length）；null = 没在拖 */
  const [dropIndex, setDropIndex] = React.useState<number | null>(null);
  /** 落点的 ref 镜像：松手那个事件读的是渲染时的闭包，state 在闭包里可能是旧的 */
  const dropIndexRef = React.useRef<number | null>(null);
  const [renamingId, setRenamingId] = React.useState<string | null>(null);
  const [renameDraft, setRenameDraft] = React.useState<string>('');
  /** Esc / Enter 之后紧跟着的那次 blur 不能再提交一遍（同一次编辑只落一次盘） */
  const renameClosedRef = React.useRef<boolean>(false);
  /** 拖动期间保持最新布局（父层随时可能因别处改动重画） */
  const layoutRef = React.useRef<SidebarLayout>(layout);
  layoutRef.current = layout;
  const teardownRef = React.useRef<(() => void) | null>(null);
  React.useEffect(() => () => { teardownRef.current?.(); }, []);

  const dragging = draggingId !== null;

  const startGroupDrag = (gid: string, event: React.PointerEvent<HTMLElement>): void => {
    if (event.button !== 0) return;
    event.preventDefault();   // 别让把手上的按下变成选中文字 / 拖出图片
    event.stopPropagation();
    teardownRef.current?.();

    // 画面上真正渲染出来的组（整组隐藏 / 本角色不可见 / 组内全隐藏的都不在 DOM 里）。
    // 落点只在它们之间算，再折算回布局下标，避免把指针位置映射到看不见的组上。
    const drawn = Array.from(
      navRef.current?.querySelectorAll<HTMLElement>('[data-sidebar-group]') ?? [],
    );
    const gids = drawn.map((el) => el.dataset.sidebarGroup || '').filter(Boolean);
    setDraggingId(gid);

    const onMove = (moveEvent: PointerEvent): void => {
      let insertAt = drawn.length;
      for (let i = 0; i < drawn.length; i += 1) {
        const rect = drawn[i].getBoundingClientRect();
        if (moveEvent.clientY < rect.top + rect.height / 2) { insertAt = i; break; }
      }
      const groups = layoutRef.current.groups;
      const anchor = insertAt < gids.length ? gids[insertAt] : '';
      const found = anchor ? groups.findIndex((g) => g.id === anchor) : -1;
      const next = found < 0 ? groups.length : found;
      // 同时写 state（画插入线）与 ref（松手时读）：pointerup 可能在这次重渲染之前到达，
      // 只靠 state 会拿到上一帧的落点。
      dropIndexRef.current = next;
      setDropIndex(next);
    };

    const teardown = (): void => {
      window.removeEventListener('pointermove', onMove);
      window.removeEventListener('pointerup', onUp);
      window.removeEventListener('pointercancel', onCancel);
      document.body.classList.remove('sidebar-group-dragging');
      teardownRef.current = null;
      dropIndexRef.current = null;
      setDraggingId(null);
      setDropIndex(null);
    };
    const onUp = (): void => {
      const current = dropIndexRef.current;
      teardown();
      if (current !== null) onApply(moveGroupTo(layoutRef.current, gid, current));
    };
    const onCancel = (): void => { teardown(); };

    document.body.classList.add('sidebar-group-dragging');
    teardownRef.current = teardown;
    window.addEventListener('pointermove', onMove);
    window.addEventListener('pointerup', onUp);
    window.addEventListener('pointercancel', onCancel);
  };

  /** 键盘排序：把手聚焦后 Alt+↑/↓ 走一格（走的是画面上可见的邻居，见 moveGroupAcrossVisible）。 */
  const onGripKeyDown = (gid: string) => (event: React.KeyboardEvent<HTMLElement>): void => {
    if (!event.altKey) return;
    if (event.key !== 'ArrowUp' && event.key !== 'ArrowDown') return;
    event.preventDefault();
    onApply(moveGroupAcrossVisible(layout, drawnIds, gid, event.key === 'ArrowUp' ? -1 : 1));
  };

  const beginRename = (group: LayoutGroup): void => {
    // 收起态不进入编辑：44px 里面只画得下分隔线，输入框会被 CSS 藏掉，
    // 结果就是「按了没反应、焦点还丢在看不见的元素上」。
    if (collapsed) return;
    renameClosedRef.current = false;
    setRenamingId(group.id);
    // 预填**存盘里那个名字**（内置组是中文原键），不是词典译名：
    // 拿译名当草稿，英文界面里按下 Enter 就会把 "General" 写进 label，把内置键覆盖掉。
    setRenameDraft(group.label);
  };

  const commitRename = (gid: string): void => {
    // 已经关过这一轮（Enter / Esc 之后紧跟着的 blur）：不重复提交
    if (renameClosedRef.current) return;
    renameClosedRef.current = true;
    const next = commitGroupRename(layout, gid, renameDraft);
    setRenamingId(null);
    onApply(next);   // null = 空名 / 没改，保留原显示名
  };

  const cancelRename = (): void => {
    renameClosedRef.current = true;
    setRenamingId(null);
  };

  const onRenameKeyDown = (gid: string) => (event: React.KeyboardEvent<HTMLInputElement>): void => {
    if (event.key === 'Enter') { event.preventDefault(); commitRename(gid); }
    if (event.key === 'Escape') { event.preventDefault(); event.stopPropagation(); cancelRename(); }
  };

  // 编辑到一半收起侧栏：输入框会被收起态的 CSS 藏掉，焦点留在看不见的元素上。
  // 直接当作取消（草稿不落盘；重新展开时用户看得见组名还是原来的，不会以为改丢了）。
  React.useEffect(() => {
    if (collapsed) {
      renameClosedRef.current = true;
      setRenamingId(null);
    }
  }, [collapsed]);

  // 插入线挂在哪一组之前（布局坐标 → 画面上真的渲染出来的那一组）；null = 列表末尾
  const dropBeforeId = dragging && dropIndex !== null
    ? dropLineBefore(layout, drawnIds, dropIndex)
    : null;
  const dropAtEnd = dragging && dropIndex !== null && dropBeforeId === null;
  const orderCustomized = groupOrderIsCustomized(layout);

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
      <nav className="sidebar-nav" ref={navRef}>
        {drawnGroups.map(({ group, visibleItems, label, folded }) => {
          const isRenaming = renamingId === group.id;
          return (
            <div
              key={group.id}
              data-sidebar-group={group.id}
              className={`sidebar-group${draggingId === group.id ? ' is-dragging' : ''}${folded ? ' is-folded' : ''}`}
            >
              {/* 插入线：拖到这一组之前。收起态不画（图标栏里横线会变成分隔线，看不出是落点） */}
              {dragging && !collapsed && dropBeforeId === group.id && (
                <div className="sidebar-drop-line" aria-hidden="true" />
              )}
              <div className="sidebar-group-head">
                {/* 拖动把手：只在展开态、且画面上真有第二个组可排时出现 —— 整行可拖会和点链接冲突；
                  只画出一个组时把手是纯噪音（拖它只会把这一组甩到看不见的组后面）。 */}
                {!collapsed && drawnIds.length > 1 && (
                  <span
                    className="sidebar-group-grip"
                    role="button"
                    tabIndex={0}
                    aria-label={t('拖动分组 {{name}}', { name: label })}
                    title={t('拖动调整分组顺序（Alt+↑ / ↓ 也可）')}
                    onPointerDown={(e) => startGroupDrag(group.id, e)}
                    onKeyDown={onGripKeyDown(group.id)}
                  >
                    <GripVertical size={12} strokeWidth={1.8} />
                  </span>
                )}
                {/* 折叠开关：长到一屏放不下的组（管理员那一组 14 项）默认收起。
                  做成一枚独立按钮而不是「点组名」：组名上已经有双击重命名，
                  再叠一个单击折叠，会出现「想改名却把组折了」的互相打架。 */}
                {!collapsed && (
                  <button
                    type="button"
                    className="sidebar-group-fold"
                    aria-expanded={!folded}
                    aria-label={folded
                      ? t('展开「{{name}}」分组', { name: label })
                      : t('折叠「{{name}}」分组', { name: label })}
                    title={folded
                      ? t('展开「{{name}}」分组', { name: label })
                      : t('折叠「{{name}}」分组', { name: label })}
                    onClick={(e) => {
                      e.stopPropagation();
                      onApply(setGroupCollapsed(layout, group.id, !folded));
                    }}
                  >
                    <ChevronDown size={11} strokeWidth={2} />
                  </button>
                )}
                {isRenaming ? (
                  <input
                    className="sidebar-group-rename"
                    value={renameDraft}
                    maxLength={12}
                    autoFocus
                    aria-label={t('分组名')}
                    onFocus={(e) => e.currentTarget.select()}
                    onChange={(e) => setRenameDraft(e.target.value)}
                    onBlur={() => commitRename(group.id)}
                    onKeyDown={onRenameKeyDown(group.id)}
                  />
                ) : (
                  <div
                    className="sidebar-group-label"
                    title={label}
                    onDoubleClick={() => beginRename(group)}
                  >
                    {label}
                  </div>
                )}
                {!isRenaming && (
                  <button
                    type="button"
                    className="sidebar-group-rename-btn"
                    title={t('重命名「{{name}}」分组', { name: label })}
                    aria-label={t('重命名「{{name}}」分组', { name: label })}
                    onClick={(e) => { e.stopPropagation(); beginRename(group); }}
                  >
                    <Pencil size={11} strokeWidth={1.8} />
                  </button>
                )}
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
              {/* 组内项：折叠时用 hidden 属性收起（元素仍在 DOM 里 —— 键盘 Tab 不会
                  跑到看不见的链接上，样式也只由这一个属性决定，不会两边各写一套显隐） */}
              <div className="sidebar-group-items" hidden={folded}>
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
            </div>
          );
        })}
        {/* 插入线落在末尾（拖到最后 / 拖到空白处） */}
        {dragging && !collapsed && dropAtEnd && (
          <div className="sidebar-drop-line" aria-hidden="true" />
        )}
        {/* 恢复入口：隐藏整组/单项后必须给一条回来的路 */}
        {hiddenCount > 0 && (
          <button type="button" className="sidebar-restore-groups" onClick={() => onApply(restoreHidden(layout))}>
            <RotateCcw size={12} strokeWidth={1.8} />
            <span>{t('已隐藏 {{n}} 项 · 恢复', { n: hiddenCount })}</span>
          </button>
        )}
        {/* 顺序 / 内置组名被改过才出现：不清隐藏状态（那是上面那条恢复条的事），
            自建分组也不删 —— 口径见 lib/sidebar-groups.ts 的 resetGroupOrderAndNames。 */}
        {!collapsed && orderCustomized && (
          <button
            type="button"
            className="sidebar-restore-groups sidebar-reset-order"
            title={t('把分组顺序与内置分组名恢复默认（隐藏状态与自建分组保留）')}
            onClick={() => onApply(resetGroupOrderAndNames(layout))}
          >
            <RotateCcw size={12} strokeWidth={1.8} />
            <span>{t('恢复默认分组')}</span>
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
