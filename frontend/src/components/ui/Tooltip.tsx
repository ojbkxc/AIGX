import React, { useId, useState } from 'react';
import { cn } from '../../lib/utils';

export interface TooltipProps {
  /** 悬停目标 */
  children: React.ReactElement;
  /** 提示内容 */
  label: React.ReactNode;
  /** 显示位置，默认 top */
  placement?: 'top' | 'bottom' | 'left' | 'right';
  /** 延迟显示（毫秒），默认 300 */
  delay?: number;
  className?: string;
}

/**
 * Tooltip — 轻量悬停提示（玻璃拟态）。
 * 不依赖第三方库：CSS 定位 + 延迟显示；focus 也可触发（可访问性）。
 */
export default function Tooltip({
  children,
  label,
  placement = 'top',
  delay = 300,
  className
}: TooltipProps) {
  const [visible, setVisible] = useState(false);
  const timer = React.useRef<ReturnType<typeof setTimeout> | null>(null);
  const tipId = useId();

  const show = (): void => {
    timer.current = setTimeout(() => setVisible(true), delay);
  };
  const hide = (): void => {
    if (timer.current) clearTimeout(timer.current);
    setVisible(false);
  };

  React.useEffect(() => () => {
    if (timer.current) clearTimeout(timer.current);
  }, []);

  const child = React.cloneElement(children as React.ReactElement<Record<string, unknown>>, {
    'aria-describedby': visible ? tipId : undefined,
    onMouseEnter: show,
    onMouseLeave: hide,
    onFocus: show,
    onBlur: hide,
  });

  return (
    <span className="glass-tooltip-wrapper" style={{ position: 'relative', display: 'inline-flex' }}>
      {child}
      {visible && (
        <span
          id={tipId}
          role="tooltip"
          className={cn('glass-tooltip', `glass-tooltip-${placement}`, className)}
        >
          {label}
        </span>
      )}
    </span>
  );
}
