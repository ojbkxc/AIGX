import { forwardRef, InputHTMLAttributes } from 'react';
import { cn } from '../../lib/utils';

export interface GlassInputProps extends Omit<InputHTMLAttributes<HTMLInputElement>, 'className'> {
  className?: string;
  error?: boolean;
  children?: never;
}

/**
 * GlassInput — 玻璃拟态输入框，复用 App.css 的 .glass-input 样式。
 * 支持 error 态与 ref 转发，供 react-hook-form 等表单库直接注册。
 */
const GlassInput = forwardRef<HTMLInputElement, GlassInputProps>(
  function GlassInput({ className, error = false, ...rest }, ref) {
    return (
      <input
        ref={ref}
        aria-invalid={error || undefined}
        className={cn('glass-input', error && 'border-red-500/60', className)}
        {...rest}
      />
    );
  }
);

export default GlassInput;