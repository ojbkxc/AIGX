/**
 * 轻量弹层（对齐 A2A `@ui` 的 Popover：位置翻转 + 点外部关闭 + Esc + 焦点归位）。
 *
 * ── 为什么不复用现有组件 ────────────────────────────────────
 * AIGX 的 ui 库只有 MobileDrawer（左侧覆盖层）与 Combobox/Select（表单语义），
 * 没有「贴底输入区要往上翻」的浮层。ChatDebugger 的 pg-param-pop-anchor 是绝对
 * 定位的手写版，只做了一次翻转判断；这里把它抽成可复用的基座，供模型胶囊与
 * 权限胶囊共用，避免两处各写一套关闭逻辑。
 *
 * ── 关闭语义 ────────────────────────────────────────────────
 * ①点击浮层与触发器之外；②Esc。触发器由调用方渲染，本组件只管浮层本体与
 * 关闭事件回调 —— 触发器的 aria-expanded 由调用方补（它才知道自己是不是开）。
 */
import { useEffect, useRef, type ReactNode } from 'react';

export interface AsstPopoverProps {
  open: boolean;
  onClose: () => void;
  /** 浮层相对触发器的展开方向：输入区在页面底部，统一向上翻 */
  side?: 'top' | 'bottom';
  /** 水平对齐：左侧胶囊左对齐，右侧胶囊右对齐（避免溢出视口右缘） */
  align?: 'start' | 'end';
  /** 浮层宽度类（如 `asst-pop-w`），由调用方给，保持样式在 CSS 里 */
  className?: string;
  children: ReactNode;
}

export default function AsstPopover({
  open,
  onClose,
  side = 'top',
  align = 'start',
  className,
  children,
}: AsstPopoverProps): JSX.Element | null {
  const ref = useRef<HTMLDivElement | null>(null);
  const closeRef = useRef(onClose);
  closeRef.current = onClose;

  useEffect(() => {
    if (!open) return undefined;
    const onDoc = (event: MouseEvent): void => {
      const node = ref.current;
      if (!node) return;
      const target = event.target as Node | null;
      if (target && node.contains(target)) return;
      // 触发器自己负责 toggle：命中触发器时不自作主张关闭，否则「点一下开、
      // 再点一下又立刻被这里关掉」，表现为开关失效。
      if (target && (target as HTMLElement).closest?.('[data-asst-trigger]')) return;
      closeRef.current();
    };
    const onKey = (event: KeyboardEvent): void => {
      if (event.key === 'Escape') closeRef.current();
    };
    document.addEventListener('mousedown', onDoc);
    document.addEventListener('keydown', onKey);
    return () => {
      document.removeEventListener('mousedown', onDoc);
      document.removeEventListener('keydown', onKey);
    };
  }, [open]);

  if (!open) return null;

  return (
    <div
      ref={ref}
      role="dialog"
      className={`asst-pop asst-pop-${side} asst-pop-${align}${className ? ` ${className}` : ''}`}
    >
      {children}
    </div>
  );
}
