import React, { useEffect } from 'react';
import { cn } from '../../lib/utils';

export interface GlassDialogProps {
  open: boolean;
  title?: string;
  onClose?: () => void;
  children: React.ReactNode;
  footer?: React.ReactNode;
  className?: string;
  maxWidth?: string;
}

/**
 * GlassDialog — 玻璃拟态弹窗，复用 App.css 的 modal-* 体系。
 * 用法与 ConfirmDialog 一致：open 控制显隐，onClose 关闭。
 */
export default function GlassDialog({
  open,
  title,
  onClose,
  children,
  footer,
  className,
  maxWidth
}: GlassDialogProps) {
  useEffect(() => {
    if (!open) return undefined;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose?.();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [open, onClose]);

  if (!open) return null;

  return (
    <div className="modal-overlay" onClick={() => onClose?.()} role="dialog" aria-modal="true">
      <div
        className={cn('modal', className)}
        style={maxWidth ? { maxWidth } : undefined}
        onClick={(e) => e.stopPropagation()}
      >
        {(title || onClose) && (
          <div className="modal-header">
            <h3>{title}</h3>
            {onClose && (
              <button className="modal-close" onClick={() => onClose?.()}>&times;</button>
            )}
          </div>
        )}
        <div className="modal-body">{children}</div>
        {footer && <div className="modal-footer">{footer}</div>}
      </div>
    </div>
  );
}