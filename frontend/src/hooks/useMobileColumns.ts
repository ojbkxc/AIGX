import { useEffect, useRef } from 'react';

/**
 * 移动端列名注入 hook（对照 Agent2API `mobile-columns.js`）。
 *
 * 在窄屏（≤767px）时，把表头 th 的文字写入对应 td 的 data-col 属性，
 * 配合 CSS 的 `td::before { content: attr(data-col) }` 在卡片化布局中
 * 显示列名标签。桌面端不碰 DOM。
 *
 * 用法：在表格所在页面的根组件里调用 `useMobileColumns()`。
 * hook 内部用 MutationObserver 监听表格变化，60ms 合并后重跑。
 */
export function useMobileColumns(): void {
  const observerRef = useRef<MutationObserver | null>(null);

  useEffect(() => {
    const MAX_WIDTH = 767;

    function labelOf(el: Element): string {
      const clone = el.cloneNode(true) as Element;
      // 摘掉列宽把手、提示等附属节点，只取主名
      clone.querySelectorAll('.column-resize-handle, .col-grip, .th-hint, .tip-q, svg').forEach((n) =>
        n.remove()
      );
      const text = (clone.textContent || '').replace(/\s+/g, ' ').trim();
      // 去掉尾部括号说明
      return text.replace(/[（(][^）)]*[）)]\s*$/, '').trim() || text;
    }

    function already(el: Element): boolean {
      return el.getAttribute('data-col') !== null;
    }

    function paintTable(table: HTMLTableElement) {
      const ths = Array.from(table.querySelectorAll('thead th'));
      if (ths.length === 0) return;

      const hasKey = ths.some((th) => th.getAttribute('data-col'));
      const map = new Map<string, string>();
      if (hasKey) {
        ths.forEach((th) => {
          const k = th.getAttribute('data-col');
          if (k) map.set(k, labelOf(th));
        });
      }
      if (!hasKey && !ths.some((th) => labelOf(th))) return;

      table.querySelectorAll('tbody tr').forEach((tr) => {
        const cells = tr.children;
        for (let i = 0; i < cells.length && i < ths.length; i++) {
          const td = cells[i] as HTMLElement;
          const th = ths[i];
          if (!th || td.tagName !== 'TD' || already(td)) continue;
          const label = hasKey
            ? map.get(th.getAttribute('data-col') || '') || labelOf(th)
            : labelOf(th);
          if (label) td.setAttribute('data-col', label);
        }
      });
    }

    function paintAll() {
      if (window.innerWidth > MAX_WIDTH) return;
      document.querySelectorAll('.table-wrapper table').forEach((t) => {
        paintTable(t as HTMLTableElement);
      });
    }

    function run() {
      paintAll();
    }

    // 首次执行
    run();

    // 窗口尺寸跨过断点时重跑
    window.addEventListener('resize', run);

    // React 重渲染后 tbody 里的行会重建（data-col 属性被清掉）
    // 用 MutationObserver 监听 document.body 子树变化，60ms 合并后重跑
    let timer: ReturnType<typeof setTimeout> | null = null;
    const schedule = () => {
      if (timer) clearTimeout(timer);
      timer = setTimeout(run, 60);
    };

    observerRef.current = new MutationObserver(schedule);
    observerRef.current.observe(document.body, { childList: true, subtree: true });

    return () => {
      window.removeEventListener('resize', run);
      observerRef.current?.disconnect();
      if (timer) clearTimeout(timer);
    };
  }, []);
}
