import React from 'react';
import { cn } from '../../lib/utils';

export interface InputProps extends React.InputHTMLAttributes<HTMLInputElement> {
  /** 表单标签（可选，渲染为 form-group + label） */
  label?: string;
  /** 输入提示文本 */
  hint?: string;
  /** 校验错误信息（显示为红色提示） */
  error?: string;
}

/**
 * Input — 通用文本输入框，复用 App.css 的 .form-input 体系。
 * 与页面现有 <input className="form-input"> 完全兼容，标签/提示可选。
 */
export default function Input({ label, hint, error, className, id, ...rest }: InputProps) {
  const inputId = id ?? (label ? input- : undefined);
  return (
    <div className="form-group">
      {label && <label htmlFor={inputId}>{label}</label>}
      <input
        id={inputId}
        className={cn('form-input', error && 'form-input-error', className)}
        aria-invalid={error ? true : undefined}
        {...rest}
      />
      {error ? (
        <span className="form-hint" style={{ color: 'var(--danger-color)' }}>{error}</span>
      ) : hint ? (
        <span className="form-hint">{hint}</span>
      ) : null}
    </div>
  );
}