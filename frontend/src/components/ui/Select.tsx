import React from 'react';
import { cn } from '../../lib/utils';

export interface SelectProps extends React.SelectHTMLAttributes<HTMLSelectElement> {
  /** 表单标签（可选） */
  label?: string;
  /** 输入提示文本 */
  hint?: string;
  /** 校验错误信息 */
  error?: string;
}

/**
 * Select — 下拉选择框，复用 App.css 的 .form-input 样式体系。
 */
export default function Select({ label, hint, error, className, id, children, ...rest }: SelectProps) {
  const selectId = id ?? (label ? select- : undefined);
  return (
    <div className="form-group">
      {label && <label htmlFor={selectId}>{label}</label>}
      <select
        id={selectId}
        className={cn('form-input', error && 'form-input-error', className)}
        aria-invalid={error ? true : undefined}
        {...rest}
      >
        {children}
      </select>
      {error ? (
        <span className="form-hint" style={{ color: 'var(--danger-color)' }}>{error}</span>
      ) : hint ? (
        <span className="form-hint">{hint}</span>
      ) : null}
    </div>
  );
}