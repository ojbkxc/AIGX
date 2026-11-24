import React, { useId } from 'react';
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
 *
 * id 口径：显式 `id` 优先；否则由 `useId()` 兜底，**不再从 label 文本派生**。
 * 旧做法（`'input-' + label`）在同一页出现两个同 label 字段时会产出重复 DOM id，
 * `htmlFor` 只认第一个，点第二个标签会聚焦到第一个输入框。
 */
export default function Input({ label, hint, error, className, id, ...rest }: InputProps) {
  const reactId = useId();
  const inputId = id ?? (label ? `input-${reactId}` : undefined);
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