/**
 * 侧边栏布局（分组 / 顺序 / 显隐 / 重命名）的唯一数据口径。
 *
 * ── 为什么单独成模块 ────────────────────────────────────────
 * 侧边栏本体（components/Sidebar.tsx）与设置页的编辑面板
 * （components/SidebarLayoutPanel.tsx）都要读同一份「默认分组 + 校验 + 补位」
 * 规则。模型放在任一侧，另一侧就得抄一份 → 两份必然漂。所以集中在这里，
 * 两侧都从这里取：侧边栏直接用，面板经同一套操作函数改。
 *
 * ── 存的是什么 ──────────────────────────────────────────────
 * `{ groups: [{ id, label, custom, hidden, items: [menuKey] }], hidden: [menuKey] }`
 *   · groups 的数组顺序 = 侧边栏上的分组顺序；
 *   · items 的数组顺序 = 组内菜单顺序（menuKey = 路由 path）；
 *   · hidden 是「单独隐藏的菜单」，与 group.hidden（整组隐藏）是两件事：
 *     整组隐藏时组内菜单的显隐状态原样保留，重新显示该组时不会丢。
 *
 * ── 存在哪里：库是权威，localStorage 是首帧缓存 ───────────────
 * 权威副本在服务端（`GET/PUT /api/sidebar-layout`）。桌面壳/网页端/多设备
 * origin 不同、localStorage 天然隔离，只存本机就会出现「在 A 处隐藏的菜单
 * 在 B 处照旧显示」。所以：同步读缓存画首帧，异步对库做同步（syncFromServer）。
 * 写：先缓存（立刻生效、刷新不丢），再 fire-and-forget 推库；推失败只 warn。
 *
 * ── 向前兼容 ────────────────────────────────────────────────
 * 读盘/读库都过 `normalizeLayout`：
 *   · 不认识的菜单键丢掉（降级回旧版本）；
 *   · 新版本新增、存盘里没有的菜单补进默认位置（否则升级后新功能凭空消失）；
 *   · 同一菜单出现在两组时只保留先出现的那份；
 *   · 分组显示名缺失时按 **id 语义**补回（内置组补「通用/管理/个人」），
 *     只有真正来历不明的组才落到 [`FALLBACK_GROUP_LABEL`]。
 *
 * ── 显示名为什么不能按序号兜底 ───────────────────────────────
 * 老实现是 `label || \`分组 ${index + 1}\``。两个后果：
 *   ① 用户看到的是「分组 1」这种**位置**残留，不是任何有意义的名字；
 *   ② `writeLayout` 会把这件兜底**落盘**（写缓存 + 推库），于是「数据缺字段」
 *      这个瞬时异常被固化成界面上的永久组名，之后再也看不出它原来是缺 label。
 * 所以兜底只保留「有意义的固定名」这一种形态，且缺失的**内置组按 id 认回本名**。
 */

import {
  LayoutDashboard, Satellite, KeyRound, Users, Wallet, ScrollText, Package,
  Settings, MessageSquare, Boxes, BookOpen, Bot, Headphones, Globe, UserRound,
  Puzzle, Route as RouteIcon, Flame, Wrench, CalendarClock, Bell, Gauge,
  type LucideIcon,
} from 'lucide-react';
import { endpoints as sidebarLayoutEndpoints } from '../api/endpoints/sidebarLayout';

/** 一枚侧栏菜单。key = 路由 path（唯一、稳定，增删菜单不影响已存布局的定位）。 */
export interface SidebarMenu {
  key: string;
  /** 文案 key（中文原文，i18n 约定） */
  labelKey: string;
  icon: LucideIcon;
  /** NavLink end（仅仪表盘 '/'） */
  end?: boolean;
  /** 仅管理员可见（展示层过滤，权限由后端强制） */
  adminOnly?: boolean;
  /** 固定项：不可单独隐藏，所在分组也不可整组隐藏 */
  fixed?: boolean;
}

/** 一个布局分组。 */
export interface LayoutGroup {
  id: string;
  label: string;
  custom: boolean;
  hidden: boolean;
  items: string[];
  /**
   * 折叠态：`true` 收起、`false` 展开、`undefined` = 未表态（按
   * [`AUTO_FOLD_MIN_ITEMS`] 自动判）。三态而不是布尔，才能让「老存盘（没有这个
   * 字段）」与「用户明确点开过」区分开 —— 老存盘要跟随以后的自动口径变化，
   * 用户点过的那一组不该被自动口径重新折起来。
   */
  collapsed?: boolean;
}

/** 整份布局。 */
export interface SidebarLayout {
  groups: LayoutGroup[];
  hidden: string[];
}

/** 渲染用分组（菜单已解析为 SidebarMenu）。 */
export interface RenderGroup {
  id: string;
  label: string;
  items: SidebarMenu[];
  hidden: boolean;
  /** 这一组在侧栏上是否收起（`items` 在 DOM 里、但不可见）。 */
  collapsed: boolean;
}

/* ─── 菜单表（唯一事实来源）─────────────────────────────── */

/** 全部菜单（默认分组、校验、固定项都按这张表算）。 */
export const SIDEBAR_MENUS: SidebarMenu[] = [
  { key: '/', labelKey: '仪表盘', icon: LayoutDashboard, end: true },
  { key: '/chat', labelKey: '聊天', icon: MessageSquare },
  { key: '/prompts', labelKey: '提示词库', icon: BookOpen },
  { key: '/keys', labelKey: 'API 密钥', icon: KeyRound },
  { key: '/logs', labelKey: '用量日志', icon: ScrollText },
  { key: '/model-manage', labelKey: '模型管理', icon: Boxes, adminOnly: true },
  { key: '/channels', labelKey: '供应商', icon: Satellite, adminOnly: true },
  { key: '/users', labelKey: '用户管理', icon: Users, adminOnly: true },
  { key: '/products', labelKey: '套餐与兑换码', icon: Package, adminOnly: true },
  { key: '/routing', labelKey: '路由', icon: RouteIcon, adminOnly: true },
  { key: '/purge', labelKey: '破甲控制台', icon: Flame, adminOnly: true },
  // 固定项：系统设置是全局入口，不允许被隐藏（隐藏了用户只能清缓存找回）
  { key: '/settings', labelKey: '系统设置', icon: Settings, adminOnly: true, fixed: true },
  { key: '/maintenance', labelKey: '自动维护', icon: CalendarClock, adminOnly: true },
  { key: '/limit', labelKey: '限流', icon: Gauge, adminOnly: true },
  { key: '/notify', labelKey: '通知', icon: Bell, adminOnly: true },
  { key: '/agent', labelKey: 'AI 运维', icon: Bot, adminOnly: true },
  { key: '/wallet', labelKey: '钱包', icon: Wallet },
  { key: '/tickets', labelKey: '工单', icon: Headphones },
  { key: '/profile', labelKey: '个人中心', icon: UserRound },
  { key: '/proxy-pool', labelKey: '网络代理', icon: Globe, adminOnly: true },
  { key: '/plugins', labelKey: '插件管理', icon: Puzzle, adminOnly: true },
  { key: '/tools', labelKey: 'MCP & Skills', icon: Wrench, adminOnly: true },
];

const MENU_BY_KEY = new Map(SIDEBAR_MENUS.map((m) => [m.key, m]));
export const MENU_KEYS = SIDEBAR_MENUS.map((m) => m.key);

/** 内置分组的展示名（存盘里保存的是这些中文原键，渲染时查词典）。 */
export const BUILTIN_GROUP_LABELS = ['通用', '管理', '个人'] as const;

/** 内置分组 id → 本名。缺 label 时按 id 认回它，而不是印一个序号。 */
const BUILTIN_LABEL_BY_ID: Readonly<Record<string, string>> = {
  'g-general': '通用',
  'g-admin': '管理',
  'g-personal': '个人',
};

/**
 * 真正缺名时的兜底显示名（数据异常时才走到这里）。
 *
 * 不含序号：序号会把「第几组」这件事当成名字写进用户视野，还会随分组增删漂移
 * （在同一份坏数据上重排一次，组名就变了）。固定名至少是诚实的 —— 它说明的正是
 * 「这个名字没存下来」，用户当场就知道该给它改个名。
 */
export const FALLBACK_GROUP_LABEL = '未命名分组';

/**
 * 老兜底（`分组 N`）留下的名字。
 *
 * 这条正则唯一的用途是**认出历史残留**：`分组 1` / `分组 2` 这种名字只可能由
 * 老版本的序号兜底产生，不可能是用户自己起的（真有人起这个名，见
 * [`normalizeLayout`] 里「只在本库自己造出来的 id 上做更正」那条判据）。
 */
const LEGACY_INDEX_LABEL_RE = /^分组\s*\d+$/;

/** 本库自动生成的组 id（`g-<index>`，只出现在外来数据的兜底路径上）。 */
const SYNTHETIC_GROUP_ID_RE = /^g-\d+$/;

/** 某个内置分组名是否可翻译（自建分组名原样显示）。 */
export function isBuiltinGroupLabel(label: string): boolean {
  return (BUILTIN_GROUP_LABELS as readonly string[]).includes(label);
}

/** 菜单项（内置；没有 = undefined）。 */
export function menuOf(key: string): SidebarMenu | undefined {
  return MENU_BY_KEY.get(key);
}

/** 是不是认得的菜单键。 */
export function isMenuKey(key: unknown): key is string {
  return typeof key === 'string' && MENU_BY_KEY.has(key);
}

/** 是不是固定项。 */
export function isFixedMenu(key: string): boolean {
  return MENU_BY_KEY.get(key)?.fixed === true;
}

/* ─── 默认布局 ──────────────────────────────────────────── */

export function defaultLayout(): SidebarLayout {
  return {
    groups: [
      {
        id: 'g-general',
        label: '通用',
        custom: false,
        hidden: false,
        items: ['/', '/chat', '/prompts', '/keys', '/logs'],
      },
      {
        id: 'g-admin',
        label: '管理',
        custom: false,
        hidden: false,
        items: [
          '/purge', '/model-manage', '/channels', '/proxy-pool', '/users', '/products',
          '/routing', '/plugins', '/tools', '/maintenance', '/limit', '/notify',
          '/settings', '/agent',
        ],
      },
      {
        id: 'g-personal',
        label: '个人',
        custom: false,
        hidden: false,
        items: ['/wallet', '/tickets', '/profile'],
      },
    ],
    hidden: [],
  };
}

/** 深拷贝。 */
export function cloneLayout(layout: SidebarLayout): SidebarLayout {
  return {
    groups: layout.groups.map((g) => {
      const copy: LayoutGroup = {
        id: g.id,
        label: g.label,
        custom: g.custom,
        hidden: g.hidden,
        items: g.items.slice(),
      };
      // 三态原样带过去：undefined 不许被写成 false（那会把「未表态」变成「用户展开过」）
      if (typeof g.collapsed === 'boolean') copy.collapsed = g.collapsed;
      return copy;
    }),
    hidden: layout.hidden.slice(),
  };
}

/**
 * 一个组的显示名：先认 id，再认用户自己写的字，最后才兜底。
 *
 * 判据顺序就是「这条数据是从哪来的」：
 *   · 内置 id（g-general / g-admin / g-personal）→ 本名。空名或老序号名都算缺失；
 *     用户把它改成「常用」这种自定义名字时**不动**（那是人写的字，不是缺失）。
 *   · 本库自己造的 id（`g-<n>`）→ 这组压根没经过 `addGroup`，名却又是空的或老序号
 *     形态 ⇒ 一定是兜底产物，给固定兜底名。
 *   · 自建 id（`g-custom-*`，`addGroup` 生成，创建时非空名是硬要求）→ 原样保留；
 *     用户真写了一模一样的「分组 1」也不会被我改掉。
 */
function resolveGroupLabel(id: string, rawLabel: unknown): string {
  const label = typeof rawLabel === 'string' ? rawLabel.trim() : '';
  const builtin = BUILTIN_LABEL_BY_ID[id];
  if (builtin) {
    if (!label || LEGACY_INDEX_LABEL_RE.test(label)) return builtin;
    return label;
  }
  if (!label) return FALLBACK_GROUP_LABEL;
  if (SYNTHETIC_GROUP_ID_RE.test(id) && LEGACY_INDEX_LABEL_RE.test(label)) {
    return FALLBACK_GROUP_LABEL;
  }
  return label;
}

/**
 * 把任意来源的数据规整成合法布局。只做减法与补位、不抛错 ——
 * 一份坏数据最多退回默认布局，不该让侧边栏整块消失。
 */
export function normalizeLayout(raw: unknown): SidebarLayout {
  const source = raw && typeof raw === 'object' ? (raw as Record<string, unknown>) : {};
  const rawGroups = Array.isArray(source.groups) ? source.groups : [];
  const seen = new Set<string>();
  const groups: LayoutGroup[] = [];

  rawGroups.forEach((g, index) => {
    if (!g || typeof g !== 'object') return;
    const group = g as Record<string, unknown>;
    const items: string[] = [];
    const rawItems = Array.isArray(group.items) ? group.items : [];
    for (const key of rawItems) {
      if (!isMenuKey(key) || seen.has(key)) continue;
      seen.add(key);
      items.push(key);
    }
    const id = typeof group.id === 'string' && group.id ? group.id : `g-${index}`;
    const normalized: LayoutGroup = {
      id,
      label: resolveGroupLabel(id, group.label),
      custom: group.custom === true,
      hidden: group.hidden === true,
      items,
    };
    // 折叠态只收真正的布尔值：老存盘没这个字段 ⇒ 保持 undefined（跟随自动口径）
    if (typeof group.collapsed === 'boolean') normalized.collapsed = group.collapsed;
    groups.push(normalized);
  });

  if (groups.length === 0) return defaultLayout();

  // 新版本新增、存盘里没有的菜单补进默认位置（保持相对次序）。
  const missing = MENU_KEYS.filter((key) => !seen.has(key));
  if (missing.length) {
    const defaults = defaultLayout().groups;
    for (const key of missing) {
      const home = defaults.find((g) => g.items.includes(key));
      if (!home) {
        groups[0].items.push(key);
        continue;
      }
      const target = groups.find((g) => g.id === home.id) ?? groups.find((g) => g.label === home.label);
      if (!target) {
        groups[0].items.push(key);
        continue;
      }
      const order = home.items;
      const next = order
        .slice(order.indexOf(key) + 1)
        .find((sibling) => target.items.includes(sibling));
      if (next) target.items.splice(target.items.indexOf(next), 0, key);
      else target.items.push(key);
    }
  }

  // 含固定项的分组不可整组隐藏。
  for (const group of groups) {
    if (group.hidden && group.items.some(isFixedMenu)) group.hidden = false;
  }

  const rawHidden = Array.isArray(source.hidden) ? source.hidden : [];
  const hidden: string[] = [];
  for (const key of rawHidden) {
    if (isMenuKey(key) && !isFixedMenu(key) && !hidden.includes(key)) hidden.push(key);
  }

  return { groups, hidden };
}

/* ─── 持久化：本地缓存 + 服务端权威 ──────────────────────── */

/** 首帧缓存的键（权威在服务端 `sidebarLayout`）。 */
export const STORAGE_KEY = 'aigx-sidebar-layout-v1';

/** 布局变更后广播的事件名（同源多实例 + 面板 ↔ 侧栏同步用）。 */
export const LAYOUT_EVENT = 'aigx:sidebar-layout';

/** 最近一次本地写入时刻，用于躲开「自己刚写的还没到库」的竞态窗口。 */
let lastLocalWriteAt = 0;
const LOCAL_WRITE_GRACE_MS = 3000;

/** 读首帧缓存；无缓存 / 坏 JSON 一律退回默认布局。 */
export function readLayout(): SidebarLayout {
  try {
    const text = localStorage.getItem(STORAGE_KEY);
    if (!text) return defaultLayout();
    return normalizeLayout(JSON.parse(text));
  } catch {
    return defaultLayout();
  }
}

/** 写布局：先写缓存（同步、立刻生效），再 fire-and-forget 推库。 */
export function writeLayout(layout: SidebarLayout): SidebarLayout {
  const normalized = normalizeLayout(layout);
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(normalized));
  } catch {
    /* 存不下只影响下次启动，不影响本次会话 */
  }
  pushToServer(normalized);
  return normalized;
}

/** 推给后端。失败只 warn（界面本地已生效）。 */
function pushToServer(layout: SidebarLayout): void {
  lastLocalWriteAt = Date.now();
  void sidebarLayoutEndpoints
    .saveSidebarLayout(layout)
    .catch((err: unknown) => {
      console.warn('侧边栏布局保存到服务端失败（本机已生效，其它设备看不到这次改动）:', err);
    });
}

/**
 * 从服务端拉一次布局并与缓存对齐。启动、窗口聚焦时调用。
 * 返回 true 表示缓存被更新过。
 */
export async function syncFromServer(): Promise<boolean> {
  if (Date.now() - lastLocalWriteAt < LOCAL_WRITE_GRACE_MS) return false;
  let remote: unknown;
  try {
    remote = await sidebarLayoutEndpoints.getSidebarLayout();
  } catch {
    return false; // 端点未就绪/离线：继续用缓存
  }
  if (Date.now() - lastLocalWriteAt < LOCAL_WRITE_GRACE_MS) return false;

  const layoutValue = extractLayout(remote);

  // 库里有布局：规整后覆盖缓存；与缓存不同才广播重画。
  if (layoutValue && typeof layoutValue === 'object') {
    const incoming = normalizeLayout(layoutValue);
    const incomingText = JSON.stringify(incoming);
    let cachedText: string | null = null;
    try {
      cachedText = localStorage.getItem(STORAGE_KEY);
    } catch {
      cachedText = null;
    }
    if (cachedText === incomingText) return false;
    try {
      localStorage.setItem(STORAGE_KEY, incomingText);
    } catch {
      /* 写不进缓存也仍然广播：本次会话按服务端那份渲染 */
    }
    if (typeof window !== 'undefined') {
      window.dispatchEvent(new CustomEvent(LAYOUT_EVENT));
    }
    return true;
  }

  // 库里没存过（首次落库）：把本机缓存上行一次（老用户无感迁移）。
  pushToServer(readLayout());
  return false;
}

/** 兼容 `{layout}` 与 `{data:{layout}}` 两种外层形态。 */
function extractLayout(remote: unknown): unknown {
  if (!remote || typeof remote !== 'object') return null;
  const obj = remote as Record<string, unknown>;
  if ('layout' in obj) return obj.layout;
  const data = obj.data;
  if (data && typeof data === 'object' && 'layout' in (data as Record<string, unknown>)) {
    return (data as Record<string, unknown>).layout;
  }
  return null;
}

/* ─── 查询 ─────────────────────────────────────────────── */

export function locate(layout: SidebarLayout, key: string): { gi: number; index: number } | null {
  for (let gi = 0; gi < layout.groups.length; gi += 1) {
    const index = layout.groups[gi].items.indexOf(key);
    if (index >= 0) return { gi, index };
  }
  return null;
}

/** 这一组是否含固定项（含则不可整组隐藏）。 */
export function groupHasFixed(group: LayoutGroup): boolean {
  return group.items.some(isFixedMenu);
}

/** 组内可见（未被单独隐藏）的菜单数。 */
export function visibleCount(group: LayoutGroup, hidden: string[]): number {
  return group.items.filter((key) => !hidden.includes(key)).length;
}

/* ─── 变更（全部返回新对象，不改入参；无变化返回 null）── */

/** 把菜单 key 移到第 toGi 组的 toIndex 位置。 */
export function moveItem(
  layout: SidebarLayout,
  key: string,
  toGi: number,
  toIndex: number,
): SidebarLayout | null {
  const from = locate(layout, key);
  if (!from || !layout.groups[toGi]) return null;
  const next = cloneLayout(layout);

  if (from.gi === toGi) {
    const list = next.groups[toGi].items;
    if (toIndex === from.index || toIndex === from.index + 1) return null;
    list.splice(from.index, 1);
    list.splice(toIndex > from.index ? toIndex - 1 : toIndex, 0, key);
    return next;
  }

  next.groups[from.gi].items.splice(from.index, 1);
  const target = next.groups[toGi].items;
  target.splice(Math.max(0, Math.min(toIndex, target.length)), 0, key);
  return next;
}

/** 把分组 gid 移到 toIndex 位置。 */
export function moveGroup(layout: SidebarLayout, gid: string, toIndex: number): SidebarLayout | null {
  const from = layout.groups.findIndex((g) => g.id === gid);
  if (from < 0) return null;
  if (toIndex === from || toIndex === from + 1) return null;
  const next = cloneLayout(layout);
  const moved = next.groups.splice(from, 1)[0];
  next.groups.splice(toIndex > from ? toIndex - 1 : toIndex, 0, moved);
  return next;
}

/** 设置某个菜单的显隐；固定项永远可见（返回 null）。 */
export function setItemHidden(
  layout: SidebarLayout,
  key: string,
  hidden: boolean,
): SidebarLayout | null {
  if (isFixedMenu(key) && hidden) return null;
  const next = cloneLayout(layout);
  const index = next.hidden.indexOf(key);
  if (hidden && index < 0) next.hidden.push(key);
  else if (!hidden && index >= 0) next.hidden.splice(index, 1);
  else return null;
  return next;
}

/** 整组显隐；含固定项的组不可隐藏（返回 null）。 */
export function setGroupHidden(
  layout: SidebarLayout,
  gid: string,
  hidden: boolean,
): SidebarLayout | null {
  const index = layout.groups.findIndex((g) => g.id === gid);
  if (index < 0) return null;
  if (hidden && groupHasFixed(layout.groups[index])) return null;
  if (layout.groups[index].hidden === hidden) return null;
  const next = cloneLayout(layout);
  next.groups[index].hidden = hidden;
  return next;
}

/** 分组改名；空名或没变化时返回 null。 */
export function renameGroup(layout: SidebarLayout, gid: string, label: string): SidebarLayout | null {
  const index = layout.groups.findIndex((g) => g.id === gid);
  if (index < 0) return null;
  const trimmed = String(label || '').trim();
  if (!trimmed || trimmed === layout.groups[index].label) return null;
  const next = cloneLayout(layout);
  next.groups[index].label = trimmed;
  return next;
}

/** 新建一个空分组（排在最后）。 */
export function addGroup(layout: SidebarLayout, label: string): SidebarLayout | null {
  const trimmed = String(label || '').trim();
  if (!trimmed) return null;
  const next = cloneLayout(layout);
  next.groups.push({
    id: `g-custom-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 6)}`,
    label: trimmed,
    custom: true,
    hidden: false,
    items: [],
  });
  return next;
}

/** 删除一个分组，组内菜单并入第一组（菜单代表真实页面，不该随分组一起丢）。 */
export function removeGroup(layout: SidebarLayout, gid: string): SidebarLayout | null {
  if (layout.groups.length <= 1) return null;
  const index = layout.groups.findIndex((g) => g.id === gid);
  if (index < 0) return null;
  const next = cloneLayout(layout);
  const removed = next.groups.splice(index, 1)[0];
  next.groups[0].items.push(...removed.items);
  return next;
}

/* ─── 折叠（管理员那一组 14 项一屏放不下）────────────────── */

/**
 * 默认折叠门槛：组内菜单数达到它就先收起（用户点一次即可展开，状态落盘）。
 *
 * 为什么是「按菜单数」而不是「按组名/角色」：门槛要能被一条判据说清、
 * 且不随角色漂 —— 同一个账号在管理员与普通用户视角下看到的折叠态必须一致，
 * 否则换角色登录会像「布局自己变了」。普通用户的组都远不到 10 项，不受影响。
 *
 * 为什么不是「全部收起」：把 5 项的「通用」也折起来，等于给最常见的入口多加一次
 * 点击。只折真正长到放不下的那一组，才配得上「省一屏」这件事。
 */
export const AUTO_FOLD_MIN_ITEMS = 10;

/** 这一组实际是否收起：用户表态优先，没表态按菜单数自动判。 */
export function groupIsCollapsed(group: LayoutGroup): boolean {
  if (typeof group.collapsed === 'boolean') return group.collapsed;
  return group.items.length >= AUTO_FOLD_MIN_ITEMS;
}

/**
 * 设置折叠态。`null` = 交回自动口径（清掉表态，而不是写一个恰好相等的布尔）。
 *
 * 传 `null` 而不是「删掉这一项」的唯一入口，是为了让「恢复默认」这类操作
 * 能把自动口径重新拿回来：老用户升级后不该被一个他当年没做过的决定钉住。
 *
 * 判「没变化」用的是**实际显示出来的折叠态**（含自动口径），不是字段本身：
 * 一组已经因为「太长」自动收起了，再点一次收起不该白写一次缓存 + 一次 PUT。
 */
export function setGroupCollapsed(
  layout: SidebarLayout,
  gid: string,
  collapsed: boolean | null,
): SidebarLayout | null {
  const index = layout.groups.findIndex((g) => g.id === gid);
  if (index < 0) return null;
  const current = layout.groups[index];
  if (collapsed === null) {
    if (typeof current.collapsed !== 'boolean') return null;
  } else if (groupIsCollapsed(current) === collapsed) {
    return null;
  }
  const next = cloneLayout(layout);
  if (collapsed === null) delete next.groups[index].collapsed;
  else next.groups[index].collapsed = collapsed;
  return next;
}

/* ─── 派生：侧边栏要画什么 ─────────────────────────────── */

/** 把布局压成「侧边栏实际要画的分组」。空组 / 整组隐藏 / 全隐藏的组跳过。 */
export function renderGroups(layout: SidebarLayout): RenderGroup[] {
  const out: RenderGroup[] = [];
  for (const group of layout.groups) {
    if (group.hidden) continue;
    const items: SidebarMenu[] = [];
    for (const key of group.items) {
      if (layout.hidden.includes(key)) continue;
      const menu = menuOf(key);
      if (menu) items.push(menu);
    }
    if (!items.length) continue;
    out.push({
      id: group.id,
      label: group.label,
      items,
      hidden: group.hidden,
      collapsed: groupIsCollapsed(group),
    });
  }
  return out;
}

/** 落地页：侧边栏上第一个可见菜单 key（一个都不剩时返回 null）。 */
export function firstVisibleKey(layout: SidebarLayout): string | null {
  const groups = renderGroups(layout);
  return groups.length ? groups[0].items[0].key : null;
}
