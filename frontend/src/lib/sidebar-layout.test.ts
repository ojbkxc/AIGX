// lib/sidebar-layout.ts 的判据：分组显示名从哪来、折叠态怎么算。
//
// 这两块都是「界面上看不见的判据」——组名与折叠态都直接印在侧栏上，错了没有
// 报错、只有一个说不通的显示。所以按「能区分新旧」的要求写：
//
//   1. 缺 label 的内置组必须认回本名（旧实现印「分组 1」）；
//   2. 老存盘里已经固化的「分组 1」必须被更正（旧实现会把它当用户起的名留下）；
//   3. 用户自己起的名（哪怕长得像序号）必须原样保留 —— 反向对照，
//      防「修 1、2 的时候顺手把用户数据也改了」；
//   4. 兜底名不得含序号（序号随分组增删漂移，同一份坏数据重排就会换名）；
//   5. 折叠三态：未表态按菜单数自动判、表态后不被自动口径推翻、可交回自动。
import { describe, expect, it } from 'vitest';
import {
  AUTO_FOLD_MIN_ITEMS,
  BUILTIN_GROUP_LABELS,
  FALLBACK_GROUP_LABEL,
  SIDEBAR_MENUS,
  addGroup,
  cloneLayout,
  defaultLayout,
  groupIsCollapsed,
  isBuiltinGroupLabel,
  normalizeLayout,
  renderGroups,
  setGroupCollapsed,
  type LayoutGroup,
  type SidebarLayout,
} from './sidebar-layout';

function group(layout: SidebarLayout, gid: string): LayoutGroup {
  const found = layout.groups.find((g) => g.id === gid);
  if (!found) throw new Error(`布局里没有分组 ${gid}`);
  return found;
}

describe('分组显示名：缺失时认回本名，而不是印序号', () => {
  it('内置组 label 为空 → 按 id 认回「通用 / 管理 / 个人」', () => {
    const layout = normalizeLayout({
      groups: [
        { id: 'g-general', label: '', items: ['/'] },
        { id: 'g-admin', items: ['/users'] }, // label 整个字段缺失
        { id: 'g-personal', label: '   ', items: ['/wallet'] },
      ],
      hidden: [],
    });
    expect(layout.groups.map((g) => g.label)).toEqual([...BUILTIN_GROUP_LABELS]);
  });

  it('内置组的名字里不带任何序号（旧实现是「分组 1 / 分组 2」）', () => {
    const layout = normalizeLayout({
      groups: [
        { id: 'g-general', label: '', items: ['/'] },
        { id: 'g-admin', label: '  ', items: ['/users'] },
        { id: 'g-personal', items: ['/wallet'] },
      ],
      hidden: [],
    });
    for (const g of layout.groups) {
      expect(g.label).not.toMatch(/^分组\s*\d+$/);
      expect(g.label).not.toMatch(/\d/);
    }
  });

  it('defaultLayout 本身就自带语义名（正常路径不该走到兜底）', () => {
    for (const g of defaultLayout().groups) {
      expect(g.label.trim()).not.toBe('');
      expect(isBuiltinGroupLabel(g.label)).toBe(true);
    }
  });

  it('老存盘里固化的「分组 1」在内置 id 上被更正回本名', () => {
    const layout = normalizeLayout({
      groups: [
        { id: 'g-general', label: '分组 1', custom: false, hidden: false, items: ['/'] },
        { id: 'g-admin', label: '分组 2', custom: false, hidden: false, items: ['/users'] },
        { id: 'g-personal', label: '分组 3', custom: false, hidden: false, items: ['/wallet'] },
      ],
      hidden: [],
    });
    expect(layout.groups.map((g) => g.label)).toEqual([...BUILTIN_GROUP_LABELS]);
  });

  it('用户自己写的名字原样保留（反向对照：改名不是「纠正」）', () => {
    const layout = normalizeLayout({
      groups: [
        { id: 'g-general', label: '常用', custom: false, hidden: false, items: ['/'] },
        // 自建组：用户真起了一个「分组 1」这种名字也不动他 —— 自建组的名字是
        // addGroup 硬要求非空的，能出现这个形态只可能是人写的
        { id: 'g-custom-mine', label: '分组 1', custom: true, hidden: false, items: ['/chat'] },
        { id: 'g-admin', label: '运维台', custom: false, hidden: false, items: ['/users'] },
      ],
      hidden: [],
    });
    expect(group(layout, 'g-general').label).toBe('常用');
    expect(group(layout, 'g-custom-mine').label).toBe('分组 1');
    expect(group(layout, 'g-admin').label).toBe('运维台');
  });

  it('来历不明的组（无 id、老库里造的 g-<n>）给固定兜底名，不是序号', () => {
    const layout = normalizeLayout({
      groups: [
        { label: '', items: ['/'] },                    // 无 id → 兜 will生成 g-0
        { id: 'g-7', label: '', items: ['/chat'] },
        { id: 'g-8', label: '分组 9', items: ['/logs'] }, // 老兜底产物
        { id: 'g-other', label: '', items: ['/wallet'] }, // 非本库形态、空名
        { id: 'g-general', label: '通用', items: ['/keys'] },
      ],
      hidden: [],
    });
    expect(group(layout, 'g-0').label).toBe(FALLBACK_GROUP_LABEL);
    expect(group(layout, 'g-7').label).toBe(FALLBACK_GROUP_LABEL);
    expect(group(layout, 'g-8').label).toBe(FALLBACK_GROUP_LABEL);
    expect(group(layout, 'g-other').label).toBe(FALLBACK_GROUP_LABEL);
    expect(FALLBACK_GROUP_LABEL).not.toMatch(/\d/);
  });

  it('兜底名与内置名都不含序号：重排同一份数据组名不漂移', () => {
    // 旧实现在这里会给出 分组 1 / 分组 2（下标一换名字就换）
    const raw = { groups: [{ id: 'g-7', label: '', items: ['/'] }, { id: 'g-8', label: '', items: ['/chat'] }] };
    const a = normalizeLayout(raw);
    const b = normalizeLayout({ groups: [raw.groups[1], raw.groups[0]] });
    expect(a.groups.map((g) => g.label)).toEqual(b.groups.map((g) => g.label));
    expect(a.groups.every((g) => g.label === FALLBACK_GROUP_LABEL)).toBe(true);
  });

  it('label 首尾空格被裁掉（手滑多打一个空格不该变成另一个组名）', () => {
    const layout = normalizeLayout({
      groups: [{ id: 'g-custom-x', label: '  我的组  ', custom: true, items: ['/'] }],
      hidden: [],
    });
    expect(group(layout, 'g-custom-x').label).toBe('我的组');
  });

  it('addGroup 的空名依然被拒（正常创建路径不该产生空名）', () => {
    expect(addGroup(defaultLayout(), '')).toBeNull();
    expect(addGroup(defaultLayout(), '   ')).toBeNull();
    const next = addGroup(defaultLayout(), ' 常用 ');
    const created = next?.groups[next.groups.length - 1];
    expect(created?.label).toBe('常用');
    expect(created?.custom).toBe(true);
  });
});

describe('分组折叠：长组收起、短组不动、表态优先', () => {
  /**
   * 造一个装了 n 项的内置组。
   *
   * 这里**故意不走 `normalizeLayout`**：它会把「存盘里没有的菜单」补回默认位置，
   * 于是 n=9 的组会被补成 14 项 —— 用那种布局测门槛，测的是补位逻辑而不是门槛。
   */
  function withItems(n: number): SidebarLayout {
    const keys = SIDEBAR_MENUS.slice(0, n).map((m) => m.key);
    return {
      groups: [{ id: 'g-admin', label: '管理', custom: false, hidden: false, items: keys }],
      hidden: [],
    };
  }

  it('默认门槛：菜单数 ≥ 门槛先收起，否则展开', () => {
    const long = withItems(AUTO_FOLD_MIN_ITEMS);
    const short = withItems(AUTO_FOLD_MIN_ITEMS - 1);
    expect(groupIsCollapsed(group(long, 'g-admin'))).toBe(true);
    expect(groupIsCollapsed(group(short, 'g-admin'))).toBe(false);
  });

  it('内置默认布局：管理员那一组自动收起，通用与个人不动', () => {
    const layout = defaultLayout();
    // 前置：这一组确实是「长组」（否则这条用例在测一个不存在的问题）
    expect(group(layout, 'g-admin').items.length).toBeGreaterThanOrEqual(AUTO_FOLD_MIN_ITEMS);
    expect(groupIsCollapsed(group(layout, 'g-admin'))).toBe(true);
    expect(groupIsCollapsed(group(layout, 'g-general'))).toBe(false);
    expect(groupIsCollapsed(group(layout, 'g-personal'))).toBe(false);
  });

  it('用户表态优先于自动口径（展开过就不许被自动口径重新折起来）', () => {
    const layout = withItems(AUTO_FOLD_MIN_ITEMS);
    const opened = setGroupCollapsed(layout, 'g-admin', false) as SidebarLayout;
    expect(group(opened, 'g-admin').collapsed).toBe(false);
    expect(groupIsCollapsed(group(opened, 'g-admin'))).toBe(false);

    // 再把菜单砍到门槛以下：明确收起过就保持收起，不被「变短了」推翻
    const short = withItems(1);
    const closed = setGroupCollapsed(short, 'g-admin', true) as SidebarLayout;
    expect(groupIsCollapsed(group(closed, 'g-admin'))).toBe(true);
  });

  it('交回自动口径（null）后按菜单数重新算', () => {
    const layout = withItems(AUTO_FOLD_MIN_ITEMS);
    const opened = setGroupCollapsed(layout, 'g-admin', false) as SidebarLayout;
    const back = setGroupCollapsed(opened, 'g-admin', null) as SidebarLayout;
    expect('collapsed' in group(back, 'g-admin')).toBe(false);
    expect(groupIsCollapsed(group(back, 'g-admin'))).toBe(true); // 又长又没表态 ⇒ 自动收起
  });

  it('无变化返回 null（不白写一次缓存 + 一次 PUT）', () => {
    const layout = withItems(AUTO_FOLD_MIN_ITEMS);
    expect(setGroupCollapsed(layout, 'g-admin', true)).toBeNull();   // 没表态但自动判=收起
    expect(setGroupCollapsed(layout, 'g-nope', true)).toBeNull();
    const opened = setGroupCollapsed(layout, 'g-admin', false) as SidebarLayout;
    expect(setGroupCollapsed(opened, 'g-admin', false)).toBeNull();
    const back = setGroupCollapsed(opened, 'g-admin', null) as SidebarLayout;
    expect(setGroupCollapsed(back, 'g-admin', null)).toBeNull();      // 本来就没表态
  });

  it('折叠态过 cloneLayout 不被写成 false（三态必须原样带过去）', () => {
    const layout = withItems(AUTO_FOLD_MIN_ITEMS);
    const copy = cloneLayout(layout);
    expect('collapsed' in group(copy, 'g-admin')).toBe(false);
    const opened = setGroupCollapsed(layout, 'g-admin', false) as SidebarLayout;
    expect(group(cloneLayout(opened), 'g-admin').collapsed).toBe(false);
  });

  it('normalizeLayout 只收真正的布尔值（脏值当没表态，不猜）', () => {
    const layout = normalizeLayout({
      groups: [{ id: 'g-admin', label: '管理', items: SIDEBAR_MENUS.slice(0, 12).map((m) => m.key), collapsed: 'yes' }],
      hidden: [],
    });
    expect('collapsed' in group(layout, 'g-admin')).toBe(false);
    expect(groupIsCollapsed(group(layout, 'g-admin'))).toBe(true); // 按菜单数自动判
  });

  it('renderGroups 把折叠态一并交给渲染层（且隐藏组不出现）', () => {
    const layout = normalizeLayout({
      groups: [
        { id: 'g-general', label: '通用', items: ['/', '/chat'] },
        { id: 'g-admin', label: '管理', items: SIDEBAR_MENUS.slice(5, 20).map((m) => m.key) },
        { id: 'g-personal', label: '个人', hidden: true, items: ['/wallet'] },
      ],
      hidden: [],
    });
    const rendered = renderGroups(layout);
    expect(rendered.map((g) => g.id)).toEqual(['g-general', 'g-admin']);
    expect(rendered.find((g) => g.id === 'g-general')?.collapsed).toBe(false);
    expect(rendered.find((g) => g.id === 'g-admin')?.collapsed).toBe(true);
  });
});

describe('存盘迁移：老布局（无 collapsed / 无 label）无损升上来', () => {
  it('老存盘三项结构不动：组顺序、组内顺序、隐藏位逐项一致', () => {
    // 这是 1.0.123 之前真实落盘过的形状：没有 collapsed 字段
    const old = {
      groups: [
        { id: 'g-general', label: '通用', custom: false, hidden: false, items: ['/', '/chat', '/prompts', '/keys', '/logs'] },
        { id: 'g-admin', label: '管理', custom: false, hidden: false, items: ['/purge', '/model-manage', '/channels', '/proxy-pool', '/users', '/products', '/routing', '/plugins', '/tools', '/maintenance', '/limit', '/notify', '/settings', '/agent'] },
        { id: 'g-personal', label: '个人', custom: false, hidden: false, items: ['/wallet', '/tickets', '/profile'] },
      ],
      hidden: ['/logs'],
    };
    const migrated = normalizeLayout(old);
    expect(migrated.groups.map((g) => g.id)).toEqual(['g-general', 'g-admin', 'g-personal']);
    expect(migrated.groups.map((g) => g.label)).toEqual(['通用', '管理', '个人']);
    // 老存盘没有的新菜单（/oauth、/epay）会被 normalize 按默认位置补进 g-admin，
    // 老菜单的相对顺序保持不变 —— 这正是「新增功能不凭空消失」的迁移契约。
    const oldAdmin = old.groups[1].items;
    const newAdmin = migrated.groups[1].items;
    // 老 14 项一个不少、相对顺序不变（filter 后逐项相等）
    expect(newAdmin.filter((k) => oldAdmin.includes(k))).toEqual(oldAdmin);
    // 新增的两项已补进来
    expect(newAdmin).toContain('/oauth');
    expect(newAdmin).toContain('/epay');
    // 其余两组无新增，逐项一致
    expect(migrated.groups[0].items).toEqual(old.groups[0].items);
    expect(migrated.groups[2].items).toEqual(old.groups[2].items);
    expect(migrated.hidden).toEqual(['/logs']);
    // 迁移只「加」三态里的未表态，不替用户做折叠决定
    expect(migrated.groups.every((g) => !('collapsed' in g))).toBe(true);
  });

  it('22 项一个不丢：老存盘 + 默认布局的菜单集合完全相等', () => {
    const old = defaultLayout();
    const oldKeys = old.groups.flatMap((g) => g.items).sort();
    const migrated = normalizeLayout(JSON.parse(JSON.stringify(old)));
    expect(migrated.groups.flatMap((g) => g.items).sort()).toEqual(oldKeys);
    expect(oldKeys).toHaveLength(SIDEBAR_MENUS.length);
    expect(migrated.groups.flatMap((g) => g.items)).toHaveLength(SIDEBAR_MENUS.length);
  });

  it('整组隐藏 + 含固定项的组：迁移后隐藏位仍被挡回 false（固定项不可藏）', () => {
    const migrated = normalizeLayout({
      groups: [
        { id: 'g-admin', label: '管理', hidden: true, items: ['/users', '/settings'] },
      ],
      hidden: ['/settings', '/users'],
    });
    expect(group(migrated, 'g-admin').hidden).toBe(false);
    expect(migrated.hidden).toEqual(['/users']); // 固定项被剔出隐藏表
  });
});