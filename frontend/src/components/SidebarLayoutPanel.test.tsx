// 设置页（components/SidebarLayoutPanel.tsx）的键盘排序通路。
//
// 为什么单独测这一层而不是只测 lib：这个缺陷的形态是「面板把参数算错」，
// 纯函数本身没错。只测 lib 时，把面板改回 `moveGroup(layout, id, index)` 依然全绿
// （lib 没被碰），缺陷就重新溜回去。所以这里从**真实元素**上按 Alt+方向键，
// 断言布局顺序真的换了。
import { describe, expect, it, beforeEach, vi } from 'vitest';
import { render, fireEvent, act, cleanup, screen } from '@testing-library/react';
import SidebarLayoutPanel from './SidebarLayoutPanel';
import i18n from '../i18n';
import { STORAGE_KEY, defaultLayout, type SidebarLayout } from '../lib/sidebar-layout';

/**
 * 面板**当前展示的**布局：写盘过就读盘，没写就是默认布局。
 *
 * 面板只在 apply()（真的改了东西）时才写 localStorage，所以「刚挂载」时缓存是空的。
 * 直接 throw 会让「先取 before、再按方向键」的用例在按下之前就炸，
 * 而它们要验的恰恰是「按了之后才该有写盘」。
 */
function currentLayout(): SidebarLayout {
  const raw = localStorage.getItem(STORAGE_KEY);
  return raw ? (JSON.parse(raw) as SidebarLayout) : defaultLayout();
}

/** 有没有写过盘（用来验「首/末位按方向键是空操作」）。 */
function hasWritten(): boolean {
  return localStorage.getItem(STORAGE_KEY) !== null;
}

/** 组 id 顺序。 */
function groupOrder(): string {
  return currentLayout().groups.map((g) => g.id).join(',');
}

/** 某组内菜单顺序。 */
function itemsOf(gid: string): string {
  return currentLayout().groups.find((g) => g.id === gid)!.items.join(',');
}

/** 第 n 个组把手的真实 DOM 元素（面板用 .sl-grip，组卡片带 data-sl-group）。 */
function groupGrip(index: number): HTMLElement {
  const card = document.querySelectorAll('[data-sl-group]')[index] as HTMLElement;
  const grip = card.querySelector('.sl-grip') as HTMLElement | null;
  if (!grip) throw new Error(`第 ${index} 组没有把手`);
  return grip;
}

/** 指定菜单行的把手。 */
function itemGrip(key: string): HTMLElement {
  const row = document.querySelector(`[data-sl-item="${key}"]`) as HTMLElement | null;
  const grip = row?.querySelector('.sl-grip') as HTMLElement | null;
  if (!grip) throw new Error(`菜单 ${key} 没有把手`);
  return grip;
}

beforeEach(async () => {
  cleanup();
  localStorage.clear();
  vi.restoreAllMocks();
  // 全量套件里别的测试文件会把 i18n_lang 污染成 en（jsdom 共享环境），
  // 那时按文字找「恢复默认」会找不到 —— 每条用例先把语言钉回中文
  // （与 dashboard/*.test.tsx 同一套写法）。单独跑本文件时这条同样成立。
  localStorage.setItem('i18n_lang', 'zh');
  await i18n.changeLanguage('zh');
  // 面板挂载就会读缓存并对服务端拉一次；jsdom 里没有后端，接口失败只 warn。
  vi.spyOn(globalThis, 'fetch').mockRejectedValue(new Error('offline'));
});

describe('SidebarLayoutPanel 键盘排序：Alt+↑ 必须真的换位（不是「没报错」）', () => {
  it('组把手 Alt+↑ 把第二组挪到第一组之前', () => {
    render(<SidebarLayoutPanel />);
    expect(groupOrder()).toBe('g-general,g-admin,g-personal');

    act(() => { fireEvent.keyDown(groupGrip(1), { key: 'ArrowUp', altKey: true }); });

    // 改前（moveGroup(layout, id, index)）这里是原位 → 返回 null → 顺序一点没变，
    // 也就是「按了没反应」，而测试若只写 not.toThrow 依然绿。
    expect(groupOrder()).toBe('g-admin,g-general,g-personal');
    // DOM 也跟着换了（不是只改了缓存）
    const domIds = Array.from(document.querySelectorAll('[data-sl-group]'))
      .map((el) => (el as HTMLElement).dataset.slGroup);
    expect(domIds.slice(0, 2)).toEqual(['g-admin', 'g-general']);
  });

  it('组把手 Alt+↑ 在首组是空操作（不绕到末尾，也不写盘）', () => {
    render(<SidebarLayoutPanel />);
    act(() => { fireEvent.keyDown(groupGrip(0), { key: 'ArrowUp', altKey: true }); });
    expect(hasWritten()).toBe(false);
    expect(Array.from(document.querySelectorAll('[data-sl-group]'))
      .map((el) => (el as HTMLElement).dataset.slGroup))
      .toEqual(['g-general', 'g-admin', 'g-personal']);
  });

  it('组把手 Alt+↓ 把首组挪到第二组之后', () => {
    render(<SidebarLayoutPanel />);
    act(() => { fireEvent.keyDown(groupGrip(0), { key: 'ArrowDown', altKey: true }); });
    expect(groupOrder()).toBe('g-admin,g-general,g-personal');
  });

  it('组把手 Alt+↓ 在末组是空操作', () => {
    render(<SidebarLayoutPanel />);
    act(() => { fireEvent.keyDown(groupGrip(2), { key: 'ArrowDown', altKey: true }); });
    expect(hasWritten()).toBe(false);
  });

  it('组内菜单 Alt+↑ 真的换位（改前上下两条腿都是死键）', () => {
    render(<SidebarLayoutPanel />);
    const before = itemsOf('g-general');           // '/' ,'/chat', ...
    const [first, second] = before.split(',');
    act(() => { fireEvent.keyDown(itemGrip(second), { key: 'ArrowUp', altKey: true }); });
    const after = itemsOf('g-general');
    expect(after.split(',')[0]).toBe(second);
    expect(after.split(',')[1]).toBe(first);
  });

  it('组内菜单 Alt+↓ 真的换位', () => {
    render(<SidebarLayoutPanel />);
    const before = itemsOf('g-general');
    const [first, second] = before.split(',');
    act(() => { fireEvent.keyDown(itemGrip(first), { key: 'ArrowDown', altKey: true }); });
    const after = itemsOf('g-general');
    expect(after.split(',')[0]).toBe(second);
    expect(after.split(',')[1]).toBe(first);
  });

  it('组内首项 Alt+↑ 跨到上一组末尾（跨组那条支路不能被「组内交换」顶掉）', () => {
    render(<SidebarLayoutPanel />);
    const firstKey = itemsOf('g-general').split(',')[0];
    act(() => { fireEvent.keyDown(itemGrip(firstKey), { key: 'ArrowUp', altKey: true }); });
    // 第一组的首项没有上一组 → 不动、不写盘
    expect(hasWritten()).toBe(false);

    // 第二组（管理）的首项上移 → 落到第一组（通用）末尾
    const adminFirst = itemsOf('g-admin').split(',')[0];
    act(() => { fireEvent.keyDown(itemGrip(adminFirst), { key: 'ArrowUp', altKey: true }); });
    expect(itemsOf('g-general').split(',').pop()).toBe(adminFirst);
    expect(itemsOf('g-admin').split(',')).not.toContain(adminFirst);
  });
});

describe('恢复默认（设置页）仍然整体可用', () => {
  it('默认布局下点「恢复默认」写回的仍是默认分组（自建分组会被清，属面板既有口径）', () => {
    render(<SidebarLayoutPanel />);
    const reset = screen.getByRole('button', { name: '恢复默认' });
    vi.spyOn(window, 'confirm').mockReturnValue(true);
    act(() => { fireEvent.click(reset); });
    expect(currentLayout().groups.map((g) => g.id))
      .toEqual(defaultLayout().groups.map((g) => g.id));
  });
});