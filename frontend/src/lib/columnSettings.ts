/**
 * AIGX · 表格列定制状态（列显隐 / 顺序 / 对齐）的跨次启动记忆。
 *
 * 每个表格页一个自己的 localStorage 键，值是 `{ columns: ColumnSetting[] }`，
 * 每条 ColumnSetting 记录某列的 key、是否可见、对齐方式。顺序由数组顺序表达。
 *
 * 对齐 Agent2API desktop-tauri/ui 的列设置持久化口径。
 */

export interface ColumnSetting {
  key: string;
  visible: boolean;
  align: 'left' | 'center' | 'right';
}

export interface ColumnSettingsState {
  columns: ColumnSetting[];
}

/** 读一份列设置。存坏 / 不存在时返回 null（由调用方回落默认）。 */
export function getColumnSettings(key: string): ColumnSettingsState | null {
  try {
    const raw = localStorage.getItem(key);
    if (!raw) return null;
    const parsed: unknown = JSON.parse(raw);
    if (
      parsed &&
      typeof parsed === 'object' &&
      !Array.isArray(parsed) &&
      Array.isArray((parsed as { columns?: unknown }).columns)
    ) {
      return parsed as ColumnSettingsState;
    }
    return null;
  } catch {
    return null;
  }
}

/** 写一份列设置（全量覆盖）。 */
export function setColumnSettings(key: string, state: ColumnSettingsState): void {
  try {
    localStorage.setItem(key, JSON.stringify(state));
  } catch {
    // 存储不可用只影响下次打开
  }
}

/** 清空一份列设置的记忆。 */
export function clearColumnSettings(key: string): void {
  try {
    localStorage.removeItem(key);
  } catch {
    // 存储不可用时静默
  }
}

/**
 * 从列定义生成默认列设置（全部可见、左对齐）。
 * 用于首次打开或存储为空时初始化。
 */
export function defaultColumnSettings(
  columnDefs: Array<{ key: string }>
): ColumnSettingsState {
  return {
    columns: columnDefs.map((c) => ({
      key: c.key,
      visible: true,
      align: 'left' as const,
    })),
  };
}

/**
 * 把列设置应用到列定义上，返回实际渲染的列（过滤可见 + 按顺序排列）。
 * 对齐方式写回 col.align，列设置里没有的列原样保留在末尾。
 */
export function applyColumnSettings<T extends { key: string; align?: 'left' | 'center' | 'right' }>(
  columns: T[],
  settings: ColumnSettingsState | null
): T[] {
  if (!settings) return columns;
  const ordered: T[] = [];
  const seen = new Set<string>();
  for (const s of settings.columns) {
    if (!s.visible) continue;
    const col = columns.find((c) => c.key === s.key);
    if (col) {
      ordered.push({ ...col, align: s.align });
      seen.add(s.key);
    }
  }
  // 列设置里没提到的列（新增列）原样追加到末尾
  for (const col of columns) {
    if (!seen.has(col.key)) ordered.push(col);
  }
  return ordered;
}
