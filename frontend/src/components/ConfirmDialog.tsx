import React, { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Loader2 } from 'lucide-react';

export interface ConfirmState {
  message: React.ReactNode;
  title?: React.ReactNode;
  confirmText?: string;
  danger?: boolean;
  /** 敏感操作二次认证：渲染密码输入框，值传给 onConfirm(password) */
  requirePassword?: boolean;
  onConfirm?: (password?: string) => void | Promise<void>;
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
  const [password, setPassword] = useState('');
  const [passwordError, setPasswordError] = useState('');

  // 打开时逐帧触发入场动画；Escape 关闭（键盘可达性，P0 验收）
  useEffect(() => {
    if (state) {
      setConfirming(false);
      setPassword('');
      setPasswordError('');
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

  const { message, title, confirmText, danger, requirePassword, onConfirm } = state;
  const confirmLabel = confirmText || t('确定');
  const titleLabel = title || t('确认');

  const handleClose = (): void => {
    if (confirming) return;
    onClose?.();
  };

  const handleConfirm = async (): Promise<void> => {
    if (confirming) return;
    if (requirePassword && !password.trim()) {
      setPasswordError(t('请输入登录密码'));
      return;
    }
    const result = requirePassword ? onConfirm?.(password) : onConfirm?.();
    // 异步确认：loading 中防重复点击/关闭，完成后再关弹窗
    if (result instanceof Promise) {
      setConfirming(true);
      setPasswordError('');
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
          {requirePassword && (
            <div className="form-group" style={{ marginTop: 12, marginBottom: 0 }}>
              <label>{t('登录密码确认')}</label>
              <input
                className="form-input"
                type="password"
                autoFocus
                autoComplete="current-password"
                value={password}
                disabled={confirming}
                placeholder={t('请输入当前登录密码')}
                onChange={(e) => { setPassword(e.target.value); setPasswordError(''); }}
                onKeyDown={(e) => {
                  if (e.key === 'Enter') { e.preventDefault(); void handleConfirm(); }
                }}
              />
              {passwordError && (
                <span className="form-hint" style={{ color: 'var(--danger-color)' }}>{passwordError}</span>
              )}
              <span className="form-hint" style={{ marginTop: 6 }}>{t('敏感操作需验证身份，密码不会存储')}</span>
            </div>
          )}
        </div>
        <div className="modal-footer">
          <button className="btn btn-outline" onClick={handleClose} disabled={confirming}>
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
