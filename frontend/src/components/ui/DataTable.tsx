import React, { useState, useRef, useCallback } from 'react';
import { cn } from '../../lib/utils';
import ColumnSettingsDrawer from './ColumnSettingsDrawer';
import {
  type ColumnSettingsState,
  getColumnSettings,
  setColumnSettings,
  defaultColumnSettings,
  applyColumnSettings,
} from '../../lib/columnSettings';

export interface Column<T> {
  key: string;
  header: React.ReactNode;
  render: (row: T, index: number) => React.ReactNode;
  className?: string;
  /** 列宽（CSS 值），可选 */
  width?: string;
  /** 对齐方式，默认 left。可被列设置覆盖 */
  align?: 'left' | 'center' | 'right';
  /** 是否允许列宽拖动，默认 true */
  resizable?: boolean;
  /** 是否固定列（不参与显隐/排序），默认 false */
  fixed?: boolean;
}

export interface DataTableProps<T> {
  columns: Column<T>[];
  data: T[];
  rowKey: (row: T, index: number) => string | number;
  className?: string;
  emptyText?: string;
  /** 当数据为空时展示的自定义内容 */
  emptyNode?: React.ReactNode;
  /** localStorage 键（启用列设置持久化时传） */
  columnSettingsKey?: string;
  /** 列设置抽屉的触发按钮文字，默认「列设置」 */
  columnSettingsLabel?: string;
}

/**
 * DataTable — 通用数据表格，复用 App.css 的 .table-wrapper/table 样式体系。
 *
 * 支持：
 * - 列显隐 / 顺序 / 对齐（localStorage 持久化，通过列设置抽屉）
 * - 列宽拖动（拖动 th 右侧手柄）
 * - 空数据提示
 */
export default function DataTable<T>({
  columns,
  data,
  rowKey,
  className,
  emptyText = '暂无数据',
  emptyNode,
  columnSettingsKey,
  columnSettingsLabel = '列设置',
}: DataTableProps<T>) {
  const [drawerOpen, setDrawerOpen] = useState(false);
  const [columnWidths, setColumnWidths] = useState<Record<string, string>>({});
  const dragState = useRef<{ key: string; startX: number; startWidth: number } | null>(null);

  // 读取列设置（持久化）
  const [settings, setSettings] = useState<ColumnSettingsState>(() => {
    if (!columnSettingsKey) return { columns: [] };
    const stored = getColumnSettings(columnSettingsKey);
    if (stored) return stored;
    return defaultColumnSettings(columns);
  });

  const effectiveColumns = applyColumnSettings(columns, settings);

  const onColumnsChange = useCallback(
    (next: ColumnSettingsState) => {
      setSettings(next);
      if (columnSettingsKey) setColumnSettings(columnSettingsKey, next);
    },
    [columnSettingsKey]
  );

  // 列宽拖动
  const startResize = (e: React.MouseEvent, col: Column<T>) => {
    if (col.resizable === false) return;
    e.preventDefault();
    e.stopPropagation();
    const th = (e.currentTarget as HTMLElement).closest('th') as HTMLElement;
    const startWidth = th.offsetWidth;
    dragState.current = { key: col.key, startX: e.clientX, startWidth };
    const onMove = (ev: MouseEvent) => {
      if (!dragState.current) return;
      const delta = ev.clientX - dragState.current.startX;
      const newWidth = Math.max(60, dragState.current.startWidth + delta);
      setColumnWidths((prev) => ({ ...prev, [col.key]: `${newWidth}px` }));
    };
    const onUp = () => {
      dragState.current = null;
      document.removeEventListener('mousemove', onMove);
      document.removeEventListener('mouseup', onUp);
    };
    document.addEventListener('mousemove', onMove);
    document.addEventListener('mouseup', onUp);
  };

  if (data.length === 0) {
    if (emptyNode) return <>{emptyNode}</>;
    return (
      <div className="empty-state">
        <p>{emptyText}</p>
      </div>
    );
  }

  return (
    <>
      <div className={cn('table-wrapper', className)}>
        {columnSettingsKey && (
          <div className="column-settings-trigger" style={{ marginBottom: '8px' }}>
            <button
              className="btn-secondary"
              onClick={() => setDrawerOpen(true)}
              style={{ padding: '4px 12px', fontSize: '12px' }}
            >
              {columnSettingsLabel}
            </button>
          </div>
        )}
        <table>
          <thead>
            <tr>
              {effectiveColumns.map((col) => {
                const width = columnWidths[col.key] ?? col.width;
                const align = col.align ?? 'left';
                const setting = settings.columns.find((s) => s.key === col.key);
                const finalAlign = setting?.align ?? align;
                return (
                  <th
                    key={col.key}
                    style={width ? { width } : undefined}
                    className={cn(
                      col.className,
                      `align-${finalAlign}`,
                      col.resizable !== false && 'col-resizable'
                    )}
                  >
                    {col.header}
                    {col.resizable !== false && (
                      <span
                        className="column-resize-handle"
                        onMouseDown={(e) => startResize(e, col)}
                      />
                    )}
                  </th>
                );
              })}
            </tr>
          </thead>
          <tbody>
            {data.map((row, index) => (
              <tr key={rowKey(row, index)}>
                {effectiveColumns.map((col) => {
                  const setting = settings.columns.find((s) => s.key === col.key);
                  const finalAlign = setting?.align ?? col.align ?? 'left';
                  return (
                    <td
                      key={col.key}
                      className={cn(col.className, `align-${finalAlign}`)}
                    >
                      {col.render(row, index)}
                    </td>
                  );
                })}
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      <ColumnSettingsDrawer
        storageKey={columnSettingsKey!}
        columns={columns}
        open={drawerOpen}
        onOpenChange={setDrawerOpen}
        onChange={onColumnsChange}
      />
    </>
  );
}
