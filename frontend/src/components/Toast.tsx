import React, { useState, useEffect, useCallback, createContext, useContext } from 'react';

export type ToastType = 'success' | 'error' | 'warning';

interface ToastItemState {
  id: number;
  message: string;
  type: ToastType;
  duration: number;
}

type AddToast = (message: string, type?: ToastType, duration?: number) => void;

const ToastContext = createContext<AddToast | null>(null);

export function useToast(): AddToast {
  const ctx = useContext(ToastContext);
  if (!ctx) {
    // 未包 ToastProvider 时降级为 console 输出，避免崩溃
    return (message: string, type: ToastType = 'success') => {
      console.log(`[toast:${type}]`, message);
    };
  }
  return ctx;
}

let toastId = 0;

export interface ToastProviderProps {
  children?: React.ReactNode;
}

export function ToastProvider({ children }: ToastProviderProps): JSX.Element {
  const [toasts, setToasts] = useState<ToastItemState[]>([]);

  const addToast = useCallback<AddToast>((message, type = 'success', duration = 3000) => {
    const id = ++toastId;
    // 同类型提示去重 + 最多保留 5 条：循环里连续 addToast（如批量删除
    // 每项一条）会堆满屏幕，旧条又不消失
    setToasts((prev) => {
      const deduped = prev.filter((t) => t.message !== message || t.type !== type);
      return [...deduped, { id, message, type, duration }].slice(-5);
    });
  }, []);

  const removeToast = useCallback((id: number) => {
    setToasts((prev) => prev.filter((t) => t.id !== id));
  }, []);

  return (
    <ToastContext.Provider value={addToast}>
      {children}
      <div className="toast-container">
        {toasts.map((toast) => (
          <ToastItem key={toast.id} toast={toast} onRemove={removeToast} />
        ))}
      </div>
    </ToastContext.Provider>
  );
}

interface ToastItemProps {
  toast: ToastItemState;
  onRemove: (id: number) => void;
}

function ToastItem({ toast, onRemove }: ToastItemProps): JSX.Element {
  const [show, setShow] = useState(false);

  useEffect(() => {
    requestAnimationFrame(() => setShow(true));
    const timer = setTimeout(() => {
      setShow(false);
      setTimeout(() => onRemove(toast.id), 300);
    }, toast.duration);
    return () => clearTimeout(timer);
  }, [toast, onRemove]);

  return (
    <div className={`toast toast-${toast.type} ${show ? 'show' : ''}`}>
      <span className="toast-icon">
        {toast.type === 'success' ? '✓' : toast.type === 'error' ? '✕' : '⚠'}
      </span>
      {toast.message}
    </div>
  );
}