import React from 'react';
import { cn } from '../../lib/utils';

export interface GlassCardProps {
  children: React.ReactNode;
  className?: string;
  hover?: boolean;
  as?: keyof JSX.IntrinsicElements;
}

/**
 * GlassCard — 玻璃拟态容器，桥接 App.css 的 .glass-card 设计 token。
 * 渐进迁移用：旧页面无需改动，新代码可直接复用统一容器。
 */
export default function GlassCard({
  children,
  className,
  hover = false,
  as: Tag = 'div',
  ...rest
}: GlassCardProps) {
  return (
    <Tag
      className={cn('glass-card', hover && 'hover:scale-[1.02] transition-transform duration-300', className)}
      {...rest}
    >
      {children}
    </Tag>
  );
}