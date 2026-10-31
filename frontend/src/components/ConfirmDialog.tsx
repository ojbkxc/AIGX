import React, { useEffect, useRef, useState } from 'react';
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
  // Escape 处理读 ref 而非闭包里的 confirming：keydown 监听器只在 state
  // 变化时重建，闭包捕获的 confirming 恒为 false，导致「确认进行中按 Esc
  // 仍会关闭弹窗、请求却继续跑」。
  const confirmingRef = useRef(false);
  confirmingRef.current = confirming;
  const dialogRef = useRef<HTMLDivElement>(null);
  /** 打开前的焦点元素：关闭时还原，避免键盘用户"丢失位置" */
  const restoreFocusRef = useRef<HTMLElement | null>(null);

  // 打开时逐帧触发入场动画；Escape 关闭（键盘可达性，P0 验收）
  useEffect(() => {
    if (state) {
      setConfirming(false);
      setPassword('');
      setPasswordError('');
      // 记录当前焦点，关闭后还原
      restoreFocusRef.current =
        document.activeElement instanceof HTMLElement ? document.activeElement : null;
      const raf = requestAnimationFrame(() => {
        // 无密码框时把焦点移到取消按钮（回车不会误触危险操作）；
        // 有密码框时 input 自己的 autoFocus 负责
        if (!state.requirePassword) {
          const cancel = dialogRef.current?.querySelector<HTMLElement>('[data-confirm-cancel]');
          cancel?.focus();
        }
      });
      const onKey = (e: KeyboardEvent): void => {
        if (e.key === 'Escape' && !confirmingRef.current) {
          onClose?.();
          return;
        }
        // 焦点陷阱：Tab 在弹窗内循环，避免焦点跑到背景页
        if (e.key === 'Tab' && dialogRef.current) {
          const focusable = dialogRef.current.querySelectorAll<HTMLElement>(
            'button:not([disabled]), input:not([disabled]), [href], [tabindex]:not([tabindex="-1"])',
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
      document.addEventListener('keydown', onKey);
      return () => {
        cancelAnimationFrame(raf);
        document.removeEventListener('keydown', onKey);
        // 关闭时还原焦点
        restoreFocusRef.current?.focus?.();
        restoreFocusRef.current = null;
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
        onClose?.();
      } catch (err) {
        // 确认失败**不能静默关闭**：保留弹窗并把后端错误显示出来，
        // 用户才能改正（如密码错误）或重试。原来的 finally 无条件关闭
        // 会让失败看起来像成功。
        setPasswordError(err instanceof Error ? err.message : String(err));
      } finally {
        setConfirming(false);
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
      aria-labelledby="confirm-dialog-title"
    >
      <div
        ref={dialogRef}
        className="modal"
        style={{ maxWidth: 420, width: '90%' }}
        onClick={(e) => e.stopPropagation()}
      >
        <div className="modal-header">
          <h3 id="confirm-dialog-title">{titleLabel}</h3>
          <button className="modal-close" onClick={handleClose} aria-label={t('关闭')}>
            &times;
          </button>
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
          <button
            className="btn btn-outline"
            data-confirm-cancel
            onClick={handleClose}
            disabled={confirming}
          >
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
