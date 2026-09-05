import React from 'react';
import { cn } from '../../lib/utils';

export interface TabItem<T extends string = string> {
  key: T;
  label: React.ReactNode;
  disabled?: boolean;
}

export interface TabsProps<T extends string = string> {
  items: TabItem<T>[];
  active: T;
  onChange: (key: T) => void;
  className?: string;
  ariaLabel?: string;
}

/**
 * Tabs — 通用标签页切换，使用 App.css 的 .ui-tabs/.ui-tab 样式。
 * 与页面私有 sub-tabs 并存，为共享组件层提供统一实现。
 */
export default function Tabs<T extends string = string>({
  items,
  active,
  onChange,
  className,
  ariaLabel
}: TabsProps<T>) {
  return (
    <div className={cn('ui-tabs', className)} role="tablist" aria-label={ariaLabel}>
      {items.map((item) => (
        <button
          key={item.key}
          type="button"
          role="tab"
          aria-selected={item.key === active}
          disabled={item.disabled}
          className={cn('ui-tab', item.key === active && 'active')}
          onClick={() => onChange(item.key)}
        >
          {item.label}
        </button>
      ))}
    </div>
  );
}