import React from 'react';
import { Inbox } from 'lucide-react';
import { cn } from '../../lib/utils';

export interface EmptyStateProps {
  message: string;
  /** 自定义图标：传 ReactNode（推荐 lucide 图标）；为兼容历史调用方，string（emoji）也接受 */
  icon?: React.ReactNode;
  action?: React.ReactNode;
  className?: string;
}

/**
 * EmptyState — 空状态占位，复用 App.css 的 empty-state 样式。
 * 默认图标为 lucide 的 Inbox（替代旧 emoji 📭）；调用方若仍传字符串 emoji 也能渲染，
 * 但新代码请直接传 <LucideIcon size={32} /> 之类的节点。
 */
export default function EmptyState({
  message,
  icon,
  action,
  className
}: EmptyStateProps) {
  const iconNode: React.ReactNode = icon ?? <Inbox size={32} strokeWidth={1.6} aria-hidden="true" />;
  return (
    <div className={cn('empty-state', className)}>
      <div className="empty-state-icon">{iconNode}</div>
      <p>{message}</p>
      {action}
    </div>
  );
}