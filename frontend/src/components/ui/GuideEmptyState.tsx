import React from 'react';
import { useTranslation } from 'react-i18next';
import { cn } from '../../lib/utils';

export interface GuideEmptyStateProps {
  /** 主标题（已翻译或用 i18n key） */
  title: string;
  /** 引导说明文案 */
  hint?: string;
  /** 图标 emoji */
  icon?: string;
  /** 主行动（如「创建渠道」按钮） */
  action?: React.ReactNode;
  /** 补充说明行（如前往文档链接） */
  footer?: React.ReactNode;
  className?: string;
}

/**
 * GuideEmptyState — 新手引导型空状态。
 * 与 EmptyState 分工：EmptyState 是轻量占位；本组件面向首次使用场景，
 * 提供 标题 + 引导文案 + 行动按钮 + 补充链接 的完整引导结构。
 */
export default function GuideEmptyState({
  title,
  hint,
  icon = '🧭',
  action,
  footer,
  className
}: GuideEmptyStateProps) {
  const { t } = useTranslation();
  return (
    <div className={cn('guide-empty', className)}>
      <div className="guide-empty-icon" aria-hidden="true">{icon}</div>
      <h3 className="guide-empty-title">{title}</h3>
      {hint && <p className="guide-empty-hint">{hint}</p>}
      {action && <div className="guide-empty-action">{action}</div>}
      {footer && <div className="guide-empty-footer">{footer}</div>}
      <span className="sr-only">{t('暂无数据')}</span>
    </div>
  );
}
