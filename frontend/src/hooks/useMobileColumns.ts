import { useEffect } from 'react';

/**
 * 移动端列名注入 hook（对照 Agent2API `mobile-columns.js`）。
 *
 * 在窄屏（≤767px）时，把表头 th 的文字写入对应 td 的 data-col 属性，
 * 配合 CSS 的 `td::before { content: attr(data-col) }` 在卡片化布局中
 * 显示列名标签。桌面端不碰 DOM。
 *
 * 用法：在表格所在页面的根组件里调用 `useMobileColumns()`。
 * #18 锦上添花：原来 document.body 全子树监听 + 60ms 合并后重绘全部表格，
 * 任何无关 DOM 变动（弹窗/toast/tooltip）都会触发全量重绘。现改为按需绑定：
 * - MutationObserver 从变更记录里只圈出受影响的 .table-wrapper 表格重绘；
 * - ResizeObserver 绑定每个表格，列宽/尺寸变化按需重绘；
 * - 断点跨越（≤767px ↔ 桌面）用 matchMedia 事件整体重绘一次。
 */
export function useMobileColumns(): void {
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

    const mq = window.matchMedia(`(max-width: ${MAX_WIDTH}px)`);

    /** 列宽/尺寸变化按需重绘（观察每个表格本体） */
    const resizeObserver = new ResizeObserver((entries) => {
      if (!mq.matches) return;
      entries.forEach((entry) => paintTable(entry.target as HTMLTableElement));
    });
    const observedTables = new WeakSet<HTMLTableElement>();

    /** 给表格挂上 ResizeObserver（已挂的跳过） */
    function trackTable(table: HTMLTableElement): void {
      if (observedTables.has(table)) return;
      observedTables.add(table);
      resizeObserver.observe(table);
    }

    function run() {
      if (!mq.matches) return;
      document.querySelectorAll('.table-wrapper table').forEach((t) => {
        const table = t as HTMLTableElement;
        paintTable(table);
        trackTable(table);
      });
    }

    /** 只重绘受影响的表格（变更记录里圈出来的），不再是全量重绘 */
    function runDirty(dirty: Set<HTMLTableElement>): void {
      if (!mq.matches) return;
      dirty.forEach((table) => {
        paintTable(table);
        trackTable(table);
      });
    }

    // 首次执行
    run();

    // 窗口尺寸跨过断点时整体重绘一次
    const onBreakpoint = (): void => run();
    mq.addEventListener('change', onBreakpoint);

    // React 重渲染后 tbody 里的行会重建（data-col 属性被清掉）
    // 用 MutationObserver 圈出受影响的表格，60ms 合并后只重绘这些表
    let timer: ReturnType<typeof setTimeout> | null = null;
    const observer = new MutationObserver((records) => {
      const dirty = new Set<HTMLTableElement>();
      for (const record of records) {
        const target = record.target as Element | null;
        if (!target || target.nodeType !== Node.ELEMENT_NODE) continue;
        const table = target.tagName === 'TABLE' ? target : target.closest?.('table');
        if (table && table.closest('.table-wrapper')) dirty.add(table as HTMLTableElement);
      }
      if (!dirty.size) return;
      if (timer) clearTimeout(timer);
      timer = setTimeout(() => runDirty(dirty), 60);
    });
    observer.observe(document.body, { childList: true, subtree: true });

    return () => {
      mq.removeEventListener('change', onBreakpoint);
      observer.disconnect();
      resizeObserver.disconnect();
      if (timer) clearTimeout(timer);
    };
  }, []);
}
