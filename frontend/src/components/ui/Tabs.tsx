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
 * 键盘按 WAI-ARIA tabs 模式：roving tabindex + ArrowLeft/Right（含 Home/End），
 * 方向键跳过 disabled 项并在两端循环。
 */
export default function Tabs<T extends string = string>({
  items,
  active,
  onChange,
  className,
  ariaLabel
}: TabsProps<T>) {
  function handleKeyDown(event: React.KeyboardEvent<HTMLDivElement>) {
    const enabledKeys = items.filter((item) => !item.disabled).map((item) => item.key);
    if (enabledKeys.length === 0) return;

    // 当前焦点所在 tab 的 key（焦点不在 tab 内时回退到激活项）
    const currentKey = items.find((item) => item.key === active)?.key ?? enabledKeys[0];
    const currentIndex = enabledKeys.indexOf(currentKey);

    let nextIndex: number;
    switch (event.key) {
      case 'ArrowRight':
      case 'ArrowDown':
        nextIndex = (currentIndex + 1) % enabledKeys.length;
        break;
      case 'ArrowLeft':
      case 'ArrowUp':
        nextIndex = (currentIndex - 1 + enabledKeys.length) % enabledKeys.length;
        break;
      case 'Home':
        nextIndex = 0;
        break;
      case 'End':
        nextIndex = enabledKeys.length - 1;
        break;
      default:
        return; // 非方向键不拦截，交给页面原生行为
    }

    event.preventDefault();
    const nextKey = enabledKeys[nextIndex];
    // 焦点移动交给浏览器默认行为：先移焦点到目标 tab 再触发激活
    const nextTab = (event.currentTarget as HTMLElement).querySelector(
      `[role="tab"][data-key="${CSS.escape(nextKey)}"]`
    ) as HTMLElement | null;
    nextTab?.focus();
    onChange(nextKey);
  }

  return (
    <div
      className={cn('ui-tabs', className)}
      role="tablist"
      aria-label={ariaLabel}
      onKeyDown={handleKeyDown}
    >
      {items.map((item) => (
        <button
          key={item.key}
          type="button"
          role="tab"
          // WAI-ARIA roving tabindex：只有激活项在 Tab 序列中
          tabIndex={item.key === active ? 0 : -1}
          data-key={item.key}
          aria-selected={item.key === active}
          aria-disabled={item.disabled || undefined}
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
