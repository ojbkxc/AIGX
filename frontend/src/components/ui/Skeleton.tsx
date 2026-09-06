import React from 'react';
import { cn } from '../../lib/utils';

export interface SkeletonProps {
  /** 骨架块宽度（CSS 值），默认 100% */
  width?: string | number;
  /** 骨架块高度（CSS 值），默认 14px */
  height?: string | number;
  /** 圆角，默认 6px */
  radius?: string;
  /** 渲染为圆形（头像/图标位） */
  circle?: boolean;
  className?: string;
  style?: React.CSSProperties;
}

/**
 * Skeleton — 骨架屏原子块（玻璃拟态微光扫过）。
 * 组合使用：<Skeleton height={28} /> 等一行，多行成块；
 * 列表场景直接用 SkeletonList / SkeletonTable。
 */
export default function Skeleton({
  width = '100%',
  height = 14,
  radius = '6px',
  circle = false,
  className,
  style
}: SkeletonProps) {
  const size = circle
    ? { width: typeof width === 'number' ? `${width}px` : width, aspectRatio: '1' }
    : { width: typeof width === 'number' ? `${width}px` : width };
  return (
    <span
      aria-hidden="true"
      className={cn('skeleton', circle && 'skeleton-circle', className)}
      style={{ ...size, height: circle ? undefined : (typeof height === 'number' ? `${height}px` : height), borderRadius: circle ? '50%' : radius, ...style }}
    />
  );
}

export interface SkeletonListProps {
  /** 行数，默认 5 */
  rows?: number;
  className?: string;
}

/** SkeletonList — 列表骨架（图标 + 双行文本） */
export function SkeletonList({ rows = 5, className }: SkeletonListProps) {
  return (
    <div className={cn('skeleton-list', className)} role="status" aria-busy="true">
      {Array.from({ length: rows }, (_, i) => (
        <div key={i} className="skeleton-row">
          <Skeleton circle width={34} />
          <div className="skeleton-col">
            <Skeleton height={12} width="55%" />
            <Skeleton height={10} width="35%" />
          </div>
          <Skeleton height={22} width={64} radius="20px" />
        </div>
      ))}
    </div>
  );
}

export interface SkeletonTableProps {
  /** 列数，默认 5 */
  columns?: number;
  /** 行数，默认 6 */
  rows?: number;
  className?: string;
}

/** SkeletonTable — 表格骨架（表头 + 网格行） */
export function SkeletonTable({ columns = 5, rows = 6, className }: SkeletonTableProps) {
  return (
    <div
      className={cn('skeleton-table', className)}
      role="status"
      aria-busy="true"
      style={{ '--skel-cols': columns } as React.CSSProperties}
    >
      <div className="skeleton-thead">
        {Array.from({ length: columns }, (_, i) => (
          <Skeleton key={i} height={11} width={i === columns - 1 ? '60%' : '85%'} />
        ))}
      </div>
      {Array.from({ length: rows }, (_, r) => (
        <div key={r} className="skeleton-tr">
          {Array.from({ length: columns }, (_, c) => (
            <Skeleton key={c} height={c === 0 ? 12 : 14} width={c === columns - 1 ? '40%' : `${65 + ((r * 13 + c * 29) % 30)}%`} />
          ))}
        </div>
      ))}
    </div>
  );
}

export interface SkeletonCardsProps {
  /** 卡片数，默认 4 */
  count?: number;
  className?: string;
}

/** SkeletonCards — 统计卡片骨架（Dashboard 顶栏） */
export function SkeletonCards({ count = 4, className }: SkeletonCardsProps) {
  return (
    <div className={cn('skeleton-cards', className)} role="status" aria-busy="true">
      {Array.from({ length: count }, (_, i) => (
        <div key={i} className="skeleton-card">
          <Skeleton height={10} width="40%" />
          <Skeleton height={26} width="65%" style={{ margin: '8px 0' }} />
          <Skeleton height={9} width="50%" />
        </div>
      ))}
    </div>
  );
}
