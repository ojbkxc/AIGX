import React, { useEffect, useRef } from 'react';
import { cn } from '../../lib/utils';

export interface MobileDrawerProps {
  open: boolean;
  onClose: () => void;
  children: React.ReactNode;
  /** 抽屉宽度（CSS 值），默认 216px 与 --sidebar-width 一致 */
  width?: string;
  /** 可访问性标签 */
  ariaLabel?: string;
  className?: string;
}

/**
 * MobileDrawer — 移动端抽屉容器（玻璃拟态）。
 * 仅在 open 时渲染；遮罩点击/Esc 关闭；背景滚动锁定。
 * 桌面端由 Sidebar 常驻，此组件服务于 ≤768px 的汉堡菜单场景。
 *
 * 焦点管理（对齐 ConfirmDialog 的弹窗三件套）：
 * 打开时记录原焦点并移入抽屉（首个可聚焦元素），Tab 在抽屉内循环，
 * 关闭后还原原焦点。
 */
export default function MobileDrawer({
  open,
  onClose,
  children,
  width,
  ariaLabel,
  className
}: MobileDrawerProps) {
  const asideRef = useRef<HTMLElement>(null);
  /** 打开前的焦点元素：关闭时还原，避免键盘用户「丢失位置」 */
  const restoreFocusRef = useRef<HTMLElement | null>(null);

  useEffect(() => {
    if (!open) return undefined;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        onClose();
        return;
      }
      // 焦点陷阱：Tab 在抽屉内循环，避免焦点跑到背景页
      if (e.key === 'Tab' && asideRef.current) {
        const focusable = asideRef.current.querySelectorAll<HTMLElement>(
          'button:not([disabled]), input:not([disabled]), a[href], [tabindex]:not([tabindex="-1"])',
        );
        if (focusable.length === 0) return;
        const first = focusable[0];
        const last = focusable[focusable.length - 1];
        if (e.shiftKey && document.activeElement === first) {
          e.preventDefault();
          last.focus();
        } else if (!e.shiftKey && document.activeElement === last) {
          e.preventDefault();
          first.focus();
        }
      }
    };
    window.addEventListener('keydown', onKey);
    const prev = document.body.style.overflow;
    document.body.style.overflow = 'hidden';
    // 记录打开前焦点（关闭时还原）；逐帧把焦点移入抽屉
    restoreFocusRef.current =
      document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const raf = requestAnimationFrame(() => {
      const focusable = asideRef.current?.querySelector<HTMLElement>(
        'button:not([disabled]), input:not([disabled]), a[href], [tabindex]:not([tabindex="-1"])',
      );
      (focusable ?? asideRef.current)?.focus();
    });
    return () => {
      cancelAnimationFrame(raf);
      window.removeEventListener('keydown', onKey);
      document.body.style.overflow = prev;
      // 关闭时还原焦点
      restoreFocusRef.current?.focus?.();
      restoreFocusRef.current = null;
    };
  }, [open, onClose]);

  if (!open) return null;

  return (
    <div className="mobile-drawer-overlay" onClick={onClose} aria-hidden="true">
      <aside
        ref={asideRef}
        role="dialog"
        aria-modal="true"
        aria-label={ariaLabel}
        className={cn('mobile-drawer', className)}
        style={width ? { width } : undefined}
        onClick={(e) => e.stopPropagation()}
      >
        {children}
      </aside>
    </div>
  );
}
