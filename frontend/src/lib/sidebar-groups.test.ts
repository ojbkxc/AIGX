import { describe, expect, it } from 'vitest';
import {
  commitGroupRename,
  dropLineBefore,
  groupOrderIsCustomized,
  moveGroupAcrossVisible,
  moveGroupStep,
  moveGroupTo,
  resetGroupOrderAndNames,
} from './sidebar-groups';
import { cloneLayout, defaultLayout, type SidebarLayout } from './sidebar-layout';

/** 默认布局 + 一个自建分组（放在末尾，模拟用户在设置页新建过一组）。 */
function layoutWithCustom(): SidebarLayout {
  const layout = defaultLayout();
  layout.groups.push({ id: 'g-custom-1', label: '常用', custom: true, hidden: false, items: ['/chat'] });
  return layout;
}

/** 组 id 顺序，断言只关心顺序时用它（比整份 JSON 好读）。 */
function order(layout: SidebarLayout): string {
  return layout.groups.map((g) => g.id).join(',');
}

/** 取样一个分组（不存在就炸，避免断言在 undefined 上假绿）。 */
function group(layout: SidebarLayout, gid: string) {
  const found = layout.groups.find((g) => g.id === gid);
  if (!found) throw new Error(`布局里没有分组 ${gid}`);
  return found;
}

describe('moveGroupTo（拖动落点提交）', () => {
  it('拖到第 0 位之前：个人组排到最前，其余相对次序不变', () => {
    const layout = defaultLayout();
    const next = moveGroupTo(layout, 'g-personal', 0);
    expect(next).not.toBeNull();
    expect(order(next as SidebarLayout)).toBe('g-personal,g-general,g-admin');
  });

  it('toIndex = groups.length 表示拖到末尾（不是越界丢弃）', () => {
    const layout = defaultLayout();
    const next = moveGroupTo(layout, 'g-general', layout.groups.length);
    expect(next).not.toBeNull();
    expect(order(next as SidebarLayout)).toBe('g-admin,g-personal,g-general');
  });

  it('越界下标夹到边界：负数当 0、超长当末尾', () => {
    const layout = defaultLayout();
    expect(order(moveGroupTo(layout, 'g-personal', -999) as SidebarLayout))
      .toBe('g-personal,g-general,g-admin');
    expect(order(moveGroupTo(layout, 'g-general', 999) as SidebarLayout))
      .toBe('g-admin,g-personal,g-general');
  });

  it('落在原位（自身下标 / 自身下标+1）返回 null：松手不写盘', () => {
    const layout = defaultLayout();
    expect(moveGroupTo(layout, 'g-admin', 1)).toBeNull();
    expect(moveGroupTo(layout, 'g-admin', 2)).toBeNull();
  });

  it('未知 id / 非数字落点返回 null，不抛错（指针跑到列表外）', () => {
    const layout = defaultLayout();
    expect(moveGroupTo(layout, 'g-nope', 0)).toBeNull();
    expect(moveGroupTo(layout, 'g-admin', Number.NaN)).toBeNull();
  });

  it('只动顺序：组内菜单、整组隐藏、单项隐藏一律原样', () => {
    const layout = layoutWithCustom();
    group(layout, 'g-admin').hidden = true;
    layout.hidden = ['/logs'];
    const next = moveGroupTo(layout, 'g-personal', 0) as SidebarLayout;

    // 按 id 取，不能按下标比：这一条本来就在换顺序，下标对齐是假命题
    for (const id of ['g-general', 'g-admin', 'g-personal']) {
      expect(group(next, id).items).toEqual(group(layout, id).items);
    }
    expect(group(next, 'g-admin').hidden).toBe(true);
    expect(next.hidden).toEqual(['/logs']);
    // 入参没被改（调用方可能还握着旧引用画界面）
    expect(order(layout)).toBe('g-general,g-admin,g-personal,g-custom-1');
  });
});

describe('moveGroupStep（Alt+↑ / ↓ 键盘走一格）', () => {
  it('↑ 把当前组与前一组交换（插入位是 from-1，不是 from）', () => {
    const layout = defaultLayout();
    const next = moveGroupStep(layout, 'g-admin', -1);
    expect(next).not.toBeNull();
    expect(order(next as SidebarLayout)).toBe('g-admin,g-general,g-personal');
  });

  it('↓ 把当前组与后一组交换', () => {
    const layout = defaultLayout();
    const next = moveGroupStep(layout, 'g-admin', 1);
    expect(order(next as SidebarLayout)).toBe('g-general,g-personal,g-admin');
  });

  it('已在首 / 尾时返回 null（不循环绕到另一边）', () => {
    const layout = defaultLayout();
    expect(moveGroupStep(layout, 'g-general', -1)).toBeNull();
    expect(moveGroupStep(layout, 'g-personal', 1)).toBeNull();
  });

  it('delta=0 与未知 id 不动', () => {
    const layout = defaultLayout();
    expect(moveGroupStep(layout, 'g-admin', 0)).toBeNull();
    expect(moveGroupStep(layout, 'g-nope', -1)).toBeNull();
  });

  it('连按两次 ↓ 能穿过一整组（不是只换一次就卡住）', () => {
    let layout = defaultLayout();
    layout = moveGroupStep(layout, 'g-general', 1) as SidebarLayout;
    expect(order(layout)).toBe('g-admin,g-general,g-personal');
    layout = moveGroupStep(layout, 'g-general', 1) as SidebarLayout;
    expect(order(layout)).toBe('g-admin,g-personal,g-general');
  });
});

describe('moveGroupAcrossVisible（Alt+↑ / ↓ 在可见组之间走一格）', () => {
  it('全部可见时行为与 moveGroupStep 一致（↑ 与本组前的一组换位）', () => {
    const layout = defaultLayout();
    const ids = layout.groups.map((g) => g.id);
    const next = moveGroupAcrossVisible(layout, ids, 'g-admin', -1) as SidebarLayout;
    expect(order(next)).toBe('g-admin,g-general,g-personal');
  });

  it('画面上只有两个组（中间那组整组隐藏）时，↓ 直接与下一个**可见**组换位', () => {
    // 普通用户视角：管理组 ownedMenusOf 为空 / 或管理员把它整组隐藏 → 画面上只有 通用、个人
    const layout = defaultLayout();
    const visible = ['g-general', 'g-personal'];
    const next = moveGroupAcrossVisible(layout, visible, 'g-general', 1);
    // 旧实现（按布局下标 +2）会得到 g-admin,g-general,g-personal —— 界面上什么都没变
    expect(next).not.toBeNull();
    const after = order(next as SidebarLayout);
    expect(after.split(',').indexOf('g-personal')).toBeLessThan(after.split(',').indexOf('g-general'));
  });

  it('↑ 在可见组里上移：插到上邻之前，被跳过的隐藏组留在原处', () => {
    const layout = defaultLayout();
    // 布局：通用(0) 管理(1) 个人(2)；画面上看到 管理、个人（通用被收起）
    const visible = ['g-admin', 'g-personal'];
    const next = moveGroupAcrossVisible(layout, visible, 'g-personal', -1) as SidebarLayout;
    expect(order(next)).toBe('g-general,g-personal,g-admin');
    // 「通用」仍在最前 —— 隐藏/不可见的组不该被顺手挪走
    expect(next.groups[0].id).toBe('g-general');
  });

  it('可见序列的边界上不动（不绕到另一头）', () => {
    const layout = defaultLayout();
    const visible = ['g-general', 'g-personal'];
    expect(moveGroupAcrossVisible(layout, visible, 'g-general', -1)).toBeNull();
    expect(moveGroupAcrossVisible(layout, visible, 'g-personal', 1)).toBeNull();
  });

  it('不在可见序列里的组 / delta=0 返回 null', () => {
    const layout = defaultLayout();
    expect(moveGroupAcrossVisible(layout, ['g-general'], 'g-admin', 1)).toBeNull();
    expect(moveGroupAcrossVisible(layout, ['g-general', 'g-admin'], 'g-admin', 0)).toBeNull();
  });

  it('可见序列末尾下移：落到布局末尾（夹在后面那些看不见的组留在前面）', () => {
    const layout = defaultLayout();   // 通用(0), 管理(1), 个人(2)
    // 画面上只看到 管理 → 个人（通用这一组当前不可见）；把「管理」往下拖，下面没有第三个可见组了
    const visible = ['g-admin', 'g-personal'];
    const next = moveGroupAcrossVisible(layout, visible, 'g-admin', 1) as SidebarLayout;
    expect(order(next)).toBe('g-general,g-personal,g-admin');
  });
});

describe('commitGroupRename（改名只改显示名）', () => {
  it('空名 / 全空白回退原显示名（返回 null，不写空标题）', () => {
    const layout = defaultLayout();
    expect(commitGroupRename(layout, 'g-general', '')).toBeNull();
    expect(commitGroupRename(layout, 'g-general', '   ')).toBeNull();
    expect(commitGroupRename(layout, 'g-general', '\t\n')).toBeNull();
  });

  it('首尾空格裁掉后再存（否则「通用 」与「通用」成两个名字）', () => {
    const layout = defaultLayout();
    const next = commitGroupRename(layout, 'g-general', '  常用  ') as SidebarLayout;
    expect(group(next, 'g-general').label).toBe('常用');
  });

  it('与原名相同返回 null（不白写一次缓存 + 一次 PUT）', () => {
    const layout = defaultLayout();
    expect(commitGroupRename(layout, 'g-general', '通用')).toBeNull();
    expect(commitGroupRename(layout, 'g-general', ' 通用 ')).toBeNull();
  });

  it('只改 label：id / items / hidden / custom 全不变（id 才是顺序与隐藏的键）', () => {
    const layout = defaultLayout();
    group(layout, 'g-general').hidden = true;
    const next = commitGroupRename(layout, 'g-general', '工作台') as SidebarLayout;

    expect(group(next, 'g-general').label).toBe('工作台');
    expect(group(next, 'g-general').id).toBe('g-general');
    expect(group(next, 'g-general').items).toEqual(group(layout, 'g-general').items);
    expect(group(next, 'g-general').hidden).toBe(true);
    expect(order(next)).toBe(order(layout));
    // 入参标签没被就地改掉
    expect(group(layout, 'g-general').label).toBe('通用');
  });

  it('未知 id 返回 null', () => {
    expect(commitGroupRename(defaultLayout(), 'g-nope', '随便')).toBeNull();
  });
});

describe('groupOrderIsCustomized（决定「恢复默认分组」出不出）', () => {
  it('默认布局 = 未定制（按钮不出现）', () => {
    expect(groupOrderIsCustomized(defaultLayout())).toBe(false);
  });

  it('自建分组追加在末尾 ≠ 定制（新建分组不该点亮恢复按钮）', () => {
    expect(groupOrderIsCustomized(layoutWithCustom())).toBe(false);
  });

  it('换了顺序 = 定制', () => {
    const layout = cloneLayout(defaultLayout());
    layout.groups.reverse();
    expect(groupOrderIsCustomized(layout)).toBe(true);
  });

  it('内置组改了显示名 = 定制', () => {
    const layout = defaultLayout();
    group(layout, 'g-admin').label = '后台';
    expect(groupOrderIsCustomized(layout)).toBe(true);
  });

  it('自建组改名不算定制（它没有默认名可回退）', () => {
    const layout = layoutWithCustom();
    group(layout, 'g-custom-1').label = '我改过';
    expect(groupOrderIsCustomized(layout)).toBe(false);
  });

  it('内置组被删 = 定制（此时按钮是唯一找回路径）', () => {
    const layout = defaultLayout();
    layout.groups = layout.groups.filter((g) => g.id !== 'g-personal');
    expect(groupOrderIsCustomized(layout)).toBe(true);
  });
});

describe('resetGroupOrderAndNames（恢复默认分组）', () => {
  it('顺序与内置名回默认，隐藏状态保留（清隐藏是底部恢复条的事）', () => {
    const layout = layoutWithCustom();
    layout.groups.reverse();
    group(layout, 'g-admin').label = '后台';
    group(layout, 'g-admin').hidden = true;
    layout.hidden = ['/logs'];

    const next = resetGroupOrderAndNames(layout) as SidebarLayout;
    expect(order(next)).toBe('g-general,g-admin,g-personal,g-custom-1');
    expect(group(next, 'g-admin').label).toBe('管理');
    expect(group(next, 'g-admin').hidden).toBe(true);
    expect(next.hidden).toEqual(['/logs']);
  });

  it('自建分组保留（含用户自己写的名字），统一排在内置组之后', () => {
    const layout = layoutWithCustom();
    // 把自建组挪到最前 + 改名：恢复默认后它仍叫「常用」，只是回到末尾
    const custom = layout.groups.pop() as SidebarLayout['groups'][number];
    layout.groups.unshift(custom);
    group(layout, 'g-custom-1').label = '常用组';

    const next = resetGroupOrderAndNames(layout) as SidebarLayout;
    expect(order(next)).toBe('g-general,g-admin,g-personal,g-custom-1');
    expect(group(next, 'g-custom-1').label).toBe('常用组');
    expect(group(next, 'g-custom-1').items).toEqual(['/chat']);
  });

  it('内置组被删过的补回空组（默认名、未隐藏、不放菜单）', () => {
    const layout = defaultLayout();
    group(layout, 'g-personal').items.forEach((key) => group(layout, 'g-general').items.push(key));
    layout.groups = layout.groups.filter((g) => g.id !== 'g-personal');

    const next = resetGroupOrderAndNames(layout) as SidebarLayout;
    expect(order(next)).toBe('g-general,g-admin,g-personal');
    expect(group(next, 'g-personal')).toMatchObject({ label: '个人', custom: false, hidden: false, items: [] });
  });

  it('已经是默认就返回 null（不白写盘）', () => {
    expect(resetGroupOrderAndNames(defaultLayout())).toBeNull();
    expect(resetGroupOrderAndNames(layoutWithCustom())).toBeNull();
  });

  it('改完立刻判「不再是定制」（按钮该自己消失）', () => {
    const layout = defaultLayout();
    layout.groups.reverse();
    expect(groupOrderIsCustomized(layout)).toBe(true);
    const next = resetGroupOrderAndNames(layout) as SidebarLayout;
    expect(groupOrderIsCustomized(next)).toBe(false);
  });
});

describe('dropLineBefore（插入线挂在哪一组前面）', () => {
  const layout = defaultLayout(); // g-general, g-admin, g-personal

  it('落点就是某组下标时，线画在该组之前', () => {
    expect(dropLineBefore(layout, ['g-general', 'g-admin', 'g-personal'], 1)).toBe('g-admin');
    expect(dropLineBefore(layout, ['g-general', 'g-admin', 'g-personal'], 0)).toBe('g-general');
  });

  it('落点指向末尾（= groups.length）时返回 null，由调用方画在列表末尾', () => {
    expect(dropLineBefore(layout, ['g-general', 'g-admin', 'g-personal'], 3)).toBeNull();
  });

  it('落点被隐藏组占住（不在 DOM 里）时顺延到下一个真的渲染出来的组', () => {
    // 「管理」整组隐藏：画面上只剩 通用 → 个人；拖到「管理」的位置应显示在「个人」之前
    const hiddenLayout = cloneLayout(layout);
    group(hiddenLayout, 'g-admin').hidden = true;
    const rendered = ['g-general', 'g-personal'];
    expect(dropLineBefore(hiddenLayout, rendered, 1)).toBe('g-personal');
  });

  it('全部渲染组都在落点之前时返回 null（拖到末尾）', () => {
    expect(dropLineBefore(layout, ['g-general'], 2)).toBeNull();
  });
});
