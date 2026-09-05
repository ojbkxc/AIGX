import { cn } from '../../lib/utils';

export interface StatCardProps {
  title: string;
  value: string | number;
  desc?: string;
  icon?: string;
  className?: string;
  valueClassName?: string;
}

/**
 * StatCard — 统计卡片，复用 App.css 的 stat-card 体系。
 * icon 放 stat-icon-badge，desc 放辅助说明。
 */
export default function StatCard({
  title,
  value,
  desc,
  icon,
  className,
  valueClassName
}: StatCardProps) {
  return (
    <div className={cn('stat-card', className)}>
      {icon && <div className="stat-icon-badge">{icon}</div>}
      <div className="stat-title">{title}</div>
      <div className={cn('stat-value', valueClassName)}>{value}</div>
      {desc && <div className="stat-desc">{desc}</div>}
    </div>
  );
}