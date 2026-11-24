/**
 * 卡片行内的通用零件（对齐 Agent2API v3.5.1 `routing-canvas-controls.tsx` 的
 * 三个共用件：`RoutingActions` / `RoutingProviderMark` / `routingOrderKey`）。
 *
 * 为什么单独一个文件：A2A 把它们放在 canvas-controls 里，本仓的 canvas 层
 * 归另一位成员（`RoutingCanvas*`），卡片这边不能反向依赖画布文件 ——
 * 依赖方向只能是「卡片 → 通用件」，不能是「卡片 → 画布」。
 *
 * 差异（如实登记，非静默变形）：
 * - A2A 用 `Popover` + `Switch`（shadcn）；AIGX 组件库没有 Popover，
 *   这里用原生 `<details>` 做「更多」菜单，键盘/点击行为一致，样式走 `rt-*`。
 * - A2A 的提供商标记读 `useProviderIcon`（favicon 缓存）；AIGX 无该缓存，
 *   直接用首字母色块（A2A 同款 letter 兜底形态）。
 */
import type React from 'react';
import { useTranslation } from 'react-i18next';

export type RoutingAction = {
  label: string;
  icon?: React.ReactNode;
  disabled?: boolean;
  destructive?: boolean;
  onClick(): void;
};

/** 「⋯」更多操作菜单（A2A `RoutingActions`）。 */
export function RoutingActions({
  label,
  actions,
  disabled,
}: {
  label: string;
  actions: RoutingAction[];
  disabled?: boolean;
}): JSX.Element {
  return (
    <details className="rt-more">
      <summary
        role="button"
        aria-label={label}
        title={label}
        aria-disabled={disabled || undefined}
        tabIndex={disabled ? -1 : 0}
        onClick={(event) => {
          if (disabled) event.preventDefault();
        }}
      >
        ⋯
      </summary>
      <div className="rt-actions-menu" role="menu" aria-label={label}>
        {actions.map((action) => (
          <button
            key={action.label}
            type="button"
            role="menuitem"
            className={`rt-action-item${action.destructive ? ' is-destructive' : ''}`}
            disabled={action.disabled}
            onClick={(event) => {
              const host = event.currentTarget.closest('details');
              if (host) host.open = false;
              action.onClick();
            }}
          >
            {action.icon}
            {action.label}
          </button>
        ))}
      </div>
    </details>
  );
}

/** 提供商标记：首字母色块（A2A 在拿不到图标时的同一形态）。 */
export function RoutingProviderMark({ label }: { label: string }): JSX.Element {
  return (
    <span className="rt-provider-mark" aria-hidden="true">
      {(label || '?').slice(0, 1).toUpperCase()}
    </span>
  );
}

/** Alt + ↑/↓ 调整顺序（A2A `routingOrderKey`，卡片把手上的键盘通路）。 */
export function routingOrderKey(event: React.KeyboardEvent, move: (direction: number) => void): void {
  if (event.altKey && (event.key === 'ArrowUp' || event.key === 'ArrowDown')) {
    event.preventDefault();
    move(event.key === 'ArrowUp' ? -1 : 1);
  }
}

/** 拖拽把手的可访问标签（两处卡片共用同一文案，避免两套说法）。 */
export function DragHandle({
  disabled,
  ariaLabel,
  hint,
  onPointerDown,
  onKeyDown,
}: {
  disabled: boolean;
  ariaLabel: string;
  hint?: string;
  onPointerDown?: (event: React.PointerEvent) => void;
  onKeyDown?: (event: React.KeyboardEvent) => void;
}): JSX.Element {
  const { t } = useTranslation();
  return (
    <button
      type="button"
      className="rt-grip"
      disabled={disabled}
      title={hint ?? t('拖动或按 Alt + 方向键调整顺序')}
      aria-label={ariaLabel}
      onPointerDown={onPointerDown}
      onKeyDown={onKeyDown}
    >
      ⠿
    </button>
  );
}