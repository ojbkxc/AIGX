import { cn } from '../../lib/utils';

export interface LoadingProps {
  text?: string;
  className?: string;
}

/**
 * Loading — 加载指示，复用 App.css 的 .loading（自带旋转环）。
 */
export default function Loading({ text, className }: LoadingProps) {
  return <div className={cn('loading', className)}>{text || ''}</div>;
}