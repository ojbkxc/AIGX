import React, { useState } from 'react';
import { Eye, EyeOff } from 'lucide-react';
import { cn } from '../../lib/utils';

export interface PasswordInputProps
  extends Omit<React.InputHTMLAttributes<HTMLInputElement>, 'type'> {
  /** 表单标签（可选，渲染为 form-group + label） */
  label?: string;
  /** 输入提示文本 */
  hint?: string;
  /** 校验错误信息（显示为红色提示） */
  error?: string;
}

/**
 * PasswordInput — 带明文切换的密码输入框。
 * 右侧眼睛按钮切换 type=password/text；复用 .form-input 体系，
 * 登录/注册/改密等场景防「输错一次全部重来」。
 */
export default function PasswordInput({
  label,
  hint,
  error,
  className,
  id,
  ...rest
}: PasswordInputProps): JSX.Element {
  const [visible, setVisible] = useState(false);
  const inputId = id ?? (label ? 'pwd-' + label.replace(/\s+/g, '-') : undefined);
  return (
    <div className="form-group">
      {label && <label htmlFor={inputId}>{label}</label>}
      <div className="password-input-wrap">
        <input
          id={inputId}
          type={visible ? 'text' : 'password'}
          className={cn('form-input password-input', error && 'form-input-error', className)}
          aria-invalid={error ? true : undefined}
          {...rest}
        />
        <button
          type="button"
          className="password-input-toggle"
          tabIndex={-1}
          onClick={() => setVisible((v) => !v)}
          aria-label={visible ? '隐藏密码' : '显示密码'}
          title={visible ? '隐藏密码' : '显示密码'}
        >
          {visible ? <EyeOff size={14} /> : <Eye size={14} />}
        </button>
      </div>
      {error ? (
        <span className="form-hint" style={{ color: 'var(--danger-color)' }}>{error}</span>
      ) : hint ? (
        <span className="form-hint">{hint}</span>
      ) : null}
    </div>
  );
}
