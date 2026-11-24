/**
 * 路由下拉控件（对齐 Agent2API v3.5.1 `routing-select.tsx` 的调用口径）。
 *
 * A2A 用 shadcn `Select` + 可选 `SearchableSelect`；AIGX 的组件库里只有
 * 原生 `select`（`.form-input` 体系），故这里落成同一个 props 契约
 * （`options: Array<[value, label]>`），调用点照 A2A 写法不变。
 *
 * `searchPlaceholder` 保留在类型里：A2A 用它切换成「可搜索下拉」，AIGX
 * 侧暂无该控件，传了也不会静默变形（渲染同一个原生下拉），避免调用点
 * 以为拿到了搜索能力。
 */
import { useId } from 'react';
import { cn } from '../../lib/utils';

export function RoutingSelect({ value, options, label, disabled, onChange, className }: {
  value: string;
  options: Array<[string, string]>;
  label: string;
  disabled?: boolean;
  onChange(value: string): void;
  /** A2A 的可搜索下拉开关；AIGX 暂无对应控件，仅作契约占位 */
  searchPlaceholder?: string;
  className?: string;
}): JSX.Element {
  const id = useId();
  const text = options.find((option) => option[0] === value)?.[1] || value;
  return (
    <select
      id={id}
      className={cn('form-input', className)}
      value={value}
      disabled={disabled}
      aria-label={label}
      title={text}
      onChange={(event) => onChange(event.target.value)}
    >
      {options.map(([key, optionLabel]) => (
        <option key={key} value={key}>
          {optionLabel}
        </option>
      ))}
    </select>
  );
}