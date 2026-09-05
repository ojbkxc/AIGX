import React from 'react';
import { cn } from '../../lib/utils';

/**
 * EmptyState — 空状态占位，复用 App.css 的 empty-state 样式。
 */
export default function EmptyState({ message, icon = '📭', action, className }) {
  return (
    <div className={cn('empty-state', className)}>
      <div className="empty-state-icon">{icon}</div>
      <p>{message}</p>
      {action}
    </div>
  );
}
