/**
 * 侧栏分组的「就地排序 / 就地重命名 / 恢复默认」纯逻辑（Sidebar.tsx 用，不碰 DOM、不碰 React）。
 *
 * ── 与 lib/sidebar-layout.ts 的分工 ────────────────────────────
 * 那边拥有布局模型与全部读写原语（moveGroup / renameGroup / 存盘 / 推库）；
 * 这里只放侧栏那套交互需要的派生与组合：拖动落点、键盘走一格、改名回退、恢复默认口径。
 * 全是纯函数，能直接单测（不用起 jsdom、不用戳 DOM 坐标）。
 *
 * ── 顺序与显示名存哪儿 ───────────────────────────────────────
 * 不另开一份 `{order:[id], names:{id:label}}`。布局对象的 `groups` 数组顺序就是显示顺序、
 * `group.label` 就是显示名，两者本来就随 `aigx-sidebar-layout-v1`（缓存）与服务端
 * `PUT /api/sidebar-layout`（权威）一起走。再存一份就是两份真相：面板改完侧栏不认、
 * 缓存与库对不上时还得人工判谁赢。
 *
 * 定位用 **组 id**（g-general / g-admin / g-personal / g-custom-*）而不是下标：
 * 下标会在增删组后整体错位，把「隐藏这一组」指到别的组上去；id 不会。
 *
 * ── 为什么不改 key ───────────────────────────────────────────
 * 组的 id 是稳定标识（顺序记录、整组隐藏、自建分组都靠它）。重命名只写 `label`
 * （显示名），id 一个字节都不动 —— 见 commitGroupRename。
 */

import {
  defaultLayout,
  locate,
  moveGroup,
  moveItem,
  renameGroup,
  type LayoutGroup,
  type SidebarLayout,
} from './sidebar-layout';

/** 重新拼一份布局（只换 groups 数组，hidden 表原样）。 */
function withGroups(layout: SidebarLayout, groups: LayoutGroup[]): SidebarLayout {
  return { groups, hidden: layout.hidden.slice() };
}

/**
 * 把分组拖到「第 toIndex 个位置之前」（toIndex 可以等于 groups.length = 拖到末尾）。
 *
 * 越界一律夹到 [0, len]：拖动时指针跑到列表外（或落在 44px 收起栏里）不该被
 * 当成「无操作」静默丢掉，用户看到插入线在末尾就该真的落到末尾。
 * 原位（自身下标 / 自身下标+1）返回 null —— 由 moveGroup 判，避免两处各写一套比较。
 */
export function moveGroupTo(
  layout: SidebarLayout,
  gid: string,
  toIndex: number,
): SidebarLayout | null {
  const from = layout.groups.findIndex((g) => g.id === gid);
  if (from < 0) return null;
  if (!Number.isFinite(toIndex)) return null;
  const clamped = Math.max(0, Math.min(Math.trunc(toIndex), layout.groups.length));
  return moveGroup(layout, gid, clamped);
}

/**
 * 键盘走一格（Alt+↑/↓），走的是**画面上真的渲染出来的**那一列邻居。
 *
 * 为什么不能按布局下标走：布局里还躺着看不见的组（本角色看不到的管理组、整组隐藏的、
 * 组内全隐藏的）。普通用户按一下 ↓ 时相邻那一格可能正是个看不见的管理组 ——
 * 布局确实换了，界面一动不动，用户只会认为快捷键坏了；再按一下才跨两步，长度也不对。
 *
 * 「插入位」与「下标」的区别也要在这里处理：上移要传前一组的下标（插在它之前），
 * 下移要传「下邻的下一个可见组」的下标（插在它之前 = 落在下邻之后）；没有就落到末尾。
 * 传自身下标在 `moveGroup` 里属原位、返回 null，也就是「按了没反应」。
 *
 * 可见序列的边界必须显式挡住。`-1` 之所以必须挡在门口：`moveGroup` 的越界处理并不是
 * 「夹到边界」，而是把负数直接交给 `splice(toIndex, 0, x)`，而负下标从**末尾**数——
 * 于是在长度 3 的布局里按一下 ↑ 得到 `b,a,c`（把自身挪到第二格，看起来像「下移」），
 * 长度 2 时又恰好不动，长度 4 时又是 `a,c,b,d`。同一组键的含义随布局长度漂移，
 * 这种「有时对、有时错」比恒错更难查，所以边界一律挡在调用 `moveGroup` 之前。
 */
export function moveGroupAcrossVisible(
  layout: SidebarLayout,
  visibleIds: readonly string[],
  gid: string,
  delta: number,
): SidebarLayout | null {
  if (delta === 0) return null;
  const from = visibleIds.indexOf(gid);
  if (from < 0) return null;
  const targetPos = from + (delta < 0 ? -1 : 1);
  if (targetPos < 0 || targetPos >= visibleIds.length) return null;
  const neighbor = visibleIds[targetPos];

  if (delta < 0) {
    return moveGroup(layout, gid, layout.groups.findIndex((g) => g.id === neighbor));
  }
  const after = visibleIds[targetPos + 1];
  const toIndex = after
    ? layout.groups.findIndex((g) => g.id === after)
    : layout.groups.length;
  return moveGroup(layout, gid, toIndex);
}

/**
 * 提交一次改名：Enter / 失焦走这里。
 *
 * 空名（或全空白）返回 null = 保留原显示名，绝不写空标题（空标题的组头和分隔线
 * 分不出来，收起态更是直接变一条看不见的线）。首尾空格裁掉 —— 手滑多打的空格
 * 会原样进 include 判断，`'通用 '` 与 `'通用'` 会被当成两个不同的名字。
 *
 * 只改 label，id / items / hidden / custom 全部原样：id 是顺序记录与整组隐藏的键，
 * 换掉 id 等于把这份布局上的所有指针作废（旧 id 的隐藏/顺序记录再也对不上）。
 */
export function commitGroupRename(
  layout: SidebarLayout,
  gid: string,
  draft: string,
): SidebarLayout | null {
  return renameGroup(layout, gid, String(draft ?? ''));
}

/** 内置默认分组的 id 集合（顺序即默认顺序）。 */
function defaultGroups(): LayoutGroup[] {
  return defaultLayout().groups;
}

/**
 * 分组顺序或内置组显示名是否已偏离默认（决定侧栏底部「恢复默认分组」出不出现）。
 *
 * 自建分组不算偏离：它默认就该在末尾，出现一个自建组不代表用户改了内置顺序。
 * 内置组被删掉算偏离 —— 此时按钮是唯一的找回路径（resetGroupOrderAndNames 会补回来）。
 */
export function groupOrderIsCustomized(layout: SidebarLayout): boolean {
  const defs = defaultGroups();
  const defIds = defs.map((g) => g.id);
  const present = layout.groups.filter((g) => defIds.includes(g.id)).map((g) => g.id);
  if (present.join('|') !== defIds.join('|')) return true;
  for (const group of layout.groups) {
    const def = defs.find((g) => g.id === group.id);
    if (def && group.label !== def.label) return true;
  }
  return false;
}

/**
 * 「恢复默认分组」的口径：**只清顺序与内置组显示名，不清隐藏状态，不删自建分组**。
 *
 * 为什么不动隐藏：
 *   · 隐藏已有自己的恢复入口（导航底部「已隐藏 N 项 · 恢复」，restoreHidden），
 *     两处按钮清同一件事会让「隐藏」这个动作随时被两次点击抹掉，可逆性被放大得很危险；
 *   · 顺序/名字是「排版」，隐藏是「这组我现在不想看」——后者带时间语义，一刀清掉
 *     等于把用户当下正在做的事也撤销了。要清隐藏的点那条恢复条。
 *
 * 自建分组保留（label 也不动，那是用户自己写的字），统一排在内置组之后：
 * 它们没有「默认位置」可言，只能给一个确定归宿；不保留就得顺手删用户的东西。
 *
 * 内置组被删过的，这里补回一个空组（默认名、未隐藏、items 为空）：否则「恢复默认分组」
 * 只能在缺组的状态下把剩下的排一排，名不副实，按钮也就永远消不掉。
 */
export function resetGroupOrderAndNames(layout: SidebarLayout): SidebarLayout | null {
  const defs = defaultGroups();
  const defIds = defs.map((g) => g.id);
  const byId = new Map(layout.groups.map((g) => [g.id, g]));
  const ordered: LayoutGroup[] = [];

  for (const def of defs) {
    const existing = byId.get(def.id);
    if (existing) {
      // 复用一个新对象：不改入参（调用方可能还握着旧引用画界面）
      ordered.push({ ...existing, label: def.label, custom: false, items: existing.items.slice() });
      continue;
    }
    ordered.push({ id: def.id, label: def.label, custom: false, hidden: false, items: [] });
  }
  for (const group of layout.groups) {
    if (defIds.includes(group.id)) continue;
    ordered.push({ ...group, items: group.items.slice() });
  }

  const before = JSON.stringify(layout.groups);
  const after = JSON.stringify(ordered);
  if (before === after) return null; // 已经就是默认：不白写一次缓存 + 一次 PUT
  return withGroups(layout, ordered);
}

/**
 * 原地走一格（设置页 SidebarLayoutPanel 的 Alt+↑/↓ 用，那页把所有组都画出来了，
 * 不存在「看不见的邻居」问题，所以按布局下标走）。
 *
 * 与 `moveGroupAcrossVisible` 的差别只在可见性口径；坑是同一个：
 *   · 传自身下标给 `moveGroup` 属原位、返回 null —— 上移传 `index` 就是死键；
 *   · 上移传 `index - 1` 还不够：首组会算出 -1，而 `moveGroup` **不夹取**边界，
 *     负下标直接进 `splice(-1, 0, x)`，从末尾数 —— 长度 3 的布局里按 ↑ 会得到
 *     `b,a,c`（像「下移」），长度 2 时又恰好不动。含义随长度漂移，所以首尾在这里挡。
 *   · 下移要传 `index + 2`（插到下邻之后），`index + 1` 同样属原位。
 */
export function moveGroupAdjacent(
  layout: SidebarLayout,
  gid: string,
  delta: number,
): SidebarLayout | null {
  const from = layout.groups.findIndex((g) => g.id === gid);
  if (from < 0) return null;
  if (delta < 0) {
    if (from === 0) return null;
    return moveGroup(layout, gid, from - 1);
  }
  if (delta > 0) {
    if (from === layout.groups.length - 1) return null;
    return moveGroup(layout, gid, from + 2);
  }
  return null;
}

/**
 * 组内菜单原地走一格（设置页 Alt+↑/↓ 的另一条腿）。
 *
 * 跨组边界已经由调用方先处理（组首上移 → 落到上一组末尾，组尾下移 → 落到下一组开头），
 * 进到这里就是同一组内的相邻交换。插入位同样不是自身下标：上移传 `index - 1`，
 * 下移传 `index + 2` —— 传 `index` / `index + 1` 在 `moveItem` 里都算原位返回 null
 * （设置页原先两条腿都踩了这个，组内上下键一起失灵）。
 */
export function moveItemAdjacent(
  layout: SidebarLayout,
  key: string,
  delta: number,
): SidebarLayout | null {
  const from = locate(layout, key);
  if (!from) return null;
  if (delta < 0) {
    if (from.index === 0) return null;
    return moveItem(layout, key, from.gi, from.index - 1);
  }
  if (delta > 0) {
    if (from.index === layout.groups[from.gi].items.length - 1) return null;
    return moveItem(layout, key, from.gi, from.index + 2);
  }
  return null;
}

/**
 * 拖动插入线该画在哪一组之前（null = 画在列表末尾）。
 *
 * 为什么不让调用方自己映射：落点算出来的是「布局里的第 N 位」，而画面上只渲染
 * 可见组（整组隐藏的、本角色看不到的、组内全隐藏的都不在 DOM 里）。夹在中间的那些
 * 组没有元素可挂线，只能顺延到下一个真的画出来的组前面 —— 否则线会凭空消失，
 * 用户以为拖不动。
 */
export function dropLineBefore(
  layout: SidebarLayout,
  renderedIds: readonly string[],
  toIndex: number,
): string | null {
  const indexOf = new Map(layout.groups.map((g, i) => [g.id, i]));
  for (const id of renderedIds) {
    const index = indexOf.get(id);
    if (index !== undefined && index >= toIndex) return id;
  }
  return null;
}
