import React from 'react';
import { cn } from '../../lib/utils';

export interface Column<T> {
  key: string;
  header: React.ReactNode;
  render: (row: T, index: number) => React.ReactNode;
  className?: string;
  /** 列宽（CSS 值），可选 */
  width?: string;
}

export interface DataTableProps<T> {
  columns: Column<T>[];
  data: T[];
  rowKey: (row: T, index: number) => string | number;
  className?: string;
  emptyText?: string;
  /** 当数据为空时展示的自定义内容 */
  emptyNode?: React.ReactNode;
}

/**
 * DataTable — 通用数据表格，复用 App.css 的 .table-wrapper/table 样式体系。
 * 各页面重复的 <table><thead>... 结构可用此组件收敛。
 */
export default function DataTable<T>({
  columns,
  data,
  rowKey,
  className,
  emptyText = '暂无数据',
  emptyNode
}: DataTableProps<T>) {
  if (data.length === 0) {
    if (emptyNode) return <>{emptyNode}</>;
    return (
      <div className="empty-state">
        <p>{emptyText}</p>
      </div>
    );
  }

  return (
    <div className={cn('table-wrapper', className)}>
      <table>
        <thead>
          <tr>
            {columns.map((col) => (
              <th key={col.key} style={col.width ? { width: col.width } : undefined}>
                {col.header}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {data.map((row, index) => (
            <tr key={rowKey(row, index)}>
              {columns.map((col) => (
                <td key={col.key} className={col.className}>
                  {col.render(row, index)}
                </td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}