import React from 'react';
import { cn } from '../../lib/utils';

export interface TextareaProps extends React.TextareaHTMLAttributes<HTMLTextAreaElement> {
  /** 表单标签（可选） */
  label?: string;
  /** 输入提示文本 */
  hint?: string;
  /** 校验错误信息 */
  error?: string;
}

/**
 * Textarea — 多行文本输入，复用 App.css 的 .form-input 样式体系。
 */
export default function Textarea({ label, hint, error, className, id, ...rest }: TextareaProps) {
  const areaId = id ?? (label ? 'textarea-' + label.replace(/\s+/g, '-') : undefined);
  return (
    <div className="form-group">
      {label && <label htmlFor={areaId}>{label}</label>}
      <textarea
        id={areaId}
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