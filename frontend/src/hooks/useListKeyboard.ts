import { useEffect, useRef, useState } from 'react';

/**
 * useListKeyboard — 列表页键盘导航（cc-haha Dialog/列表键盘体系模式）。
 * - `/` 聚焦搜索框（输入框内不响应，避免吞内容）
 * - ↑↓/j k 在当前页行间移动高亮（输入框内所有导航键一律交还输入框，j/k 是正文）
 * - Enter 打开当前行（onEnter）
 * 纯增量：不拦截表单输入态；行点击交互不受影响。
 *
 * @param total 当前页总行数
 * @param onEnter 高亮行上按 Enter 的回调（参数为行索引）
 * @param enabled 关闭开关（弹窗打开、loading 时传 false）
 */
export function useListKeyboard(
  total: number,
  onEnter: (index: number) => void,
  enabled: boolean = true,
): {
  searchRef: React.RefObject<HTMLInputElement>;
  rowRefs: React.MutableRefObject<(HTMLTableRowElement | null)[]>;
  activeIndex: number;
  setActiveIndex: (i: number) => void;
} {
  const searchRef = useRef<HTMLInputElement>(null);
  const rowRefs = useRef<(HTMLTableRowElement | null)[]>([]);
  const [activeIndex, setActiveIndexState] = useState(-1);
  // onEnter 用 ref 透传：避免调用方内联箭头函数导致键盘监听频繁重建
  const onEnterRef = useRef(onEnter);
  onEnterRef.current = onEnter;

  const setActiveIndex = (i: number): void => {
    setActiveIndexState(i);
    // 行到视口边缘时滚动跟随
    const el = rowRefs.current[i];
    if (el) el.scrollIntoView({ block: 'nearest' });
  };

  useEffect(() => {
    if (!enabled) return;
    const onKey = (e: KeyboardEvent): void => {
      const target = e.target as HTMLElement | null;
      const inInput = !!target && (
        target.tagName === 'INPUT'
        || target.tagName === 'TEXTAREA'
        || target.tagName === 'SELECT'
        || target.isContentEditable
      );

      // `/` 聚焦搜索：仅非输入态响应
      if (e.key === '/' && !inInput) {
        e.preventDefault();
        searchRef.current?.focus();
        return;
      }

      // 输入态（含聚焦的搜索框）把所有导航键交还给输入框：↑↓ 移光标，
      // j/k 是打字内容（修复：搜索框输入 ojbkxc 时打不出 j）。行间导航
      // 只在非输入态生效，与 cc-haha 的「输入框内不响应」口径一致。
      if (inInput) return;

      if (!['ArrowUp', 'ArrowDown', 'j', 'k', 'Enter'].includes(e.key)) return;
      if (total <= 0) return;

      if (e.key === 'ArrowUp' || e.key === 'k') {
        e.preventDefault();
        setActiveIndex(activeIndex <= 0 ? total - 1 : activeIndex - 1);
        return;
      }
      if (e.key === 'ArrowDown' || e.key === 'j') {
        e.preventDefault();
        setActiveIndex(activeIndex >= total - 1 ? 0 : activeIndex + 1);
        return;
      }
      // Enter：非输入态且已有高亮行才触发（inInput 已在上面统一早退，此处仅高亮行校验）
      if (e.key === 'Enter' && activeIndex >= 0) {
        e.preventDefault();
        onEnterRef.current(activeIndex);
      }
    };
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, [enabled, total, activeIndex]);

  // 行数收缩（翻页/过滤/删除）后钳制越界高亮
  useEffect(() => {
    if (activeIndex >= total) setActiveIndexState(total - 1);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [total]);

  return { searchRef, rowRefs, activeIndex, setActiveIndex };
}
