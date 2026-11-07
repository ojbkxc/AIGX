/**
 * AIGX · 公共表格列设置（v2）——纯逻辑层，无 React 依赖。
 *
 * 功能集（对齐 Agent2API ui/table-columns.js + table-col-settings.tsx 的设计意图，
 * DOM 手法不照抄——AIGX 是 React）：
 * - 列序（数组顺序 = 用户拖出的顺序）、显隐、对齐三档
 * - 列宽覆盖（px）+ 锁定列（恒显示、可固定宽度）
 * - **defaults 快照**：存盘带当时的列声明默认值，用于分辨「用户挑的对齐/显隐」
 *   与「当时默认值」——默认值随版本变化时不会把用户选择误判成默认而重置
 * - 版本号 v2 + 逐字段脏值校验：存坏/缺字段退回该字段默认，整体存坏退回全默认
 *   ——列设置读不出来绝不能让整张表渲染不了
 * - 未登记的新列按声明相对位置插入（老存盘不至于错位），多余键丢弃
 *
 * 存储形态：
 * { v: 2, defaults: [{key,visible,align,width?}], items: [{key,visible,align,width?,locked?}] }
 *
 * 性能：读走 lazy init（组件内 useState 初始化器调用一次），写只在用户动作时
 * 发生；本模块不做任何 render 内 JSON.parse。
 */

/** 列对齐三档 */
export type TableColumnAlign = 'left' | 'center' | 'right';

/** 列声明（页面提供；width 缺省 = 弹性列） */
export interface TableColumnDef {
  key: string;
  /** 设置面板显示名 */
  label: string;
  /** 默认对齐 */
  align?: TableColumnAlign;
  /** 默认列宽 px（缺省 = 弹性列，不写宽度吃表格剩余空间） */
  width?: number;
  /** 锁定：恒显示（勾选框/名称/操作列），面板里不可关 */
  locked?: boolean;
}

/** 一列的用户配置 */
export interface TableColumnItem {
  key: string;
  visible: boolean;
  align: TableColumnAlign;
  /** 列宽覆盖 px（缺省 = 用列声明默认） */
  width?: number;
  /** 锁定列标记（只冗余存声明里的 locked，读回时不信任它） */
  locked?: boolean;
}

/** v2 存盘形态（带 defaults 快照） */
export interface TableColumnState {
  v: 2;
  /** 存盘时刻的列声明默认值快照 */
  defaults: TableColumnItem[];
  /** 用户配置（数组顺序 = 列顺序） */
  items: TableColumnItem[];
}

export const TABLE_COLUMNS_VERSION = 2;

const ALIGNS: TableColumnAlign[] = ['left', 'center', 'right'];

const isAlign = (v: unknown): v is TableColumnAlign => ALIGNS.includes(v as TableColumnAlign);
const isPositiveInt = (v: unknown): v is number =>
  typeof v === 'number' && Number.isFinite(v) && Number.isInteger(v) && v > 0;

function itemOfDef(def: TableColumnDef): TableColumnItem {
  return {
    key: def.key,
    visible: true,
    align: def.align ?? 'left',
    ...(def.width != null ? { width: def.width } : {}),
    ...(def.locked ? { locked: true } : {}),
  };
}

/** 从列声明生成全默认配置 */
export function defaultTableColumns(defs: TableColumnDef[]): TableColumnState {
  const items = defs.map(itemOfDef);
  return { v: TABLE_COLUMNS_VERSION, defaults: items.map((i) => ({ ...i })), items };
}

/**
 * 逐条目脏值校验：以声明为准键集合与锁定位；未声明的键丢弃；
 * 声明里没出现过的键（新版本加列）按声明的相对位置插回。
 */
function sanitizeItems(raw: unknown, defs: TableColumnDef[]): TableColumnItem[] | null {
  if (!Array.isArray(raw)) return null;
  const base = defs.map(itemOfDef);
  const byKey = new Map(base.map((b) => [b.key, b]));
  const items: TableColumnItem[] = [];
  const seen = new Set<string>();
  for (const entry of raw) {
    if (!entry || typeof entry !== 'object' || Array.isArray(entry)) continue;
    const e = entry as { key?: unknown; visible?: unknown; align?: unknown; width?: unknown; locked?: unknown };
    if (typeof e.key !== 'string') continue;
    const def = byKey.get(e.key);
    if (!def || seen.has(e.key)) continue; // 未声明键 / 重复键丢弃
    seen.add(e.key);
    const locked = def.locked === true;
    const width = isPositiveInt(e.width) ? e.width : def.width;
    items.push({
      key: def.key,
      visible: locked ? true : e.visible !== false,
      align: isAlign(e.align) ? e.align : def.align ?? 'left',
      ...(width != null ? { width } : {}),
      ...(locked ? { locked: true } : {}),
    });
  }
  // 声明里有、存盘没有的键（新版本加列）：按声明的相对位置插回
  for (const b of base) {
    if (seen.has(b.key)) continue;
    const specIdx = defs.findIndex((d) => d.key === b.key);
    let insertAt = items.length;
    for (let k = 0; k < items.length; k += 1) {
      if (defs.findIndex((d) => d.key === items[k].key) > specIdx) {
        insertAt = k;
        break;
      }
    }
    items.splice(insertAt, 0, b);
  }
  return items;
}

/** v1（columnSettings.ts 旧格式 `{columns:[{key,visible,align}]}`）→ v2 迁移 */
function migrateV1(raw: unknown, defs: TableColumnDef[]): TableColumnState | null {
  if (!raw || typeof raw !== 'object' || Array.isArray(raw)) return null;
  const columns = (raw as { columns?: unknown }).columns;
  const items = sanitizeItems(columns, defs);
  if (!items) return null;
  const full = defaultTableColumns(defs);
  // v1 只有显隐/对齐/顺序：列宽全部取声明默认；显隐语义一致
  const merged = full.items.map((b) => {
    const found = items.find((i) => i.key === b.key);
    return found ? { ...b, visible: found.visible, align: found.align } : b;
  });
  // 按用户顺序重排
  const order = items.map((i) => i.key);
  merged.sort((a, b) => order.indexOf(a.key) - order.indexOf(b.key));
  return { v: TABLE_COLUMNS_VERSION, defaults: full.defaults, items: merged };
}

/**
 * 读列设置：v2 → v1 迁移 → 全默认。
 * 逐字段脏值校验，整体形态存坏（非对象/键集合对不上）退回全默认。
 */
export function readTableColumns(
  storageKey: string,
  defs: TableColumnDef[],
  /** localStorage 读取器（注入以便测试；生产用默认实现） */
  getItem: (key: string) => string | null = (k) => {
    try {
      return localStorage.getItem(k);
    } catch {
      return null;
    }
  },
): TableColumnState {
  const full = defaultTableColumns(defs);
  let raw: string | null = null;
  try {
    raw = getItem(storageKey);
  } catch {
    return full;
  }
  if (!raw) return full;
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return full;
  }
  if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) return full;
  const obj = parsed as { v?: unknown; defaults?: unknown; items?: unknown };
  if (obj.v === TABLE_COLUMNS_VERSION) {
    const items = sanitizeItems(obj.items, defs);
    if (!items) return full;
    const defaults = sanitizeItems(obj.defaults, defs);
    return { v: TABLE_COLUMNS_VERSION, defaults: defaults ?? full.defaults, items };
  }
  const migrated = migrateV1(parsed, defs);
  return migrated ?? full;
}

/** 写列设置（全量覆盖；存储不可用静默——只影响下次打开） */
export function writeTableColumns(
  storageKey: string,
  state: TableColumnState,
  setItem: (key: string, value: string) => void = (k, v) => {
    try {
      localStorage.setItem(k, v);
    } catch {
      // 隐私模式等存储不可用：静默
    }
  },
): void {
  try {
    setItem(storageKey, JSON.stringify(state));
  } catch {
    // 序列化失败（循环引用等理论不可达）：静默
  }
}

/** 清掉列设置记忆（恢复默认 = 删键 + 返回全默认状态） */
export function resetTableColumns(
  storageKey: string,
  defs: TableColumnDef[],
  removeItem: (key: string) => void = (k) => {
    try {
      localStorage.removeItem(k);
    } catch {
      // 静默
    }
  },
): TableColumnState {
  try {
    removeItem(storageKey);
  } catch {
    // 静默
  }
  return defaultTableColumns(defs);
}

/** 应用后的列视图：顺序 + 显隐过滤 + 生效对齐/宽度/锁定 */
export interface AppliedColumn {
  key: string;
  align: TableColumnAlign;
  /** 生效宽度 px（用户覆盖 > 声明默认；undefined = 弹性列） */
  width?: number;
  locked: boolean;
}

/**
 * 应用列设置到声明：过滤隐藏列 + 按用户顺序输出。
 * 纯函数，返回新数组不改入参；页面每 render 调用 O(n) 可接受（列数 ≤ 20）。
 */
export function applyTableColumns(
  state: TableColumnState,
  defs: TableColumnDef[],
): AppliedColumn[] {
  const byKey = new Map(defs.map((d) => [d.key, d]));
  const out: AppliedColumn[] = [];
  for (const item of state.items) {
    const def = byKey.get(item.key);
    if (!def) continue;
    if (!item.visible && item.locked !== true) continue;
    out.push({
      key: item.key,
      align: item.align,
      width: item.width ?? def.width,
      locked: def.locked === true,
    });
  }
  return out;
}

/** 列是否可见（面板开关用；锁定列恒 true） */
export function isColumnVisible(state: TableColumnState, key: string): boolean {
  const item = state.items.find((i) => i.key === key);
  if (!item) return true;
  return item.visible || item.locked === true;
}
