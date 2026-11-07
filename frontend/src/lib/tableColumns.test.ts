import { describe, expect, it, beforeEach } from 'vitest';
import {
  applyTableColumns,
  defaultTableColumns,
  readTableColumns,
  resetTableColumns,
  writeTableColumns,
  type TableColumnDef,
  type TableColumnState,
} from './tableColumns';

const DEFS: TableColumnDef[] = [
  { key: 'check', label: '选择', align: 'center', width: 34, locked: true },
  { key: 'name', label: '名称', width: 200 },
  { key: 'status', label: '状态', align: 'center' },
  { key: 'act', label: '操作', width: 120, locked: true },
];

const KEY = 'test-table-columns';

/** 内存版存储（不碰真 localStorage） */
function memStore(initial: Record<string, string> = {}) {
  const store = new Map(Object.entries(initial));
  return {
    getItem: (k: string) => store.get(k) ?? null,
    setItem: (k: string, v: string) => void store.set(k, v),
    removeItem: (k: string) => void store.delete(k),
    dump: () => Object.fromEntries(store),
  };
}

beforeEach(() => {
  // 防用例间串键（读走注入的 memStore，这里只清可能的默认实现调用）
});

describe('defaultTableColumns', () => {
  it('生成全默认：声明顺序、全部可见、声明对齐/宽度/锁定', () => {
    const st = defaultTableColumns(DEFS);
    expect(st.v).toBe(2);
    expect(st.items.map((i) => i.key)).toEqual(['check', 'name', 'status', 'act']);
    expect(st.items[0]).toMatchObject({ visible: true, align: 'center', width: 34, locked: true });
    expect(st.items[2]).toMatchObject({ visible: true, align: 'center' });
    expect(st.items[2].width).toBeUndefined(); // 弹性列无宽
    expect(st.defaults).toEqual(st.items);
  });
});

describe('readTableColumns 脏值校验', () => {
  it('无存盘返回全默认', () => {
    const s = memStore();
    expect(readTableColumns(KEY, DEFS, s.getItem)).toEqual(defaultTableColumns(DEFS));
  });

  it('存坏 JSON 退回全默认', () => {
    const s = memStore({ [KEY]: '{not json' });
    expect(readTableColumns(KEY, DEFS, s.getItem)).toEqual(defaultTableColumns(DEFS));
  });

  it('数组/标量存盘视为存坏', () => {
    const s = memStore({ [KEY]: JSON.stringify([1, 2]) });
    expect(readTableColumns(KEY, DEFS, s.getItem)).toEqual(defaultTableColumns(DEFS));
  });

  it('未登记新列按声明相对位置插回，多余键丢弃', () => {
    const st = defaultTableColumns(DEFS);
    const saved: TableColumnState = {
      v: 2,
      defaults: st.defaults,
      items: [
        { key: 'check', visible: true, align: 'center', width: 34, locked: true },
        { key: 'ghost', visible: false, align: 'left' }, // 不存在的键 → 丢弃
        { key: 'act', visible: true, align: 'left', width: 120, locked: true },
        // name / status 缺失 → 插回
      ],
    };
    const s = memStore({ [KEY]: JSON.stringify(saved) });
    const read = readTableColumns(KEY, DEFS, s.getItem);
    expect(read.items.map((i) => i.key)).toEqual(['check', 'name', 'status', 'act']);
  });

  it('脏字段逐项退默认：非法对齐/负宽度/字符串宽度', () => {
    const st = defaultTableColumns(DEFS);
    const saved: TableColumnState = {
      v: 2,
      defaults: st.defaults,
      items: [
        { key: 'check', visible: false, align: 'center', width: 34, locked: true }, // 锁定列 visible=false 强制 true
        { key: 'name', visible: true, align: 'diagonal' as never, width: -5 as never },
        { key: 'status', visible: false, align: 'right', width: '80' as never }, // 宽度脏 → 取声明（undefined 弹性）
        { key: 'act', visible: true, align: 'left', width: 120, locked: true },
      ],
    };
    const s = memStore({ [KEY]: JSON.stringify(saved) });
    const read = readTableColumns(KEY, DEFS, s.getItem);
    expect(read.items[0].visible).toBe(true); // 锁定恒显示
    expect(read.items[1].align).toBe('left'); // 非法对齐退声明默认
    expect(read.items[1].width).toBe(200);
    expect(read.items[2].width).toBeUndefined();
    expect(read.items[2].visible).toBe(false); // 非锁定的显隐用户说了算
  });

  it('v1 旧格式迁移：显隐/对齐/顺序保留，宽取声明', () => {
    const v1 = {
      columns: [
        { key: 'act', visible: true, align: 'left' },
        { key: 'name', visible: false, align: 'right' },
        { key: 'status', visible: true, align: 'center' },
        { key: 'check', visible: true, align: 'center' },
      ],
    };
    const s = memStore({ [KEY]: JSON.stringify(v1) });
    const read = readTableColumns(KEY, DEFS, s.getItem);
    expect(read.v).toBe(2);
    expect(read.items.map((i) => i.key)).toEqual(['act', 'name', 'status', 'check']);
    expect(read.items[1].visible).toBe(false);
    expect(read.items[1].align).toBe('right');
    expect(read.items[1].width).toBe(200); // v1 无宽 → 声明默认
  });

  it('v1 形态也存坏时退全默认', () => {
    const s = memStore({ [KEY]: JSON.stringify({ columns: 'nope' }) });
    expect(readTableColumns(KEY, DEFS, s.getItem)).toEqual(defaultTableColumns(DEFS));
  });
});

describe('write / reset', () => {
  it('写入后读回一致（round-trip）', () => {
    const s = memStore();
    const st = defaultTableColumns(DEFS);
    const modified: TableColumnState = {
      ...st,
      items: st.items.map((i) => (i.key === 'name' ? { ...i, width: 321, align: 'right' } : i)),
    };
    writeTableColumns(KEY, modified, s.setItem);
    expect(readTableColumns(KEY, DEFS, s.getItem)).toEqual(modified);
  });

  it('存储抛错静默（隐私模式）', () => {
    expect(() =>
      writeTableColumns(KEY, defaultTableColumns(DEFS), () => {
        throw new Error('quota');
      }),
    ).not.toThrow();
    expect(() =>
      readTableColumns(KEY, DEFS, () => {
        throw new Error('unavailable');
      }),
    ).not.toThrow();
  });

  it('reset 删键并返回全默认', () => {
    const s = memStore({ [KEY]: '{"v":2}' });
    const st = resetTableColumns(KEY, DEFS, s.removeItem);
    expect(st).toEqual(defaultTableColumns(DEFS));
    expect(s.getItem(KEY)).toBeNull();
  });
});

describe('applyTableColumns', () => {
  it('按用户顺序输出，过滤隐藏列（锁定列除外）', () => {
    const st = defaultTableColumns(DEFS);
    const modified: TableColumnState = {
      ...st,
      items: [
        { key: 'act', visible: true, align: 'left', width: 120, locked: true },
        { key: 'status', visible: false, align: 'center' },
        { key: 'check', visible: true, align: 'center', width: 34, locked: true },
        { key: 'name', visible: true, align: 'left', width: 200 },
      ],
    };
    const applied = applyTableColumns(modified, DEFS);
    expect(applied.map((c) => c.key)).toEqual(['act', 'check', 'name']); // status 被隐藏
    expect(applied[0].locked).toBe(true);
  });

  it('锁定列 visible=false 仍输出（恒显示）', () => {
    const st = defaultTableColumns(DEFS);
    const modified: TableColumnState = {
      ...st,
      items: st.items.map((i) => (i.key === 'check' ? { ...i, visible: false } : i)),
    };
    const applied = applyTableColumns(modified, DEFS);
    expect(applied.map((c) => c.key)).toContain('check');
  });

  it('宽度生效序：用户覆盖 > 声明默认 > 弹性', () => {
    const st = defaultTableColumns(DEFS);
    const modified: TableColumnState = {
      ...st,
      items: st.items.map((i) => (i.key === 'name' ? { ...i, width: 500 } : i)),
    };
    const applied = applyTableColumns(modified, DEFS);
    const byKey = new Map(applied.map((c) => [c.key, c]));
    expect(byKey.get('name')?.width).toBe(500); // 用户覆盖
    expect(byKey.get('act')?.width).toBe(120); // 声明默认
    expect(byKey.get('status')?.width).toBeUndefined(); // 弹性
  });
});
