import React from 'react';
import { cn } from '../../lib/utils';

export interface CardProps {
  children: React.ReactNode;
  title?: React.ReactNode;
  /** 标题行右侧操作区 */
  actions?: React.ReactNode;
  className?: string;
  headerClassName?: string;
  bodyClassName?: string;
}

/**
 * Card — 基础卡片容器，桥接 App.css 的 .card/.card-header/.card-body 体系。
 * 不带标题时直接渲染 children。
 */
export default function Card({
  children,
  title,
  actions,
  className,
  headerClassName,
  bodyClassName
}: CardProps) {
  return (
    <div className={cn('card', className)}>
      {(title || actions) && (
        <div className={cn('card-header', headerClassName)}>
          <h2>{title}</h2>
          {actions}
        </div>
      )}
      <div className={cn('card-body', bodyClassName)}>{children}</div>
    </div>
  );
}