import React from 'react';
import { cn } from '../../lib/utils';

export interface EmptyStateProps {
  message: string;
  icon?: string;
  action?: React.ReactNode;
  className?: string;
}

/**
 * EmptyState — 空状态占位，复用 App.css 的 empty-state 样式。
 */
export default function EmptyState({
  message,
  icon = '📭',
  action,
  className
}: EmptyStateProps) {
  return (
    <div className={cn('empty-state', className)}>
      <div className="empty-state-icon">{icon}</div>
      <p>{message}</p>
      {action}
    </div>
  );
}