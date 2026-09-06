import React, { useState, useEffect } from 'react';
import { useTranslation } from 'react-i18next';

export interface ConfirmState {
  message: React.ReactNode;
  title?: React.ReactNode;
  confirmText?: string;
  danger?: boolean;
  onConfirm?: () => void | Promise<void>;
}

export interface ConfirmDialogProps {
  state: ConfirmState | null;
  onClose?: () => void;
}

/**
 * ConfirmDialog — 通用确认弹窗，替换原生 window.confirm。
 * 用法：
 *   const [confirmState, setConfirmState] = useState<ConfirmState | null>(null);
 *   // 触发：setConfirmState({ message, title, confirmText, onConfirm })
 *   // 渲染：<ConfirmDialog state={confirmState} onClose={() => setConfirmState(null)} />
 *
 * 样式复用 App.css 的 modal-* 类，保持玻璃拟态风格。
 * 原生 confirm 在 iframe / Electron 容器中可能静默失败，统一改用组件弹窗。
 */
export default function ConfirmDialog({ state, onClose }: ConfirmDialogProps): JSX.Element | null {
  const { t } = useTranslation();
  const [show, setShow] = useState(false);

  // 打开时逐帧触发入场动画
  useEffect(() => {
    if (state) {
      setShow(false);
      const raf = requestAnimationFrame(() => setShow(true));
      return () => cancelAnimationFrame(raf);
    }
    setShow(false);
  }, [state]);

  if (!state) return null;

  const { message, title, confirmText, danger, onConfirm } = state;
  const confirmLabel = confirmText || t('确定');
  const titleLabel = title || t('确认');

  const handleConfirm = () => {
    onClose?.();
    onConfirm?.();
  };

  return (
    <div
      className="modal-overlay"
      onClick={() => onClose?.()}
      role="dialog"
      aria-modal="true"
    >
      <div
        className="modal"
        style={{ maxWidth: 420, width: '90%' }}
        onClick={(e) => e.stopPropagation()}
      >
        <div className="modal-header">
          <h3>{titleLabel}</h3>
          <button className="modal-close" onClick={() => onClose?.()}>&times;</button>
        </div>
        <div className="modal-body" style={{ fontSize: 13.5, lineHeight: 1.6, color: 'var(--text-main)' }}>
          {message}
        </div>
        <div className="modal-footer">
          <button className="btn btn-outline" onClick={() => onClose?.()}>{t('取消')}</button>
          <button
            className={danger ? 'btn btn-danger' : 'btn btn-primary'}
            onClick={handleConfirm}
            autoFocus
          >
            {confirmLabel}
          </button>
        </div>
      </div>
    </div>
  );
}