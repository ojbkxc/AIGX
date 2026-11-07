/**
 * AIGX · 表格列宽拖拽 hook（自定义 <table> 页面用）。
 *
 * 功能集（对齐 Agent2API ui/table-columns.js 列宽层，DOM 手法换 React）：
 * - pointerdown 拖拽表头右缘把手改列宽（pointer events，触屏可用）
 * - **双击把手还原**该列声明默认宽（删用户覆盖）
 * - 锁定列固定 px：不参与拖拽（fixedPxOf 语义）
 * - **最后一列不给把手**：钉在表格右缘会顶出横向滚动条（上游踩过的坑）
 * - 容器宽夹最小宽：拖拽时按容器宽把总宽夹住（limitOf 语义简化版——
 *   单列最小 60px；表格本身有 min-width 横滚，不需要逐列精确夹）
 *
 * 性能：pointermove 直接写 DOM（col 元素 style.width），不经过 React state
 * ——10k 行表格拖列宽零重渲染；pointerup 才 setState+落盘一次。
 */
import { useCallback, useRef, useState } from 'react';

/** col 元素类型（lib.dom 缺 HTMLColElement，用 ElementType 兜底） */
type ColElement = HTMLElement & { width?: string };

/** 单列最小宽 px（拖不出负列） */
const MIN_COL_PX = 60;

export interface ColumnWidthsHook {
  /** 当前宽度覆盖表（key → px；缺失 = 用声明默认） */
  widths: Record<string, number>;
  /** 拖拽/双击提交（widths 变更 + 落盘由页面做） */
  setWidths: (next: Record<string, number>) => void;
  /** 生效宽：覆盖 > 默认；锁定列恒默认（页面渲染 colgroup 用） */
  effectiveWidth: (key: string) => number | undefined;
  /** 表头把手事件绑定（onPointerDown / onDoubleClick）；末列传 null 不绑 */
  gripProps: (key: string, opts: { isLast: boolean; locked?: boolean; defaultWidth?: number }) => {
    onPointerDown?: (e: React.PointerEvent<HTMLElement>) => void;
    onDoubleClick?: (e: React.MouseEvent<HTMLElement>) => void;
  };
  /** 拖拽中的列 key（把手加高亮类用） */
  draggingKey: string | null;
}

export function useColumnResize(options: {
  widths: Record<string, number>;
  setWidths: (next: Record<string, number>) => void;
  /** 声明默认宽（key → px；缺省列不参与拖拽宽度记忆） */
  defaultWidths: Record<string, number | undefined>;
  /** 锁定列 key 集合（不参与拖拽/双击还原） */
  lockedKeys?: ReadonlySet<string>;
  /** 表格容器 ref（容器宽夹最小宽用；缺省退纯 MIN_COL_PX 夹取） */
  containerRef?: React.RefObject<HTMLElement | null>;
  /** 落盘（pointerup 时调用一次；缺省不落盘只 setState） */
  onCommit?: (widths: Record<string, number>) => void;
}): ColumnWidthsHook {
  const { widths, setWidths, defaultWidths, lockedKeys, containerRef, onCommit } = options;
  const draggingKey = useRef<string | null>(null);
  // 拖拽把手高亮需要一次重渲染；拖拽中 move 路径零 setState（性能红线）
  const [, setRenderStamp] = useState(0);
  const forceRender = useCallback(() => setRenderStamp((n) => n + 1), []);

  const effectiveWidth = useCallback(
    (key: string): number | undefined => {
      if (lockedKeys?.has(key)) return defaultWidths[key];
      return widths[key] ?? defaultWidths[key];
    },
    [widths, defaultWidths, lockedKeys],
  );

  const gripProps = useCallback(
    (key: string, opts: { isLast: boolean; locked?: boolean; defaultWidth?: number }) => {
      // 末列不给把手（钉右缘顶出横滚条）；锁定列不给（固定 px）
      if (opts.isLast || opts.locked || lockedKeys?.has(key)) return {};
      const startWidth = (): number => {
        const w = widths[key] ?? defaultWidths[key];
        return typeof w === 'number' ? w : MIN_COL_PX;
      };
      return {
        onPointerDown: (e: React.PointerEvent<HTMLElement>) => {
          if (e.button !== 0) return;
          e.preventDefault();
          e.stopPropagation();
          const startX = e.clientX;
          const base = startWidth();
          const grip = e.currentTarget;
          const containerW = containerRef?.current?.clientWidth;
          draggingKey.current = key;
          forceRender();
          const totalBase = Object.keys(defaultWidths).reduce((sum, k) => {
            const w = k === key ? base : (widths[k] ?? defaultWidths[k]);
            return sum + (typeof w === 'number' ? w : 0);
          }, 0);
          const onMove = (ev: PointerEvent): void => {
            const dx = ev.clientX - startX;
            let next = base + dx;
            if (next < MIN_COL_PX) next = MIN_COL_PX;
            // 容器宽夹取：总宽超出容器时按容器宽收（只在容器可测时启用）
            if (containerW && totalBase > containerW) {
              const others = totalBase - base;
              const maxThis = containerW - others;
              if (maxThis >= MIN_COL_PX && next > maxThis) next = maxThis;
            }
            // 直写 DOM：拖拽路径零 React 重渲染（性能红线）
            const colEl = grip.closest('table')?.querySelector<ColElement>(
              `col[data-col-key="${cssEscape(key)}"]`,
            );
            if (colEl) colEl.style.width = `${next}px`;
          };
          const onUp = (): void => {
            window.removeEventListener('pointermove', onMove);
            window.removeEventListener('pointerup', onUp);
            draggingKey.current = null;
            // 松手才读一次 DOM 终值进 state + 落盘
            const colEl = grip.closest('table')?.querySelector<ColElement>(
              `col[data-col-key="${cssEscape(key)}"]`,
            );
            const finalW = colEl ? parseFloat(colEl.style.width) : NaN;
            if (Number.isFinite(finalW) && finalW > 0) {
              setWidths({ ...widths, [key]: Math.round(finalW) });
              onCommit?.({ ...widths, [key]: Math.round(finalW) });
            }
            forceRender();
          };
          window.addEventListener('pointermove', onMove);
          window.addEventListener('pointerup', onUp);
        },
        onDoubleClick: (e: React.MouseEvent<HTMLElement>) => {
          e.preventDefault();
          e.stopPropagation();
          // 双击还原：删覆盖（回声明默认；声明默认列连覆盖键一起删）
          if (!(key in widths)) return;
          const next = { ...widths };
          delete next[key];
          setWidths(next);
          onCommit?.(next);
        },
      };
    },
    [widths, defaultWidths, lockedKeys, containerRef, setWidths, onCommit, forceRender],
  );

  return { widths, setWidths, effectiveWidth, gripProps, draggingKey: draggingKey.current };
}

/** 最小 CSS 转义（属性选择器里的 key 只会是 [\w-]，防御性处理） */
function cssEscape(s: string): string {
  return s.replace(/["\\]/g, '\\$&');
}
