import React from 'react';
import { cn } from '../../lib/utils';

export interface SectionCardProps {
  title?: string;
  actions?: React.ReactNode;
  children: React.ReactNode;
  className?: string;
  bodyClassName?: string;
}

/**
 * SectionCard — 分区卡片（header + body），复用 App.css 的 section-card 体系。
 * actions 渲染在标题行右侧，与现有页面布局一致。
 */
export default function SectionCard({
  title,
  actions,
  children,
  className,
  bodyClassName
}: SectionCardProps) {
  return (
    <section className={cn('section-card', className)}>
      {(title || actions) && (
        <div className="section-card-header">
          {title ? <h2>{title}</h2> : <span />}
          {actions && <div className="actions-cell">{actions}</div>}
        </div>
      )}
      <div className={cn('section-card-body', bodyClassName)}>{children}</div>
    </section>
  );
}