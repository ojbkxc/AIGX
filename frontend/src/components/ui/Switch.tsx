import { cn } from '../../lib/utils';

export interface SwitchProps {
  checked: boolean;
  onChange: (next: boolean) => void;
  disabled?: boolean;
  /** 可访问性标签 */
  ariaLabel?: string;
  className?: string;
  id?: string;
}

/**
 * Switch — 开关控件（玻璃拟态）。
 * 纯受控组件：checked/onChange；键盘可达（Tab 聚焦、空格切换）。
 */
export default function Switch({
  checked,
  onChange,
  disabled = false,
  ariaLabel,
  className,
  id
}: SwitchProps) {
  return (
    <button
      type="button"
      id={id}
      role="switch"
      aria-checked={checked}
      aria-label={ariaLabel}
      aria-disabled={disabled || undefined}
      disabled={disabled}
      className={cn('glass-switch', checked && 'glass-switch-on', className)}
      onClick={() => onChange(!checked)}
    >
      <span className="glass-switch-thumb" />
    </button>
  );
}
