import React from 'react';
import { cn } from '../../lib/utils';

export interface BadgeProps {
  children: React.ReactNode;
  tone?: 'success' | 'warning' | 'danger' | 'neutral';
  className?: string;
}

const tones = {
  success: 'badge badge-success',
  warning: 'badge badge-warning',
  danger: 'badge badge-danger',
  neutral: 'badge badge-neutral',
};

/**
 * Badge — 状态徽章封装，tone 对应 App.css 的 badge-* 色板。
 */
export default function Badge({ children, tone = 'neutral', className, ...rest }: BadgeProps) {
  return (
    <span className={cn(tones[tone] || tones.neutral, className)} {...rest}>
      {children}
    </span>
  );
}