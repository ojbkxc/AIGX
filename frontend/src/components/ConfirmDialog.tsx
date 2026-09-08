import React, { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Loader2 } from 'lucide-react';

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
 * onConfirm 返回 Promise 时按钮显示 loading 并防重复点击，完成后才关闭。
 * 取消按钮 autoFocus（回车不会误触危险操作）。
 * 样式复用 App.css 的 modal-* 类，保持玻璃拟态风格。
 * 原生 confirm 在 iframe / Electron 容器中可能静默失败，统一改用组件弹窗。
 */
export default function ConfirmDialog({ state, onClose }: ConfirmDialogProps): JSX.Element | null {
  const { t } = useTranslation();
  const [confirming, setConfirming] = useState(false);

  // 打开时逐帧触发入场动画；Escape 关闭（键盘可达性，P0 验收）
  useEffect(() => {
    if (state) {
      setConfirming(false);
      const raf = requestAnimationFrame(() => {
        // 弹窗显隐由 state 驱动；动画由 App.css 的 modal-* 样式处理
      });
      const onKey = (e: KeyboardEvent): void => {
        if (e.key === 'Escape' && !confirming) onClose?.();
      };
      document.addEventListener('keydown', onKey);
      return () => {
        cancelAnimationFrame(raf);
        document.removeEventListener('keydown', onKey);
      };
    }
    return undefined;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [state]);

  if (!state) return null;

  const { message, title, confirmText, danger, onConfirm } = state;
  const confirmLabel = confirmText || t('确定');
  const titleLabel = title || t('确认');

  const handleClose = (): void => {
    if (confirming) return;
    onClose?.();
  };

  const handleConfirm = async (): Promise<void> => {
    if (confirming) return;
    const result = onConfirm?.();
    // 异步确认：loading 中防重复点击/关闭，完成后再关弹窗
    if (result instanceof Promise) {
      setConfirming(true);
      try {
        await result;
      } finally {
        setConfirming(false);
        onClose?.();
      }
      return;
    }
    onClose?.();
  };

  return (
    <div
      className="modal-overlay"
      onClick={handleClose}
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
          <button className="modal-close" onClick={handleClose}>&times;</button>
        </div>
        <div className="modal-body" style={{ fontSize: 13.5, lineHeight: 1.6, color: 'var(--text-main)' }}>
          {message}
        </div>
        <div className="modal-footer">
          <button className="btn btn-outline" onClick={handleClose} disabled={confirming} autoFocus>
            {t('取消')}
          </button>
          <button
            className={danger ? 'btn btn-danger' : 'btn btn-primary'}
            onClick={() => void handleConfirm()}
            disabled={confirming}
          >
            {confirming && <Loader2 size={14} className="confirm-dialog-spin" />}
            {confirming ? t('处理中…') : confirmLabel}
          </button>
        </div>
      </div>
    </div>
  );
}
